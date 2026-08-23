// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/pm/pm.h and pm.cpp
//!
//! Process Manager services.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::hle::result::{ErrorModule, ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{
    HLERequestContext, SessionRequestHandler,
};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

// --- PM result codes (matching upstream pm.cpp) ---

/// Upstream: `ResultProcessNotFound{ErrorModule::PM, 1}`
pub const RESULT_PROCESS_NOT_FOUND: ResultCode =
    ResultCode::from_module_description(ErrorModule::PM, 1);
/// Upstream: `ResultAlreadyStarted{ErrorModule::PM, 2}`
pub const RESULT_ALREADY_STARTED: ResultCode =
    ResultCode::from_module_description(ErrorModule::PM, 2);
/// Upstream: `ResultNotTerminated{ErrorModule::PM, 3}`
pub const RESULT_NOT_TERMINATED: ResultCode =
    ResultCode::from_module_description(ErrorModule::PM, 3);
/// Upstream: `ResultDebugHookInUse{ErrorModule::PM, 4}`
pub const RESULT_DEBUG_HOOK_IN_USE: ResultCode =
    ResultCode::from_module_description(ErrorModule::PM, 4);
/// Upstream: `ResultApplicationRunning{ErrorModule::PM, 5}`
pub const RESULT_APPLICATION_RUNNING: ResultCode =
    ResultCode::from_module_description(ErrorModule::PM, 5);
/// Upstream: `ResultInvalidSize{ErrorModule::PM, 6}`
pub const RESULT_INVALID_SIZE: ResultCode = ResultCode::from_module_description(ErrorModule::PM, 6);

/// Upstream: `NO_PROCESS_FOUND_PID`
const NO_PROCESS_FOUND_PID: u64 = 0;

/// SystemBootMode enum. Upstream: `SystemBootMode` in `pm.h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum SystemBootMode {
    Normal = 0,
    Maintenance = 1,
}

/// Minimal process info for the process list.
///
/// In upstream this uses `Kernel::KProcess` objects. Here we track the
/// essential fields needed by PM services.
#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub process_id: u64,
    pub program_id: u64,
    pub is_application: bool,
}

/// Shared process list. Upstream uses `Kernel::KernelCore::GetProcessList()`.
pub type ProcessList = Vec<ProcessInfo>;

fn process_list_from_kernel(system: crate::core::SystemRef) -> Option<ProcessList> {
    if system.is_null() {
        return None;
    }

    let kernel = system.get().kernel()?;
    Some(
        kernel
            .get_process_list()
            .into_iter()
            .map(|process| {
                let process = process.lock().unwrap();
                ProcessInfo {
                    process_id: process.get_process_id(),
                    program_id: process.get_program_id(),
                    is_application: process.is_application(),
                }
            })
            .collect(),
    )
}

/// Search the process list with a predicate.
///
/// Corresponds to upstream `SearchProcessList` template function.
fn search_process_list<F>(process_list: &ProcessList, predicate: F) -> Option<&ProcessInfo>
where
    F: Fn(&ProcessInfo) -> bool,
{
    process_list.iter().find(|p| predicate(p))
}

/// Get the application process ID from the process list.
///
/// Corresponds to upstream `GetApplicationPidGeneric`.
fn get_application_pid_generic(process_list: &ProcessList) -> u64 {
    match search_process_list(process_list, |p| p.is_application) {
        Some(p) => p.process_id,
        None => NO_PROCESS_FOUND_PID,
    }
}

/// Atmosphere process info structures used by AtmosphereGetProcessInfo.
///
/// Corresponds to upstream `ProgramLocation` in pm.cpp.
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct ProgramLocation {
    pub program_id: u64,
    pub storage_id: u8,
    pub _padding: [u8; 7],
}

const _: () = assert!(std::mem::size_of::<ProgramLocation>() == 0x10);

/// Corresponds to upstream `OverrideStatus` in pm.cpp.
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct OverrideStatus {
    pub keys_held: u64,
    pub flags: u64,
}

