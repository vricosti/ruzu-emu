// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

// SPDX-FileCopyrightText: Copyright 2022 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of Eden `src/core/hle/service/jit/jit.{h,cpp}`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::jit_code_memory::CodeMemory;
use super::jit_context::JitContext;
use crate::arm::symbols::get_symbols_from_data;
use crate::core::SystemRef;
use crate::hle::kernel::k_code_memory::KCodeMemory;
use crate::hle::kernel::k_process::ProcessLock;
use crate::hle::kernel::k_transfer_memory::KTransferMemory;
use crate::hle::kernel::svc::svc_types::MemoryPermission;
use crate::hle::kernel::svc_common::PseudoHandle;
use crate::hle::result::{ResultCode, RESULT_SUCCESS, RESULT_UNKNOWN};
use crate::hle::service::cmif_serialization::CmifRequest;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::ResponseBuilder;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(C)]
pub struct CodeRange {
    pub offset: u64,
    pub size: u64,
}

type Struct32 = [u64; 4];

#[derive(Debug, Clone, Copy, Default)]
struct GuestCallbacks {
    // Eden resolves these two symbols but never invokes them. Retaining the
    // fields preserves the plugin ABI inventory without inventing lifecycle calls.
    #[allow(dead_code)]
    rtld_fini: u64,
    rtld_init: u64,
    control: u64,
    resolve_basic_symbols: u64,
    setup_diagnostics: u64,
    configure: u64,
    generate_code: u64,
    get_version: u64,
    #[allow(dead_code)]
    keeper: u64,
    on_prepared: u64,
}

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
struct JITConfiguration {
    user_rx_memory: CodeRange,
    user_ro_memory: CodeRange,
    transfer_memory: CodeRange,
    sys_rx_memory: CodeRange,
    sys_ro_memory: CodeRange,
}

struct Mt19937_64 {
    state: [u64; Self::N],
    index: usize,
}

impl Mt19937_64 {
    const N: usize = 312;
    const M: usize = 156;
    const MATRIX_A: u64 = 0xB502_6F5A_A966_19E9;
    const UPPER_MASK: u64 = 0xFFFF_FFFF_8000_0000;
    const LOWER_MASK: u64 = 0x0000_0000_7FFF_FFFF;

    fn new(seed: u64) -> Self {
        let mut state = [0; Self::N];
        state[0] = seed;
        for index in 1..Self::N {
            state[index] = 6_364_136_223_846_793_005_u64
                .wrapping_mul(state[index - 1] ^ (state[index - 1] >> 62))
                .wrapping_add(index as u64);
        }
        Self {
            state,
            index: Self::N,
        }
    }

    fn next_u64(&mut self) -> u64 {
        if self.index >= Self::N {
            self.twist();
        }

        let mut value = self.state[self.index];
        self.index += 1;
        value ^= (value >> 29) & 0x5555_5555_5555_5555;
        value ^= (value << 17) & 0x71D6_7FFF_EDA6_0000;
        value ^= (value << 37) & 0xFFF7_EEE0_0000_0000;
        value ^= value >> 43;
        value
    }

    fn twist(&mut self) {
        for index in 0..Self::N {
            let value = (self.state[index] & Self::UPPER_MASK)
                | (self.state[(index + 1) % Self::N] & Self::LOWER_MASK);
            let mut mixed = value >> 1;
            if value & 1 != 0 {
                mixed ^= Self::MATRIX_A;
            }
            self.state[index] = self.state[(index + Self::M) % Self::N] ^ mixed;
        }
        self.index = 0;
    }
}

impl Default for Mt19937_64 {
    fn default() -> Self {
        Self::new(5489)
    }
}

struct IJitEnvironmentState {
    user_rx: CodeMemory,
    user_ro: CodeMemory,
    callbacks: GuestCallbacks,
    configuration: JITConfiguration,
    context: JitContext,
}

