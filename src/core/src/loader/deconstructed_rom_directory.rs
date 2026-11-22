// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/loader/deconstructed_rom_directory.h and
//! deconstructed_rom_directory.cpp
//!
//! Loads a "deconstructed ROM directory", which are the typical format for
//! Switch game dumps. The path should be a "main" NSO in a directory that
//! contains the other standard ExeFS NSOs (rtld, sdk, etc.).
//! Furthermore, it will look for the first .romfs file (optionally) and use
//! this for the RomFS.
//!
//! The loose RomFS lookup (`find_loose_rom_fs_directory`, `find_loose_rom_fs`,
//! `loose_rom_fs`) and the LayeredFS step in `load` are ported from Lichii's
//! src/core/loader/deconstructed_rom_directory.cpp (commits 94e97ed8dd,
//! dfa55d1679, f8813998e7, 8e777214b0); Eden has neither.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use crate::file_sys::control_metadata::NACP;
use crate::file_sys::nca_metadata::ContentRecordType;
use crate::file_sys::vfs::vfs::VfsFile;
use crate::file_sys::patch_manager::PatchManager;
use crate::file_sys::program_metadata::ProgramMetadata;
use crate::file_sys::vfs::vfs_types::{VirtualDir, VirtualFile};
use crate::hle::result::RESULT_SUCCESS;

use super::loader::{
    AppLoader, FileType, FileTypeIdentifier, KProcess, LoadParameters, LoadResult, Modules,
    ResultStatus, System,
};

// ============================================================================
// Static module list
// ============================================================================

/// The standard set of ExeFS modules, in load order.
///
/// Maps to upstream `static_modules` array in deconstructed_rom_directory.cpp.
const STATIC_MODULES: &[&str] = &[
    "rtld", "main", "subsdk0", "subsdk1", "subsdk2", "subsdk3", "subsdk4", "subsdk5", "subsdk6",
    "subsdk7", "subsdk8", "subsdk9", "sdk",
];

// ============================================================================
// Loose RomFS lookup (Lichii deconstructed_rom_directory.cpp, anonymous namespace)
// ============================================================================

/// The `romfs` subdirectory of `parent`, matched without regard to case.
///
/// Maps to Lichii `FindLooseRomFSDirectory` (deconstructed_rom_directory.cpp:85-101).
fn find_loose_rom_fs_directory(parent: Option<&VirtualDir>) -> Option<VirtualDir> {
    let parent = parent?;
    if let Some(dir) = parent.get_subdirectory("romfs") {
        return Some(dir);
    }
    for dir in parent.get_subdirectories() {
        let name = dir.get_name();
        // Lichii lowers each byte of the directory name (std::tolower) and
        // compares it with "romfs".
        if name.len() == 5
            && name
                .bytes()
                .zip("romfs".bytes())
                .all(|(a, b)| a.to_ascii_lowercase() == b)
        {
            return Some(dir);
        }
    }
    None
}

/// Builds the RomFS image of a loose `romfs` directory, or reuses the build
/// that another loader of the same directory still holds.
///
/// Maps to the `build_from` lambda of Lichii `FindLooseRomFS`
/// (deconstructed_rom_directory.cpp:115-137).
fn build_from(romfs_dir: Option<VirtualDir>) -> Option<VirtualFile> {
    let romfs_dir = romfs_dir?;
    // Building it walks every file and keeps an object for each (title 1: 206k files, ~150
    // MiB), so loaders of the same directory share one build while any of them holds it.
    static CACHE: Mutex<BTreeMap<String, Weak<dyn VfsFile>>> = Mutex::new(BTreeMap::new());
    let mut cache = CACHE.lock().unwrap();
    // Lichii's std::map operator[] default-constructs an empty weak_ptr.
    let cached = cache
        .entry(romfs_dir.get_full_path())
        .or_insert_with(|| Weak::<crate::file_sys::vfs::vfs_vector::VectorVfsFile>::new());
    if let Some(built) = cached.upgrade() {
        return Some(built);
    }
    log::info!("Using RomFS directory next to the ExeFS");
    let started = std::time::Instant::now();
    let built = crate::file_sys::romfs::create_romfs(Some(romfs_dir), None);
    log::info!(
        "Loose RomFS image ready: elapsed_ms={}",
        started.elapsed().as_millis()
    );
    *cached = match &built {
        Some(built) => Arc::downgrade(built),
        None => Weak::<crate::file_sys::vfs::vfs_vector::VectorVfsFile>::new(),
    };
    built
}

/// The size of the `main` file of `dir`, or 0.
///
/// Maps to the `main_size` lambda of Lichii `FindLooseRomFS`
/// (deconstructed_rom_directory.cpp:172-182).
fn main_size(dir: Option<&VirtualDir>) -> u64 {
    let Some(dir) = dir else {
        return 0;
    };
    for candidate in dir.get_files() {
        if candidate.get_name() == "main" {
            return candidate.get_size() as u64;
        }
    }
    0
}

