//! Port of Eden core/hle/service/am/service/overlay_functions.h/.cpp.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::am::{applet::Applet, window_system::WindowSystem};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

pub struct IOverlayFunctions {
    applet: Arc<Mutex<Applet>>,
    // Weak WindowSystem reference replaces lookup through System, as in other AM services.
    window_system: Weak<Mutex<WindowSystem>>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IOverlayFunctions {
    pub fn new(applet: Arc<Mutex<Applet>>, window_system: Weak<Mutex<WindowSystem>>) -> Self {
        Self {
            applet, window_system,
            handlers: build_handler_map(&[
                (0, Some(Self::begin_to_watch_short_home_button_message), "BeginToWatchShortHomeButtonMessage"),
                (1, Some(Self::end_to_watch_short_home_button_message), "EndToWatchShortHomeButtonMessage"),
                (4, Some(Self::set_auto_sleep_time_and_dimming_time_enabled), "SetAutoSleepTimeAndDimmingTimeEnabled"),
                (20, Some(Self::set_handling_home_button_short_pressed_enabled), "SetHandlingHomeButtonShortPressedEnabled"),
                (21, Some(Self::set_handling_touch_screen_input_enabled), "SetHandlingTouchScreenInputEnabled"),
                (2, Some(Self::get_application_id_for_logo), "GetApplicationIdForLogo"),
                (3, None, "SetGpuTimeSliceBoost"),
                (5, None, "TerminateApplicationAndSetReason"),
                (6, None, "SetScreenShotPermissionGlobally"),
                (10, None, "StartShutdownSequenceForOverlay"),
                (11, None, "StartRebootSequenceForOverlay"),
                (30, None, "SetHealthWarningShowingState"),
                (31, Some(Self::is_health_warning_required), "IsHealthWarningRequired"),
                (40, None, "GetApplicationNintendoLogo"),
                (41, None, "GetApplicationStartupMovie"),
                (50, None, "SetGpuTimeSliceBoostForApplication"),
                (60, None, "Unknown60"),
                (70, Some(Self::unknown70), "Unknown70"),
                (90, None, "SetRequiresGpuResourceUse"),
                (101, None, "BeginToObserveHidInputForDevelop"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn begin_to_watch_short_home_button_message(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        service.applet.lock().unwrap().overlay_watching_short_home_button = true;
        if let Some(window) = service.window_system.upgrade() { window.lock().unwrap().request_update(); }
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn end_to_watch_short_home_button_message(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        service.applet.lock().unwrap().overlay_watching_short_home_button = false;
        if let Some(window) = service.window_system.upgrade() { window.lock().unwrap().request_update(); }
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn set_auto_sleep_time_and_dimming_time_enabled(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let enabled = RequestParser::new(ctx).pop_bool();
        service.applet.lock().unwrap().auto_sleep_disabled = !enabled;
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn set_handling_home_button_short_pressed_enabled(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let enabled = RequestParser::new(ctx).pop_bool();
        service.applet.lock().unwrap().home_button_short_pressed_blocked = !enabled;
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn set_handling_touch_screen_input_enabled(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let enabled = RequestParser::new(ctx).pop_bool();
        service.applet.lock().unwrap().overlay_handling_touch_input = enabled;
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_application_id_for_logo(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let target = service.window_system.upgrade()
            .and_then(|window| window.lock().unwrap().get_main_applet())
            .unwrap_or_else(|| service.applet.clone());
        let applet = target.lock().unwrap();
        let id = if applet.screen_shot_identity.application_id != 0 {
            applet.screen_shot_identity.application_id
        } else { applet.program_id };
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(id);
    }

    fn is_health_warning_required(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let _applet = service.applet.lock().unwrap();
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(false);
    }

    fn unknown70(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }
}

impl SessionRequestHandler for IOverlayFunctions {
    fn as_any(&self) -> &dyn std::any::Any { self }
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }
    fn service_name(&self) -> &str { "IOverlayFunctions" }
}
impl ServiceFramework for IOverlayFunctions {
    fn get_service_name(&self) -> &str { "IOverlayFunctions" }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> { &self.handlers }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> { &self.handlers_tipc }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::SystemRef;
    use crate::hle::service::os::process::Process;

    fn call(service: &IOverlayFunctions, command: u32, value: u32) -> HLERequestContext {
        let mut ctx = HLERequestContext::new();
        ctx.cmd_buf[2] = value;
        service.handlers[&command].handler_callback.unwrap()(service, &mut ctx);
        assert_eq!(ctx.cmd_buf[6], RESULT_SUCCESS.get_inner_value());
        ctx
    }

    #[test]
    fn command_policy_and_watch_state_match_upstream() {
        let applet = Arc::new(Mutex::new(Applet::new(SystemRef::null(), Process::new(), false)));
        let service = IOverlayFunctions::new(applet.clone(), Weak::new());
        assert_eq!(service.handlers.len(), 20);
        for command in [3, 5, 6, 10, 11, 30, 40, 41, 50, 60, 90, 101] {
            assert!(service.handlers[&command].handler_callback.is_none());
        }
        call(&service, 0, 0);
        assert!(applet.lock().unwrap().overlay_watching_short_home_button);
        call(&service, 1, 0);
        assert!(!applet.lock().unwrap().overlay_watching_short_home_button);
        for value in [0, 1] {
            call(&service, 4, value);
            assert_eq!(applet.lock().unwrap().auto_sleep_disabled, value == 0);
            call(&service, 20, value);
            assert_eq!(applet.lock().unwrap().home_button_short_pressed_blocked, value == 0);
            call(&service, 21, value);
            assert_eq!(applet.lock().unwrap().overlay_handling_touch_input, value != 0);
        }
        assert_eq!(call(&service, 31, 0).cmd_buf[8], 0);
        call(&service, 70, 0);
    }

    #[test]
    fn logo_uses_main_applet_identity_then_program_id_then_overlay_fallback() {
        let overlay = Arc::new(Mutex::new(Applet::new(SystemRef::null(), Process::new(), false)));
        overlay.lock().unwrap().program_id = 0x1122334455667788;
        let window = Arc::new(Mutex::new(WindowSystem::new(SystemRef::null())));
        let service = IOverlayFunctions::new(overlay, Arc::downgrade(&window));
        let read = || {
            let ctx = call(&service, 2, 0);
            ctx.cmd_buf[8] as u64 | ((ctx.cmd_buf[9] as u64) << 32)
        };
        assert_eq!(read(), 0x1122334455667788);
        let main = Arc::new(Mutex::new(Applet::new(SystemRef::null(), Process::new(), true)));
        main.lock().unwrap().program_id = 42;
        window.lock().unwrap().track_applet(main.clone(), true);
        assert_eq!(read(), 42);
        main.lock().unwrap().screen_shot_identity.application_id = 73;
        assert_eq!(read(), 73);
    }
}
