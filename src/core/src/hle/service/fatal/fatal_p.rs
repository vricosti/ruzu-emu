// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/fatal/fatal_p.h
//! Port of zuyu/src/core/hle/service/fatal/fatal_p.cpp
//!
//! Fatal_P -- "fatal:p" service interface.
//! This service provides private fatal error query interface.

use crate::hle::result::ResultCode;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Fatal_P service.
///
/// Corresponds to `Fatal_P` in upstream fatal_p.h / fatal_p.cpp.
/// Both commands are nullptr (unimplemented) in upstream.
pub struct FatalP {
    pub interface: super::fatal::Interface,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl FatalP {
    pub fn new(module: Arc<super::fatal::Module>, system: crate::core::SystemRef) -> Self {
        let handlers =
            build_handler_map(&[(0, None, "GetFatalEvent"), (10, None, "GetFatalContext")]);

        log::debug!("fatal:p created");
        Self {
            interface: super::fatal::Interface::new(system, module, "fatal:p"),
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for FatalP {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }
    fn service_name(&self) -> &str {
        "fatal:p"
    }
}

impl ServiceFramework for FatalP {
    fn get_service_name(&self) -> &str {
        "fatal:p"
    }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}
