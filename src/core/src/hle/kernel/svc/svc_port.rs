//! Port of zuyu/src/core/hle/kernel/svc/svc_port.cpp
//! Status: COMPLET
//! Derniere synchro: 2026-03-20
//!
//! SVC handlers for port operations (ConnectToNamedPort, CreatePort, ConnectToPort, ManageNamedPort).
//!
//! CreatePort / ConnectToPort / ManageNamedPort / ConnectToNamedPort now use
//! real port object registries instead of placeholder handles, but named-port
//! lookup still resolves the `KObjectName` object id through `KernelCore`'s
//! process registries rather than a literal typed auto-object lookup.

use std::sync::{Arc, Mutex};

use crate::core::System;
use crate::hle::kernel::k_port::KPort;
use crate::hle::kernel::svc::svc_results::*;
use crate::hle::kernel::svc_common::{Handle, INVALID_HANDLE};
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::SessionRequestManager;

const PORT_NAME_MAX_LENGTH: usize = 12;

/// Read a null-terminated port name from guest memory.
/// Matches upstream `ReadCString(user_name, KObjectName::NameLengthMax)` +
/// `R_UNLESS(name[sizeof(name) - 1] == '\x00', ResultOutOfRange)`.
fn read_port_name(system: &System, user_name: u64) -> Result<String, ResultCode> {
    // Upstream behavior:
    //   1. ReadCString(user_name, NameLengthMax) — reads a null-terminated string
    //   2. std::array<char, NameLengthMax> name{} — zero-initialized buffer
    //   3. strncpy(name.data(), string_name.c_str(), NameLengthMax - 1) — copy up to 11 chars
    //   4. R_UNLESS(name[sizeof(name) - 1] == '\x00', ResultOutOfRange)
    //
    // The key is that upstream reads a C string (stopping at null), then copies it
    // into a zero-initialized buffer. So name[11] is always 0 for strings <= 11 chars.
    // We must NOT read raw 12 bytes from guest memory — bytes after the null terminator
    // may be non-zero garbage.

    // Step 1: Read null-terminated string from guest memory (up to NameLengthMax bytes).
    let mut raw = [0u8; PORT_NAME_MAX_LENGTH];
    let mut string_len = PORT_NAME_MAX_LENGTH;
    if let Some(memory) = system.get_svc_memory() {
        let m = memory.lock().unwrap();
        for i in 0..PORT_NAME_MAX_LENGTH {
            let b = m.read_8(user_name + i as u64);
            raw[i] = b;
            if b == 0 {
                string_len = i;
                break;
            }
        }
    } else {
        let mem = system.shared_process_memory().read().unwrap();
        for i in 0..PORT_NAME_MAX_LENGTH {
            let b = mem.read_8(user_name + i as u64);
            raw[i] = b;
            if b == 0 {
                string_len = i;
                break;
            }
        }
    }

    // Step 2: Copy into zero-initialized buffer (like upstream strncpy into name{}).
    let mut name = [0u8; PORT_NAME_MAX_LENGTH];
    let copy_len = string_len.min(PORT_NAME_MAX_LENGTH - 1);
    name[..copy_len].copy_from_slice(&raw[..copy_len]);

    // Step 3: Validate — upstream: R_UNLESS(name[sizeof(name) - 1] == '\x00', ResultOutOfRange)
    // This catches strings that are exactly NameLengthMax (12) chars with no null terminator.
    if name[PORT_NAME_MAX_LENGTH - 1] != 0 {
        return Err(RESULT_OUT_OF_RANGE);
    }

    let len = name
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(PORT_NAME_MAX_LENGTH);
    Ok(String::from_utf8_lossy(&name[..len]).to_string())
}

