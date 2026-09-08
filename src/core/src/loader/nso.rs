// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/loader/nso.h and nso.cpp
//!
//! NSO (Nintendo Shared Object) loader.

use std::collections::BTreeMap;

use crate::file_sys::patch_manager::PatchManager;
use crate::file_sys::vfs::vfs::VfsFile;
use crate::file_sys::vfs::vfs_types::VirtualFile;
use crate::hle::kernel::code_set::CodeSet;
use common::common_funcs::make_magic;

use super::loader::{
    AppLoader, FileType, FileTypeIdentifier, KProcess, LoadParameters, LoadResult, Modules,
    ResultStatus, System,
};

// ============================================================================
// NSOSegmentHeader
// ============================================================================

/// Segment header within an NSO file.
///
/// Maps to upstream `Loader::NSOSegmentHeader`.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct NsoSegmentHeader {
    pub offset: u32,
    pub location: u32,
    pub size: u32,
    /// Union in C++: alignment or bss_size depending on context.
    pub alignment_or_bss_size: u32,
}

const _: () = assert!(std::mem::size_of::<NsoSegmentHeader>() == 0x10);

// ============================================================================
// NSOHeader
// ============================================================================

/// RoData-relative extent descriptor.
///
/// Maps to upstream `NSOHeader::RODataRelativeExtent`.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct RoDataRelativeExtent {
    pub data_offset: u32,
    pub size: u32,
}

/// SHA-256 hash type.
pub type Sha256Hash = [u8; 0x20];

/// NSO file header.
///
/// Maps to upstream `Loader::NSOHeader`.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct NsoHeader {
    pub magic: u32,
    pub version: u32,
    pub reserved: u32,
    pub flags: u32,
    /// Text, RoData, Data segments (in that order).
    pub segments: [NsoSegmentHeader; 3],
    pub build_id: [u8; 0x20],
    pub segments_compressed_size: [u32; 3],
    pub padding: [u8; 0x1C],
    pub api_info_extent: RoDataRelativeExtent,
    pub dynstr_extent: RoDataRelativeExtent,
    pub dynsyn_extent: RoDataRelativeExtent,
    pub segment_hashes: [Sha256Hash; 3],
}

const _: () = assert!(std::mem::size_of::<NsoHeader>() == 0x100);

impl Default for NsoHeader {
    fn default() -> Self {
        // Safety: NsoHeader is repr(C) and all-zeros is valid.
        unsafe { std::mem::zeroed() }
    }
}

impl NsoHeader {
    /// Check if a specific segment is compressed.
    ///
    /// Maps to upstream `NSOHeader::IsSegmentCompressed`.
    pub fn is_segment_compressed(&self, segment_num: usize) -> bool {
        assert!(segment_num < 3, "Invalid segment {}", segment_num);
        ((self.flags >> segment_num) & 1) != 0
    }
}

// ============================================================================
// NSO_ARGUMENT_DATA_ALLOCATION_SIZE
// ============================================================================

/// Maps to upstream `NSO_ARGUMENT_DATA_ALLOCATION_SIZE`.
pub const NSO_ARGUMENT_DATA_ALLOCATION_SIZE: u32 = 0x9000;

// ============================================================================
// NSOArgumentHeader
// ============================================================================

/// NSO argument header.
///
/// Maps to upstream `Loader::NSOArgumentHeader`.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct NsoArgumentHeader {
    pub allocated_size: u32,
    pub actual_size: u32,
    pub _padding: [u8; 0x18],
}

const _: () = assert!(std::mem::size_of::<NsoArgumentHeader>() == 0x20);

impl Default for NsoArgumentHeader {
    fn default() -> Self {
        Self {
            allocated_size: 0,
            actual_size: 0,
            _padding: [0u8; 0x18],
        }
    }
}

// ============================================================================
// MODHeader (private to nso.cpp)
// ============================================================================

/// MOD header found within NSO modules.
///
/// Maps to upstream anonymous `MODHeader` struct in nso.cpp.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
struct ModHeader {
    magic: u32,
    dynamic_offset: u32,
    bss_start_offset: u32,
    bss_end_offset: u32,
    eh_frame_hdr_start_offset: u32,
    eh_frame_hdr_end_offset: u32,
    /// Offset to runtime-generated module object. Typically equal to .bss base.
    module_offset: u32,
}

const _: () = assert!(std::mem::size_of::<ModHeader>() == 0x1c);

// ============================================================================
// Helper functions
// ============================================================================

/// Page-align a size value.
///
/// Maps to upstream anonymous `PageAlignSize`.
const YUZU_PAGEMASK: u32 = 0xFFF;

