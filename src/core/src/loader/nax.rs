// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/loader/nax.h and nax.cpp
//!
//! NAX (Nintendo Aes Xts) loader.

use crate::crypto::key_manager::KeyManager;
use crate::file_sys::partition_filesystem::ResultStatus as FsResultStatus;
use crate::file_sys::vfs::vfs_types::VirtualFile;
use crate::file_sys::xts_archive::Nax;

use super::loader::{
    AppLoader, FileType, FileTypeIdentifier, KProcess, LoadResult, Modules, ResultStatus, System,
};
use super::nca::AppLoaderNca;

// ============================================================================
// IdentifyTypeImpl — anonymous-namespace helper
// ============================================================================

/// Maps to upstream anonymous-namespace `IdentifyTypeImpl(const FileSys::NAX&)`.
fn identify_type_impl(nax: &Nax) -> FileType {
    if nax.get_status() != ResultStatus::Success {
        return FileType::Error;
    }

    let nca = nax.as_nca();
    match nca {
        Some(nca) if nca.get_status() == FsResultStatus::Success => FileType::NAX,
        _ => FileType::Error,
    }
}

// ============================================================================
// AppLoaderNax
// ============================================================================

/// Loads a NAX file.
///
/// Maps to upstream `Loader::AppLoader_NAX`.
pub struct AppLoaderNax {
    _file: VirtualFile,
    is_loaded: bool,
    nax: Nax,
    nca_loader: Option<AppLoaderNca>,
}

impl FileTypeIdentifier for AppLoaderNax {
    /// Identifies whether or not the given file is a NAX file.
    ///
    /// Maps to upstream `AppLoader_NAX::IdentifyType`.
    fn identify_type(nax_file: &VirtualFile) -> FileType {
        let nax = Nax::new(nax_file.clone());
        identify_type_impl(&nax)
    }
}

impl AppLoaderNax {
    /// Create a new NAX loader.
    ///
    /// Maps to upstream `AppLoader_NAX::AppLoader_NAX`.
    ///
    /// Upstream: creates `FileSys::NAX` from file, then creates
    /// `AppLoader_NCA` from `nax->GetDecrypted()`.
    pub fn new(file: VirtualFile) -> Self {
        let nax = Nax::new(file.clone());
        let nca_loader = nax.get_decrypted().map(AppLoaderNca::new);
        Self {
            _file: file,
            is_loaded: false,
            nax,
            nca_loader,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_sys::vfs::vfs_vector::VectorVfsFile;
    use std::sync::Arc;

    #[test]
    fn loader_retains_its_base_file_in_addition_to_nax() {
        let file: VirtualFile = Arc::new(VectorVfsFile::new(
            Vec::new(),
            "free-homebrew.nax".to_string(),
            None,
        ));
        let observer = file.clone();
        let loader = AppLoaderNax::new(file);

        assert_eq!(Arc::strong_count(&observer), 3);
        drop(loader);
        assert_eq!(Arc::strong_count(&observer), 1);
    }
}

impl AppLoader for AppLoaderNax {
    /// Maps to upstream `AppLoader_NAX::GetFileType`.
    fn read_hardware_profile(&mut self, system: &mut System)
        -> Result<crate::hardware_properties::HardwareProfile, ResultStatus> {
        if self.nax.get_status() != ResultStatus::Success { return Err(self.nax.get_status()); }
        let Some(nca) = self.nax.as_nca() else {
            return Err(if !KeyManager::key_file_exists(false) { ResultStatus::ErrorMissingProductionKeyFile }
                else { ResultStatus::ErrorNAXInconvertibleToNCA });
        };
        if nca.get_status() != FsResultStatus::Success { return Err(map_fs_status(nca.get_status())); }
        self.nca_loader.as_mut().ok_or(ResultStatus::ErrorNAXInconvertibleToNCA)?.read_hardware_profile(system)
    }

    fn get_file_type(&self) -> FileType {
        identify_type_impl(&self.nax)
    }

    /// Maps to upstream `AppLoader_NAX::Load`.
    fn load(&mut self, process: &mut KProcess, system: &mut System) -> LoadResult {
        if self.is_loaded {
            return (ResultStatus::ErrorAlreadyLoaded, None);
        }

        let nax_status = self.nax.get_status();
        if nax_status != ResultStatus::Success {
            return (nax_status, None);
        }

        let nca = self.nax.as_nca();
        match nca {
            None => {
                if !KeyManager::key_file_exists(false) {
                    return (ResultStatus::ErrorMissingProductionKeyFile, None);
                }
                (ResultStatus::ErrorNAXInconvertibleToNCA, None)
            }
            Some(nca) => {
                let nca_status = nca.get_status();
                if nca_status != FsResultStatus::Success {
                    return (map_fs_status(nca_status), None);
                }

                let nca_loader = match self.nca_loader.as_mut() {
                    Some(loader) => loader,
                    None => return (ResultStatus::ErrorNAXInconvertibleToNCA, None),
                };

                let result = nca_loader.load(process, system);
                if result.0 != ResultStatus::Success {
                    return result;
                }

                self.is_loaded = true;
                result
            }
        }
    }

    /// Maps to upstream `AppLoader_NAX::ReadRomFS`.
    fn read_rom_fs(&self, out_file: &mut Option<VirtualFile>) -> ResultStatus {
        match &self.nca_loader {
            Some(loader) => loader.read_rom_fs(out_file),
            None => ResultStatus::ErrorNotImplemented,
        }
    }

    /// Maps to upstream `AppLoader_NAX::ReadProgramId`.
    fn read_program_id(&self, out_program_id: &mut u64) -> ResultStatus {
        match &self.nca_loader {
            Some(loader) => loader.read_program_id(out_program_id),
            None => ResultStatus::ErrorNotImplemented,
        }
    }

    /// Maps to upstream `AppLoader_NAX::ReadBanner`.
    fn read_banner(&self, buffer: &mut Vec<u8>) -> ResultStatus {
        match &self.nca_loader {
            Some(loader) => loader.read_banner(buffer),
            None => ResultStatus::ErrorNotImplemented,
        }
    }

    /// Maps to upstream `AppLoader_NAX::ReadLogo`.
    fn read_logo(&self, buffer: &mut Vec<u8>) -> ResultStatus {
        match &self.nca_loader {
            Some(loader) => loader.read_logo(buffer),
            None => ResultStatus::ErrorNotImplemented,
        }
    }

    /// Maps to upstream `AppLoader_NAX::ReadNSOModules`.
    fn read_nso_modules(&self, modules: &mut Modules) -> ResultStatus {
        match &self.nca_loader {
            Some(loader) => loader.read_nso_modules(modules),
            None => ResultStatus::ErrorNotImplemented,
        }
    }
}

/// Map file_sys ResultStatus to loader ResultStatus.
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
