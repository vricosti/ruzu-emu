// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/loader/nca.h and nca.cpp
//!
//! NCA (Nintendo Content Archive) loader.

use std::sync::Arc;

use crate::file_sys::content_archive::{NCAContentType, NCA};
use crate::file_sys::nca_metadata::ContentRecordType;
use crate::file_sys::partition_filesystem::ResultStatus as FsResultStatus;
use crate::file_sys::registered_cache::{get_update_title_id, ContentProvider};
use crate::file_sys::romfs_factory::RomFSFactory;
use crate::file_sys::vfs::vfs_types::VirtualFile;

use super::deconstructed_rom_directory::AppLoaderDeconstructedRomDirectory;
use super::loader::{
    AppLoader, FileType, FileTypeIdentifier, KProcess, LoadResult, Modules, ResultStatus, System,
};

// ============================================================================
// Constants for VerifyIntegrity
// ============================================================================

/// Maps to upstream `NcaFileNameWithHashLength` in nca.cpp.
const NCA_FILE_NAME_WITH_HASH_LENGTH: usize = 36;

/// Maps to upstream `NcaFileNameHashLength` in nca.cpp.
const NCA_FILE_NAME_HASH_LENGTH: usize = 32;

/// Maps to upstream `NcaSha256HashLength` in nca.cpp.
const NCA_SHA256_HASH_LENGTH: usize = 32;

/// Maps to upstream `NcaSha256HalfHashLength` in nca.cpp.
const NCA_SHA256_HALF_HASH_LENGTH: usize = NCA_SHA256_HASH_LENGTH / 2;

/// Map file_sys ResultStatus to loader ResultStatus.
///
/// Both enums share many variant names; this function converts the file_sys
/// version used by NCA/NSP parsing into the loader version returned by Load().
fn map_fs_status(fs: FsResultStatus) -> ResultStatus {
    match fs {
        FsResultStatus::Success => ResultStatus::Success,
        FsResultStatus::ErrorBadNCAHeader => ResultStatus::ErrorBadNCAHeader,
        FsResultStatus::ErrorMissingKeyAreaKey => ResultStatus::ErrorMissingKeyAreaKey,
        FsResultStatus::ErrorMissingTitlekey => ResultStatus::ErrorMissingTitlekey,
        FsResultStatus::ErrorMissingTitlekek => ResultStatus::ErrorMissingTitlekek,
        FsResultStatus::ErrorMissingBKTRBaseRomFS => ResultStatus::ErrorNotImplemented,
        FsResultStatus::ErrorNullFile => ResultStatus::ErrorNullFile,
        FsResultStatus::ErrorNSPMissingProgramNCA => ResultStatus::ErrorNSPMissingProgramNCA,
        _ => ResultStatus::ErrorNotImplemented,
    }
}

// ============================================================================
// AppLoaderNca
// ============================================================================

/// Loads an NCA file.
///
/// Maps to upstream `Loader::AppLoader_NCA`.
pub struct AppLoaderNca {
    file: VirtualFile,
    is_loaded: bool,
    nca: NCA,
    update_only_program_id: u64,
    directory_loader: Option<AppLoaderDeconstructedRomDirectory>,
}

impl FileTypeIdentifier for AppLoaderNca {
    /// Identifies whether or not the given file is an NCA file.
    ///
    /// Maps to upstream `AppLoader_NCA::IdentifyType`.
    ///
    /// Upstream constructs `FileSys::NCA(nca_file)` and checks
    /// `nca.GetStatus() == Success && nca.GetType() == Program`.
    /// NCA headers are encrypted; proper identification requires the crypto
    /// key infrastructure (which is ported in FileSys::NCA). We construct an NCA
    /// and check status + content type.
    fn identify_type(nca_file: &VirtualFile) -> FileType {
        let nca = NCA::new(nca_file.clone(), None);
        if nca.get_status() == FsResultStatus::Success && nca.get_type() == NCAContentType::Program
        {
            return FileType::NCA;
        }
        FileType::Error
    }
}

impl AppLoaderNca {
    /// Create a new NCA loader.
    ///
    /// Maps to upstream `AppLoader_NCA::AppLoader_NCA`.
    pub fn new(file: VirtualFile) -> Self {
        Self::new_with_update_only(file, 0)
    }

