// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/set/settings.h and settings.cpp
//!
//! Entry point for the settings service module.
//!
//! Registers the following named services:
//! - "set"     -> ISettingsServer
//! - "set:cal" -> IFactorySettingsServer
//! - "set:fd"  -> IFirmwareDebugSettingsServer
//! - "set:sys" -> ISystemSettingsServer

use super::factory_settings_server::IFactorySettingsServer;
use super::firmware_debug_settings_server::IFirmwareDebugSettingsServer;
use super::settings_server::ISettingsServer;
use crate::hle::service::hle_ipc::{SessionRequestHandlerFactory, SessionRequestHandlerPtr};
use std::sync::Arc;

/// Adapts Eden's single shared `ISystemSettingsServer` registration to Ruzu's
/// factory-based `ServerManager` API without creating per-session settings state.
fn make_system_settings_factory() -> SessionRequestHandlerFactory {
    let service = Arc::new(super::system_settings_server::SystemSettingsService::new());
    Box::new(move || -> SessionRequestHandlerPtr { service.clone() })
}

/// Registers "set", "set:cal", "set:fd", "set:sys" services.
///
/// Corresponds to `Set::LoopProcess` in upstream settings.cpp.
pub fn loop_process(system: crate::core::SystemRef) {
    use crate::hle::service::server_manager::ServerManager;

    let server_manager = ServerManager::new_shared(system);
    let settings = Arc::new(ISettingsServer::new());
    let factory_settings = Arc::new(IFactorySettingsServer::new());
    let firmware_debug_settings = Arc::new(IFirmwareDebugSettingsServer::new());

    {
        let mut server_manager = server_manager.lock().unwrap();

        // "set" -> ISettingsServer
        server_manager.register_named_service(
            "set",
            Box::new(move || -> SessionRequestHandlerPtr { settings.clone() }),
            60,
        );

        // "set:cal" -> IFactorySettingsServer
        server_manager.register_named_service(
            "set:cal",
            Box::new(move || -> SessionRequestHandlerPtr { factory_settings.clone() }),
            60,
        );

        // "set:fd" -> IFirmwareDebugSettingsServer
        server_manager.register_named_service(
            "set:fd",
            Box::new(move || -> SessionRequestHandlerPtr { firmware_debug_settings.clone() }),
            60,
        );

        // "set:sys" -> ISystemSettingsServer (via SystemSettingsService wrapper)
        server_manager.register_named_service("set:sys", make_system_settings_factory(), 60);
    }

    ServerManager::run_server_shared(server_manager);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_settings_factory_preserves_upstream_singleton_ownership() {
        std::thread::Builder::new()
            .name("set:sys singleton ownership test".to_string())
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let factory = make_system_settings_factory();
                let first = factory();
                let second = factory();

                assert!(Arc::ptr_eq(&first, &second));
                assert!(first
                    .as_any()
                    .is::<super::super::system_settings_server::SystemSettingsService>());
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
