// SPDX-FileCopyrightText: Copyright 2021 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/ngc/ngc.cpp
//!
//! NgctServiceImpl ("ngct:u") and NgcServiceImpl ("ngc:u") services.

use std::collections::BTreeMap;

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{
    HLERequestContext, SessionRequestHandler, SessionRequestHandlerPtr,
};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command IDs for NgctServiceImpl ("ngct:u")
pub mod ngct_commands {
    pub const MATCH: u32 = 0;
    pub const FILTER: u32 = 1;
}

/// IPC command IDs for NgcServiceImpl ("ngc:u")
pub mod ngc_commands {
    pub const GET_CONTENT_VERSION: u32 = 0;
    pub const CHECK: u32 = 1;
    pub const MASK: u32 = 2;
    pub const RELOAD: u32 = 3;
    pub const CHECK2: u32 = 4;
    pub const MASK2: u32 = 5;
}

/// Upstream: `NgcContentVersion` constant.
pub const NGC_CONTENT_VERSION: u32 = 1;

/// ProfanityFilterOption. Upstream: `ProfanityFilterOption` in `ngc.cpp`.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct ProfanityFilterOption {
    pub _data: [u8; 0x20],
}

impl Default for ProfanityFilterOption {
    fn default() -> Self {
        Self { _data: [0; 0x20] }
    }
}

#[derive(Clone, Copy, Default)]
#[repr(C)]
struct InputParameters {
    flags: u32,
    option: ProfanityFilterOption,
}

/// NgctServiceImpl ("ngct:u").
pub struct NgctServiceImpl {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl NgctServiceImpl {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (
                    ngct_commands::MATCH,
                    Some(NgctServiceImpl::match_handler),
                    "Match",
                ),
                (
                    ngct_commands::FILTER,
                    Some(NgctServiceImpl::filter_handler),
                    "Filter",
                ),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// Match (cmd 0) - returns false since we don't censor anything
    pub fn match_text(&self, text: &str) -> bool {
        log::warn!("(STUBBED) NgctServiceImpl::match called, text={}", text);
        false
    }

    /// Filter (cmd 1) - returns same string since we don't censor anything
    pub fn filter(&self, buffer: &[u8]) -> Vec<u8> {
        log::warn!("(STUBBED) NgctServiceImpl::filter called");
        buffer.to_vec()
    }

    fn match_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const NgctServiceImpl) };
        let buffer = ctx.read_buffer(0);
        let text = fixed_zero_terminated_string(&buffer);
        let matched = service.match_text(&text);

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(matched);
    }

    fn filter_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const NgctServiceImpl) };
        let buffer = ctx.read_buffer(0);
        let filtered = service.filter(&buffer);
        ctx.write_buffer(&filtered, 0);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }
}

impl SessionRequestHandler for NgctServiceImpl {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "ngct:u"
    }
}

impl ServiceFramework for NgctServiceImpl {
    fn get_service_name(&self) -> &str {
        "ngct:u"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// `ngct:s` management API. Every command is intentionally stubbed upstream.
pub struct NgctServiceWithManagementApi {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl NgctServiceWithManagementApi {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "Match"),
                (1, None, "Filter"),
                (100, None, "ConfigureAutoUpdateSetting"),
                (101, None, "RequestResourceUpdateCheck"),
                (110, None, "Reload"),
                (111, None, "IsReloadRequired"),
                (112, None, "TryAcquireReloadRequestNotifier"),
                (120, None, "CalculateContentFingerprint"),
                (130, None, "TryEnableTemporalPassThrough"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for NgctServiceWithManagementApi {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "ngct:s"
    }
}

impl ServiceFramework for NgctServiceWithManagementApi {
    fn get_service_name(&self) -> &str {
        "ngct:s"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// NgcServiceImpl ("ngc:u").
pub struct NgcServiceImpl {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl NgcServiceImpl {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (
                    ngc_commands::GET_CONTENT_VERSION,
                    Some(NgcServiceImpl::get_content_version_handler),
                    "GetContentVersion",
                ),
                (
                    ngc_commands::CHECK,
                    Some(NgcServiceImpl::check_handler),
                    "Check",
                ),
                (
                    ngc_commands::MASK,
                    Some(NgcServiceImpl::mask_handler),
                    "Mask",
                ),
                (
                    ngc_commands::RELOAD,
                    Some(NgcServiceImpl::reload_handler),
                    "Reload",
                ),
                (
                    ngc_commands::CHECK2,
                    Some(NgcServiceImpl::check_handler),
                    "Check2",
                ),
                (
                    ngc_commands::MASK2,
                    Some(NgcServiceImpl::mask_handler),
                    "Mask2",
                ),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// GetContentVersion (cmd 0)
    pub fn get_content_version(&self) -> u32 {
        log::info!("(STUBBED) NgcServiceImpl::get_content_version called");
        NGC_CONTENT_VERSION
    }

    /// Check (cmd 1) - returns 0 flags (no profanity detected)
    pub fn check(&self, _flags: u32, _option: &ProfanityFilterOption, _input: &[u8]) -> u32 {
        log::info!("(STUBBED) NgcServiceImpl::check called");
        0
    }

    /// Mask (cmd 2) - returns input unchanged and 0 flags
    pub fn mask(
        &self,
        _flags: u32,
        _option: &ProfanityFilterOption,
        input: &[u8],
    ) -> (u32, Vec<u8>) {
        log::info!("(STUBBED) NgcServiceImpl::mask called");
        (0, input.to_vec())
    }

    /// Reload (cmd 3)
    pub fn reload(&self) {
        log::info!("(STUBBED) NgcServiceImpl::reload called");
    }

    fn get_content_version_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const NgcServiceImpl) };
        let version = service.get_content_version();

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(version);
    }

    fn check_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const NgcServiceImpl) };
        let mut rp = RequestParser::new(ctx);
        let params: InputParameters = rp.pop_raw();
        let input = ctx.read_buffer(0);
        let out_flags = service.check(params.flags, &params.option, &input);

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(out_flags);
    }

