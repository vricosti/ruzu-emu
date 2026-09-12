// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of eden/src/core/hle/service/mm/mm_u.{h,cpp}
//!
//! MM_U service ("mm:u").

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};
use std::collections::BTreeMap;
use std::sync::Mutex;

// An open enum preserves the raw module values accepted by upstream PopEnum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Module(u32);

#[allow(dead_code)] // Upstream's named module values; requests also carry other raw values.
impl Module {
    const CPU: Self = Self(0);
    const GPU: Self = Self(1);
    const EMC: Self = Self(2);
    const SYS_BUS: Self = Self(3);
    const M_SELECT: Self = Self(4);
    const NVDEC: Self = Self(5);
    const NVENC: Self = Self(6);
    const NVJPG: Self = Self(7);
    const TEST: Self = Self(8);
}

struct Session {
    module: Module,
    request_id: u32,
    min: u32,
    max: i32,
    #[allow(dead_code)] // Retained by upstream; no event is exposed by this service yet.
    is_auto_clear_event: bool,
}

impl Session {
    fn new(module: Module, request_id: u32, is_auto_clear_event: bool) -> Self {
        Self {
            module,
            request_id,
            min: 0,
            max: -1,
            is_auto_clear_event,
        }
    }

    fn set_and_wait(&mut self, min: u32, max: i32) {
        self.min = min;
        self.max = max;
    }
}

/// Mutex protects the state owned by upstream MM_U across host dispatches.
struct MmUState {
    sessions: Vec<Session>,
    request_id: u32,
}

/// MM_U service ("mm:u").
///
/// Corresponds to `MM_U` class in upstream `mm_u.cpp`.
pub struct MmU {
    state: Mutex<MmUState>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl MmU {
    pub fn new() -> Self {
        let handlers = build_handler_map(&[
            (0, Some(Self::initialize_old_handler), "InitializeOld"),
            (1, Some(Self::finalize_old_handler), "FinalizeOld"),
            (2, Some(Self::set_and_wait_old_handler), "SetAndWaitOld"),
            (3, Some(Self::get_old_handler), "GetOld"),
            (4, Some(Self::initialize_handler), "Initialize"),
            (5, Some(Self::finalize_handler), "Finalize"),
            (6, Some(Self::set_and_wait_handler), "SetAndWait"),
            (7, Some(Self::get_handler), "Get"),
        ]);

        Self {
            state: Mutex::new(MmUState {
                sessions: Vec::new(),
                request_id: 1,
            }),
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn initialize_old(&self, module: Module, event_clear_mode: u32) {
        self.initialize(module, event_clear_mode);
    }

    fn finalize_old(&self, module: Module) {
        let mut state = self.state.lock().unwrap();
        if let Some(index) = state.sessions.iter().position(|s| s.module == module) {
            state.sessions.remove(index);
        }
    }

    fn set_and_wait_old(&self, module: Module, min: u32, max: i32) {
        let mut state = self.state.lock().unwrap();
        if let Some(session) = state.sessions.iter_mut().find(|s| s.module == module) {
            session.set_and_wait(min, max);
        }
    }

    fn get_old(&self, module: Module) -> u32 {
        self.state
            .lock()
            .unwrap()
            .sessions
            .iter()
            .find(|s| s.module == module)
            .map_or(0, |s| s.min)
    }

    fn initialize(&self, module: Module, event_clear_mode: u32) -> u32 {
        let mut state = self.state.lock().unwrap();
        let id = state.request_id;
        state.request_id = state.request_id.wrapping_add(1);
        state
            .sessions
            .push(Session::new(module, id, event_clear_mode == 1));
        id
    }

    fn finalize(&self, id: u32) {
        let mut state = self.state.lock().unwrap();
        if let Some(index) = state.sessions.iter().position(|s| s.request_id == id) {
            state.sessions.remove(index);
        }
    }

    fn set_and_wait(&self, id: u32, min: u32, max: i32) {
        let mut state = self.state.lock().unwrap();
        if let Some(session) = state.sessions.iter_mut().find(|s| s.request_id == id) {
            session.set_and_wait(min, max);
        }
    }

    fn get(&self, id: u32) -> u32 {
        self.state
            .lock()
            .unwrap()
            .sessions
            .iter()
            .find(|s| s.request_id == id)
            .map_or(0, |s| s.min)
    }

    // --- Handler bridge functions ---

    fn initialize_old_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const MmU) };
        let mut rp = RequestParser::new(ctx);
        let module = Module(rp.pop_u32());
        rp.pop_u32();
        let event_clear_mode = rp.pop_u32();
        service.initialize_old(module, event_clear_mode);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn finalize_old_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const MmU) };
        let mut rp = RequestParser::new(ctx);
        service.finalize_old(Module(rp.pop_u32()));
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn set_and_wait_old_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const MmU) };
        let mut rp = RequestParser::new(ctx);
        let module = Module(rp.pop_u32());
        let min = rp.pop_u32();
        let max = rp.pop_u32() as i32;
        service.set_and_wait_old(module, min, max);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_old_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const MmU) };
        let mut rp = RequestParser::new(ctx);
        let val = service.get_old(Module(rp.pop_u32()));
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(val);
    }

    fn initialize_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const MmU) };
        let mut rp = RequestParser::new(ctx);
        let module = Module(rp.pop_u32());
        rp.pop_u32();
        let event_clear_mode = rp.pop_u32();
        let id = service.initialize(module, event_clear_mode);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(id);
    }

    fn finalize_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const MmU) };
        let mut rp = RequestParser::new(ctx);
        service.finalize(rp.pop_u32());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn set_and_wait_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const MmU) };
        let mut rp = RequestParser::new(ctx);
        let input_id = rp.pop_u32();
        let min = rp.pop_u32();
        let max = rp.pop_u32() as i32;
        service.set_and_wait(input_id, min, max);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const MmU) };
        let mut rp = RequestParser::new(ctx);
        let val = service.get(rp.pop_u32());
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(val);
    }
}

