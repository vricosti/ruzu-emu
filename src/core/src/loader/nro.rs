// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of eden/src/core/loader/nro.h and nro.cpp
//!
//! NRO (Nintendo Relocatable Object) loader.

use std::sync::Arc;

use crate::file_sys::control_metadata::NACP as FileSysNACP;
use crate::file_sys::program_metadata::ProgramMetadata;
use crate::file_sys::vfs::vfs::VfsFile;
use crate::file_sys::vfs::vfs_offset::OffsetVfsFile;
use crate::file_sys::vfs::vfs_types::VirtualFile;
use crate::hle::kernel::code_set::CodeSet;
use crate::hle::kernel::k_typed_address::KProcessAddress;
use common::common_funcs::make_magic;

use super::loader::{
    AppLoader, FileType, FileTypeIdentifier, KProcess, LoadParameters, LoadResult, ResultStatus,
    System, NACP,
};
use super::nso::read_object;

// ============================================================================
// NroSegmentHeader
// ============================================================================

/// Segment header within an NRO file.
///
/// Maps to upstream `Loader::NroSegmentHeader` (in nro.cpp).
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct NroSegmentHeader {
    pub offset: u32,
    pub size: u32,
}

const _: () = assert!(std::mem::size_of::<NroSegmentHeader>() == 0x8);

// ============================================================================
// NroHeader
// ============================================================================

/// NRO file header.
///
/// Maps to upstream `Loader::NroHeader` (in nro.cpp).
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct NroHeader {
    pub _padding0: [u8; 0x4],
    pub module_header_offset: u32,
    pub magic_ext1: u32,
    pub magic_ext2: u32,
    pub magic: u32,
    pub _padding1: [u8; 0x4],
    pub file_size: u32,
    pub _padding2: [u8; 0x4],
    /// Text, RoData, Data segments (in that order).
    pub segments: [NroSegmentHeader; 3],
    pub bss_size: u32,
    pub _padding3: [u8; 0x44],
}

const _: () = assert!(std::mem::size_of::<NroHeader>() == 0x80);