pub fn connect_to_named_port(system: &System, out: &mut Handle, user_name: u64) -> ResultCode {
    let name = match read_port_name(system, user_name) {
        Ok(n) => n,
        Err(rc) => return rc,
    };
    log::info!("  ConnectToNamedPort(\"{}\")", name);

    // Find the named port via KObjectName registry (matching upstream
    // KObjectName::Find<KClientPort>(kernel, name)).
    let kernel = system.kernel().expect("kernel not initialized");
    let service_manager = system.service_manager().unwrap();

    let Some(object_name_data) = kernel.object_name_global_data() else {
        log::error!("  ConnectToNamedPort: kernel object name registry unavailable");
        return RESULT_NOT_FOUND;
    };

    let Some(named_object_id) = object_name_data.find(&name).map(|id| id as u64) else {
        log::error!("  ConnectToNamedPort: service \"{}\" not found", name);
        return RESULT_NOT_FOUND;
    };

    let Some(port) = kernel.get_client_port_by_object_id(named_object_id) else {
        log::error!(
            "  ConnectToNamedPort: named object \"{}\" does not resolve to a client port",
            name
        );
        return RESULT_NOT_FOUND;
    };

    let handler = service_manager.lock().unwrap().get_service(&name);
    let process_arc = system.current_process_arc();
    let mut process = process_arc.lock().unwrap();
    let reserved_handle = match process.handle_table.reserve() {
        Ok(handle) => handle,
        Err(_) => return RESULT_OUT_OF_HANDLES,
    };

    let (session_object_id, client_session_object_id, server_session) = {
        let port_guard = port.lock().unwrap();
        if port_guard.is_light() {
            process.handle_table.unreserve(reserved_handle);
            log::warn!(
                "  ConnectToNamedPort(\"{}\"): light ports not yet ported",
                name
            );
            return RESULT_INVALID_HANDLE;
        }

        let (session_object_id, client_session_object_id) = match port_guard.client.create_session(
            &mut process,
            kernel,
            Some(named_object_id),
            port_guard.get_name(),
        ) {
            Ok(ids) => ids,
            Err(result) => {
                process.handle_table.unreserve(reserved_handle);
                return result;
            }
        };

        let server_session = process
            .get_server_session_by_object_id(session_object_id)
            .expect("created server session must be registered");

        drop(port_guard);
        let enqueue_result = KPort::enqueue_session_arc(&port, session_object_id);
        if enqueue_result.is_error() {
            process.handle_table.unreserve(reserved_handle);
            process.unregister_client_session_object_by_object_id(client_session_object_id);
            process.unregister_session_object_by_object_id(session_object_id);
            return enqueue_result;
        }

        (session_object_id, client_session_object_id, server_session)
    };

    if let Some(handler) = handler {
        // Look up the owning ServerManager's endpoints (queue + wakeup +
        // server_manager Weak) so the new session's SessionRequestManager can
        // propagate them to descendants. Without this, descendants would have
        // no way to register themselves via the queue and would either
        // deadlock (if they tried to lock `Mutex<ServerManager>`) or remain
        // out of the host thread's multi_wait.
        let ownership = service_manager
            .lock()
            .unwrap()
            .get_service_ownership(&name)
            .cloned();
        let manager = if let Some(ownership) = ownership {
            let queue = ownership.queue;
            let wakeup = ownership.wakeup;
            Arc::new(Mutex::new(
                SessionRequestManager::from_parent_server_manager_endpoints(
                    ownership.server_manager.upgrade(),
                    Some(queue),
                    Some(wakeup),
                ),
            ))
        } else {
            Arc::new(Mutex::new(SessionRequestManager::new()))
        };
        manager.lock().unwrap().set_session_handler(handler);
        if let Some(client_session) =
            process.get_client_session_by_object_id(client_session_object_id)
        {
            client_session.lock().unwrap().initialize(session_object_id);
        }
        server_session.lock().unwrap().set_manager(manager.clone());
    }

    if !process
        .handle_table
        .register(reserved_handle, client_session_object_id)
    {
        process.handle_table.unreserve(reserved_handle);
        process.unregister_client_session_object_by_object_id(client_session_object_id);
        process.unregister_session_object_by_object_id(session_object_id);
        return RESULT_OUT_OF_HANDLES;
    }

    *out = reserved_handle;
    log::info!(
        "  ConnectToNamedPort(\"{}\") -> success, handle={:#x}",
        name,
        reserved_handle
    );
    RESULT_SUCCESS
}

