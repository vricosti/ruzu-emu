//! Port of zuyu/src/core/hle/service/filesystem/filesystem.h and filesystem.cpp
//!
//! FileSystemController and VfsDirectoryServiceWrapper.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use common::fs::path_util::{self, DirectorySeparator};
use common::ResultCode;

use crate::file_sys::bis_factory::BisPartitionId;
use crate::file_sys::card_image::XCI;
use crate::file_sys::errors;
use crate::file_sys::fs_filesystem::{DirectoryEntryType, OpenMode};
use crate::file_sys::registered_cache::{
    ContentProvider, ContentProviderUnion, ContentProviderUnionSlot, ExternalContentProvider,
    PlaceholderCache, RegisteredCache,
};
use crate::file_sys::romfs_factory::{RomFSFactory, StorageId};
use crate::file_sys::savedata_factory::SaveDataFactory;
use crate::file_sys::sdmc_factory::SdmcFactory;
use crate::file_sys::vfs::vfs_offset::OffsetVfsFile;
use crate::file_sys::vfs::vfs_types::{FileTimeStampRaw, VirtualDir, VirtualFile};
use crate::hle::result::RESULT_SUCCESS;
use crate::hle::service::hle_ipc::SessionRequestHandlerPtr;

/// Port of upstream `ResultUnknown` (result.h:250) — `common::ResultCode(UINT32_MAX)`.
/// Used as a fallback error code in VfsDirectoryServiceWrapper methods.
const RESULT_UNKNOWN: ResultCode = ResultCode(u32::MAX);

/// Port of Service::FileSystem::ContentStorageId
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ContentStorageId {
    System = 0,
    User = 1,
    SdCard = 2,
}

/// Port of Service::FileSystem::ImageDirectoryId
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ImageDirectoryId {
    Nand = 0,
    SdCard = 1,
}

/// Port of Service::FileSystem::ProcessId
pub type ProcessId = u64;

/// Port of Service::FileSystem::ProgramId
pub type ProgramId = u64;

/// Port of upstream `FileSystemController::Registration` (filesystem.h:129-133).
struct Registration {
    program_id: ProgramId,
    romfs_factory: Option<Arc<RomFSFactory>>,
    save_data_factory: Option<Arc<Mutex<SaveDataFactory>>>,
}

/// Port of Service::FileSystem::FileSystemController
///
/// Manages filesystem factories and process registrations.
/// Upstream: filesystem.h:64-146, filesystem.cpp.
pub struct FileSystemController {
    registrations: Mutex<BTreeMap<ProcessId, Registration>>,
    /// BIS factory for NAND system/user content.
    /// Upstream: `std::unique_ptr<FileSys::BISFactory> bis_factory`.
    bis_factory: Option<Box<crate::file_sys::bis_factory::BisFactory>>,
    /// Virtual filesystem reference for creating factories.
    /// Upstream: `Core::System::GetFilesystem()`.
    vfs: Option<Arc<crate::file_sys::vfs::vfs_real::RealVfsFilesystem>>,
    /// SDMC factory for SD card content and mod roots.
    /// Upstream: `std::unique_ptr<FileSys::SDMCFactory> sdmc_factory`.
    sdmc_factory: Option<Box<SdmcFactory>>,
    /// Content discovered in the user-configured external update/DLC roots.
    /// Upstream: `std::unique_ptr<FileSys::ExternalContentProvider> external_provider`.
    external_provider: Option<Box<ExternalContentProvider>>,
    /// Frontend-owned per-launch view returned by `OpenSDMC`.
    ///
    /// Eden always returns `sdmc_factory->Open()`. Ruzu uses this optional
    /// override only while booting a standalone homebrew NRO so its sibling
    /// assets can be exposed without host symbolic links or junction points.
    /// Content caches and SDMC metadata continue to use `sdmc_factory`.
    sdmc_open_override: Option<VirtualDir>,
    /// Shared content provider union owned by System.
    content_provider: Option<Arc<Mutex<ContentProviderUnion>>>,
    /// Parsed game-card image and the two cache views built from its pseudo-directory.
    /// Upstream: `gamecard`, `gamecard_registered`, `gamecard_placeholder`.
    gamecard: Option<Box<XCI>>,
    gamecard_registered: Option<Box<RegisteredCache>>,
    gamecard_placeholder: Option<Box<PlaceholderCache>>,
}

impl FileSystemController {
    pub fn new() -> Self {
        Self {
            registrations: Mutex::new(BTreeMap::new()),
            bis_factory: None,
            vfs: None,
            sdmc_factory: None,
            external_provider: None,
            sdmc_open_override: None,
            content_provider: None,
            gamecard: None,
            gamecard_registered: None,
            gamecard_placeholder: None,
        }
    }

    /// Set the virtual filesystem reference.
    pub fn set_filesystem(&mut self, vfs: Arc<crate::file_sys::vfs::vfs_real::RealVfsFilesystem>) {
        self.vfs = Some(vfs);
    }

    pub fn set_content_provider(&mut self, provider: Arc<Mutex<ContentProviderUnion>>) {
        self.content_provider = Some(provider);
    }

    /// Set the BIS factory (called during system initialization).
    pub fn set_bis_factory(&mut self, factory: crate::file_sys::bis_factory::BisFactory) {
        self.bis_factory = Some(Box::new(factory));
    }

    /// Port of upstream `FileSystemController::SetGameCard`.
    pub fn set_game_card(&mut self, file: VirtualFile) {
        let mut gamecard = XCI::new(file, 0, 0);
        let directory = gamecard.concatenated_pseudo_directory();
        self.gamecard_registered = Some(Box::new(RegisteredCache::new(Arc::clone(&directory))));
        self.gamecard_placeholder = Some(Box::new(PlaceholderCache::new(directory)));
        self.gamecard = Some(Box::new(gamecard));
    }

    /// Port of upstream `FileSystemController::GetGameCard`.
    pub fn get_game_card(&self) -> Option<&XCI> {
        self.gamecard.as_deref()
    }

    /// Port of upstream `FileSystemController::GetGameCardContents`.
    pub fn get_game_card_contents(&self) -> Option<&RegisteredCache> {
        self.gamecard_registered.as_deref()
    }

    /// Mutable Rust counterpart used where upstream returns a mutable pointer.
    pub fn get_game_card_contents_mut(&mut self) -> Option<&mut RegisteredCache> {
        self.gamecard_registered.as_deref_mut()
    }

    /// Port of upstream `FileSystemController::GetGameCardPlaceholder`.
    pub fn get_game_card_placeholder(&self) -> Option<&PlaceholderCache> {
        self.gamecard_placeholder.as_deref()
    }

    /// Get the System NAND RegisteredCache.
    /// Upstream: `FileSystemController::GetSystemNANDContents()`.
    pub fn get_system_nand_contents(
        &self,
    ) -> Option<&crate::file_sys::registered_cache::RegisteredCache> {
        self.bis_factory.as_ref()?.get_system_nand_contents()
    }

    /// Get the User NAND RegisteredCache.
    /// Upstream: `FileSystemController::GetUserNANDContents()`.
    pub fn get_user_nand_contents(
        &self,
    ) -> Option<&crate::file_sys::registered_cache::RegisteredCache> {
        self.bis_factory.as_ref()?.get_user_nand_contents()
    }

