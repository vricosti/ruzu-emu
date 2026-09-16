// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of eden/src/core/hle/service/olsc/stopper_object.{h,cpp}.
//! Eden exposes an empty service object with no commands or other side effects.

use std::collections::BTreeMap;
use crate::hle::result::ResultCode;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::service::{FunctionInfo, ServiceFramework};

#[derive(Default)]
pub struct IStopperObject {
    handlers: BTreeMap<u32, FunctionInfo>,
}

impl SessionRequestHandler for IStopperObject {
    fn as_any(&self) -> &dyn std::any::Any { self }
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        self.handle_sync_request_impl(ctx)
    }
    fn service_name(&self) -> &str { "IStopperObject" }
}

impl ServiceFramework for IStopperObject {
    fn get_service_name(&self) -> &str { self.service_name() }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> { &self.handlers }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> { &self.handlers }
}
