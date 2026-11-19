// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/vi/vi.cpp/.h
//!
//! VI service registration: creates a shared Container and registers
//! vi:m, vi:s, vi:u root services.

use std::sync::Arc;

use crate::hle::service::hle_ipc::SessionRequestHandler;

use super::application_root_service::IApplicationRootService;
use super::container::Container;
use super::manager_root_service::IManagerRootService;
use super::system_root_service::ISystemRootService;

pub const VI_SERVICE_NAMES: &[&str] = &["vi:m", "vi:s", "vi:u"];

// -- Registration --

/// Launches VI services.
///
/// Matches upstream `void VI::LoopProcess(Core::System& system, std::stop_token token)`.
/// Creates a shared Container, then registers vi:m, vi:s, vi:u.
pub fn loop_process(system: crate::core::SystemRef) {
    let container = Container::new(system);

    // Upstream keeps this callback alive on the VI service thread's stack and
    // calls Container::OnTerminate as soon as System requests shutdown, before
    // KernelCore::CloseServices waits for the server loop to exit.
    let container_for_stop = Arc::downgrade(&container);
    let _stop_callback = system.get().register_stop_callback(move || {
        if let Some(container) = container_for_stop.upgrade() {
            container.on_terminate();
        }
    });

    let server_manager = crate::hle::service::server_manager::ServerManager::new_shared(system);

    {
        let mut server_manager = server_manager.lock().unwrap();

        // vi:m — IManagerRootService (cmd 2 = GetDisplayService)
        let container_m = Arc::clone(&container);
        server_manager.register_named_service(
            "vi:m",
            Box::new(move || -> Arc<dyn SessionRequestHandler> {
                Arc::new(IManagerRootService::new(Arc::clone(&container_m)))
            }),
            64,
        );

        // vi:s — ISystemRootService (cmd 1 = GetDisplayService)
        let container_s = Arc::clone(&container);
        server_manager.register_named_service(
            "vi:s",
            Box::new(move || -> Arc<dyn SessionRequestHandler> {
                Arc::new(ISystemRootService::new(Arc::clone(&container_s)))
            }),
            64,
        );

        // vi:u — IApplicationRootService (cmd 0 = GetDisplayService)
        let container_u = Arc::clone(&container);
        server_manager.register_named_service(
            "vi:u",
            Box::new(move || -> Arc<dyn SessionRequestHandler> {
                Arc::new(IApplicationRootService::new(Arc::clone(&container_u)))
            }),
            64,
        );
    }

    // Preserve upstream's stack-owner destruction even if the guest fiber is
    // abandoned. Container retains the compositor and the NVDRV module; leaking
    // it also keeps NX2 address-space callbacks into the old System alive.
    system.get().kernel().expect("VI requires a kernel")
        .retain_service_lifetime_owner((container, _stop_callback));
    crate::hle::service::server_manager::ServerManager::run_server_shared(server_manager);
}

/// Backward-compatible alias.
pub fn register_services(system: crate::core::SystemRef) {
    loop_process(system);
}
