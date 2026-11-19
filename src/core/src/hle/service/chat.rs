// SPDX-License-Identifier: GPL-3.0-or-later
//! SDK-derived Switch 2 chat service, authorized separately from the Eden port.
//!
//! No Eden-NX2 counterpart exists. Evidence: Mario Kart World 1.8.0 SDK
//! InitializeUserService (+0x6bc330), CreateUserServiceByHipc (+0x6bc7d0),
//! IUserService proxy (+0x6bcda0), IUserShimScopedObject proxy (+0x1d1ff0).
//! Offsets are module-relative, verified against the captured SDK symbol table.
//! This implements acquisition and notification ownership for an inactive chat
//! session. Network chat, camera and unknown commands are not implemented.
//! An initially unsignaled event models no chat transition; this is an explicit
//! local offline policy, not a claim about the original server's initial state.

pub mod gsv;

use std::collections::BTreeMap;
use std::sync::Arc;

use super::hle_ipc::{HLERequestContext, SessionRequestHandler};
use super::ipc_helpers::{ResponseBuilder, RESULT_NOT_SUPPORTED};
use super::kernel_helpers::ServiceContext;
use super::os::event::Event;
use super::service::{build_handler_map, FunctionInfo, ServiceFramework};
use crate::core::SystemRef;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};

struct ChatState {
    chatting_event: Arc<Event>,
    notification_event: Arc<Event>,
    // Retained by both the root and all returned objects, including after the
    // client closes its root session. ServiceContext owns event resources.
    _context: ServiceContext,
}

impl ChatState {
    fn new() -> Self {
        let mut context = ServiceContext::new("chat:u".to_owned());
        let event = context.create_event("ChattingEvent".to_owned());
        // SDK IUserShimScopedObject vtable +0x38 -> +0x1d13c0:
        // command 2 uses the same copied-event ABI as command 1.
        let notification = context.create_event("ChatNotification2".to_owned());
        Self {
            chatting_event: context.get_event(event).expect("chat event allocation"),
            notification_event: context
                .get_event(notification)
                .expect("chat notification allocation"),
            _context: context,
        }
    }
}

pub struct IUserService {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
    state: Arc<ChatState>,
}

impl IUserService {
    fn new() -> Self {
        Self {
            handlers: build_handler_map(&[(
                0,
                Some(Self::create_user_shim_scoped_object),
                "CreateUserShimScopedObject",
            )]),
            handlers_tipc: BTreeMap::new(),
            state: Arc::new(ChatState::new()),
        }
    }

    fn create_user_shim_scoped_object(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = this.as_any().downcast_ref::<Self>().unwrap();
        log::info!("chat:u: create user shim scoped object");
        let child = Arc::new(IUserShimScopedObject::new(this.state.clone()));
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(child);
    }
}

struct IUserShimScopedObject {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
    state: Arc<ChatState>,
}

impl IUserShimScopedObject {
    fn new(state: Arc<ChatState>) -> Self {
        Self {
            handlers: build_handler_map(&[
                (1, Some(Self::get_chatting_event), "GetChattingEvent"),
                (
                    2,
                    Some(Self::get_notification_event),
                    "GetNotificationEvent2",
                ),
            ]),
            handlers_tipc: BTreeMap::new(),
            state,
        }
    }

    fn get_chatting_event(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = this.as_any().downcast_ref::<Self>().unwrap();
        let object = this
            .state
            .chatting_event
            .copy_object_id(ctx)
            .expect("chat event requires a live request owner");
        log::info!("chat:u: get chatting event, inactive conversation");
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object);
    }

    fn get_notification_event(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = this.as_any().downcast_ref::<Self>().unwrap();
        let object = this
            .state
            .notification_event
            .copy_object_id(ctx)
            .expect("chat notification requires request owner");
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object);
    }
}

// Do not inherit the NX2 success fallback: an unimplemented chat command may
// require objects or handles, and an empty success would corrupt the client.
fn unsupported(ctx: &mut HLERequestContext, name: &str) {
    log::error!("Unsupported chat command {}::{}", name, ctx.get_command());
    let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
    rb.push_result(RESULT_NOT_SUPPORTED);
}