const _: () = assert!(std::mem::size_of::<OverrideStatus>() == 0x10);

// --- IPC command tables ---

/// IPC command IDs for BootMode ("pm:bm")
pub mod boot_mode_commands {
    pub const GET_BOOT_MODE: u32 = 0;
    pub const SET_MAINTENANCE_BOOT: u32 = 1;
}

/// IPC command IDs for DebugMonitor ("pm:dmnt")
pub mod debug_monitor_commands {
    pub const GET_JIT_DEBUG_PROCESS_ID_LIST: u32 = 0;
    pub const START_PROCESS: u32 = 1;
    pub const GET_PROCESS_ID: u32 = 2;
    pub const HOOK_TO_CREATE_PROCESS: u32 = 3;
    pub const GET_APPLICATION_PROCESS_ID: u32 = 4;
    pub const HOOK_TO_CREATE_APPLICATION_PROGRESS: u32 = 5;
    pub const CLEAR_HOOK: u32 = 6;
    pub const ATMOSPHERE_GET_PROCESS_INFO: u32 = 65000;
    pub const ATMOSPHERE_GET_CURRENT_LIMIT_INFO: u32 = 65001;
}

/// IPC command IDs for Info ("pm:info")
pub mod info_commands {
    pub const GET_PROGRAM_ID: u32 = 0;
    pub const ATMOSPHERE_GET_PROCESS_ID: u32 = 65000;
    pub const ATMOSPHERE_HAS_LAUNCHED_PROGRAM: u32 = 65001;
    pub const ATMOSPHERE_GET_PROCESS_INFO: u32 = 65002;
}

/// IPC command IDs for Shell ("pm:shell")
pub mod shell_commands {
    pub const LAUNCH_PROGRAM: u32 = 0;
    pub const TERMINATE_PROCESS: u32 = 1;
    pub const TERMINATE_PROGRAM: u32 = 2;
    pub const GET_PROCESS_EVENT_HANDLE: u32 = 3;
    pub const GET_PROCESS_EVENT_INFO: u32 = 4;
    pub const NOTIFY_BOOT_FINISHED: u32 = 5;
    pub const GET_APPLICATION_PROCESS_ID_FOR_SHELL: u32 = 6;
    pub const BOOST_SYSTEM_MEMORY_RESOURCE_LIMIT: u32 = 7;
    pub const BOOST_APPLICATION_THREAD_RESOURCE_LIMIT: u32 = 8;
    pub const GET_BOOT_FINISHED_EVENT_HANDLE: u32 = 9;
}

/// BootMode service ("pm:bm").
///
/// Corresponds to `BootMode` in upstream pm.cpp.
pub struct BootMode {
    boot_mode: AtomicU32,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl BootMode {
    pub fn new() -> Self {
        Self {
            boot_mode: AtomicU32::new(SystemBootMode::Normal as u32),
            handlers: build_handler_map(&[
                (
                    boot_mode_commands::GET_BOOT_MODE,
                    Some(BootMode::get_boot_mode_handler),
                    "GetBootMode",
                ),
                (
                    boot_mode_commands::SET_MAINTENANCE_BOOT,
                    Some(BootMode::set_maintenance_boot_handler),
                    "SetMaintenanceBoot",
                ),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// GetBootMode (cmd 0).
    ///
    /// Corresponds to `BootMode::GetBootMode` in upstream.
    pub fn get_boot_mode(&self) -> SystemBootMode {
        log::debug!("BootMode::GetBootMode called");
        match self.boot_mode.load(Ordering::Relaxed) {
            1 => SystemBootMode::Maintenance,
            _ => SystemBootMode::Normal,
        }
    }

    /// SetMaintenanceBoot (cmd 1).
    ///
    /// Corresponds to `BootMode::SetMaintenanceBoot` in upstream.
    pub fn set_maintenance_boot(&self) {
        log::debug!("BootMode::SetMaintenanceBoot called");
        self.boot_mode
            .store(SystemBootMode::Maintenance as u32, Ordering::Relaxed);
    }

    fn get_boot_mode_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const BootMode) };
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(service.get_boot_mode() as u32);
    }

    fn set_maintenance_boot_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const BootMode) };
        service.set_maintenance_boot();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }
}

