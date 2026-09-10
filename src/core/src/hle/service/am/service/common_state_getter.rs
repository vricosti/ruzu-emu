// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of Eden's `core/hle/service/am/service/common_state_getter.{h,cpp}`.

use crate::core::SystemRef;
use crate::hle::service::am::am_types::{
    AppletId, AppletMessage, FocusState, OperationMode, SystemButtonType,
};
use crate::hle::service::am::applet::Applet;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::hle::result::{ResultCode, RESULT_SUCCESS, RESULT_UNKNOWN};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};
use crate::hle::service::set::settings_types::PlatformRegion;

/// IPC command table for ICommonStateGetter:
/// - 0: GetEventHandle
/// - 1: ReceiveMessage
/// - 2: GetThisAppletKind (unimplemented)
/// - 3: AllowToEnterSleep (unimplemented)
/// - 4: DisallowToEnterSleep (unimplemented)
/// - 5: GetOperationMode
/// - 6: GetPerformanceMode
/// - 7: GetCradleStatus (unimplemented)
/// - 8: GetBootMode
/// - 9: GetCurrentFocusState
/// - 10: RequestToAcquireSleepLock
/// - 11: ReleaseSleepLock (unimplemented)
/// - 12: ReleaseSleepLockTransiently (unimplemented)
/// - 13: GetAcquiredSleepLockEvent
/// - 14: GetWakeupCount (unimplemented)
/// - 20: PushToGeneralChannel
/// - 30: GetHomeButtonReaderLockAccessor (unimplemented)
/// - 31: GetReaderLockAccessorEx
/// - 32: GetWriterLockAccessorEx
/// - 40: GetCradleFwVersion (unimplemented)
/// - 50: IsVrModeEnabled
/// - 51: SetVrModeEnabled
/// - 52: SetLcdBacklighOffEnabled
/// - 53: BeginVrModeEx
/// - 54: EndVrModeEx
/// - 55: IsInControllerFirmwareUpdateSection
/// - 59: SetVrPositionForDebug (unimplemented)
/// - 60: GetDefaultDisplayResolution
/// - 61: GetDefaultDisplayResolutionChangeEvent
/// - 62: GetHdcpAuthenticationState (unimplemented)
/// - 63: GetHdcpAuthenticationStateChangeEvent (unimplemented)
/// - 64: SetTvPowerStateMatchingMode (unimplemented)
/// - 65: GetApplicationIdByContentActionName (unimplemented)
/// - 66: SetCpuBoostMode
/// - 67: CancelCpuBoostMode (unimplemented)
/// - 68: GetBuiltInDisplayType
/// - 80: PerformSystemButtonPressingIfInFocus
/// - 90: SetPerformanceConfigurationChangedNotification (unimplemented)
/// - 91: GetCurrentPerformanceConfiguration (unimplemented)
/// - 100: SetHandlingHomeButtonShortPressedEnabled (unimplemented)
/// - 110: OpenMyGpuErrorHandler (unimplemented)
/// - 120: GetAppletLaunchedHistory
/// - 200: GetOperationModeSystemInfo
/// - 300: GetSettingsPlatformRegion
/// - 400: ActivateMigrationService (unimplemented)
/// - 401: DeactivateMigrationService (unimplemented)
/// - 500: DisableSleepTillShutdown (unimplemented)
/// - 501: SuppressDisablingSleepTemporarily (unimplemented)
/// - 502: IsSleepEnabled (unimplemented)
/// - 503: IsDisablingSleepSuppressed (unimplemented)
/// - 610: Unknown610
/// - 611: Unknown611
/// - 900: SetRequestExitToLibraryAppletAtExecuteNextProgramEnabled
pub struct ICommonStateGetter {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
    /// Matches upstream `Core::System& system`.
    system: SystemRef,
    /// Reference to the applet.
    /// Matches upstream `const std::shared_ptr<Applet> m_applet`.
    applet: Arc<Mutex<Applet>>,
}