    /// Port of upstream `FileSystemController::GetExternalContentProvider`.
    pub fn get_external_content_provider(&self) -> Option<&ExternalContentProvider> {
        self.external_provider.as_deref()
    }

    /// Get the System NAND content directory.
    /// Upstream: `FileSystemController::GetSystemNANDContentDirectory()`.
    pub fn get_system_nand_content_directory(
        &self,
    ) -> Option<crate::file_sys::vfs::vfs_types::VirtualDir> {
        self.bis_factory
            .as_ref()?
            .get_system_nand_content_directory()
    }

    /// Get the User NAND content directory.
    /// Upstream: `FileSystemController::GetUserNANDContentDirectory()`.
    pub fn get_user_nand_content_directory(
        &self,
    ) -> Option<crate::file_sys::vfs::vfs_types::VirtualDir> {
        self.bis_factory.as_ref()?.get_user_nand_content_directory()
    }

    /// Get the SDMC RegisteredCache.
    /// Upstream: `FileSystemController::GetSDMCContents()`.
    pub fn get_sdmc_contents(&self) -> Option<&crate::file_sys::registered_cache::RegisteredCache> {
        self.sdmc_factory.as_ref()?.get_sdmc_contents()
    }

    /// Port of upstream `FileSystemController::GetSystemNANDPlaceholder`.
    pub fn get_system_nand_placeholder(&self) -> Option<&PlaceholderCache> {
        log::trace!("Opening System NAND Placeholder");
        self.bis_factory.as_ref()?.get_system_nand_placeholder()
    }

    /// Port of upstream `FileSystemController::GetUserNANDPlaceholder`.
    pub fn get_user_nand_placeholder(&self) -> Option<&PlaceholderCache> {
        log::trace!("Opening User NAND Placeholder");
        self.bis_factory.as_ref()?.get_user_nand_placeholder()
    }

    /// Port of upstream `FileSystemController::GetSDMCPlaceholder`.
    pub fn get_sdmc_placeholder(&self) -> Option<&PlaceholderCache> {
        log::trace!("Opening SDMC Placeholder");
        self.sdmc_factory.as_ref()?.get_sdmc_placeholder()
    }

    /// Port of upstream `FileSystemController::GetRegisteredCacheForStorage`.
    pub fn get_registered_cache_for_storage(
        &mut self,
        id: StorageId,
    ) -> Option<&mut RegisteredCache> {
        match id {
            StorageId::None | StorageId::Host => {
                log::error!(
                    "FileSystemController::GetRegisteredCacheForStorage unimplemented for {:?}",
                    id
                );
                None
            }
            StorageId::GameCard => self.get_game_card_contents_mut(),
            StorageId::NandSystem => self.bis_factory.as_mut()?.get_system_nand_contents_mut(),
            StorageId::NandUser => self.bis_factory.as_mut()?.get_user_nand_contents_mut(),
            StorageId::SdCard => self.sdmc_factory.as_mut()?.get_sdmc_contents_mut(),
        }
    }

    /// Port of upstream `FileSystemController::GetPlaceholderCacheForStorage`.
    pub fn get_placeholder_cache_for_storage(&self, id: StorageId) -> Option<&PlaceholderCache> {
        match id {
            StorageId::None | StorageId::Host => {
                log::error!(
                    "FileSystemController::GetPlaceholderCacheForStorage unimplemented for {:?}",
                    id
                );
                None
            }
            StorageId::GameCard => self.get_game_card_placeholder(),
            StorageId::NandSystem => self.get_system_nand_placeholder(),
            StorageId::NandUser => self.get_user_nand_placeholder(),
            StorageId::SdCard => self.get_sdmc_placeholder(),
        }
    }

    /// Get the SDMC content directory.
    /// Upstream: `FileSystemController::GetSDMCContentDirectory()`.
    pub fn get_sdmc_content_directory(
        &self,
    ) -> Option<crate::file_sys::vfs::vfs_types::VirtualDir> {
        self.sdmc_factory.as_ref()?.get_sdmc_content_directory()
    }

    /// Port of upstream `FileSystemController::GetNANDImageDirectory`.
    pub fn get_nand_image_directory(&self) -> Option<VirtualDir> {
        log::trace!("Opening NAND image directory");
        self.bis_factory.as_ref()?.get_image_directory()
    }

    /// Port of upstream `FileSystemController::GetSDMCImageDirectory`.
    pub fn get_sdmc_image_directory(&self) -> Option<VirtualDir> {
        log::trace!("Opening SDMC image directory");
        self.sdmc_factory.as_ref()?.get_image_directory()
    }

    /// Port of upstream `FileSystemController::GetContentDirectory`.
    pub fn get_content_directory(&self, id: ContentStorageId) -> Option<VirtualDir> {
        match id {
            ContentStorageId::System => self.get_system_nand_content_directory(),
            ContentStorageId::User => self.get_user_nand_content_directory(),
            ContentStorageId::SdCard => self.get_sdmc_content_directory(),
        }
    }

    /// Port of upstream `FileSystemController::GetImageDirectory`.
    pub fn get_image_directory(&self, id: ImageDirectoryId) -> Option<VirtualDir> {
        match id {
            ImageDirectoryId::Nand => self.get_nand_image_directory(),
            ImageDirectoryId::SdCard => self.get_sdmc_image_directory(),
        }
    }

    /// Port of upstream `FileSystemController::OpenSDMC` (filesystem.cpp:359-372).
    pub fn open_sdmc(&self) -> Result<VirtualDir, ResultCode> {
        log::trace!("FileSystemController::OpenSDMC called");

        if let Some(sdmc) = self.sdmc_open_override.as_ref() {
            log::debug!(
                "FileSystemController::OpenSDMC returning per-launch root={}",
                sdmc.get_full_path()
            );
            return Ok(Arc::clone(sdmc));
        }

        let Some(sdmc_factory) = self.sdmc_factory.as_ref() else {
            return Err(errors::RESULT_PORT_SD_CARD_NO_DEVICE);
        };

        let sdmc = sdmc_factory.open();
        log::debug!(
            "FileSystemController::OpenSDMC returning root={}",
            sdmc.get_full_path()
        );
        Ok(sdmc)
    }

    /// Port of upstream `FileSystemController::OpenSaveDataController`.
    pub fn open_save_data_controller(&self) -> super::save_data_controller::SaveDataController {
        match self.create_save_data_factory(0) {
            Some(factory) => super::save_data_controller::SaveDataController::with_factory(factory),
            None => super::save_data_controller::SaveDataController::new(),
        }
    }

    /// Port of upstream `FileSystemController::OpenBISPartition`.
    pub fn open_bis_partition(&self, id: BisPartitionId) -> Result<VirtualDir, ResultCode> {
        log::trace!("Opening BIS Partition with id={:08X}", id as u32);
        let factory = self
            .bis_factory
            .as_ref()
            .ok_or(errors::RESULT_TARGET_NOT_FOUND)?;
        factory
            .open_partition(id)
            .ok_or(errors::RESULT_INVALID_ARGUMENT)
    }

