// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/bcat/bcat.h
//! Port of zuyu/src/core/hle/service/bcat/bcat.cpp
//!
//! Registers BCAT and News services.

/// Registers BCAT services with the server manager.
///
/// Corresponds to `LoopProcess` in upstream `bcat.cpp`.
/// Services registered:
///   bcat:a, bcat:m, bcat:u, bcat:s
///   news:a (permissions=0xffffffff), news:p (0x1), news:c (0x2), news:v (0x4), news:m (0xd)
pub fn loop_process(system: crate::core::SystemRef) {
    use crate::hle::service::hle_ipc::SessionRequestHandlerPtr;
    use crate::hle::service::server_manager::ServerManager;
    use std::sync::Arc;

    log::debug!("BCAT::LoopProcess - registering bcat and news services");

    let server_manager = ServerManager::new_shared(system);

    {
        let mut server_manager = server_manager.lock().unwrap();

        // bcat:a, bcat:m, bcat:u, bcat:s -> IServiceCreator
        for &name in &["bcat:a", "bcat:m", "bcat:u", "bcat:s"] {
            let n = name.to_string();
            server_manager.register_named_service(
                name,
                Box::new(move || -> SessionRequestHandlerPtr {
                    Arc::new(super::service_creator::IServiceCreator::new(system, &n))
                }),
                64,
            );
        }

        // news:a (0xffffffff), news:p (0x1), news:c (0x2), news:v (0x4), news:m (0xd)
        for &(name, perms) in &[
            ("news:a", 0xffffffff_u32),
            ("news:p", 0x1),
            ("news:c", 0x2),
            ("news:v", 0x4),
            ("news:m", 0xd),
        ] {
            let n = name.to_string();
            server_manager.register_named_service(
                name,
                Box::new(move || -> SessionRequestHandlerPtr {
                    Arc::new(super::news::service_creator::IServiceCreator::new(
                        perms, &n,
                    ))
                }),
                64,
            );
        }
    }

    ServerManager::run_server_shared(server_manager);
}