impl ICommonStateGetter {
    /// Create with an applet reference, matching upstream constructor:
    /// `ICommonStateGetter(Core::System&, std::shared_ptr<Applet>)`
    pub fn new(system: SystemRef, applet: Arc<Mutex<Applet>>) -> Self {
        let handlers = build_handler_map(&[
            (0, Some(Self::get_event_handle_handler), "GetEventHandle"),
            (1, Some(Self::receive_message_handler), "ReceiveMessage"),
            (5, Some(Self::get_operation_mode_handler), "GetOperationMode"),
            (6, Some(Self::get_performance_mode_handler), "GetPerformanceMode"),
            (8, Some(Self::get_boot_mode_handler), "GetBootMode"),
            (9, Some(Self::get_current_focus_state_handler), "GetCurrentFocusState"),
            (10, Some(Self::request_to_acquire_sleep_lock_handler), "RequestToAcquireSleepLock"),
            (13, Some(Self::get_acquired_sleep_lock_event_handler), "GetAcquiredSleepLockEvent"),
            (
                20,
                Some(Self::push_to_general_channel_handler),
                "PushToGeneralChannel",
            ),
            (31, Some(Self::get_reader_lock_accessor_ex_handler), "GetReaderLockAccessorEx"),
            (32, Some(Self::get_writer_lock_accessor_ex_handler), "GetWriterLockAccessorEx"),
            (50, Some(Self::is_vr_mode_enabled_handler), "IsVrModeEnabled"),
            (51, Some(Self::set_vr_mode_enabled_handler), "SetVrModeEnabled"),
            (52, Some(Self::set_lcd_backlight_off_enabled_handler), "SetLcdBacklighOffEnabled"),
            (53, Some(Self::begin_vr_mode_ex_handler), "BeginVrModeEx"),
            (54, Some(Self::end_vr_mode_ex_handler), "EndVrModeEx"),
            (
                55,
                Some(Self::is_in_controller_firmware_update_section_handler),
                "IsInControllerFirmwareUpdateSection",
            ),
            (
                60,
                Some(Self::get_default_display_resolution_handler),
                "GetDefaultDisplayResolution",
            ),
            (
                61,
                Some(Self::get_default_display_resolution_change_event_handler),
                "GetDefaultDisplayResolutionChangeEvent",
            ),
            (66, Some(Self::set_cpu_boost_mode_handler), "SetCpuBoostMode"),
            (68, Some(Self::get_built_in_display_type_handler), "GetBuiltInDisplayType"),
            (
                80,
                Some(Self::perform_system_button_pressing_if_in_focus_handler),
                "PerformSystemButtonPressingIfInFocus",
            ),
            (
                120,
                Some(Self::get_applet_launched_history_handler),
                "GetAppletLaunchedHistory",
            ),
            (
                200,
                Some(Self::get_operation_mode_system_info_handler),
                "GetOperationModeSystemInfo",
            ),
            (
                300,
                Some(Self::get_settings_platform_region_handler),
                "GetSettingsPlatformRegion",
            ),
            (610, Some(Self::unknown610), "Unknown610"),
            (611, Some(Self::unknown611), "Unknown611"),
            (
                900,
                Some(Self::set_request_exit_to_library_applet_at_execute_next_program_enabled_handler),
                "SetRequestExitToLibraryAppletAtExecuteNextProgramEnabled",
            ),
        ]);
        Self {
            handlers,
            handlers_tipc: BTreeMap::new(),
            system,
            applet,
        }
    }

    fn unknown610(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) Unknown610 called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn unknown611(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) Unknown611 called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Port of ICommonStateGetter::GetCurrentFocusState
    /// Matches upstream: locks applet, returns lifecycle_manager.GetAndClearFocusState()
    pub fn get_current_focus_state(&self) -> FocusState {
        log::debug!("GetCurrentFocusState called");
        let mut applet = self.applet.lock().unwrap();
        applet.lifecycle_manager.get_and_clear_focus_state()
    }

