// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/service.h and service.cpp
//! Status: Structural port
//!
//! Contains:
//! - ServerSessionCountMax constant
//! - ServiceFrameworkBase: non-generic base for service dispatch
//! - ServiceFramework trait: CRTP-like pattern for registering handlers
//!
//! The C++ CRTP pattern (ServiceFramework<Self>) is represented here as a trait
//! with handler registration and dispatch methods. The type-erasure pattern is
//! handled through function pointer dispatch.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::hle::ipc;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers;

/// Default number of maximum connections to a server session.
pub const SERVER_SESSION_COUNT_MAX: u32 = 0x40;

const _: () = assert!(SERVER_SESSION_COUNT_MAX == 0x40);

static IPC_INVALID_TRACE_SEQ: AtomicU64 = AtomicU64::new(0);

fn trace_invalid_ipc(service_name: &str, ctx: &HLERequestContext) {
    if !common::trace::is_enabled(common::trace::cat::IPC_INVALID) {
        return;
    }

    let service_id = common::trace::intern_service(service_name) as u64;
    let thread_id = ctx
        .get_thread()
        .as_ref()
        .map(|thread| thread.lock().unwrap().thread_id)
        .unwrap_or(0);
    let cmd_buf = ctx.command_buffer();
    let seq = IPC_INVALID_TRACE_SEQ.fetch_add(1, Ordering::Relaxed);
    common::trace::emit_raw(
        common::trace::cat::IPC_INVALID,
        &[
            seq,
            service_id,
            ctx.get_command() as u64,
            thread_id,
            ctx.tls_address(),
            cmd_buf[0] as u64,
            cmd_buf[1] as u64,
            cmd_buf[2] as u64,
            cmd_buf[3] as u64,
            cmd_buf[4] as u64,
            cmd_buf[5] as u64,
            cmd_buf[6] as u64,
            cmd_buf[7] as u64,
            cmd_buf[8] as u64,
        ],
    );
}

/// Information about a single IPC handler function.
#[derive(Clone)]
pub struct FunctionInfo {
    pub expected_header: u32,
    pub handler_callback: Option<fn(&dyn ServiceFramework, &mut HLERequestContext)>,
    pub name: &'static str,
}

impl FunctionInfo {
    pub const fn new(
        expected_header: u32,
        handler_callback: Option<fn(&dyn ServiceFramework, &mut HLERequestContext)>,
        name: &'static str,
    ) -> Self {
        Self {
            expected_header,
            handler_callback,
            name,
        }
    }
}

/// Trait that corresponds to upstream `ServiceFramework<Self>`.
///
/// Services implement this trait to register CMIF/TIPC handlers and dispatch IPC requests.
/// The upstream C++ CRTP (Curiously Recurring Template Pattern) is replaced by a trait.
///
/// Upstream stores `Core::System& system` in `ServiceFrameworkBase`, giving every service
/// access to `system.ServiceManager()`. The Rust port routes that global owner through
/// `HLERequestContext`, so control requests remain system-owned instead of service-owned.
pub trait ServiceFramework: SessionRequestHandler {
    /// Returns the service name.
    fn get_service_name(&self) -> &str;

    /// Returns the maximum number of concurrent sessions.
    fn get_max_sessions(&self) -> u32 {
        SERVER_SESSION_COUNT_MAX
    }

    /// Returns a reference to the CMIF handler map.
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo>;

    /// Returns a reference to the TIPC handler map.
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo>;

    /// Deprecated compatibility hook kept for older services.
    fn service_manager(
        &self,
    ) -> Option<std::sync::Arc<std::sync::Mutex<crate::hle::service::sm::sm::ServiceManager>>> {
        None
    }

    /// Invokes a service request routine using the HIPC protocol.
    fn invoke_request(&self, ctx: &mut HLERequestContext)
    where
        Self: Sized,
    {
        let cmd = ctx.get_command();
        let info = self.handlers().get(&cmd);

        match info {
            Some(fi) if fi.handler_callback.is_some() => {
                log::trace!("Service::{}: {}", self.get_service_name(), fi.name);
                if let Some(callback) = fi.handler_callback {
                    callback(self, ctx);
                }
            }
            _ => {
                self.report_unimplemented_function(ctx, info);
            }
        }
    }