    /// Port of upstream `FileSystemController::OpenBISPartitionStorage`.
    pub fn open_bis_partition_storage(
        &self,
        id: BisPartitionId,
    ) -> Result<VirtualFile, ResultCode> {
        log::trace!("Opening BIS Partition Storage with id={:08X}", id as u32);
        let factory = self
            .bis_factory
            .as_ref()
            .ok_or(errors::RESULT_TARGET_NOT_FOUND)?;
        let filesystem = self
            .vfs
            .as_ref()
            .cloned()
            .ok_or(errors::RESULT_TARGET_NOT_FOUND)?;
        factory
            .open_partition_storage(id, filesystem)
            .ok_or(errors::RESULT_INVALID_ARGUMENT)
    }

    /// Install a frontend-owned SDMC view for the current launch.
    ///
    /// This is a Ruzu frontend extension; passing `None` restores Eden's
    /// factory-backed `OpenSDMC` behavior.
    pub fn set_sdmc_open_override(&mut self, sdmc: Option<VirtualDir>) {
        self.sdmc_open_override = sdmc;
    }

    /// Port of upstream `FileSystemController::GetFreeSpaceSize` (filesystem.cpp:402-424).
    pub fn get_free_space_size(&self, id: StorageId) -> u64 {
        match id {
            StorageId::None | StorageId::GameCard => 0,
            StorageId::SdCard => self
                .sdmc_factory
                .as_ref()
                .map(|factory| factory.get_sdmc_free_space())
                .unwrap_or(0),
            StorageId::Host => self
                .bis_factory
                .as_ref()
                .map(|factory| {
                    factory.get_system_nand_free_space() + factory.get_user_nand_free_space()
                })
                .unwrap_or(0),
            StorageId::NandSystem => self
                .bis_factory
                .as_ref()
                .map(|factory| factory.get_system_nand_free_space())
                .unwrap_or(0),
            StorageId::NandUser => self
                .bis_factory
                .as_ref()
                .map(|factory| factory.get_user_nand_free_space())
                .unwrap_or(0),
        }
    }

    /// Port of upstream `FileSystemController::GetTotalSpaceSize` (filesystem.cpp:426-448).
    pub fn get_total_space_size(&self, id: StorageId) -> u64 {
        match id {
            StorageId::None | StorageId::GameCard => 0,
            StorageId::SdCard => self
                .sdmc_factory
                .as_ref()
                .map(|factory| factory.get_sdmc_total_space())
                .unwrap_or(0),
            StorageId::Host => self
                .bis_factory
                .as_ref()
                .map(|factory| factory.get_full_nand_total_space())
                .unwrap_or(0),
            StorageId::NandSystem => self
                .bis_factory
                .as_ref()
                .map(|factory| factory.get_system_nand_total_space())
                .unwrap_or(0),
            StorageId::NandUser => self
                .bis_factory
                .as_ref()
                .map(|factory| factory.get_user_nand_total_space())
                .unwrap_or(0),
        }
    }

    /// Port of upstream `FileSystemController::RegisterProcess` (filesystem.cpp:298-311).
    ///
    /// Called by the NCA/NRO loader after loading a process. Stores the
    /// process_id → program_id mapping and associated RomFS/SaveData factories
    /// so that FSP_SRV::SetCurrentProcess can resolve controllers.
    pub fn register_process(
        &self,
        process_id: ProcessId,
        program_id: ProgramId,
        romfs_factory: Option<Arc<RomFSFactory>>,
    ) -> u32 {
        let save_data_factory = self.create_save_data_factory(program_id);
        let mut registrations = self.registrations.lock().unwrap();
        registrations.insert(
            process_id,
            Registration {
                program_id,
                romfs_factory,
                save_data_factory,
            },
        );
        log::debug!(
            "FileSystemController::RegisterProcess: process_id={:#x}, program_id={:#018x}",
            process_id,
            program_id,
        );
        RESULT_SUCCESS.get_inner_value()
    }

    /// Port of upstream `FileSystemController::SetPackedUpdate` (filesystem.cpp:331-341).
    pub fn set_packed_update(&self, process_id: ProcessId, update_raw: VirtualFile) {
        log::trace!(
            "FileSystemController::SetPackedUpdate: process_id={:#x}",
            process_id
        );
        let registrations = self.registrations.lock().unwrap();
        let Some(registration) = registrations.get(&process_id) else {
            return;
        };
        let Some(romfs_factory) = registration.romfs_factory.as_ref() else {
            return;
        };
        romfs_factory.set_packed_update(update_raw);
    }

    /// Port of upstream `FileSystemController::OpenProcess` (filesystem.cpp:313-328).
    ///
    /// Looks up the registration for `process_id` and returns the associated
    /// program_id, SaveDataController, and RomFsController.
    pub fn open_process(
        &self,
        process_id: ProcessId,
    ) -> Option<(
        ProgramId,
        super::save_data_controller::SaveDataController,
        super::romfs_controller::RomFsController,
    )> {
        let registrations = self.registrations.lock().unwrap();
        let reg = registrations.get(&process_id)?;
        let save_data_controller = match &reg.save_data_factory {
            Some(factory) => {
                super::save_data_controller::SaveDataController::with_factory(factory.clone())
            }
            None => super::save_data_controller::SaveDataController::new(),
        };
        let romfs_controller = match &reg.romfs_factory {
            Some(factory) => super::romfs_controller::RomFsController::with_factory(
                reg.program_id,
                factory.clone(),
            ),
            None => super::romfs_controller::RomFsController::new(reg.program_id),
        };
        Some((reg.program_id, save_data_controller, romfs_controller))
    }

    /// Port of upstream `FileSystemController::CreateSaveDataFactory` (filesystem.cpp:347-357).
    ///
    /// Creates a SaveDataFactory for the given program_id using the NAND save directory.
    fn create_save_data_factory(
        &self,
        program_id: ProgramId,
    ) -> Option<Arc<Mutex<SaveDataFactory>>> {
        use crate::file_sys::fs_filesystem::OpenMode;
        use common::fs::path_util::{get_ruzu_path_string, RuzuPath};

        // Upstream: auto vfs = system.GetFilesystem();
        //           auto save_directory = vfs->OpenDirectory(SaveDir, ReadWrite);
        let vfs = self.vfs.as_ref()?;
        let save_path = get_ruzu_path_string(RuzuPath::SaveDir);
        let save_directory: VirtualDir =
            Arc::new(crate::file_sys::vfs::vfs_real::RealVfsDirectory::new(
                vfs.clone(),
                save_path,
                OpenMode::READ_WRITE,
            ));
        Some(Arc::new(Mutex::new(SaveDataFactory::new(
            program_id,
            save_directory,
        ))))
    }

    /// Get modification load root for a given title.
    /// Upstream: `FileSystemController::GetModificationLoadRoot`.
    pub fn get_modification_load_root(
        &self,
        title_id: u64,
    ) -> Option<crate::file_sys::vfs::vfs_types::VirtualDir> {
        log::trace!("Opening mod load root for tid={:016X}", title_id);
        self.bis_factory
            .as_ref()?
            .get_modification_load_root(title_id)
    }

