//! Port of zuyu/src/core/hle/service/filesystem/fsp/fsp_srv.h and fsp_srv.cpp
//!
//! FSP_SRV service ("fsp-srv").

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use crate::file_sys::errors::RESULT_TARGET_NOT_FOUND;
use crate::file_sys::fs_save_data_types::{SaveDataAttribute, SaveDataSpaceId};
use crate::file_sys::nca_metadata::ContentRecordType;
use crate::file_sys::patch_manager::PatchManager;
use crate::file_sys::registered_cache::{ContentProvider, ContentProviderUnion};
use crate::file_sys::romfs_factory::StorageId;
use crate::file_sys::vfs::vfs_types::VirtualFile;
use crate::hle::result::{ResultCode, RESULT_SUCCESS, RESULT_UNKNOWN};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

use super::super::filesystem::FileSystemController;
use super::fs_i_filesystem::IFileSystem;
use super::fs_i_save_data_info_reader::ISaveDataInfoReader;
use super::fs_i_storage::IStorage;
use super::fsp_types::SizeGetter;

// Extension beyond Eden's null command 8: libnx fs.c's two wire layouts.
// Explicit padding avoids reading a u64 from offset 4 for the legacy command.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct OpenFileSystemWithIdParameters {
    fs_type: u32,
    padding: u32,
    program_id: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct OpenFileSystemWithIdAndAttributesParameters {
    attributes: u8,
    padding: [u8; 3],
    fs_type: u32,
    program_id: u64,
}

/// Port of Service::FileSystem::AccessLogVersion
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum AccessLogVersion {
    V7_0_0 = 2,
}

impl AccessLogVersion {
    pub const LATEST: Self = Self::V7_0_0;
}

/// Port of Service::FileSystem::AccessLogMode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum AccessLogMode {
    None = 0,
    Log = 1,
    SdCard = 2,
}

