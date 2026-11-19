// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden-NX2 `core/hle/service/am/service/graphics_scale_controller.h`.
//! Experimental upstream stub: this name is guessed and command 10 returns
//! only u32(10). The guest SDK expects 12 bytes; the other fields are not
//! implemented upstream. This is not functional graphics scaling emulation.

use std::collections::BTreeMap;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::ResponseBuilder;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

pub struct IGraphicsScaleController {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IGraphicsScaleController {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[(10, Some(Self::get_current_graphics_scale), "GetCurrentGraphicsScale")]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn get_current_graphics_scale(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) GetCurrentGraphicsScale called, returning 10");
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(10);
    }
}

impl SessionRequestHandler for IGraphicsScaleController {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        self.handle_sync_request_impl(ctx)
    }
}

impl ServiceFramework for IGraphicsScaleController {
    fn get_service_name(&self) -> &str { "nx2:IGraphicsScaleController" }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> { &self.handlers }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> { &self.handlers_tipc }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nx2_graphics_scale_matches_upstream_stub_payload() {
        let service = IGraphicsScaleController::new();
        let mut ctx = HLERequestContext::new();
        ctx.command_buffer_mut().fill(u32::MAX);
        (service.handlers()[&10].handler_callback.unwrap())(&service, &mut ctx);
        let words = ctx.command_buffer();
        assert_eq!(words[4], 0x4f434653);
        assert_eq!(&words[6..9], &[0, 0, 10]);
        assert!(words[9..].iter().all(|word| *word == 0));
    }
}
