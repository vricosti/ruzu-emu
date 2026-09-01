// SPDX-FileCopyrightText: Copyright 2019 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/glue/glue.h and glue.cpp
//!
//! Upstream `Glue::LoopProcess(Core::System& system)` creates a ServerManager
//! and registers: arp:r, arp:w, bgtc:t, bgtc:sc, ectx:aw,
//! notif:a, notif:s, time:u, time:a, time:r

use std::sync::{Arc, Mutex};

use crate::hle::service::hle_ipc::SessionRequestHandlerFactory;
use crate::hle::service::server_manager::ServerManager;
use crate::hle::service::services::GenericStubService;
use crate::hle::service::sm::sm::ServiceManager;

/// Glue LoopProcess — launches all Glue services.
///
/// Matches upstream `void Glue::LoopProcess(Core::System& system)` (glue.cpp):
/// ```cpp
/// void LoopProcess(Core::System& system) {
///     auto server_manager = std::make_unique<ServerManager>(system);
///     server_manager->RegisterNamedService("arp:r", ...);
///     server_manager->RegisterNamedService("arp:w", ...);
///     server_manager->RegisterNamedService("bgtc:t", ...);
///     server_manager->RegisterNamedService("bgtc:sc", ...);
///     server_manager->RegisterNamedService("ectx:aw", ...);
///     server_manager->RegisterNamedService("notif:a", ...);
///     server_manager->RegisterNamedService("notif:s", ...);
///     // Time services
///     server_manager->RegisterNamedService("time:u", ...);
///     server_manager->RegisterNamedService("time:a", ...);
///     server_manager->RegisterNamedService("time:r", ...);
///     ServerManager::RunServer(std::move(server_manager));
/// }
/// ```
///
pub fn loop_process(service_manager: &Arc<Mutex<ServiceManager>>, system: crate::core::SystemRef) {
    let server_manager = ServerManager::new_shared(system);

    {
        let mut server_manager = server_manager.lock().unwrap();

        // ARP — stub until real implementations
        register_stub(&mut server_manager, "arp:r");
        register_stub(&mut server_manager, "arp:w");

        // BackGround Task Controller — stub
        register_stub(&mut server_manager, "bgtc:t");
        register_stub(&mut server_manager, "bgtc:sc");

        // Error Context
        use crate::hle::service::glue::ectx::{EctxAW, EctxR, EctxW};
        let factory: SessionRequestHandlerFactory = Box::new(|| Arc::new(EctxW::new()));
        server_manager.register_named_service("ectx:w", factory, 64);
        let factory: SessionRequestHandlerFactory = Box::new(|| Arc::new(EctxR::new()));
        server_manager.register_named_service("ectx:r", factory, 64);
        let factory: SessionRequestHandlerFactory = Box::new(|| Arc::new(EctxAW::new()));
        server_manager.register_named_service("ectx:aw", factory, 64);

        // Notification Services — stub
        register_stub(&mut server_manager, "notif:a");
        register_stub(&mut server_manager, "notif:s");
    }

    // Create the Glue::Time::TimeManager, matching upstream constructor.
    // This makes direct calls to time:m and set:sys via ServiceManager.
    use crate::hle::service::glue::time::manager::TimeManager;
    let time_manager = Arc::new(Mutex::new(TimeManager::new(
        service_manager.clone(),
        system,
    )));
    time_manager.lock().unwrap().initialize(service_manager);
    log::info!("Glue::LoopProcess: TimeManager initialized");

    // Time services — upstream creates a shared TimeManager and passes it
    // to each StaticService instance.
    {
        let mut server_manager = server_manager.lock().unwrap();
        use crate::hle::service::glue::time::r#static::StaticService as GlueTimeStaticService;
        use crate::hle::service::psc::time::common::StaticServiceSetupInfo;

        // time:u — user variant (all writes false)
        // Upstream: StaticServiceSetupInfo{0, 0, 0, 0, 0, 0}
        let user_setup = StaticServiceSetupInfo {
            can_write_local_clock: false,
            can_write_user_clock: false,
            can_write_network_clock: false,
            can_write_timezone_device_location: false,
            can_write_steady_clock: false,
            can_write_uninitialized_clock: false,
        };
        let time_manager_user = Arc::clone(&time_manager);
        let system_user = system;
        let factory: SessionRequestHandlerFactory = Box::new(move || {
            Arc::new(GlueTimeStaticService::new(
                system_user,
                user_setup,
                "time:u",
                Arc::clone(&time_manager_user),
            ))
        });
        server_manager.register_named_service("time:u", factory, 64);

        // time:a — admin variant
        // Upstream: StaticServiceSetupInfo{1, 1, 0, 1, 0, 0}
        let admin_setup = StaticServiceSetupInfo {
            can_write_local_clock: true,
            can_write_user_clock: true,
            can_write_network_clock: false,
            can_write_timezone_device_location: true,
            can_write_steady_clock: false,
            can_write_uninitialized_clock: false,
        };
        let time_manager_admin = Arc::clone(&time_manager);
        let system_admin = system;
        let factory: SessionRequestHandlerFactory = Box::new(move || {
            Arc::new(GlueTimeStaticService::new(
                system_admin,
                admin_setup,
                "time:a",
                Arc::clone(&time_manager_admin),
            ))
        });
        server_manager.register_named_service("time:a", factory, 64);

        // time:r — repair variant
        // Upstream: StaticServiceSetupInfo{0, 0, 0, 0, 1, 0}
        let repair_setup = StaticServiceSetupInfo {
            can_write_local_clock: false,
            can_write_user_clock: false,
            can_write_network_clock: false,
            can_write_timezone_device_location: false,
            can_write_steady_clock: true,
            can_write_uninitialized_clock: false,
        };
        let time_manager_repair = Arc::clone(&time_manager);
        let system_repair = system;
        let factory: SessionRequestHandlerFactory = Box::new(move || {
            Arc::new(GlueTimeStaticService::new(
                system_repair,
                repair_setup,
                "time:r",
                Arc::clone(&time_manager_repair),
            ))
        });
        server_manager.register_named_service("time:r", factory, 64);

        // time:s — upstream registers this in PSC::LoopProcess (psc.cpp),
        // not in Glue. Placed here temporarily until PSC is ported.
        // Upstream: StaticServiceSetupInfo{0, 0, 1, 0, 0, 0}
        let psc_setup = StaticServiceSetupInfo {
            can_write_local_clock: false,
            can_write_user_clock: false,
            can_write_network_clock: true,
            can_write_timezone_device_location: false,
            can_write_steady_clock: false,
            can_write_uninitialized_clock: false,
        };
        let time_manager_psc = Arc::clone(&time_manager);
        let system_psc = system;
        let factory: SessionRequestHandlerFactory = Box::new(move || {
            Arc::new(GlueTimeStaticService::new(
                system_psc,
                psc_setup,
                "time:s",
                Arc::clone(&time_manager_psc),
            ))
        });
        server_manager.register_named_service("time:s", factory, 64);
    }

    log::debug!(
        "Glue::LoopProcess: registered arp, bgtc, ectx, notif, time services \
         (time:s temporary, belongs to PSC)"
    );

    // Upstream: ServerManager::RunServer(std::move(server_manager));
    ServerManager::run_server_shared(server_manager);
}

/// Helper to register a stub service on a ServerManager.
fn register_stub(server_manager: &mut ServerManager, name: &str) {
    let svc_name = name.to_string();
    let factory: SessionRequestHandlerFactory =
        Box::new(move || Arc::new(GenericStubService::new(&svc_name)));
    server_manager.register_named_service(name, factory, 64);
}
