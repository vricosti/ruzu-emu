// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/am/am.h
//! Port of zuyu/src/core/hle/service/am/am.cpp
//!
//! Entry point for the AM service. Registers "appletAE" and "appletOE"
//! named services.

use std::sync::{Arc, Mutex};

use crate::hle::service::hle_ipc::{SessionRequestHandlerFactory, SessionRequestHandlerPtr};
use crate::hle::service::server_manager::ServerManager;

use super::event_observer::EventObserver;
use super::window_system::WindowSystem;

/// Launches AM services.
///
/// Matches upstream `void AM::LoopProcess(Core::System& system)`.
/// In the C++ version, this creates a WindowSystem, ButtonPoller,
/// EventObserver, and registers the two named services.
pub fn loop_process(system: crate::core::SystemRef) {
    let server_manager = ServerManager::new_shared(system);

    // Upstream owns WindowSystem on the LoopProcess stack. The service objects
    // receive non-owning references, represented here by Weak.
    let window_system = Arc::new(Mutex::new(WindowSystem::new(system)));
    let window_system_ref = Arc::downgrade(&window_system);
    let window_system_ptr = {
        let mut guard = window_system.lock().unwrap();
        &mut *guard as *mut WindowSystem
    };
    let event_observer = Box::new(EventObserver::new(window_system_ptr as *const WindowSystem));

    {
        let mut guard = window_system.lock().unwrap();
        guard.set_event_observer(event_observer);
    }

    {
        let mut server_manager = server_manager.lock().unwrap();

        let ws = window_system_ref.clone();
        let system_oe = system;
        let factory_oe: SessionRequestHandlerFactory =
            Box::new(move || -> SessionRequestHandlerPtr {
                Arc::new(
                    super::service::application_proxy_service::IApplicationProxyService::new(
                        system_oe,
                        ws.clone(),
                    ),
                )
            });
        server_manager.register_named_service("appletOE", factory_oe, 64);

        let ws = window_system_ref.clone();
        let system_ae = system;
        let factory_ae: SessionRequestHandlerFactory = Box::new(
            move || -> SessionRequestHandlerPtr {
                Arc::new(
                    super::service::all_system_applet_proxies_service::IAllSystemAppletProxiesService::new(
                        system_ae,
                        ws.clone(),
                    ),
                )
            },
        );
        server_manager.register_named_service("appletAE", factory_ae, 64);
    }

    // Upstream reaches `AppletManager::SetWindowSystem(this)` from
    // `EventObserver -> WindowSystem::SetEventObserver()`. Rust keeps the same
    // owners but performs the blocking call here, after both AM services are
    // published and outside the `WindowSystem` mutex, to avoid deadlocking
    // service registration before the frontend provides the pending process.
    // This is also the Rust owner for the upstream blocking wait until the
    // frontend has provided `CreateAndInsertByFrontendAppletParameters`.
    system
        .get()
        .get_applet_manager()
        .set_window_system(Some(window_system.clone()));

    // Upstream keeps WindowSystem on this guest service thread's native stack.
    // A suspended cooperative Rust fiber cannot run stack-local destructors
    // when CpuManager releases its context, so transfer the sole strong owner
    // to KernelCore's explicit post-fiber service lifecycle.
    system
        .get()
        .kernel()
        .expect("AM service requires an initialized kernel")
        .retain_service_lifetime_owner(window_system);

    ServerManager::run_server_shared(server_manager);
}