    /// Get SDMC modification load root for a given title.
    /// Upstream: `FileSystemController::GetSDMCModificationLoadRoot`.
    pub fn get_sdmc_modification_load_root(
        &self,
        title_id: u64,
    ) -> Option<crate::file_sys::vfs::vfs_types::VirtualDir> {
        self.sdmc_factory
            .as_ref()?
            .get_sdmc_modification_load_root(title_id)
    }

    /// Get modification dump root for a given title.
    /// Upstream: `FileSystemController::GetModificationDumpRoot`.
    pub fn get_modification_dump_root(
        &self,
        title_id: u64,
    ) -> Option<crate::file_sys::vfs::vfs_types::VirtualDir> {
        log::trace!("Opening mod dump root for tid={:016X}", title_id);
        self.bis_factory
            .as_ref()?
            .get_modification_dump_root(title_id)
    }

    /// Get BCAT directory for a given title.
    /// Upstream: `FileSystemController::GetBCATDirectory`.
    pub fn get_bcat_directory(
        &self,
        title_id: u64,
    ) -> Option<crate::file_sys::vfs::vfs_types::VirtualDir> {
        log::trace!("Opening BCAT root for tid={:016X}", title_id);
        self.bis_factory.as_ref()?.get_bcat_directory(title_id)
    }

    /// Port of upstream `FileSystemController::CreateFactories`.
    pub fn create_factories(
        &mut self,
        vfs: Arc<crate::file_sys::vfs::vfs_real::RealVfsFilesystem>,
        overwrite: bool,
    ) {
        use common::fs::fs_util::path_to_utf8_string;
        use common::fs::path_util::{get_ruzu_path, get_ruzu_path_string, RuzuPath};

        self.vfs = Some(vfs.clone());

        if overwrite {
            self.bis_factory = None;
            self.sdmc_factory = None;
            self.external_provider = None;
            self.sdmc_open_override = None;
            if let Some(provider) = self.content_provider.as_ref() {
                let mut provider = provider.lock().unwrap();
                provider.clear_slot(ContentProviderUnionSlot::SysNAND);
                provider.clear_slot(ContentProviderUnionSlot::UserNAND);
                provider.clear_slot(ContentProviderUnionSlot::SDMC);
                provider.clear_slot(ContentProviderUnionSlot::External);
            }
        }

        if self.bis_factory.is_none() {
            let nand_directory: VirtualDir =
                Arc::new(crate::file_sys::vfs::vfs_real::RealVfsDirectory::new(
                    vfs.clone(),
                    get_ruzu_path_string(RuzuPath::NANDDir),
                    OpenMode::READ_WRITE,
                ));
            let load_directory: VirtualDir =
                Arc::new(crate::file_sys::vfs::vfs_real::RealVfsDirectory::new(
                    vfs.clone(),
                    get_ruzu_path_string(RuzuPath::LoadDir),
                    OpenMode::READ,
                ));
            let dump_directory: VirtualDir =
                Arc::new(crate::file_sys::vfs::vfs_real::RealVfsDirectory::new(
                    vfs.clone(),
                    get_ruzu_path_string(RuzuPath::DumpDir),
                    OpenMode::READ_WRITE,
                ));

            self.bis_factory = Some(Box::new(crate::file_sys::bis_factory::BisFactory::new(
                nand_directory,
                load_directory,
                dump_directory,
            )));
        }

        if self.sdmc_factory.is_none() {
            let sdmc_dir_path = get_ruzu_path(RuzuPath::SDMCDir);
            let sdmc_load_dir_path = sdmc_dir_path.join("atmosphere/contents");
            log::debug!(
                "FileSystemController::CreateFactories using sdmc_dir={} sdmc_load_dir={}",
                path_to_utf8_string(&sdmc_dir_path),
                path_to_utf8_string(&sdmc_load_dir_path),
            );
            let sd_directory: VirtualDir =
                Arc::new(crate::file_sys::vfs::vfs_real::RealVfsDirectory::new(
                    vfs.clone(),
                    path_to_utf8_string(&sdmc_dir_path),
                    OpenMode::READ_WRITE,
                ));
            let sd_load_directory: VirtualDir =
                Arc::new(crate::file_sys::vfs::vfs_real::RealVfsDirectory::new(
                    vfs.clone(),
                    path_to_utf8_string(&sdmc_load_dir_path),
                    OpenMode::READ,
                ));
            self.sdmc_factory = Some(Box::new(SdmcFactory::new(sd_directory, sd_load_directory)));
        }

        if self.external_provider.is_none() {
            let configured_dirs = common::settings::values().external_content_dirs.clone();
            log::debug!(
                "Initializing ExternalContentProvider with {} configured directories",
                configured_dirs.len()
            );
            let mut external_dirs = Vec::new();
            for dir_path in configured_dirs {
                if dir_path.is_empty() {
                    continue;
                }
                log::debug!("Attempting to open external content directory: {dir_path}");
                if !std::path::Path::new(&dir_path).is_dir() {
                    log::error!("Failed to open external content directory: {dir_path}");
                    continue;
                }
                if let Some(directory) = vfs.arc_open_directory(&dir_path, OpenMode::READ) {
                    external_dirs.push(directory);
                    log::debug!("Successfully opened external content directory: {dir_path}");
                } else {
                    log::error!("Failed to open external content directory: {dir_path}");
                }
            }
            log::debug!(
                "Creating ExternalContentProvider with {} opened directories",
                external_dirs.len()
            );
            self.external_provider = Some(Box::new(ExternalContentProvider::new(external_dirs)));
        }

        if let Some(provider) = self.content_provider.as_ref() {
            let mut provider = provider.lock().unwrap();
            if let Some(cache) = self
                .bis_factory
                .as_ref()
                .and_then(|factory| factory.get_system_nand_contents())
            {
                unsafe {
                    provider.set_slot(
                        ContentProviderUnionSlot::SysNAND,
                        (cache as *const dyn ContentProvider).cast_mut(),
                    );
                }
            }
            if let Some(cache) = self
                .bis_factory
                .as_ref()
                .and_then(|factory| factory.get_user_nand_contents())
            {
                unsafe {
                    provider.set_slot(
                        ContentProviderUnionSlot::UserNAND,
                        (cache as *const dyn ContentProvider).cast_mut(),
                    );
                }
            }
            if let Some(cache) = self
                .sdmc_factory
                .as_ref()
                .and_then(|factory| factory.get_sdmc_contents())
            {
                unsafe {
                    provider.set_slot(
                        ContentProviderUnionSlot::SDMC,
                        (cache as *const dyn ContentProvider).cast_mut(),
                    );
                }
            }
            if let Some(external_provider) = self.external_provider.as_mut() {
                unsafe {
                    provider.set_slot(
                        ContentProviderUnionSlot::External,
                        (external_provider.as_mut() as *mut ExternalContentProvider)
                            as *mut dyn ContentProvider,
                    );
                }
            }
        }
    }