pub fn create_port(
    system: &System,
    out_server: &mut Handle,
    out_client: &mut Handle,
    max_sessions: i32,
    is_light: bool,
    name: u64,
) -> ResultCode {
    // Upstream: R_UNLESS(max_sessions > 0, ResultOutOfRange)
    if max_sessions <= 0 {
        return RESULT_OUT_OF_RANGE;
    }

    let kernel = system.kernel().expect("kernel not initialized");

    let port = Arc::new(Mutex::new(KPort::new()));
    port.lock()
        .unwrap()
        .initialize(max_sessions, is_light, name as usize);

    let client_port_object_id = kernel.create_new_object_id() as u64;
    let server_port_object_id = kernel.create_new_object_id() as u64;

    let process_arc = system.current_process_arc();
    let mut process = process_arc.lock().unwrap();
    process.register_client_port_object(client_port_object_id, port.clone());
    process.register_server_port_object(server_port_object_id, port);
    kernel.register_kernel_object(client_port_object_id);
    kernel.register_kernel_object(server_port_object_id);

    match process.handle_table.add(client_port_object_id) {
        Ok(handle) => *out_client = handle,
        Err(_) => {
            process.unregister_client_port_object_by_object_id(client_port_object_id);
            process.unregister_server_port_object_by_object_id(server_port_object_id);
            kernel.unregister_kernel_object(client_port_object_id);
            kernel.unregister_kernel_object(server_port_object_id);
            return RESULT_OUT_OF_HANDLES;
        }
    }

    match process.handle_table.add(server_port_object_id) {
        Ok(handle) => *out_server = handle,
        Err(_) => {
            // Clean up on failure — upstream: ON_RESULT_FAILURE { handle_table.Remove(*out_client); }
            process.handle_table.remove(*out_client);
            process.unregister_client_port_object_by_object_id(client_port_object_id);
            process.unregister_server_port_object_by_object_id(server_port_object_id);
            kernel.unregister_kernel_object(client_port_object_id);
            kernel.unregister_kernel_object(server_port_object_id);
            return RESULT_OUT_OF_HANDLES;
        }
    }

    RESULT_SUCCESS
}

pub fn connect_to_port(system: &System, out: &mut Handle, port: Handle) -> ResultCode {
    log::debug!("svc::ConnectToPort called, port_handle=0x{:08X}", port);

    enum CreatedPortSession {
        Normal {
            session_object_id: u64,
            client_session_object_id: u64,
        },
        Light {
            light_session_object_id: u64,
            server_session_object_id: u64,
            client_session_object_id: u64,
        },
    }

    impl CreatedPortSession {
        fn client_session_object_id(&self) -> u64 {
            match *self {
                Self::Normal {
                    client_session_object_id,
                    ..
                }
                | Self::Light {
                    client_session_object_id,
                    ..
                } => client_session_object_id,
            }
        }

        fn cleanup(self, process: &mut crate::hle::kernel::k_process::KProcess) {
            match self {
                Self::Normal {
                    session_object_id,
                    client_session_object_id,
                } => {
                    process.unregister_client_session_object_by_object_id(client_session_object_id);
                    process.unregister_session_object_by_object_id(session_object_id);
                }
                Self::Light {
                    light_session_object_id,
                    server_session_object_id,
                    client_session_object_id,
                } => {
                    process
                        .light_client_session_objects
                        .remove(&client_session_object_id);
                    process
                        .light_server_session_objects
                        .remove(&server_session_object_id);
                    process
                        .light_session_objects
                        .remove(&light_session_object_id);
                }
            }
        }
    }

    let kernel = system.kernel().expect("kernel not initialized");
    let process_arc = system.current_process_arc();
    let mut process = process_arc.lock().unwrap();
    let Some(object_id) = process.handle_table.get_object(port) else {
        return RESULT_INVALID_HANDLE;
    };

    let Some(port) = process.get_client_port_by_object_id(object_id) else {
        return RESULT_INVALID_HANDLE;
    };

    let reserved_handle = match process.handle_table.reserve() {
        Ok(handle) => handle,
        Err(_) => return RESULT_OUT_OF_HANDLES,
    };

    let created_session = {
        let port_guard = port.lock().unwrap();
        if port_guard.is_light() {
            let port_name = port_guard.get_name();
            let (light_session_object_id, server_session_object_id, client_session_object_id) =
                match port_guard
                    .client
                    .create_light_session(&mut process, kernel, port_name)
                {
                    Ok(ids) => ids,
                    Err(result) => {
                        process.handle_table.unreserve(reserved_handle);
                        return result;
                    }
                };

            drop(port_guard);
            let enqueue_result = KPort::enqueue_light_session_arc(&port, server_session_object_id);
            if enqueue_result.is_error() {
                process.handle_table.unreserve(reserved_handle);
                process
                    .light_client_session_objects
                    .remove(&client_session_object_id);
                process
                    .light_server_session_objects
                    .remove(&server_session_object_id);
                process
                    .light_session_objects
                    .remove(&light_session_object_id);
                return enqueue_result;
            }

            CreatedPortSession::Light {
                light_session_object_id,
                server_session_object_id,
                client_session_object_id,
            }
        } else {
            let port_name = port_guard.get_name();
            let (session_object_id, client_session_object_id) = match port_guard
                .client
                .create_session(&mut process, kernel, Some(object_id), port_name)
            {
                Ok(ids) => ids,
                Err(result) => {
                    process.handle_table.unreserve(reserved_handle);
                    return result;
                }
            };

            drop(port_guard);
            let enqueue_result = KPort::enqueue_session_arc(&port, session_object_id);
            if enqueue_result.is_error() {
                process.handle_table.unreserve(reserved_handle);
                process.unregister_client_session_object_by_object_id(client_session_object_id);
                process.unregister_session_object_by_object_id(session_object_id);
                return enqueue_result;
            }

            CreatedPortSession::Normal {
                session_object_id,
                client_session_object_id,
            }
        }
    };

    if !process
        .handle_table
        .register(reserved_handle, created_session.client_session_object_id())
    {
        process.handle_table.unreserve(reserved_handle);
        created_session.cleanup(&mut process);
        return RESULT_OUT_OF_HANDLES;
    }

    *out = reserved_handle;
    RESULT_SUCCESS
}