impl SessionRequestHandler for BootMode {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "pm:bm"
    }
}

impl ServiceFramework for BootMode {
    fn get_service_name(&self) -> &str {
        "pm:bm"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// DebugMonitor service ("pm:dmnt").
///
/// Corresponds to `DebugMonitor` in upstream pm.cpp.
pub struct DebugMonitor {
    system: crate::core::SystemRef,
    process_list: ProcessList,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl DebugMonitor {
    pub fn new(system: crate::core::SystemRef) -> Self {
        Self {
            system,
            process_list: Vec::new(),
            handlers: debug_monitor_handlers(),
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// For test/integration purposes: provide a process list.
    pub fn with_process_list(process_list: ProcessList) -> Self {
        Self {
            system: crate::core::SystemRef::null(),
            process_list,
            handlers: debug_monitor_handlers(),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn process_list(&self) -> ProcessList {
        process_list_from_kernel(self.system).unwrap_or_else(|| self.process_list.clone())
    }

    /// GetProcessId (cmd 2).
    ///
    /// Corresponds to `DebugMonitor::GetProcessId` in upstream.
    pub fn get_process_id(&self, program_id: u64) -> Result<u64, ResultCode> {
        log::debug!(
            "DebugMonitor::GetProcessId called, program_id={:016X}",
            program_id
        );

        let process_list = self.process_list();
        match search_process_list(&process_list, |p| p.program_id == program_id) {
            Some(p) => Ok(p.process_id),
            None => Err(RESULT_PROCESS_NOT_FOUND),
        }
    }

    /// GetApplicationProcessId (cmd 4).
    ///
    /// Corresponds to `DebugMonitor::GetApplicationProcessId` in upstream.
    pub fn get_application_process_id(&self) -> u64 {
        log::debug!("DebugMonitor::GetApplicationProcessId called");
        let process_list = self.process_list();
        get_application_pid_generic(&process_list)
    }

    /// AtmosphereGetProcessInfo (cmd 65000).
    ///
    /// Partial implementation matching upstream.
    pub fn atmosphere_get_process_info(
        &self,
        pid: u64,
    ) -> Result<(ProgramLocation, OverrideStatus), ResultCode> {
        log::warn!(
            "DebugMonitor::AtmosphereGetProcessInfo (Partial Implementation) called, pid={:016X}",
            pid
        );

        let process_list = self.process_list();
        match search_process_list(&process_list, |p| p.process_id == pid) {
            Some(p) => {
                let program_location = ProgramLocation {
                    program_id: p.program_id,
                    storage_id: 0,
                    _padding: [0u8; 7],
                };
                let override_status = OverrideStatus {
                    keys_held: 0,
                    flags: 0,
                };
                Ok((program_location, override_status))
            }
            None => Err(RESULT_PROCESS_NOT_FOUND),
        }
    }

    fn get_process_id_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const DebugMonitor) };
        let mut rp = RequestParser::new(ctx);
        let program_id = rp.pop_u64();
        match service.get_process_id(program_id) {
            Ok(pid) => {
                let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_u64(pid);
            }
            Err(result) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(result);
            }
        }
    }

    fn get_application_process_id_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const DebugMonitor) };
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(service.get_application_process_id());
    }

    fn atmosphere_get_process_info_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const DebugMonitor) };
        let mut rp = RequestParser::new(ctx);
        let pid = rp.pop_u64();
        match service.atmosphere_get_process_info(pid) {
            Ok((program_location, override_status)) => {
                let mut rb = ResponseBuilder::new(ctx, 10, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_raw(&program_location);
                rb.push_raw(&override_status);
            }
            Err(result) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(result);
            }
        }
    }
}