    /// Port of ICommonStateGetter::GetOperationMode
    pub fn get_operation_mode(&self) -> OperationMode {
        log::debug!("GetOperationMode called");
        if common::settings::is_docked_mode(&common::settings::values()) {
            OperationMode::Docked
        } else {
            OperationMode::Handheld
        }
    }

    /// Port of ICommonStateGetter::GetPerformanceMode
    pub fn get_performance_mode(&self) -> u32 {
        self.system
            .get()
            .apm_controller()
            .lock()
            .unwrap()
            .get_current_performance_mode()
            .raw() as u32
    }

    /// Port of ICommonStateGetter::IsVrModeEnabled
    pub fn is_vr_mode_enabled(&self) -> bool {
        log::debug!("IsVrModeEnabled called");
        self.applet.lock().unwrap().vr_mode_enabled
    }

    /// Port of ICommonStateGetter::SetVrModeEnabled
    pub fn set_vr_mode_enabled(&self, enabled: bool) {
        let mut applet = self.applet.lock().unwrap();
        applet.vr_mode_enabled = enabled;
        log::warn!(
            "VR Mode is {}",
            if applet.vr_mode_enabled { "on" } else { "off" }
        );
    }

    /// Port of ICommonStateGetter::SetLcdBacklighOffEnabled
    pub fn set_lcd_backlight_off_enabled(&self, enabled: bool) {
        log::warn!(
            "(STUBBED) SetLcdBacklighOffEnabled called, enabled={}",
            enabled
        );
        self.applet.lock().unwrap().lcd_backlight_off_enabled = enabled;
    }

    /// Port of ICommonStateGetter::BeginVrModeEx
    pub fn begin_vr_mode_ex(&self) {
        log::warn!("(STUBBED) BeginVrModeEx called");
        self.applet.lock().unwrap().vr_mode_enabled = true;
    }

    /// Port of ICommonStateGetter::EndVrModeEx
    pub fn end_vr_mode_ex(&self) {
        log::warn!("(STUBBED) EndVrModeEx called");
        self.applet.lock().unwrap().vr_mode_enabled = false;
    }

    /// Port of ICommonStateGetter::IsInControllerFirmwareUpdateSection
    pub fn is_in_controller_firmware_update_section(&self) -> bool {
        log::info!("IsInControllerFirmwareUpdateSection called");
        false
    }

    /// Port of ICommonStateGetter::GetDefaultDisplayResolution
    pub fn get_default_display_resolution(&self) -> (i32, i32) {
        log::debug!("GetDefaultDisplayResolution called");
        if common::settings::is_docked_mode(&common::settings::values()) {
            (1920, 1080)
        } else {
            (1280, 720)
        }
    }

    /// Port of ICommonStateGetter::GetBuiltInDisplayType
    pub fn get_built_in_display_type(&self) -> i32 {
        log::warn!("(STUBBED) GetBuiltInDisplayType called");
        0
    }

    /// Port of ICommonStateGetter::PerformSystemButtonPressingIfInFocus
    pub fn perform_system_button_pressing_if_in_focus(&self, _button_type: SystemButtonType) {
        log::warn!("(STUBBED) PerformSystemButtonPressingIfInFocus called");
    }

    /// Port of ICommonStateGetter::GetOperationModeSystemInfo
    pub fn get_operation_mode_system_info(&self) -> u32 {
        log::warn!("(STUBBED) GetOperationModeSystemInfo called");
        0
    }

    /// Port of ICommonStateGetter::GetAppletLaunchedHistory.
    pub fn get_applet_launched_history(&self, max_count: usize) -> Vec<AppletId> {
        log::info!("GetAppletLaunchedHistory called");
        let mut history = Vec::new();
        let mut current = Some(Arc::clone(&self.applet));

        while history.len() < max_count {
            let Some(applet) = current.take() else {
                break;
            };
            let (applet_id, caller) = {
                let applet = applet.lock().unwrap();
                (applet.applet_id, applet.caller_applet.clone())
            };
            history.push(applet_id);
            current = caller.upgrade();
        }

        history
    }