impl Default for NroHeader {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

// ============================================================================
// ModHeader (NRO variant)
// ============================================================================

/// MOD header found within NRO modules.
///
/// Maps to upstream `Loader::ModHeader` (in nro.cpp).
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
struct ModHeader {
    magic: u32,
    dynamic_offset: u32,
    bss_start_offset: u32,
    bss_end_offset: u32,
    unwind_start_offset: u32,
    unwind_end_offset: u32,
    /// Offset to runtime-generated module object. Typically equal to .bss base.
    module_offset: u32,
}

const _: () = assert!(std::mem::size_of::<ModHeader>() == 0x1c);

// ============================================================================
// AssetSection / AssetHeader
// ============================================================================

/// Asset section descriptor.
///
/// Maps to upstream `Loader::AssetSection` (in nro.cpp).
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct AssetSection {
    pub offset: u64,
    pub size: u64,
}

const _: () = assert!(std::mem::size_of::<AssetSection>() == 0x10);

/// NRO asset header, appended after the NRO proper.
///
/// Maps to upstream `Loader::AssetHeader` (in nro.cpp).
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct AssetHeader {
    pub magic: u32,
    pub format_version: u32,
    pub icon: AssetSection,
    pub nacp: AssetSection,
    pub romfs: AssetSection,
}

const _: () = assert!(std::mem::size_of::<AssetHeader>() == 0x38);

// ============================================================================
// Helper
// ============================================================================

const YUZU_PAGEMASK: u32 = 0xFFF;
const YUZU_PAGESIZE: usize = 0x1000;

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
struct ConfigEntry {
    key: u32,
    flags: u32,
    value: [u64; 2],
}

const _: () = assert!(std::mem::size_of::<ConfigEntry>() == 0x18);

const SVC_EXIT_PROCESS_INSTRUCTION: u32 = 0xD400_00E1;
const NUM_CONFIG_ENTRIES: usize = 4;
const CONFIG_TABLE_SIZE: usize = NUM_CONFIG_ENTRIES * std::mem::size_of::<ConfigEntry>();
const ENTRY_END_OF_LIST: u32 = 0;
const ENTRY_MAIN_THREAD_HANDLE: u32 = 1;
const ENTRY_ARGV: u32 = 5;
const ENTRY_APPLET_TYPE: u32 = 7;
const APPLET_TYPE_APPLICATION: u64 = 0;
const MAIN_THREAD_HANDLE_VALUE_OFFSET: u64 = 8;

const fn page_align_size(size: u32) -> u32 {
    (size + YUZU_PAGEMASK) & !YUZU_PAGEMASK
}

fn build_config_table(argv_addr: u64) -> [u8; CONFIG_TABLE_SIZE] {
    let entries = [
        ConfigEntry {
            key: ENTRY_MAIN_THREAD_HANDLE,
            flags: 0,
            value: [0, 0],
        },
        ConfigEntry {
            key: ENTRY_APPLET_TYPE,
            flags: 0,
            value: [APPLET_TYPE_APPLICATION, 0],
        },
        ConfigEntry {
            key: ENTRY_ARGV,
            flags: 0,
            value: [0, argv_addr],
        },
        ConfigEntry {
            key: ENTRY_END_OF_LIST,
            flags: 0,
            value: [0, 0],
        },
    ];
    let mut bytes = [0u8; CONFIG_TABLE_SIZE];
    for (index, entry) in entries.iter().enumerate() {
        let offset = index * std::mem::size_of::<ConfigEntry>();
        bytes[offset..offset + 4].copy_from_slice(&entry.key.to_le_bytes());
        bytes[offset + 4..offset + 8].copy_from_slice(&entry.flags.to_le_bytes());
        bytes[offset + 8..offset + 16].copy_from_slice(&entry.value[0].to_le_bytes());
        bytes[offset + 16..offset + 24].copy_from_slice(&entry.value[1].to_le_bytes());
    }
    bytes
}

// ============================================================================
// AppLoaderNro
// ============================================================================

/// Loads an NRO file.
///
/// Maps to upstream `Loader::AppLoader_NRO`.
pub struct AppLoaderNro {
    file: VirtualFile,
    is_loaded: bool,
    icon_data: Vec<u8>,
    nacp: Option<FileSysNACP>,
    romfs: Option<VirtualFile>,
}

impl FileTypeIdentifier for AppLoaderNro {
    /// Identifies whether or not the given file is an NRO file.
    ///
    /// Maps to upstream `AppLoader_NRO::IdentifyType`.
    fn identify_type(nro_file: &VirtualFile) -> FileType {
        let header: Option<NroHeader> = read_object(nro_file.as_ref(), 0);
        match header {
            Some(h) if h.magic == make_magic(b'N', b'R', b'O', b'0') => FileType::NRO,
            _ => FileType::Error,
        }
    }
}

impl AppLoaderNro {
    /// Create a new NRO loader.
    ///
    /// Maps to upstream `AppLoader_NRO::AppLoader_NRO`.
    pub fn new(file: VirtualFile) -> Self {
        let mut icon_data = Vec::new();
        let mut nacp: Option<FileSysNACP> = None;
        let mut romfs: Option<VirtualFile> = None;

        // Read NRO header to find asset section
        if let Some(nro_header) = read_object::<NroHeader>(file.as_ref(), 0) {
            let file_size = file.get_size();
            if file_size >= nro_header.file_size as usize + std::mem::size_of::<AssetHeader>() {
                let offset = nro_header.file_size as usize;
                if let Some(asset_header) = read_object::<AssetHeader>(file.as_ref(), offset) {
                    if asset_header.format_version != 0 {
                        log::warn!(
                            "NRO Asset Header has format {}, currently supported format is 0. \
                             If strange glitches occur with metadata, check NRO assets.",
                            asset_header.format_version
                        );
                    }

                    if asset_header.magic == make_magic(b'A', b'S', b'E', b'T') {
                        // Parse NACP from asset header
                        if asset_header.nacp.size > 0 {
                            let nacp_file: VirtualFile = Arc::new(OffsetVfsFile::new(
                                file.clone(),
                                asset_header.nacp.size as usize,
                                offset + asset_header.nacp.offset as usize,
                                "Control.nacp".to_string(),
                            ));
                            nacp = Some(FileSysNACP::from_file(&nacp_file));
                        }

                        // Create RomFS from asset header
                        if asset_header.romfs.size > 0 {
                            romfs = Some(Arc::new(OffsetVfsFile::new(
                                file.clone(),
                                asset_header.romfs.size as usize,
                                offset + asset_header.romfs.offset as usize,
                                "game.romfs".to_string(),
                            )));
                        }

                        // Read icon data
                        if asset_header.icon.size > 0 {
                            icon_data = file.read_bytes(
                                asset_header.icon.size as usize,
                                offset + asset_header.icon.offset as usize,
                            );
                        }
                    }
                }
            }
        }

        Self {
            file,
            is_loaded: false,
            icon_data,
            nacp,
            romfs,
        }
    }

