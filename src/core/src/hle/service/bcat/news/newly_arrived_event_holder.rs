// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/core/hle/service/bcat/news/newly_arrived_event_holder.h
//! Port of zuyu/src/core/hle/service/bcat/news/newly_arrived_event_holder.cpp

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::hle::result::ResultCode;
use crate::hle::result::RESULT_SUCCESS;
use crate::hle::result::RESULT_UNKNOWN;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::ResponseBuilder;
use crate::hle::service::kernel_helpers::ServiceContext;
use crate::hle::service::os::event::Event;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command IDs for INewlyArrivedEventHolder
pub mod commands {
    pub const GET: u32 = 0;
}

/// INewlyArrivedEventHolder corresponds to upstream `News::INewlyArrivedEventHolder`.
pub struct INewlyArrivedEventHolder {
    service_context: ServiceContext,
    arrived_event_handle: u32,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl INewlyArrivedEventHolder {
    pub fn new() -> Self {
        let handlers = build_handler_map(&[(commands::GET, Some(Self::get_handler), "Get")]);

        let mut service_context = ServiceContext::new("INewlyArrivedEventHolder".to_string());
        let arrived_event_handle =
            service_context.create_event("INewlyArrivedEventHolder::ArrivedEvent".to_string());

        Self {
            service_context,
            arrived_event_handle,
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn get_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let (_, event) = service.get();
        let Some(object) = event.copy_object_id(ctx) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object);
    }

    pub fn get(&self) -> (ResultCode, Arc<Event>) {
        log::info!("INewlyArrivedEventHolder::get called");
        let event = self
            .service_context
            .get_event(self.arrived_event_handle)
            .expect("arrived_event must exist");
        (RESULT_SUCCESS, event)
    }
}

impl Drop for INewlyArrivedEventHolder {
    fn drop(&mut self) {
        self.service_context.close_event(self.arrived_event_handle);
    }
}

impl SessionRequestHandler for INewlyArrivedEventHolder {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "INewlyArrivedEventHolder"
    }
}

impl ServiceFramework for INewlyArrivedEventHolder {
    fn get_service_name(&self) -> &str {
        "INewlyArrivedEventHolder"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}