fn debug_monitor_handlers() -> BTreeMap<u32, FunctionInfo> {
    build_handler_map(&[
        (
            debug_monitor_commands::GET_JIT_DEBUG_PROCESS_ID_LIST,
            None,
            "GetJitDebugProcessIdList",
        ),
        (debug_monitor_commands::START_PROCESS, None, "StartProcess"),
        (
            debug_monitor_commands::GET_PROCESS_ID,
            Some(DebugMonitor::get_process_id_handler),
            "GetProcessId",
        ),
        (
            debug_monitor_commands::HOOK_TO_CREATE_PROCESS,
            None,
            "HookToCreateProcess",
        ),
        (
            debug_monitor_commands::GET_APPLICATION_PROCESS_ID,
            Some(DebugMonitor::get_application_process_id_handler),
            "GetApplicationProcessId",
        ),
        (
            debug_monitor_commands::HOOK_TO_CREATE_APPLICATION_PROGRESS,
            None,
            "HookToCreateApplicationProgress",
        ),
        (debug_monitor_commands::CLEAR_HOOK, None, "ClearHook"),
        (
            debug_monitor_commands::ATMOSPHERE_GET_PROCESS_INFO,
            Some(DebugMonitor::atmosphere_get_process_info_handler),
            "AtmosphereGetProcessInfo",
        ),
        (
            debug_monitor_commands::ATMOSPHERE_GET_CURRENT_LIMIT_INFO,
            None,
            "AtmosphereGetCurrentLimitInfo",
        ),
    ])
}

impl SessionRequestHandler for DebugMonitor {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "pm:dmnt"
    }
}

impl ServiceFramework for DebugMonitor {
    fn get_service_name(&self) -> &str {
        "pm:dmnt"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// Info service ("pm:info").
///
/// Corresponds to `Info` in upstream pm.cpp.
pub struct Info {
    system: crate::core::SystemRef,
    process_list: ProcessList,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl Info {
    pub fn new(system: crate::core::SystemRef) -> Self {
        Self {
            system,
            process_list: Vec::new(),
            handlers: info_handlers(),
            handlers_tipc: BTreeMap::new(),
        }
    }

    pub fn with_process_list(process_list: ProcessList) -> Self {
        Self {
            system: crate::core::SystemRef::null(),
            process_list,
            handlers: info_handlers(),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn process_list(&self) -> ProcessList {
        process_list_from_kernel(self.system).unwrap_or_else(|| self.process_list.clone())
    }

    /// GetProgramId (cmd 0).
    ///
    /// Corresponds to `Info::GetProgramId` in upstream.
    pub fn get_program_id(&self, process_id: u64) -> Result<u64, ResultCode> {
        log::debug!("Info::GetProgramId called, process_id={:016X}", process_id);

        let process_list = self.process_list();
        match search_process_list(&process_list, |p| p.process_id == process_id) {
            Some(p) => Ok(p.program_id),
            None => Err(RESULT_PROCESS_NOT_FOUND),
        }
    }

    /// AtmosphereGetProcessId (cmd 65000).
    ///
    /// Corresponds to `Info::AtmosphereGetProcessId` in upstream.
    pub fn atmosphere_get_process_id(&self, program_id: u64) -> Result<u64, ResultCode> {
        log::debug!(
            "Info::AtmosphereGetProcessId called, program_id={:016X}",
            program_id
        );

        let process_list = self.process_list();
        match search_process_list(&process_list, |p| p.program_id == program_id) {
            Some(p) => Ok(p.process_id),
            None => Err(RESULT_PROCESS_NOT_FOUND),
        }
    }

    fn get_program_id_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Info) };
        let mut rp = RequestParser::new(ctx);
        let process_id = rp.pop_u64();
        match service.get_program_id(process_id) {
            Ok(program_id) => {
                let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_u64(program_id);
            }
            Err(result) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(result);
            }
        }
    }

    fn atmosphere_get_process_id_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Info) };
        let mut rp = RequestParser::new(ctx);
        let program_id = rp.pop_u64();
        match service.atmosphere_get_process_id(program_id) {
            Ok(pid) => {
                let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_u64(pid);
            }
            Err(result) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(result);
            }
        }
    }
}