    /// Check if this NRO is homebrew (has HOMEBREW magic).
    ///
    /// Maps to upstream `AppLoader_NRO::IsHomebrew`.
    pub fn is_homebrew(&self) -> bool {
        if let Some(header) = read_object::<NroHeader>(self.file.as_ref(), 0) {
            header.magic_ext1 == make_magic(b'H', b'O', b'M', b'E')
                && header.magic_ext2 == make_magic(b'B', b'R', b'E', b'W')
        } else {
            false
        }
    }

    /// Internal NRO loading implementation.
    ///
    /// Maps to upstream static `LoadNroImpl`.
    fn load_nro_impl(_system: &mut System, process: &mut KProcess, data: &[u8]) -> bool {
        if data.len() < std::mem::size_of::<NroHeader>() {
            return false;
        }

        // Read NRO header
        let mut nro_header = NroHeader::default();
        let header_bytes = unsafe {
            std::slice::from_raw_parts_mut(
                &mut nro_header as *mut NroHeader as *mut u8,
                std::mem::size_of::<NroHeader>(),
            )
        };
        header_bytes.copy_from_slice(&data[..std::mem::size_of::<NroHeader>()]);

        if nro_header.magic != make_magic(b'N', b'R', b'O', b'0') {
            return false;
        }

        // Build program image
        let aligned_size = page_align_size(nro_header.file_size) as usize;
        if data.len() < aligned_size {
            return false;
        }
        let mut program_image = vec![0u8; aligned_size];
        program_image.copy_from_slice(&data[..aligned_size]);

        if program_image.len() != aligned_size {
            return false;
        }

        // Set up CodeSet segments from NRO header
        let mut codeset = CodeSet::new();
        for i in 0..3 {
            codeset.segments[i].addr = nro_header.segments[i].offset as u64;
            codeset.segments[i].offset = nro_header.segments[i].offset as usize;
            codeset.segments[i].size = page_align_size(nro_header.segments[i].size);
        }

        // Default .bss to NRO header bss size if MOD0 section doesn't exist
        let mut bss_size = page_align_size(nro_header.bss_size);

        // Read MOD header
        let mod_offset = nro_header.module_header_offset as usize;
        if mod_offset + std::mem::size_of::<ModHeader>() <= program_image.len() {
            let mut mod_header = ModHeader::default();
            let mod_bytes = unsafe {
                std::slice::from_raw_parts_mut(
                    &mut mod_header as *mut ModHeader as *mut u8,
                    std::mem::size_of::<ModHeader>(),
                )
            };
            mod_bytes.copy_from_slice(
                &program_image[mod_offset..mod_offset + std::mem::size_of::<ModHeader>()],
            );

            if mod_header.magic == make_magic(b'M', b'O', b'D', b'0') {
                bss_size = page_align_size(mod_header.bss_end_offset - mod_header.bss_start_offset);
            }
        }

        // Upstream: codeset.DataSegment().size += bss_size
        codeset.data_segment_mut().size += bss_size;
        program_image.resize(program_image.len() + bss_size as usize, 0);

        let program_args = common::settings::values().program_args.get_value().clone();
        let mut argv_string = Vec::new();
        let mut args_offset_in_image = 0usize;
        let mut exit_process_offset_in_image = None;
        if !program_args.is_empty() {
            argv_string.extend_from_slice(b"homebrew ");
            argv_string.extend_from_slice(program_args.as_bytes());
            argv_string.push(0);

            let code = codeset.code_segment();
            let code_end = program_image.len().min(code.offset + code.size as usize);
            for offset in (code.offset..code_end.saturating_sub(3)).step_by(4) {
                let instruction = u32::from_le_bytes(
                    program_image[offset..offset + 4]
                        .try_into()
                        .expect("four-byte instruction"),
                );
                if instruction == SVC_EXIT_PROCESS_INSTRUCTION {
                    exit_process_offset_in_image = Some(offset);
                    break;
                }
            }
            if exit_process_offset_in_image.is_none() {
                log::warn!("Unable to find svcExitProcess in NRO; returning from main may fault");
            }

            let entries_and_argv = common::alignment::align_up(
                (CONFIG_TABLE_SIZE + argv_string.len()) as u64,
                YUZU_PAGESIZE as u64,
            ) as usize;
            args_offset_in_image = program_image.len();
            codeset.data_segment_mut().size += entries_and_argv as u32;
            program_image.resize(args_offset_in_image + entries_and_argv, 0);
        }
        let image_size = program_image.len() as u64;

        // Upstream: NCE patcher (HAS_NCE path). NCE patching is not yet integrated.

        // TODO: this is bad form of ASLR, it sucks
        let aslr_offset = {
            let settings = common::settings::values();
            let random = if *settings.rng_seed_enabled.get_value() {
                u64::from(*settings.rng_seed.get_value())
            } else {
                common::random::random64(0)
            };
            (random << 12) & 0xFFF000
        };

        // Setup the process code layout.
        let metadata = ProgramMetadata::get_default();
        let result = process.load_from_metadata(&metadata, image_size, 0, aslr_offset, false);
        if result != 0 {
            return false;
        }

        // Load codeset for current process.
        // Upstream: codeset.memory = std::move(program_image);
        //           process.LoadModule(std::move(codeset), process.GetEntryPoint());
        let entry_point = process.get_entry_point().into();
        codeset.memory = program_image;
        process.load_module(codeset, entry_point);

        if !argv_string.is_empty() {
            let base = process.get_entry_point().get();
            let config_addr = base + args_offset_in_image as u64;
            let argv_addr = config_addr + CONFIG_TABLE_SIZE as u64;
            process.write_memory(config_addr, &build_config_table(argv_addr));
            process.write_memory(argv_addr, &argv_string);
            process.set_arg_pointer(KProcessAddress::new(config_addr));
            if let Some(offset) = exit_process_offset_in_image {
                process.set_arg_return_address(KProcessAddress::new(base + offset as u64));
            }
            process.set_main_thread_handle_addr(KProcessAddress::new(
                config_addr + MAIN_THREAD_HANDLE_VALUE_OFFSET,
            ));
        }

        true
    }