impl SessionRequestHandler for IUserService {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        self.handle_sync_request_impl(ctx)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl ServiceFramework for IUserService {
    fn get_service_name(&self) -> &str {
        "chat:u"
    }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
    fn report_unimplemented_function(&self, ctx: &mut HLERequestContext, _: Option<&FunctionInfo>) {
        unsupported(ctx, self.get_service_name());
    }
}

impl SessionRequestHandler for IUserShimScopedObject {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        self.handle_sync_request_impl(ctx)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl ServiceFramework for IUserShimScopedObject {
    fn get_service_name(&self) -> &str {
        "chat:IUserShimScopedObject"
    }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
    fn report_unimplemented_function(&self, ctx: &mut HLERequestContext, _: Option<&FunctionInfo>) {
        unsupported(ctx, self.get_service_name());
    }
}

pub fn loop_process(system: SystemRef) {
    use super::server_manager::ServerManager;
    let server = ServerManager::new_shared(system);
    server.lock().unwrap().register_named_service(
        "chat:u",
        Box::new(|| Arc::new(IUserService::new())),
        32,
    );
    ServerManager::run_server_shared(server);
}

#[cfg(test)]
mod tests {
    use super::super::hle_ipc::KAutoObjectRef;
    use super::*;
    use crate::hle::kernel::k_process::{KProcess, ProcessLock};
    use crate::hle::kernel::k_thread::{KThread, KThreadLock};
    use crate::hle::kernel::kernel::ScopedKernelForTest;

    #[test]
    fn chat_event_is_real_stable_and_outlives_root() {
        // Build service resources without a live global kernel, then install a
        // local kernel to exercise actual readable-object materialization.
        let no_kernel = ScopedKernelForTest::without_kernel();
        let root = IUserService::new();
        let child = IUserShimScopedObject::new(root.state.clone());
        assert!(!child.state.chatting_event.is_signaled());
        drop(root);
        drop(no_kernel);
        let _kernel = ScopedKernelForTest::new();
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let mut thread = KThread::new();
        thread.parent = Some(Arc::downgrade(&process));
        let thread = Arc::new(KThreadLock::new(thread));
        let mut ids = Vec::new();
        for _ in 0..2 {
            let mut ctx = HLERequestContext::new_with_thread(thread.clone(), 0);
            IUserShimScopedObject::get_chatting_event(&child, &mut ctx);
            assert_eq!(ctx.outgoing_copy_objects.len(), 1);
            assert!(ctx.outgoing_move_objects.is_empty());
            let KAutoObjectRef::ObjectId(id) = ctx.outgoing_copy_objects[0] else {
                panic!("expected real event")
            };
            let event = process
                .lock()
                .unwrap()
                .get_readable_event_by_object_id(id)
                .unwrap();
            assert!(!event.lock().unwrap().is_signaled());
            ids.push(id);
        }
        assert_eq!(ids[0], ids[1]);
        let mut notification_ctx = HLERequestContext::new_with_thread(thread, 0);
        IUserShimScopedObject::get_notification_event(&child, &mut notification_ctx);
        let KAutoObjectRef::ObjectId(notification_id) = notification_ctx.outgoing_copy_objects[0]
        else {
            panic!("notification identity")
        };
        assert_ne!(notification_id, ids[0]);
        assert!(!process
            .lock()
            .unwrap()
            .get_readable_event_by_object_id(notification_id)
            .unwrap()
            .lock()
            .unwrap()
            .is_signaled());
        // Exercise the existing event bridge, rather than substituting a fake
        // handle or a pre-signaled event to make initialization proceed.
        child.state.chatting_event.signal();
        assert!(process
            .lock()
            .unwrap()
            .get_readable_event_by_object_id(ids[0])
            .unwrap()
            .lock()
            .unwrap()
            .is_signaled());
        child.state.chatting_event.clear();
    }

    #[test]
    fn chat_unknown_command_is_error_even_for_nx2() {
        let _scope = ScopedKernelForTest::without_kernel();
        let root = IUserService::new();
        let mut ctx = HLERequestContext::new();
        root.report_unimplemented_function(&mut ctx, None);
        assert_eq!(
            ctx.command_buffer()[6],
            RESULT_NOT_SUPPORTED.get_inner_value()
        );
        assert!(ctx.outgoing_copy_objects.is_empty());
        assert!(ctx.outgoing_move_objects.is_empty());
    }
}
