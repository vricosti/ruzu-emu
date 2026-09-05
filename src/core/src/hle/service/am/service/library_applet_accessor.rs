// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/am/service/library_applet_accessor.h
//! Port of zuyu/src/core/hle/service/am/service/library_applet_accessor.cpp

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::core::SystemRef;
use crate::hle::result::{ResultCode, RESULT_SUCCESS, RESULT_UNKNOWN};
use crate::hle::service::am::applet_data_broker::AppletDataBroker;
use crate::hle::service::am::service::storage::IStorage;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command table for ILibraryAppletAccessor:
/// - 0: GetAppletStateChangedEvent
/// - 1: IsCompleted
/// - 10: Start
/// - 20: RequestExit
/// - 25: Terminate
/// - 30: GetResult
/// - 50: SetOutOfFocusApplicationSuspendingEnabled (unimplemented)
/// - 60: PresetLibraryAppletGpuTimeSliceZero
/// - 100: PushInData
/// - 101: PopOutData
/// - 102: PushExtraStorage (unimplemented)
/// - 103: PushInteractiveInData
/// - 104: PopInteractiveOutData
/// - 105: GetPopOutDataEvent
/// - 106: GetPopInteractiveOutDataEvent
/// - 110: NeedsToExitProcess (unimplemented)
/// - 120: GetLibraryAppletInfo (unimplemented)
/// - 150: RequestForAppletToGetForeground (unimplemented)
/// - 160: GetIndirectLayerConsumerHandle
pub struct ILibraryAppletAccessor {
    system: SystemRef,
    applet: Arc<Mutex<crate::hle::service::am::applet::Applet>>,
    /// Matches upstream `const std::shared_ptr<AppletDataBroker> m_broker`.
    broker: Arc<AppletDataBroker>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ILibraryAppletAccessor {
    pub fn new(
        system: SystemRef,
        broker: Arc<AppletDataBroker>,
        applet: Arc<Mutex<crate::hle::service::am::applet::Applet>>,
    ) -> Self {
        let handlers = build_handler_map(&[
            (
                0,
                Some(Self::get_applet_state_changed_event_handler),
                "GetAppletStateChangedEvent",
            ),
            (1, Some(Self::is_completed_handler), "IsCompleted"),
            (10, Some(Self::start_handler), "Start"),
            (20, Some(Self::request_exit_handler), "RequestExit"),
            (25, Some(Self::terminate_handler), "Terminate"),
            (30, Some(Self::get_result_handler), "GetResult"),
            (50, None, "SetOutOfFocusApplicationSuspendingEnabled"),
            (
                60,
                Some(Self::preset_library_applet_gpu_time_slice_zero_handler),
                "PresetLibraryAppletGpuTimeSliceZero",
            ),
            (100, Some(Self::push_in_data_handler), "PushInData"),
            (101, Some(Self::pop_out_data_handler), "PopOutData"),
            (102, None, "PushExtraStorage"),
            (
                103,
                Some(Self::push_interactive_in_data_handler),
                "PushInteractiveInData",
            ),
            (
                104,
                Some(Self::pop_interactive_out_data_handler),
                "PopInteractiveOutData",
            ),
            (
                105,
                Some(Self::get_pop_out_data_event_handler),
                "GetPopOutDataEvent",
            ),
            (
                106,
                Some(Self::get_pop_interactive_out_data_event_handler),
                "GetPopInteractiveOutDataEvent",
            ),
            (110, None, "NeedsToExitProcess"),
            (120, None, "GetLibraryAppletInfo"),
            (150, None, "RequestForAppletToGetForeground"),
            (
                160,
                Some(Self::get_indirect_layer_consumer_handle_handler),
                "GetIndirectLayerConsumerHandle",
            ),
        ]);
        Self {
            system,
            applet,
            broker,
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn get_applet_state_changed_event_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletAccessor) };
        log::debug!("ILibraryAppletAccessor::GetAppletStateChangedEvent called");
        let handle = service
            .applet
            .lock()
            .unwrap()
            .ensure_state_changed_event_object_id(ctx)
            .unwrap_or(0);

        if std::env::var_os("RUZU_TRACE_APPLET_RETURN").is_some() {
            let applet = service.applet.lock().unwrap();
            log::info!(
                "[APPLET_RETURN] GetAppletStateChangedEvent aruid={} object_id={}",
                applet.aruid.pid,
                handle
            );
        }

        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(handle);
    }

