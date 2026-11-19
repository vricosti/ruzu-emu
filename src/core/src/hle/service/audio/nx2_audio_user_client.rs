// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later
//! Port of eden-nx2/src/core/hle/service/audio/nx2_audio_user_client.h.
//! Header-only upstream: aud:u command2 returns the existing renderer manager.
use super::audio_renderer_manager::IAudioRendererManager;
use crate::core::SystemRef;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};
use std::collections::BTreeMap;
use std::sync::Arc;

pub struct INx2AudioUserClient {
    system: SystemRef,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}
impl INx2AudioUserClient {
    pub fn new(system: SystemRef) -> Self {
        Self {
            system,
            handlers: build_handler_map(&[(
                2,
                Some(Self::get_audio_renderer_manager),
                "GetAudioRendererManager (guessed)",
            )]),
            handlers_tipc: BTreeMap::new(),
        }
    }
    fn get_audio_renderer_manager(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let arg = RequestParser::new(ctx).pop_u32();
        log::warn!(
            "(STUBBED) Switch 2 aud:u 2 arg={}, returning IAudioRendererManager",
            arg
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(Arc::new(IAudioRendererManager::new(this.system)));
    }
}
impl SessionRequestHandler for INx2AudioUserClient {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        self.handle_sync_request_impl(ctx)
    }
}
impl ServiceFramework for INx2AudioUserClient {
    fn get_service_name(&self) -> &str {
        "nx2:aud:u"
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
    use crate::hle::kernel::kernel::ScopedKernelForTest;
    use std::sync::Mutex;
    #[test]
    fn renderer_manager_factory_returns_fresh_moved_sessions() {
        use crate::core::SystemRef;
        use crate::hle::kernel::k_process::{KProcess, ProcessLock};
        use crate::hle::kernel::k_resource_limit::{KResourceLimit, LimitableResource};
        use crate::hle::kernel::k_thread::{KThread, KThreadLock};
        use crate::hle::service::hle_ipc::{KAutoObjectRef, SessionRequestManager};
        use crate::hle::service::server_manager::ServerManager;
        let _scope = ScopedKernelForTest::without_kernel();
        let limit = Arc::new(KResourceLimit::new());
        limit
            .set_limit_value(LimitableResource::SessionCountMax, 2)
            .unwrap();
        let mut process = KProcess::new();
        process.resource_limit = Some(limit.clone());
        process.initialize_handle_table();
        let process = Arc::new(ProcessLock::from_value(process));
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));
        let server = ServerManager::new_shared(SystemRef::null());
        let (queue, wakeup) = {
            let guard = server.lock().unwrap();
            (guard.pending_registrations_arc(), guard.wakeup_event_arc())
        };
        let manager = Arc::new(Mutex::new(
            SessionRequestManager::new_with_server_manager_full(
                server.clone(),
                queue.clone(),
                wakeup,
            ),
        ));
        let service = INx2AudioUserClient::new(SystemRef::null());
        let mut ids = Vec::new();
        for arg in [0, u32::MAX] {
            let mut ctx = HLERequestContext::new_with_thread(thread.clone(), 0);
            ctx.set_session_request_manager(manager.clone());
            ctx.populate_from_incoming_command_buffer(&[
                6, 9, 0, 0, 0x49434653, 1, 2, 0, arg, 0, 0, 0,
            ]);
            service.handle_sync_request(&mut ctx);
            assert_eq!(ctx.command_buffer()[2], 0x20);
            assert_eq!(ctx.command_buffer()[4], 0x4f434653);
            assert_eq!(ctx.command_buffer()[6], 0);
            assert!(ctx.outgoing_copy_objects.is_empty());
            let KAutoObjectRef::ObjectId(id) = ctx.outgoing_move_objects[0] else {
                panic!("session identity")
            };
            assert!(process
                .lock()
                .unwrap()
                .client_session_objects
                .contains_key(&id));
            ids.push(id);
        }
        assert_ne!(ids[0], ids[1]);
        assert_eq!(queue.lock().unwrap().len(), 2);

        drop(service);
        for id in ids {
            process
                .lock()
                .unwrap()
                .unregister_session_object_by_object_id(id);
        }
        assert_eq!(
            limit.get_current_value(LimitableResource::SessionCountMax),
            0
        );
    }
}
