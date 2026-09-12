// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/ldr/ldr.cpp
//!
//! Loader services.

use std::collections::BTreeMap;

use crate::hle::result::ResultCode;
use crate::hle::service::hle_ipc::{
    HLERequestContext, SessionRequestHandler, SessionRequestHandlerPtr,
};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command IDs for DebugMonitor ("ldr:dmnt")
pub mod debug_monitor_commands {
    pub const SET_PROGRAM_ARGUMENT: u32 = 0;
    pub const FLUSH_ARGUMENTS: u32 = 1;
    pub const GET_PROCESS_MODULE_INFO: u32 = 2;
}

/// IPC command IDs for ProcessManager ("ldr:pm")
pub mod process_manager_commands {
    pub const CREATE_PROCESS: u32 = 0;
    pub const GET_PROGRAM_INFO: u32 = 1;
    pub const PIN_PROGRAM: u32 = 2;
    pub const UNPIN_PROGRAM: u32 = 3;
    pub const SET_ENABLED_PROGRAM_VERIFICATION: u32 = 4;
}

/// IPC command IDs for Shell ("ldr:shel")
pub mod shell_commands {
    pub const SET_PROGRAM_ARGUMENT: u32 = 0;
    pub const FLUSH_ARGUMENTS: u32 = 1;
}

pub struct DebugMonitor {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl DebugMonitor {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (
                    debug_monitor_commands::SET_PROGRAM_ARGUMENT,
                    None,
                    "SetProgramArgument",
                ),
                (
                    debug_monitor_commands::FLUSH_ARGUMENTS,
                    None,
                    "FlushArguments",
                ),
                (
                    debug_monitor_commands::GET_PROCESS_MODULE_INFO,
                    None,
                    "GetProcessModuleInfo",
                ),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for DebugMonitor {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "ldr:dmnt"
    }
}

impl ServiceFramework for DebugMonitor {
    fn get_service_name(&self) -> &str {
        "ldr:dmnt"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct ProcessManager {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (
                    process_manager_commands::CREATE_PROCESS,
                    None,
                    "CreateProcess",
                ),
                (
                    process_manager_commands::GET_PROGRAM_INFO,
                    None,
                    "GetProgramInfo",
                ),
                (process_manager_commands::PIN_PROGRAM, None, "PinProgram"),
                (
                    process_manager_commands::UNPIN_PROGRAM,
                    None,
                    "UnpinProgram",
                ),
                (
                    process_manager_commands::SET_ENABLED_PROGRAM_VERIFICATION,
                    None,
                    "SetEnabledProgramVerification",
                ),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for ProcessManager {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "ldr:pm"
    }
}

impl ServiceFramework for ProcessManager {
    fn get_service_name(&self) -> &str {
        "ldr:pm"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct Shell {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl Shell {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (
                    shell_commands::SET_PROGRAM_ARGUMENT,
                    None,
                    "SetProgramArgument",
                ),
                (shell_commands::FLUSH_ARGUMENTS, None, "FlushArguments"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for Shell {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "ldr:shel"
    }
}

impl ServiceFramework for Shell {
    fn get_service_name(&self) -> &str {
        "ldr:shel"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// Registers "ldr:pm", "ldr:shel", "ldr:dmnt" services.
///
/// Corresponds to `LoopProcess` in upstream `ldr.cpp`.
pub fn loop_process(system: crate::core::SystemRef) {
    use crate::hle::service::server_manager::ServerManager;

    let server_manager = ServerManager::new_shared(system);

    {
        let mut server_manager = server_manager.lock().unwrap();
        server_manager.register_named_service(
            "ldr:dmnt",
            Box::new(|| -> SessionRequestHandlerPtr { std::sync::Arc::new(DebugMonitor::new()) }),
            3,
        );
        server_manager.register_named_service(
            "ldr:pm",
            Box::new(|| -> SessionRequestHandlerPtr { std::sync::Arc::new(ProcessManager::new()) }),
            1,
        );
        server_manager.register_named_service(
            "ldr:shel",
            Box::new(|| -> SessionRequestHandlerPtr { std::sync::Arc::new(Shell::new()) }),
            3,
        );
    }

    ServerManager::run_server_shared(server_manager);
}
