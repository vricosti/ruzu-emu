// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Ported from: core/file_sys/romfs_factory.h and romfs_factory.cpp
// RomFS factory for opening patched/unpatched RomFS images.

use std::sync::Arc;
use std::sync::Mutex;

use super::common_funcs::get_base_title_id_with_program_index;
use super::content_archive::NCA;
use super::nca_metadata::ContentRecordType;
use super::patch_manager::PatchManager;
use super::registered_cache::ContentProvider;
use super::registered_cache::ContentProviderUnion;
use super::vfs::vfs_types::VirtualFile;

// ============================================================================
// StorageId
// ============================================================================

/// Storage identifier for content.
/// Corresponds to upstream `StorageId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum StorageId {
    None = 0,
    Host = 1,
    GameCard = 2,
    NandSystem = 3,
    NandUser = 4,
    SdCard = 5,
}

// ============================================================================
// RomFSFactory
// ============================================================================

/// File system interface to the RomFS archive.
/// Corresponds to upstream `RomFSFactory`.
///
/// Upstream holds references to `ContentProvider` and `FileSystemController`.
pub struct RomFSFactory {
    file: Option<VirtualFile>,
    packed_update_raw: Mutex<Option<VirtualFile>>,
    updatable: bool,
    content_provider: Option<Arc<Mutex<ContentProviderUnion>>>,
    filesystem_controller: Option<
        Arc<std::sync::Mutex<crate::hle::service::filesystem::filesystem::FileSystemController>>,
    >,
}

impl RomFSFactory {
    /// Create a new RomFSFactory.
    ///
    /// Matches upstream `RomFSFactory::RomFSFactory(AppLoader&, ContentProvider&,
    /// FileSystemController&)` (romfs_factory.cpp:15-22).
    /// Reads the RomFS and updatable flag from the AppLoader.
    pub fn new(
        app_loader: &dyn crate::loader::loader::AppLoader,
        content_provider: Arc<Mutex<ContentProviderUnion>>,
        filesystem_controller: Arc<
            std::sync::Mutex<crate::hle::service::filesystem::filesystem::FileSystemController>,
        >,
    ) -> Self {
        // Upstream: app_loader.ReadRomFS(file)
        let mut file = None;
        if app_loader.read_rom_fs(&mut file) != crate::loader::loader::ResultStatus::Success {
            log::warn!("Unable to read base RomFS");
        }

        // Upstream: updatable = app_loader.IsRomFSUpdatable()
        let updatable = app_loader.is_rom_fs_updatable();

        Self {
            file,
            packed_update_raw: Mutex::new(None),
            updatable,
            content_provider: Some(content_provider),
            filesystem_controller: Some(filesystem_controller),
        }
    }

    /// Create a RomFSFactory with explicit file/updatable args (for tests and
    /// cases where the AppLoader is not available).
    pub fn new_with_file(
        file: Option<VirtualFile>,
        updatable: bool,
        content_provider: Option<Arc<Mutex<ContentProviderUnion>>>,
        filesystem_controller: Option<
            Arc<
                std::sync::Mutex<crate::hle::service::filesystem::filesystem::FileSystemController>,
            >,
        >,
    ) -> Self {
        Self {
            file,
            packed_update_raw: Mutex::new(None),
            updatable,
            content_provider,
            filesystem_controller,
        }
    }

    pub fn set_packed_update(&self, update_raw_file: VirtualFile) {
        *self.packed_update_raw.lock().unwrap() = Some(update_raw_file);
    }

    /// Open the RomFS for the current process.
    /// Corresponds to upstream `RomFSFactory::OpenCurrentProcess`.
    pub fn open_current_process(&self, current_process_title_id: u64) -> Option<VirtualFile> {
        if !self.updatable {
            return self.file.clone();
        }

        // Upstream:
        //   const auto nca = content_provider.GetEntry(current_process_title_id, type);
        //   const PatchManager patch_manager{...};
        //   return patch_manager.PatchRomFS(nca.get(), file, Program, packed_update_raw);
        let base = match &self.file {
            Some(f) => f.clone(),
            None => return None,
        };
        let fs_ctrl = self.filesystem_controller.as_ref()?;
        let fs_ctrl_guard = fs_ctrl.lock().unwrap();
        let content_prov = self.content_provider.as_ref()?.lock().unwrap();
        let nca = content_prov.get_entry(current_process_title_id, ContentRecordType::Program);
        let patch_manager =
            PatchManager::new(current_process_title_id, &fs_ctrl_guard, &*content_prov);
        Some(patch_manager.patch_romfs(
            nca.as_ref(),
            base,
            ContentRecordType::Program,
            self.packed_update_raw.lock().unwrap().clone(),
            true,
        ))
    }

    /// Open a patched RomFS for a given title.
    /// Corresponds to upstream `RomFSFactory::OpenPatchedRomFS`.
    pub fn open_patched_romfs(
        &self,
        title_id: u64,
        type_: ContentRecordType,
    ) -> Option<VirtualFile> {
        let provider = self.content_provider.as_ref()?.lock().unwrap();
        let nca = provider.get_entry(title_id, type_)?;
        let romfs = nca.get_romfs()?;

        let fs_ctrl = self.filesystem_controller.as_ref()?;
        let fs_ctrl_guard = fs_ctrl.lock().unwrap();
        let patch_manager = PatchManager::new(title_id, &fs_ctrl_guard, &*provider);
        Some(patch_manager.patch_romfs(Some(&nca), romfs, type_, None, true))
    }

