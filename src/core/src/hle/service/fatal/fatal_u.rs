// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/fatal/fatal_u.h
//! Port of zuyu/src/core/hle/service/fatal/fatal_u.cpp
//!
//! Fatal_U -- "fatal:u" service interface.
//! Provides user-facing fatal error reporting (ThrowFatal, ThrowFatalWithPolicy,
//! ThrowFatalWithCpuContext).

use crate::hle::result::ResultCode;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Fatal_U service.
///
/// Corresponds to `Fatal_U` in upstream fatal_u.h / fatal_u.cpp.
/// Commands delegate to Module::Interface methods in fatal.rs.
pub struct FatalU {
    pub interface: super::fatal::Interface,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl FatalU {
    pub fn new(module: Arc<super::fatal::Module>, system: crate::core::SystemRef) -> Self {
        let handlers = build_handler_map(&[
            (0, Some(FatalU::throw_fatal_handler), "ThrowFatal"),
            (
                1,
                Some(FatalU::throw_fatal_with_policy_handler),
                "ThrowFatalWithPolicy",
            ),
            (
                2,
                Some(FatalU::throw_fatal_with_cpu_context_handler),
                "ThrowFatalWithCpuContext",
            ),
        ]);

        log::debug!("fatal:u created");
        Self {
            interface: super::fatal::Interface::new(
                system,
                module,
                "fatal:u",
            ),
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn throw_fatal_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = unsafe { &*(this as *const dyn ServiceFramework as *const FatalU) };
        svc.interface.throw_fatal_handler(ctx);
    }

    fn throw_fatal_with_policy_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = unsafe { &*(this as *const dyn ServiceFramework as *const FatalU) };
        svc.interface.throw_fatal_with_policy_handler(ctx);
    }

    fn throw_fatal_with_cpu_context_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = unsafe { &*(this as *const dyn ServiceFramework as *const FatalU) };
        svc.interface.throw_fatal_with_cpu_context_handler(ctx);
    }
}

impl SessionRequestHandler for FatalU {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }
    fn service_name(&self) -> &str {
        "fatal:u"
    }
}

impl ServiceFramework for FatalU {
    fn get_service_name(&self) -> &str {
        "fatal:u"
    }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}