const fn page_align_size(size: u32) -> u32 {
    (size + YUZU_PAGEMASK) & !YUZU_PAGEMASK
}

/// Decompress a segment using LZ4.
///
/// Maps to upstream `DecompressSegment`.
fn decompress_segment(compressed_data: &[u8], expected_size: u32) -> Vec<u8> {
    let mut result = vec![0u8; expected_size as usize];
    match lz4_flex::block::decompress_into(compressed_data, &mut result) {
        Ok(written) => {
            if written != expected_size as usize {
                log::warn!(
                    "NSO LZ4 decompression: expected {} bytes, got {}",
                    expected_size,
                    written
                );
            }
        }
        Err(e) => {
            log::error!("NSO LZ4 decompression failed: {}", e);
            // Return zeroed buffer on failure
            result.fill(0);
        }
    }
    result
}

/// Read a plain-old-data struct from a VfsFile at the given offset.
///
/// Safety: T must be repr(C) and valid when zero-initialized.
pub fn read_object<T: Copy + Default>(file: &dyn VfsFile, offset: usize) -> Option<T> {
    let size = std::mem::size_of::<T>();
    let mut obj = T::default();
    let bytes = unsafe { std::slice::from_raw_parts_mut(&mut obj as *mut T as *mut u8, size) };
    let read = file.read(bytes, size, offset);
    if read == size {
        Some(obj)
    } else {
        None
    }
}

// ============================================================================
// AppLoaderNso
// ============================================================================

/// Loads an NSO file.
///
/// Maps to upstream `Loader::AppLoader_NSO`.
pub struct AppLoaderNso {
    file: VirtualFile,
    is_loaded: bool,
    modules: Modules,
}

impl FileTypeIdentifier for AppLoaderNso {
    /// Identifies whether or not the given file is a form of NSO file.
    ///
    /// Maps to upstream `AppLoader_NSO::IdentifyType`.
    fn identify_type(in_file: &VirtualFile) -> FileType {
        let magic: Option<u32> = read_object::<u32>(in_file.as_ref(), 0);
        match magic {
            Some(m) if m == make_magic(b'N', b'S', b'O', b'0') => FileType::NSO,
            _ => FileType::Error,
        }
    }
}

impl AppLoaderNso {
    pub fn new(file: VirtualFile) -> Self {
        Self {
            file,
            is_loaded: false,
            modules: BTreeMap::new(),
        }
    }