    /// Port of ICommonStateGetter::GetSettingsPlatformRegion
    pub fn get_settings_platform_region(&self) -> PlatformRegion {
        log::info!("GetSettingsPlatformRegion called");
        PlatformRegion::Global
    }

    /// Port of ICommonStateGetter::SetRequestExitToLibraryAppletAtExecuteNextProgramEnabled
    ///
    /// Upstream takes no parameter — it unconditionally sets the flag to true.
    pub fn set_request_exit_to_library_applet_at_execute_next_program_enabled(&self) {
        log::info!("SetRequestExitToLibraryAppletAtExecuteNextProgramEnabled called");
        self.applet
            .lock()
            .unwrap()
            .request_exit_to_library_applet_at_execute_next_program_enabled = true;
    }

    /// Port of `ICommonStateGetter::PushToGeneralChannel` after CMIF has resolved the storage
    /// interface to its byte payload.
    pub fn push_to_general_channel(&self, data: Vec<u8>) {
        log::debug!("ICommonStateGetter::PushToGeneralChannel called");
        self.system.get().push_general_channel_data(data);
    }

    fn push_interface_response(
        ctx: &mut HLERequestContext,
        object: Arc<dyn SessionRequestHandler>,
    ) {
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(object);
    }

    /// GetEventHandle (cmd 0): returns a copy handle to the message event.
    /// Matches upstream: `*out_event = m_applet->lifecycle_manager.GetSystemEvent().GetHandle()`
    fn get_event_handle_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ICommonStateGetter) };
        let object_id = service
            .applet
            .lock()
            .unwrap()
            .lifecycle_manager
            .ensure_system_event_object_id(ctx)
            .unwrap_or(0);
        log::debug!(
            "ICommonStateGetter::GetEventHandle called -> object_id={:#x}",
            object_id
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0); // 1 copy handle
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object_id);
    }

    /// ReceiveMessage (cmd 1): receives an applet message.
    /// Matches upstream: `m_applet->lifecycle_manager.PopMessage(out_applet_message)`
    fn receive_message_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ICommonStateGetter) };
        let mut applet = service.applet.lock().unwrap();
        let mut message = AppletMessage::None;

        if applet.lifecycle_manager.pop_message(&mut message) {
            log::info!("ICommonStateGetter::ReceiveMessage -> {:?}", message);
            let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
            rb.push_result(RESULT_SUCCESS);
            rb.push_u32(message as u32);
        } else {
            log::debug!("ICommonStateGetter::ReceiveMessage -> NoMessages");
            let result_no_messages =
                ResultCode::from_module_description(crate::hle::result::ErrorModule::AM, 3);
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(result_no_messages);
        }
    }

    /// GetPerformanceMode (cmd 6): returns current performance mode.
    /// Matches upstream: `*out = system.GetAPMController().GetCurrentPerformanceMode()`
    fn get_performance_mode_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ICommonStateGetter) };
        log::debug!("ICommonStateGetter::GetPerformanceMode called");
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(service.get_performance_mode());
    }

    /// RequestToAcquireSleepLock (cmd 10): acquires sleep lock immediately.
    /// Matches upstream: signals sleep_lock_event.
    fn request_to_acquire_sleep_lock_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ICommonStateGetter) };
        log::warn!("(STUBBED) RequestToAcquireSleepLock called");
        let _ = {
            let mut applet = service.applet.lock().unwrap();
            applet.ensure_sleep_lock_event(ctx)
        };
        if let Some(thread) = ctx.get_thread() {
            if let Some(process) = thread
                .lock()
                .unwrap()
                .parent
                .as_ref()
                .and_then(|parent| parent.upgrade())
            {
                let mut process = process.lock().unwrap();
                service
                    .applet
                    .lock()
                    .unwrap()
                    .signal_sleep_lock_event(&mut process);
            }
        }
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// GetAcquiredSleepLockEvent (cmd 13): returns event handle for sleep lock.
    /// Matches upstream: returns OutCopyHandle<KReadableEvent> from sleep_lock_event.
    fn get_acquired_sleep_lock_event_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ICommonStateGetter) };
        log::warn!("(STUBBED) GetAcquiredSleepLockEvent called");
        let object_id = service
            .applet
            .lock()
            .unwrap()
            .ensure_sleep_lock_event_object_id(ctx)
            .unwrap_or(0);
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object_id);
    }

    fn pop_domain_storage(ctx: &mut HLERequestContext) -> Option<Vec<u8>> {
        let mut rp = RequestParser::new(ctx);
        let object_id = rp.pop_u32();
        if object_id == 0 {
            log::error!("ICommonStateGetter storage argument is null");
            return None;
        }

        let handler = {
            let manager = ctx.get_manager()?;
            let manager = manager.lock().unwrap();
            if !manager.is_domain() {
                log::error!("ICommonStateGetter storage argument requires domain IPC");
                return None;
            }
            manager.domain_handler(object_id as usize - 1)?.clone()
        };

        let storage = handler
            .as_any()
            .downcast_ref::<super::storage::IStorage>()?;
        Some(storage.get_data())
    }

    fn push_to_general_channel_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ICommonStateGetter) };
        let Some(data) = Self::pop_domain_storage(ctx) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };

        service.push_to_general_channel(data);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_reader_lock_accessor_ex_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut rp = RequestParser::new(ctx);
        let button_type = rp.pop_u32();
        log::info!(
            "ICommonStateGetter::GetReaderLockAccessorEx called, button_type={}",
            button_type
        );
        let owner_process = ctx
            .owner_process_arc()
            .expect("ICommonStateGetter::GetReaderLockAccessorEx requires owner process");
        Self::push_interface_response(
            ctx,
            Arc::new(super::lock_accessor::ILockAccessor::new(owner_process)),
        );
    }

    fn get_writer_lock_accessor_ex_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut rp = RequestParser::new(ctx);
        let button_type = rp.pop_u32();
        log::info!(
            "ICommonStateGetter::GetWriterLockAccessorEx called, button_type={}",
            button_type
        );
        let owner_process = ctx
            .owner_process_arc()
            .expect("ICommonStateGetter::GetWriterLockAccessorEx requires owner process");
        Self::push_interface_response(
            ctx,
            Arc::new(super::lock_accessor::ILockAccessor::new(owner_process)),
        );
    }

    fn get_operation_mode_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ICommonStateGetter) };
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u8(service.get_operation_mode() as u8);
    }

    fn get_boot_mode_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u8(0);
    }

    fn get_current_focus_state_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ICommonStateGetter) };
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u8(service.get_current_focus_state() as u8);
    }

    fn is_vr_mode_enabled_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ICommonStateGetter) };
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(service.is_vr_mode_enabled());
    }

    fn set_vr_mode_enabled_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ICommonStateGetter) };
        let mut rp = RequestParser::new(ctx);
        service.set_vr_mode_enabled(rp.pop_bool());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn set_lcd_backlight_off_enabled_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ICommonStateGetter) };
        let mut rp = RequestParser::new(ctx);
        service.set_lcd_backlight_off_enabled(rp.pop_bool());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn begin_vr_mode_ex_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ICommonStateGetter) };
        service.begin_vr_mode_ex();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn end_vr_mode_ex_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ICommonStateGetter) };
        service.end_vr_mode_ex();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn is_in_controller_firmware_update_section_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ICommonStateGetter) };
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(service.is_in_controller_firmware_update_section());
    }

    fn get_default_display_resolution_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ICommonStateGetter) };
        let (w, h) = service.get_default_display_resolution();
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(w);
        rb.push_i32(h);
    }

    fn get_default_display_resolution_change_event_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ICommonStateGetter) };
        let object_id = service
            .applet
            .lock()
            .unwrap()
            .lifecycle_manager
            .ensure_operation_mode_changed_system_event_object_id(ctx)
            .unwrap_or(0);
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object_id);
    }

    /// SetCpuBoostMode (cmd 66). Mirrors upstream
    /// `ICommonStateGetter::SetCpuBoostMode` in
    /// `am/service/common_state_getter.cpp`:
    /// ```cpp
    /// const auto& sm = system.ServiceManager();
    /// const auto apm_sys = sm.GetService<APM::APM_Sys>("apm:sys");
    /// ASSERT(apm_sys != nullptr);
    /// apm_sys->SetCpuBoostMode(ctx);
    /// ```
    /// Forwards the request to apm:sys's matching handler.
    fn set_cpu_boost_mode_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ICommonStateGetter) };
        let mut rp = RequestParser::new(ctx);
        let mode = crate::hle::service::apm::apm_controller::CpuBoostMode::from_raw(rp.pop_u32());

        if let Some(service_manager) = service.system.get().service_manager() {
            let apm_sys = crate::hle::service::sm::sm::ServiceManager::get_service_blocking(
                &service_manager,
                service.system,
                "apm:sys",
            );
            if let Some(apm) = apm_sys
                .as_any()
                .downcast_ref::<crate::hle::service::apm::apm_interface::ApmSys>()
            {
                apm.set_cpu_boost_mode(mode);
            } else {
                log::warn!(
                    "ICommonStateGetter::SetCpuBoostMode: apm:sys is not ApmSys (downcast failed)"
                );
            }
        }

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_built_in_display_type_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ICommonStateGetter) };
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(service.get_built_in_display_type());
    }

    fn perform_system_button_pressing_if_in_focus_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ICommonStateGetter) };
        let mut rp = RequestParser::new(ctx);
        let button = match rp.pop_i32() {
            1 => SystemButtonType::HomeButtonShortPressing,
            2 => SystemButtonType::HomeButtonLongPressing,
            6 => SystemButtonType::CaptureButtonShortPressing,
            7 => SystemButtonType::CaptureButtonLongPressing,
            _ => SystemButtonType::None,
        };
        service.perform_system_button_pressing_if_in_focus(button);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_operation_mode_system_info_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ICommonStateGetter) };
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(service.get_operation_mode_system_info());
    }

    fn get_applet_launched_history_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ICommonStateGetter) };
        let max_count = ctx.get_write_buffer_size(0) / core::mem::size_of::<AppletId>();
        let history = service.get_applet_launched_history(max_count);
        let raw: Vec<u32> = history.iter().map(|id| *id as u32).collect();
        let bytes = unsafe {
            core::slice::from_raw_parts(
                raw.as_ptr() as *const u8,
                raw.len() * core::mem::size_of::<u32>(),
            )
        };
        ctx.write_buffer(bytes, 0);

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(history.len() as i32);
    }

    fn get_settings_platform_region_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ICommonStateGetter) };
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(service.get_settings_platform_region() as u32);
    }

    fn set_request_exit_to_library_applet_at_execute_next_program_enabled_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ICommonStateGetter) };
        service.set_request_exit_to_library_applet_at_execute_next_program_enabled();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }
}

