// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/olsc/olsc_service_for_system_service.h
//! Port of zuyu/src/core/hle/service/olsc/olsc_service_for_system_service.cpp
//!
//! IOlscServiceForSystemService: "olsc:s" service implementation.

use std::collections::BTreeMap;

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::ResponseBuilder;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

use super::daemon_controller::IDaemonController;
use super::remote_storage_controller::IRemoteStorageController;
use super::transfer_task_list_controller::ITransferTaskListController;

/// IPC command table for IOlscServiceForSystemService.
///
/// Corresponds to the function table in upstream olsc_service_for_system_service.cpp constructor.
pub mod commands {
    pub const OPEN_TRANSFER_TASK_LIST_CONTROLLER: u32 = 0;
    pub const OPEN_REMOTE_STORAGE_CONTROLLER: u32 = 1;
    pub const OPEN_DAEMON_CONTROLLER: u32 = 2;
    pub const UNKNOWN_10: u32 = 10;
    pub const UNKNOWN_11: u32 = 11;
    pub const UNKNOWN_12: u32 = 12;
    pub const UNKNOWN_13: u32 = 13;
    pub const LIST_LAST_TRANSFER_TASK_ERROR_INFO: u32 = 100;
    pub const GET_LAST_ERROR_INFO_COUNT: u32 = 101;
    pub const REMOVE_LAST_ERROR_INFO_OLD: u32 = 102;
    pub const GET_LAST_ERROR_INFO: u32 = 103;
    pub const GET_LAST_ERROR_EVENT_HOLDER: u32 = 104;
    pub const GET_LAST_TRANSFER_TASK_ERROR_INFO: u32 = 105;
    pub const GET_DATA_TRANSFER_POLICY_INFO: u32 = 200;
    pub const REMOVE_DATA_TRANSFER_POLICY_INFO: u32 = 201;
    pub const UPDATE_DATA_TRANSFER_POLICY_OLD: u32 = 202;
    pub const UPDATE_DATA_TRANSFER_POLICY: u32 = 203;
    pub const CLEANUP_DATA_TRANSFER_POLICY_INFO: u32 = 204;
    pub const REQUEST_DATA_TRANSFER_POLICY: u32 = 205;
    pub const GET_AUTO_TRANSFER_SERIES_INFO: u32 = 300;
    pub const UPDATE_AUTO_TRANSFER_SERIES_INFO: u32 = 301;
    pub const CLEANUP_SAVE_DATA_ARCHIVE_INFO_TYPE1: u32 = 400;
    pub const CLEANUP_TRANSFER_TASK: u32 = 900;
    pub const CLEANUP_SERIES_INFO_TYPE0: u32 = 902;
    pub const CLEANUP_SAVE_DATA_ARCHIVE_INFO_TYPE0: u32 = 903;
    pub const CLEANUP_APPLICATION_AUTO_TRANSFER_SETTING: u32 = 904;
    pub const CLEANUP_ERROR_HISTORY: u32 = 905;
    pub const SET_LAST_ERROR: u32 = 906;
    pub const ADD_SAVE_DATA_ARCHIVE_INFO_TYPE0: u32 = 907;
    pub const REMOVE_SERIES_INFO_TYPE0: u32 = 908;
    pub const GET_SERIES_INFO_TYPE0: u32 = 909;
    pub const REMOVE_LAST_ERROR_INFO: u32 = 910;
    pub const CLEANUP_SERIES_INFO_TYPE1: u32 = 911;
    pub const REMOVE_SERIES_INFO_TYPE1: u32 = 912;
    pub const GET_SERIES_INFO_TYPE1: u32 = 913;
    pub const UPDATE_ISSUE_OLD: u32 = 1000;
    pub const UNKNOWN_1010: u32 = 1010;
    pub const LIST_ISSUE_INFO_OLD: u32 = 1011;
    pub const GET_ISSUE_OLD: u32 = 1012;
    pub const GET_ISSUE2_OLD: u32 = 1013;
    pub const GET_ISSUE3_OLD: u32 = 1014;
    pub const REPAIR_ISSUE_OLD: u32 = 1020;
    pub const REPAIR_ISSUE_WITH_USER_ID_OLD: u32 = 1021;
    pub const REPAIR_ISSUE2_OLD: u32 = 1022;
    pub const REPAIR_ISSUE3_OLD: u32 = 1023;
    pub const UNKNOWN_1024: u32 = 1024;
    pub const UPDATE_ISSUE: u32 = 1100;
    pub const UNKNOWN_1110: u32 = 1110;
    pub const LIST_ISSUE_INFO: u32 = 1111;
    pub const GET_ISSUE: u32 = 1112;
    pub const GET_ISSUE2: u32 = 1113;
    pub const GET_ISSUE3: u32 = 1114;
    pub const REPAIR_ISSUE: u32 = 1120;
    pub const REPAIR_ISSUE_WITH_USER_ID: u32 = 1121;
    pub const REPAIR_ISSUE2: u32 = 1122;
    pub const REPAIR_ISSUE3: u32 = 1123;
    pub const UNKNOWN_1124: u32 = 1124;
    pub const CLONE_SERVICE: u32 = 10000;
}

