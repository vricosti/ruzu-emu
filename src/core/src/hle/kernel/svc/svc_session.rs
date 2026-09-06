//! Port of zuyu/src/core/hle/kernel/svc/svc_session.cpp
//! Status: Ported
//! Derniere synchro: 2026-03-20
//!
//! SVC handlers for session operations (CreateSession, AcceptSession).
//!
//! Upstream uses KSession/KLightSession kernel objects with resource reservation,
//! handle table operations, and reference counting. This port implements the
//! structure matching upstream but uses the existing KSession/KHandleTable
//! infrastructure available in ruzu.

use crate::core::System;
use crate::hle::kernel::k_resource_limit::LimitableResource;
use crate::hle::kernel::k_scoped_resource_reservation::KScopedResourceReservation;
use crate::hle::kernel::svc::svc_results::*;
use crate::hle::kernel::svc_common::Handle;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};

/// Creates a session (light or normal).
///
/// Upstream: CreateSession<KSession> or CreateSession<KLightSession> depending on is_light.
/// Both paths: reserve from resource limit, create, initialize, register, add server+client
/// to handle table.
pub fn create_session(
    system: &System,
    out_server: &mut Handle,
    out_client: &mut Handle,
    is_light: bool,
    _name: u64,
) -> ResultCode {
    let process_arc = system.current_process_arc();
    let mut process = process_arc.lock().unwrap();

    let mut session_reservation = KScopedResourceReservation::new(
        process.resource_limit.clone(),
        LimitableResource::SessionCountMax,
        1,
    );
    if !session_reservation.succeeded() {
        return RESULT_LIMIT_REACHED;
    }

    let Some(kernel) = system.kernel() else {
        return RESULT_INVALID_STATE;
    };
    let server_object_id = kernel.create_new_object_id() as u64;
    let client_object_id = kernel.create_new_object_id() as u64;
    let light_session_object_id = if is_light {
        Some(kernel.create_new_object_id() as u64)
    } else {
        None
    };

    if is_light {
        let light_session_object_id = light_session_object_id.unwrap();
        let light_session = std::sync::Arc::new(std::sync::Mutex::new(
            crate::hle::kernel::k_light_session::KLightSession::new(),
        ));
        let light_server_session = std::sync::Arc::new(std::sync::Mutex::new(
            crate::hle::kernel::k_light_server_session::KLightServerSession::new(),
        ));
        let light_client_session = std::sync::Arc::new(std::sync::Mutex::new(
            crate::hle::kernel::k_light_client_session::KLightClientSession::new(),
        ));
        {
            light_session.lock().unwrap().initialize_with_endpoints(
                None,
                _name as usize,
                server_object_id,
                client_object_id,
                Some(process.process_id),
            );
            light_server_session
                .lock()
                .unwrap()
                .initialize(light_session_object_id);
            light_client_session
                .lock()
                .unwrap()
                .initialize(light_session_object_id);
        }
        process.register_light_session_object(light_session_object_id, light_session);
        process.register_light_server_session_object(server_object_id, light_server_session);
        process.register_light_client_session_object(client_object_id, light_client_session);
    } else {
        // Create and initialize a KSession.
        let session = std::sync::Arc::new(std::sync::Mutex::new(
            crate::hle::kernel::k_session::KSession::new(),
        ));
        {
            let mut s = session.lock().unwrap();
            s.initialize(None, _name as usize);
            s.get_client_session()
                .lock()
                .unwrap()
                .initialize(server_object_id);
            s.get_server_session()
                .lock()
                .unwrap()
                .initialize(server_object_id);
        }
        let client_session = session.lock().unwrap().get_client_session().clone();
        process.register_session_object(server_object_id, session);
        process.register_client_session_object(client_object_id, client_session, server_object_id);
    }

    // Add server session to handle table.
    let server_handle = match process.handle_table.add(server_object_id) {
        Ok(h) => h,
        Err(_) => {
            if is_light {
                process
                    .light_server_session_objects
                    .remove(&server_object_id);
                process
                    .light_client_session_objects
                    .remove(&client_object_id);
                if let Some(light_session_object_id) = light_session_object_id {
                    process
                        .light_session_objects
                        .remove(&light_session_object_id);
                }
            } else {
                process.unregister_client_session_object_by_object_id(client_object_id);
                process.unregister_session_object_by_object_id(server_object_id);
            }
            return RESULT_OUT_OF_HANDLES;
        }
    };

    // Add client session to handle table.
    let client_handle = match process.handle_table.add(client_object_id) {
        Ok(h) => h,
        Err(_) => {
            // Clean up server handle on failure.
            process.handle_table.remove(server_handle);
            if is_light {
                process
                    .light_server_session_objects
                    .remove(&server_object_id);
                process
                    .light_client_session_objects
                    .remove(&client_object_id);
                if let Some(light_session_object_id) = light_session_object_id {
                    process
                        .light_session_objects
                        .remove(&light_session_object_id);
                }
            } else {
                process.unregister_client_session_object_by_object_id(client_object_id);
                process.unregister_session_object_by_object_id(server_object_id);
            }
            return RESULT_OUT_OF_HANDLES;
        }
    };

    *out_server = server_handle;
    *out_client = client_handle;
    if !is_light {
        if let Some(session) = process.get_session_by_object_id(server_object_id) {
            session.lock().unwrap().mark_session_resource_committed();
        }
    }
    session_reservation.commit();
    RESULT_SUCCESS
}

/// Accepts a session from a server port.
///
/// Upstream: Get server port from handle, reserve handle entry, accept session
/// (light or normal), register in handle table.
pub fn accept_session(system: &System, out: &mut Handle, port_handle: Handle) -> ResultCode {
    let process_arc = system.current_process_arc();
    let mut process = process_arc.lock().unwrap();

    // Get the server port object from the handle table.
    let object_id = match process.handle_table.get_object(port_handle) {
        Some(id) => id,
        None => {
            return RESULT_INVALID_HANDLE;
        }
    };

    let port = match process.get_server_port_by_object_id(object_id) {
        Some(port) => port,
        None => return RESULT_INVALID_HANDLE,
    };

    let is_light = port.lock().unwrap().is_light();
    let server_session_object_id = if is_light {
        crate::hle::kernel::k_server_port::KServerPort::accept_light_session_arc(&port)
    } else {
        crate::hle::kernel::k_server_port::KServerPort::accept_session_arc(&port)
    };
    let Some(server_session_object_id) = server_session_object_id else {
        return RESULT_NOT_FOUND;
    };
    let handle = match process.handle_table.add(server_session_object_id) {
        Ok(h) => h,
        Err(_) => return RESULT_OUT_OF_HANDLES,
    };

    *out = handle;
    RESULT_SUCCESS
}
