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
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder, RESULT_NOT_SUPPORTED};
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

/// Upstream SaveDataHandle; this is not an implemented persistent save handle.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SaveDataHandle {
    unk0: u64,
}
const _: () = assert!(std::mem::size_of::<SaveDataHandle>() == 8);

/// Port of upstream IUserShimScopedObject in ngc.cpp. StreamPlay remains stubbed.
struct IUserShimScopedObject {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IUserShimScopedObject {
    fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (450, None, "InitializeForSaveData"),
                (451, None, "FinalizeForSaveData"),
                (452, Some(Self::open_save_data), "OpenSaveData"),
                (453, None, "CloseSaveData"),
                (454, Some(Self::read_save_slot), "ReadSaveSlot"),
                (455, Some(Self::write_save_slot), "WriteSaveSlot"),
                (456, None, "FlushSaveSlot"),
                (457, None, "CommitSaveData"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn open_save_data(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let _uid = RequestParser::new(ctx).pop_raw::<crate::hle::service::acc::profile_manager::Uid>();
        log::warn!("IUserShimScopedObject::OpenSaveData stubbed");
        let handle = SaveDataHandle::default();
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_NOT_SUPPORTED);
        rb.push_u64(handle.unk0);
    }

    fn read_save_slot(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let _offset = rp.pop_i32();
        rp.skip(1); // CMIF aligns the following SaveDataHandle to eight bytes.
        let _handle = rp.pop_raw::<SaveDataHandle>();
        log::warn!("IUserShimScopedObject::ReadSaveSlot stubbed");
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_NOT_SUPPORTED);
        rb.push_u32(0);
        // Eden's CMIF wrapper copies its temporary output buffer even on error.
        // Rust initializes it rather than exposing uninitialized host memory.
        if ctx.can_write_buffer(0) {
            ctx.write_buffer(&vec![0; ctx.get_write_buffer_size(0)], 0);
        }
    }

    fn write_save_slot(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let _offset = rp.pop_i32();
        rp.skip(1);
        let _handle = rp.pop_raw::<SaveDataHandle>();
        let _data = ctx.read_buffer(0);
        log::warn!("IUserShimScopedObject::WriteSaveSlot stubbed");
        // Upstream returns success without storing anything.
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }
}

impl SessionRequestHandler for IUserShimScopedObject {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "IUserShimScopedObject"
    }
}