/// Manages a named port (create or destroy).
///
/// Upstream: If max_sessions > 0, creates a port, registers it, adds server to handle table,
/// and creates a KObjectName entry. If max_sessions == 0, deletes the named object.
pub fn manage_named_port(
    system: &System,
    out_server_handle: &mut Handle,
    user_name: u64,
    max_sessions: i32,
) -> ResultCode {
    // Read the port name from guest memory.
    let name = match read_port_name(system, user_name) {
        Ok(n) => n,
        Err(rc) => return rc,
    };

    // Upstream: R_UNLESS(max_sessions >= 0, ResultOutOfRange)
    if max_sessions < 0 {
        return RESULT_OUT_OF_RANGE;
    }

    if max_sessions > 0 {
        let kernel = system.kernel().expect("kernel not initialized");
        let port = Arc::new(Mutex::new(KPort::new()));
        port.lock().unwrap().initialize(max_sessions, false, 0);
        let server_port_object_id = kernel.create_new_object_id() as u64;
        let client_port_object_id = kernel.create_new_object_id() as u64;

        let process_arc = system.current_process_arc();
        let mut process = process_arc.lock().unwrap();
        process.register_server_port_object(server_port_object_id, port.clone());
        process.register_client_port_object(client_port_object_id, port);
        kernel.register_kernel_object(server_port_object_id);
        kernel.register_kernel_object(client_port_object_id);

        // Add server to handle table.
        match process.handle_table.add(server_port_object_id) {
            Ok(handle) => *out_server_handle = handle,
            Err(_) => return RESULT_OUT_OF_HANDLES,
        }

        // Upstream: KObjectName::NewFromName(kernel, &port.GetClientPort(), name)
        // Register the client port with the named object system.
        if let Some(object_name_data) = kernel.object_name_global_data() {
            if object_name_data
                .new_from_name(client_port_object_id as usize, &name)
                .is_err()
            {
                // Name already exists — clean up server handle.
                // Upstream: ON_RESULT_FAILURE { handle_table.Remove(*out_server_handle); }
                process.handle_table.remove(*out_server_handle);
                process.unregister_server_port_object_by_object_id(server_port_object_id);
                process.unregister_client_port_object_by_object_id(client_port_object_id);
                kernel.unregister_kernel_object(server_port_object_id);
                kernel.unregister_kernel_object(client_port_object_id);
                return RESULT_INVALID_STATE;
            }
        }

        log::info!(
            "svc::ManageNamedPort(create): name=\"{}\", server_handle=0x{:08X}",
            name,
            *out_server_handle
        );

        RESULT_SUCCESS
    } else {
        // max_sessions == 0: delete the named port.
        debug_assert_eq!(max_sessions, 0);
        *out_server_handle = INVALID_HANDLE;

        let kernel = system.kernel().expect("kernel not initialized");

        // Upstream: KObjectName::Delete<KClientPort>(kernel, name)
        if let Some(object_name_data) = kernel.object_name_global_data() {
            // Find the object and delete it.
            if let Some(obj) = object_name_data.find(&name) {
                if object_name_data.delete(obj, &name).is_err() {
                    return RESULT_NOT_FOUND;
                }
                let object_id = obj as u64;
                let process_arc = system.current_process_arc();
                let mut process = process_arc.lock().unwrap();
                process.unregister_client_port_object_by_object_id(object_id);
                kernel.unregister_kernel_object(object_id);
            } else {
                return RESULT_NOT_FOUND;
            }
        }

        log::info!("svc::ManageNamedPort(delete): name=\"{}\"", name);

        RESULT_SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::kernel::k_process::{KProcess, ProcessLock};
    use crate::hle::kernel::k_resource_limit::{
        create_resource_limit_for_process, LimitableResource,
    };
    use crate::hle::kernel::k_thread::{KThread, KThreadLock};

    fn test_system() -> System {
        let mut system = System::new_for_test();

        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        {
            let mut process_guard = process.lock().unwrap();
            process_guard.process_id = 1;
            process_guard.initialize_handle_table();
        }

        let current_thread = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut thread = current_thread.lock().unwrap();
            thread.thread_id = 1;
            thread.object_id = 1;
        }
        process
            .lock()
            .unwrap()
            .register_thread_object(current_thread);

        let scheduler = Arc::new(Mutex::new(
            crate::hle::kernel::k_scheduler::KScheduler::new(0),
        ));
        scheduler.lock().unwrap().initialize(1, 0, 0);
        let shared_memory = process.lock().unwrap().get_shared_memory();

        system.set_current_process_arc(process);
        system.set_scheduler_arc(scheduler);
        system.set_shared_process_memory(shared_memory);
        system.set_runtime_program_id(1);
        system.set_runtime_64bit(false);
        system
    }

    #[test]
    fn create_port_registers_resolvable_client_and_server_handles() {
        let system = test_system();
        let mut server = 0;
        let mut client = 0;

        assert_eq!(
            create_port(&system, &mut server, &mut client, 4, false, 0),
            RESULT_SUCCESS
        );

        let process_arc = system.current_process_arc();
        let process = process_arc.lock().unwrap();
        let server_object_id = process.handle_table.get_object(server).unwrap();
        let client_object_id = process.handle_table.get_object(client).unwrap();
        assert!(process
            .get_server_port_by_object_id(server_object_id)
            .is_some());
        assert!(process
            .get_client_port_by_object_id(client_object_id)
            .is_some());
    }

    #[test]
    fn connect_to_port_creates_client_session_and_enqueues_server_session() {
        let system = test_system();
        let mut server = 0;
        let mut client = 0;

        assert_eq!(
            create_port(&system, &mut server, &mut client, 4, false, 0x55),
            RESULT_SUCCESS
        );

        let mut session_handle = 0;
        assert_eq!(
            connect_to_port(&system, &mut session_handle, client),
            RESULT_SUCCESS
        );

        let process_arc = system.current_process_arc();
        let process = process_arc.lock().unwrap();
        let client_session_object_id = process.handle_table.get_object(session_handle).unwrap();
        let client_session = process
            .get_client_session_by_object_id(client_session_object_id)
            .unwrap();
        let server_session_object_id = client_session.lock().unwrap().get_parent_id().unwrap();

        let server_port_object_id = process.handle_table.get_object(server).unwrap();
        let port = process
            .get_server_port_by_object_id(server_port_object_id)
            .unwrap();
        assert_eq!(
            port.lock().unwrap().server.accept_session(),
            Some(server_session_object_id)
        );
    }

    #[test]
    fn normal_port_session_finalize_notifies_client_port() {
        let system = test_system();
        let resource_limit = Arc::new(create_resource_limit_for_process(0x1_0000_0000));
        system.current_process_arc().lock().unwrap().resource_limit =
            Some(Arc::clone(&resource_limit));
        let mut server = 0;
        let mut client = 0;

        assert_eq!(
            create_port(&system, &mut server, &mut client, 4, false, 0x57),
            RESULT_SUCCESS
        );

        let mut session_handle = 0;
        assert_eq!(
            connect_to_port(&system, &mut session_handle, client),
            RESULT_SUCCESS
        );

        let process_arc = system.current_process_arc();
        let mut process = process_arc.lock().unwrap();
        let client_port_object_id = process.handle_table.get_object(client).unwrap();
        let session_client_object_id = process.handle_table.get_object(session_handle).unwrap();
        let session_object_id = process
            .get_client_session_parent_id(session_client_object_id)
            .unwrap();
        let port = process
            .get_client_port_by_object_id(client_port_object_id)
            .unwrap();

        assert_eq!(port.lock().unwrap().client.get_num_sessions(), 1);
        assert_eq!(
            resource_limit.get_current_value(LimitableResource::SessionCountMax),
            1
        );
        process.unregister_session_object_by_object_id(session_object_id);
        assert_eq!(port.lock().unwrap().client.get_num_sessions(), 0);
        assert_eq!(
            resource_limit.get_current_value(LimitableResource::SessionCountMax),
            0
        );
    }

    #[test]
    fn connect_to_port_creates_light_client_session_for_light_port() {
        let system = test_system();
        let mut server = 0;
        let mut client = 0;

        assert_eq!(
            create_port(&system, &mut server, &mut client, 4, true, 0x56),
            RESULT_SUCCESS
        );

        let mut session_handle = 0;
        assert_eq!(
            connect_to_port(&system, &mut session_handle, client),
            RESULT_SUCCESS
        );

        let process_arc = system.current_process_arc();
        let process = process_arc.lock().unwrap();
        let client_session_object_id = process.handle_table.get_object(session_handle).unwrap();
        assert!(process
            .get_light_client_session_by_object_id(client_session_object_id)
            .is_some());

        let server_port_object_id = process.handle_table.get_object(server).unwrap();
        let port = process
            .get_server_port_by_object_id(server_port_object_id)
            .unwrap();
        let server_session_object_id = port
            .lock()
            .unwrap()
            .server
            .accept_light_session()
            .expect("light server session must be enqueued");
        assert!(process
            .get_light_server_session_by_object_id(server_session_object_id)
            .is_some());
    }

    #[test]
    fn connect_to_named_port_resolves_named_client_port_via_kernel_object_name() {
        let system = test_system();
        let kernel = system.kernel().unwrap();
        let port = Arc::new(Mutex::new(KPort::new()));
        port.lock().unwrap().initialize(4, false, 0);

        let client_port_object_id = 0x1234_u64;
        {
            let process_arc = system.current_process_arc();
            let mut process = process_arc.lock().unwrap();
            process.register_client_port_object(client_port_object_id, port);
        }
        kernel.register_kernel_object(client_port_object_id);
        kernel
            .object_name_global_data()
            .unwrap()
            .new_from_name(client_port_object_id as usize, "sm:")
            .unwrap();

        let name_addr = 0x5000;
        system
            .shared_process_memory()
            .write()
            .unwrap()
            .write_block(name_addr, b"sm:\0");

        let mut handle = 0;
        assert_eq!(
            connect_to_named_port(&system, &mut handle, name_addr),
            RESULT_SUCCESS
        );

        let process_arc = system.current_process_arc();
        let process = process_arc.lock().unwrap();
        let client_session_object_id = process.handle_table.get_object(handle).unwrap();
        assert!(process
            .get_client_session_by_object_id(client_session_object_id)
            .is_some());
    }

    #[test]
    fn connect_to_named_port_attaches_manager_to_server_session_for_hle_service() {
        let system = test_system();
        let name_addr = 0x5000;
        system
            .shared_process_memory()
            .write()
            .unwrap()
            .write_block(name_addr, b"sm:\0");

        let mut handle = 0;
        assert_eq!(
            connect_to_named_port(&system, &mut handle, name_addr),
            RESULT_SUCCESS
        );

        let process_arc = system.current_process_arc();
        let process = process_arc.lock().unwrap();
        let client_session_object_id = process.handle_table.get_object(handle).unwrap();
        let client_session = process
            .get_client_session_by_object_id(client_session_object_id)
            .unwrap();
        let parent_id = client_session.lock().unwrap().get_parent_id().unwrap();
        let parent_session = process.get_session_by_object_id(parent_id).unwrap();
        let server_session = parent_session.lock().unwrap().get_server_session().clone();
        assert!(server_session.lock().unwrap().get_manager().is_some());
    }
}
