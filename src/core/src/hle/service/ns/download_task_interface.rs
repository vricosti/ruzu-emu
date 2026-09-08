// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/ns/download_task_interface.h
//! Port of zuyu/src/core/hle/service/ns/download_task_interface.cpp
//!
//! IDownloadTaskInterface — download task management for NS.

use std::collections::BTreeMap;

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::ipc_helpers::ResponseBuilder;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command table for IDownloadTaskInterface.
///
/// Corresponds to the function table in upstream download_task_interface.cpp.
pub mod commands {
    pub const CLEAR_TASK_STATUS_LIST: u32 = 701;
    pub const REQUEST_DOWNLOAD_TASK_LIST: u32 = 702;
    pub const REQUEST_ENSURE_DOWNLOAD_TASK: u32 = 703;
    pub const LIST_DOWNLOAD_TASK_STATUS: u32 = 704;
    pub const REQUEST_DOWNLOAD_TASK_LIST_DATA: u32 = 705;
    pub const TRY_COMMIT_CURRENT_APPLICATION_DOWNLOAD_TASK: u32 = 706;
    pub const ENABLE_AUTO_COMMIT: u32 = 707;
    pub const DISABLE_AUTO_COMMIT: u32 = 708;
    pub const TRIGGER_DYNAMIC_COMMIT_EVENT: u32 = 709;
}

/// IDownloadTaskInterface.
///
/// Corresponds to `IDownloadTaskInterface` in upstream.
pub struct IDownloadTaskInterface {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IDownloadTaskInterface {
    pub fn new() -> Self {
        let handlers = build_handler_map(&[
            (
                commands::CLEAR_TASK_STATUS_LIST,
                None,
                "ClearTaskStatusList",
            ),
            (
                commands::REQUEST_DOWNLOAD_TASK_LIST,
                None,
                "RequestDownloadTaskList",
            ),
            (
                commands::REQUEST_ENSURE_DOWNLOAD_TASK,
                None,
                "RequestEnsureDownloadTask",
            ),
            (
                commands::LIST_DOWNLOAD_TASK_STATUS,
                None,
                "ListDownloadTaskStatus",
            ),
            (
                commands::REQUEST_DOWNLOAD_TASK_LIST_DATA,
                None,
                "RequestDownloadTaskListData",
            ),
            (
                commands::TRY_COMMIT_CURRENT_APPLICATION_DOWNLOAD_TASK,
                None,
                "TryCommitCurrentApplicationDownloadTask",
            ),
            (commands::ENABLE_AUTO_COMMIT, Some(Self::enable_auto_commit_handler), "EnableAutoCommit"),
            (commands::DISABLE_AUTO_COMMIT, Some(Self::disable_auto_commit_handler), "DisableAutoCommit"),
            (
                commands::TRIGGER_DYNAMIC_COMMIT_EVENT,
                None,
                "TriggerDynamicCommitEvent",
            ),
        ]);
        Self {
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// EnableAutoCommit (cmd 707).
    fn enable_auto_commit_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let result = service.enable_auto_commit().map_or_else(|error| error, |_| RESULT_SUCCESS);
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }

    fn disable_auto_commit_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let result = service.disable_auto_commit().map_or_else(|error| error, |_| RESULT_SUCCESS);
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }

    ///
    /// Corresponds to upstream `IDownloadTaskInterface::EnableAutoCommit`.
    pub fn enable_auto_commit(&self) -> Result<(), ResultCode> {
        log::warn!("(STUBBED) EnableAutoCommit called");
        Ok(())
    }

    /// DisableAutoCommit (cmd 708).
    ///
    /// Corresponds to upstream `IDownloadTaskInterface::DisableAutoCommit`.
    pub fn disable_auto_commit(&self) -> Result<(), ResultCode> {
        log::warn!("(STUBBED) DisableAutoCommit called");
        Ok(())
    }
}

impl SessionRequestHandler for IDownloadTaskInterface {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "ns::IDownloadTaskInterface"
    }
}

impl ServiceFramework for IDownloadTaskInterface {
    fn get_service_name(&self) -> &str {
        "ns::IDownloadTaskInterface"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}
