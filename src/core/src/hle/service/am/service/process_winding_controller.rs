// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/am/service/process_winding_controller.h
//! Port of zuyu/src/core/hle/service/am/service/process_winding_controller.cpp

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::hle::result::{ResultCode, RESULT_SUCCESS, RESULT_UNKNOWN};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use super::{storage::IStorage, library_applet_accessor::ILibraryAppletAccessor};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command table for IProcessWindingController:
/// - 0: GetLaunchReason
/// - 11: OpenCallingLibraryApplet
pub struct IProcessWindingController {
    applet: Arc<Mutex<crate::hle::service::am::applet::Applet>>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IProcessWindingController {
    pub fn new(applet: Arc<Mutex<crate::hle::service::am::applet::Applet>>) -> Self {
        let handlers = build_handler_map(&[
            (0, Some(Self::get_launch_reason_handler), "GetLaunchReason"),
            (11, Some(Self::open_calling_library_applet), "OpenCallingLibraryApplet"),
            (21, Some(Self::push_context), "PushContext"),
            (22, Some(Self::pop_context), "PopContext"),
            (23, Some(Self::cancel_winding_reservation), "CancelWindingReservation"),
            (30, Some(Self::wind_and_do_reserved), "WindAndDoReserved"),
            (40, Some(Self::reserve_to_start_and_wait_and_unwind_this), "ReserveToStartAndWaitAndUnwindThis"),
            (41, Some(Self::reserve_to_start_and_wait), "ReserveToStartAndWait"),
        ]);
        Self {
            applet,
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn service(this: &dyn ServiceFramework) -> &Self {
        unsafe { &*(this as *const dyn ServiceFramework as *const Self) }
    }

    // Domain input interface decoding, as in the sibling AM services. Retain
    // the actual handler, not a copy of IStorage's bytes, across Push/Pop.
    fn input_object(ctx: &HLERequestContext) -> Result<Option<Arc<dyn SessionRequestHandler>>, ResultCode> {
        let id = RequestParser::new(ctx).pop_u32();
        if id == 0 { return Ok(None); }
        let manager = ctx.get_manager().ok_or(RESULT_UNKNOWN)?;
        let manager = manager.lock().unwrap();
        if !manager.is_domain() || id as usize > manager.domain_handler_count() {
            return Err(RESULT_UNKNOWN);
        }
        manager.domain_handler(id as usize - 1).cloned().map(Some).ok_or(RESULT_UNKNOWN)
    }

    // Preserve the fixed Out<SharedPointer<T>> slot even for a null/error reply.
    fn output_object(ctx: &mut HLERequestContext, result: Result<Option<Arc<dyn SessionRequestHandler>>, ResultCode>) {
        let domain = ctx.get_manager().is_some_and(|m| m.lock().unwrap().is_domain());
        let (code, object) = match result {
            Ok(object) => (RESULT_SUCCESS, object),
            Err(code) => (code, None),
        };
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(code);
        if let Some(object) = object {
            rb.push_ipc_interface(object);
        } else if domain {
            drop(rb);
            ctx.add_null_domain_object();
        } else {
            drop(rb);
            ctx.add_move_handle(0);
        }
    }

    fn open_calling_library_applet(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::service(this);
        let (reserved, caller, caller_broker) = {
            let mut a = service.applet.lock().unwrap();
            (a.reserved_applet.take(), a.caller_applet.upgrade(), a.caller_applet_broker.clone())
        };
        let target = if let Some(reserved) = reserved {
            let broker = reserved.lock().unwrap().caller_applet_broker.clone();
            broker.map(|broker| (reserved, broker))
        } else {
            caller.zip(caller_broker)
        };
        let result = target.map(|(applet, broker)| {
            Some(Arc::new(ILibraryAppletAccessor::new(
                ctx.get_system().unwrap_or(crate::core::SystemRef::null()), broker, applet,
            )) as Arc<dyn SessionRequestHandler>)
        }).ok_or(RESULT_UNKNOWN);
        Self::output_object(ctx, result);
    }

    fn push_context(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let result = Self::input_object(ctx).and_then(|object| {
            if object.as_ref().is_some_and(|o| !o.as_any().is::<IStorage>()) {
                return Err(RESULT_UNKNOWN);
            }
            Self::service(this).applet.lock().unwrap().context_stack.push(object);
            Ok(())
        });
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result.err().unwrap_or(RESULT_SUCCESS));
    }

    fn pop_context(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let result = Self::service(this).applet.lock().unwrap().context_stack.pop().ok_or(RESULT_UNKNOWN);
        Self::output_object(ctx, result);
    }

    fn cancel_winding_reservation(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut a = Self::service(this).applet.lock().unwrap();
        a.reserved_applet = None;
        a.unwind_after_reserved = false;
        drop(a);
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }

    fn reserve_to_start_and_wait_and_unwind_this(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        Self::reserve(this, ctx, true);
    }

    fn reserve_to_start_and_wait(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        Self::reserve(this, ctx, false);
    }

    // Mechanical common body of the two upstream reservation methods.
    fn reserve(this: &dyn ServiceFramework, ctx: &mut HLERequestContext, unwind: bool) {
        let result = Self::input_object(ctx).and_then(|object| {
            let object = object.ok_or(RESULT_UNKNOWN)?;
            let accessor = object.as_any().downcast_ref::<ILibraryAppletAccessor>().ok_or(RESULT_UNKNOWN)?;
            let mut a = Self::service(this).applet.lock().unwrap();
            a.reserved_applet = Some(accessor.get_applet());
            a.unwind_after_reserved = unwind;
            Ok(())
        });
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result.err().unwrap_or(RESULT_SUCCESS));
    }

    fn wind_and_do_reserved(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::service(this);
        let reserved = {
            let mut a = service.applet.lock().unwrap();
            let reserved = a.reserved_applet.clone();
            a.display_layer_manager.set_window_visibility(false);
            a.exit_locked = false;
            if let Some(system) = ctx.get_system() { system.get().set_exit_locked(false); }
            reserved
        };
        if let Some(reserved) = reserved {
            service.applet.lock().unwrap().is_winding = true;
            let mut frontend = {
                let mut a = reserved.lock().unwrap();
                a.window_visible = true;
                a.process.run();
                a.frontend.take()
            };
            // Match the frontend-completion adaptation in LibraryAppletAccessor.
            // Invoke callbacks without the Applet guard, as upstream does.
            let complete = if let Some(frontend) = frontend.as_mut() {
                frontend.initialize();
                frontend.execute();
                frontend.is_complete()
            } else { false };
            let mut a = reserved.lock().unwrap();
            a.frontend = frontend;
            if complete {
                a.is_completed = true;
                a.signal_state_changed_event_without_process();
            }
        } else {
            log::warn!("WindAndDoReserved without a reserved applet");
        }
        service.applet.lock().unwrap().process.terminate();
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }

    fn get_launch_reason_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IProcessWindingController) };
        let applet = service.applet.lock().unwrap();
        let launch_reason = applet.launch_reason;
        drop(applet);

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        // AppletProcessLaunchReason is a repr(C) u32-sized struct; push as raw u32
        rb.push_u32(unsafe { std::mem::transmute::<_, u32>(launch_reason) });
    }
}