    pub fn reset(&mut self) {
        self.registrations.lock().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{ContentStorageId, FileSystemController, ImageDirectoryId};
    use crate::file_sys::bis_factory::BisPartitionId;
    use crate::file_sys::registered_cache::{ContentProviderUnion, ContentProviderUnionSlot};
    use crate::file_sys::romfs_factory::{RomFSFactory, StorageId};
    use crate::file_sys::vfs::vfs_types::{VirtualDir, VirtualFile};
    use crate::file_sys::vfs::vfs_vector::{VectorVfsDirectory, VectorVfsFile};
    use common::fs::path_util::{get_ruzu_path, set_ruzu_path, RuzuPath};
    use std::fs;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn open_sdmc_uses_and_clears_the_frontend_override() {
        let mut controller = FileSystemController::new();
        let override_root: VirtualDir = Arc::new(VectorVfsDirectory::new(
            Vec::new(),
            Vec::new(),
            "homebrew-sdmc".to_string(),
            None,
        ));

        controller.set_sdmc_open_override(Some(Arc::clone(&override_root)));
        let opened = controller.open_sdmc().unwrap();
        assert!(Arc::ptr_eq(&opened, &override_root));

        controller.set_sdmc_open_override(None);
        assert!(matches!(
            controller.open_sdmc(),
            Err(result) if result == crate::file_sys::errors::RESULT_PORT_SD_CARD_NO_DEVICE
        ));
    }

    #[test]
    fn registered_nca_process_exposes_its_romfs_controller() {
        let controller = FileSystemController::new();
        let process_id = 0x42;
        let program_id = 0x0500_0000_0000_0001;
        let romfs: VirtualFile = Arc::new(VectorVfsFile::new(
            vec![0x5A; 0x40],
            "test-applet.romfs".to_string(),
            None,
        ));
        let factory = Arc::new(RomFSFactory::new_with_file(
            Some(Arc::clone(&romfs)),
            false,
            None,
            None,
        ));

        controller.register_process(process_id, program_id, Some(factory));

        let (registered_program_id, _, romfs_controller) =
            controller.open_process(process_id).unwrap();
        assert_eq!(registered_program_id, program_id);
        let opened = romfs_controller.open_romfs_current_process().unwrap();
        assert!(Arc::ptr_eq(&opened, &romfs));
    }

    #[test]
    fn create_factories_initializes_bis_factory_from_ruzu_paths() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("ruzu-fsc-bis-{unique}"));
        let nand = base.join("nand");
        let load = base.join("load");
        let dump = base.join("dump");
        let sdmc = base.join("sdmc");

        fs::create_dir_all(nand.join("system/Contents/registered")).unwrap();
        fs::create_dir_all(nand.join("system/Contents/placehld")).unwrap();
        fs::create_dir_all(nand.join("user/Contents/registered")).unwrap();
        fs::create_dir_all(nand.join("user/Contents/placehld")).unwrap();
        fs::create_dir_all(&load).unwrap();
        fs::create_dir_all(&dump).unwrap();
        fs::create_dir_all(sdmc.join("Nintendo/Contents/registered")).unwrap();
        fs::create_dir_all(sdmc.join("Nintendo/Contents/placehld")).unwrap();
        fs::create_dir_all(sdmc.join("atmosphere/contents")).unwrap();

        let old_nand = get_ruzu_path(RuzuPath::NANDDir);
        let old_load = get_ruzu_path(RuzuPath::LoadDir);
        let old_dump = get_ruzu_path(RuzuPath::DumpDir);
        let old_sdmc = get_ruzu_path(RuzuPath::SDMCDir);

        set_ruzu_path(RuzuPath::NANDDir, &nand);
        set_ruzu_path(RuzuPath::LoadDir, &load);
        set_ruzu_path(RuzuPath::DumpDir, &dump);
        set_ruzu_path(RuzuPath::SDMCDir, &sdmc);

        let mut controller = FileSystemController::new();
        let vfs = crate::file_sys::vfs::vfs_real::RealVfsFilesystem::new();
        let provider = Arc::new(Mutex::new(ContentProviderUnion::new()));
        controller.set_content_provider(provider.clone());
        controller.create_factories(vfs, false);

        let system_cache_address = controller.get_system_nand_contents().unwrap() as *const _;
        let user_cache_address = controller.get_user_nand_contents().unwrap() as *const _;
        let sdmc_cache_address = controller.get_sdmc_contents().unwrap() as *const _;
        let external_provider_address =
            controller.get_external_content_provider().unwrap() as *const _;

        // `CreateFactories` stores non-owning pointers to these providers in the
        // union. Eden's unique_ptr ownership keeps their addresses stable when
        // FileSystemController itself moves; the Rust ownership must do the same.
        let mut controller = Box::new(controller);
        assert_eq!(
            controller.get_system_nand_contents().unwrap() as *const _,
            system_cache_address
        );
        assert_eq!(
            controller.get_user_nand_contents().unwrap() as *const _,
            user_cache_address
        );
        assert_eq!(
            controller.get_sdmc_contents().unwrap() as *const _,
            sdmc_cache_address
        );
        assert_eq!(
            controller.get_external_content_provider().unwrap() as *const _,
            external_provider_address
        );

        assert!(controller.get_system_nand_contents().is_some());
        assert!(controller.get_user_nand_contents().is_some());
        assert!(controller.get_sdmc_contents().is_some());
        assert!(controller
            .get_placeholder_cache_for_storage(StorageId::NandSystem)
            .is_some());
        assert!(controller
            .get_placeholder_cache_for_storage(StorageId::NandUser)
            .is_some());
        assert!(controller
            .get_placeholder_cache_for_storage(StorageId::SdCard)
            .is_some());
        assert!(controller
            .get_registered_cache_for_storage(StorageId::NandSystem)
            .is_some());
        assert!(controller
            .get_registered_cache_for_storage(StorageId::NandUser)
            .is_some());
        assert!(controller
            .get_registered_cache_for_storage(StorageId::SdCard)
            .is_some());
        assert!(controller.get_system_nand_content_directory().is_some());
        assert!(controller.get_user_nand_content_directory().is_some());
        assert!(controller.get_sdmc_content_directory().is_some());
        assert!(controller.get_system_nand_placeholder().is_some());
        assert!(controller.get_user_nand_placeholder().is_some());
        assert!(controller.get_sdmc_placeholder().is_some());
        assert!(controller
            .get_content_directory(ContentStorageId::System)
            .is_some());
        assert!(controller
            .get_content_directory(ContentStorageId::User)
            .is_some());
        assert!(controller
            .get_content_directory(ContentStorageId::SdCard)
            .is_some());
        assert!(controller
            .get_image_directory(ImageDirectoryId::Nand)
            .is_some());
        assert!(controller
            .get_image_directory(ImageDirectoryId::SdCard)
            .is_some());
        assert!(controller
            .open_bis_partition(BisPartitionId::System)
            .is_ok());
        let _save_data_controller = controller.open_save_data_controller();
        let provider = provider.lock().unwrap();
        assert!(provider.has_slot(ContentProviderUnionSlot::SysNAND));
        assert!(provider.has_slot(ContentProviderUnionSlot::UserNAND));
        assert!(provider.has_slot(ContentProviderUnionSlot::SDMC));
        assert!(provider.has_slot(ContentProviderUnionSlot::External));
        drop(provider);
        assert!(controller.get_external_content_provider().is_some());

