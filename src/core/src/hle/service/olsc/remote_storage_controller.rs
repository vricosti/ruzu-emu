// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of Eden src/core/hle/service/olsc/remote_storage_controller.h/.cpp

use std::collections::BTreeMap;

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::cmif_serialization::{CmifRequest, CmifResponse};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SecondarySaveOutput {
    has_secondary_save: bool,
    _padding: [u8; 7],
    unknown: [u64; 3],
}

const _: () = assert!(std::mem::size_of::<SecondarySaveOutput>() == 0x20);

/// `IRemoteStorageController` — remote save-data storage operations.
pub struct IRemoteStorageController {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IRemoteStorageController {
    pub fn new() -> Self {
        // clang-format off
        let handlers = build_handler_map(&[
            (0, None, "GetSaveDataArchiveInfoBySaveDataId"),
            (1, None, "GetSaveDataArchiveInfoByApplicationId"),
            (3, None, "GetSaveDataArchiveCount"),
            (6, None, "CleanupSaveDataArchives"),
            (7, None, "CreateSaveDataArchiveCacheUpdationTask"),
            (
                8,
                None,
                "CreateSaveDataArchiveCacheUpdationForSpecifiedApplicationTask",
            ),
            (9, None, "Delete"),
            (10, None, "GetSeriesInfo"),
            (11, None, "CreateDeleteDataTask"),
            (12, None, "DeleteSeriesInfo"),
            (13, None, "CreateRegisterNotificationTokenTask"),
            (
                14,
                Some(Self::get_data_newness_by_application_id_handler),
                "GetDataNewnessByApplicationId",
            ),
            (
                15,
                None,
                "RegisterUploadSaveDataTransferTaskForAutonomyRegistration",
            ),
            (16, None, "CreateCleanupToDeleteSaveDataArchiveInfoTask"),
            (17, None, "ListDataInfo"),
            (18, Some(Self::get_data_info_handler), "GetDataInfoV1"),
            (19, None, "GetDataInfoCacheUpdateNativeHandleHolder"),
            (
                20,
                None,
                "CreateSaveDataArchiveInfoCacheForSaveDataBackupUpdationTask",
            ),
            (21, None, "ListSecondarySaves"),
            (
                22,
                Some(Self::get_secondary_save_handler),
                "GetSecondarySave",
            ),
            (23, None, "TouchSecondarySave"),
            (24, None, "GetSecondarySaveDataInfo"),
            (
                25,
                None,
                "RegisterDownloadSaveDataTransferTaskForAutonomyRegistration",
            ),
            (26, None, "Unknown26"),
            (27, Some(Self::get_data_info_handler), "GetDataInfoV2"),
            (28, None, "Unknown28"),
            (29, None, "Unknown29"),
            (800, None, "Unknown800"),
            (900, None, "SetLoadedDataMissing"),
            (901, None, "Unknown901"),
        ]);
        // clang-format on
        Self {
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn as_self(this: &dyn ServiceFramework) -> &Self {
        // The callback is registered only in this concrete service's table.
        unsafe { &*(this as *const dyn ServiceFramework as *const Self) }
    }

    /// `GetSecondarySave` (command 22).
    pub fn get_secondary_save(&self, application_id: u64) -> (ResultCode, bool, [u64; 3]) {
        log::error!(
            "(STUBBED) IRemoteStorageController::GetSecondarySave called, application_id={:016X}",
            application_id
        );
        (RESULT_SUCCESS, false, [0; 3])
    }

    /// `GetDataNewnessByApplicationId` (command 14).
    pub fn get_data_newness_by_application_id(&self, application_id: u64) -> (ResultCode, u8) {
        log::warn!(
            "(STUBBED) IRemoteStorageController::GetDataNewnessByApplicationId called, application_id={:016X}",
            application_id
        );
        (RESULT_SUCCESS, 0)
    }

    /// `GetDataInfo` (commands 18 and 27).
    pub fn get_data_info(&self, application_id: u64) -> (ResultCode, [u8; 0x38]) {
        log::warn!(
            "(STUBBED) IRemoteStorageController::GetDataInfo called, application_id={:016X}",
            application_id
        );
        (RESULT_SUCCESS, [0; 0x38])
    }

    fn get_secondary_save_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let application_id = CmifRequest::new(ctx).u64();
        let (result, has_secondary_save, unknown) = service.get_secondary_save(application_id);
        let output = SecondarySaveOutput {
            has_secondary_save,
            _padding: [0; 7],
            unknown,
        };
        let mut response = CmifResponse::new(ctx, 10, 0, 0);
        response.push_result(result);
        response.push_raw(&output);
    }

    fn get_data_newness_by_application_id_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let application_id = CmifRequest::new(ctx).u64();
        let (result, newness) = service.get_data_newness_by_application_id(application_id);
        let mut response = CmifResponse::new(ctx, 3, 0, 0);
        response.push_result(result);
        response.push_raw(&newness);
    }