pub struct IJitEnvironment {
    // Upstream keeps a KScopedAutoObject<KProcess> solely to retain the owner.
    #[allow(dead_code)]
    process: Arc<ProcessLock>,
    state: Mutex<IJitEnvironmentState>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IJitEnvironment {
    fn as_self(this: &dyn ServiceFramework) -> &Self {
        unsafe { &*(this as *const dyn ServiceFramework as *const Self) }
    }

    fn new(
        system: SystemRef,
        process: Arc<ProcessLock>,
        mut user_rx: CodeMemory,
        mut user_ro: CodeMemory,
    ) -> Result<Self, String> {
        let Some(memory) = system.get().memory_shared() else {
            user_rx.finalize();
            user_ro.finalize();
            return Err("JIT service requires application memory".to_string());
        };
        let context = match JitContext::new(memory) {
            Ok(context) => context,
            Err(error) => {
                // Dynarmic construction is infallible in Eden. The Rust backend
                // can report allocation/emitter errors, so unwind mappings here.
                user_rx.finalize();
                user_ro.finalize();
                return Err(error);
            }
        };

        let user_rx_memory = CodeRange {
            offset: user_rx.get_address(),
            size: user_rx.get_size() as u64,
        };
        let user_ro_memory = CodeRange {
            offset: user_ro.get_address(),
            size: user_ro.get_size() as u64,
        };
        let configuration = JITConfiguration {
            user_rx_memory,
            user_ro_memory,
            transfer_memory: CodeRange::default(),
            sys_rx_memory: user_rx_memory,
            sys_ro_memory: user_ro_memory,
        };
        let handlers = build_handler_map(&[
            (0, Some(Self::generate_code_handler), "GenerateCode"),
            (1, Some(Self::control_handler), "Control"),
            (1000, Some(Self::load_plugin_handler), "LoadPlugin"),
            (1001, Some(Self::get_code_address_handler), "GetCodeAddress"),
        ]);

        Ok(Self {
            process,
            state: Mutex::new(IJitEnvironmentState {
                user_rx,
                user_ro,
                callbacks: GuestCallbacks::default(),
                configuration,
                context,
            }),
            handlers,
            handlers_tipc: BTreeMap::new(),
        })
    }

    fn clear_size(mut range: CodeRange) -> CodeRange {
        range.size = 0;
        range
    }

    fn generate_code(
        &self,
        data_size: u32,
        command: u64,
        range0: CodeRange,
        range1: CodeRange,
        data: Struct32,
        input_buffer: &[u8],
        output_buffer: &mut [u8],
    ) -> (ResultCode, i32, CodeRange, CodeRange) {
        let mut state = self.state.lock().unwrap();
        let ret_ptr = state.context.add_heap(0u32);
        let c0_in_ptr = state.context.add_heap(range0);
        let c1_in_ptr = state.context.add_heap(range1);
        let c0_out_ptr = state.context.add_heap(Self::clear_size(range0));
        let c1_out_ptr = state.context.add_heap(Self::clear_size(range1));
        let input_ptr = state.context.add_heap_bytes(input_buffer);
        let output_ptr = state.context.add_heap_bytes(output_buffer);
        let data_ptr = state.context.add_heap(data);
        let configuration = state.configuration;
        let configuration_ptr = state.context.add_heap(configuration);
        let generate_code = state.callbacks.generate_code;

        state.context.call_function(
            generate_code,
            &[
                ret_ptr,
                c0_out_ptr,
                c1_out_ptr,
                configuration_ptr,
                command,
                input_ptr,
                input_buffer.len() as u64,
                c0_in_ptr,
                c1_in_ptr,
                data_ptr,
                data_size as u64,
                output_ptr,
                output_buffer.len() as u64,
            ],
        );

        let return_value = state.context.get_heap::<i32>(ret_ptr);
        let out_range0 = state.context.get_heap::<CodeRange>(c0_out_ptr);
        let out_range1 = state.context.get_heap::<CodeRange>(c1_out_ptr);
        state.context.get_heap_into(output_ptr, output_buffer);
        let result = if return_value == 0 {
            RESULT_SUCCESS
        } else {
            log::warn!("JIT plugin GenerateCode callback failed");
            RESULT_UNKNOWN
        };
        (result, return_value, out_range0, out_range1)
    }

    fn control(
        &self,
        command: u64,
        input_buffer: &[u8],
        output_buffer: &mut [u8],
    ) -> (ResultCode, i32) {
        let mut state = self.state.lock().unwrap();
        let ret_ptr = state.context.add_heap(0u32);
        let configuration = state.configuration;
        let configuration_ptr = state.context.add_heap(configuration);
        let input_ptr = state.context.add_heap_bytes(input_buffer);
        let output_ptr = state.context.add_heap_bytes(output_buffer);
        let control = state.callbacks.control;

        let wrapper_value = state.context.call_function(
            control,
            &[
                ret_ptr,
                configuration_ptr,
                command,
                input_ptr,
                input_buffer.len() as u64,
                output_ptr,
                output_buffer.len() as u64,
            ],
        );
        let return_value = state.context.get_heap::<i32>(ret_ptr);
        state.context.get_heap_into(output_ptr, output_buffer);
        let result = if wrapper_value == 0 && return_value == 0 {
            RESULT_SUCCESS
        } else {
            log::warn!("JIT plugin Control callback failed");
            RESULT_UNKNOWN
        };
        (result, return_value)
    }

    fn load_plugin(
        &self,
        transfer_memory_size: u64,
        transfer_memory: Arc<Mutex<KTransferMemory>>,
        _nrr: &[u8],
        nro: &[u8],
    ) -> ResultCode {
        let transfer_memory_address = transfer_memory.lock().unwrap().get_source_address();
        let mut state = self.state.lock().unwrap();
        state.configuration.transfer_memory = CodeRange {
            offset: transfer_memory_address,
            size: transfer_memory_size,
        };

        let symbols = get_symbols_from_data(nro, true);
        let get_symbol = |name: &str| symbols.get(name).map_or(0, |symbol| symbol.0);
        state.callbacks = GuestCallbacks {
            rtld_fini: get_symbol("_fini"),
            rtld_init: get_symbol("_init"),
            control: get_symbol("nnjitpluginControl"),
            resolve_basic_symbols: get_symbol("nnjitpluginResolveBasicSymbols"),
            setup_diagnostics: get_symbol("nnjitpluginSetupDiagnostics"),
            configure: get_symbol("nnjitpluginConfigure"),
            generate_code: get_symbol("nnjitpluginGenerateCode"),
            get_version: get_symbol("nnjitpluginGetVersion"),
            keeper: get_symbol("nnjitpluginKeeper"),
            on_prepared: get_symbol("nnjitpluginOnPrepared"),
        };

        if state.callbacks.get_version == 0
            || state.callbacks.configure == 0
            || state.callbacks.generate_code == 0
            || state.callbacks.on_prepared == 0
            || state.callbacks.control == 0
        {
            log::error!("JIT plugin does not implement all necessary functionality");
            return RESULT_UNKNOWN;
        }

        if !state.context.load_nro(nro) {
            log::error!("Failed to load JIT plugin");
            return RESULT_UNKNOWN;
        }

        let callbacks = state.callbacks;
        let configuration = state.configuration;
        state.context.map_process_memory(
            configuration.sys_ro_memory.offset,
            configuration.sys_ro_memory.size as usize,
        );
        state.context.map_process_memory(
            configuration.sys_rx_memory.offset,
            configuration.sys_rx_memory.size as usize,
        );
        state.context.map_process_memory(
            configuration.transfer_memory.offset,
            configuration.transfer_memory.size as usize,
        );

        if callbacks.rtld_init != 0 {
            state.context.call_function(callbacks.rtld_init, &[]);
        }

        let version = state.context.call_function(callbacks.get_version, &[]);
        if version > 1 {
            log::error!("Unknown JIT plugin version {version}");
            return RESULT_UNKNOWN;
        }

        let resolve = state.context.get_helper("_resolve");
        if callbacks.resolve_basic_symbols != 0 {
            state
                .context
                .call_function(callbacks.resolve_basic_symbols, &[resolve]);
        }

        let resolve_ptr = state.context.add_heap(resolve);
        if callbacks.setup_diagnostics != 0 {
            state
                .context
                .call_function(callbacks.setup_diagnostics, &[0, resolve_ptr]);
        }

        state.context.call_function(callbacks.configure, &[0]);
        let configuration_ptr = state.context.add_heap(configuration);
        state
            .context
            .call_function(callbacks.on_prepared, &[configuration_ptr]);
        RESULT_SUCCESS
    }

    fn get_code_address(&self) -> (u64, u64) {
        log::debug!("IJitEnvironment::GetCodeAddress called");
        let state = self.state.lock().unwrap();
        (
            state.configuration.user_rx_memory.offset,
            state.configuration.user_ro_memory.offset,
        )
    }

    fn read_output_buffer(ctx: &HLERequestContext, index: usize) -> Vec<u8> {
        let (address, size) = if let Some(descriptor) = ctx
            .buffer_descriptor_b()
            .get(index)
            .filter(|descriptor| descriptor.size() != 0)
        {
            (descriptor.address(), descriptor.size() as usize)
        } else if let Some(descriptor) = ctx.buffer_descriptor_c().get(index) {
            (descriptor.address(), descriptor.size() as usize)
        } else {
            return Vec::new();
        };

        let mut data = vec![0; size];
        if let Some(memory) = ctx.get_memory() {
            memory.lock().unwrap().read_block(address, &mut data);
        }
        data
    }

    fn generate_code_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let (data_size, command, range0, range1, data) = {
            let mut request = CmifRequest::new(ctx);
            let data_size = request.u32();
            request.align_for::<u64>();
            let command = request.u64();
            let range0 = request.raw::<CodeRange>();
            let range1 = request.raw::<CodeRange>();
            let data = request.raw::<Struct32>();
            (data_size, command, range0, range1, data)
        };
        let input_buffer = ctx.read_buffer(0);
        let mut output_buffer = Self::read_output_buffer(ctx, 0);
        let (result, return_value, out_range0, out_range1) = service.generate_code(
            data_size,
            command,
            range0,
            range1,
            data,
            &input_buffer,
            &mut output_buffer,
        );
        ctx.write_buffer(&output_buffer, 0);

        let mut response = ResponseBuilder::new(ctx, 12, 0, 0);
        response.push_result(result);
        response.push_i32(return_value);
        response.push_u32(0);
        response.push_raw(&out_range0);
        response.push_raw(&out_range1);
    }