    fn is_completed_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletAccessor) };
        log::debug!("ILibraryAppletAccessor::IsCompleted called");
        let is_completed = service.applet.lock().unwrap().is_completed;

        if std::env::var_os("RUZU_TRACE_APPLET_RETURN").is_some() {
            let aruid = service.applet.lock().unwrap().aruid.pid;
            log::info!(
                "[APPLET_RETURN] IsCompleted aruid={} completed={}",
                aruid,
                is_completed
            );
        }

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(is_completed);
    }

    fn get_result_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletAccessor) };
        log::debug!("ILibraryAppletAccessor::GetResult called");
        let result = ResultCode::new(service.applet.lock().unwrap().terminate_result);

        if std::env::var_os("RUZU_TRACE_APPLET_RETURN").is_some() {
            let aruid = service.applet.lock().unwrap().aruid.pid;
            log::info!(
                "[APPLET_RETURN] GetResult aruid={} result=0x{:X}",
                aruid,
                result.get_inner_value()
            );
        }

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    fn preset_library_applet_gpu_time_slice_zero_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::info!("(STUBBED) ILibraryAppletAccessor::PresetLibraryAppletGpuTimeSliceZero called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn start_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletAccessor) };
        log::debug!("ILibraryAppletAccessor::Start called");
        {
            let mut applet = service.applet.lock().unwrap();
            applet.process.run();
        }
        service.frontend_execute();

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn request_exit_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletAccessor) };
        log::debug!("ILibraryAppletAccessor::RequestExit called");
        {
            let applet = service.applet.lock().unwrap();
            applet.lifecycle_manager.request_exit();
        }
        service.frontend_request_exit();

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn terminate_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletAccessor) };
        log::debug!("ILibraryAppletAccessor::Terminate called");
        {
            let mut applet = service.applet.lock().unwrap();
            applet.process.terminate();
        }
        service.frontend_request_exit();

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_indirect_layer_consumer_handle_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) ILibraryAppletAccessor::GetIndirectLayerConsumerHandle called");
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(0xdeadbeef);
    }

    fn pop_domain_storage(ctx: &mut HLERequestContext) -> Option<Vec<u8>> {
        let mut rp = RequestParser::new(ctx);
        let object_id = rp.pop_u32();
        if object_id == 0 {
            log::error!("ILibraryAppletAccessor storage argument is null");
            return None;
        }

        let handler = {
            let manager = ctx.get_manager()?;
            let manager = manager.lock().unwrap();
            if !manager.is_domain() {
                log::error!("ILibraryAppletAccessor storage argument requires domain IPC");
                return None;
            }
            manager.domain_handler(object_id as usize - 1)?.clone()
        };

        let storage = handler.as_any().downcast_ref::<IStorage>()?;
        Some(storage.get_data())
    }

    fn push_in_data_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletAccessor) };
        log::debug!("ILibraryAppletAccessor::PushInData called");

        let Some(data) = Self::pop_domain_storage(ctx) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };

        service.broker.get_in_data().push(data);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn pop_out_data_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletAccessor) };
        log::debug!("ILibraryAppletAccessor::PopOutData called");

        let caller_applet = service.applet.lock().unwrap().caller_applet.upgrade();
        if let Some(caller_applet) = caller_applet {
            let mut caller_applet = caller_applet.lock().unwrap();
            caller_applet.lifecycle_manager.get_system_event().signal();
            caller_applet
                .lifecycle_manager
                .request_resume_notification();
            caller_applet.lifecycle_manager.get_system_event().clear();
            caller_applet
                .lifecycle_manager
                .update_requested_focus_state();

            if std::env::var_os("RUZU_TRACE_APPLET_RETURN").is_some() {
                log::info!(
                    "[APPLET_RETURN] PopOutData resumed caller aruid={}",
                    caller_applet.aruid.pid
                );
            }
        }

        match service.broker.get_out_data().pop() {
            Ok(data) => {
                if std::env::var_os("RUZU_TRACE_APPLET_RETURN").is_some() {
                    log::info!("[APPLET_RETURN] PopOutData size={}", data.len());
                }
                let storage = Arc::new(IStorage::new_with_system(service.system, data));
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
                rb.push_result(RESULT_SUCCESS);
                rb.push_ipc_interface(storage);
            }
            Err(result) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(result);
            }
        }
    }

    fn push_interactive_in_data_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletAccessor) };
        log::debug!("ILibraryAppletAccessor::PushInteractiveInData called");

        let Some(data) = Self::pop_domain_storage(ctx) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };

        service.broker.get_interactive_in_data().push(data);
        service.frontend_execute_interactive();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn pop_interactive_out_data_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletAccessor) };
        log::debug!("ILibraryAppletAccessor::PopInteractiveOutData called");

        match service.broker.get_interactive_out_data().pop() {
            Ok(data) => {
                let storage = Arc::new(IStorage::new_with_system(service.system, data));
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
                rb.push_result(RESULT_SUCCESS);
                rb.push_ipc_interface(storage);
            }
            Err(result) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(result);
            }
        }
    }

    fn get_pop_out_data_event_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletAccessor) };
        log::debug!("ILibraryAppletAccessor::GetPopOutDataEvent called");
        let object_id = service
            .broker
            .get_out_data()
            .get_event_object_id(ctx)
            .unwrap_or(0);

        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object_id);
    }

    fn get_pop_interactive_out_data_event_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletAccessor) };
        log::debug!("ILibraryAppletAccessor::GetPopInteractiveOutDataEvent called");
        let object_id = service
            .broker
            .get_interactive_out_data()
            .get_event_object_id(ctx)
            .unwrap_or(0);

        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object_id);
    }

    fn frontend_execute(&self) {
        let mut applet = self.applet.lock().unwrap();
        let complete = if let Some(frontend) = applet.frontend.as_mut() {
            frontend.initialize();
            frontend.execute();
            frontend.is_complete()
        } else {
            false
        };
        if complete {
            applet.is_completed = true;
            applet.signal_state_changed_event_without_process();
        }
    }

    fn frontend_execute_interactive(&self) {
        let mut applet = self.applet.lock().unwrap();
        let complete = if let Some(frontend) = applet.frontend.as_mut() {
            frontend.execute_interactive();
            frontend.execute();
            frontend.is_complete()
        } else {
            false
        };
        if complete {
            applet.is_completed = true;
            applet.signal_state_changed_event_without_process();
        }
    }

    fn frontend_request_exit(&self) {
        let mut applet = self.applet.lock().unwrap();
        if let Some(frontend) = applet.frontend.as_mut() {
            frontend.request_exit();
        }
    }
}

impl SessionRequestHandler for ILibraryAppletAccessor {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }
}

impl ServiceFramework for ILibraryAppletAccessor {
    fn get_service_name(&self) -> &str {
        "am::ILibraryAppletAccessor"
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
    use crate::hle::service::am::applet::Applet;
    use crate::hle::service::os::process::Process;

    #[test]
    fn frontend_start_does_not_fake_a_running_guest_process() {
        let applet = Arc::new(Mutex::new(Applet::new(
            SystemRef::null(),
            Process::new(),
            false,
        )));
        let broker = Arc::new(AppletDataBroker::new());
        let accessor = ILibraryAppletAccessor::new(SystemRef::null(), broker, Arc::clone(&applet));
        let mut ctx = HLERequestContext::new();

        ILibraryAppletAccessor::start_handler(&accessor, &mut ctx);

        assert!(!applet.lock().unwrap().is_process_running);
    }
}