impl SessionRequestHandler for IProcessWindingController {
    fn handle_sync_request(&self, context: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, context)
    }
}

impl ServiceFramework for IProcessWindingController {
    fn get_service_name(&self) -> &str {
        "am::IProcessWindingController"
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
    use crate::core::SystemRef;
    use crate::hle::service::am::{applet::Applet, applet_data_broker::AppletDataBroker};
    use crate::hle::service::hle_ipc::SessionRequestManager;
    use crate::hle::service::os::process::Process;

    fn applet() -> Arc<Mutex<Applet>> {
        Arc::new(Mutex::new(Applet::new(SystemRef::null(), Process::new(), false)))
    }

    fn request(service: &IProcessWindingController, command: u32, object: u32,
        manager: &Arc<Mutex<SessionRequestManager>>) -> HLERequestContext {
        let mut ctx = HLERequestContext::new();
        ctx.set_session_request_manager(manager.clone());
        ctx.cmd_buf[2] = object;
        service.handlers[&command].handler_callback.unwrap()(service, &mut ctx);
        ctx.write_to_outgoing_command_buffer();
        ctx
    }

    fn output(ctx: &HLERequestContext, manager: &Arc<Mutex<SessionRequestManager>>) -> Option<Arc<dyn SessionRequestHandler>> {
        let id = ctx.cmd_buf[ctx.domain_offset as usize - 1];
        if id == 0 { None } else { manager.lock().unwrap().domain_handler(id as usize - 1).cloned() }
    }

    fn result(ctx: &HLERequestContext) -> u32 { ctx.cmd_buf[ctx.data_payload_offset as usize] }

    fn manager() -> Arc<Mutex<SessionRequestManager>> {
        let mut manager = SessionRequestManager::new();
        manager.convert_to_domain();
        Arc::new(Mutex::new(manager))
    }

    #[test]
    fn context_stack_preserves_identity_order_and_null_objects() {
        let service = IProcessWindingController::new(applet());
        let manager = manager();
        let first: Arc<dyn SessionRequestHandler> = Arc::new(IStorage::new(vec![1]));
        let second: Arc<dyn SessionRequestHandler> = Arc::new(IStorage::new(vec![2]));
        manager.lock().unwrap().append_domain_handler(first.clone());
        manager.lock().unwrap().append_domain_handler(second.clone());
        for id in [2, 3, 0] {
            assert_eq!(result(&request(&service, 21, id, &manager)), 0);
        }
        manager.lock().unwrap().close_domain_handler(1);
        manager.lock().unwrap().close_domain_handler(2);
        let null = request(&service, 22, 0, &manager);
        assert_eq!(result(&null), 0);
        assert!(output(&null, &manager).is_none());
        for expected in [second, first] {
            let ctx = request(&service, 22, 0, &manager);
            assert_eq!(result(&ctx), 0);
            assert!(Arc::ptr_eq(&output(&ctx, &manager).unwrap(), &expected));
        }
        let empty = request(&service, 22, 0, &manager);
        assert_eq!(result(&empty), RESULT_UNKNOWN.get_inner_value());
        assert!(output(&empty, &manager).is_none());
        assert_eq!(result(&request(&service, 21, 99, &manager)), RESULT_UNKNOWN.get_inner_value());
    }

    #[test]
    fn winding_executes_frontend_outside_applet_guard_and_marks_completion() {
        use crate::hle::service::am::frontend::applets::FrontendApplet;
        use crate::hle::service::am::am_types::LibraryAppletMode;
        struct Frontend {
            applet: std::sync::Weak<Mutex<Applet>>,
            calls: Arc<Mutex<Vec<u8>>>,
        }
        impl FrontendApplet for Frontend {
            fn initialize(&mut self) { self.calls.lock().unwrap().push(0); }
            fn execute(&mut self) {
                assert!(self.applet.upgrade().unwrap().try_lock().is_ok());
                self.calls.lock().unwrap().push(1);
            }
            fn execute_interactive(&mut self) {}
            fn request_exit(&mut self) {}
            fn get_status(&self) -> ResultCode { RESULT_SUCCESS }
            fn get_library_applet_mode(&self) -> LibraryAppletMode { LibraryAppletMode::AllForeground }
            fn is_initialized(&self) -> bool { true }
            fn is_complete(&self) -> bool { true }
        }
        let current = applet();
        let child = applet();
        let calls = Arc::new(Mutex::new(Vec::new()));
        child.lock().unwrap().frontend = Some(Box::new(Frontend {
            applet: Arc::downgrade(&child), calls: calls.clone(),
        }));
        child.lock().unwrap().window_visible = false;
        current.lock().unwrap().reserved_applet = Some(child.clone());
        current.lock().unwrap().exit_locked = true;
        let service = IProcessWindingController::new(current.clone());
        assert_eq!(result(&request(&service, 30, 0, &manager())), 0);
        assert_eq!(*calls.lock().unwrap(), vec![0, 1]);
        assert!(current.lock().unwrap().is_winding);
        assert!(!current.lock().unwrap().exit_locked);
        assert!(child.lock().unwrap().window_visible);
        assert!(child.lock().unwrap().is_completed);
    }

    #[test]
    fn reservations_are_consumed_and_cancellation_clears_unwind() {
        let current = applet();
        let caller = applet();
        let child = applet();
        let broker = Arc::new(AppletDataBroker::new());
        current.lock().unwrap().caller_applet = Arc::downgrade(&caller);
        current.lock().unwrap().caller_applet_broker = Some(broker.clone());
        child.lock().unwrap().caller_applet_broker = Some(broker.clone());
        let accessor = Arc::new(ILibraryAppletAccessor::new(SystemRef::null(), broker, child.clone()));
        let service = IProcessWindingController::new(current.clone());
        let manager = manager();
        manager.lock().unwrap().append_domain_handler(accessor);
        for (command, unwind) in [(40, true), (41, false)] {
            assert_eq!(result(&request(&service, command, 2, &manager)), 0);
            assert_eq!(current.lock().unwrap().unwind_after_reserved, unwind);
            let opened = request(&service, 11, 0, &manager);
            let target = output(&opened, &manager).unwrap()
                .as_any().downcast_ref::<ILibraryAppletAccessor>().unwrap().get_applet();
            assert!(Arc::ptr_eq(&target, &child));
            assert!(current.lock().unwrap().reserved_applet.is_none());
        }
        let opened = request(&service, 11, 0, &manager);
        let target = output(&opened, &manager).unwrap()
            .as_any().downcast_ref::<ILibraryAppletAccessor>().unwrap().get_applet();
        assert!(Arc::ptr_eq(&target, &caller));
        request(&service, 40, 2, &manager);
        request(&service, 23, 0, &manager);
        assert!(!current.lock().unwrap().unwind_after_reserved);
        assert!(current.lock().unwrap().reserved_applet.is_none());
        assert_eq!(result(&request(&service, 40, 0, &manager)), RESULT_UNKNOWN.get_inner_value());
        current.lock().unwrap().caller_applet = std::sync::Weak::new();
        assert_eq!(result(&request(&service, 11, 0, &manager)), RESULT_UNKNOWN.get_inner_value());
    }
}