    /// Load an NRO file into a process.
    ///
    /// Maps to upstream `AppLoader_NRO::LoadNro`.
    fn load_nro(
        &self,
        system: &mut System,
        process: &mut KProcess,
        nro_file: &dyn VfsFile,
    ) -> bool {
        let data = nro_file.read_all_bytes();
        Self::load_nro_impl(system, process, &data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn homebrew_config_table_matches_eden_binary_layout() {
        let argv_addr = 0x1234_5678_9ABC_DEF0;
        let table = build_config_table(argv_addr);

        assert_eq!(table.len(), 0x60);
        assert_eq!(u32::from_le_bytes(table[0..4].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(table[24..28].try_into().unwrap()), 7);
        assert_eq!(u32::from_le_bytes(table[48..52].try_into().unwrap()), 5);
        assert_eq!(
            u64::from_le_bytes(table[64..72].try_into().unwrap()),
            argv_addr
        );
        assert!(table[72..].iter().all(|byte| *byte == 0));
    }
}

impl AppLoader for AppLoaderNro {
    fn get_file_type(&self) -> FileType {
        Self::identify_type(&self.file)
    }

    /// Maps to upstream `AppLoader_NRO::Load`.
    fn load(&mut self, process: &mut KProcess, system: &mut System) -> LoadResult {
        if self.is_loaded {
            return (ResultStatus::ErrorAlreadyLoaded, None);
        }

        if !self.load_nro(system, process, self.file.as_ref()) {
            return (ResultStatus::ErrorLoadingNRO, None);
        }

        // Upstream: calls ReadProgramId and registers with FileSystemController.
        // FileSystemController registration (system.GetFileSystemController().RegisterProcess)
        // is not yet wired into the loader, matching the same limitation as the NSO loader.

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

    fn read_icon(&self, buffer: &mut Vec<u8>) -> ResultStatus {
        if self.icon_data.is_empty() {
            return ResultStatus::ErrorNoIcon;
        }
        *buffer = self.icon_data.clone();
        ResultStatus::Success
    }

    fn read_program_id(&self, out_program_id: &mut u64) -> ResultStatus {
        match &self.nacp {
            Some(nacp) => {
                *out_program_id = nacp.get_title_id();
                ResultStatus::Success
            }
            None => ResultStatus::ErrorNoControl,
        }
    }

    fn read_rom_fs(&self, out_file: &mut Option<VirtualFile>) -> ResultStatus {
        match &self.romfs {
            Some(f) => {
                *out_file = Some(f.clone());
                ResultStatus::Success
            }
            None => ResultStatus::ErrorNoRomFS,
        }
    }

    fn read_title(&self, title: &mut String) -> ResultStatus {
        match &self.nacp {
            Some(nacp) => {
                *title = nacp.get_application_name();
                ResultStatus::Success
            }
            None => ResultStatus::ErrorNoControl,
        }
    }

    fn read_control_data(&self, control: &mut NACP) -> ResultStatus {
        match &self.nacp {
            Some(nacp) => {
                *control = nacp.clone();
                ResultStatus::Success
            }
            None => ResultStatus::ErrorNoControl,
        }
    }

    fn is_rom_fs_updatable(&self) -> bool {
        false
    }
}