/// IPC command table for FSP_SRV ("fsp-srv"):
///
/// | Cmd  | Name                                                          |
/// |------|---------------------------------------------------------------|
/// | 0    | OpenFileSystem                                                |
/// | 1    | SetCurrentProcess                                             |
/// | 2    | OpenDataFileSystemByCurrentProcess                             |
/// | 7    | OpenFileSystemWithPatch                                       |
/// | 8    | OpenFileSystemWithId                                          |
/// | 9    | OpenDataFileSystemByApplicationId                              |
/// | 11   | OpenBisFileSystem                                             |
/// | 12   | OpenBisStorage                                                |
/// | 13   | InvalidateBisCache                                            |
/// | 17   | OpenHostFileSystem                                            |
/// | 18   | OpenSdCardFileSystem                                          |
/// | 19   | FormatSdCardFileSystem                                        |
/// | 21   | DeleteSaveDataFileSystem                                      |
/// | 22   | CreateSaveDataFileSystem                                      |
/// | 23   | CreateSaveDataFileSystemBySystemSaveDataId                     |
/// | 24   | RegisterSaveDataFileSystemAtomicDeletion                       |
/// | 25   | DeleteSaveDataFileSystemBySaveDataSpaceId                      |
/// | 26   | FormatSdCardDryRun                                            |
/// | 27   | IsExFatSupported                                              |
/// | 28   | DeleteSaveDataFileSystemBySaveDataAttribute                    |
/// | 30   | OpenGameCardStorage                                           |
/// | 31   | OpenGameCardFileSystem                                        |
/// | 32   | ExtendSaveDataFileSystem                                      |
/// | 33   | DeleteCacheStorage                                            |
/// | 34   | GetCacheStorageSize                                           |
/// | 35   | CreateSaveDataFileSystemByHashSalt                             |
/// | 36   | OpenHostFileSystemWithOption                                   |
/// | 51   | OpenSaveDataFileSystem                                        |
/// | 52   | OpenSaveDataFileSystemBySystemSaveDataId                       |
/// | 53   | OpenReadOnlySaveDataFileSystem                                 |
/// | 57   | ReadSaveDataFileSystemExtraDataBySaveDataSpaceId                |
/// | 58   | ReadSaveDataFileSystemExtraData                                 |
/// | 59   | WriteSaveDataFileSystemExtraData                                |
/// | 60   | OpenSaveDataInfoReader                                          |
/// | 61   | OpenSaveDataInfoReaderBySaveDataSpaceId                         |
/// | 62   | OpenSaveDataInfoReaderOnlyCacheStorage                          |
/// | 64   | OpenSaveDataInternalStorageFileSystem                           |
/// | 65   | UpdateSaveDataMacForDebug                                       |
/// | 66   | WriteSaveDataFileSystemExtraData2                                |
/// | 67   | FindSaveDataWithFilter                                           |
/// | 68   | OpenSaveDataInfoReaderBySaveDataFilter                           |
/// | 69   | ReadSaveDataFileSystemExtraDataBySaveDataAttribute               |
/// | 70   | WriteSaveDataFileSystemExtraDataWithMaskBySaveDataAttribute      |
/// | 71   | ReadSaveDataFileSystemExtraDataWithMaskBySaveDataAttribute       |
/// | 80   | OpenSaveDataMetaFile                                             |
/// | 81   | OpenSaveDataTransferManager                                      |
/// | 82   | OpenSaveDataTransferManagerVersion2                               |
/// | 83   | OpenSaveDataTransferProhibiter                                    |
/// | 84   | ListApplicationAccessibleSaveDataOwnerId                          |
/// | 85   | OpenSaveDataTransferManagerForSaveDataRepair                       |
/// | 86   | OpenSaveDataMover                                                  |
/// | 87   | OpenSaveDataTransferManagerForRepair                               |
/// | 100  | OpenImageDirectoryFileSystem                                       |
/// | 101  | OpenBaseFileSystem                                                  |
/// | 102  | FormatBaseFileSystem                                                |
/// | 110  | OpenContentStorageFileSystem                                        |
/// | 120  | OpenCloudBackupWorkStorageFileSystem                                |
/// | 130  | OpenCustomStorageFileSystem                                          |
/// | 200  | OpenDataStorageByCurrentProcess                                      |
/// | 201  | OpenDataStorageByProgramId                                            |
/// | 202  | OpenDataStorageByDataId                                                |
/// | 203  | OpenPatchDataStorageByCurrentProcess                                   |
/// | 204  | OpenDataFileSystemByProgramIndex                                       |
/// | 205  | OpenDataStorageWithProgramIndex                                        |
/// | 206  | OpenDataStorageByPath                                                  |
/// | 210  | SetCurrentProcess                                                      |
/// | 400  | OpenDeviceOperator                                                     |
/// | 500  | OpenSdCardDetectionEventNotifier                                       |
/// | 501  | OpenGameCardDetectionEventNotifier                                     |
/// | 510  | OpenSystemDataUpdateEventNotifier                                      |
/// | 511  | NotifySystemDataUpdateEvent                                            |
/// | 520  | SimulateGameCardDetectionEvent                                         |
/// | 600-617 | (various utility commands)                                          |
/// | 620  | SetSdCardEncryptionSeed                                                |
/// | 630-631 | SD card accessibility                                               |
/// | 640  | IsSignedSystemPartitionOnSdCardValid                                   |
/// | 700-720 | Access failure resolver                                             |
/// | 800  | GetAndClearFileSystemProxyErrorInfo                                    |
/// | 810  | RegisterProgramIndexMapInfo                                            |
/// | 1000-1019 | Debug/development commands                                       |
/// | 1003 | DisableAutoSaveDataCreation                                            |
/// | 1004 | SetGlobalAccessLogMode                                                 |
/// | 1005 | GetGlobalAccessLogMode                                                 |
/// | 1006 | OutputAccessLogToSdCard                                                |
/// | 1011 | GetProgramIndexForAccessLog                                            |
/// | 1016 | FlushAccessLogOnSdCard                                                 |
/// | 1100 | OverrideSaveDataTransferTokenSignVerificationKey                        |
/// | 1110 | CorruptSaveDataFileSystemBySaveDataSpaceId2                             |
/// | 1200 | OpenMultiCommitManager                                                 |
/// | 1300 | OpenBisWiper                                                           |
pub struct FspSrv {
    /// Upstream: `FileSystemController& fsc`.
    fsc: Option<Arc<std::sync::Mutex<super::super::filesystem::FileSystemController>>>,
    /// Upstream: `FileSys::ContentProviderUnion& content_provider`.
    content_provider: Option<Arc<StdMutex<ContentProviderUnion>>>,
    /// Upstream constructor-owned Reporter reference. Absent only in the
    /// existing controller-less construction used by table tests.
    reporter: Option<Arc<crate::reporter::Reporter>>,
    current_process_id: std::sync::Mutex<u64>,
    access_log_program_index: std::sync::Mutex<u32>,
    // Preserve all raw IPC enum values, including values unknown to this SDK.
    access_log_mode: std::sync::Mutex<u32>,
    program_id: std::sync::Mutex<u64>,
    /// Upstream: `FileSys::VirtualFile romfs`.
    romfs: std::sync::Mutex<Option<VirtualFile>>,
    /// Upstream: `std::shared_ptr<SaveDataController> save_data_controller`.
    save_data_controller:
        std::sync::Mutex<Option<super::super::save_data_controller::SaveDataController>>,
    /// Upstream: `std::shared_ptr<RomFsController> romfs_controller`.
    romfs_controller: std::sync::Mutex<Option<super::super::romfs_controller::RomFsController>>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl FspSrv {
    fn make_default_size_getter() -> SizeGetter {
        SizeGetter {
            get_free_size: Box::new(|| 0),
            get_total_size: Box::new(|| 0),
        }
    }

    fn make_size_getter_from_storage_id(
        fsc: Arc<std::sync::Mutex<FileSystemController>>,
        id: StorageId,
    ) -> SizeGetter {
        let fsc_for_free = Arc::clone(&fsc);
        SizeGetter {
            get_free_size: Box::new(move || fsc_for_free.lock().unwrap().get_free_space_size(id)),
            get_total_size: Box::new(move || fsc.lock().unwrap().get_total_space_size(id)),
        }
    }

    fn storage_id_for_save_data_space(space_id: SaveDataSpaceId) -> Option<StorageId> {
        match space_id {
            SaveDataSpaceId::User => Some(StorageId::NandUser),
            SaveDataSpaceId::SdSystem | SaveDataSpaceId::SdUser => Some(StorageId::SdCard),
            SaveDataSpaceId::System => Some(StorageId::NandSystem),
            SaveDataSpaceId::Temporary
            | SaveDataSpaceId::ProperSystem
            | SaveDataSpaceId::SafeMode => None,
        }
    }

    pub fn new() -> Self {
        Self {
            fsc: None,
            content_provider: None,
            reporter: None,
            current_process_id: std::sync::Mutex::new(0),
            access_log_program_index: std::sync::Mutex::new(0),
            access_log_mode: std::sync::Mutex::new(
                if *common::settings::values().enable_fs_access_log.get_value() {
                    AccessLogMode::SdCard as u32
                } else {
                    AccessLogMode::None as u32
                },
            ),
            program_id: std::sync::Mutex::new(0),
            romfs: std::sync::Mutex::new(None),
            save_data_controller: std::sync::Mutex::new(None),
            romfs_controller: std::sync::Mutex::new(None),
            handlers: build_handler_map(&[
                (32, Some(Self::extend_save_data_file_system_handler), "ExtendSaveDataFileSystem"),
                (57, Some(Self::read_save_data_file_system_extra_data_by_save_data_space_id_handler), "ReadSaveDataFileSystemExtraDataBySaveDataSpaceId"),
                (67, Some(Self::find_save_data_with_filter_handler), "FindSaveDataWithFilter"),
                (83, Some(Self::open_save_data_transfer_prohibiter_handler), "OpenSaveDataTransferProhibiter"),
                (
                    23,
                    Some(Self::create_save_data_file_system_by_system_save_data_id_handler),
                    "CreateSaveDataFileSystemBySystemSaveDataId",
                ),
                (
                    1,
                    Some(Self::set_current_process_handler),
                    "SetCurrentProcess",
                ),
                (
                    2,
                    Some(Self::open_data_file_system_by_current_process_handler),
                    "OpenDataFileSystemByCurrentProcess",
                ),
                (
                    18,
                    Some(Self::open_sd_card_file_system_handler),
                    "OpenSdCardFileSystem",
                ),
                (8, Some(Self::open_file_system_with_id_handler), "OpenFileSystemWithId"),
                (10, Some(Self::open_file_system_with_id_and_attributes_handler), "OpenFileSystemWithIdAndAttributes"),
                (
                    51,
                    Some(Self::open_save_data_file_system_handler),
                    "OpenSaveDataFileSystem",
                ),
                (
                    52,
                    Some(Self::open_save_data_file_system_by_system_save_data_id_handler),
                    "OpenSaveDataFileSystemBySystemSaveDataId",
                ),
                (
                    53,
                    Some(Self::open_read_only_save_data_file_system_handler),
                    "OpenReadOnlySaveDataFileSystem",
                ),
                (
                    61,
                    Some(Self::open_save_data_info_reader_by_save_data_space_id_handler),
                    "OpenSaveDataInfoReaderBySaveDataSpaceId",
                ),
                (
                    62,
                    Some(Self::open_save_data_info_reader_only_cache_storage_handler),
                    "OpenSaveDataInfoReaderOnlyCacheStorage",
                ),
                (
                    200,
                    Some(Self::open_data_storage_by_current_process_handler),
                    "OpenDataStorageByCurrentProcess",
                ),
                (
                    202,
                    Some(Self::open_data_storage_by_data_id_handler),
                    "OpenDataStorageByDataId",
                ),
                (
                    203,
                    Some(Self::open_patch_data_storage_by_current_process_handler),
                    "OpenPatchDataStorageByCurrentProcess",
                ),
                (
                    205,
                    Some(Self::open_data_storage_with_program_index_handler),
                    "OpenDataStorageWithProgramIndex",
                ),
                (
                    210,
                    Some(Self::set_current_process_handler),
                    "SetCurrentProcess",
                ),
                (
                    1004,
                    Some(Self::set_global_access_log_mode_handler),
                    "SetGlobalAccessLogMode",
                ),
                (
                    1005,
                    Some(Self::get_global_access_log_mode_handler),
                    "GetGlobalAccessLogMode",
                ),
                (
                    1006,
                    Some(Self::output_access_log_to_sd_card_handler),
                    "OutputAccessLogToSdCard",
                ),
                (
                    1011,
                    Some(Self::get_program_index_for_access_log_handler),
                    "GetProgramIndexForAccessLog",
                ),
                (
                    1016,
                    Some(Self::flush_access_log_on_sd_card_handler),
                    "FlushAccessLogOnSdCard",
                ),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// Create an FspSrv with the upstream constructor-owned filesystem and
    /// content provider references.
    pub fn new_with_system(
        system: crate::core::SystemRef,
        fsc: Arc<std::sync::Mutex<super::super::filesystem::FileSystemController>>,
    ) -> Self {
        let mut srv = Self::new();
        srv.fsc = Some(fsc);
        srv.content_provider = if system.is_null() {
            None
        } else {
            system.get().get_content_provider().cloned()
        };
        if !system.is_null() {
            srv.reporter = Some(Arc::clone(system.get_reporter()));
        }
        srv
    }

    /// Push an error response for a command that has Out<SharedPointer<T>> in domain mode.
    /// In domain mode, the response always includes a domain object ID slot (0 for null),
    /// matching upstream CMIF serialization where response layout is compile-time fixed.
    fn push_error_with_null_interface(ctx: &mut HLERequestContext, error: u32) {
        let is_domain = ctx
            .get_manager()
            .map_or(false, |m| m.lock().unwrap().is_domain());
        if is_domain {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
            rb.push_result(ResultCode::new(error));
            // Add a null domain object — WriteToOutgoingCommandBuffer will write 0
            // to the domain object ID slot, matching upstream behavior.
            ctx.add_null_domain_object();
        } else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(ResultCode::new(error));
        }
    }

    fn push_interface_response(
        ctx: &mut HLERequestContext,
        object: Arc<dyn SessionRequestHandler>,
    ) {
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(object);
    }

    fn open_file_system_with_id_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = this.as_any().downcast_ref::<Self>().unwrap();
        let input = RequestParser::new(ctx).pop_raw::<OpenFileSystemWithIdParameters>();
        service.open_file_system_with_id_response(ctx, input.fs_type, input.program_id, 0);
    }

    fn open_file_system_with_id_and_attributes_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = this.as_any().downcast_ref::<Self>().unwrap();
        let input = RequestParser::new(ctx).pop_raw::<OpenFileSystemWithIdAndAttributesParameters>();
        service.open_file_system_with_id_response(ctx, input.fs_type, input.program_id, input.attributes);
    }

    fn open_file_system_with_id_response(&self, ctx: &mut HLERequestContext, fs_type: u32, program_id: u64, attributes: u8) {
        let bytes = ctx.read_buffer(0);
        let path = bytes.iter().take(0x301).position(|byte| *byte == 0)
            .and_then(|end| std::str::from_utf8(&bytes[..end]).ok());
        let result = path.ok_or(crate::file_sys::errors::RESULT_INVALID_PATH)
            .and_then(|path| self.open_file_system_with_id(fs_type, program_id, attributes, path));
        match result {
            Ok(directory) => Self::push_interface_response(ctx, Arc::new(IFileSystem::new(directory, Self::make_default_size_getter()))),
            Err(error) => {
                log::warn!("OpenFileSystemWithId failed: type={fs_type}, id={program_id:016x}, attributes={attributes:#x}, path={path:?}, result={:#x}", error.raw());
                Self::push_error_with_null_interface(ctx, error.raw());
            }
        }
    }

    /// User-authorized HLE extension, not an Eden implementation. Open the
    /// catalogued NCA identified by NS's guest path, never a host filesystem path.
    /// This slice supports document/control/data RomFS mounts. Other filesystem
    /// kinds fail explicitly, rather than returning success without an object.
    fn open_file_system_with_id(&self, fs_type: u32, program_id: u64, attributes: u8, path: &str)
        -> Result<crate::file_sys::vfs::vfs_types::VirtualDir, common::ResultCode>
    {
        use crate::file_sys::content_archive::{NCA, NCAContentType};
        use crate::file_sys::errors::{RESULT_INVALID_ARGUMENT, RESULT_NOT_IMPLEMENTED, RESULT_PATH_NOT_FOUND};
        use crate::file_sys::partition_filesystem::ResultStatus;
        use crate::file_sys::registered_cache::{get_base_title_id, get_update_title_id};

        if attributes & !0xf != 0 { return Err(RESULT_INVALID_ARGUMENT); }
        let expected = match fs_type {
            3 => NCAContentType::Control,
            4 => NCAContentType::Manual,
            6 => NCAContentType::Data,
            _ => return Err(RESULT_NOT_IMPLEMENTED),
        };
        let provider = self.content_provider.as_ref().ok_or(RESULT_PATH_NOT_FOUND)?.lock().unwrap();
        for (slot, entry) in provider.list_entries_filter_origin(None, None, None, None) {
            if program_id != 0 && entry.title_id != program_id
                && entry.title_id != get_update_title_id(program_id) { continue; }
            if provider.get_entry_content_path(slot, entry.title_id, entry.record_type).as_deref() != Some(path) { continue; }
            let raw = provider.get_entry_raw_from_slot(slot, entry.title_id, entry.record_type)
                .ok_or(RESULT_PATH_NOT_FOUND)?;
            let base_id = get_base_title_id(entry.title_id);
            let base = (base_id != entry.title_id).then(|| provider.get_entry(base_id, entry.record_type)).flatten();
            let nca = NCA::new(raw, base.as_ref());
            if nca.get_status() != ResultStatus::Success {
                log::warn!("OpenFileSystemWithId: NCA {path} failed: {:?}", nca.get_status());
                return Err(RESULT_PATH_NOT_FOUND);
            }
            if nca.get_type() != expected && !(fs_type == 6 && nca.get_type() == NCAContentType::PublicData) {
                return Err(RESULT_INVALID_ARGUMENT);
            }
            let directory = crate::file_sys::romfs::extract_romfs(nca.get_romfs()).ok_or(RESULT_PATH_NOT_FOUND)?;
            log::info!("OpenFileSystemWithId: mounted {path}, type={fs_type}, id={program_id:016x}");
            return Ok(directory);
        }
        Err(RESULT_PATH_NOT_FOUND)
    }

    /// Port of upstream `FSP_SRV::SetCurrentProcess` (fsp_srv.cpp:186-193).
    ///
    /// Upstream calls `fsc.OpenProcess(&program_id, ..., current_process_id)`
    /// which looks up the process_id → program_id mapping registered by the
    /// NCA loader via `FileSystemController::RegisterProcess`.
    fn set_current_process_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const FspSrv) };
        let pid = ctx.get_pid();
        *service.current_process_id.lock().unwrap() = pid;

