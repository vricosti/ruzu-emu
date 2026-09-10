// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/am/service/applet_common_functions.h
//! Port of zuyu/src/core/hle/service/am/service/applet_common_functions.cpp

use std::collections::BTreeMap;

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command table for IAppletCommonFunctions:
/// - 0: SetTerminateResult (unimplemented)
/// - 10: ReadThemeStorage (unimplemented)
/// - 11: WriteThemeStorage (unimplemented)
/// - 20: PushToAppletBoundChannel (unimplemented)
/// - 21: TryPopFromAppletBoundChannel (unimplemented)
/// - 40: GetDisplayLogicalResolution (unimplemented)
/// - 42: SetDisplayMagnification
/// - 50: SetHomeButtonDoubleClickEnabled
/// - 51: GetHomeButtonDoubleClickEnabled
/// - 52: IsHomeButtonShortPressedBlocked (unimplemented)
/// - 60: IsVrModeCurtainRequired (unimplemented)
/// - 61: IsSleepRequiredByHighTemperature (unimplemented)
/// - 62: IsSleepRequiredByLowBattery (unimplemented)
/// - 70: SetCpuBoostRequestPriority
/// - 80: SetHandlingCaptureButtonShortPressedMessageEnabledForApplet (unimplemented)
/// - 81: SetHandlingCaptureButtonLongPressedMessageEnabledForApplet (unimplemented)
/// - 90: OpenNamedChannelAsParent (unimplemented)
/// - 91: OpenNamedChannelAsChild (unimplemented)
/// - 100: SetApplicationCoreUsageMode (unimplemented)
/// - 300: GetCurrentApplicationId
pub struct IAppletCommonFunctions {
    system: crate::core::SystemRef,
    applet: Option<std::sync::Arc<std::sync::Mutex<crate::hle::service::am::applet::Applet>>>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IAppletCommonFunctions {
    pub fn new() -> Self {
        let handlers = build_handler_map(&[
            (0, None, "SetTerminateResult"),
            (10, None, "ReadThemeStorage"),
            (11, None, "WriteThemeStorage"),
            (20, None, "PushToAppletBoundChannel"),
            (21, None, "TryPopFromAppletBoundChannel"),
            (40, None, "GetDisplayLogicalResolution"),
            (42, Some(Self::set_display_magnification_handler), "SetDisplayMagnification"),
            (
                50,
                Some(Self::set_home_button_double_click_enabled_handler),
                "SetHomeButtonDoubleClickEnabled",
            ),
            (
                51,
                Some(Self::get_home_button_double_click_enabled_handler),
                "GetHomeButtonDoubleClickEnabled",
            ),
            (52, None, "IsHomeButtonShortPressedBlocked"),
            (60, None, "IsVrModeCurtainRequired"),
            (61, None, "IsSleepRequiredByHighTemperature"),
            (62, None, "IsSleepRequiredByLowBattery"),
            (
                70,
                Some(Self::set_cpu_boost_request_priority_handler),
                "SetCpuBoostRequestPriority",
            ),
            (
                80,
                None,
                "SetHandlingCaptureButtonShortPressedMessageEnabledForApplet",
            ),
            (
                81,
                None,
                "SetHandlingCaptureButtonLongPressedMessageEnabledForApplet",
            ),
            (90, None, "OpenNamedChannelAsParent"),
            (91, None, "OpenNamedChannelAsChild"),
            (100, None, "SetApplicationCoreUsageMode"),
            (
                300,
                Some(Self::get_current_application_id_handler),
                "GetCurrentApplicationId",
            ),
            (310, None, "IsSystemAppletHomeMenu"),
            (320, Some(Self::set_gpu_time_slice_boost), "SetGpuTimeSliceBoost"),
            (321, None, "SetGpuTimeSliceBoostDueToApplication"),
            (350, Some(Self::unknown350), "Unknown350"),
        ]);
        Self {
            system: crate::core::SystemRef::null(),
            applet: None,
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    pub fn with_applet(
        system: crate::core::SystemRef,
        applet: std::sync::Arc<std::sync::Mutex<crate::hle::service::am::applet::Applet>>,
    ) -> Self {
        let handlers = build_handler_map(&[
            (0, None, "SetTerminateResult"),
            (10, None, "ReadThemeStorage"),
            (11, None, "WriteThemeStorage"),
            (20, None, "PushToAppletBoundChannel"),
            (21, None, "TryPopFromAppletBoundChannel"),
            (40, None, "GetDisplayLogicalResolution"),
            (42, Some(Self::set_display_magnification_handler), "SetDisplayMagnification"),
            (
                50,
                Some(Self::set_home_button_double_click_enabled_handler),
                "SetHomeButtonDoubleClickEnabled",
            ),
            (
                51,
                Some(Self::get_home_button_double_click_enabled_handler),
                "GetHomeButtonDoubleClickEnabled",
            ),
            (52, None, "IsHomeButtonShortPressedBlocked"),
            (60, None, "IsVrModeCurtainRequired"),
            (61, None, "IsSleepRequiredByHighTemperature"),
            (62, None, "IsSleepRequiredByLowBattery"),
            (
                70,
                Some(Self::set_cpu_boost_request_priority_handler),
                "SetCpuBoostRequestPriority",
            ),
            (
                80,
                None,
                "SetHandlingCaptureButtonShortPressedMessageEnabledForApplet",
            ),
            (
                81,
                None,
                "SetHandlingCaptureButtonLongPressedMessageEnabledForApplet",
            ),
            (90, None, "OpenNamedChannelAsParent"),
            (91, None, "OpenNamedChannelAsChild"),
            (100, None, "SetApplicationCoreUsageMode"),
            (
                300,
                Some(Self::get_current_application_id_handler),
                "GetCurrentApplicationId",
            ),
            (310, None, "IsSystemAppletHomeMenu"),
            (320, Some(Self::set_gpu_time_slice_boost), "SetGpuTimeSliceBoost"),
            (321, None, "SetGpuTimeSliceBoostDueToApplication"),
            (350, Some(Self::unknown350), "Unknown350"),
        ]);
        Self {
            system,
            applet: Some(applet),
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn set_gpu_time_slice_boost(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let time_span = RequestParser::new(ctx).pop_i64();
        log::warn!("(STUBBED) SetGpuTimeSliceBoost called, time_span={}", time_span);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn set_display_magnification_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IAppletCommonFunctions) };
        let mut rp = RequestParser::new(ctx);
        let x = rp.pop_f32();
        let y = rp.pop_f32();
        let width = rp.pop_f32();
        let height = rp.pop_f32();
        if let Some(applet) = &service.applet {
            applet.lock().unwrap().display_magnification =
                common::math_util::Rectangle::new(x, y, x + width, y + height);
        }
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn unknown350(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) Unknown350 called");
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u16(0);
    }

    /// Port of IAppletCommonFunctions::SetHomeButtonDoubleClickEnabled
    pub fn set_home_button_double_click_enabled(&self, _enabled: bool) {
        log::warn!("(STUBBED) SetHomeButtonDoubleClickEnabled called");
    }

    /// Port of IAppletCommonFunctions::GetHomeButtonDoubleClickEnabled
    pub fn get_home_button_double_click_enabled(&self) -> bool {
        log::warn!("(STUBBED) GetHomeButtonDoubleClickEnabled called");
        false
    }

    /// Port of IAppletCommonFunctions::SetCpuBoostRequestPriority
    pub fn set_cpu_boost_request_priority(&self, _priority: i32) {
        log::debug!(
            "SetCpuBoostRequestPriority called with priority={}",
            _priority
        );
        if let Some(ref applet) = self.applet {
            applet.lock().unwrap().cpu_boost_request_priority = _priority;
        }
    }

    /// Port of IAppletCommonFunctions::GetCurrentApplicationId
    pub fn get_current_application_id(&self) -> u64 {
        let program_id = if self.system.is_null() {
            0
        } else {
            self.system.get().get_application_process_program_id()
        };
        log::debug!("GetCurrentApplicationId: {:016X}", program_id & !0xFFF);
        program_id & !0xFFF
    }

    fn set_home_button_double_click_enabled_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IAppletCommonFunctions) };
        let mut rp = RequestParser::new(ctx);
        let enabled = rp.pop_bool();
        service.set_home_button_double_click_enabled(enabled);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_home_button_double_click_enabled_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IAppletCommonFunctions) };
        let enabled = service.get_home_button_double_click_enabled();

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(enabled);
    }

    fn set_cpu_boost_request_priority_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IAppletCommonFunctions) };
        let mut rp = RequestParser::new(ctx);
        let priority = rp.pop_u32() as i32;
        service.set_cpu_boost_request_priority(priority);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_current_application_id_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IAppletCommonFunctions) };
        let app_id = service.get_current_application_id();

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(app_id);
    }
}

