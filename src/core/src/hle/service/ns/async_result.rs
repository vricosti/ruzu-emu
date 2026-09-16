// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of core/hle/service/ns/async_result.{h,cpp}.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::ResponseBuilder;
use crate::hle::service::os::event::Event;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

pub struct IAsyncResult {
    // Upstream borrows Event*. Retain the shared event if the parent IPC
    // interface closes before this child; Event owns its kernel bridge.
    event: Option<Arc<Event>>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IAsyncResult {
    pub fn new(event: Option<Arc<Event>>) -> Self {
        Self {
            event,
            handlers: build_handler_map(&[
                (0, None, "Get"),
                (1, Some(Self::cancel_handler), "Cancel"),
                (2, None, "GetErrorContext"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn cancel(&self) -> ResultCode {
        if let Some(event) = &self.event {
            event.signal();
        }
        RESULT_SUCCESS
    }

    fn cancel_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let result = service.cancel();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }
}

impl SessionRequestHandler for IAsyncResult {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        self.handle_sync_request_impl(ctx)
    }

    fn service_name(&self) -> &str {
        "nn::ns::detail::IAsyncResult"
    }
}

impl ServiceFramework for IAsyncResult {
    fn get_service_name(&self) -> &str {
        self.service_name()
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
    fn cancel_signals_the_shared_event_after_parent_release() {
        let event = Arc::new(Event::new());
        let weak = Arc::downgrade(&event);
        let result = IAsyncResult::new(Some(event.clone()));
        assert!(!event.is_signaled());
        drop(event);
        assert_eq!(result.cancel(), RESULT_SUCCESS);
        assert!(weak.upgrade().unwrap().is_signaled());
        drop(result);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn only_cancel_is_implemented_and_null_event_is_allowed() {
        let result = IAsyncResult::new(None);
        assert_eq!(result.cancel(), RESULT_SUCCESS);
        for (id, implemented) in [(0, false), (1, true), (2, false)] {
            assert_eq!(result.handlers()[&id].handler_callback.is_some(), implemented);
        }
    }
}