/// RomFS for a loose ExeFS: the first .romfs file next to main, else a romfs/ directory next to
/// the exefs/ directory (the usual layout of an extracted title).
///
/// Maps to Lichii `FindLooseRomFS` (deconstructed_rom_directory.cpp:103-202).
fn find_loose_rom_fs(exefs_dir: Option<&VirtualDir>) -> Option<VirtualFile> {
    let exefs_dir = exefs_dir?;
    for f in exefs_dir.get_files() {
        if f.get_extension() == "romfs" {
            return Some(f);
        }
    }

    if let Some(parent) = exefs_dir.get_parent_directory() {
        if let Some(romfs_dir) = find_loose_rom_fs_directory(Some(&parent)) {
            return build_from(Some(romfs_dir));
        }
        // Some base dumps carry only the executable and leave every file in an update, so the
        // base has no romfs at all (base/romfs does not exist, the data is in
        // updates/<version>/romfs). Handing a null RomFS to the title makes
        // OpenDataStorageByCurrentProcess fail and the game panics with an opaque 0x3fffff
        // instead of saying where the data is. Prefer the newest update present.
        if let Some(root) = parent.get_parent_directory() {
            if let Some(updates) = root.get_subdirectory("updates") {
                let mut newest: Option<VirtualDir> = None;
                let mut newest_version: u64 = 0;
                for candidate in updates.get_subdirectories() {
                    let name = candidate.get_name();
                    // from_chars stops at the first non digit and still reports success, so a
                    // directory called "655360_old" or "999999.bak" would be accepted with its
                    // leading digits. Require the whole name to be the number.
                    if name.is_empty() || !name.bytes().all(|c| c.is_ascii_digit()) {
                        continue;
                    }
                    // Out of range also fails from_chars (errc::result_out_of_range).
                    let Ok(version) = name.parse::<u64>() else {
                        continue;
                    };
                    if version >= newest_version {
                        newest_version = version;
                        newest = Some(candidate);
                    }
                }
                if let Some(romfs_dir) = find_loose_rom_fs_directory(newest.as_ref()) {
                    // Only safe when the executable is the same build as the update: running a
                    // 1.0 program against 2.0.1 data boots and then writes to unmapped memory in
                    // a loop, which is worse than refusing.
                    let base_exefs_size = main_size(Some(exefs_dir));
                    let update_exefs_size = main_size(
                        newest
                            .as_ref()
                            .and_then(|newest| newest.get_subdirectory("exefs"))
                            .as_ref(),
                    );
                    if base_exefs_size != 0
                        && update_exefs_size != 0
                        && base_exefs_size != update_exefs_size
                    {
                        log::error!(
                            "Base RomFS is absent and the update RomFS belongs to a different \
                             build (base main {} bytes, update main {} bytes). Load the \
                             update's exefs instead of the base one.",
                            base_exefs_size,
                            update_exefs_size
                        );
                        return None;
                    }
                    log::info!(
                        "Base RomFS is absent, using the update RomFS (version {})",
                        newest_version
                    );
                    return build_from(Some(romfs_dir));
                }
            }
        }
    }
    None
}

// ============================================================================
// AppLoaderDeconstructedRomDirectory
// ============================================================================

/// Lichii's `romfs` and `romfs_resolved` members. Ruzu's `AppLoader::read_rom_fs`
/// takes `&self` (Lichii's `ReadRomFS` is non-const), so the lazily resolved
/// pair lives behind one mutex.
#[derive(Default)]
struct LooseRomFsState {
    romfs: Option<VirtualFile>,
    romfs_resolved: bool,
}

/// Loads a deconstructed ROM directory (ExeFS dump).
///
/// Maps to upstream `Loader::AppLoader_DeconstructedRomDirectory`.
pub struct AppLoaderDeconstructedRomDirectory {
    file: Option<VirtualFile>,
    dir: Option<VirtualDir>,
    is_loaded: bool,
    // Preflight and Load share the effective NPDM after upstream ExeFS patching.
    prepared_metadata: Option<ProgramMetadata>,
    /// The loose RomFS next to the ExeFS, found (and built) on first use.
    romfs: Mutex<LooseRomFsState>,
    /// Where `loose_rom_fs` looks.
    romfs_exefs_dir: Option<VirtualDir>,
    icon_data: Vec<u8>,
    name: String,
    title_id: u64,
    override_update: bool,
    is_hbl: bool,
    modules: Modules,
}

impl FileTypeIdentifier for AppLoaderDeconstructedRomDirectory {
    /// Identifies whether or not the given file is a deconstructed ROM directory.
    ///
    /// Maps to upstream `AppLoader_DeconstructedRomDirectory::IdentifyType`.
    fn identify_type(dir_file: &VirtualFile) -> FileType {
        // Upstream checks: FileSys::IsDirectoryExeFS(dir_file->GetContainingDirectory())
        if let Some(containing_dir) = dir_file.get_containing_directory() {
            if is_directory_exefs(&containing_dir) {
                return FileType::DeconstructedRomDirectory;
            }
        }

        FileType::Error
    }
}

