// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/bpc/bpc.cpp
//!
//! BPC service ("bpc") and BPC_R service ("bpc:r").
//! All commands are stubs (nullptr in upstream).

use std::collections::BTreeMap;

use crate::hle::result::ResultCode;
use crate::hle::service::hle_ipc::{
    HLERequestContext, SessionRequestHandler, SessionRequestHandlerPtr,
};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command IDs for BPC service
pub mod bpc_commands {
    pub const SHUTDOWN_SYSTEM: u32 = 0;
    pub const REBOOT_SYSTEM: u32 = 1;
    pub const GET_WAKEUP_REASON: u32 = 2;
    pub const GET_SHUTDOWN_REASON: u32 = 3;
    pub const GET_AC_OK: u32 = 4;
    pub const GET_BOARD_POWER_CONTROL_EVENT: u32 = 5;
    pub const GET_SLEEP_BUTTON_STATE: u32 = 6;
    pub const GET_POWER_EVENT: u32 = 7;
    pub const CREATE_WAKEUP_TIMER: u32 = 8;
    pub const CANCEL_WAKEUP_TIMER: u32 = 9;
    pub const ENABLE_WAKEUP_TIMER_ON_DEVICE: u32 = 10;
    pub const CREATE_WAKEUP_TIMER_EX: u32 = 11;
    pub const GET_LAST_ENABLED_WAKEUP_TIMER_TYPE: u32 = 12;
    pub const CLEAN_ALL_WAKEUP_TIMERS: u32 = 13;
    pub const GET_POWER_BUTTON: u32 = 14;
    pub const SET_ENABLE_WAKEUP_TIMER: u32 = 15;
}

/// IPC command IDs for BPC_R service
pub mod bpc_r_commands {
    pub const GET_RTC_TIME: u32 = 0;
    pub const SET_RTC_TIME: u32 = 1;
    pub const GET_RTC_RESET_DETECTED: u32 = 2;
    pub const CLEAR_RTC_RESET_DETECTED: u32 = 3;
    pub const SET_UP_RTC_RESET_ON_SHUTDOWN: u32 = 4;
}

