// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/am/service/application_proxy.h
//! Port of zuyu/src/core/hle/service/am/service/application_proxy.cpp

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use crate::core::SystemRef;
use crate::hle::kernel::k_process::ProcessLock;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::am::applet::Applet;
use crate::hle::service::am::window_system::WindowSystem;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::ResponseBuilder;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command table for IApplicationProxy:
/// - 0: GetCommonStateGetter
/// - 1: GetSelfController
/// - 2: GetWindowController
/// - 3: GetAudioController
/// - 4: GetDisplayController
/// - 10: GetProcessWindingController
/// - 11: GetLibraryAppletCreator
/// - 20: GetApplicationFunctions
/// - 40, 50, 70, 80, 90: experimental NX2 stub interfaces
/// - 60: GetGraphicsScaleController (experimental NX2 stub)
/// - 1000: GetDebugFunctions
pub struct IApplicationProxy {
    /// Matches upstream `Core::System& system`.
    system: SystemRef,
    /// Reference to the applet.
    /// Matches upstream `const std::shared_ptr<Applet> m_applet`.
    applet: Arc<Mutex<Applet>>,
    /// Reference to the window system.
    /// Matches upstream `WindowSystem& m_window_system`.
    window_system: Weak<Mutex<WindowSystem>>,
    /// Matches upstream `Kernel::KProcess* m_process`.
    process: Option<Arc<ProcessLock>>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IApplicationProxy {
    /// Create with an applet and window system reference.
    /// Matches upstream: `IApplicationProxy(Core::System&, std::shared_ptr<Applet>, Kernel::KProcess*, WindowSystem&)`
    pub fn new(
        system: SystemRef,
        applet: Arc<Mutex<Applet>>,
        process: Option<Arc<ProcessLock>>,
        window_system: Weak<Mutex<WindowSystem>>,
    ) -> Self {
        let handlers = build_handler_map(&[
            (
                0,
                Some(Self::get_common_state_getter_handler),
                "GetCommonStateGetter",
            ),
            (
                1,
                Some(Self::get_self_controller_handler),
                "GetSelfController",
            ),
            (
                2,
                Some(Self::get_window_controller_handler),
                "GetWindowController",
            ),
            (
                3,
                Some(Self::get_audio_controller_handler),
                "GetAudioController",
            ),
            (
                4,
                Some(Self::get_display_controller_handler),
                "GetDisplayController",
            ),
            (
                10,
                Some(Self::get_process_winding_controller_handler),
                "GetProcessWindingController",
            ),
            (
                11,
                Some(Self::get_library_applet_creator_handler),
                "GetLibraryAppletCreator",
            ),
            (
                20,
                Some(Self::get_application_functions_handler),
                "GetApplicationFunctions",
            ),
            (
                1000,
                Some(Self::get_debug_functions_handler),
                "GetDebugFunctions",
            ),
            (40, Some(Self::get_nx2_interface), "Nx2Getter40"),
            (50, Some(Self::get_nx2_interface), "Nx2Getter50"),
            (60, Some(Self::get_graphics_scale_controller), "GetGraphicsScaleController (Switch 2)"),
            (70, Some(Self::get_nx2_interface), "Nx2Getter70"),
            (80, Some(Self::get_nx2_interface), "Nx2Getter80"),
            (90, Some(Self::get_nx2_interface), "Nx2Getter90"),
        ]);
        Self {
            system,
            applet,
            window_system,
            process,
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    pub fn push_interface_response(
        ctx: &mut HLERequestContext,
        object: Arc<dyn SessionRequestHandler>,
    ) {
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(object);
    }

    fn get_graphics_scale_controller(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        Self::push_interface_response(
            ctx,
            Arc::new(super::graphics_scale_controller::IGraphicsScaleController::new()),
        );
    }

    fn get_nx2_interface(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        crate::hle::service::nx2_stub_interface::return_nx2_stub_interface(ctx, "IApplicationProxy");
    }

    fn get_common_state_getter_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let proxy = unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationProxy) };
        Self::push_interface_response(
            ctx,
            Arc::new(super::common_state_getter::ICommonStateGetter::new(
                proxy.system,
                proxy.applet.clone(),
            )),
        );
    }

    fn get_self_controller_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let proxy = unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationProxy) };
        Self::push_interface_response(
            ctx,
            Arc::new(super::self_controller::ISelfController::new(
                proxy.system,
                proxy.applet.clone(),
                proxy.process.clone(),
            )),
        );
    }

    fn get_window_controller_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let proxy = unsafe { &*(_this as *const dyn ServiceFramework as *const IApplicationProxy) };
        Self::push_interface_response(
            ctx,
            Arc::new(super::window_controller::IWindowController::new(
                proxy.applet.clone(),
                proxy.window_system.clone(),
            )),
        );
    }

    fn get_audio_controller_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        Self::push_interface_response(
            ctx,
            Arc::new(super::audio_controller::IAudioController::new()),
        );
    }

    fn get_display_controller_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let proxy = unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationProxy) };
        Self::push_interface_response(
            ctx,
            Arc::new(super::display_controller::IDisplayController::new(
                proxy.applet.clone(),
            )),
        );
    }

    fn get_process_winding_controller_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let proxy = unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationProxy) };
        Self::push_interface_response(
            ctx,
            Arc::new(
                super::process_winding_controller::IProcessWindingController::new(
                    proxy.applet.clone(),
                ),
            ),
        );
    }

    fn get_library_applet_creator_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let proxy = unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationProxy) };
        Self::push_interface_response(
            ctx,
            Arc::new(super::library_applet_creator::ILibraryAppletCreator::new(
                proxy.system,
                proxy.applet.clone(),
                proxy.window_system.clone(),
            )),
        );
    }

    fn get_application_functions_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let proxy = unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationProxy) };
        Self::push_interface_response(
            ctx,
            Arc::new(super::application_functions::IApplicationFunctions::new(
                proxy.system,
                proxy.applet.clone(),
            )),
        );
    }

    fn get_debug_functions_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        Self::push_interface_response(
            ctx,
            Arc::new(super::debug_functions::IDebugFunctions::new()),
        );
    }
}

impl SessionRequestHandler for IApplicationProxy {
    fn handle_sync_request(&self, context: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, context)
    }
}

impl ServiceFramework for IApplicationProxy {
    fn get_service_name(&self) -> &str {
        "am::IApplicationProxy"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}