/// Check if a directory is an ExeFS directory.
///
/// Maps to upstream `FileSys::IsDirectoryExeFS`.
/// Inline implementation matching upstream: checks for "main" and "main.npdm" files.
fn is_directory_exefs(dir: &VirtualDir) -> bool {
    // An ExeFS directory contains "main" and "main.npdm" files.
    let has_main = dir.get_file("main").is_some();
    let has_npdm = dir.get_file("main.npdm").is_some();
    has_main && has_npdm
}

// Mechanical bridge: upstream uses one ResultStatus enum for FileSys and Loader.
fn map_metadata_status(result: crate::file_sys::partition_filesystem::ResultStatus) -> ResultStatus {
    use crate::file_sys::partition_filesystem::ResultStatus as PfsResultStatus;
    match result {
        PfsResultStatus::ErrorBadNPDMHeader => ResultStatus::ErrorBadNPDMHeader,
        PfsResultStatus::ErrorBadACIDHeader => ResultStatus::ErrorBadACIDHeader,
        PfsResultStatus::ErrorBadACIHeader => ResultStatus::ErrorBadACIHeader,
        PfsResultStatus::ErrorBadFileAccessControl => ResultStatus::ErrorBadFileAccessControl,
        PfsResultStatus::ErrorBadFileAccessHeader => ResultStatus::ErrorBadFileAccessHeader,
        PfsResultStatus::ErrorBadKernelCapabilityDescriptors => ResultStatus::ErrorBadKernelCapabilityDescriptors,
        _ => ResultStatus::ErrorBadNPDMHeader,
    }
}

impl AppLoaderDeconstructedRomDirectory {
    // Mechanical split of Load's metadata prefix for pre-kernel selection.
    // Cache the effective patched directory and metadata so Load cannot select
    // a different profile or apply an update a second time.
    fn prepare_metadata(&mut self, system: &mut System) -> Result<(), ResultStatus> {
        use crate::file_sys::partition_filesystem::ResultStatus as PfsResultStatus;
        if self.prepared_metadata.is_some() { return Ok(()); }
        // Resolve dir from file if needed
        if self.dir.is_none() {
            match &self.file {
                Some(f) => {
                    self.dir = f.get_containing_directory();
                }
                None => return Err(ResultStatus::ErrorNullFile),
            }
        }

        let mut dir = match &self.dir {
            Some(d) => d.clone(),
            None => return Err(ResultStatus::ErrorNullFile),
        };

        // Read meta to determine title ID
        let npdm_file = match dir.get_file("main.npdm") {
            Some(f) => f,
            None => return Err(ResultStatus::ErrorMissingNPDM),
        };

        let mut metadata = ProgramMetadata::new();
        let result = metadata.load(npdm_file);
        if result != PfsResultStatus::Success {
            // Map FileSys ResultStatus to Loader ResultStatus.
            // Upstream: both share the same ResultStatus enum, so the mapping
            // is identity. In Rust they are separate enums; map by name.
            return Err(map_metadata_status(result));
        }

        if self.override_update {
            if let (Some(fs_controller), Some(content_provider)) =
                (&system.filesystem_controller, &system.content_provider)
            {
                let fs_guard = fs_controller.lock().unwrap();
                let content_guard = content_provider.lock().unwrap();
                let patch_manager =
                    PatchManager::new(metadata.get_title_id(), &fs_guard, &*content_guard);
                dir = patch_manager.patch_exefs(dir);
            } else {
                log::warn!(
                    "Unable to patch ExeFS for {:016X}: missing filesystem/content provider",
                    metadata.get_title_id()
                );
            }
        }

        // Upstream rereads main.npdm after PatchExeFS because an update or
        // LayeredExeFS mod may replace it.
        let npdm_file = match dir.get_file("main.npdm") {
            Some(f) => f,
            None => return Err(ResultStatus::ErrorMissingNPDM),
        };
        let result = metadata.reload(npdm_file);
        if result != PfsResultStatus::Success {
            return Err(map_metadata_status(result));
        }

        self.dir = Some(dir);
        self.prepared_metadata = Some(metadata);
        Ok(())
    }