    /// Load an NSO module into a process at the specified base address.
    ///
    /// Maps to upstream `AppLoader_NSO::LoadModule`.
    pub fn load_module(
        process: &mut KProcess,
        system: &mut System,
        nso_file: &dyn VfsFile,
        load_base: u64,
        should_pass_arguments: bool,
        load_into_process: bool,
        patch_manager: Option<&PatchManager<'_>>,
    ) -> Option<u64> {
        if nso_file.get_size() < std::mem::size_of::<NsoHeader>() {
            return None;
        }

        let nso_header: NsoHeader = read_object(nso_file, 0)?;

        if nso_header.magic != make_magic(b'N', b'S', b'O', b'0') {
            return None;
        }

        // Build program image and codeset metadata.
        // Upstream: module_start accounts for NCE PreText patch space; without NCE it is 0.
        let module_start: usize = 0;

        let mut code_set = CodeSet::new();
        let mut program_image: Vec<u8> = Vec::new();

        for i in 0..3 {
            let mut data = nso_file.read_bytes(
                nso_header.segments_compressed_size[i] as usize,
                nso_header.segments[i].offset as usize,
            );
            if nso_header.is_segment_compressed(i) {
                data = decompress_segment(&data, nso_header.segments[i].size);
            }
            log::info!(
                "NSO segment[{}]: location={:#x} size={:#x} compressed={:#x} data.len={:#x}",
                i,
                nso_header.segments[i].location,
                nso_header.segments[i].size,
                nso_header.segments_compressed_size[i],
                data.len()
            );
            let needed_size = module_start + nso_header.segments[i].location as usize + data.len();
            if program_image.len() < needed_size {
                program_image.resize(needed_size, 0);
            }
            let loc = module_start + nso_header.segments[i].location as usize;
            program_image[loc..loc + data.len()].copy_from_slice(&data);

            code_set.segments[i].addr =
                (module_start + nso_header.segments[i].location as usize) as u64;
            code_set.segments[i].offset = module_start + nso_header.segments[i].location as usize;
            code_set.segments[i].size = nso_header.segments[i].size;
        }
        log::info!(
            "NSO bss_size={:#x} program_image.len={:#x}",
            nso_header.segments[2].alignment_or_bss_size,
            program_image.len()
        );

        // Upstream: arguments are added BEFORE BSS, matching upstream ordering.
        // Upstream condition: `should_pass_arguments && !Settings::values.program_args.GetValue().empty()`
        let program_args = common::settings::values().program_args.get_value().clone();
        if should_pass_arguments && !program_args.is_empty() {
            // Upstream memcpy has no length guard. Do not panic or copy beyond
            // the fixed argument allocation for an oversized user setting.
            let capacity = NSO_ARGUMENT_DATA_ALLOCATION_SIZE as usize
                - std::mem::size_of::<NsoArgumentHeader>();
            if program_args.len() > capacity {
                log::error!("NSO arguments exceed the reserved block: {} bytes, maximum {}",
                    program_args.len(), capacity);
                return None;
            }
            code_set.data_segment_mut().size += NSO_ARGUMENT_DATA_ALLOCATION_SIZE;
            let arg_header = NsoArgumentHeader {
                allocated_size: NSO_ARGUMENT_DATA_ALLOCATION_SIZE.to_le(),
                actual_size: (program_args.len() as u32).to_le(),
                _padding: [0u8; 0x18],
            };
            let end_offset = program_image.len();
            program_image.resize(
                program_image.len() + NSO_ARGUMENT_DATA_ALLOCATION_SIZE as usize,
                0,
            );
            let header_bytes = unsafe {
                std::slice::from_raw_parts(
                    &arg_header as *const NsoArgumentHeader as *const u8,
                    std::mem::size_of::<NsoArgumentHeader>(),
                )
            };
            program_image[end_offset..end_offset + header_bytes.len()]
                .copy_from_slice(header_bytes);
            let arg_data_start = end_offset + std::mem::size_of::<NsoArgumentHeader>();
            program_image[arg_data_start..arg_data_start + program_args.len()]
                .copy_from_slice(program_args.as_bytes());
        }

        // BSS: add after arguments, matching upstream ordering.
        let bss_size = nso_header.segments[2].alignment_or_bss_size;
        code_set.data_segment_mut().size += bss_size;
        let image_size = page_align_size(program_image.len() as u32 + bss_size);
        program_image.resize(image_size as usize, 0);

        for segment in &mut code_set.segments {
            segment.size = page_align_size(segment.size);
        }

        // Apply patches if necessary.
        let name = nso_file.get_name();
        if let Some(patch_manager) = patch_manager {
            if patch_manager.has_nso_patch(&nso_header.build_id, &name)
                || *common::settings::values().dump_nso.get_value()
            {
                let header_size = std::mem::size_of::<NsoHeader>();
                let mut patchable = Vec::with_capacity(header_size + program_image.len());
                let header_bytes = unsafe {
                    std::slice::from_raw_parts(
                        &nso_header as *const NsoHeader as *const u8,
                        header_size,
                    )
                };
                patchable.extend_from_slice(header_bytes);
                patchable.extend_from_slice(&program_image);
                let patched = patch_manager.patch_nso(patchable, &name);
                if patched.len() != header_size + program_image.len() {
                    log::error!(
                        "NSO patch changed image size unexpectedly: before={:#X}, after={:#X}",
                        program_image.len(),
                        patched.len().saturating_sub(header_size)
                    );
                    return None;
                }
                program_image.copy_from_slice(&patched[header_size..]);
            }
        }

        // NCE patching remains owned by the separate backend-specific subsystem.

        // If we are not actually loading (just computing process code layout), return early.
        // Matches upstream: `if (!load_into_process) { return load_base + image_size; }`
        if !load_into_process {
            return Some(load_base + image_size as u64);
        }

        // Apply cheats if they exist and the program has a valid title ID.
        if let Some(patch_manager) = patch_manager {
            system.set_application_process_build_id(nso_header.build_id);
            let cheats = patch_manager.create_cheat_list(&nso_header.build_id);
            if !cheats.is_empty() {
                system.register_cheat_list(
                    cheats,
                    nso_header.build_id,
                    load_base,
                    image_size as u64,
                );
            }
        }

        // Dump raw module binary for offline disassembly.
        if let Ok(dump_dir) = std::env::var("RUZU_DUMP_MODULES") {
            let _ = std::fs::create_dir_all(&dump_dir);
            let module_name = nso_file.get_name();
            let path = format!("{}/0x{:08X}_{}.bin", dump_dir, load_base, module_name);
            if let Err(e) = std::fs::write(&path, &program_image) {
                log::error!("Failed to dump module: {}", e);
            } else {
                log::info!("NSO: dumped {} bytes to {}", program_image.len(), path);
            }
        }

        // Load codeset into process.
        code_set.memory = program_image;
        process.load_module(code_set, load_base);
        log::info!("NSO: loaded {} bytes at {:#X}", image_size, load_base);

        Some(load_base + image_size as u64)
    }
}