impl SessionRequestHandler for IAppletCommonFunctions {
    fn handle_sync_request(&self, context: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, context)
    }

    fn service_name(&self) -> &str {
        "am::IAppletCommonFunctions"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::SystemRef;
    use crate::hle::service::am::applet::Applet;
    use crate::hle::service::os::process::Process;
    use std::sync::{Arc, Mutex};

    #[test]
    fn current_application_id_does_not_report_the_library_applet_program() {
        let mut system = Box::new(crate::core::System::new_for_test());
        let mut process = crate::hle::kernel::k_process::KProcess::new();
        process.program_id = 0x1234_5678_9abc_def0;
        system.set_current_process_arc(Arc::new(crate::hle::kernel::k_process::ProcessLock::new(process)));
        let system_ref = SystemRef::from_ref(&system);
        let mut applet = Applet::new(SystemRef::null(), Process::new(), false);
        applet.program_id = 0x4321_0000_0000_1111;
        let service = IAppletCommonFunctions::with_applet(system_ref, Arc::new(Mutex::new(applet)));
        let mut ctx = HLERequestContext::new();
        service.handlers()[&300].handler_callback.unwrap()(&service, &mut ctx);
        assert_eq!(ctx.cmd_buf[6], RESULT_SUCCESS.get_inner_value());
        assert_eq!(ctx.cmd_buf[8], 0x9abc_d000);
        assert_eq!(ctx.cmd_buf[9], 0x1234_5678);
    }

    #[test]
    fn display_magnification_preserves_signed_extents_and_applet_ownership() {
        let applet = Arc::new(Mutex::new(Applet::new(SystemRef::null(), Process::new(), false)));
        assert_eq!(applet.lock().unwrap().display_magnification,
            common::math_util::Rectangle::new(0.0, 0.0, 1.0, 1.0));
        let service = IAppletCommonFunctions::with_applet(SystemRef::null(), applet.clone());
        let mut ctx = HLERequestContext::new();
        ctx.cmd_buf[2..6].copy_from_slice(&[-2.0_f32, 3.0, 0.5, -4.0].map(f32::to_bits));
        service.handlers()[&42].handler_callback.unwrap()(&service, &mut ctx);
        assert_eq!(ctx.cmd_buf[6], RESULT_SUCCESS.get_inner_value());
        assert_eq!(applet.lock().unwrap().display_magnification,
            common::math_util::Rectangle::new(-2.0, 3.0, -1.5, -1.0));
    }

    #[test]
    fn firmware_common_commands_return_upstream_payloads_in_both_constructors() {
        let applet = Arc::new(Mutex::new(Applet::new(SystemRef::null(), Process::new(), false)));
        for service in [IAppletCommonFunctions::new(), IAppletCommonFunctions::with_applet(SystemRef::null(), applet)] {
            let mut ctx = HLERequestContext::new();
            ctx.cmd_buf.fill(u32::MAX);
            service.handlers()[&350].handler_callback.unwrap()(&service, &mut ctx);
            assert_eq!(ctx.cmd_buf[6], RESULT_SUCCESS.get_inner_value());
            assert_eq!(ctx.cmd_buf[8], 0);
            assert_eq!(ctx.cmd_buf[0], 0);
            for time_span in [0_i64, -1, i64::MIN, i64::MAX] {
                let mut ctx = HLERequestContext::new();
                ctx.cmd_buf[2] = time_span as u32;
                ctx.cmd_buf[3] = (time_span as u64 >> 32) as u32;
                service.handlers()[&320].handler_callback.unwrap()(&service, &mut ctx);
                assert_eq!(ctx.cmd_buf[6], RESULT_SUCCESS.get_inner_value());
                assert_eq!(ctx.cmd_buf[1] & 0x3ff, 10);
            }
        }
    }
}

impl ServiceFramework for IAppletCommonFunctions {
    fn get_service_name(&self) -> &str {
        "am::IAppletCommonFunctions"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}