    /// Create a new deconstructed ROM directory loader from a main NSO file.
    ///
    /// Maps to upstream `AppLoader_DeconstructedRomDirectory(VirtualFile, bool)`.
    pub fn new_from_file(file: VirtualFile) -> Self {
        let mut title_id = 0u64;
        let mut icon_data = Vec::new();
        let mut name = String::new();
        let file_dir = file.get_containing_directory();

        // Try to read metadata from the containing directory.
        // Matches upstream constructor behavior.
        if let Some(ref file_dir) = file_dir {
            // Title ID: parse main.npdm to get title ID.
            // Upstream: `metadata.Load(npdm)` then `title_id = metadata.GetTitleID()`.
            if let Some(npdm) = file_dir.get_file("main.npdm") {
                let mut metadata = ProgramMetadata::new();
                let res = metadata.load(npdm);
                if res == crate::file_sys::partition_filesystem::ResultStatus::Success {
                    title_id = metadata.get_title_id();
                }
            }

            // Icon: look for icon_<language>.dat
            // Inline language name list matching upstream FileSys::LANGUAGE_NAMES.
            let language_names = [
                "AmericanEnglish",
                "BritishEnglish",
                "Japanese",
                "French",
                "German",
                "LatinAmericanSpanish",
                "Spanish",
                "Italian",
                "Dutch",
                "CanadianFrench",
                "Portuguese",
                "Russian",
                "Korean",
                "TraditionalChinese",
                "SimplifiedChinese",
                "BrazilianPortuguese",
            ];

            for lang in &language_names {
                let icon_filename = format!("icon_{}.dat", lang);
                if let Some(icon_file) = file_dir.get_file(&icon_filename) {
                    icon_data = icon_file.read_all_bytes();
                    break;
                }
            }

            if icon_data.is_empty() {
                // Any png, jpeg, or bmp file
                for f in file_dir.get_files() {
                    let ext = f.get_extension();
                    if ext == "png" || ext == "jpg" || ext == "bmp" || ext == "jpeg" {
                        icon_data = f.read_all_bytes();
                        break;
                    }
                }
            }

            // Metadata: look for control.nacp or any .nacp file.
            // Upstream: `FileSys::NACP nacp(nacp_file); name = nacp.GetApplicationName();`
            let nacp_file = file_dir.get_file("control.nacp").or_else(|| {
                file_dir
                    .get_files()
                    .into_iter()
                    .find(|f| f.get_extension() == "nacp")
            });

            if let Some(nacp_file) = nacp_file {
                let nacp = NACP::from_file(&nacp_file);
                name = nacp.get_application_name();
            }
        }

        Self {
            file: Some(file),
            dir: None,
            is_loaded: false,
            prepared_metadata: None,
            romfs: Mutex::new(LooseRomFsState::default()),
            // Lichii: romfs_exefs_dir = file_dir;
            romfs_exefs_dir: file_dir,
            icon_data,
            name,
            title_id,
            override_update: false,
            is_hbl: false,
            modules: Modules::new(),
        }
    }

    /// Create a new deconstructed ROM directory loader from an ExeFS directory.
    ///
    /// Maps to upstream `AppLoader_DeconstructedRomDirectory(VirtualDir, bool, bool)`.
    pub fn new_from_directory(directory: VirtualDir, override_update: bool, is_hbl: bool) -> Self {
        let file = directory.get_file("main");
        Self {
            file,
            // Lichii: romfs_exefs_dir = dir;
            romfs_exefs_dir: Some(directory.clone()),
            dir: Some(directory),
            is_loaded: false,
            prepared_metadata: None,
            romfs: Mutex::new(LooseRomFsState::default()),
            icon_data: Vec::new(),
            name: String::new(),
            title_id: 0,
            override_update,
            is_hbl,
            modules: Modules::new(),
        }
    }

    /// Loaders made only for metadata (the game list, title lookups) never need the RomFS, and
    /// several of them were alive with a whole copy each.
    ///
    /// Maps to Lichii `AppLoader_DeconstructedRomDirectory::LooseRomFS`
    /// (deconstructed_rom_directory.cpp:374-380).
    fn loose_rom_fs(&self) -> Option<VirtualFile> {
        let mut state = self.romfs.lock().unwrap();
        if !state.romfs_resolved {
            state.romfs_resolved = true;
            state.romfs = find_loose_rom_fs(self.romfs_exefs_dir.as_ref());
        }
        state.romfs.clone()
    }
}

impl AppLoader for AppLoaderDeconstructedRomDirectory {
    fn read_hardware_profile(&mut self, system: &mut System)
        -> Result<crate::hardware_properties::HardwareProfile, ResultStatus> {
        self.prepare_metadata(system)?;
        Ok(if self.prepared_metadata.as_ref().unwrap().is_switch2() {
            crate::hardware_properties::NX2_PROFILE
        } else { crate::hardware_properties::SWITCH1_PROFILE })
    }

    fn get_file_type(&self) -> FileType {
        match &self.file {
            Some(f) => Self::identify_type(f),
            None => FileType::Error,
        }
    }