    fn control_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let command = {
            let mut request = CmifRequest::new(ctx);
            request.u64()
        };
        let input_buffer = ctx.read_buffer(0);
        let mut output_buffer = Self::read_output_buffer(ctx, 0);
        let (result, return_value) = service.control(command, &input_buffer, &mut output_buffer);
        ctx.write_buffer(&output_buffer, 0);

        let mut response = ResponseBuilder::new(ctx, 3, 0, 0);
        response.push_result(result);
        response.push_i32(return_value);
    }

    fn resolve_transfer_memory(
        ctx: &HLERequestContext,
        handle: u32,
    ) -> Option<Arc<Mutex<KTransferMemory>>> {
        let process = ctx.owner_process_arc()?;
        let process = process.lock().unwrap();
        let object_id = process.handle_table.get_object(handle)?;
        process.get_transfer_memory_by_object_id(object_id)
    }

    fn load_plugin_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let transfer_memory_size = {
            let mut request = CmifRequest::new(ctx);
            request.u64()
        };
        let transfer_memory_handle = ctx.get_copy_handle(0);
        let nrr = ctx.read_buffer(0);
        let nro = ctx.read_buffer(1);
        let result = match Self::resolve_transfer_memory(ctx, transfer_memory_handle) {
            Some(transfer_memory) => {
                service.load_plugin(transfer_memory_size, transfer_memory, &nrr, &nro)
            }
            None => {
                log::error!("Invalid JIT transfer memory handle");
                RESULT_UNKNOWN
            }
        };
        let mut response = ResponseBuilder::new(ctx, 2, 0, 0);
        response.push_result(result);
    }

    fn get_code_address_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let (rx_offset, ro_offset) = service.get_code_address();
        let mut response = ResponseBuilder::new(ctx, 6, 0, 0);
        response.push_result(RESULT_SUCCESS);
        response.push_u64(rx_offset);
        response.push_u64(ro_offset);
    }
}

