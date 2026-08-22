// SPDX-FileCopyrightText: Copyright 2022 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/jit/jit.h
//! Port of zuyu/src/core/hle/service/jit/jit.cpp
//!
//! JIT service — "jit:u" service registration and IJitEnvironment.

use super::jit_code_memory::CodeMemory;
use super::jit_context::JitContext;

/// CodeRange — offset/size pair for JIT code memory regions.
///
/// Corresponds to `CodeRange` in upstream jit.cpp.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct CodeRange {
    pub offset: u64,
    pub size: u64,
}

/// GuestCallbacks — addresses of plugin callback functions resolved from the NRO.
///
/// Corresponds to the private `GuestCallbacks` struct in upstream IJitEnvironment.
#[derive(Debug, Clone, Copy, Default)]
struct GuestCallbacks {
    rtld_fini: u64,
    rtld_init: u64,
    control: u64,
    resolve_basic_symbols: u64,
    setup_diagnostics: u64,
    configure: u64,
    generate_code: u64,
    get_version: u64,
    keeper: u64,
    on_prepared: u64,
}

/// JITConfiguration — memory layout configuration passed to the JIT plugin.
///
/// Corresponds to the private `JITConfiguration` struct in upstream IJitEnvironment.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
struct JITConfiguration {
    user_rx_memory: CodeRange,
    user_ro_memory: CodeRange,
    transfer_memory: CodeRange,
    sys_rx_memory: CodeRange,
    sys_ro_memory: CodeRange,
}

/// LoopProcess — registers "jit:u" service.
///
/// Corresponds to `Service::JIT::LoopProcess` in upstream jit.cpp.
pub fn loop_process(system: crate::core::SystemRef) {
    use crate::hle::service::hle_ipc::SessionRequestHandlerPtr;
    use crate::hle::service::server_manager::ServerManager;

    let server_manager = ServerManager::new_shared(system);

    let stub = |sm: &mut ServerManager, name: &str| {
        let svc_name = name.to_string();
        sm.register_named_service(
            name,
            Box::new(move || -> SessionRequestHandlerPtr {
                std::sync::Arc::new(crate::hle::service::services::GenericStubService::new(
                    &svc_name,
                ))
            }),
            64,
        );
    };
    {
        let mut server_manager = server_manager.lock().unwrap();
        stub(&mut server_manager, "jit:u");
    }

    ServerManager::run_server_shared(server_manager);
}

/// IPC command table for IJitEnvironment.
pub mod jit_environment_commands {
    pub const GENERATE_CODE: u32 = 0;
    pub const CONTROL: u32 = 1;
    pub const LOAD_PLUGIN: u32 = 1000;
    pub const GET_CODE_ADDRESS: u32 = 1001;
}

/// IJitEnvironment — JIT execution environment.
///
/// Corresponds to `IJitEnvironment` in upstream jit.cpp.
///
/// Upstream fields:
///   process: KScopedAutoObject<KProcess> — the game process that owns the code memory.
///   user_rx: CodeMemory — read-execute code memory region mapped into the process.
///   user_ro: CodeMemory — read-only code memory region mapped into the process.
///   callbacks: GuestCallbacks — resolved plugin function addresses.
///   configuration: JITConfiguration — memory layout passed to plugin callbacks.
///   context: JITContext — the JIT execution context (Dynarmic A64 engine + local memory).
///
/// Blocked on KProcess/KCodeMemory handle passing and rdynarmic integration. The struct
/// fields are defined to match upstream layout; the methods that invoke plugin callbacks
/// require rdynarmic to actually execute ARM64 code.
pub struct IJitEnvironment {
    #[allow(dead_code)]
    user_rx: CodeMemory,
    #[allow(dead_code)]
    user_ro: CodeMemory,
    #[allow(dead_code)]
    callbacks: GuestCallbacks,
    #[allow(dead_code)]
    configuration: JITConfiguration,
    #[allow(dead_code)]
    context: JitContext,
}

impl IJitEnvironment {
    pub fn new(
        memory: std::sync::Arc<std::sync::Mutex<crate::memory::memory::Memory>>,
    ) -> Result<Self, String> {
        Ok(Self {
            user_rx: CodeMemory::new(),
            user_ro: CodeMemory::new(),
            callbacks: GuestCallbacks::default(),
            configuration: JITConfiguration::default(),
            context: JitContext::new(memory)?,
        })
    }
}