    /// Maps to upstream `AppLoader_DeconstructedRomDirectory::Load`.
    fn load(&mut self, process: &mut KProcess, system: &mut System) -> LoadResult {

        if self.is_loaded {
            return (ResultStatus::ErrorAlreadyLoaded, None);
        }

        if let Err(error) = self.prepare_metadata(system) { return (error, None); }
        let metadata = self.prepared_metadata.as_ref().expect("prepared metadata");
        let dir = self.dir.as_ref().expect("prepared ExeFS").clone();

        metadata.print();
        self.title_id = metadata.get_title_id();

        // Loose-ExeFS loads bypass the NCA/NSP loaders, which are the ones that normally apply
        // LayeredFS to the RomFS; apply it here so mods in user/load/<titleid>/ work too.
        // Lichii deconstructed_rom_directory.cpp:436-448.
        if let Some(romfs) = self.loose_rom_fs() {
            if let (Some(fs_controller), Some(content_provider)) =
                (&system.filesystem_controller, &system.content_provider)
            {
                let fs_guard = fs_controller.lock().unwrap();
                let content_guard = content_provider.lock().unwrap();
                let patch_manager =
                    PatchManager::new(metadata.get_title_id(), &fs_guard, &*content_guard);
                // Lichii: PatchRomFS(nullptr, romfs) with the header's default arguments.
                // Ruzu's patch_romfs returns a non-null file, as Lichii's does for a non-null
                // base, so the `patched != nullptr` branch is always taken.
                let patched = patch_manager.patch_romfs(
                    None,
                    romfs,
                    ContentRecordType::Program,
                    None,
                    true,
                );
                self.romfs.lock().unwrap().romfs = Some(patched);
                log::info!(
                    "Applied RomFS patches (LayeredFS) for title {:016X}",
                    metadata.get_title_id()
                );
            } else {
                log::warn!(
                    "Unable to patch RomFS for {:016X}: missing filesystem/content provider",
                    metadata.get_title_id()
                );
            }
        }

        // Upstream does not block on 32-bit ISA here; it passes metadata
        // to KProcess::LoadFromMetadata which handles address space setup.
        let is_64bit = metadata.is_64_bit_program();
        if !is_64bit {
            log::info!("Loading a 32-bit (AArch32) program");
        }

        // ====================================================================
        // Pass 1: Layout computation (load_into_process = false)
        // ====================================================================
        // Use the NSO module loader to figure out the code layout.
        // Maps to upstream first loop with `LoadModule(..., false, ...)`.
        let mut code_size: u64 = 0;

        for &module_name in STATIC_MODULES {
            let module_file = match dir.get_file(module_name) {
                Some(f) => f,
                None => continue,
            };

            let should_pass_args = module_name == "rtld";
            match super::nso::AppLoaderNso::load_module(
                process,
                system,
                module_file.as_ref(),
                code_size,
                should_pass_args,
                false, // load_into_process = false
                None,
            ) {
                Some(tentative_next_load_addr) => {
                    code_size = tentative_next_load_addr;
                }
                None => {
                    return (ResultStatus::ErrorLoadingNSO, None);
                }
            }
        }

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

        // ====================================================================
        // Process setup
        // ====================================================================
        // Upstream: process.LoadFromMetadata(metadata, code_size, fastmem_base, aslr_offset)
        // This single call handles everything: InitializeForProcess (creates page table,
        // block manager, address space layout), maps code region, sets permissions,
        // initializes capabilities, assigns process ID, creates PLR.
        let process_setup_result = process.load_from_metadata(
            &metadata,
            code_size,
            0, // fastmem_base; Ruzu does not yet integrate Eden's NCE direct mapping
            aslr_offset,
            self.is_hbl,
        );
        if process_setup_result != RESULT_SUCCESS.get_inner_value() {
            return (ResultStatus::ErrorUnableToParseKernelMetadata, None);
        }

        // ====================================================================
        // Pass 2: Actual loading (load_into_process = true)
        // ====================================================================
        // Maps to upstream second loop with `LoadModule(..., true, ...)`.
        // Upstream: `const VAddr base_address{GetInteger(process.GetEntryPoint())};`
        self.modules.clear();
        let base_address: u64 = process.get_entry_point().get();
        let mut next_load_addr: u64 = base_address;

        let filesystem_controller = system.filesystem_controller.clone();
        let content_provider = system.content_provider.clone();
        let fs_guard = filesystem_controller
            .as_ref()
            .map(|controller| controller.lock().unwrap());
        let content_guard = content_provider
            .as_ref()
            .map(|provider| provider.lock().unwrap());
        let patch_manager = fs_guard
            .as_deref()
            .zip(content_guard.as_deref())
            .map(|(controller, provider)| PatchManager::new(self.title_id, controller, provider));

        for &module_name in STATIC_MODULES {
            let module_file = match dir.get_file(module_name) {
                Some(f) => f,
                None => continue,
            };

            let load_addr = next_load_addr;
            let should_pass_args = module_name == "rtld";
            match super::nso::AppLoaderNso::load_module(
                process,
                system,
                module_file.as_ref(),
                load_addr,
                should_pass_args,
                true, // load_into_process = true
                patch_manager.as_ref(),
            ) {
                Some(tentative_next_load_addr) => {
                    next_load_addr = tentative_next_load_addr;
                    self.modules.insert(load_addr, module_name.to_string());
                    log::debug!("loaded module {} @ {:#X}", module_name, load_addr);
                }
                None => {
                    return (ResultStatus::ErrorLoadingNSO, None);
                }
            }
        }

        self.is_loaded = true;

        (
            ResultStatus::Success,
            Some(LoadParameters {
                main_thread_priority: metadata.get_main_thread_priority() as i32,
                main_thread_stack_size: metadata.get_main_thread_stack_size() as u64,
            }),
        )
    }