    /// Invokes a service request routine using the TIPC protocol.
    fn invoke_request_tipc(&self, ctx: &mut HLERequestContext)
    where
        Self: Sized,
    {
        let cmd = ctx.get_command();
        let info = self.handlers_tipc().get(&cmd);

        match info {
            Some(fi) if fi.handler_callback.is_some() => {
                log::trace!("Service::{}: {}", self.get_service_name(), fi.name);
                if let Some(callback) = fi.handler_callback {
                    callback(self, ctx);
                }
            }
            _ => {
                self.report_unimplemented_function(ctx, info);
            }
        }
    }

    /// Reports an unimplemented function and optionally writes a stub success response.
    fn report_unimplemented_function(
        &self,
        ctx: &mut HLERequestContext,
        info: Option<&FunctionInfo>,
    ) {
        let function_name = match info {
            Some(fi) => fi.name.to_string(),
            None => "<unknown>".to_owned(),
        };

        let cmd_buf = ctx.command_buffer();
        let mut buf = format!(
            "function '{}({})': port='{}' cmd_buf={{[0]={:#x}",
            ctx.get_command(),
            function_name,
            self.get_service_name(),
            cmd_buf[0]
        );
        for i in 1..=8 {
            buf.push_str(&format!(", [{}]={:#x}", i, cmd_buf[i]));
        }
        buf.push('}');

        // Upstream reads the System reference stored directly by
        // ServiceFrameworkBase. The Rust framework carries the same non-owning
        // owner through the request's KernelCore; do not recover it by locking
        // ServerManager, whose cooperative service fiber may be suspended.
        if let Some(system) = ctx.get_system() {
            system.get_reporter().save_unimplemented_function_report(
                system,
                ctx,
                ctx.get_command(),
                &function_name,
                self.get_service_name(),
            );
        }

        log::error!("Unknown / unimplemented {}", buf);
        common::assert::assert_fail_soft_impl();

        if *common::settings::values().use_auto_stub.get_value() {
            log::warn!("Using auto stub fallback!");
            let mut rb = ipc_helpers::ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_SUCCESS);
        }
    }

    /// Handles a synchronization request for the service.
    ///
    /// Corresponds to upstream `ServiceFrameworkBase::HandleSyncRequest`.
    fn handle_sync_request_impl(&self, ctx: &mut HLERequestContext) -> ResultCode
    where
        Self: Sized,
    {
        let mut result = RESULT_SUCCESS;

        match ctx.get_command_type() {
            ipc::CommandType::Close | ipc::CommandType::TipcClose => {
                let mut rb = ipc_helpers::ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                result = ipc_helpers::RESULT_SESSION_CLOSED;
            }
            ipc::CommandType::ControlWithContext | ipc::CommandType::Control => {
                // Matches upstream: system.ServiceManager().InvokeControlRequest(ctx)
                log::debug!(
                    "Control cmd={} on service '{}'",
                    ctx.get_command(),
                    self.get_service_name()
                );
                if let Some(sm) = ctx
                    .get_service_manager()
                    .cloned()
                    .or_else(|| self.service_manager())
                {
                    let controller = sm.lock().unwrap().controller_interface();
                    controller.invoke_request(ctx);
                } else {
                    log::warn!(
                        "Control request but no ServiceManager available for service '{}'",
                        self.get_service_name()
                    );
                }
            }
            ipc::CommandType::RequestWithContext | ipc::CommandType::Request => {
                self.invoke_request(ctx);
            }
            _ => {
                if ctx.is_tipc() {
                    self.invoke_request_tipc(ctx);
                } else {
                    let cmd_buf = ctx.command_buffer();
                    trace_invalid_ipc(self.get_service_name(), ctx);
                    log::warn!(
                        "Unimplemented command_type={:?} service={} cmd={} cmd_buf={{[0]=0x{:X}, [1]=0x{:X}, [2]=0x{:X}, [3]=0x{:X}, [4]=0x{:X}, [5]=0x{:X}, [6]=0x{:X}, [7]=0x{:X}}}",
                        ctx.get_command_type(),
                        self.get_service_name(),
                        ctx.get_command(),
                        cmd_buf[0],
                        cmd_buf[1],
                        cmd_buf[2],
                        cmd_buf[3],
                        cmd_buf[4],
                        cmd_buf[5],
                        cmd_buf[6],
                        cmd_buf[7],
                    );
                }
            }
        }

        // Write response back. Matches upstream `ServiceFrameworkBase::HandleSyncRequest`
        // (service.cpp:148): the service handler is the sole writer of the outgoing buffer.
        ctx.write_to_outgoing_command_buffer();

        result
    }
}