    /// Open a patched RomFS with program index.
    ///
    /// Uses `GetBaseTitleIDWithProgramIndex` to compute the resolved title ID,
    /// then delegates to `open_patched_romfs`.
    ///
    /// Corresponds to upstream `RomFSFactory::OpenPatchedRomFSWithProgramIndex`.
    pub fn open_patched_romfs_with_program_index(
        &self,
        title_id: u64,
        program_index: u8,
        type_: ContentRecordType,
    ) -> Option<VirtualFile> {
        let res_title_id = get_base_title_id_with_program_index(title_id, program_index as u64);
        self.open_patched_romfs(res_title_id, type_)
    }

    /// Internal helper: get the NCA entry for a given title/storage/type.
    /// Corresponds to upstream `RomFSFactory::GetEntry`.
    ///
    /// Upstream dispatches based on `StorageId` to the process content
    /// provider or `FileSystemController` caches.
    pub fn get_entry(
        &self,
        title_id: u64,
        storage: StorageId,
        type_: ContentRecordType,
    ) -> Option<NCA> {
        match storage {
            StorageId::None => {
                // Only this branch uses the union upstream. Holding its lock
                // for NAND/SDMC would invert the controller -> provider order.
                let provider = self.content_provider.as_ref()?.lock().unwrap();
                provider.get_entry(title_id, type_)
            }
            StorageId::NandSystem => {
                // Upstream: filesystem_controller.GetSystemNANDContents()->GetEntry(...)
                if let Some(ref fsc) = self.filesystem_controller {
                    let fsc = fsc.lock().unwrap();
                    fsc.get_system_nand_contents()?.get_entry(title_id, type_)
                } else {
                    None
                }
            }
            StorageId::NandUser => {
                // Upstream: filesystem_controller.GetUserNANDContents()->GetEntry(...)
                if let Some(ref fsc) = self.filesystem_controller {
                    let fsc = fsc.lock().unwrap();
                    fsc.get_user_nand_contents()?.get_entry(title_id, type_)
                } else {
                    None
                }
            }
            StorageId::SdCard => {
                // Upstream: filesystem_controller.GetSDMCContents()->GetEntry(...)
                if let Some(ref fsc) = self.filesystem_controller {
                    let fsc = fsc.lock().unwrap();
                    fsc.get_sdmc_contents()?.get_entry(title_id, type_)
                } else {
                    None
                }
            }
            StorageId::Host | StorageId::GameCard => {
                log::warn!(
                    "RomFSFactory::get_entry: unimplemented storage_id={:02X}",
                    storage as u8
                );
                None
            }
        }
    }

    /// Open a RomFS by title ID, storage, and type.
    ///
    /// Retrieves the NCA entry from the appropriate storage and returns
    /// its RomFS.
    ///
    /// Corresponds to upstream `RomFSFactory::Open`.
    pub fn open(
        &self,
        title_id: u64,
        storage: StorageId,
        type_: ContentRecordType,
    ) -> Option<VirtualFile> {
        let nca = self.get_entry(title_id, storage, type_)?;
        nca.get_romfs()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_sys::vfs::vfs_vector::VectorVfsFile;
    use std::sync::Arc;

    fn make_test_file() -> VirtualFile {
        Arc::new(VectorVfsFile::new(
            vec![0u8; 100],
            "test.romfs".to_string(),
            None,
        ))
    }

    #[test]
    fn test_storage_id_values() {
        assert_eq!(StorageId::None as u8, 0);
        assert_eq!(StorageId::Host as u8, 1);
        assert_eq!(StorageId::GameCard as u8, 2);
        assert_eq!(StorageId::NandSystem as u8, 3);
        assert_eq!(StorageId::NandUser as u8, 4);
        assert_eq!(StorageId::SdCard as u8, 5);
    }

    #[test]
    fn explicit_storage_lookup_does_not_lock_the_content_union() {
        for storage in [StorageId::NandSystem, StorageId::NandUser, StorageId::SdCard,
            StorageId::Host, StorageId::GameCard] {
            let provider = Arc::new(Mutex::new(ContentProviderUnion::new()));
            let guard = provider.lock().unwrap();
            let worker_provider = provider.clone();
            let (sender, receiver) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                let factory = RomFSFactory::new_with_file(None, false, Some(worker_provider), None);
                let missing = factory.get_entry(42, storage, ContentRecordType::Data).is_none();
                sender.send(missing).unwrap();
            });
            let result = receiver.recv_timeout(std::time::Duration::from_secs(2));
            // Release before joining/asserting so the regression also terminates
            // on the old implementation instead of hanging the test suite.
            drop(guard);
            worker.join().unwrap();
            assert_eq!(result, Ok(true), "storage {storage:?} must not wait on the union");
        }
    }

    #[test]
    fn test_new() {
        let factory = RomFSFactory::new_with_file(Some(make_test_file()), true, None, None);
        assert!(factory.file.is_some());
        assert!(factory.updatable);
    }

    #[test]
    fn test_open_current_process_not_updatable() {
        let file = make_test_file();
        let factory = RomFSFactory::new_with_file(Some(file.clone()), false, None, None);
        // When not updatable, should return the file directly.
        let result = factory.open_current_process(0x0100000000001000);
        assert!(result.is_some());
    }

    #[test]
    fn test_set_packed_update() {
        let factory = RomFSFactory::new_with_file(None, false, None, None);
        assert!(factory.packed_update_raw.lock().unwrap().is_none());
        factory.set_packed_update(make_test_file());
        assert!(factory.packed_update_raw.lock().unwrap().is_some());
    }

    #[test]
    fn test_open_patched_romfs_no_provider() {
        let factory = RomFSFactory::new_with_file(None, false, None, None);
        // No content provider, should return None.
        assert!(factory
            .open_patched_romfs(0x0100000000001000, ContentRecordType::Program)
            .is_none());
    }
}