fn info_handlers() -> BTreeMap<u32, FunctionInfo> {
    build_handler_map(&[
        (
            info_commands::GET_PROGRAM_ID,
            Some(Info::get_program_id_handler),
            "GetProgramId",
        ),
        (
            info_commands::ATMOSPHERE_GET_PROCESS_ID,
            Some(Info::atmosphere_get_process_id_handler),
            "AtmosphereGetProcessId",
        ),
        (
            info_commands::ATMOSPHERE_HAS_LAUNCHED_PROGRAM,
            None,
            "AtmosphereHasLaunchedProgram",
        ),
        (
            info_commands::ATMOSPHERE_GET_PROCESS_INFO,
            None,
            "AtmosphereGetProcessInfo",
        ),
    ])
}

impl SessionRequestHandler for Info {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "pm:info"
    }
}

impl ServiceFramework for Info {
    fn get_service_name(&self) -> &str {
        "pm:info"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// Shell service ("pm:shell").
///
/// Corresponds to `Shell` in upstream pm.cpp.
pub struct Shell {
    system: crate::core::SystemRef,
    process_list: ProcessList,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl Shell {
    pub fn new(system: crate::core::SystemRef) -> Self {
        Self {
            system,
            process_list: Vec::new(),
            handlers: shell_handlers(),
            handlers_tipc: BTreeMap::new(),
        }
    }

    pub fn with_process_list(process_list: ProcessList) -> Self {
        Self {
            system: crate::core::SystemRef::null(),
            process_list,
            handlers: shell_handlers(),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn process_list(&self) -> ProcessList {
        process_list_from_kernel(self.system).unwrap_or_else(|| self.process_list.clone())
    }

    /// GetApplicationProcessIdForShell (cmd 6).
    ///
    /// Corresponds to `Shell::GetApplicationProcessIdForShell` in upstream.
    pub fn get_application_process_id_for_shell(&self) -> u64 {
        log::debug!("Shell::GetApplicationProcessIdForShell called");
        let process_list = self.process_list();
        get_application_pid_generic(&process_list)
    }

    fn get_application_process_id_for_shell_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Shell) };
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(service.get_application_process_id_for_shell());
    }
}

fn shell_handlers() -> BTreeMap<u32, FunctionInfo> {
    build_handler_map(&[
        (shell_commands::LAUNCH_PROGRAM, None, "LaunchProgram"),
        (shell_commands::TERMINATE_PROCESS, None, "TerminateProcess"),
        (shell_commands::TERMINATE_PROGRAM, None, "TerminateProgram"),
        (
            shell_commands::GET_PROCESS_EVENT_HANDLE,
            None,
            "GetProcessEventHandle",
        ),
        (
            shell_commands::GET_PROCESS_EVENT_INFO,
            None,
            "GetProcessEventInfo",
        ),
        (
            shell_commands::NOTIFY_BOOT_FINISHED,
            None,
            "NotifyBootFinished",
        ),
        (
            shell_commands::GET_APPLICATION_PROCESS_ID_FOR_SHELL,
            Some(Shell::get_application_process_id_for_shell_handler),
            "GetApplicationProcessIdForShell",
        ),
        (
            shell_commands::BOOST_SYSTEM_MEMORY_RESOURCE_LIMIT,
            None,
            "BoostSystemMemoryResourceLimit",
        ),
        (
            shell_commands::BOOST_APPLICATION_THREAD_RESOURCE_LIMIT,
            None,
            "BoostApplicationThreadResourceLimit",
        ),
        (
            shell_commands::GET_BOOT_FINISHED_EVENT_HANDLE,
            None,
            "GetBootFinishedEventHandle",
        ),
    ])
}

impl SessionRequestHandler for Shell {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "pm:shell"
    }
}

impl ServiceFramework for Shell {
    fn get_service_name(&self) -> &str {
        "pm:shell"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// Registers "pm:bm", "pm:dmnt", "pm:info", "pm:shell" services.
///
/// Corresponds to `LoopProcess` in upstream `pm.cpp`.
pub fn loop_process(system: crate::core::SystemRef) {
    use crate::hle::service::hle_ipc::SessionRequestHandlerPtr;
    use crate::hle::service::server_manager::ServerManager;

    let server_manager = ServerManager::new_shared(system);

    {
        let mut server_manager = server_manager.lock().unwrap();
        server_manager.register_named_service(
            "pm:bm",
            Box::new(|| -> SessionRequestHandlerPtr { std::sync::Arc::new(BootMode::new()) }),
            16,
        );
        server_manager.register_named_service(
            "pm:dmnt",
            Box::new(move || -> SessionRequestHandlerPtr {
                std::sync::Arc::new(DebugMonitor::new(system))
            }),
            16,
        );
        server_manager.register_named_service(
            "pm:info",
            Box::new(move || -> SessionRequestHandlerPtr {
                std::sync::Arc::new(Info::new(system))
            }),
            16,
        );
        server_manager.register_named_service(
            "pm:shell",
            Box::new(move || -> SessionRequestHandlerPtr {
                std::sync::Arc::new(Shell::new(system))
            }),
            16,
        );
    }

    ServerManager::run_server_shared(server_manager);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_result_codes() {
        assert!(RESULT_PROCESS_NOT_FOUND.is_error());
        assert_eq!(RESULT_PROCESS_NOT_FOUND.get_module(), ErrorModule::PM);
        assert_eq!(RESULT_PROCESS_NOT_FOUND.get_description(), 1);
        assert!(RESULT_ALREADY_STARTED.is_error());
        assert_eq!(RESULT_ALREADY_STARTED.get_description(), 2);
    }

    fn make_test_process_list() -> ProcessList {
        vec![
            ProcessInfo {
                process_id: 100,
                program_id: 0x0100000000001000,
                is_application: true,
            },
            ProcessInfo {
                process_id: 200,
                program_id: 0x0100000000002000,
                is_application: false,
            },
        ]
    }

    #[test]
    fn test_boot_mode() {
        let bm = BootMode::new();
        assert_eq!(bm.get_boot_mode(), SystemBootMode::Normal);
        bm.set_maintenance_boot();
        assert_eq!(bm.get_boot_mode(), SystemBootMode::Maintenance);
    }

    #[test]
    fn test_debug_monitor_get_process_id() {
        let dm = DebugMonitor::with_process_list(make_test_process_list());
        assert_eq!(dm.get_process_id(0x0100000000001000), Ok(100));
        assert_eq!(dm.get_process_id(0x0100000000002000), Ok(200));
        assert_eq!(dm.get_process_id(0xDEAD), Err(RESULT_PROCESS_NOT_FOUND));
    }

    #[test]
    fn test_debug_monitor_get_application_process_id() {
        let dm = DebugMonitor::with_process_list(make_test_process_list());
        assert_eq!(dm.get_application_process_id(), 100);

        let dm_empty = DebugMonitor::new(crate::core::SystemRef::null());
        assert_eq!(dm_empty.get_application_process_id(), NO_PROCESS_FOUND_PID);
    }

    #[test]
    fn test_info_get_program_id() {
        let info = Info::with_process_list(make_test_process_list());
        assert_eq!(info.get_program_id(100), Ok(0x0100000000001000));
        assert_eq!(info.get_program_id(999), Err(RESULT_PROCESS_NOT_FOUND));
    }

    #[test]
    fn test_info_atmosphere_get_process_id() {
        let info = Info::with_process_list(make_test_process_list());
        assert_eq!(info.atmosphere_get_process_id(0x0100000000001000), Ok(100));
        assert_eq!(
            info.atmosphere_get_process_id(0xDEAD),
            Err(RESULT_PROCESS_NOT_FOUND)
        );
    }

    #[test]
    fn test_shell_get_application_pid() {
        let shell = Shell::with_process_list(make_test_process_list());
        assert_eq!(shell.get_application_process_id_for_shell(), 100);
    }
}