impl AppLoader for AppLoaderNso {
    fn get_file_type(&self) -> FileType {
        Self::identify_type(&self.file)
    }

    /// Maps to upstream `AppLoader_NSO::Load`.
    fn load(&mut self, process: &mut KProcess, system: &mut System) -> LoadResult {
        if self.is_loaded {
            return (ResultStatus::ErrorAlreadyLoaded, None);
        }

        self.modules.clear();

        // Upstream: base_address = GetInteger(process.GetEntryPoint())
        let base_address: u64 = process.get_entry_point().get();

        let result = Self::load_module(
            process,
            system,
            self.file.as_ref(),
            base_address,
            true,
            true,
            None,
        );

        if result.is_none() {
            return (ResultStatus::ErrorLoadingNSO, None);
        }

        self.modules.insert(base_address, self.file.get_name());
        log::debug!(
            "loaded module {} @ {:#X}",
            self.file.get_name(),
            base_address
        );

        self.is_loaded = true;

        // Upstream: Kernel::KThread::DefaultThreadPriority = 44
        // Upstream: Core::Memory::DEFAULT_STACK_SIZE = 0x100000
        const DEFAULT_THREAD_PRIORITY: i32 = 44;
        const DEFAULT_STACK_SIZE: u64 = 0x100000;

        (
            ResultStatus::Success,
            Some(LoadParameters {
                main_thread_priority: DEFAULT_THREAD_PRIORITY,
                main_thread_stack_size: DEFAULT_STACK_SIZE,
            }),
        )
    }

    fn read_nso_modules(&self, modules: &mut Modules) -> ResultStatus {
        *modules = self.modules.clone();
        ResultStatus::Success
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argument_layout_pass_respects_setting_gate_and_byte_capacity() {
        const CHILD: &str = "RUZU_TEST_NSO_ARGUMENTS";
        if std::env::var_os(CHILD).is_none() {
            assert!(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", std::thread::current().name().unwrap()])
                .env(CHILD, "1").status().unwrap().success());
            return;
        }
        std::thread::Builder::new().stack_size(32 * 1024 * 1024).spawn(|| {
            use common::fs::path_util::{set_ruzu_path, RuzuPath};
            use crate::file_sys::vfs::vfs_vector::VectorVfsFile;
            let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let directory = std::env::temp_dir().join(format!("ruzu-argument-test-{}-{nonce}", std::process::id()));
            std::fs::create_dir(&directory).unwrap();
            std::fs::create_dir(directory.join("sdmc")).unwrap();
            set_ruzu_path(RuzuPath::LogDir, &directory);
            set_ruzu_path(RuzuPath::SDMCDir, &directory.join("sdmc"));
            let mut system = Box::new(System::new(None, None));
            let mut process = KProcess::new();
            // Minimal synthetic NSO: empty segments and a zeroed header. Its
            // layout consists only of the optional argument block and BSS.
            let mut bytes = vec![0; 0x100];
            bytes[..4].copy_from_slice(b"NSO0");
            bytes[0x3C..0x40].copy_from_slice(&1u32.to_le_bytes()); // BSS follows args.
            let file = VectorVfsFile::new(bytes, "synthetic".into(), None);
            let capacity = NSO_ARGUMENT_DATA_ALLOCATION_SIZE as usize - 0x20;
            for text in [String::new(), "--label café".into(), "x".repeat(capacity),
                "é".repeat(capacity / 2), "x".repeat(capacity + 1), "é".repeat(capacity / 2 + 1)] {
                common::settings::values_mut().program_args.set_value(text.clone());
                for pass_arguments in [false, true] {
                    let result = AppLoaderNso::load_module(&mut process, &mut system, &file,
                        0x10000, pass_arguments, false, None);
                    let expected = if !pass_arguments || text.is_empty() {
                        Some(0x11000)
                    } else if text.len() <= capacity {
                        Some(0x1A000)
                    } else { None };
                    assert_eq!(result, expected, "{} bytes, pass={pass_arguments}", text.len());
                }
            }
            assert_eq!(std::mem::size_of::<NsoArgumentHeader>(), 0x20);
            assert_eq!(std::mem::offset_of!(NsoArgumentHeader, actual_size), 4);
            assert_eq!(std::mem::offset_of!(NsoArgumentHeader, _padding), 8);
            drop(process);
            drop(system);
            std::fs::remove_dir_all(directory).unwrap();
        }).unwrap().join().unwrap();
    }
}
