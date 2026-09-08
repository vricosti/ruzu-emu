//! Port of zuyu/src/core/hle/service/filesystem/save_data_controller.h and .cpp
//!
//! SaveDataController - manages save data creation and access.

use std::sync::{Arc, Mutex};

use crate::file_sys::fs_save_data_types::{
    SaveDataAttribute, SaveDataSize, SaveDataSpaceId, SaveDataType, UserId,
};
use crate::file_sys::savedata_factory::SaveDataFactory;
use crate::file_sys::vfs::vfs_types::VirtualDir;

/// Default size for normal/journal save data if application control metadata cannot be found.
/// ~4.2GB, matching upstream SufficientSaveDataSize.
const SUFFICIENT_SAVE_DATA_SIZE: u64 = 0xF000_0000;

/// Port of the upstream file-local GetDefaultSaveDataSize helper.
fn get_default_save_data_size(system: &crate::core::System, program_id: u64) -> SaveDataSize {
    if let Some(provider) = system.get_content_provider() {
        let filesystem = system.get_filesystem_controller();
        let filesystem = filesystem.lock().unwrap();
        let provider = provider.lock().unwrap();
        let pm =
            crate::file_sys::patch_manager::PatchManager::new(program_id, &filesystem, &*provider);
        if let Some(nacp) = pm.get_control_metadata().0 {
            return SaveDataSize {
                normal: nacp.get_default_normal_save_size(),
                journal: nacp.get_default_journal_save_size(),
            };
        }
    }
    SaveDataSize {
        normal: SUFFICIENT_SAVE_DATA_SIZE,
        journal: SUFFICIENT_SAVE_DATA_SIZE,
    }
}

/// Port of Service::FileSystem::SaveDataController
#[derive(Clone)]
pub struct SaveDataController {
    factory: Option<Arc<Mutex<SaveDataFactory>>>,
}

impl SaveDataController {
    pub fn new() -> Self {
        Self { factory: None }
    }

    pub fn with_factory(factory: Arc<Mutex<SaveDataFactory>>) -> Self {
        Self {
            factory: Some(factory),
        }
    }

    pub fn set_factory(&mut self, factory: Arc<Mutex<SaveDataFactory>>) {
        self.factory = Some(factory);
    }

    pub fn create_save_data(
        &self,
        space: SaveDataSpaceId,
        attribute: &SaveDataAttribute,
    ) -> Option<VirtualDir> {
        let factory = self.factory.as_ref()?;
        factory.lock().unwrap().create(space, attribute)
    }

    pub fn open_save_data(
        &self,
        space: SaveDataSpaceId,
        attribute: &SaveDataAttribute,
    ) -> Option<VirtualDir> {
        let factory = self.factory.as_ref()?;
        factory.lock().unwrap().open(space, attribute)
    }

    pub fn open_save_data_space(&self, space: SaveDataSpaceId) -> Option<VirtualDir> {
        let factory = self.factory.as_ref()?;
        factory.lock().unwrap().get_save_data_space_directory(space)
    }

    pub fn set_auto_create(&mut self, state: bool) {
        if let Some(ref factory) = self.factory {
            factory.lock().unwrap().set_auto_create(state);
        }
    }

    /// System is borrowed at the call site rather than stored as a raw pointer.
    /// Callers must release the filesystem-controller lock before this call:
    /// metadata lookup reacquires it, unlike upstream's non-mutex reference.
    pub fn read_save_data_size(
        &self,
        system: &crate::core::System,
        save_type: SaveDataType,
        title_id: u64,
        user_id: UserId,
    ) -> SaveDataSize {
        let factory = self
            .factory
            .as_ref()
            .expect("save data factory is not initialized");
        let value = factory
            .lock()
            .unwrap()
            .read_save_data_size(save_type, title_id, user_id);
        if value.normal == 0 && value.journal == 0 {
            let size = get_default_save_data_size(system, title_id);
            factory
                .lock()
                .unwrap()
                .write_save_data_size(save_type, title_id, user_id, size);
            return size;
        }
        value
    }

    pub fn write_save_data_size(
        &self,
        save_type: SaveDataType,
        title_id: u64,
        user_id: UserId,
        new_value: SaveDataSize,
    ) {
        self.factory
            .as_ref()
            .expect("save data factory is not initialized")
            .lock()
            .unwrap()
            .write_save_data_size(save_type, title_id, user_id, new_value);
    }
}

impl Default for SaveDataController {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_sizes_initialize_persist_and_preserve_partial_zero_pairs() {
        use crate::file_sys::fs_filesystem::OpenMode;
        use crate::file_sys::vfs::vfs_real::RealVfsFilesystem;
        let path = std::env::temp_dir().join(format!(
            "ruzu-save-size-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        let vfs = RealVfsFilesystem::new();
        let root = vfs
            .arc_open_directory(&path.to_string_lossy(), OpenMode::READ_WRITE)
            .unwrap();
        let factory = Arc::new(Mutex::new(SaveDataFactory::new(42, root)));
        let controller = SaveDataController::with_factory(factory.clone());
        let system = crate::core::System::new();
        let user = [123, 456];
        let initial = controller.read_save_data_size(&system, SaveDataType::Account, 42, user);
        assert_eq!(
            initial,
            SaveDataSize {
                normal: SUFFICIENT_SAVE_DATA_SIZE,
                journal: SUFFICIENT_SAVE_DATA_SIZE
            }
        );
        assert_eq!(
            factory
                .lock()
                .unwrap()
                .read_save_data_size(SaveDataType::Account, 42, user),
            initial
        );
        for size in [
            SaveDataSize {
                normal: 0,
                journal: 8192,
            },
            SaveDataSize {
                normal: 4096,
                journal: 0,
            },
            SaveDataSize {
                normal: 1 << 40,
                journal: 1 << 35,
            },
        ] {
            controller.write_save_data_size(SaveDataType::Account, 42, user, size);
            let reopened =
                SaveDataController::with_factory(Arc::new(Mutex::new(SaveDataFactory::new(
                    42,
                    vfs.arc_open_directory(&path.to_string_lossy(), OpenMode::READ_WRITE)
                        .unwrap(),
                ))));
            assert_eq!(
                reopened.read_save_data_size(&system, SaveDataType::Account, 42, user),
                size
            );
        }
        assert_eq!(
            factory
                .lock()
                .unwrap()
                .read_save_data_size(SaveDataType::Account, 43, user),
            SaveDataSize::default()
        );
        assert_eq!(
            factory
                .lock()
                .unwrap()
                .read_save_data_size(SaveDataType::Account, 42, [456, 123]),
            SaveDataSize::default()
        );
        std::fs::remove_dir_all(path).unwrap();
    }
}