    /// Create a new NCA loader for an update-only indexed program.
    ///
    /// Maps to upstream `AppLoader_NCA(file, update_only_program_id)`.
    pub fn new_with_update_only(file: VirtualFile, update_only_program_id: u64) -> Self {
        let nca = NCA::new_with_options(file.clone(), None, update_only_program_id != 0);
        Self {
            file,
            is_loaded: false,
            nca,
            update_only_program_id,
            directory_loader: None,
        }
    }

    /// Maps to upstream `AppLoader_NCA::GetProgramId`.
    pub fn program_id(&self) -> u64 {
        if self.update_only_program_id != 0 {
            self.update_only_program_id
        } else {
            self.nca.get_title_id()
        }
    }
}

impl AppLoader for AppLoaderNca {
    fn get_file_type(&self) -> FileType {
        Self::identify_type(&self.file)
    }

    /// Maps to upstream `AppLoader_NCA::Load`.
    fn load(&mut self, process: &mut KProcess, system: &mut System) -> LoadResult {
        if self.is_loaded {
            return (ResultStatus::ErrorAlreadyLoaded, None);
        }

        let nca_status = self.nca.get_status();
        if nca_status != FsResultStatus::Success {
            return (map_fs_status(nca_status), None);
        }

        if self.nca.get_type() != NCAContentType::Program {
            return (ResultStatus::ErrorNCANotProgram, None);
        }

        let mut exefs = self.nca.get_exefs();
        if exefs.is_none() {
            log::info!("No ExeFS found in NCA, looking for ExeFS from update");

            // Upstream: const auto& installed = system.GetContentProvider();
            //           const auto update_nca = installed.GetEntry(
            //               FileSys::GetUpdateTitleID(nca->GetTitleId()), Program);
            if let Some(ref cp) = system.content_provider {
                let cp_guard = cp.lock().unwrap();
                let update_title_id = get_update_title_id(self.nca.get_title_id());
                let update_nca = cp_guard.get_entry(update_title_id, ContentRecordType::Program);
                if let Some(update) = update_nca {
                    exefs = update.get_exefs();
                }
            }

            if exefs.is_none() {
                return (ResultStatus::ErrorNoExeFS, None);
            }
        }

        let exefs = exefs.unwrap();

        // Upstream: directory_loader = make_unique<AppLoader_DeconstructedRomDirectory>(exefs, true);
        // The second arg `true` means override_update=true, is_hbl=false.
        let mut dir_loader =
            AppLoaderDeconstructedRomDirectory::new_from_directory(exefs, true, false);

        let load_result = dir_loader.load(process, system);
        if load_result.0 != ResultStatus::Success {
            self.directory_loader = Some(dir_loader);
            return load_result;
        }

        // Keep the loaded directory owned by this loader before constructing
        // the factory, matching upstream's `directory_loader` ownership at
        // this point in `AppLoader_NCA::Load`.
        self.directory_loader = Some(dir_loader);

        // Upstream: system.GetFileSystemController().RegisterProcess(
        //     process.GetProcessId(), nca->GetTitleId(),
        //     make_shared<RomFSFactory>(*this, system.GetContentProvider(),
        //                               system.GetFileSystemController()));
        // `Process::initialize` assigns the process id before invoking the
        // loader, so both application and LLE applet processes must be fully
        // registered here. The application load path may replace the same
        // registration later when applying its packed-update state.
        if let Some(ref fsc) = system.filesystem_controller {
            let romfs_factory = system.content_provider.as_ref().map(|content_provider| {
                Arc::new(RomFSFactory::new(
                    self,
                    Arc::clone(content_provider),
                    Arc::clone(fsc),
                ))
            });
            fsc.lock().unwrap().register_process(
                process.process_id,
                self.program_id(),
                romfs_factory,
            );
        }

        self.is_loaded = true;
        load_result
    }