    /// Maps to Lichii `AppLoader_DeconstructedRomDirectory::ReadRomFS`
    /// (deconstructed_rom_directory.cpp:547-554).
    fn read_rom_fs(&self, out_file: &mut Option<VirtualFile>) -> ResultStatus {
        match self.loose_rom_fs() {
            Some(f) => {
                *out_file = Some(f);
                ResultStatus::Success
            }
            None => ResultStatus::ErrorNoRomFS,
        }
    }

    fn read_icon(&self, buffer: &mut Vec<u8>) -> ResultStatus {
        if self.icon_data.is_empty() {
            return ResultStatus::ErrorNoIcon;
        }
        *buffer = self.icon_data.clone();
        ResultStatus::Success
    }

    fn read_program_id(&self, out_program_id: &mut u64) -> ResultStatus {
        *out_program_id = self.title_id;
        ResultStatus::Success
    }

    fn read_title(&self, title: &mut String) -> ResultStatus {
        if self.name.is_empty() {
            return ResultStatus::ErrorNoControl;
        }
        *title = self.name.clone();
        ResultStatus::Success
    }

    fn is_rom_fs_updatable(&self) -> bool {
        false
    }

    fn read_nso_modules(&self, out_modules: &mut Modules) -> ResultStatus {
        if !self.is_loaded {
            return ResultStatus::ErrorNotInitialized;
        }
        *out_modules = self.modules.clone();
        ResultStatus::Success
    }
}

#[cfg(test)]
mod loose_rom_fs_tests {
    use super::*;
    use crate::file_sys::fs_filesystem::OpenMode;
    use crate::file_sys::romfs::extract_romfs;
    use crate::file_sys::vfs::vfs_real::RealVfsFilesystem;
    use std::path::{Path, PathBuf};

    /// A temporary game folder removed on drop.
    struct TempTree(PathBuf);

    impl TempTree {
        fn new(tag: &str) -> Self {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "ruzu-loose-romfs-{tag}-{}-{unique}",
                std::process::id()
            ));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        fn write(&self, relative: &str, data: &[u8]) {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, data).unwrap();
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.0.join(relative)
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn loader_for(main: &Path) -> AppLoaderDeconstructedRomDirectory {
        let filesystem = RealVfsFilesystem::new();
        let file = filesystem
            .arc_open_file(&main.to_string_lossy(), OpenMode::READ)
            .expect("main exists");
        AppLoaderDeconstructedRomDirectory::new_from_file(file)
    }

    fn read_rom_fs(loader: &AppLoaderDeconstructedRomDirectory) -> Option<VirtualFile> {
        let mut out = None;
        match loader.read_rom_fs(&mut out) {
            ResultStatus::Success => Some(out.expect("success carries a file")),
            ResultStatus::ErrorNoRomFS => {
                assert!(out.is_none());
                None
            }
            other => panic!("unexpected status {other:?}"),
        }
    }

    fn read_all(dir: &VirtualDir, relative: &str) -> Vec<u8> {
        dir.get_file_relative(relative)
            .unwrap_or_else(|| panic!("{relative} in built RomFS"))
            .read_all_bytes()
    }

    #[test]
    fn romfs_file_next_to_main_is_used_as_is() {
        let tree = TempTree::new("file");
        tree.write("exefs/main", b"NSO0");
        tree.write("exefs/game.romfs", b"raw romfs image");
        // A romfs/ directory next to exefs/ loses to the .romfs file.
        tree.write("romfs/ignored.bin", b"x");
        let loader = loader_for(&tree.path("exefs/main"));
        let romfs = read_rom_fs(&loader).expect("romfs file");
        assert_eq!(romfs.get_name(), "game.romfs");
        assert_eq!(romfs.read_all_bytes(), b"raw romfs image");
    }

