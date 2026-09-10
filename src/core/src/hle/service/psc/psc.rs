// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/psc/psc.cpp/.h
//!
//! LoopProcess registers the following named services:
//!   psc:c     -> IPmControl
//!   psc:m     -> IPmService
//!   ovln:rcv  -> IReceiverService
//!   ovln:snd  -> ISenderService
//!   time:m    -> Time::ServiceManager
//!   time:su   -> Time::StaticService
//!   time:al   -> Time::IAlarmService

use std::collections::BTreeMap;

use crate::hle::result::ResultCode;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

pub struct PscL {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl PscL {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "Initialize_3"),
                (1, None, "Lock"),
                (2, None, "Unlock"),
                (3, None, "IsLocked"),
                (4, None, "GetRelatedState"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for PscL {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "psc:l"
    }
}

impl ServiceFramework for PscL {
    fn get_service_name(&self) -> &str {
        "psc:l"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct InsR {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl InsR {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "GetInputSourceState"),
                (1, None, "GetTriggerTargetEvent"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for InsR {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "ins:r"
    }
}

impl ServiceFramework for InsR {
    fn get_service_name(&self) -> &str {
        "ins:r"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct InsS {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl InsS {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[(0, None, "GetNotifyEvent")]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for InsS {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "ins:s"
    }
}

impl ServiceFramework for InsS {
    fn get_service_name(&self) -> &str {
        "ins:s"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct HshlSys {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl HshlSys {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "GetBatteryPercentage"),
                (1, None, "GetChargerType"),
                (2, None, "OpenChargeSession"),
                (3, None, "GetRawBatteryPercentage"),
                (4, None, "GetBatteryVoltageLevel"),
                (5, None, "OpenThermalSession"),
                (6, None, "GetAbnormalTemperatureSet"),
                (7, None, "OpenClockSession"),
                (8, None, "GetClockRate"),
                (9, None, "OpenBridgeSession"),
                (10, None, "GetBridgePowerSupply"),
                (11, None, "OpenVsysVoltageSession"),
                (12, None, "GetIsBatteryEnoughForFullAwake"),
                (13, None, "GetIsCharging"),
                (14, None, "Cmd14"),
                (15, None, "Cmd15"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for HshlSys {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "hshl:sys"
    }
}

impl ServiceFramework for HshlSys {
    fn get_service_name(&self) -> &str {
        "hshl:sys"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct HshlSet {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl HshlSet {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "OpenChargeSession_2"),
                (1, None, "OpenThermalSession_2"),
                (2, None, "SetClockRate"),
                (3, None, "SetBridgePowerSupply"),
                (4, None, "Cmd4"),
                (5, None, "Cmd5"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for HshlSet {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "hshl:set"
    }
}

impl ServiceFramework for HshlSet {
    fn get_service_name(&self) -> &str {
        "hshl:set"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub const PSC_SERVICE_NAMES: &[&str] = &[
    "psc:c", "psc:m", "psc:l", "ins:r", "ins:s", "hshl:sys", "hshl:set", "ovln:rcv", "ovln:snd",
    "time:m", "time:su", "time:al",
];

/// Register all PSC services.
///
/// Corresponds to upstream `PSC::LoopProcess` in `psc.cpp`.
pub fn loop_process(
    system: crate::core::SystemRef,
    device_memory: *const crate::device_memory::DeviceMemory,
    memory_manager: *mut crate::hle::kernel::k_memory_manager::KMemoryManager,
) {
    use std::sync::Arc;

    use crate::hle::service::hle_ipc::SessionRequestHandler;
    use crate::hle::service::server_manager::ServerManager;

    let server_manager = ServerManager::new_shared(system.clone());
    let time_server = Arc::downgrade(&server_manager);
    {
        let mut server_manager = server_manager.lock().unwrap();

        let stub = |sm: &mut ServerManager, name: &str| {
            let svc_name = name.to_string();
            sm.register_named_service(
                name,
                Box::new(move || -> Arc<dyn SessionRequestHandler> {
                    Arc::new(crate::hle::service::services::GenericStubService::new(
                        &svc_name,
                    ))
                }),
                64,
            );
        };

        stub(&mut server_manager, "psc:c");
        stub(&mut server_manager, "psc:m");
        server_manager.register_named_service(
            "psc:l",
            Box::new(|| -> Arc<dyn SessionRequestHandler> { Arc::new(PscL::new()) }),
            64,
        );
        server_manager.register_named_service(
            "ins:r",
            Box::new(|| -> Arc<dyn SessionRequestHandler> { Arc::new(InsR::new()) }),
            64,
        );
        server_manager.register_named_service(
            "ins:s",
            Box::new(|| -> Arc<dyn SessionRequestHandler> { Arc::new(InsS::new()) }),
            64,
        );
        server_manager.register_named_service(
            "hshl:sys",
            Box::new(|| -> Arc<dyn SessionRequestHandler> { Arc::new(HshlSys::new()) }),
            64,
        );
        server_manager.register_named_service(
            "hshl:set",
            Box::new(|| -> Arc<dyn SessionRequestHandler> { Arc::new(HshlSet::new()) }),
            64,
        );
        stub(&mut server_manager, "ovln:rcv");
        stub(&mut server_manager, "ovln:snd");

        let time_sm: Arc<dyn SessionRequestHandler> = Arc::new(
            crate::hle::service::psc::time::service_manager::TimeServiceManager::new_with_server_manager(
                system,
                device_memory,
                memory_manager,
                time_server,
            ),
        );
        let time_sm_factory = {
            let shared = Arc::clone(&time_sm);
            Box::new(move || -> Arc<dyn SessionRequestHandler> { Arc::clone(&shared) })
        };
        server_manager.register_named_service("time:m", time_sm_factory, 64);

        stub(&mut server_manager, "time:su");
        stub(&mut server_manager, "time:al");
    }

    ServerManager::run_server_shared(server_manager);
}