    /// Maps to upstream `AppLoader_NCA::VerifyIntegrity`.
    fn verify_integrity(&self, progress_callback: &dyn Fn(usize, usize) -> bool) -> ResultStatus {
        let name = self.file.get_name();

        // We won't try to verify meta NCAs.
        if name.ends_with(".cnmt.nca") {
            return ResultStatus::Success;
        }

        // Check if we can verify this file. NCAs should be named after their hashes.
        if !name.ends_with(".nca") || name.len() != NCA_FILE_NAME_WITH_HASH_LENGTH {
            log::warn!("Unable to validate NCA with name {}", name);
            return ResultStatus::ErrorIntegrityVerificationNotImplemented;
        }

        // Get the expected truncated hash of the NCA.
        let hash_hex = &name[..NCA_FILE_NAME_HASH_LENGTH];
        let input_hash = match hex_string_to_bytes(hash_hex) {
            Some(h) => h,
            None => {
                log::warn!("Unable to parse hash from NCA filename {}", name);
                return ResultStatus::ErrorIntegrityVerificationNotImplemented;
            }
        };

        // Compute SHA-256 of the file.
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();

        let total_size = self.file.get_size();
        let mut processed_size = 0usize;
        let buffer_size = 4 * 1024 * 1024; // 4 MiB

        while processed_size < total_size {
            let intended_read_size = std::cmp::min(buffer_size, total_size - processed_size);
            let data = self.file.read_bytes(intended_read_size, processed_size);
            let read_size = data.len();

            hasher.update(&data);
            processed_size += read_size;

            if !progress_callback(processed_size, total_size) {
                return ResultStatus::ErrorIntegrityVerificationFailed;
            }
        }

        let output_hash = hasher.finalize();

        // Compare first half of the hash.
        if input_hash.len() < NCA_SHA256_HALF_HASH_LENGTH
            || output_hash[..NCA_SHA256_HALF_HASH_LENGTH]
                != input_hash[..NCA_SHA256_HALF_HASH_LENGTH]
        {
            log::error!("NCA hash mismatch detected for file {}", name);
            return ResultStatus::ErrorIntegrityVerificationFailed;
        }

        ResultStatus::Success
    }

    /// Maps to upstream `AppLoader_NCA::ReadRomFS`.
    fn read_rom_fs(&self, out_file: &mut Option<VirtualFile>) -> ResultStatus {
        if self.nca.get_status() != FsResultStatus::Success {
            return ResultStatus::ErrorNotInitialized;
        }

        match self.nca.get_romfs() {
            Some(romfs) if romfs.get_size() > 0 => {
                *out_file = Some(romfs);
                ResultStatus::Success
            }
            _ => ResultStatus::ErrorNoRomFS,
        }
    }

    /// Maps to upstream `AppLoader_NCA::ReadProgramId`.
    fn read_program_id(&self, out_program_id: &mut u64) -> ResultStatus {
        if self.nca.get_status() != FsResultStatus::Success {
            return ResultStatus::ErrorNotInitialized;
        }

        *out_program_id = self.program_id();
        ResultStatus::Success
    }

    /// Maps to upstream `AppLoader_NCA::ReadBanner`.
    fn read_banner(&self, buffer: &mut Vec<u8>) -> ResultStatus {
        if self.nca.get_status() != FsResultStatus::Success {
            return ResultStatus::ErrorNotInitialized;
        }

        let logo = match self.nca.get_logo_partition() {
            Some(l) => l,
            None => return ResultStatus::ErrorNoIcon,
        };

        match logo.get_file("StartupMovie.gif") {
            Some(f) => {
                *buffer = f.read_all_bytes();
                ResultStatus::Success
            }
            None => ResultStatus::ErrorNoIcon,
        }
    }

    /// Maps to upstream `AppLoader_NCA::ReadLogo`.
    fn read_logo(&self, buffer: &mut Vec<u8>) -> ResultStatus {
        if self.nca.get_status() != FsResultStatus::Success {
            return ResultStatus::ErrorNotInitialized;
        }

        let logo = match self.nca.get_logo_partition() {
            Some(l) => l,
            None => return ResultStatus::ErrorNoIcon,
        };

        match logo.get_file("NintendoLogo.png") {
            Some(f) => {
                *buffer = f.read_all_bytes();
                ResultStatus::Success
            }
            None => ResultStatus::ErrorNoIcon,
        }
    }

    /// Maps to upstream `AppLoader_NCA::ReadNSOModules`.
    fn read_nso_modules(&self, modules: &mut Modules) -> ResultStatus {
        match &self.directory_loader {
            Some(loader) => loader.read_nso_modules(modules),
            None => ResultStatus::ErrorNotInitialized,
        }
    }
}

/// Convert a hex string to bytes.
///
/// Maps to upstream `Common::HexStringToVector`.
fn hex_string_to_bytes(hex: &str) -> Option<Vec<u8>> {
    if hex.len() % 2 != 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for i in (0..hex.len()).step_by(2) {
        let byte = u8::from_str_radix(&hex[i..i + 2], 16).ok()?;
        bytes.push(byte);
    }
    Some(bytes)
}