impl Drop for IJitEnvironment {
    fn drop(&mut self) {
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.user_rx.finalize();
        state.user_ro.finalize();
    }
}

impl SessionRequestHandler for IJitEnvironment {
    fn handle_sync_request(&self, context: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, context)
    }

    fn service_name(&self) -> &str {
        "IJitEnvironment"
    }
}

impl ServiceFramework for IJitEnvironment {
    fn get_service_name(&self) -> &str {
        "IJitEnvironment"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct JITU {
    system: SystemRef,
    generate_random: Mutex<Mt19937_64>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl JITU {
    fn as_self(this: &dyn ServiceFramework) -> &Self {
        unsafe { &*(this as *const dyn ServiceFramework as *const Self) }
    }

    pub fn new(system: SystemRef) -> Self {
        Self {
            system,
            generate_random: Mutex::new(Mt19937_64::default()),
            handlers: build_handler_map(&[(
                0,
                Some(Self::create_jit_environment_handler),
                "CreateJitEnvironment",
            )]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn resolve_process(&self, ctx: &HLERequestContext, handle: u32) -> Option<Arc<ProcessLock>> {
        let owner = ctx.owner_process_arc()?;
        if handle == PseudoHandle::CurrentProcess as u32 {
            return Some(owner);
        }

        let (object_id, owner_process_id) = {
            let owner = owner.lock().unwrap();
            (
                owner.handle_table.get_object(handle)?,
                owner.get_process_id(),
            )
        };
        if object_id == owner_process_id {
            return Some(owner);
        }
        self.system.get().kernel()?.get_process_by_id(object_id)
    }

    fn resolve_code_memory(
        ctx: &HLERequestContext,
        handle: u32,
    ) -> Option<Arc<Mutex<KCodeMemory>>> {
        let owner = ctx.owner_process_arc()?;
        let owner = owner.lock().unwrap();
        let object_id = owner.handle_table.get_object(handle)?;
        owner.get_code_memory_by_object_id(object_id)
    }

    fn create_jit_environment(
        &self,
        ctx: &HLERequestContext,
        rx_size: u64,
        ro_size: u64,
        process_handle: u32,
        rx_memory_handle: u32,
        ro_memory_handle: u32,
    ) -> Result<Arc<IJitEnvironment>, ResultCode> {
        let process = self.resolve_process(ctx, process_handle).ok_or_else(|| {
            log::error!("JIT process handle is null or invalid");
            RESULT_UNKNOWN
        })?;
        let rx_memory = Self::resolve_code_memory(ctx, rx_memory_handle).ok_or_else(|| {
            log::error!("JIT RX code-memory handle is null or invalid");
            RESULT_UNKNOWN
        })?;
        let ro_memory = Self::resolve_code_memory(ctx, ro_memory_handle).ok_or_else(|| {
            log::error!("JIT RO code-memory handle is null or invalid");
            RESULT_UNKNOWN
        })?;

        let mut rx = CodeMemory::new();
        let mut ro = CodeMemory::new();
        let mut random = self.generate_random.lock().unwrap();
        let mut generate_random = || random.next_u64();
        let result = rx.initialize(
            &process,
            &rx_memory,
            rx_size as usize,
            MemoryPermission::ReadExecute,
            &mut generate_random,
        );
        if result != RESULT_SUCCESS {
            return Err(result);
        }
        let result = ro.initialize(
            &process,
            &ro_memory,
            ro_size as usize,
            MemoryPermission::Read,
            &mut generate_random,
        );
        if result != RESULT_SUCCESS {
            return Err(result);
        }
        drop(generate_random);
        drop(random);

        IJitEnvironment::new(self.system, process, rx, ro)
            .map(Arc::new)
            .map_err(|error| {
                log::error!("Failed to create JIT environment: {error}");
                RESULT_UNKNOWN
            })
    }

    fn create_jit_environment_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let (rx_size, ro_size) = {
            let mut request = CmifRequest::new(ctx);
            (request.u64(), request.u64())
        };
        let process_handle = ctx.get_copy_handle(0);
        let rx_memory_handle = ctx.get_copy_handle(1);
        let ro_memory_handle = ctx.get_copy_handle(2);
        match service.create_jit_environment(
            ctx,
            rx_size,
            ro_size,
            process_handle,
            rx_memory_handle,
            ro_memory_handle,
        ) {
            Ok(environment) => {
                let mut response = ResponseBuilder::new(ctx, 2, 0, 1);
                response.push_result(RESULT_SUCCESS);
                response.push_ipc_interface(environment);
            }
            Err(result) => {
                let mut response = ResponseBuilder::new(ctx, 2, 0, 0);
                response.push_result(result);
            }
        }
    }
}

impl SessionRequestHandler for JITU {
    fn handle_sync_request(&self, context: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, context)
    }

    fn service_name(&self) -> &str {
        "jit:u"
    }
}

impl ServiceFramework for JITU {
    fn get_service_name(&self) -> &str {
        "jit:u"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub fn loop_process(system: SystemRef) {
    use crate::hle::service::hle_ipc::SessionRequestHandlerPtr;
    use crate::hle::service::server_manager::ServerManager;

    let server_manager = ServerManager::new_shared(system);
    let service: SessionRequestHandlerPtr = Arc::new(JITU::new(system));
    let service_for_factory = Arc::clone(&service);
    server_manager.lock().unwrap().register_named_service(
        "jit:u",
        Box::new(move || Arc::clone(&service_for_factory)),
        64,
    );
    ServerManager::run_server_shared(server_manager);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::System;
    use crate::hle::kernel::k_process::KProcess;
    use common::common_funcs::make_magic;

    fn minimal_nro(code_offset: usize, code: &[u32]) -> Vec<u8> {
        let mut data = vec![0; (code_offset + code.len() * 4).max(0x38)];
        data[4..8].copy_from_slice(&0x10u32.to_le_bytes());
        data[0x10..0x14].copy_from_slice(&make_magic(b'M', b'O', b'D', b'0').to_le_bytes());
        data[0x14..0x18].copy_from_slice(&8u32.to_le_bytes());
        for (index, instruction) in code.iter().enumerate() {
            let start = code_offset + index * 4;
            data[start..start + 4].copy_from_slice(&instruction.to_le_bytes());
        }
        data
    }

    fn environment() -> (Box<System>, Arc<IJitEnvironment>) {
        let mut system = Box::new(System::new_for_test());
        system.initialize();
        let mut process = KProcess::new();
        process.create_memory(&system);
        let process = Arc::new(ProcessLock::from_value(process));
        system.set_current_process_arc(Arc::clone(&process));
        let environment = IJitEnvironment::new(
            SystemRef::from_ref(system.as_ref()),
            process,
            CodeMemory::new(),
            CodeMemory::new(),
        )
        .unwrap();
        (system, Arc::new(environment))
    }

    #[test]
    fn binary_layouts_match_upstream() {
        assert_eq!(std::mem::size_of::<CodeRange>(), 16);
        assert_eq!(std::mem::align_of::<CodeRange>(), 8);
        assert_eq!(std::mem::size_of::<Struct32>(), 32);
        assert_eq!(std::mem::size_of::<JITConfiguration>(), 80);
    }

    #[test]
    fn random_engine_matches_std_mt19937_64_default_sequence() {
        let mut random = Mt19937_64::default();
        assert_eq!(random.next_u64(), 14_514_284_786_278_117_030);
        assert_eq!(random.next_u64(), 4_620_546_740_167_642_908);
        assert_eq!(random.next_u64(), 13_109_570_281_517_897_720);
    }

    #[test]
    fn services_register_all_upstream_commands() {
        let jitu = JITU::new(SystemRef::null());
        assert_eq!(jitu.handlers.keys().copied().collect::<Vec<_>>(), vec![0]);

        let (_system, environment) = environment();
        assert_eq!(
            environment.handlers.keys().copied().collect::<Vec<_>>(),
            vec![0, 1, 1000, 1001]
        );
    }

    #[test]
    fn generate_code_and_control_execute_resolved_callbacks() {
        // GenerateCode: RET. Control: MOV X0, #0; RET.
        let code = [0xD65F_03C0, 0xD280_0000, 0xD65F_03C0];
        let (_system, environment) = environment();
        {
            let mut state = environment.state.lock().unwrap();
            assert!(state.context.load_nro(&minimal_nro(0x40, &code)));
            state.callbacks.generate_code = 0x40;
            state.callbacks.control = 0x44;
        }

        let range0 = CodeRange {
            offset: 0x1000,
            size: 0x2000,
        };
        let range1 = CodeRange {
            offset: 0x4000,
            size: 0x3000,
        };
        let mut output = [0x55; 16];
        let (result, return_value, out_range0, out_range1) =
            environment.generate_code(32, 7, range0, range1, [1, 2, 3, 4], &[0xAA; 8], &mut output);
        assert_eq!(result, RESULT_SUCCESS);
        assert_eq!(return_value, 0);
        assert_eq!(out_range0, CodeRange { size: 0, ..range0 });
        assert_eq!(out_range1, CodeRange { size: 0, ..range1 });
        assert_eq!(output, [0x55; 16]);

        let (result, return_value) = environment.control(9, &[1, 2], &mut output);
        assert_eq!(result, RESULT_SUCCESS);
        assert_eq!(return_value, 0);
        assert_eq!(output, [0x55; 16]);
    }

    #[test]
    fn load_plugin_rejects_missing_required_callbacks() {
        let (_system, environment) = environment();
        let transfer_memory = Arc::new(Mutex::new(KTransferMemory::new()));
        let result = environment.load_plugin(
            0x1000,
            transfer_memory,
            &[],
            &minimal_nro(0x40, &[0xD65F_03C0]),
        );
        assert_eq!(result, RESULT_UNKNOWN);

        let state = environment.state.lock().unwrap();
        assert_eq!(state.configuration.transfer_memory.size, 0x1000);
        assert_eq!(state.callbacks.get_version, 0);
    }
}