impl SessionRequestHandler for ICommonStateGetter {
    fn handle_sync_request(&self, context: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, context)
    }
}

impl ServiceFramework for ICommonStateGetter {
    fn get_service_name(&self) -> &str {
        "am::ICommonStateGetter"
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
    use common::settings_enums::ConsoleMode;

    #[test]
    fn exercised_common_state_handlers_are_registered() {
        let service = ICommonStateGetter::new(
            crate::core::SystemRef::null(),
            Arc::new(Mutex::new(Applet::new(
                crate::core::SystemRef::null(),
                crate::hle::service::os::process::Process::new(),
                false,
            ))),
        );

        for cmd in [31u32, 32, 52, 53, 54, 300, 610, 611] {
            assert!(
                service
                    .handlers()
                    .get(&cmd)
                    .and_then(|info| info.handler_callback)
                    .is_some(),
                "cmd {} should have a real handler",
                cmd
            );
        }
    }

    #[test]
    fn firmware_notification_stubs_return_only_success() {
        let service = ICommonStateGetter::new(
            SystemRef::null(),
            Arc::new(Mutex::new(Applet::new(
                SystemRef::null(),
                crate::hle::service::os::process::Process::new(),
                false,
            ))),
        );
        for cmd in [610, 611] {
            let mut ctx = HLERequestContext::new();
            service.handlers[&cmd].handler_callback.unwrap()(&service, &mut ctx);
            assert_eq!(ctx.cmd_buf[4], 0x4f43_4653);
            assert_eq!(&ctx.cmd_buf[6..8], &[0, 0]);
            assert_eq!(ctx.write_size, 8);
        }
    }

    #[test]
    fn get_settings_platform_region_returns_global() {
        let service = ICommonStateGetter::new(
            crate::core::SystemRef::null(),
            Arc::new(Mutex::new(Applet::new(
                crate::core::SystemRef::null(),
                crate::hle::service::os::process::Process::new(),
                false,
            ))),
        );
        assert_eq!(
            service.get_settings_platform_region(),
            PlatformRegion::Global
        );
    }

    #[test]
    fn push_to_general_channel_uses_system_owner() {
        let system = Box::new(crate::core::System::new());
        let system_ref = crate::core::SystemRef::from_ref(&system);
        let service = ICommonStateGetter::new(
            system_ref,
            Arc::new(Mutex::new(Applet::new(
                system_ref,
                crate::hle::service::os::process::Process::new(),
                false,
            ))),
        );

        assert!(service
            .handlers
            .get(&20)
            .unwrap()
            .handler_callback
            .is_some());
        service.push_to_general_channel(vec![0x48, 0x42]);
        assert_eq!(system.try_pop_general_channel(), Some(vec![0x48, 0x42]));
    }

    #[test]
    fn vr_mode_ex_handlers_toggle_vr_mode() {
        let service = ICommonStateGetter::new(
            crate::core::SystemRef::null(),
            Arc::new(Mutex::new(Applet::new(
                crate::core::SystemRef::null(),
                crate::hle::service::os::process::Process::new(),
                false,
            ))),
        );
        assert!(!service.is_vr_mode_enabled());
        service.begin_vr_mode_ex();
        assert!(service.is_vr_mode_enabled());
        service.end_vr_mode_ex();
        assert!(!service.is_vr_mode_enabled());
    }

    #[test]
    fn get_performance_mode_uses_system_apm_controller() {
        let system = crate::core::System::new();
        let (old_using_global, old_mode) = {
            let values = common::settings::values();
            (
                values.use_docked_mode.using_global(),
                *values.use_docked_mode.get_value(),
            )
        };
        let service = ICommonStateGetter::new(
            crate::core::SystemRef::from_ref(&system),
            Arc::new(Mutex::new(Applet::new(
                crate::core::SystemRef::null(),
                crate::hle::service::os::process::Process::new(),
                false,
            ))),
        );

        {
            let mut values = common::settings::values_mut();
            values.use_docked_mode.set_global(false);
            values.use_docked_mode.set_value(ConsoleMode::Docked);
        }
        assert_eq!(service.get_performance_mode(), 1);

        {
            let mut values = common::settings::values_mut();
            values.use_docked_mode.set_value(ConsoleMode::Handheld);
        }
        assert_eq!(service.get_performance_mode(), 0);

        {
            let mut values = common::settings::values_mut();
            values.use_docked_mode.set_global(false);
            values.use_docked_mode.set_value(old_mode);
            values.use_docked_mode.set_global(old_using_global);
        }
    }
}