        // Upstream: fsc.OpenProcess(&program_id, &save_data_controller, &romfs_controller, pid)
        let result = service
            .fsc
            .as_ref()
            .and_then(|fsc| fsc.lock().unwrap().open_process(pid));

        match result {
            Some((program_id, save_data_ctrl, romfs_ctrl)) => {
                *service.program_id.lock().unwrap() = program_id;
                *service.romfs.lock().unwrap() = None;
                *service.save_data_controller.lock().unwrap() = Some(save_data_ctrl);
                *service.romfs_controller.lock().unwrap() = Some(romfs_ctrl);
                log::info!(
                    "FspSrv::SetCurrentProcess: pid={:#x}, program_id={:#018x}",
                    pid,
                    program_id,
                );
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(RESULT_SUCCESS);
            }
            None => {
                // No registration found — matches upstream returning ResultTargetNotFound.
                log::warn!(
                    "FspSrv::SetCurrentProcess: pid={:#x} not registered with FileSystemController",
                    pid,
                );
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(ResultCode::new(RESULT_TARGET_NOT_FOUND.raw()));
            }
        }
    }

    fn open_sd_card_file_system_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const FspSrv) };
        log::info!("FspSrv::OpenSdCardFileSystem called");
        let Some(fsc) = service.fsc.as_ref() else {
            Self::push_error_with_null_interface(ctx, RESULT_TARGET_NOT_FOUND.raw());
            return;
        };

        let sdmc_dir = {
            let fsc = fsc.lock().unwrap();
            match fsc.open_sdmc() {
                Ok(dir) => dir,
                Err(rc) => {
                    Self::push_error_with_null_interface(ctx, rc.raw());
                    return;
                }
            }
        };

        let size_getter =
            Self::make_size_getter_from_storage_id(Arc::clone(fsc), StorageId::SdCard);

        Self::push_interface_response(ctx, Arc::new(IFileSystem::new(sdmc_dir, size_getter)));
    }

    fn parse_save_data_space_id(raw: u8) -> Option<SaveDataSpaceId> {
        match raw {
            0 => Some(SaveDataSpaceId::System),
            1 => Some(SaveDataSpaceId::User),
            2 => Some(SaveDataSpaceId::SdSystem),
            3 => Some(SaveDataSpaceId::Temporary),
            4 => Some(SaveDataSpaceId::SdUser),
            100 => Some(SaveDataSpaceId::ProperSystem),
            101 => Some(SaveDataSpaceId::SafeMode),
            _ => None,
        }
    }

    fn create_save_data_file_system_by_system_save_data_id_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        use crate::file_sys::fs_save_data_types::{SaveDataRank, SaveDataType};
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let words = RequestParser::new(ctx).pop_raw::<[u32; 16]>();
        // Decode enum bytes before constructing Rust enums: arbitrary IPC bytes
        // cannot safely be copied into a Rust enum. CreationInfo follows this
        // attribute but is unused by upstream's implementation of command 23.
        let save_type = match words[8] as u8 {
            0 => Some(SaveDataType::System),
            1 => Some(SaveDataType::Account),
            2 => Some(SaveDataType::Bcat),
            3 => Some(SaveDataType::Device),
            4 => Some(SaveDataType::Temporary),
            5 => Some(SaveDataType::Cache),
            6 => Some(SaveDataType::SystemBcat),
            _ => None,
        };
        let rank = match (words[8] >> 8) as u8 {
            0 => Some(SaveDataRank::Primary),
            1 => Some(SaveDataRank::Secondary),
            _ => None,
        };
        let mut result = ResultCode::new(RESULT_TARGET_NOT_FOUND.raw());
        if let (Some(save_type), Some(rank)) = (save_type, rank) {
            let attribute = SaveDataAttribute::make(
                words[0] as u64 | ((words[1] as u64) << 32),
                save_type,
                [
                    words[2] as u64 | ((words[3] as u64) << 32),
                    words[4] as u64 | ((words[5] as u64) << 32),
                ],
                words[6] as u64 | ((words[7] as u64) << 32),
                (words[8] >> 16) as u16,
                rank,
            );
            let created = service
                .save_data_controller
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|controller| {
                    controller.create_save_data(SaveDataSpaceId::System, &attribute)
                });
            if created.is_some() {
                result = RESULT_SUCCESS;
            }
        }
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    fn open_save_data_file_system_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const FspSrv) };
        let mut rp = RequestParser::new(ctx);

        let Some(space_id) = Self::parse_save_data_space_id(rp.pop_u8()) else {
            log::error!("FspSrv::OpenSaveDataFileSystem: invalid SaveDataSpaceId");
            Self::push_error_with_null_interface(ctx, RESULT_TARGET_NOT_FOUND.raw());
            return;
        };

        // CMIF serializes the u8 enum with padding before the following 0x40-byte struct.
        rp.skip(1);

        let attribute = {
            let size = core::mem::size_of::<SaveDataAttribute>();
            let words = (size + 3) / 4;
            let start = rp.get_current_offset();
            if start + words > crate::hle::ipc::COMMAND_BUFFER_LENGTH {
                log::error!("FspSrv::OpenSaveDataFileSystem: request payload too small");
                Self::push_error_with_null_interface(ctx, RESULT_TARGET_NOT_FOUND.raw());
                return;
            }

            let mut value = core::mem::MaybeUninit::<SaveDataAttribute>::zeroed();
            unsafe {
                let src = ctx.command_buffer()[start..].as_ptr() as *const u8;
                let dst = value.as_mut_ptr() as *mut u8;
                core::ptr::copy_nonoverlapping(src, dst, size);
                value.assume_init()
            }
        };

        log::info!(
            "FspSrv::OpenSaveDataFileSystem called, space={:?}, attribute={}",
            space_id,
            attribute.debug_info(),
        );

        let opened = service
            .save_data_controller
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|controller| controller.open_save_data(space_id, &attribute));

        let Some(dir) = opened else {
            log::warn!(
                "FspSrv::OpenSaveDataFileSystem: save data not found for {}",
                attribute.debug_info(),
            );
            Self::push_error_with_null_interface(ctx, RESULT_TARGET_NOT_FOUND.raw());
            return;
        };

        let size_getter = service
            .fsc
            .as_ref()
            .and_then(|fsc| {
                Self::storage_id_for_save_data_space(space_id)
                    .map(|id| Self::make_size_getter_from_storage_id(Arc::clone(fsc), id))
            })
            .unwrap_or_else(Self::make_default_size_getter);

        Self::push_interface_response(ctx, Arc::new(IFileSystem::new(dir, size_getter)));
    }

    fn open_save_data_file_system_by_system_save_data_id_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!(
            "(STUBBED) OpenSaveDataFileSystemBySystemSaveDataId called, delegating to OpenSaveDataFileSystem"
        );
        Self::open_save_data_file_system_handler(this, ctx);
    }

    fn open_read_only_save_data_file_system_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!(
            "(STUBBED) OpenReadOnlySaveDataFileSystem called, delegating to OpenSaveDataFileSystem"
        );
        Self::open_save_data_file_system_handler(this, ctx);
    }

    /// Port of upstream `FSP_SRV::OpenSaveDataInfoReaderBySaveDataSpaceId`.
    fn open_save_data_info_reader_by_save_data_space_id_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const FspSrv) };
        let mut rp = RequestParser::new(ctx);
        let Some(space) = Self::parse_save_data_space_id(rp.pop_u8()) else {
            log::error!("FspSrv::OpenSaveDataInfoReaderBySaveDataSpaceId: invalid SaveDataSpaceId");
            Self::push_error_with_null_interface(ctx, RESULT_TARGET_NOT_FOUND.raw());
            return;
        };

        log::info!(
            "FspSrv::OpenSaveDataInfoReaderBySaveDataSpaceId called, space={:?}",
            space
        );
        let controller = service.save_data_controller.lock().unwrap().clone();
        let Some(controller) = controller else {
            Self::push_error_with_null_interface(ctx, RESULT_TARGET_NOT_FOUND.raw());
            return;
        };

        Self::push_interface_response(ctx, Arc::new(ISaveDataInfoReader::new(controller, space)));
    }

    /// Port of upstream `FSP_SRV::OpenSaveDataInfoReaderOnlyCacheStorage`.
    fn open_save_data_info_reader_only_cache_storage_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const FspSrv) };
        log::warn!("(STUBBED) FspSrv::OpenSaveDataInfoReaderOnlyCacheStorage called");

        let controller = service.save_data_controller.lock().unwrap().clone();
        let Some(controller) = controller else {
            Self::push_error_with_null_interface(ctx, RESULT_TARGET_NOT_FOUND.raw());
            return;
        };

        Self::push_interface_response(
            ctx,
            Arc::new(ISaveDataInfoReader::new(
                controller,
                SaveDataSpaceId::Temporary,
            )),
        );
    }

    /// Port of upstream `FSP_SRV::OpenDataFileSystemByCurrentProcess`.
    fn open_data_file_system_by_current_process_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const FspSrv) };
        log::debug!("FspSrv::OpenDataFileSystemByCurrentProcess called");

        let backend = {
            let mut cached_romfs = service.romfs.lock().unwrap();
            if cached_romfs.is_none() {
                let current_romfs = service
                    .romfs_controller
                    .lock()
                    .unwrap()
                    .as_ref()
                    .and_then(|controller| controller.open_romfs_current_process());
                if current_romfs.is_none() {
                    log::error!(
                        "FspSrv::OpenDataFileSystemByCurrentProcess: no file system interface available"
                    );
                    Self::push_error_with_null_interface(ctx, RESULT_UNKNOWN.get_inner_value());
                    return;
                }
                *cached_romfs = current_romfs;
            }
            cached_romfs.as_ref().cloned().unwrap()
        };

        let Some(extracted) = crate::file_sys::romfs::extract_romfs(Some(backend)) else {
            log::error!(
                "FspSrv::OpenDataFileSystemByCurrentProcess: failed to extract RomFS for the current process"
            );
            Self::push_error_with_null_interface(ctx, RESULT_UNKNOWN.get_inner_value());
            return;
        };

        let size_getter = service
            .fsc
            .as_ref()
            .map(|fsc| Self::make_size_getter_from_storage_id(Arc::clone(fsc), StorageId::NandUser))
            .unwrap_or_else(Self::make_default_size_getter);
        Self::push_interface_response(ctx, Arc::new(IFileSystem::new(extracted, size_getter)));
    }

    fn open_data_storage_by_current_process_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const FspSrv) };
        let program_id = *service.program_id.lock().unwrap();
        let pid = *service.current_process_id.lock().unwrap();

        log::info!(
            "FspSrv::OpenDataStorageByCurrentProcess called, current_process_id={:#x}, program_id={:#x}",
            pid,
            program_id
        );

        let backend = {
            let mut cached_romfs = service.romfs.lock().unwrap();
            if cached_romfs.is_none() {
                let current_romfs = service
                    .romfs_controller
                    .lock()
                    .unwrap()
                    .as_ref()
                    .and_then(|controller| controller.open_romfs_current_process());
                if current_romfs.is_none() {
                    log::error!("FspSrv::OpenDataStorageByCurrentProcess: no RomFS available");
                    Self::push_error_with_null_interface(ctx, u32::MAX);
                    return;
                }
                *cached_romfs = current_romfs;
            }
            cached_romfs.as_ref().cloned().unwrap()
        };

        Self::push_interface_response(ctx, Arc::new(IStorage::new(backend)));
    }

    /// Port of upstream `FSP_SRV::OpenDataStorageByDataId`.
    /// Opens a system data archive by title ID. Tries to synthesize
    /// known system archives (MiiModel, NgWord, SharedFont, etc.).
    fn open_data_storage_by_data_id_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const FspSrv) };
        let mut rp = RequestParser::new(ctx);
        let storage_id = rp.pop_raw::<u8>();
        let _unknown = rp.pop_raw::<u32>();
        let title_id = rp.pop_raw::<u64>();

        let storage_id = match storage_id {
            0 => StorageId::None,
            1 => StorageId::Host,
            2 => StorageId::GameCard,
            3 => StorageId::NandSystem,
            4 => StorageId::NandUser,
            5 => StorageId::SdCard,
            other => {
                log::error!(
                    "FspSrv::OpenDataStorageByDataId: invalid storage_id={}",
                    other
                );
                Self::push_error_with_null_interface(ctx, u32::MAX);
                return;
            }
        };

        log::info!(
            "FspSrv::OpenDataStorageByDataId called, storage_id={}, title_id={:#x}",
            storage_id as u8,
            title_id
        );

        let ctrl_guard = service.romfs_controller.lock().unwrap();
        let controller = ctrl_guard.as_ref();
        log::info!(
            "FspSrv::OpenDataStorageByDataId trace: controller_present={} title_id={:#x} storage_id={}",
            controller.is_some(),
            title_id,
            storage_id as u8,
        );
        let data =
            controller.and_then(|c| c.open_romfs(title_id, storage_id, ContentRecordType::Data));
        log::info!(
            "FspSrv::OpenDataStorageByDataId trace: romfs_present={}",
            data.is_some()
        );
        // Upstream does not open the base NCA on the missing-data fallback.
        let nca = data.as_ref().and_then(|_| {
            controller.and_then(|c| c.open_base_nca(title_id, storage_id, ContentRecordType::Data))
        });
        log::info!(
            "FspSrv::OpenDataStorageByDataId trace: nca_present={}",
            nca.is_some()
        );
        drop(ctrl_guard);

        if let Some(data) = data {
            let storage = match (service.fsc.as_ref(), service.content_provider.as_ref()) {
                (Some(fs_controller), Some(content_provider)) => {
                    let fs_controller = fs_controller.lock().unwrap();
                    let content_provider = content_provider.lock().unwrap();
                    let patch_manager =
                        PatchManager::new(title_id, &fs_controller, &*content_provider);
                    patch_manager.patch_romfs(
                        nca.as_ref(),
                        data,
                        ContentRecordType::Data,
                        None,
                        true,
                    )
                }
                _ => data,
            };
            Self::push_interface_response(ctx, Arc::new(IStorage::new(storage)));
            return;
        }

        // Try synthesizing the system archive (matches upstream fallback path)
        if let Some(archive) =
            crate::file_sys::system_archive::system_archive::synthesize_system_archive(title_id)
        {
            log::info!(
                "FspSrv::OpenDataStorageByDataId: synthesized archive for title_id={:#x}",
                title_id
            );
            Self::push_interface_response(ctx, Arc::new(IStorage::new(archive)));
            return;
        }

        log::warn!(
            "FspSrv::OpenDataStorageByDataId: no data for title_id={:#x}, returning error",
            title_id
        );
        Self::push_error_with_null_interface(ctx, RESULT_UNKNOWN.get_inner_value());
    }

    fn open_patch_data_storage_by_current_process_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const FspSrv) };
        let program_id = *service.program_id.lock().unwrap();

        log::warn!(
            "FspSrv::OpenPatchDataStorageByCurrentProcess called, program_id={:#x}; returning ResultTargetNotFound like upstream",
            program_id
        );

        // Upstream: this command has Out<SharedPointer<IStorage>> in its signature.
        // In domain mode, the response ALWAYS includes a domain object ID slot
        // (containing 0 for null) even on error, because the response layout is
        // computed from the method signature at compile time.
        // Without this, the game reads past the result expecting a domain object ID,
        // gets garbage, and eventually crashes.
        Self::push_error_with_null_interface(ctx, RESULT_TARGET_NOT_FOUND.raw());
    }

    /// Port of upstream `FSP_SRV::OpenDataStorageWithProgramIndex`.
    fn open_data_storage_with_program_index_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const FspSrv) };
        let mut rp = RequestParser::new(ctx);
        let program_index = rp.pop_raw::<u8>();
        let program_id = *service.program_id.lock().unwrap();

        log::info!(
            "FspSrv::OpenDataStorageWithProgramIndex called, program_index={}",
            program_index
        );

        let patched_romfs =
            service
                .romfs_controller
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|controller| {
                    controller.open_patched_romfs_with_program_index(program_id, program_index)
                });

        let Some(patched_romfs) = patched_romfs else {
            log::error!(
                "FspSrv::OpenDataStorageWithProgramIndex: could not open storage with program_index={}",
                program_index
            );
            Self::push_error_with_null_interface(ctx, u32::MAX);
            return;
        };

        Self::push_interface_response(ctx, Arc::new(IStorage::new(patched_romfs)));
    }

    fn set_global_access_log_mode_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const FspSrv) };
        let mode_raw = ctx.command_buffer()[ctx.get_data_payload_offset() as usize + 2];
        *service.access_log_mode.lock().unwrap() = mode_raw;
        log::debug!("FspSrv::SetGlobalAccessLogMode called, mode={}", mode_raw);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_global_access_log_mode_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const FspSrv) };
        let mode = *service.access_log_mode.lock().unwrap();
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(mode);
    }

    fn output_access_log_to_sd_card_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const FspSrv) };
        log::debug!("FspSrv::OutputAccessLogToSdCard called");
        // InBuffer<HipcMapAlias> reads A, not the auto-select A/X helper.
        let buffer = ctx.read_buffer_a(0);
        let length = buffer
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(buffer.len());
        service
            .reporter
            .as_ref()
            .expect("FspSrv requires its System Reporter")
            .save_fs_access_log(&buffer[..length]);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn extend_save_data_file_system_handler(_: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        // Upstream has no save-ID index and deliberately leaves extension inert.
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }

    fn read_save_data_file_system_extra_data_by_save_data_space_id_handler(_: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let bytes = vec![0; ctx.get_write_buffer_size(0)];
        ctx.write_buffer(&bytes, 0);
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }

    fn find_save_data_with_filter_handler(_: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        // Eden returns TargetNotFound, not an empty success. CMIF value-initializes
        // the count; Rust also clears the otherwise unspecified scratch buffer.
        let bytes = vec![0; ctx.get_write_buffer_size(0)];
        ctx.write_buffer(&bytes, 0);
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(ResultCode::new(RESULT_TARGET_NOT_FOUND.raw()));
        rb.push_i64(0);
    }

    fn open_save_data_transfer_prohibiter_handler(_: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let child = Arc::new(super::save_data_transfer_prohibiter::ISaveDataTransferProhibiter::new());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(child);
    }

    fn flush_access_log_on_sd_card_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("FspSrv::FlushAccessLogOnSdCard (STUBBED) called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_program_index_for_access_log_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const FspSrv) };
        let version = AccessLogVersion::LATEST as u32;
        let program_index = *service.access_log_program_index.lock().unwrap();

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(version);
        rb.push_u32(program_index);
    }
}