impl ServiceFramework for IUserShimScopedObject {
    fn get_service_name(&self) -> &str {
        "IUserShimScopedObject"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// Port of upstream IUserService in ngc.cpp. StreamPlay remains stubbed.
struct IUserService {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IUserService {
    fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, Some(Self::cmd0), "Cmd0"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn cmd0(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let _unk0 = RequestParser::new(ctx).pop_u32();
        log::warn!("IUserService::Cmd0 stubbed");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(std::sync::Arc::new(IUserShimScopedObject::new()));
    }
}

impl SessionRequestHandler for IUserService {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "stpl:u"
    }
}

impl ServiceFramework for IUserService {
    fn get_service_name(&self) -> &str {
        "stpl:u"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// Port of upstream ISystemShimScopedObject in ngc.cpp. StreamPlay remains stubbed.
struct ISystemShimScopedObject {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ISystemShimScopedObject {
    fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (106, None, "Cmd106"),
                (107, None, "Cmd107"),
                (108, Some(Self::cmd108), "Cmd108"),
                (207, None, "Cmd207"),
                (208, Some(Self::cmd208), "Cmd208"),
                (209, None, "Cmd209"),
                (210, None, "Cmd210"),
                (211, None, "Cmd211"),
                (212, None, "Cmd212"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn cmd108(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("ISystemShimScopedObject::Cmd108 stubbed");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_NOT_SUPPORTED);
    }

    fn cmd208(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("ISystemShimScopedObject::Cmd208 stubbed");
        let mut rb = ResponseBuilder::new(ctx, 10, 0, 0);
        rb.push_result(RESULT_NOT_SUPPORTED);
        for _ in 0..8 {
            rb.push_u32(0);
        }
    }
}

impl SessionRequestHandler for ISystemShimScopedObject {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "ISystemShimScopedObject"
    }
}

impl ServiceFramework for ISystemShimScopedObject {
    fn get_service_name(&self) -> &str {
        "ISystemShimScopedObject"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// Port of upstream ISystemService in ngc.cpp. StreamPlay remains stubbed.
struct ISystemService {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ISystemService {
    fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, Some(Self::cmd0), "Cmd0"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn cmd0(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("ISystemService::Cmd0 stubbed");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(std::sync::Arc::new(ISystemShimScopedObject::new()));
    }
}

impl SessionRequestHandler for ISystemService {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "stpl:sys"
    }
}

impl ServiceFramework for ISystemService {
    fn get_service_name(&self) -> &str {
        "stpl:sys"
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
        // Eden FirmwareManager::GetFirmwareVersion wraps this system-aware
        // reader. The context-free legacy helper would always report18.0.0.
        use crate::hle::service::set::system_settings_server::get_firmware_version_impl_for_system;
        use crate::hle::service::set::settings_types::GetFirmwareVersionType;
        let firmware = get_firmware_version_impl_for_system(
            system.get(), GetFirmwareVersionType::Version2,
        ).unwrap_or_default();
        if firmware.major >= 23 {
            server_manager.register_named_service(
                "stpl:u",
                Box::new(|| -> SessionRequestHandlerPtr { std::sync::Arc::new(IUserService::new()) }),
                4,
            );
            server_manager.register_named_service(
                "stpl:sys",
                Box::new(|| -> SessionRequestHandlerPtr { std::sync::Arc::new(ISystemService::new()) }),
                4,
            );
        }
    }

    ServerManager::run_server_shared(server_manager);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamplay_tables_and_reply_payloads_match_upstream() {
        let user = IUserShimScopedObject::new();
        let system = ISystemShimScopedObject::new();
        assert_eq!(user.handlers().keys().copied().collect::<Vec<_>>(), (450..=457).collect::<Vec<_>>());
        assert_eq!(system.handlers().keys().copied().collect::<Vec<_>>(), [106,107,108,207,208,209,210,211,212]);
        assert_eq!(std::mem::size_of::<SaveDataHandle>(), 8);
        assert_eq!(std::mem::align_of::<SaveDataHandle>(), 8);
        assert_eq!(std::mem::offset_of!(SaveDataHandle, unk0), 0);
        for (service, cases) in [
            (&user as &dyn ServiceFramework, &[(452, 0x20b, 2), (454, 0x20b, 1), (455, 0, 0)][..]),
            (&system as &dyn ServiceFramework, &[(108, 0x20b, 0), (208, 0x20b, 8)][..]),
        ] {
            for (&id, info) in service.handlers() {
                assert_eq!(info.handler_callback.is_some(), cases.iter().any(|c| c.0 == id));
            }
            for &(command, result, output_words) in cases {
                let mut ctx = HLERequestContext::new();
                ctx.cmd_buf.fill(0xa5a5a5a5);
                service.handlers()[&command].handler_callback.unwrap()(service, &mut ctx);
                assert_eq!(ctx.cmd_buf[6], result);
                assert_eq!(&ctx.cmd_buf[8..8 + output_words], vec![0; output_words]);
            }
        }
    }

    #[test]
    fn streamplay_factories_return_the_corresponding_scoped_object() {
        use crate::hle::service::hle_ipc::SessionRequestManager;
        use std::sync::{Arc, Mutex};
        for (service, expected) in [
            (Arc::new(IUserService::new()) as SessionRequestHandlerPtr, "IUserShimScopedObject"),
            (Arc::new(ISystemService::new()) as SessionRequestHandlerPtr, "ISystemShimScopedObject"),
        ] {
            let manager = Arc::new(Mutex::new(SessionRequestManager::new()));
            manager.lock().unwrap().set_session_handler(service.clone());
            manager.lock().unwrap().convert_to_domain();
            let mut ctx = HLERequestContext::new();
            ctx.set_session_request_manager(manager.clone());
            let mut words = [0; crate::hle::ipc::COMMAND_BUFFER_LENGTH];
            words[0] = 4;
            words[1] = 12;
            words[4..8].copy_from_slice(&[0x200001, 1, 0, 0]);
            words[8..12].copy_from_slice(&[u32::from_le_bytes(*b"SFCI"), 0, 0, 0]);
            words[12] = 0x12345678;
            ctx.populate_from_incoming_command_buffer(&words);
            assert_eq!(service.handle_sync_request(&mut ctx), RESULT_SUCCESS);
            ctx.write_to_outgoing_command_buffer();
            assert_eq!(ctx.cmd_buf[10], 0);
            let guard = manager.lock().unwrap();
            assert_eq!(guard.domain_handler_count(), 2);
            assert_eq!(guard.domain_handler(1).unwrap().service_name(), expected);
        }
    }

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