        set_ruzu_path(RuzuPath::NANDDir, &old_nand);
        set_ruzu_path(RuzuPath::LoadDir, &old_load);
        set_ruzu_path(RuzuPath::DumpDir, &old_dump);
        set_ruzu_path(RuzuPath::SDMCDir, &old_sdmc);

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn set_game_card_builds_both_upstream_cache_views() {
        let mut controller = FileSystemController::new();
        let image: VirtualFile = Arc::new(VectorVfsFile::new(
            vec![0; 0x200],
            "homebrew-test.xci".to_owned(),
            None,
        ));

        controller.set_game_card(image);

        assert!(controller.get_game_card().is_some());
        assert!(controller.get_game_card_contents().is_some());
        assert!(controller.get_game_card_placeholder().is_some());
        assert!(controller
            .get_registered_cache_for_storage(StorageId::GameCard)
            .is_some());
        assert!(controller
            .get_placeholder_cache_for_storage(StorageId::GameCard)
            .is_some());
    }

    #[test]
    fn create_factories_overwrite_recreates_factories_and_slots() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("ruzu-fsc-overwrite-{unique}"));
        let nand = base.join("nand");
        let load = base.join("load");
        let dump = base.join("dump");
        let sdmc = base.join("sdmc");

        fs::create_dir_all(nand.join("system/Contents/registered")).unwrap();
        fs::create_dir_all(nand.join("system/Contents/placehld")).unwrap();
        fs::create_dir_all(nand.join("user/Contents/registered")).unwrap();
        fs::create_dir_all(nand.join("user/Contents/placehld")).unwrap();
        fs::create_dir_all(&load).unwrap();
        fs::create_dir_all(&dump).unwrap();
        fs::create_dir_all(sdmc.join("Nintendo/Contents/registered")).unwrap();
        fs::create_dir_all(sdmc.join("Nintendo/Contents/placehld")).unwrap();
        fs::create_dir_all(sdmc.join("atmosphere/contents")).unwrap();

        let old_nand = get_ruzu_path(RuzuPath::NANDDir);
        let old_load = get_ruzu_path(RuzuPath::LoadDir);
        let old_dump = get_ruzu_path(RuzuPath::DumpDir);
        let old_sdmc = get_ruzu_path(RuzuPath::SDMCDir);

        set_ruzu_path(RuzuPath::NANDDir, &nand);
        set_ruzu_path(RuzuPath::LoadDir, &load);
        set_ruzu_path(RuzuPath::DumpDir, &dump);
        set_ruzu_path(RuzuPath::SDMCDir, &sdmc);

        let mut controller = FileSystemController::new();
        let vfs = crate::file_sys::vfs::vfs_real::RealVfsFilesystem::new();
        let provider = Arc::new(Mutex::new(ContentProviderUnion::new()));
        controller.set_content_provider(provider.clone());
        controller.create_factories(vfs.clone(), false);

        let first_sys = controller.get_system_nand_contents().unwrap() as *const _ as usize;
        let first_user = controller.get_user_nand_contents().unwrap() as *const _ as usize;
        let first_sdmc = controller.get_sdmc_contents().unwrap() as *const _ as usize;

        controller.create_factories(vfs, true);

        let second_sys = controller.get_system_nand_contents().unwrap() as *const _ as usize;
        let second_user = controller.get_user_nand_contents().unwrap() as *const _ as usize;
        let second_sdmc = controller.get_sdmc_contents().unwrap() as *const _ as usize;

        assert_ne!(first_sys, second_sys);
        assert_ne!(first_user, second_user);
        assert_ne!(first_sdmc, second_sdmc);

        let provider = provider.lock().unwrap();
        assert!(provider.has_slot(ContentProviderUnionSlot::SysNAND));
        assert!(provider.has_slot(ContentProviderUnionSlot::UserNAND));
        assert!(provider.has_slot(ContentProviderUnionSlot::SDMC));
        assert!(provider.has_slot(ContentProviderUnionSlot::External));

        set_ruzu_path(RuzuPath::NANDDir, &old_nand);
        set_ruzu_path(RuzuPath::LoadDir, &old_load);
        set_ruzu_path(RuzuPath::DumpDir, &old_dump);
        set_ruzu_path(RuzuPath::SDMCDir, &old_sdmc);

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn open_sdmc_returns_real_sdmc_root_after_factory_creation() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("ruzu-fsc-open-sdmc-{unique}"));
        let nand = base.join("nand");
        let load = base.join("load");
        let dump = base.join("dump");
        let sdmc = base.join("sdmc");

        fs::create_dir_all(nand.join("system/Contents/registered")).unwrap();
        fs::create_dir_all(nand.join("system/Contents/placehld")).unwrap();
        fs::create_dir_all(nand.join("user/Contents/registered")).unwrap();
        fs::create_dir_all(nand.join("user/Contents/placehld")).unwrap();
        fs::create_dir_all(&load).unwrap();
        fs::create_dir_all(&dump).unwrap();
        fs::create_dir_all(sdmc.join("Nintendo/Contents/registered")).unwrap();
        fs::create_dir_all(sdmc.join("Nintendo/Contents/placehld")).unwrap();
        fs::create_dir_all(sdmc.join("atmosphere/contents")).unwrap();
        fs::create_dir_all(sdmc.join("share/supertuxkart/data")).unwrap();
        fs::write(sdmc.join("share/supertuxkart/data/supertuxkart.1.5"), b"x").unwrap();

        let old_nand = get_ruzu_path(RuzuPath::NANDDir);
        let old_load = get_ruzu_path(RuzuPath::LoadDir);
        let old_dump = get_ruzu_path(RuzuPath::DumpDir);
        let old_sdmc = get_ruzu_path(RuzuPath::SDMCDir);

        set_ruzu_path(RuzuPath::NANDDir, &nand);
        set_ruzu_path(RuzuPath::LoadDir, &load);
        set_ruzu_path(RuzuPath::DumpDir, &dump);
        set_ruzu_path(RuzuPath::SDMCDir, &sdmc);

        let mut controller = FileSystemController::new();
        controller.create_factories(
            crate::file_sys::vfs::vfs_real::RealVfsFilesystem::new(),
            false,
        );

        let opened = controller.open_sdmc().unwrap();
        assert!(opened
            .get_file_relative("share/supertuxkart/data/supertuxkart.1.5")
            .is_some());
        assert!(
            controller.get_free_space_size(crate::file_sys::romfs_factory::StorageId::SdCard) > 0
        );
        assert_eq!(
            controller.get_total_space_size(crate::file_sys::romfs_factory::StorageId::SdCard),
            0x10000000000
        );

        set_ruzu_path(RuzuPath::NANDDir, &old_nand);
        set_ruzu_path(RuzuPath::LoadDir, &old_load);
        set_ruzu_path(RuzuPath::DumpDir, &old_dump);
        set_ruzu_path(RuzuPath::SDMCDir, &old_sdmc);

        let _ = fs::remove_dir_all(base);
    }
}

