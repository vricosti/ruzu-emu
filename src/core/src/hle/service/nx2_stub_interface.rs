// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden-NX2 `core/hle/service/nx2_stub_interface.h`.
//! Experimental success stubs, explicitly authorized for the NX2 investigation.

use std::collections::BTreeMap;
use std::sync::Arc;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use super::hle_ipc::{HLERequestContext, SessionRequestHandler};
use super::ipc_helpers::ResponseBuilder;
use super::service::{FunctionInfo, ServiceFramework};

pub const NX2_STUB_PREFIX: &str = "nx2:";

pub struct INx2StubInterface {
    // Rust owns the name rather than interning C strings for pointer stability.
    name: String,
    handlers: BTreeMap<u32, FunctionInfo>,
}

impl INx2StubInterface {
    pub fn new(name: String) -> Self {
        Self { name, handlers: BTreeMap::new() }
    }
}

impl SessionRequestHandler for INx2StubInterface {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        self.handle_sync_request_impl(ctx)
    }
}

impl ServiceFramework for INx2StubInterface {
    fn get_service_name(&self) -> &str { &self.name }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> { &self.handlers }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> { &self.handlers }
}

pub fn return_nx2_stub_interface(ctx: &mut HLERequestContext, owner: &str) {
    let name = format!("{NX2_STUB_PREFIX}{owner}::{}", ctx.get_command());
    log::warn!("(STUBBED) Switch 2 {}::{} -> stub interface {}", owner, ctx.get_command(), name);
    let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
    rb.push_result(RESULT_SUCCESS);
    rb.push_ipc_interface(Arc::new(INx2StubInterface::new(name)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nx2_unknown_interface_command_returns_empty_success() {
        let service = INx2StubInterface::new("nx2:IApplicationProxy::40".to_owned());
        let mut ctx = HLERequestContext::new();
        ctx.command_buffer_mut().fill(u32::MAX);
        service.report_unimplemented_function(&mut ctx, None);
        assert_eq!(ctx.command_buffer()[4], 0x4f434653); // SFCO
        assert!(ctx.command_buffer()[6..].iter().all(|word| *word == 0));
    }
}
