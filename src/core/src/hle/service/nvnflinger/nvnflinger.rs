// SPDX-FileCopyrightText: Copyright 2021 yuzu Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/core/hle/service/nvnflinger/nvnflinger.h
//! Port of zuyu/src/core/hle/service/nvnflinger/nvnflinger.cpp
//!
//! Nvnflinger is the display compositor service. It manages the lifecycle
//! of the HOS binder driver, the surface flinger, and provides the main
//! service registration.

use std::sync::Arc;

use crate::hle::service::hle_ipc::SessionRequestHandler;
use crate::hle::service::server_manager::ServerManager;

use super::hos_binder_driver::IHosBinderDriver;
use super::hos_binder_driver_server::HosBinderDriverServer;
use super::surface_flinger::SurfaceFlinger;

/// Nvnflinger manages the display services infrastructure.
///
/// In upstream C++, Nvnflinger creates and owns:
/// - HosBinderDriverServer (binder registry)
/// - SurfaceFlinger (compositor)
/// - IHOSBinderDriver (service interface)
///
/// It registers "dispdrv" as a named service.
pub struct Nvnflinger {
    server: Arc<HosBinderDriverServer>,
    surface_flinger: Arc<SurfaceFlinger>,
    binder_driver: Arc<IHosBinderDriver>,
}

impl Nvnflinger {
    pub fn new(system: crate::core::SystemRef) -> Self {
        let server = HosBinderDriverServer::new();
        let surface_flinger = SurfaceFlinger::new(system, Arc::clone(&server));
        let binder_driver = Arc::new(IHosBinderDriver::new(
            Arc::clone(&server),
            Arc::clone(&surface_flinger),
        ));

        Self {
            server,
            surface_flinger,
            binder_driver,
        }
    }

    pub fn get_server(&self) -> &Arc<HosBinderDriverServer> {
        &self.server
    }

    pub fn get_surface_flinger(&self) -> &Arc<SurfaceFlinger> {
        &self.surface_flinger
    }

    pub fn get_binder_driver(&self) -> &Arc<IHosBinderDriver> {
        &self.binder_driver
    }
}

/// Matches upstream `Service::Nvnflinger::LoopProcess(Core::System& system)`.
pub fn loop_process(system: crate::core::SystemRef) {
    let nvnflinger = Arc::new(Nvnflinger::new(system));

    let server_manager = ServerManager::new_shared(system);
    let binder_driver = Arc::clone(nvnflinger.get_binder_driver());
    server_manager.lock().unwrap().register_named_service(
        "dispdrv",
        Box::new(move || -> Arc<dyn SessionRequestHandler> { binder_driver.clone() }),
        64,
    );
    // Upstream's native service stack destroys these owners on return. Guest
    // fibers can be abandoned at shutdown, so move the local owner into the
    // kernel's post-CPU cleanup (not another Arc clone on the fiber stack).
    system.get().kernel().expect("Nvnflinger requires a kernel")
        .retain_service_lifetime_owner(nvnflinger);
    ServerManager::run_server_shared(server_manager);
}