    fn get_data_info_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let application_id = CmifRequest::new(ctx).u64();
        let (result, data) = service.get_data_info(application_id);
        let mut response = CmifResponse::new(ctx, 16, 0, 0);
        response.push_result(result);
        response.push_raw(&data);
    }
}

impl Default for IRemoteStorageController {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionRequestHandler for IRemoteStorageController {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "IRemoteStorageController"
    }
}

impl ServiceFramework for IRemoteStorageController {
    fn get_service_name(&self) -> &str {
        "IRemoteStorageController"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_table_matches_upstream() {
        let service = IRemoteStorageController::new();
        let entries = service
            .handlers
            .iter()
            .map(|(id, info)| (*id, info.name, info.handler_callback.is_some()))
            .collect::<Vec<_>>();
        assert_eq!(
            entries,
            [
                (0, "GetSaveDataArchiveInfoBySaveDataId", false),
                (1, "GetSaveDataArchiveInfoByApplicationId", false),
                (3, "GetSaveDataArchiveCount", false),
                (6, "CleanupSaveDataArchives", false),
                (7, "CreateSaveDataArchiveCacheUpdationTask", false),
                (
                    8,
                    "CreateSaveDataArchiveCacheUpdationForSpecifiedApplicationTask",
                    false
                ),
                (9, "Delete", false),
                (10, "GetSeriesInfo", false),
                (11, "CreateDeleteDataTask", false),
                (12, "DeleteSeriesInfo", false),
                (13, "CreateRegisterNotificationTokenTask", false),
                (14, "GetDataNewnessByApplicationId", true),
                (
                    15,
                    "RegisterUploadSaveDataTransferTaskForAutonomyRegistration",
                    false
                ),
                (16, "CreateCleanupToDeleteSaveDataArchiveInfoTask", false),
                (17, "ListDataInfo", false),
                (18, "GetDataInfoV1", true),
                (19, "GetDataInfoCacheUpdateNativeHandleHolder", false),
                (
                    20,
                    "CreateSaveDataArchiveInfoCacheForSaveDataBackupUpdationTask",
                    false
                ),
                (21, "ListSecondarySaves", false),
                (22, "GetSecondarySave", true),
                (23, "TouchSecondarySave", false),
                (24, "GetSecondarySaveDataInfo", false),
                (
                    25,
                    "RegisterDownloadSaveDataTransferTaskForAutonomyRegistration",
                    false
                ),
                (26, "Unknown26", false),
                (27, "GetDataInfoV2", true),
                (28, "Unknown28", false),
                (29, "Unknown29", false),
                (800, "Unknown800", false),
                (900, "SetLoadedDataMissing", false),
                (901, "Unknown901", false),
            ]
        );
    }

    #[test]
    fn implemented_stub_outputs_match_upstream() {
        let service = IRemoteStorageController::new();
        assert_eq!(
            service.get_secondary_save(0x201),
            (RESULT_SUCCESS, false, [0; 3])
        );
        assert_eq!(
            service.get_data_newness_by_application_id(0x202),
            (RESULT_SUCCESS, 0)
        );
        assert_eq!(service.get_data_info(0x203), (RESULT_SUCCESS, [0; 0x38]));
    }

    #[test]
    fn secondary_save_output_layout_matches_cmif_template() {
        assert_eq!(std::mem::size_of::<SecondarySaveOutput>(), 0x20);
        assert_eq!(std::mem::offset_of!(SecondarySaveOutput, unknown), 8);
    }
}