/// IOlscServiceForSystemService ("olsc:s").
///
/// Corresponds to `IOlscServiceForSystemService` in upstream olsc_service_for_system_service.cpp.
pub struct IOlscServiceForSystemService {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
    system: crate::core::SystemRef,
}

impl IOlscServiceForSystemService {
    /// Stub handler for nullptr entries -- logs STUBBED and returns RESULT_SUCCESS.
    fn stub_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let cmd = ctx.get_command();
        log::warn!("(STUBBED) IOlscServiceForSystemService command {}", cmd);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    pub fn new(system: crate::core::SystemRef) -> Self {
        let s = Some(Self::stub_handler as fn(&dyn ServiceFramework, &mut HLERequestContext));
        let handlers = build_handler_map(&[
            (
                0,
                Some(Self::open_transfer_task_list_controller_handler),
                "OpenTransferTaskListController",
            ),
            (
                1,
                Some(Self::open_remote_storage_controller_handler),
                "OpenRemoteStorageController",
            ),
            (
                2,
                Some(Self::open_daemon_controller_handler),
                "OpenDaemonController",
            ),
            (10, s, "Unknown10"),
            (11, s, "Unknown11"),
            (12, s, "Unknown12"),
            (13, s, "Unknown13"),
            (100, s, "ListLastTransferTaskErrorInfo"),
            (101, s, "GetLastErrorInfoCount"),
            (102, s, "RemoveLastErrorInfoOld"),
            (103, s, "GetLastErrorInfo"),
            (104, s, "GetLastErrorEventHolder"),
            (105, s, "GetLastTransferTaskErrorInfo"),
            (
                200,
                Some(Self::get_data_transfer_policy_info_handler),
                "GetDataTransferPolicyInfo",
            ),
            (201, s, "RemoveDataTransferPolicyInfo"),
            (202, s, "UpdateDataTransferPolicyOld"),
            (203, s, "UpdateDataTransferPolicy"),
            (204, s, "CleanupDataTransferPolicyInfo"),
            (205, s, "RequestDataTransferPolicy"),
            (300, s, "GetAutoTransferSeriesInfo"),
            (301, s, "UpdateAutoTransferSeriesInfo"),
            (400, s, "CleanupSaveDataArchiveInfoType1"),
            (900, s, "CleanupTransferTask"),
            (902, s, "CleanupSeriesInfoType0"),
            (903, s, "CleanupSaveDataArchiveInfoType0"),
            (904, s, "CleanupApplicationAutoTransferSetting"),
            (905, s, "CleanupErrorHistory"),
            (906, s, "SetLastError"),
            (907, s, "AddSaveDataArchiveInfoType0"),
            (908, s, "RemoveSeriesInfoType0"),
            (909, s, "GetSeriesInfoType0"),
            (910, s, "RemoveLastErrorInfo"),
            (911, s, "CleanupSeriesInfoType1"),
            (912, s, "RemoveSeriesInfoType1"),
            (913, s, "GetSeriesInfoType1"),
            (1000, s, "UpdateIssueOld"),
            (1010, s, "Unknown1010"),
            (1011, s, "ListIssueInfoOld"),
            (1012, s, "GetIssueOld"),
            (1013, s, "GetIssue2Old"),
            (1014, s, "GetIssue3Old"),
            (1020, s, "RepairIssueOld"),
            (1021, s, "RepairIssueWithUserIdOld"),
            (1022, s, "RepairIssue2Old"),
            (1023, s, "RepairIssue3Old"),
            (1024, s, "Unknown1024"),
            (1100, s, "UpdateIssue"),
            (1110, s, "Unknown1110"),
            (1111, s, "ListIssueInfo"),
            (1112, s, "GetIssue"),
            (1113, s, "GetIssue2"),
            (1114, s, "GetIssue3"),
            (1120, s, "RepairIssue"),
            (1121, s, "RepairIssueWithUserId"),
            (1122, s, "RepairIssue2"),
            (1123, s, "RepairIssue3"),
            (1124, s, "Unknown1124"),
            (10000, Some(Self::clone_service_handler), "CloneService"),
        ]);

        Self {
            handlers,
            handlers_tipc: BTreeMap::new(),
            system,
        }
    }

