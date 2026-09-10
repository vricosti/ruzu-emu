// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/am/service/all_system_applet_proxies_service.h
//! Port of zuyu/src/core/hle/service/am/service/all_system_applet_proxies_service.cpp

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use crate::core::SystemRef;
use crate::hle::kernel::k_process::ProcessLock;
use crate::hle::result::{ResultCode, RESULT_SUCCESS, RESULT_UNKNOWN};
use crate::hle::service::am::applet::Applet;
use crate::hle::service::am::window_system::WindowSystem;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::ResponseBuilder;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command table for IAllSystemAppletProxiesService ("appletAE"):
/// - 100: OpenSystemAppletProxy
/// - 110: OpenSystemAppletProxyEx
/// - 200: OpenLibraryAppletProxyOld
/// - 201: OpenLibraryAppletProxy
/// - 300: OpenOverlayAppletProxy (unimplemented)
/// - 350: OpenSystemApplicationProxy (unimplemented)
/// - 400: CreateSelfLibraryAppletCreatorForDevelop (unimplemented)
/// - 410: GetSystemAppletControllerForDebug (unimplemented)
/// - 450: GetSystemProcessCommonFunctions (upstream stub)
/// - 460: GetAppletAlternativeFunctions (upstream stub)
/// - 1000: GetDebugFunctions (unimplemented)
pub struct IAllSystemAppletProxiesService {
    system: SystemRef,
    window_system: Weak<Mutex<WindowSystem>>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IAllSystemAppletProxiesService {
    pub fn new(system: SystemRef, window_system: Weak<Mutex<WindowSystem>>) -> Self {
        let handlers = build_handler_map(&[
            (
                100,
                Some(Self::open_system_applet_proxy_handler),
                "OpenSystemAppletProxy",
            ),
            (110, Some(Self::open_system_applet_proxy_handler), "OpenSystemAppletProxyEx"),
            (
                200,
                Some(Self::open_library_applet_proxy_old_handler),
                "OpenLibraryAppletProxyOld",
            ),
            (
                201,
                Some(Self::open_library_applet_proxy_handler),
                "OpenLibraryAppletProxy",
            ),
            (300, None, "OpenOverlayAppletProxy"),
            (350, None, "OpenSystemApplicationProxy"),
            (400, None, "CreateSelfLibraryAppletCreatorForDevelop"),
            (410, None, "GetSystemAppletControllerForDebug"),
            (450, Some(Self::get_system_process_common_functions), "GetSystemProcessCommonFunctions"),
            (460, Some(Self::get_applet_alternative_functions), "GetAppletAlternativeFunctions"),
            (1000, None, "GetDebugFunctions"),
        ]);
        Self {
            system,
            window_system,
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn get_system_process_common_functions(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("IAllSystemAppletProxiesService::GetSystemProcessCommonFunctions (STUBBED)");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_applet_alternative_functions(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("IAllSystemAppletProxiesService::GetAppletAlternativeFunctions (STUBBED)");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_applet_from_process_id(&self, pid: u64) -> Option<Arc<Mutex<Applet>>> {
        self.window_system
            .upgrade()?
            .lock()
            .unwrap()
            .get_by_applet_resource_user_id(pid)
    }

    fn get_process_from_context(ctx: &HLERequestContext) -> Option<Arc<ProcessLock>> {
        ctx.get_thread().and_then(|thread| {
            thread
                .lock()
                .unwrap()
                .parent
                .as_ref()
                .and_then(|p| p.upgrade())
        })
    }

    fn push_interface_response(
        ctx: &mut HLERequestContext,
        object: Arc<dyn SessionRequestHandler>,
    ) {
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(object);
    }

    fn open_system_applet_proxy_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe {
            &*(this as *const dyn ServiceFramework as *const IAllSystemAppletProxiesService)
        };
        log::debug!("IAllSystemAppletProxiesService::OpenSystemAppletProxy");
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
        let Some(applet) = service.get_applet_from_process_id(pid) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };
        let proxy = Arc::new(super::system_applet_proxy::ISystemAppletProxy::new(
            service.system,
            applet,
            Self::get_process_from_context(ctx),
            service.window_system.clone(),
        ));
        Self::push_interface_response(ctx, proxy);
    }

    fn open_library_applet_proxy_old_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe {
            &*(this as *const dyn ServiceFramework as *const IAllSystemAppletProxiesService)
        };
        log::debug!("IAllSystemAppletProxiesService::OpenLibraryAppletProxyOld");
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
        let Some(applet) = service.get_applet_from_process_id(pid) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };
        let proxy = Arc::new(super::library_applet_proxy::ILibraryAppletProxy::new(
            service.system,
            applet,
            Self::get_process_from_context(ctx),
            service.window_system.clone(),
        ));
        Self::push_interface_response(ctx, proxy);
    }

    fn open_library_applet_proxy_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe {
            &*(this as *const dyn ServiceFramework as *const IAllSystemAppletProxiesService)
        };
        log::debug!("IAllSystemAppletProxiesService::OpenLibraryAppletProxy");
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
        let Some(applet) = service.get_applet_from_process_id(pid) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };
        let proxy = Arc::new(super::library_applet_proxy::ILibraryAppletProxy::new(
            service.system,
            applet,
            Self::get_process_from_context(ctx),
            service.window_system.clone(),
        ));
        Self::push_interface_response(ctx, proxy);
    }
}

impl SessionRequestHandler for IAllSystemAppletProxiesService {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firmware_common_commands_match_upstream_result_only_stubs() {
        let service = IAllSystemAppletProxiesService::new(SystemRef::null(), Weak::new());
        for command in [450, 460] {
            let mut ctx = HLERequestContext::new();
            service.handlers()[&command].handler_callback.unwrap()(&service, &mut ctx);
            assert_eq!(ctx.cmd_buf[6], RESULT_SUCCESS.get_inner_value());
            assert_eq!(ctx.cmd_buf[0], 0);
            // Same result-only reply as Eden: no returned IPC interface/handles.
            assert_eq!(ctx.cmd_buf[1] & 0x3ff, 10);
        }
    }

    #[test]
    fn extended_system_proxy_preserves_missing_applet_error() {
        let service = IAllSystemAppletProxiesService::new(SystemRef::null(), Weak::new());
        for command in [100, 110] {
            let mut ctx = HLERequestContext::new();
            service.handlers()[&command].handler_callback.unwrap()(&service, &mut ctx);
            assert_eq!(ctx.cmd_buf[6], RESULT_UNKNOWN.get_inner_value());
        }
    }
}

impl ServiceFramework for IAllSystemAppletProxiesService {
    fn get_service_name(&self) -> &str {
        "appletAE"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}