    fn mask_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const NgcServiceImpl) };
        let mut rp = RequestParser::new(ctx);
        let params: InputParameters = rp.pop_raw();
        let input = ctx.read_buffer(0);
        let (out_flags, output) = service.mask(params.flags, &params.option, &input);
        ctx.write_buffer(&output, 0);

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(out_flags);
    }

    fn reload_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const NgcServiceImpl) };
        service.reload();

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }
}

impl SessionRequestHandler for NgcServiceImpl {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "ngc:u"
    }
}

impl ServiceFramework for NgcServiceImpl {
    fn get_service_name(&self) -> &str {
        "ngc:u"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

fn fixed_zero_terminated_string(buffer: &[u8]) -> String {
    let end = buffer
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).into_owned()
}

/// Registers "ngct:u" and "ngc:u" services.
///
/// Corresponds to `LoopProcess` in upstream `ngc.cpp`.
pub fn loop_process(system: crate::core::SystemRef) {
    use crate::hle::service::server_manager::ServerManager;

    let server_manager = ServerManager::new_shared(system);

    {
        let mut server_manager = server_manager.lock().unwrap();
        server_manager.register_named_service(
            "ngct:u",
            Box::new(|| -> SessionRequestHandlerPtr {
                std::sync::Arc::new(NgctServiceImpl::new())
            }),
            4,
        );
        server_manager.register_named_service(
            "ngct:s",
            Box::new(|| -> SessionRequestHandlerPtr {
                std::sync::Arc::new(NgctServiceWithManagementApi::new())
            }),
            4,
        );
        server_manager.register_named_service(
            "ngc:u",
            Box::new(|| -> SessionRequestHandlerPtr { std::sync::Arc::new(NgcServiceImpl::new()) }),
            4,
        );
    }

    ServerManager::run_server_shared(server_manager);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profanity_filter_option_matches_upstream_size() {
        assert_eq!(core::mem::size_of::<ProfanityFilterOption>(), 0x20);
        assert_eq!(core::mem::size_of::<InputParameters>(), 0x24);
    }

    #[test]
    fn fixed_zero_terminated_string_stops_at_first_nul() {
        assert_eq!(fixed_zero_terminated_string(b"abc\0def"), "abc");
        assert_eq!(fixed_zero_terminated_string(b"abc"), "abc");
    }

    #[test]
    fn management_api_table_matches_upstream() {
        assert_eq!(
            NgctServiceWithManagementApi::new()
                .handlers()
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            [0, 1, 100, 101, 110, 111, 112, 120, 130]
        );
    }

    #[test]
    fn ngc_service_aliases_match_upstream() {
        let service = NgcServiceImpl::new();
        assert_eq!(service.handlers().len(), 6);
        assert_eq!(service.handlers().get(&4).unwrap().name, "Check2");
        assert_eq!(service.handlers().get(&5).unwrap().name, "Mask2");
        assert!(service
            .handlers()
            .get(&4)
            .unwrap()
            .handler_callback
            .is_some());
        assert!(service
            .handlers()
            .get(&5)
            .unwrap()
            .handler_callback
            .is_some());
    }
}
