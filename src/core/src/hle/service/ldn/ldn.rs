// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/ldn/ldn.h
//! Port of zuyu/src/core/hle/service/ldn/ldn.cpp
//!
//! LoopProcess and internal service creator classes for LDN.
//! Registers: ldn:m, ldn:s, ldn:u, lp2p:app, lp2p:sys, lp2p:m

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// Service names registered by the LDN module.
pub const SERVICE_NAME_MONITOR: &str = "ldn:m";
pub const SERVICE_NAME_SYSTEM: &str = "ldn:s";
pub const SERVICE_NAME_USER: &str = "ldn:u";
pub const SERVICE_NAME_LP2P_APP: &str = "lp2p:app";
pub const SERVICE_NAME_LP2P_SYS: &str = "lp2p:sys";
pub const SERVICE_NAME_LP2P_MONITOR: &str = "lp2p:m";

/// IMonitorServiceCreator: "ldn:m" service.
///
/// | Cmd | Handler              | Name                 |
/// |-----|---------------------|----------------------|
/// | 0   | CreateMonitorService | CreateMonitorService |
pub struct IMonitorServiceCreator {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IMonitorServiceCreator {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[(
                0,
                Some(Self::create_monitor_service_handler),
                "CreateMonitorService",
            )]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn create_monitor_service_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("IMonitorServiceCreator::CreateMonitorService called");
        let service: Arc<dyn SessionRequestHandler> =
            Arc::new(crate::hle::service::ldn::monitor_service::IMonitorService::new());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(service);
    }
}

impl SessionRequestHandler for IMonitorServiceCreator {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        SERVICE_NAME_MONITOR
    }
}

