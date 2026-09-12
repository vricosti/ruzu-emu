// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/ns/ns.h
//! Port of zuyu/src/core/hle/service/ns/ns.cpp
//!
//! NS LoopProcess registers the following named services:
//!   ns:am2, ns:ec, ns:rid, ns:rt, ns:web, ns:ro -> IServiceGetterInterface
//!   ns:dev                                        -> IDevelopInterface
//!   ns:su                                         -> ISystemUpdateInterface
//!   ns:vm                                         -> IVulnerabilityManagerInterface
//!   pdm:qry                                       -> IQueryService
//!   pl:s, pl:u                                    -> IPlatformServiceManager

use std::collections::BTreeMap;

use crate::hle::result::ResultCode;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

pub struct INotifyService {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl INotifyService {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "NotifyAppletEvent"),
                (2, None, "NotifyOperationModeChangeEvent"),
                (3, None, "NotifyPowerStateChangeEvent"),
                (4, None, "NotifyClearAllEvent"),
                (5, None, "NotifyEventForDebug"),
                (6, None, "SuspendUserAccountEventService"),
                (7, None, "ResumeUserAccountEventService"),
                (8, None, "NotifyLibraryAppletEvent"),
                (9, None, "Cmd9"),
                (20, None, "Cmd20"),
                (30, None, "Cmd30"),
                (100, None, "Cmd100"),
                (101, None, "Cmd101"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for INotifyService {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }
    fn service_name(&self) -> &str {
        "pdm:ntfy"
    }
}
impl ServiceFramework for INotifyService {
    fn get_service_name(&self) -> &str {
        "pdm:ntfy"
    }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// Service names registered by NS LoopProcess.
///
/// Corresponds to the registrations in upstream ns.cpp `LoopProcess`.
pub const NS_SERVICE_GETTER_NAMES: &[&str] =
    &["ns:am2", "ns:ec", "ns:rid", "ns:rt", "ns:web", "ns:ro"];

/// LoopProcess — registers all NS services.
///
/// Corresponds to `Service::NS::LoopProcess` in upstream ns.cpp.
pub fn loop_process(system: crate::core::SystemRef) {
    use crate::hle::service::hle_ipc::SessionRequestHandlerPtr;
    use crate::hle::service::server_manager::ServerManager;
    use std::sync::Arc;

    log::debug!("NS::LoopProcess called");

    let server_manager = ServerManager::new_shared(system);

    {
        let mut server_manager = server_manager.lock().unwrap();

        // ns:am2, ns:ec, ns:rid, ns:rt, ns:web, ns:ro -> IServiceGetterInterface
        for &name in NS_SERVICE_GETTER_NAMES {
            server_manager.register_named_service(
                name,
                Box::new(move || -> SessionRequestHandlerPtr {
                    Arc::new(
                        super::service_getter_interface::IServiceGetterInterface::new(system, name),
                    )
                }),
                5,
            );
        }

        // ns:dev -> IDevelopInterface
        server_manager.register_named_service(
            "ns:dev",
            Box::new(|| -> SessionRequestHandlerPtr {
                Arc::new(super::develop_interface::IDevelopInterface::new())
            }),
            5,
        );

        // ns:su -> ISystemUpdateInterface
        server_manager.register_named_service(
            "ns:su",
            Box::new(|| -> SessionRequestHandlerPtr {
                Arc::new(super::system_update_interface::ISystemUpdateInterface::new())
            }),
            5,
        );

        // ns:vm -> IVulnerabilityManagerInterface
        server_manager.register_named_service(
            "ns:vm",
            Box::new(|| -> SessionRequestHandlerPtr {
                Arc::new(
                    super::vulnerability_manager_interface::IVulnerabilityManagerInterface::new(),
                )
            }),
            5,
        );

        // pdm:qry -> IQueryService
        server_manager.register_named_service(
            "pdm:qry",
            Box::new(|| -> SessionRequestHandlerPtr {
                Arc::new(super::query_service::IQueryService::new())
            }),
            64,
        );
        server_manager.register_named_service(
            "pdm:ntfy",
            Box::new(|| -> SessionRequestHandlerPtr { Arc::new(INotifyService::new()) }),
            64,
        );

        // pl:s -> IPlatformServiceManager
        let pl_s: Arc<dyn crate::hle::service::hle_ipc::SessionRequestHandler> =
            Arc::new(super::platform_service_manager::IPlatformServiceManager::new(system, "pl:s"));
        let pl_s_clone = Arc::clone(&pl_s);
        server_manager.register_named_service("pl:s", Box::new(move || pl_s_clone.clone()), 64);

        // pl:u -> IPlatformServiceManager
        let pl_u: Arc<dyn crate::hle::service::hle_ipc::SessionRequestHandler> =
            Arc::new(super::platform_service_manager::IPlatformServiceManager::new(system, "pl:u"));
        let pl_u_clone = Arc::clone(&pl_u);
        server_manager.register_named_service("pl:u", Box::new(move || pl_u_clone.clone()), 64);
    }

    ServerManager::run_server_shared(server_manager);
}
