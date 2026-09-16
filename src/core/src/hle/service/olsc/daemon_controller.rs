// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/olsc/daemon_controller.h
//! Port of zuyu/src/core/hle/service/olsc/daemon_controller.cpp
//!
//! IDaemonController: manages auto-transfer settings for accounts.

use std::collections::BTreeMap;

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IDaemonController.
///
/// Corresponds to `IDaemonController` in upstream daemon_controller.cpp.
pub struct IDaemonController {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IDaemonController {
    pub fn new() -> Self {
        let s: Option<fn(&dyn ServiceFramework, &mut HLERequestContext)> = Some(Self::stub_handler);
        let handlers = build_handler_map(&[
            (0, s, "GetAutoTransferEnabledForAccountAndApplication"),
            (1, s, "SetAutoTransferEnabledForAccountAndApplication"),
            (2, s, "GetGlobalUploadEnabledForAccount"),
            (3, s, "SetGlobalUploadEnabledForAccount"),
            (4, s, "TouchAccount"),
            (5, s, "GetGlobalDownloadEnabledForAccount"),
            (6, s, "SetGlobalDownloadEnabledForAccount"),
            (10, s, "GetForbiddenSaveDataIndication"),
            (11, s, "GetStopperObject"),
            (12, Some(Self::get_autonomy_task_status_handler), "GetAutonomyTaskStatus"),
        ]);
        Self {
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// Eden IDaemonController::GetAutonomyTaskStatus always reports idle.
    pub fn get_autonomy_task_status(&self, user_id: u128) -> (ResultCode, u8) {
        log::info!("IDaemonController::GetAutonomyTaskStatus called, user_id={user_id:032X}");
        (RESULT_SUCCESS, 0)
    }

    fn get_autonomy_task_status_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        // Registered only on IDaemonController, matching the other OLSC bridges.
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let user_id = RequestParser::new(ctx).pop_raw::<u128>();
        let (result, status) = service.get_autonomy_task_status(user_id);
        // Two result words plus one word containing the u8 and zero padding.
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_u8(status);
    }

    fn stub_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let cmd = ctx.get_command();
        log::warn!("(STUBBED) IDaemonController command {}", cmd);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Cmd 0: GetAutoTransferEnabledForAccountAndApplication
    ///
    /// Returns whether auto-transfer is enabled for the given user and application.
    /// Upstream always returns false (stubbed).
    pub fn get_auto_transfer_enabled_for_account_and_application(
        &self,
        user_id: u128,
        application_id: u64,
    ) -> (ResultCode, bool) {
        log::warn!(
            "(STUBBED) IDaemonController::get_auto_transfer_enabled_for_account_and_application called, user_id={:032X}, application_id={:016X}",
            user_id,
            application_id
        );
        (RESULT_SUCCESS, false)
    }
}

impl SessionRequestHandler for IDaemonController {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "IDaemonController"
    }
}

impl ServiceFramework for IDaemonController {
    fn get_service_name(&self) -> &str {
        "IDaemonController"
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
    fn autonomy_task_status_reply_includes_idle_byte_and_zero_padding() {
        let service = IDaemonController::new();
        for user_id in [0, 1, u128::MAX] {
            assert_eq!(service.get_autonomy_task_status(user_id), (RESULT_SUCCESS, 0));
            let mut ctx = HLERequestContext::new();
            ctx.command_buffer_mut().fill(u32::MAX);
            for (word, bytes) in ctx.command_buffer_mut()[2..6]
                .iter_mut().zip(user_id.to_le_bytes().chunks_exact(4))
            {
                *word = u32::from_le_bytes(bytes.try_into().unwrap());
            }
            let handler = service.handlers().get(&12).unwrap();
            assert_eq!(handler.name, "GetAutonomyTaskStatus");
            handler.handler_callback.unwrap()(&service, &mut ctx);
            let offset = ctx.get_data_payload_offset() as usize;
            assert_eq!(&ctx.command_buffer()[offset..offset + 3], &[0, 0, 0]);
            assert_eq!(ctx.write_size, (offset + 3) as u32);
            // Matches Eden ResponseBuilder's raw-data-size accounting:
            // initial parameter count + CMIF header + alignment + parameters.
            assert_eq!(ctx.command_buffer()[1] & 0x3ff, 3 + 2 + 4 + 3);
        }
    }
}