    #[test]
    fn romfs_directory_next_to_exefs_is_built_and_reads_back() {
        let tree = TempTree::new("dir");
        tree.write("exefs/main", b"NSO0");
        tree.write("romfs/top.bin", b"top-level data");
        tree.write("romfs/Sound/bgm/track01.bin", &[7u8; 4099]);
        tree.write("romfs/Movie/intro.mp4", &(0..=255u8).cycle().take(70_000).collect::<Vec<_>>());
        tree.write("romfs/Empty/.keep", b"");
        let loader = loader_for(&tree.path("exefs/main"));
        let romfs = read_rom_fs(&loader).expect("built romfs");
        let extracted = extract_romfs(Some(romfs)).expect("built image parses");
        for relative in ["top.bin", "Sound/bgm/track01.bin", "Movie/intro.mp4", "Empty/.keep"] {
            let source = std::fs::read(tree.path(&format!("romfs/{relative}"))).unwrap();
            assert_eq!(read_all(&extracted, relative), source, "{relative}");
        }
    }

    #[test]
    fn romfs_directory_name_is_matched_without_case() {
        let tree = TempTree::new("case");
        tree.write("exefs/main", b"NSO0");
        tree.write("RomFS/data.bin", b"upper case directory");
        let loader = loader_for(&tree.path("exefs/main"));
        let extracted = extract_romfs(read_rom_fs(&loader)).expect("built image parses");
        assert_eq!(read_all(&extracted, "data.bin"), b"upper case directory");
    }

    #[test]
    fn missing_romfs_reports_no_romfs() {
        let tree = TempTree::new("none");
        tree.write("exefs/main", b"NSO0");
        tree.write("exefs/main.npdm", b"");
        tree.write("notromfs/data.bin", b"x");
        let loader = loader_for(&tree.path("exefs/main"));
        assert!(read_rom_fs(&loader).is_none());
        // Still absent on the second read (resolved once).
        assert!(read_rom_fs(&loader).is_none());
    }

    #[test]
    fn base_without_romfs_uses_newest_numeric_update() {
        let tree = TempTree::new("update");
        tree.write("base/exefs/main", b"same build");
        tree.write("updates/65536/romfs/data.bin", b"older update");
        tree.write("updates/131072/romfs/data.bin", b"newest update");
        tree.write("updates/131072/exefs/main", b"same build");
        // Not a whole number: skipped, though its leading digits are larger.
        tree.write("updates/999999_old/romfs/data.bin", b"stale copy");
        let loader = loader_for(&tree.path("base/exefs/main"));
        let extracted = extract_romfs(read_rom_fs(&loader)).expect("update romfs");
        assert_eq!(read_all(&extracted, "data.bin"), b"newest update");
    }

    #[test]
    fn base_without_romfs_refuses_update_of_another_build() {
        let tree = TempTree::new("mismatch");
        tree.write("base/exefs/main", b"build 1.0");
        tree.write("updates/131072/romfs/data.bin", b"2.0 data");
        tree.write("updates/131072/exefs/main", b"build 2.0.1 is larger");
        let loader = loader_for(&tree.path("base/exefs/main"));
        assert!(read_rom_fs(&loader).is_none());
    }

    #[test]
    fn romfs_is_built_on_first_read_and_shared_between_loaders() {
        let tree = TempTree::new("shared");
        tree.write("exefs/main", b"NSO0");
        tree.write("romfs/data.bin", b"shared");
        let first = loader_for(&tree.path("exefs/main"));
        assert!(!first.romfs.lock().unwrap().romfs_resolved, "constructor must not build");
        let second = loader_for(&tree.path("exefs/main"));
        let a = read_rom_fs(&first).unwrap();
        let b = read_rom_fs(&second).unwrap();
        assert!(Arc::ptr_eq(&a, &b), "loaders of one directory share the build");
        drop((a, b, first, second));
        // Once nobody holds it the build is freed and the next read builds again.
        let third = loader_for(&tree.path("exefs/main"));
        let extracted = extract_romfs(read_rom_fs(&third)).unwrap();
        assert_eq!(read_all(&extracted, "data.bin"), b"shared");
    }

    fn resident_kib() -> u64 {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| {
                status
                    .lines()
                    .find(|line| line.starts_with("VmRSS:"))
                    .and_then(|line| line.split_whitespace().nth(1)?.parse().ok())
            })
            .unwrap_or(0)
    }

    /// Measurement on a real extracted title (read only):
    /// RUZU_LOOSE_ROMFS_MAIN=<…>/exefs/main cargo test --release -p core
    ///   loose_rom_fs_real_title -- --ignored --nocapture
    #[test]
    #[ignore]
    fn loose_rom_fs_real_title() {
        let Ok(main) = std::env::var("RUZU_LOOSE_ROMFS_MAIN") else {
            return;
        };
        let rss_before = resident_kib();
        let started = std::time::Instant::now();
        let loader = loader_for(Path::new(&main));
        let romfs = read_rom_fs(&loader).expect("loose romfs");
        let built = started.elapsed();
        let rss_built = resident_kib();
        let mut header = [0u8; 0x50];
        assert_eq!(romfs.read(&mut header, 0x50, 0), 0x50);
        let started = std::time::Instant::now();
        let extracted = extract_romfs(Some(romfs.clone())).expect("image parses");
        let parsed = started.elapsed();
        let started = std::time::Instant::now();
        let mut files = 0usize;
        let mut stack = vec![extracted.clone()];
        while let Some(dir) = stack.pop() {
            files += dir.get_files().len();
            stack.extend(dir.get_subdirectories());
        }
        let walked = started.elapsed();
        let started = std::time::Instant::now();
        drop((stack, extracted, romfs, loader));
        let dropped = started.elapsed();
        println!(
            "build_ms={} parse_ms={} walk_ms={} files={} drop_ms={} \
             rss_before_kib={} rss_built_kib={} rss_after_drop_kib={}",
            built.as_millis(),
            parsed.as_millis(),
            walked.as_millis(),
            files,
            dropped.as_millis(),
            rss_before,
            rss_built,
            resident_kib()
        );
    }
}