impl SessionRequestHandler for FspSrv {
    fn as_any(&self) -> &dyn std::any::Any { self }
    fn handle_sync_request(&self, context: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, context)
    }

    fn service_name(&self) -> &str {
        "fsp-srv"
    }
}

impl ServiceFramework for FspSrv {
    fn get_service_name(&self) -> &str {
        "fsp-srv"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }

    fn invoke_request(&self, ctx: &mut HLERequestContext)
    where
        Self: Sized,
    {
        let cmd = ctx.get_command();
        if let Some(fi) = self.handlers().get(&cmd) {
            if let Some(callback) = fi.handler_callback {
                log::trace!("Service::{}: {}", self.get_service_name(), fi.name);
                callback(self, ctx);
                return;
            }
        }

        log::warn!(
            "FspSrv: unimplemented command '{}' returned stub success",
            cmd
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_mount_ipc_layouts_match_libnx() {
        assert_eq!(std::mem::size_of::<OpenFileSystemWithIdParameters>(), 16);
        assert_eq!(std::mem::offset_of!(OpenFileSystemWithIdParameters, program_id), 8);
        assert_eq!(std::mem::size_of::<OpenFileSystemWithIdAndAttributesParameters>(), 16);
        assert_eq!(std::mem::offset_of!(OpenFileSystemWithIdAndAttributesParameters, fs_type), 4);
        assert_eq!(std::mem::offset_of!(OpenFileSystemWithIdAndAttributesParameters, program_id), 8);
        let mut ctx = HLERequestContext::new();
        ctx.command_buffer_mut()[2..6].copy_from_slice(&[0xa5a5a50f, 4, 0x12345678, 0x01000000]);
        let parsed = RequestParser::new(&ctx).pop_raw::<OpenFileSystemWithIdAndAttributesParameters>();
        assert_eq!(parsed.attributes, 15);
        assert_eq!(parsed.fs_type, 4);
        assert_eq!(parsed.program_id, 0x0100000012345678);
        let service = FspSrv::new();
        for command in [8, 10] {
            let mut ctx = HLERequestContext::new();
            service.handlers[&command].handler_callback.unwrap()(&service, &mut ctx);
            assert_eq!(ctx.command_buffer()[6], crate::file_sys::errors::RESULT_INVALID_PATH.raw());
        }
    }

    #[test]
    fn document_mount_never_opens_a_host_path_or_missing_content() {
        let service = FspSrv::new();
        for path in ["C:/Windows/win.ini", "@UserContent://../secret.nca", "@UserContent://missing.nca"] {
            assert_eq!(service.open_file_system_with_id(4, 42, 0, path).err(),
                Some(crate::file_sys::errors::RESULT_PATH_NOT_FOUND));
        }
        assert_eq!(service.open_file_system_with_id(4, 42, 0x80, "").err(),
            Some(crate::file_sys::errors::RESULT_INVALID_ARGUMENT));
        assert_eq!(service.open_file_system_with_id(7, 42, 0, "").err(),
            Some(crate::file_sys::errors::RESULT_NOT_IMPLEMENTED));
    }

    #[test]
    fn save_metadata_stubs_have_upstream_result_and_payload_widths() {
        let service = FspSrv::new();
        for command in [32, 57, 67] {
            let mut ctx = HLERequestContext::new();
            service.handlers[&command].handler_callback.unwrap()(&service, &mut ctx);
            let mut expected = HLERequestContext::new();
            let mut rb = ResponseBuilder::new(&mut expected, if command == 67 { 4 } else { 2 }, 0, 0);
            rb.push_result(if command == 67 { ResultCode::new(RESULT_TARGET_NOT_FOUND.raw()) } else { RESULT_SUCCESS });
            if command == 67 { rb.push_i64(0); }
            drop(rb);
            assert_eq!(ctx.command_buffer(), expected.command_buffer());
        }
        assert!(service.handlers[&83].handler_callback.is_some());
    }

    #[test]
    fn create_system_save_creates_a_reopenable_directory() {
        use crate::file_sys::fs_filesystem::OpenMode;
        use crate::file_sys::fs_save_data_types::{SaveDataRank, SaveDataType};
        use crate::file_sys::savedata_factory::SaveDataFactory;
        use crate::file_sys::vfs::vfs_real::RealVfsFilesystem;
        use crate::hle::service::filesystem::save_data_controller::SaveDataController;
        let path = std::env::temp_dir().join(format!(
            "ruzu-system-save-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        let root = RealVfsFilesystem::new()
            .arc_open_directory(&path.to_string_lossy(), OpenMode::READ_WRITE)
            .unwrap();
        let factory = Arc::new(StdMutex::new(SaveDataFactory::new(42, root)));
        factory.lock().unwrap().set_auto_create(false);
        let controller = SaveDataController::with_factory(factory);
        let attribute = SaveDataAttribute::make(
            0,
            SaveDataType::System,
            [0, 0],
            0x8000000000000042,
            0,
            SaveDataRank::Primary,
        );
        assert!(controller
            .open_save_data(SaveDataSpaceId::System, &attribute)
            .is_none());
        let service = FspSrv::new();
        *service.save_data_controller.lock().unwrap() = Some(controller.clone());
        let mut ctx = HLERequestContext::new();
        // Input attribute begins after the command's u64 ID; creation info is zero.
        ctx.command_buffer_mut()[8] = 0x42;
        ctx.command_buffer_mut()[9] = 0x80000000;
        service.handlers[&23].handler_callback.unwrap()(&service, &mut ctx);
        assert_eq!(ctx.command_buffer()[6], RESULT_SUCCESS.get_inner_value());
        assert!(controller
            .open_save_data(SaveDataSpaceId::System, &attribute)
            .is_some());
        assert!(controller
            .open_save_data(SaveDataSpaceId::User, &attribute)
            .is_none());
        drop(service);
        drop(controller);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn missing_data_storage_returns_unknown_like_upstream() {
        let service = FspSrv::new();
        let mut ctx = HLERequestContext::new();
        ctx.command_buffer_mut()[2] = StorageId::None as u32;
        ctx.command_buffer_mut()[3] = 0;
        ctx.command_buffer_mut()[4] = 42; // Synthetic, non-system title.
        ctx.command_buffer_mut()[5] = 0;
        service.handlers[&202].handler_callback.unwrap()(&service, &mut ctx);
        assert_eq!(ctx.command_buffer()[6], RESULT_UNKNOWN.get_inner_value());
    }

    #[test]
    fn firmware_23_registers_open_data_file_system_and_aliased_set_current_process() {
        let service = FspSrv::new();
        assert_eq!(
            service.handlers[&2].name,
            "OpenDataFileSystemByCurrentProcess"
        );
        assert!(service.handlers[&2].handler_callback.is_some());
        assert_eq!(service.handlers[&210].name, "SetCurrentProcess");
        assert!(service.handlers[&210].handler_callback.is_some());
    }

    #[test]
    fn access_log_setting_and_ipc_preserve_modes_and_bytes() {
        const CHILD: &str = "RUZU_TEST_FS_ACCESS_LOG";
        if std::env::var_os(CHILD).is_none() {
            assert!(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "hle::service::filesystem::fsp::fsp_srv::tests::access_log_setting_and_ipc_preserve_modes_and_bytes"])
                .env(CHILD, "1").status().unwrap().success());
            return;
        }
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                use crate::core::{System, SystemRef};
                use crate::device_memory::DeviceMemory;
                use crate::hle::ipc;
                use crate::memory::memory::Memory;
                use common::fs::path_util::{set_ruzu_path, RuzuPath};
                use common::page_table::{PageTable, PageType};
                let nonce = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let directory = std::env::temp_dir()
                    .join(format!("ruzu-fs-log-{}-{nonce}", std::process::id()));
                std::fs::create_dir(&directory).unwrap();
                std::fs::create_dir(directory.join("sdmc")).unwrap();
                set_ruzu_path(RuzuPath::LogDir, &directory);
                set_ruzu_path(RuzuPath::SDMCDir, &directory.join("sdmc"));
                let system = Box::new(System::new());
                let device = Box::new(DeviceMemory::new());
                let mut table = Box::new(PageTable::new());
                table.resize(32, 12);
                table.entries.get_and_fault(3).store(
                    false,
                    PageType::Memory,
                    1,
                    device.buffer.backing_base_pointer() as usize,
                );
                let memory = Arc::new(StdMutex::new(unsafe {
                    Memory::new(
                        SystemRef::null(),
                        device.as_ref() as *const _,
                        &device.buffer as *const _,
                    )
                }));
                memory
                    .lock()
                    .unwrap()
                    .set_current_page_table(table.as_mut() as *mut _, true);
                let log = directory.join("sdmc/FsAccessLog.txt");
                let mut expected = Vec::new();
                for enabled in [false, true] {
                    common::settings::values_mut()
                        .enable_fs_access_log
                        .set_value(enabled);
                    // Reporter gate is deliberately independent of FS access logging.
                    common::settings::values_mut()
                        .reporting_services
                        .set_value(false);
                    let service = FspSrv::new_with_system(
                        SystemRef::from_ref(&system),
                        Arc::new(StdMutex::new(FileSystemController::new())),
                    );
                    assert!(Arc::ptr_eq(
                        service.reporter.as_ref().unwrap(),
                        &system.reporter
                    ));
                    let invoke = |command, value| {
                        let mut ctx = HLERequestContext::new();
                        ctx.command_buffer_mut()[2] = value;
                        service.handlers[&command].handler_callback.unwrap()(&service, &mut ctx);
                        assert_eq!(ctx.command_buffer()[6], 0);
                        ctx
                    };
                    assert_eq!(
                        invoke(1005, 0).command_buffer()[8],
                        if enabled { 2 } else { 0 }
                    );
                    // The setting seeds each service, not every GetGlobalAccessLogMode.
                    common::settings::values_mut()
                        .enable_fs_access_log
                        .set_value(!enabled);
                    for mode in [0, 1, 2, 3, u32::MAX] {
                        invoke(1004, mode);
                        assert_eq!(invoke(1005, 0).command_buffer()[8], mode);
                    }
                    for message in [
                        b"first\n\0ignored".as_slice(),
                        b"\xFF\x80\n",
                        b"",
                        b"\0ignored",
                    ] {
                        for (i, &byte) in message.iter().enumerate() {
                            memory.lock().unwrap().write_8(0x3000 + i as u64, byte);
                        }
                        let mut words = [0u32; ipc::COMMAND_BUFFER_LENGTH];
                        words[0] = ipc::CommandType::Request as u32 | (1 << 20);
                        words[1] = 8;
                        words[2..5].copy_from_slice(&[message.len() as u32, 0x3000, 0]);
                        words[8] = u32::from_le_bytes(*b"SFCI");
                        words[10] = 1006;
                        let mut ctx = HLERequestContext::new();
                        ctx.populate_from_incoming_command_buffer(&words);
                        ctx.set_memory(memory.clone());
                        assert_eq!(ctx.get_command(), 1006);
                        service.handlers[&1006].handler_callback.unwrap()(&service, &mut ctx);
                        assert_eq!(ctx.command_buffer()[6], 0);
                        for &byte in message.iter().take_while(|&&byte| byte != 0) {
                            if cfg!(windows) && byte == b'\n' {
                                expected.push(b'\r');
                            }
                            expected.push(byte);
                        }
                        assert_eq!(std::fs::read(&log).unwrap(), expected);
                    }
                    invoke(1016, 0);
                    assert_eq!(std::fs::read(&log).unwrap(), expected);
                }
                drop(memory);
                drop(table);
                drop(device);
                drop(system);
                std::fs::remove_dir_all(directory).unwrap();
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn save_data_info_reader_handlers_match_upstream_table() {
        let service = FspSrv::new();
        let by_space = service.handlers.get(&61).unwrap();
        let cache_only = service.handlers.get(&62).unwrap();

        assert_eq!(by_space.name, "OpenSaveDataInfoReaderBySaveDataSpaceId");
        assert!(by_space.handler_callback.is_some());
        assert_eq!(cache_only.name, "OpenSaveDataInfoReaderOnlyCacheStorage");
        assert!(cache_only.handler_callback.is_some());
    }
}
