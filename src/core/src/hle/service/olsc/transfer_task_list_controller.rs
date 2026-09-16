// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/olsc/transfer_task_list_controller.h
//! Port of zuyu/src/core/hle/service/olsc/transfer_task_list_controller.cpp
//!
//! ITransferTaskListController: manages transfer task lists.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

use super::native_handle_holder::INativeHandleHolder;
use super::stopper_object::IStopperObject;

/// ITransferTaskListController.
///
/// Corresponds to `ITransferTaskListController` in upstream transfer_task_list_controller.cpp.
pub struct ITransferTaskListController {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ITransferTaskListController {
    pub fn new() -> Self {
        let handlers = build_handler_map(&[
            (0, None, "GetTransferTaskCountForOcean"),
            (1, None, "GetTransferTaskInfoForOcean"),
            (2, None, "ListTransferTaskInfoForOcean"),
            (3, None, "DeleteTransferTaskForOcean"),
            (4, None, "RaiseTransferTaskPriorityForOcean"),
            (5, Some(Self::get_transfer_task_end_event_native_handle_holder), "GetTransferTaskEndEventNativeHandleHolder"),
            (6, None, "GetTransferTaskProgressForOcean"),
            (7, None, "GetTransferTaskLastResultForOcean"),
            (8, Some(Self::stop_next_transfer_task_execution), "StopNextTransferTaskExecution"),
            (9, Some(Self::get_transfer_task_start_event_native_handle_holder), "GetTransferTaskStartEventNativeHandleHolder"),
            (10, None, "SuspendTransferTaskForOcean"),
            (11, None, "GetCurrentTransferTaskInfoForOcean"),
            (12, None, "FindTransferTaskInfoForOcean"),
            (13, None, "CancelCurrentRepairTransferTask"),
            (14, None, "GetRepairTransferTaskProgress"),
            (15, None, "EnsureExecutableForRepairTransferTask"),
            (16, Some(Self::get_transfer_task_count), "GetTransferTaskCount"),
            (17, None, "GetTransferTaskInfo"),
            (18, None, "ListTransferTaskInfo"),
            (19, None, "DeleteTransferTask"),
            (20, None, "RaiseTransferTaskPriority"),
            (21, Some(Self::get_transfer_task_progress_handler), "GetTransferTaskProgress"),
            (22, None, "GetTransferTaskLastResult"),
            (23, None, "SuspendTransferTask"),
            (24, Some(Self::get_current_transfer_task_info), "GetCurrentTransferTaskInfo"),
            (25, Some(Self::find_transfer_task_info), "FindTransferTaskInfo"),
            (26, None, "Unknown26"),
            (27, None, "Unknown27"),
            (28, None, "Unknown28"),
            (29, None, "Unknown29"),
            (30, None, "Unknown30"),
        ]);
        Self {
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn get_transfer_task_end_event_native_handle_holder(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let holder = Arc::new(INativeHandleHolder::new());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(holder);
    }

    fn get_transfer_task_start_event_native_handle_holder(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let holder = Arc::new(INativeHandleHolder::new());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(holder);
    }

    fn stop_next_transfer_task_execution(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let stopper = Arc::new(IStopperObject::default());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(stopper);
    }

    fn get_transfer_task_count(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let _unknown = RequestParser::new(ctx).pop_u8();
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(0);
    }

    fn get_current_transfer_task_info(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let _unknown = RequestParser::new(ctx).pop_u8();
        let mut rb = ResponseBuilder::new(ctx, 2 + 0x30 / 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_raw_bytes(&[0; 0x30]);
    }

    fn find_transfer_task_info(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let _input = ctx.read_buffer(0);
        let mut rb = ResponseBuilder::new(ctx, 2 + 0x30 / 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_raw_bytes(&[0; 0x30]);
    }

    /// Upstream deliberately returns success without a progress payload.
    fn get_transfer_task_progress_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) GetTransferTaskProgress called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

}

impl SessionRequestHandler for ITransferTaskListController {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "ITransferTaskListController"
    }
}

impl ServiceFramework for ITransferTaskListController {
    fn get_service_name(&self) -> &str {
        "ITransferTaskListController"
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
    fn event_holder_and_stopper_commands_return_real_child_sessions() {
        use crate::hle::kernel::k_process::{KProcess, ProcessLock};
        use crate::hle::kernel::k_thread::{KThread, KThreadLock};
        use crate::hle::service::hle_ipc::SessionRequestManager;
        use std::sync::Mutex;

        let service = ITransferTaskListController::new();
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));
        for (command, expected_name) in [(5, "INativeHandleHolder"), (9, "INativeHandleHolder"), (8, "IStopperObject")] {
            let mut ctx = HLERequestContext::new_with_thread(thread.clone(), 0);
            ctx.set_session_request_manager(Arc::new(Mutex::new(SessionRequestManager::new())));
            service.handlers()[&command].handler_callback.unwrap()(&service, &mut ctx);
            assert_eq!(ctx.outgoing_move_objects.len(), 1);
            let object_id = match ctx.outgoing_move_objects[0] {
                crate::hle::service::hle_ipc::KAutoObjectRef::ObjectId(id) => id,
                _ => panic!("expected an object-backed child session"),
            };
            let server = process.lock().unwrap().get_server_session_by_object_id(object_id).unwrap();
            let manager = server.lock().unwrap().get_manager().unwrap().clone();
            let manager = manager.lock().unwrap();
            let child = manager.session_handler().unwrap();
            assert_eq!(child.service_name(), expected_name);
            if command != 8 {
                let holder = child.as_any().downcast_ref::<INativeHandleHolder>().unwrap();
                assert!(holder.get_native_handle().unwrap().is_signaled());
            }
        }
    }

    #[test]
    fn transfer_count_and_info_have_complete_zero_payloads() {
        let service = ITransferTaskListController::new();
        for (command, words) in [(16, 3), (24, 14), (25, 14)] {
            let mut ctx = HLERequestContext::new();
            ctx.command_buffer_mut().fill(u32::MAX);
            service.handlers()[&command].handler_callback.unwrap()(&service, &mut ctx);
            let offset = ctx.get_data_payload_offset() as usize;
            assert_eq!(ctx.write_size as usize, offset + words);
            assert!(ctx.command_buffer()[offset..offset + words].iter().all(|word| *word == 0));
        }
        assert_eq!(service.handlers().len(), 31);
        let implemented: Vec<_> = service.handlers().iter()
            .filter_map(|(&id, info)| info.handler_callback.is_some().then_some(id)).collect();
        assert_eq!(implemented, [5, 8, 9, 16, 21, 24, 25]);
    }

    #[test]
    fn transfer_task_progress_returns_only_success() {
        let service = ITransferTaskListController::new();
        let mut ctx = HLERequestContext::new();
        let function = &service.handlers()[&21];
        assert_eq!(function.name, "GetTransferTaskProgress");
        function.handler_callback.unwrap()(&service, &mut ctx);
        assert_eq!(ctx.command_buffer()[6], 0);
        assert!(ctx.outgoing_copy_objects.is_empty());
    }
}