/// Helper to build a handler map from a slice of (id, callback, name) tuples.
pub fn build_handler_map(
    functions: &[(
        u32,
        Option<fn(&dyn ServiceFramework, &mut HLERequestContext)>,
        &'static str,
    )],
) -> BTreeMap<u32, FunctionInfo> {
    let infos: Vec<FunctionInfo> = functions
        .iter()
        .map(|&(id, callback, name)| FunctionInfo::new(id, callback, name))
        .collect();
    build_handler_map_from_infos(&infos)
}

/// Helper to build a handler map from upstream-shaped `FunctionInfo` entries.
pub fn build_handler_map_from_infos(functions: &[FunctionInfo]) -> BTreeMap<u32, FunctionInfo> {
    let mut map = BTreeMap::new();
    for info in functions {
        map.insert(info.expected_header, info.clone());
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::SystemRef;
    use crate::hle::service::hle_ipc::SessionRequestManager;
    use crate::hle::service::server_manager::ServerManager;
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::Duration;

    struct TestService {
        handlers: BTreeMap<u32, FunctionInfo>,
    }

    impl TestService {
        fn new() -> Self {
            Self {
                handlers: BTreeMap::new(),
            }
        }
    }

    impl SessionRequestHandler for TestService {
        fn handle_sync_request(&self, _ctx: &mut HLERequestContext) -> ResultCode {
            RESULT_SUCCESS
        }
    }

    impl ServiceFramework for TestService {
        fn get_service_name(&self) -> &str {
            "TestService"
        }

        fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
            &self.handlers
        }

        fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
            &self.handlers
        }
    }

    #[test]
    fn test_server_session_count_max() {
        assert_eq!(SERVER_SESSION_COUNT_MAX, 0x40);
    }

    #[test]
    fn test_build_handler_map() {
        let map = build_handler_map(&[(0, None, "Initialize"), (1, None, "GetService")]);
        assert_eq!(map.len(), 2);
        assert!(map.contains_key(&0));
        assert!(map.contains_key(&1));
        assert_eq!(map[&0].name, "Initialize");
        assert_eq!(map[&1].name, "GetService");
    }

    #[test]
    fn test_build_handler_map_from_infos() {
        let infos = [
            FunctionInfo::new(3, None, "Get"),
            FunctionInfo::new(4, None, "Get1"),
        ];
        let map = build_handler_map_from_infos(&infos);
        assert_eq!(map.len(), 2);
        assert_eq!(map[&3].expected_header, 3);
        assert_eq!(map[&4].name, "Get1");
    }

    #[test]
    fn unimplemented_report_does_not_lock_server_manager() {
        let server_manager = ServerManager::new_shared(SystemRef::null());
        let request_manager = Arc::new(Mutex::new(SessionRequestManager::new_with_server_manager(
            Arc::clone(&server_manager),
        )));
        let mut ctx = HLERequestContext::new();
        ctx.set_session_request_manager(request_manager);

        let (locked_tx, locked_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let held_manager = Arc::clone(&server_manager);
        let holder = std::thread::spawn(move || {
            let _guard = held_manager.lock().unwrap();
            locked_tx.send(()).unwrap();
            let _ = release_rx.recv();
        });
        locked_rx.recv().unwrap();

        let (done_tx, done_rx) = mpsc::channel();
        let reporter = std::thread::spawn(move || {
            TestService::new().report_unimplemented_function(&mut ctx, None);
            done_tx.send(()).unwrap();
        });

        let completed_without_manager = done_rx.recv_timeout(Duration::from_millis(250)).is_ok();
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        reporter.join().unwrap();

        assert!(
            completed_without_manager,
            "unimplemented report waited for the owning ServerManager"
        );
    }
}