#[cfg(test)]
mod hardware_profile_tests {
    use super::*;
    use std::sync::Arc;
    use crate::file_sys::vfs::vfs_vector::{VectorVfsFile, VectorVfsDirectory};
    use crate::hardware_properties::{NX2_PROFILE, SWITCH1_PROFILE};

    fn npdm(nx2: bool) -> Vec<u8> {
        let mut data = vec![0; 0x700];
        data[0x70..0x74].copy_from_slice(&0x80u32.to_le_bytes());
        data[0x78..0x7c].copy_from_slice(&0x200u32.to_le_bytes());
        data[0xa0..0xa4].copy_from_slice(&0x40u32.to_le_bytes());
        let body = if nx2 { 0x500 } else { 0x400 };
        data[body..body + 4].copy_from_slice(b"ACID");
        data[body + 0x20..body + 0x24].copy_from_slice(&0x400u32.to_le_bytes());
        data
    }

    #[test]
    fn hardware_profile_preflight_reads_and_retains_effective_metadata() {
        let mut system = System::new(None, None);
        for profile in [NX2_PROFILE, SWITCH1_PROFILE, NX2_PROFILE] {
            let file = Arc::new(VectorVfsFile::new(npdm(profile == NX2_PROFILE), "main.npdm".into(), None));
            let dir = Arc::new(VectorVfsDirectory::new(vec![file.clone()], vec![], "exefs".into(), None));
            let mut loader = AppLoaderDeconstructedRomDirectory::new_from_directory(dir, false, false);
            assert_eq!(loader.read_hardware_profile(&mut system), Ok(profile));
            // The subsequent Load uses this prepared snapshot, not different metadata.
            file.assign(npdm(profile != NX2_PROFILE));
            assert_eq!(loader.read_hardware_profile(&mut system), Ok(profile));
            assert_eq!(loader.prepared_metadata.as_ref().unwrap().is_switch2(), profile == NX2_PROFILE);
            assert!(!loader.is_loaded);
        }
    }

    #[test]
    fn hardware_profile_failed_load_retry_retains_prepared_metadata() {
        let mut system = System::new(None, None);
        let metadata = Arc::new(VectorVfsFile::new(npdm(true), "main.npdm".into(), None));
        let invalid_main = Arc::new(VectorVfsFile::new(vec![0; 4], "main".into(), None));
        let dir = Arc::new(VectorVfsDirectory::new(vec![metadata.clone(), invalid_main], vec![], "exefs".into(), None));
        let mut loader = AppLoaderDeconstructedRomDirectory::new_from_directory(dir, true, false);
        assert_eq!(loader.read_hardware_profile(&mut system), Ok(NX2_PROFILE));
        for _ in 0..2 {
            assert_eq!(loader.load(&mut KProcess::new(), &mut system).0, ResultStatus::ErrorLoadingNSO);
            // Re-preparing the source would change the selected profile.
            metadata.assign(npdm(false));
            assert_eq!(loader.read_hardware_profile(&mut system), Ok(NX2_PROFILE));
            assert!(loader.prepared_metadata.is_some());
        }
    }

    #[test]
    fn hardware_profile_preflight_propagates_bad_or_missing_npdm() {
        let mut system = System::new(None, None);
        let dir = Arc::new(VectorVfsDirectory::new(vec![], vec![], "exefs".into(), None));
        let mut loader = AppLoaderDeconstructedRomDirectory::new_from_directory(dir, false, false);
        assert_eq!(loader.read_hardware_profile(&mut system), Err(ResultStatus::ErrorMissingNPDM));
        let file = Arc::new(VectorVfsFile::new(vec![], "main.npdm".into(), None));
        let dir = Arc::new(VectorVfsDirectory::new(vec![file], vec![], "exefs".into(), None));
        let mut loader = AppLoaderDeconstructedRomDirectory::new_from_directory(dir, false, false);
        assert_eq!(loader.read_hardware_profile(&mut system), Err(ResultStatus::ErrorBadNPDMHeader));
    }
}