/// Port of upstream static `GetDirectoryRelativeWrapped` (filesystem.cpp:33-39).
///
/// Resolves a directory path relative to `base`, treating empty / "." / "/" / "\\"
/// as the base directory itself.
fn get_directory_relative_wrapped(base: &VirtualDir, dir_name: &str) -> Option<VirtualDir> {
    let dir_name = path_util::sanitize_path(dir_name, DirectorySeparator::ForwardSlash);
    if dir_name.is_empty() || dir_name == "." || dir_name == "/" || dir_name == "\\" {
        return Some(Arc::clone(base));
    }
    base.get_directory_relative(&dir_name)
}

/// Port of Service::FileSystem::VfsDirectoryServiceWrapper
///
/// Wraps a VfsDirectory with Result-returning methods for use with Switch services.
pub struct VfsDirectoryServiceWrapper {
    backing: VirtualDir,
}

impl VfsDirectoryServiceWrapper {
    pub fn new(backing: VirtualDir) -> Self {
        Self { backing }
    }

    /// Port of upstream `VfsDirectoryServiceWrapper::GetName` (filesystem.cpp:47-49).
    pub fn get_name(&self) -> String {
        self.backing.get_name()
    }

    /// Port of upstream `VfsDirectoryServiceWrapper::CreateFile` (filesystem.cpp:51-73).
    pub fn create_file(&self, path: &str, size: u64) -> Result<(), ResultCode> {
        let path = path_util::sanitize_path(path, DirectorySeparator::ForwardSlash);
        let dir = get_directory_relative_wrapped(&self.backing, &path_util::get_parent_path(&path))
            .ok_or(errors::RESULT_PATH_NOT_FOUND)?;

        if self.get_entry_type(&path).is_ok() {
            return Err(errors::RESULT_PATH_ALREADY_EXISTS);
        }

        let filename = path_util::get_filename(&path);
        let file = dir
            .create_file(filename)
            // Upstream TODO(DarkLordZach): Find a better error code for this
            .ok_or(RESULT_UNKNOWN)?;
        if !file.resize(size as usize) {
            // Upstream TODO(DarkLordZach): Find a better error code for this
            return Err(RESULT_UNKNOWN);
        }
        Ok(())
    }

    /// Port of upstream `VfsDirectoryServiceWrapper::DeleteFile` (filesystem.cpp:75-92).
    pub fn delete_file(&self, path: &str) -> Result<(), ResultCode> {
        let path = path_util::sanitize_path(path, DirectorySeparator::ForwardSlash);
        if path.is_empty() {
            // Upstream TODO(DarkLordZach): Why do games call this and what should it do?
            // Works as is but...
            return Ok(());
        }

        let dir = get_directory_relative_wrapped(&self.backing, &path_util::get_parent_path(&path))
            .ok_or(errors::RESULT_PATH_NOT_FOUND)?;
        let filename = path_util::get_filename(&path);
        if dir.get_file(filename).is_none() {
            return Err(errors::RESULT_PATH_NOT_FOUND);
        }
        if !dir.delete_file(filename) {
            // Upstream TODO(DarkLordZach): Find a better error code for this
            return Err(RESULT_UNKNOWN);
        }
        Ok(())
    }

    /// Port of upstream `VfsDirectoryServiceWrapper::CreateDirectory` (filesystem.cpp:94-112).
    ///
    /// NOTE: This is inaccurate behavior. CreateDirectory is not recursive.
    /// CreateDirectory should return PathNotFound if the parent directory does not exist.
    /// This is here temporarily in order to have UMM "work" in the meantime.
    /// Upstream TODO (Morph): Remove this when a hardware test verifies the correct behavior.
    pub fn create_directory(&self, path: &str) -> Result<(), ResultCode> {
        let path = path_util::sanitize_path(path, DirectorySeparator::ForwardSlash);
        let components = path_util::split_path_components_copy(&path);
        let mut relative_path = String::new();
        for component in &components {
            relative_path = path_util::sanitize_path(
                &format!("{}/{}", relative_path, component),
                DirectorySeparator::ForwardSlash,
            );
            if self.backing.create_subdirectory(&relative_path).is_none() {
                // Upstream TODO(DarkLordZach): Find a better error code for this
                return Err(RESULT_UNKNOWN);
            }
        }
        Ok(())
    }

    /// Port of upstream `VfsDirectoryServiceWrapper::DeleteDirectory` (filesystem.cpp:114-122).
    pub fn delete_directory(&self, path: &str) -> Result<(), ResultCode> {
        let path = path_util::sanitize_path(path, DirectorySeparator::ForwardSlash);
        let dir = get_directory_relative_wrapped(&self.backing, &path_util::get_parent_path(&path))
            .ok_or(RESULT_UNKNOWN)?;
        let filename = path_util::get_filename(&path);
        if !dir.delete_subdirectory(filename) {
            // Upstream TODO(DarkLordZach): Find a better error code for this
            return Err(RESULT_UNKNOWN);
        }
        Ok(())
    }

    /// Port of upstream `VfsDirectoryServiceWrapper::DeleteDirectoryRecursively`
    /// (filesystem.cpp:124-132).
    pub fn delete_directory_recursively(&self, path: &str) -> Result<(), ResultCode> {
        let path = path_util::sanitize_path(path, DirectorySeparator::ForwardSlash);
        let dir = get_directory_relative_wrapped(&self.backing, &path_util::get_parent_path(&path))
            .ok_or(RESULT_UNKNOWN)?;
        let filename = path_util::get_filename(&path);
        if !dir.delete_subdirectory_recursive(filename) {
            // Upstream TODO(DarkLordZach): Find a better error code for this
            return Err(RESULT_UNKNOWN);
        }
        Ok(())
    }

    /// Port of upstream `VfsDirectoryServiceWrapper::CleanDirectoryRecursively`
    /// (filesystem.cpp:134-144).
    pub fn clean_directory_recursively(&self, path: &str) -> Result<(), ResultCode> {
        let sanitized_path = path_util::sanitize_path(path, DirectorySeparator::ForwardSlash);
        let dir = get_directory_relative_wrapped(
            &self.backing,
            &path_util::get_parent_path(&sanitized_path),
        )
        .ok_or(RESULT_UNKNOWN)?;
        let filename = path_util::get_filename(&sanitized_path);
        if !dir.clean_subdirectory_recursive(filename) {
            // Upstream TODO(DarkLordZach): Find a better error code for this
            return Err(RESULT_UNKNOWN);
        }
        Ok(())
    }

