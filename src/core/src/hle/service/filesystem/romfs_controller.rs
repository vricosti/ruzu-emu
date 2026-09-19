//! Port of zuyu/src/core/hle/service/filesystem/romfs_controller.h and .cpp
//!
//! RomFsController - manages RomFS access for a process.

use std::sync::Arc;

use crate::file_sys::content_archive::NCA;
use crate::file_sys::nca_metadata::ContentRecordType;
use crate::file_sys::romfs_factory::RomFSFactory;
use crate::file_sys::romfs_factory::StorageId;
use crate::file_sys::vfs::vfs_types::VirtualFile;

/// Port of Service::FileSystem::RomFsController
pub struct RomFsController {
    program_id: u64,
    factory: Option<Arc<RomFSFactory>>,
}

impl RomFsController {
    pub fn new(program_id: u64) -> Self {
        Self {
            program_id,
            factory: None,
        }
    }

    pub fn with_factory(program_id: u64, factory: Arc<RomFSFactory>) -> Self {
        Self {
            program_id,
            factory: Some(factory),
        }
    }

    pub fn set_factory(&mut self, factory: Arc<RomFSFactory>) {
        self.factory = Some(factory);
    }

    pub fn open_romfs_current_process(&self) -> Option<VirtualFile> {
        let factory = self.factory.as_ref()?;
        factory.open_current_process(self.program_id)
    }

    pub fn open_patched_romfs(&self, title_id: u64, type_: ContentRecordType) -> Option<VirtualFile> {
        let factory = self.factory.as_ref()?;
        factory.open_patched_romfs(title_id, type_)
    }

    pub fn open_patched_romfs_with_program_index(
        &self,
        title_id: u64,
        program_index: u8,
        type_: ContentRecordType,
    ) -> Option<VirtualFile> {
        let factory = self.factory.as_ref()?;
        factory.open_patched_romfs_with_program_index(
            title_id,
            program_index,
            type_,
        )
    }

    pub fn open_romfs(
        &self,
        title_id: u64,
        storage_id: StorageId,
        type_: ContentRecordType,
    ) -> Option<VirtualFile> {
        let factory = self.factory.as_ref()?;
        factory.open(title_id, storage_id, type_)
    }

    pub fn open_base_nca(
        &self,
        title_id: u64,
        storage_id: StorageId,
        type_: ContentRecordType,
    ) -> Option<NCA> {
        let factory = self.factory.as_ref()?;
        factory.get_entry(title_id, storage_id, type_)
    }
}