impl ServiceFramework for IMonitorServiceCreator {
    fn get_service_name(&self) -> &str {
        SERVICE_NAME_MONITOR
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// ISystemServiceCreator: "ldn:s" service.
///
/// | Cmd | Handler                                | Name                                   |
/// |-----|---------------------------------------|----------------------------------------|
/// | 0   | CreateSystemLocalCommunicationService | CreateSystemLocalCommunicationService   |
pub struct ISystemServiceCreator {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ISystemServiceCreator {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[(
                0,
                Some(Self::create_system_local_communication_service_handler),
                "CreateSystemLocalCommunicationService",
            )]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn create_system_local_communication_service_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("ISystemServiceCreator::CreateSystemLocalCommunicationService called");
        let service: Arc<dyn SessionRequestHandler> = Arc::new(
            crate::hle::service::ldn::system_local_communication_service::ISystemLocalCommunicationService::new(),
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(service);
    }
}

impl SessionRequestHandler for ISystemServiceCreator {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        SERVICE_NAME_SYSTEM
    }
}

impl ServiceFramework for ISystemServiceCreator {
    fn get_service_name(&self) -> &str {
        SERVICE_NAME_SYSTEM
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// IUserServiceCreator: "ldn:u" service.
///
/// | Cmd | Handler                              | Name                                 |
/// |-----|-------------------------------------|--------------------------------------|
/// | 0   | CreateUserLocalCommunicationService | CreateUserLocalCommunicationService   |
pub struct IUserServiceCreator {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IUserServiceCreator {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[(
                0,
                Some(Self::create_user_local_communication_service_handler),
                "CreateUserLocalCommunicationService",
            )]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn create_user_local_communication_service_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("IUserServiceCreator::CreateUserLocalCommunicationService called");
        let service: Arc<dyn SessionRequestHandler> = Arc::new(
            crate::hle::service::ldn::user_local_communication_service::IUserLocalCommunicationService::new(),
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(service);
    }
}

impl SessionRequestHandler for IUserServiceCreator {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        SERVICE_NAME_USER
    }
}

impl ServiceFramework for IUserServiceCreator {
    fn get_service_name(&self) -> &str {
        SERVICE_NAME_USER
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// ISfServiceCreator: "lp2p:app" / "lp2p:sys" service.
///
/// | Cmd | Handler                   | Name                      |
/// |-----|--------------------------|---------------------------|
/// | 0   | CreateNetworkService     | CreateNetworkService      |
/// | 8   | CreateNetworkServiceMonitor | CreateNetworkServiceMonitor |
pub struct ISfServiceCreator {
    pub is_system: bool,
    service_name: &'static str,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ISfServiceCreator {
    pub fn new(is_system: bool, service_name: &'static str) -> Self {
        Self {
            is_system,
            service_name,
            handlers: build_handler_map(&[
                (
                    0,
                    Some(Self::create_network_service_handler),
                    "CreateNetworkService",
                ),
                (
                    8,
                    Some(Self::create_network_service_monitor_handler),
                    "CreateNetworkServiceMonitor",
                ),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn create_network_service_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let input = rp.pop_u32();
        let reserved_input = rp.pop_u64();
        log::warn!(
            "ISfServiceCreator::CreateNetworkService (STUBBED) called reserved_input={} input={}",
            reserved_input,
            input
        );
        let service: Arc<dyn SessionRequestHandler> =
            Arc::new(crate::hle::service::ldn::sf_service::ISfService::new());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(service);
    }

    fn create_network_service_monitor_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut rp = RequestParser::new(ctx);
        let reserved_input = rp.pop_u64();
        log::warn!(
            "ISfServiceCreator::CreateNetworkServiceMonitor (STUBBED) called reserved_input={}",
            reserved_input
        );
        let service: Arc<dyn SessionRequestHandler> =
            Arc::new(crate::hle::service::ldn::sf_service_monitor::ISfServiceMonitor::new());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(service);
    }
}

impl SessionRequestHandler for ISfServiceCreator {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        self.service_name
    }
}

impl ServiceFramework for ISfServiceCreator {
    fn get_service_name(&self) -> &str {
        self.service_name
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// ISfMonitorServiceCreator: "lp2p:m" service.
///
/// | Cmd | Handler              | Name                 |
/// |-----|---------------------|----------------------|
/// | 0   | CreateMonitorService | CreateMonitorService |
pub struct ISfMonitorServiceCreator {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ISfMonitorServiceCreator {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[(
                0,
                Some(Self::create_monitor_service_handler),
                "CreateMonitorService",
            )]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn create_monitor_service_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let reserved_input = rp.pop_u64();
        log::info!(
            "ISfMonitorServiceCreator::CreateMonitorService called, reserved_input={}",
            reserved_input
        );
        let service: Arc<dyn SessionRequestHandler> =
            Arc::new(crate::hle::service::ldn::sf_monitor_service::ISfMonitorService::new());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(service);
    }
}

impl SessionRequestHandler for ISfMonitorServiceCreator {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        SERVICE_NAME_LP2P_MONITOR
    }
}

impl ServiceFramework for ISfMonitorServiceCreator {
    fn get_service_name(&self) -> &str {
        SERVICE_NAME_LP2P_MONITOR
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// Entry point for the LDN service module.
///
/// Corresponds to `LDN::LoopProcess` in upstream ldn.cpp.
pub fn loop_process(system: crate::core::SystemRef) {
    use crate::hle::service::hle_ipc::SessionRequestHandlerPtr;
    use crate::hle::service::server_manager::ServerManager;

    let server_manager = ServerManager::new_shared(system);

    {
        let mut server_manager = server_manager.lock().unwrap();
        server_manager.register_named_service(
            SERVICE_NAME_MONITOR,
            Box::new(|| -> SessionRequestHandlerPtr { Arc::new(IMonitorServiceCreator::new()) }),
            64,
        );
        server_manager.register_named_service(
            SERVICE_NAME_SYSTEM,
            Box::new(|| -> SessionRequestHandlerPtr { Arc::new(ISystemServiceCreator::new()) }),
            64,
        );
        server_manager.register_named_service(
            SERVICE_NAME_USER,
            Box::new(|| -> SessionRequestHandlerPtr { Arc::new(IUserServiceCreator::new()) }),
            64,
        );
        server_manager.register_named_service(
            SERVICE_NAME_LP2P_APP,
            Box::new(|| -> SessionRequestHandlerPtr {
                Arc::new(ISfServiceCreator::new(false, SERVICE_NAME_LP2P_APP))
            }),
            64,
        );
        server_manager.register_named_service(
            SERVICE_NAME_LP2P_SYS,
            Box::new(|| -> SessionRequestHandlerPtr {
                Arc::new(ISfServiceCreator::new(true, SERVICE_NAME_LP2P_SYS))
            }),
            64,
        );
        server_manager.register_named_service(
            SERVICE_NAME_LP2P_MONITOR,
            Box::new(|| -> SessionRequestHandlerPtr { Arc::new(ISfMonitorServiceCreator::new()) }),
            64,
        );
    }
    ServerManager::run_server_shared(server_manager);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creator_handler_tables_match_upstream() {
        assert_eq!(IMonitorServiceCreator::new().handlers().len(), 1);
        assert_eq!(ISystemServiceCreator::new().handlers().len(), 1);
        assert_eq!(IUserServiceCreator::new().handlers().len(), 1);
        assert_eq!(
            ISfServiceCreator::new(false, SERVICE_NAME_LP2P_APP)
                .handlers()
                .len(),
            2
        );
        assert_eq!(ISfMonitorServiceCreator::new().handlers().len(), 1);
    }
}