/// BPC service. All commands are unimplemented stubs in upstream.
pub struct BPC {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl BPC {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (bpc_commands::SHUTDOWN_SYSTEM, None, "ShutdownSystem"),
                (bpc_commands::REBOOT_SYSTEM, None, "RebootSystem"),
                (bpc_commands::GET_WAKEUP_REASON, None, "GetWakeupReason"),
                (bpc_commands::GET_SHUTDOWN_REASON, None, "GetShutdownReason"),
                (bpc_commands::GET_AC_OK, None, "GetAcOk"),
                (
                    bpc_commands::GET_BOARD_POWER_CONTROL_EVENT,
                    None,
                    "GetBoardPowerControlEvent",
                ),
                (
                    bpc_commands::GET_SLEEP_BUTTON_STATE,
                    None,
                    "GetSleepButtonState",
                ),
                (bpc_commands::GET_POWER_EVENT, None, "GetPowerEvent"),
                (bpc_commands::CREATE_WAKEUP_TIMER, None, "CreateWakeupTimer"),
                (bpc_commands::CANCEL_WAKEUP_TIMER, None, "CancelWakeupTimer"),
                (
                    bpc_commands::ENABLE_WAKEUP_TIMER_ON_DEVICE,
                    None,
                    "EnableWakeupTimerOnDevice",
                ),
                (
                    bpc_commands::CREATE_WAKEUP_TIMER_EX,
                    None,
                    "CreateWakeupTimerEx",
                ),
                (
                    bpc_commands::GET_LAST_ENABLED_WAKEUP_TIMER_TYPE,
                    None,
                    "GetLastEnabledWakeupTimerType",
                ),
                (
                    bpc_commands::CLEAN_ALL_WAKEUP_TIMERS,
                    None,
                    "CleanAllWakeupTimers",
                ),
                (bpc_commands::GET_POWER_BUTTON, None, "GetPowerButton"),
                (
                    bpc_commands::SET_ENABLE_WAKEUP_TIMER,
                    None,
                    "SetEnableWakeupTimer",
                ),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for BPC {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "bpc"
    }
}

impl ServiceFramework for BPC {
    fn get_service_name(&self) -> &str {
        "bpc"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// BPC_R service. All commands are unimplemented stubs in upstream.
pub struct BpcR {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl BpcR {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (bpc_r_commands::GET_RTC_TIME, None, "GetRtcTime"),
                (bpc_r_commands::SET_RTC_TIME, None, "SetRtcTime"),
                (
                    bpc_r_commands::GET_RTC_RESET_DETECTED,
                    None,
                    "GetRtcResetDetected",
                ),
                (
                    bpc_r_commands::CLEAR_RTC_RESET_DETECTED,
                    None,
                    "ClearRtcResetDetected",
                ),
                (
                    bpc_r_commands::SET_UP_RTC_RESET_ON_SHUTDOWN,
                    None,
                    "SetUpRtcResetOnShutdown",
                ),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for BpcR {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "bpc:r"
    }
}

impl ServiceFramework for BpcR {
    fn get_service_name(&self) -> &str {
        "bpc:r"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct BpcC {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl BpcC {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "ShutdownSystem"),
                (1, None, "RebootSystem"),
                (2, None, "GetWakeupReason"),
                (3, None, "GetShutdownReason"),
                (4, None, "GetAcOk"),
                (5, None, "GetPowerEvent"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for BpcC {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "bpc:c"
    }
}

impl ServiceFramework for BpcC {
    fn get_service_name(&self) -> &str {
        "bpc:c"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct BpcB {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl BpcB {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "GetSleepButtonState"),
                (1, None, "GetPowerButtonEvent"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for BpcB {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "bpc:b"
    }
}

impl ServiceFramework for BpcB {
    fn get_service_name(&self) -> &str {
        "bpc:b"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct BpcW {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl BpcW {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "CreateWakeupTimer"),
                (1, None, "CancelWakeupTimer"),
                (2, None, "EnableWakeupTimerOnDevice"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for BpcW {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "bpc:w"
    }
}

impl ServiceFramework for BpcW {
    fn get_service_name(&self) -> &str {
        "bpc:w"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// IPC command IDs for BPC_AMS service
pub mod bpc_ams_commands {
    pub const REBOOT_TO_FATAL_ERROR: u32 = 65000;
    pub const SET_REBOOT_PAYLOAD: u32 = 65001;
}

/// BPC_AMS service. All commands are unimplemented stubs in upstream.
pub struct BpcAms {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl BpcAms {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (
                    bpc_ams_commands::REBOOT_TO_FATAL_ERROR,
                    None,
                    "RebootToFatalError",
                ),
                (
                    bpc_ams_commands::SET_REBOOT_PAYLOAD,
                    None,
                    "SetRebootPayload",
                ),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for BpcAms {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "bpc:ams"
    }
}

impl ServiceFramework for BpcAms {
    fn get_service_name(&self) -> &str {
        "bpc:ams"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// Registers BPC services.
///
/// Corresponds to `LoopProcess` in upstream `bpc.cpp`:
/// ```cpp
/// server_manager->RegisterNamedService("bpc", std::make_shared<BPC>(system), 13);
/// server_manager->RegisterNamedService("bpc:r", std::make_shared<BPC_R>(system), 13);
/// server_manager->RegisterNamedService("bpc:c", std::make_shared<BPC_C>(system), 13);
/// server_manager->RegisterNamedService("bpc:b", std::make_shared<BPC_B>(system), 13);
/// server_manager->RegisterNamedService("bpc:w", std::make_shared<BPC_W>(system), 13);
/// server_manager->RegisterNamedService("bpc:ams", std::make_shared<BPC_AMS>(system), 4);
/// ```
pub fn loop_process(system: crate::core::SystemRef) {
    let server_manager = crate::hle::service::server_manager::ServerManager::new_shared(system);
    {
        let mut server_manager = server_manager.lock().unwrap();
        server_manager.register_named_service(
            "bpc",
            Box::new(|| -> SessionRequestHandlerPtr { std::sync::Arc::new(BPC::new()) }),
            13,
        );
        server_manager.register_named_service(
            "bpc:r",
            Box::new(|| -> SessionRequestHandlerPtr { std::sync::Arc::new(BpcR::new()) }),
            13,
        );
        server_manager.register_named_service(
            "bpc:c",
            Box::new(|| -> SessionRequestHandlerPtr { std::sync::Arc::new(BpcC::new()) }),
            13,
        );
        server_manager.register_named_service(
            "bpc:b",
            Box::new(|| -> SessionRequestHandlerPtr { std::sync::Arc::new(BpcB::new()) }),
            13,
        );
        server_manager.register_named_service(
            "bpc:w",
            Box::new(|| -> SessionRequestHandlerPtr { std::sync::Arc::new(BpcW::new()) }),
            13,
        );
        server_manager.register_named_service(
            "bpc:ams",
            Box::new(|| -> SessionRequestHandlerPtr { std::sync::Arc::new(BpcAms::new()) }),
            4,
        );
    }
    crate::hle::service::server_manager::ServerManager::run_server_shared(server_manager);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn homebrew_service_tables_match_upstream() {
        assert_eq!(BpcC::new().handlers().len(), 6);
        assert_eq!(BpcB::new().handlers().len(), 2);
        assert_eq!(BpcW::new().handlers().len(), 3);
    }

    #[test]
    fn bpc_ams_commands_match_upstream() {
        let service = BpcAms::new();
        let keys: Vec<u32> = service.handlers().keys().copied().collect();
        assert_eq!(keys, [65000, 65001]);
        assert_eq!(service.handlers()[&65000].name, "RebootToFatalError");
        assert_eq!(service.handlers()[&65001].name, "SetRebootPayload");
        assert_eq!(service.service_name(), "bpc:ams");
    }

    #[test]
    fn bpc_ams_register_service_uses_max_sessions_4() {
        use crate::hle::service::hle_ipc::SessionRequestHandlerFactory;
        use crate::hle::service::sm::sm::ServiceManager;
        use std::sync::Arc;

        let mut sm = ServiceManager::new();
        let factory: SessionRequestHandlerFactory =
            Box::new(|| -> SessionRequestHandlerPtr { Arc::new(BpcAms::new()) });
        let port = sm
            .register_service_with_port("bpc:ams".to_string(), 4, factory)
            .expect("register");
        assert_eq!(port.lock().unwrap().client.get_max_sessions(), 4);
        let handler = sm.get_service("bpc:ams").expect("handler");
        assert_eq!(handler.service_name(), "bpc:ams");
    }
}