    /// Port of upstream `VfsDirectoryServiceWrapper::RenameFile` (filesystem.cpp:146-187).
    pub fn rename_file(&self, src_path: &str, dest_path: &str) -> Result<(), ResultCode> {
        let src_path = path_util::sanitize_path(src_path, DirectorySeparator::ForwardSlash);
        let dest_path = path_util::sanitize_path(dest_path, DirectorySeparator::ForwardSlash);
        let src = self.backing.get_file_relative(&src_path);
        let dst = self.backing.get_file_relative(&dest_path);

        if path_util::get_parent_path(&src_path) == path_util::get_parent_path(&dest_path) {
            // Use more-optimized vfs implementation rename.
            let src = src.ok_or(errors::RESULT_PATH_NOT_FOUND)?;

            if let Some(ref dst) = dst {
                let full_path = dst.get_full_path();
                if std::path::Path::new(&full_path).exists() {
                    log::error!("File at new_path={} already exists", full_path);
                    return Err(errors::RESULT_PATH_ALREADY_EXISTS);
                }
            }

            let dest_filename = path_util::get_filename(&dest_path);
            if !src.rename(dest_filename) {
                // Upstream TODO(DarkLordZach): Find a better error code for this
                return Err(RESULT_UNKNOWN);
            }
            return Ok(());
        }

        // Move by hand -- Upstream TODO(DarkLordZach): Optimize
        let src = src.ok_or(errors::RESULT_PATH_NOT_FOUND)?;
        self.create_file(&dest_path, src.get_size() as u64)?;

        let dest = self
            .backing
            .get_file_relative(&dest_path)
            .expect("Newly created file with success cannot be found.");

        let bytes = src.read_all_bytes();
        assert_eq!(
            dest.write_bytes(&bytes, 0),
            src.get_size(),
            "Could not write all of the bytes but everything else has succeeded."
        );

        let src_filename = path_util::get_filename(&src_path);
        let src_dir = src.get_containing_directory().ok_or(RESULT_UNKNOWN)?;
        if !src_dir.delete_file(src_filename) {
            // Upstream TODO(DarkLordZach): Find a better error code for this
            return Err(RESULT_UNKNOWN);
        }

        Ok(())
    }

    /// Port of upstream `VfsDirectoryServiceWrapper::RenameDirectory` (filesystem.cpp:189-213).
    pub fn rename_directory(&self, src_path: &str, dest_path: &str) -> Result<(), ResultCode> {
        let src_path = path_util::sanitize_path(src_path, DirectorySeparator::ForwardSlash);
        let dest_path = path_util::sanitize_path(dest_path, DirectorySeparator::ForwardSlash);
        let src = get_directory_relative_wrapped(&self.backing, &src_path);

        if path_util::get_parent_path(&src_path) == path_util::get_parent_path(&dest_path) {
            // Use more-optimized vfs implementation rename.
            let src = src.ok_or(errors::RESULT_PATH_NOT_FOUND)?;
            let dest_filename = path_util::get_filename(&dest_path);
            if !src.rename(dest_filename) {
                // Upstream TODO(DarkLordZach): Find a better error code for this
                return Err(RESULT_UNKNOWN);
            }
            return Ok(());
        }

        // Upstream TODO(DarkLordZach): Implement renaming across the tree (move).
        panic!(
            "Could not rename directory with path \"{}\" to new path \"{}\" because parent dirs \
             don't match -- UNIMPLEMENTED",
            src_path, dest_path
        );
    }

    /// Port of upstream `VfsDirectoryServiceWrapper::OpenFile` (filesystem.cpp:215-236).
    pub fn open_file(&self, path: &str, mode: OpenMode) -> Result<VirtualFile, ResultCode> {
        let path = path_util::sanitize_path(path, DirectorySeparator::ForwardSlash);
        let npath = path.trim_start_matches(|c| c == '/' || c == '\\');

        let file = self
            .backing
            .get_file_relative(npath)
            .ok_or(errors::RESULT_PATH_NOT_FOUND)?;

        if mode == OpenMode::ALLOW_APPEND {
            let size = file.get_size();
            Ok(Arc::new(OffsetVfsFile::new(file, size, 0, String::new())))
        } else {
            Ok(file)
        }
    }

    /// Port of upstream `VfsDirectoryServiceWrapper::OpenDirectory` (filesystem.cpp:238-248).
    pub fn open_directory(&self, path: &str) -> Result<VirtualDir, ResultCode> {
        let path = path_util::sanitize_path(path, DirectorySeparator::ForwardSlash);
        let dir = get_directory_relative_wrapped(&self.backing, &path)
            // Upstream TODO(DarkLordZach): Find a better error code for this
            .ok_or(errors::RESULT_PATH_NOT_FOUND)?;
        Ok(dir)
    }

    /// Port of upstream `VfsDirectoryServiceWrapper::GetEntryType` (filesystem.cpp:250-276).
    pub fn get_entry_type(&self, path: &str) -> Result<DirectoryEntryType, ResultCode> {
        let path = path_util::sanitize_path(path, DirectorySeparator::ForwardSlash);
        let dir = get_directory_relative_wrapped(&self.backing, &path_util::get_parent_path(&path))
            .ok_or(errors::RESULT_PATH_NOT_FOUND)?;

        let filename = path_util::get_filename(&path);
        // Upstream TODO(Subv): Some games use the '/' path, find out what this means.
        if filename.is_empty() {
            return Ok(DirectoryEntryType::Directory);
        }

        if dir.get_file(filename).is_some() {
            return Ok(DirectoryEntryType::File);
        }

        if dir.get_subdirectory(filename).is_some() {
            return Ok(DirectoryEntryType::Directory);
        }

        Err(errors::RESULT_PATH_NOT_FOUND)
    }

    /// Port of upstream `VfsDirectoryServiceWrapper::GetFileTimeStampRaw`
    /// (filesystem.cpp:278-292).
    pub fn get_file_time_stamp_raw(&self, path: &str) -> Result<FileTimeStampRaw, ResultCode> {
        let dir = get_directory_relative_wrapped(&self.backing, &path_util::get_parent_path(path))
            .ok_or(errors::RESULT_PATH_NOT_FOUND)?;

        // Check that the entry exists
        self.get_entry_type(path)?;

        let filename = path_util::get_filename(path);
        Ok(dir.get_file_time_stamp(filename))
    }
}

/// Launches FileSystem services.
///
/// Matches upstream `void FileSystem::LoopProcess(Core::System& system)`:
/// Registers "fsp-ldr", "fsp:pr", "fsp-srv".
pub fn loop_process(system: crate::core::SystemRef, fsc: Arc<Mutex<FileSystemController>>) {
    register_services(system, fsc);
}

pub fn register_services(system: crate::core::SystemRef, fsc: Arc<Mutex<FileSystemController>>) {
    let server_manager = crate::hle::service::server_manager::ServerManager::new_shared(system);

    {
        let mut server_manager = server_manager.lock().unwrap();
        server_manager.register_named_service(
            "fsp-ldr",
            Box::new(|| -> SessionRequestHandlerPtr {
                Arc::new(super::fsp::fsp_ldr::FspLdr::new())
            }),
            64,
        );
        server_manager.register_named_service(
            "fsp:pr",
            Box::new(|| -> SessionRequestHandlerPtr { Arc::new(super::fsp::fsp_pr::FspPr::new()) }),
            64,
        );
        let fsc_for_closure = fsc.clone();
        server_manager.register_named_service(
            "fsp-srv",
            Box::new(move || -> SessionRequestHandlerPtr {
                Arc::new(super::fsp::fsp_srv::FspSrv::new_with_system(
                    system,
                    fsc_for_closure.clone(),
                ))
            }),
            64,
        );
    }

    crate::hle::service::server_manager::ServerManager::run_server_shared(server_manager);
}
