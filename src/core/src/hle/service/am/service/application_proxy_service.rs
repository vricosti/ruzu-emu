// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/am/service/application_proxy_service.h
//! Port of zuyu/src/core/hle/service/am/service/application_proxy_service.cpp

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use crate::core::SystemRef;
use crate::hle::kernel::k_process::ProcessLock;
use crate::hle::result::{ResultCode, RESULT_SUCCESS, RESULT_UNKNOWN};
use crate::hle::service::am::window_system::WindowSystem;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::ResponseBuilder;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command table for IApplicationProxyService ("appletOE"):
/// - 0: OpenApplicationProxy
/// - 10: OpenApplicationProxy (Switch 2)
pub struct IApplicationProxyService {
    system: SystemRef,
    window_system: Weak<Mutex<WindowSystem>>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IApplicationProxyService {
    pub fn new(system: SystemRef, window_system: Weak<Mutex<WindowSystem>>) -> Self {
        let handlers = build_handler_map(&[(
            0,
            Some(Self::open_application_proxy_handler),
            "OpenApplicationProxy",
        ), (10, Some(Self::open_application_proxy_handler), "OpenApplicationProxy (Switch 2)")]);
        Self {
            system,
            window_system,
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// Port of IApplicationProxyService::OpenApplicationProxy
    ///
    /// Upstream calls GetAppletFromProcessId(pid) via WindowSystem::GetByAppletResourceUserId
    /// to find the existing applet for the calling process, then wraps it in an
    /// IApplicationProxy.
    pub fn open_application_proxy(
        &self,
        pid: u64,
    ) -> Option<Arc<Mutex<crate::hle::service::am::applet::Applet>>> {
        log::debug!(
            "IApplicationProxyService::OpenApplicationProxy called, pid={:#x}",
            pid
        );
        self.window_system
            .upgrade()?
            .lock()
            .unwrap()
            .get_by_applet_resource_user_id(pid)
    }

    fn open_application_proxy_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationProxyService) };
        let pid = if ctx.get_pid() != 0 {
            ctx.get_pid()
        } else {
            ctx.get_thread()
                .and_then(|thread| {
                    thread
                        .lock()
                        .unwrap()
                        .parent
                        .as_ref()
                        .and_then(|p| p.upgrade())
                })
                .map(|process| process.lock().unwrap().get_process_id())
                .unwrap_or(0)
        };
        let Some(applet) = service.open_application_proxy(pid) else {
            // Upstream: UNIMPLEMENTED(); R_THROW(ResultUnknown);
            log::error!("OpenApplicationProxy: no tracked applet for pid={:#x}", pid);
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };
        log::debug!("OpenApplicationProxy: found applet for pid={:#x}", pid);
        let process = ctx
            .get_thread()
            .and_then(|thread| {
                thread
                    .lock()
                    .unwrap()
                    .parent
                    .as_ref()
                    .and_then(|p| p.upgrade())
            })
            .map(|process| process as Arc<ProcessLock>);
        let proxy = Arc::new(super::application_proxy::IApplicationProxy::new(
            service.system,
            applet,
            process,
            service.window_system.clone(),
        ));
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(proxy);
    }
}

impl SessionRequestHandler for IApplicationProxyService {
    fn handle_sync_request(&self, context: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, context)
    }
}

impl ServiceFramework for IApplicationProxyService {
    fn get_service_name(&self) -> &str {
        "appletOE"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nx2_open_proxy_uses_existing_handler() {
        let service = IApplicationProxyService::new(SystemRef::null(), Weak::new());
        let original = service.handlers.get(&0).unwrap().handler_callback.unwrap();
        let nx2 = service.handlers.get(&10).unwrap().handler_callback.unwrap();
        assert!(std::ptr::fn_addr_eq(original, nx2));
    }

    #[test]
    fn service_does_not_own_window_system() {
        let window_system = Arc::new(Mutex::new(WindowSystem::new(SystemRef::null())));
        let weak = Arc::downgrade(&window_system);
        let service = IApplicationProxyService::new(SystemRef::null(), weak.clone());

        drop(window_system);

        assert!(
            weak.upgrade().is_none(),
            "AM services must mirror upstream's non-owning WindowSystem references"
        );
        drop(service);
    }
}
