// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/olsc/native_handle_holder.h
//! Port of zuyu/src/core/hle/service/olsc/native_handle_holder.cpp
//!
//! INativeHandleHolder: provides a native handle (KReadableEvent) to callers.

use std::collections::BTreeMap;
use std::sync::Arc;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::ResponseBuilder;
use crate::hle::service::kernel_helpers::ServiceContext;
use crate::hle::service::os::event::Event;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command table for INativeHandleHolder.
///
/// | Cmd | Handler         | Name            |
/// |-----|-----------------|-----------------|
/// | 0   | GetNativeHandle | GetNativeHandle |
pub struct INativeHandleHolder {
    service_context: ServiceContext,
    event: u32,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl INativeHandleHolder {
    pub fn new() -> Self {
        let mut service_context = ServiceContext::new("OLSC".into());
        let event = service_context.create_event("OLSC::INativeHandleHolder".into());
        Self {
            service_context,
            event,
            handlers: build_handler_map(&[(0, Some(Self::get_native_handle_handler), "GetNativeHandle")]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// Cmd 0: GetNativeHandle
    ///
    /// Eden signals the same owned event on every call, not only on creation.
    pub fn get_native_handle(&self) -> Option<Arc<Event>> {
        let event = self.service_context.get_event(self.event)?;
        event.signal();
        Some(event)
    }

    fn get_native_handle_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = this.as_any().downcast_ref::<Self>().unwrap();
        let object_id = service.get_native_handle().and_then(|event| event.copy_object_id(ctx));
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        // Upstream returns a null copy object if event allocation failed.
        rb.push_copy_object_id(object_id.unwrap_or(0));
    }
}

impl Drop for INativeHandleHolder {
    fn drop(&mut self) {
        self.service_context.close_event(self.event);
        self.event = 0;
    }
}

impl SessionRequestHandler for INativeHandleHolder {
    fn as_any(&self) -> &dyn std::any::Any { self }
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        self.handle_sync_request_impl(ctx)
    }
    fn service_name(&self) -> &str { "INativeHandleHolder" }
}

impl ServiceFramework for INativeHandleHolder {
    fn get_service_name(&self) -> &str { self.service_name() }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> { &self.handlers }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> { &self.handlers_tipc }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_handle_reply_contains_readable_copy_object() {
        use crate::hle::kernel::k_process::{KProcess, ProcessLock};
        use crate::hle::kernel::k_readable_event::KReadableEvent;
        use crate::hle::kernel::k_thread::{KThread, KThreadLock};
        use crate::hle::service::hle_ipc::KAutoObjectRef;
        use std::sync::Mutex;

        let holder = INativeHandleHolder::new();
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));
        let readable = Arc::new(Mutex::new(KReadableEvent::new()));
        readable.lock().unwrap().initialize(0x100, 0x101);
        process.lock().unwrap().register_readable_event_object(0x101, readable.clone());
        holder.service_context.get_event(holder.event).unwrap()
            .attach_kernel_event(readable.clone(), process.clone());
        let mut ctx = HLERequestContext::new_with_thread(thread, 0);
        holder.handlers()[&0].handler_callback.unwrap()(&holder, &mut ctx);
        assert_eq!(ctx.outgoing_copy_objects.len(), 1);
        assert!(matches!(ctx.outgoing_copy_objects[0], KAutoObjectRef::ObjectId(0x101)));
        assert!(process.lock().unwrap().get_readable_event_by_object_id(0x101).is_some());
        assert!(readable.lock().unwrap().is_signaled.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn native_handle_reuses_and_resignals_owned_event() {
        let holder = INativeHandleHolder::new();
        let initial = holder.service_context.get_event(holder.event).unwrap();
        assert!(!initial.is_signaled());
        let first = holder.get_native_handle().unwrap();
        assert!(Arc::ptr_eq(&initial, &first));
        assert!(first.is_signaled());
        first.clear();
        let second = holder.get_native_handle().unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(second.is_signaled());
    }
}