    /// Cmd 0: OpenTransferTaskListController
    ///
    /// Corresponds to `IOlscServiceForSystemService::OpenTransferTaskListController` in upstream.
    pub fn open_transfer_task_list_controller(&self) -> (ResultCode, ITransferTaskListController) {
        log::info!("IOlscServiceForSystemService::open_transfer_task_list_controller called");
        (RESULT_SUCCESS, ITransferTaskListController::new())
    }

    /// Cmd 1: OpenRemoteStorageController
    ///
    /// Corresponds to `IOlscServiceForSystemService::OpenRemoteStorageController` in upstream.
    pub fn open_remote_storage_controller(&self) -> (ResultCode, IRemoteStorageController) {
        log::info!("IOlscServiceForSystemService::open_remote_storage_controller called");
        (RESULT_SUCCESS, IRemoteStorageController::new())
    }

    /// Cmd 2: OpenDaemonController
    ///
    /// Corresponds to `IOlscServiceForSystemService::OpenDaemonController` in upstream.
    pub fn open_daemon_controller(&self) -> (ResultCode, IDaemonController) {
        log::info!("IOlscServiceForSystemService::open_daemon_controller called");
        (RESULT_SUCCESS, IDaemonController::new())
    }

    /// Cmd 200: GetDataTransferPolicyInfo
    ///
    /// Corresponds to `IOlscServiceForSystemService::GetDataTransferPolicyInfo` in upstream.
    pub fn get_data_transfer_policy_info(&self, _application_id: u64) -> (ResultCode, u16) {
        log::warn!("(STUBBED) IOlscServiceForSystemService::get_data_transfer_policy_info called");
        (RESULT_SUCCESS, 0)
    }

    /// Cmd 10000: CloneService
    ///
    /// Corresponds to `IOlscServiceForSystemService::CloneService` in upstream.
    /// Upstream returns shared_from_this(); we create a new instance since state is minimal.
    pub fn clone_service(&self) -> (ResultCode, IOlscServiceForSystemService) {
        log::info!("IOlscServiceForSystemService::clone_service called");
        (
            RESULT_SUCCESS,
            IOlscServiceForSystemService::new(self.system),
        )
    }

    // --- Handler bridge functions ---

    fn open_transfer_task_list_controller_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::info!("IOlscServiceForSystemService::OpenTransferTaskListController called");
        let service: std::sync::Arc<dyn SessionRequestHandler> =
            std::sync::Arc::new(ITransferTaskListController::new());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(service);
    }

    fn open_remote_storage_controller_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::info!("IOlscServiceForSystemService::OpenRemoteStorageController called");
        let service: std::sync::Arc<dyn SessionRequestHandler> =
            std::sync::Arc::new(IRemoteStorageController::new());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(service);
    }

    fn open_daemon_controller_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::info!("IOlscServiceForSystemService::OpenDaemonController called");
        let service: std::sync::Arc<dyn SessionRequestHandler> =
            std::sync::Arc::new(IDaemonController::new());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(service);
    }

    fn get_data_transfer_policy_info_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) IOlscServiceForSystemService::GetDataTransferPolicyInfo called");
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        // out_policy_info: u16, packed into u32 word
        rb.push_u32(0);
    }

    fn clone_service_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::info!("IOlscServiceForSystemService::CloneService called");
        let svc = unsafe {
            &*(this as *const dyn ServiceFramework as *const IOlscServiceForSystemService)
        };
        // Upstream returns shared_from_this(); we create a new instance since state is minimal.
        let service: std::sync::Arc<dyn SessionRequestHandler> =
            std::sync::Arc::new(IOlscServiceForSystemService::new(svc.system));
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(service);
    }
}

impl SessionRequestHandler for IOlscServiceForSystemService {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "olsc:s"
    }
}

impl ServiceFramework for IOlscServiceForSystemService {
    fn get_service_name(&self) -> &str {
        "olsc:s"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}