impl SessionRequestHandler for MmU {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }
    fn service_name(&self) -> &str {
        "mm:u"
    }
}

impl ServiceFramework for MmU {
    fn get_service_name(&self) -> &str {
        "mm:u"
    }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// Registers "mm:u" service.
///
/// Corresponds to `LoopProcess` in upstream `mm_u.cpp`.
pub fn loop_process(system: crate::core::SystemRef) {
    use crate::hle::service::hle_ipc::SessionRequestHandlerPtr;
    use crate::hle::service::server_manager::ServerManager;

    let server_manager = ServerManager::new_shared(system);
    // RegisterNamedService upstream shares one MM_U instance across connections.
    let service: SessionRequestHandlerPtr = std::sync::Arc::new(MmU::new());
    {
        let mut server_manager = server_manager.lock().unwrap();
        server_manager.register_named_service(
            "mm:u",
            Box::new(move || std::sync::Arc::clone(&service)),
            64,
        );
    }
    ServerManager::run_server_shared(server_manager);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(args: &[u32]) -> HLERequestContext {
        let mut ctx = HLERequestContext::new();
        ctx.command_buffer_mut()[2..2 + args.len()].copy_from_slice(args);
        ctx
    }

    fn reply_value(ctx: &HLERequestContext) -> u32 {
        ctx.command_buffer()[ctx.get_data_payload_offset() as usize + 2]
    }

    #[test]
    fn sessions_keep_independent_values_and_signed_maximum() {
        let service = MmU::new();
        let first = service.initialize(Module::NVDEC, 1);
        let second = service.initialize(Module::GPU, 2);
        assert_eq!((first, second), (1, 2));
        service.set_and_wait(first, 123, -1);
        service.set_and_wait(second, 456, i32::MIN);
        assert_eq!((service.get(first), service.get(second)), (123, 456));
        let state = service.state.lock().unwrap();
        assert_eq!(state.sessions[0].max, -1);
        assert!(state.sessions[0].is_auto_clear_event);
        assert_eq!(state.sessions[1].max, i32::MIN);
        assert!(!state.sessions[1].is_auto_clear_event);
        drop(state);
        service.finalize(first);
        service.set_and_wait(first, 999, 0);
        service.finalize(999);
        assert_eq!((service.get(first), service.get(second)), (0, 456));
        assert_eq!(service.initialize(Module::NVDEC, 0), 3);
    }

    #[test]
    fn old_commands_select_only_first_matching_module() {
        let service = MmU::new();
        service.initialize_old(Module::NVDEC, 0);
        let second = service.initialize(Module::NVDEC, 0);
        service.set_and_wait(second, 22, -1);
        service.set_and_wait_old(Module::NVDEC, 11, -2);
        assert_eq!(service.get_old(Module::NVDEC), 11);
        assert_eq!(service.get(second), 22);
        service.finalize_old(Module::NVDEC);
        assert_eq!(service.get_old(Module::NVDEC), 22);
        service.finalize_old(Module::NVDEC);
        assert_eq!(service.get_old(Module::NVDEC), 0);
    }

    #[test]
    fn ipc_commands_parse_module_id_and_event_mode_in_order() {
        let service = MmU::new();
        let mut init = request(&[Module::NVDEC.0, 0xdead, 1]);
        MmU::initialize_handler(&service, &mut init);
        assert_eq!(reply_value(&init), 1);
        let mut old_init = request(&[Module::GPU.0, 0xbeef, 2]);
        MmU::initialize_old_handler(&service, &mut old_init);
        let mut set = request(&[1, 333, u32::MAX]);
        MmU::set_and_wait_handler(&service, &mut set);
        let mut old_set = request(&[Module::GPU.0, 444, 0x8000_0000]);
        MmU::set_and_wait_old_handler(&service, &mut old_set);
        let mut get = request(&[1]);
        MmU::get_handler(&service, &mut get);
        assert_eq!(reply_value(&get), 333);
        let mut old_get = request(&[Module::GPU.0]);
        MmU::get_old_handler(&service, &mut old_get);
        assert_eq!(reply_value(&old_get), 444);
        let state = service.state.lock().unwrap();
        assert_eq!(state.sessions[1].max, i32::MIN);
        assert!(state.sessions[0].is_auto_clear_event);
        assert!(!state.sessions[1].is_auto_clear_event);
        drop(state);
        let mut finalize = request(&[1]);
        MmU::finalize_handler(&service, &mut finalize);
        let mut old_finalize = request(&[Module::GPU.0]);
        MmU::finalize_old_handler(&service, &mut old_finalize);
        assert!(service.state.lock().unwrap().sessions.is_empty());
    }

    #[test]
    fn request_ids_wrap_as_unsigned_and_raw_modules_remain_distinct() {
        let service = MmU::new();
        service.state.lock().unwrap().request_id = u32::MAX;
        assert_eq!(service.initialize(Module(u32::MAX), 0), u32::MAX);
        assert_eq!(service.initialize(Module::CPU, 0), 0);
        service.set_and_wait_old(Module(u32::MAX), 77, -1);
        assert_eq!(service.get(u32::MAX), 77);
        assert_eq!(service.get_old(Module::CPU), 0);
    }
}
