// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of Eden's `core/hle/service/am/service/self_controller.{h,cpp}`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::core::SystemRef;
use crate::hle::kernel::k_process::ProcessLock;
use crate::hle::result::{ErrorModule, ResultCode, RESULT_SUCCESS};
use crate::hle::service::am::am_types::AppletIdentityInfo;
use crate::hle::service::caps::caps_su::IScreenShotApplicationService;
use crate::hle::service::caps::caps_types::{AlbumImageOrientation, AlbumReportOption};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

const RESULT_FATAL_SECTION_COUNT_IMBALANCE: ResultCode =
    ResultCode::from_module_description(ErrorModule::AM, 512);

/// IPC command table for ISelfController:
/// - 0: Exit
/// - 1: LockExit
/// - 2: UnlockExit
/// - 3: EnterFatalSection
/// - 4: LeaveFatalSection
/// - 9: GetLibraryAppletLaunchableEvent
/// - 10: SetScreenShotPermission
/// - 11: SetOperationModeChangedNotification
/// - 12: SetPerformanceModeChangedNotification
/// - 13: SetFocusHandlingMode
/// - 14: SetRestartMessageEnabled
/// - 15: SetScreenShotAppletIdentityInfo
/// - 16: SetOutOfFocusSuspendingEnabled
/// - 19: SetAlbumImageOrientation
/// - 40: CreateManagedDisplayLayer
/// - 41: IsSystemBufferSharingEnabled
/// - 42: GetSystemSharedLayerHandle
/// - 43: GetSystemSharedBufferHandle
/// - 44: CreateManagedDisplaySeparableLayer
/// - 50: SetHandlesRequestToDisplay
/// - 51: ApproveToDisplay
/// - 60: OverrideAutoSleepTimeAndDimmingTime
/// - 61: SetMediaPlaybackState
/// - 62: SetIdleTimeDetectionExtension
/// - 63: GetIdleTimeDetectionExtension
/// - 65: ReportUserIsActive
/// - 67: IsIlluminanceAvailable
/// - 68: SetAutoSleepDisabled
/// - 69: IsAutoSleepDisabled
/// - 72: SetInputDetectionPolicy
/// - 90: GetAccumulatedSuspendedTickValue
/// - 91: GetAccumulatedSuspendedTickChangedEvent
/// - 100: SetAlbumImageTakenNotificationEnabled
/// - 120: SaveCurrentScreenshot
/// - 130: SetRecordVolumeMuted
/// - 230: Unknown230
/// - 240: Unknown240
pub struct ISelfController {
    /// Matches upstream `Core::System& system`.
    system: SystemRef,
    /// Reference to the applet.
    /// Matches upstream `const std::shared_ptr<Applet> m_applet`.
    applet: Arc<Mutex<crate::hle::service::am::applet::Applet>>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ISelfController {
    pub fn new(
        system: SystemRef,
        applet: Arc<Mutex<crate::hle::service::am::applet::Applet>>,
        process: Option<Arc<ProcessLock>>,
    ) -> Self {
        {
            let mut applet_guard = applet.lock().unwrap();
            let applet_id = applet_guard.applet_id;
            let mode = applet_guard.library_applet_mode;
            let process_for_display = process
                .clone()
                .or_else(|| applet_guard.process.get_process());
            if let Some(process_for_display) = process_for_display {
                applet_guard.display_layer_manager.initialize(
                    system,
                    process_for_display,
                    applet_id,
                    mode,
                );
            }
        }
        let handlers = build_handler_map(&[
            (0, Some(Self::exit_handler), "Exit"),
            (1, Some(Self::lock_exit_handler), "LockExit"),
            (2, Some(Self::unlock_exit_handler), "UnlockExit"),
            (
                3,
                Some(Self::enter_fatal_section_handler),
                "EnterFatalSection",
            ),
            (
                4,
                Some(Self::leave_fatal_section_handler),
                "LeaveFatalSection",
            ),
            (
                9,
                Some(Self::get_library_applet_launchable_event_handler),
                "GetLibraryAppletLaunchableEvent",
            ),
            (
                10,
                Some(Self::set_screen_shot_permission_handler),
                "SetScreenShotPermission",
            ),
            (
                11,
                Some(Self::set_operation_mode_changed_notification_handler),
                "SetOperationModeChangedNotification",
            ),
            (
                12,
                Some(Self::set_performance_mode_changed_notification_handler),
                "SetPerformanceModeChangedNotification",
            ),
            (
                13,
                Some(Self::set_focus_handling_mode_handler),
                "SetFocusHandlingMode",
            ),
            (
                14,
                Some(Self::set_restart_message_enabled_handler),
                "SetRestartMessageEnabled",
            ),
            (
                15,
                Some(Self::set_screen_shot_applet_identity_info_handler),
                "SetScreenShotAppletIdentityInfo",
            ),
            (
                16,
                Some(Self::set_out_of_focus_suspending_enabled_handler),
                "SetOutOfFocusSuspendingEnabled",
            ),
            (17, None, "SetControllerFirmwareUpdateSection"),
            (18, None, "SetRequiresCaptureButtonShortPressedMessage"),
            (
                19,
                Some(Self::set_album_image_orientation_handler),
                "SetAlbumImageOrientation",
            ),
            (20, None, "SetDesirableKeyboardLayout"),
            (21, None, "GetScreenShotProgramId"),
            (
                40,
                Some(Self::create_managed_display_layer_handler),
                "CreateManagedDisplayLayer",
            ),
            (
                41,
                Some(Self::is_system_buffer_sharing_enabled_handler),
                "IsSystemBufferSharingEnabled",
            ),
            (
                42,
                Some(Self::get_system_shared_layer_handle_handler),
                "GetSystemSharedLayerHandle",
            ),
            (
                43,
                Some(Self::get_system_shared_buffer_handle_handler),
                "GetSystemSharedBufferHandle",
            ),
            (
                44,
                Some(Self::create_managed_display_separable_layer_handler),
                "CreateManagedDisplaySeparableLayer",
            ),
            (45, None, "SetManagedDisplayLayerSeparationMode"),
            (46, None, "SetRecordingLayerCompositionEnabled"),
            (
                50,
                Some(Self::set_handles_request_to_display_handler),
                "SetHandlesRequestToDisplay",
            ),
            (
                51,
                Some(Self::approve_to_display_handler),
                "ApproveToDisplay",
            ),
            (
                60,
                Some(Self::override_auto_sleep_time_and_dimming_time_handler),
                "OverrideAutoSleepTimeAndDimmingTime",
            ),
            (
                61,
                Some(Self::set_media_playback_state_handler),
                "SetMediaPlaybackState",
            ),
            (
                62,
                Some(Self::set_idle_time_detection_extension_handler),
                "SetIdleTimeDetectionExtension",
            ),
            (
                63,
                Some(Self::get_idle_time_detection_extension_handler),
                "GetIdleTimeDetectionExtension",
            ),
            (64, None, "SetInputDetectionSourceSet"),
            (
                65,
                Some(Self::report_user_is_active_handler),
                "ReportUserIsActive",
            ),
            (66, None, "GetCurrentIlluminance"),
            (
                67,
                Some(Self::is_illuminance_available_handler),
                "IsIlluminanceAvailable",
            ),
            (
                68,
                Some(Self::set_auto_sleep_disabled_handler),
                "SetAutoSleepDisabled",
            ),
            (
                69,
                Some(Self::is_auto_sleep_disabled_handler),
                "IsAutoSleepDisabled",
            ),
            (70, None, "ReportMultimediaError"),
            (71, None, "GetCurrentIlluminanceEx"),
            (
                72,
                Some(Self::set_input_detection_policy_handler),
                "SetInputDetectionPolicy",
            ),
            (80, None, "SetWirelessPriorityMode"),
            (
                90,
                Some(Self::get_accumulated_suspended_tick_value_handler),
                "GetAccumulatedSuspendedTickValue",
            ),
            (
                91,
                Some(Self::get_accumulated_suspended_tick_changed_event_handler),
                "GetAccumulatedSuspendedTickChangedEvent",
            ),
            (
                100,
                Some(Self::set_album_image_taken_notification_enabled_handler),
                "SetAlbumImageTakenNotificationEnabled",
            ),
            (110, None, "SetApplicationAlbumUserData"),
            (
                120,
                Some(Self::save_current_screenshot_handler),
                "SaveCurrentScreenshot",
            ),
            (
                130,
                Some(Self::set_record_volume_muted_handler),
                "SetRecordVolumeMuted",
            ),
            (230, Some(Self::unknown_230_handler), "Unknown230"),
            (240, Some(Self::unknown_240_handler), "Unknown240"),
            (1000, None, "GetDebugStorageChannel"),
        ]);
        Self {
            system,
            applet,
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// Port of ISelfController::Exit
    pub fn exit(&self) {
        log::debug!("Exit called");
        self.applet.lock().unwrap().process.terminate();
    }

    /// Port of ISelfController::LockExit
    pub fn lock_exit(&self) {
        log::debug!("LockExit called");
        let mut applet = self.applet.lock().unwrap();
        if applet.lifecycle_manager.get_exit_requested() {
            applet.process.terminate();
        } else {
            applet.exit_locked = true;
            if !self.system.is_null() {
                self.system.get().set_exit_locked(true);
            }
        }
    }

    /// Port of ISelfController::UnlockExit
    pub fn unlock_exit(&self) {
        log::debug!("UnlockExit called");
        let mut applet = self.applet.lock().unwrap();
        applet.exit_locked = false;
        if !self.system.is_null() {
            self.system.get().set_exit_locked(false);
        }
        if applet.lifecycle_manager.get_exit_requested() {
            applet.process.terminate();
        }
    }

    /// Port of ISelfController::SetOperationModeChangedNotification
    pub fn set_operation_mode_changed_notification(&self, enabled: bool) {
        log::info!(
            "SetOperationModeChangedNotification called, enabled={}",
            enabled
        );
        let mut applet = self.applet.lock().unwrap();
        applet
            .lifecycle_manager
            .set_operation_mode_changed_notification_enabled(enabled);
    }

    /// Port of ISelfController::SetPerformanceModeChangedNotification
    pub fn set_performance_mode_changed_notification(&self, enabled: bool) {
        log::info!(
            "SetPerformanceModeChangedNotification called, enabled={}",
            enabled
        );
        let mut applet = self.applet.lock().unwrap();
        applet
            .lifecycle_manager
            .set_performance_mode_changed_notification_enabled(enabled);
    }

    /// Port of ISelfController::SetFocusHandlingMode
    pub fn set_focus_handling_mode(&self, notify: bool, background: bool, suspend: bool) {
        log::info!(
            "SetFocusHandlingMode called, notify={} background={} suspend={}",
            notify,
            background,
            suspend
        );
        let mut applet = self.applet.lock().unwrap();
        applet
            .lifecycle_manager
            .set_focus_state_changed_notification_enabled(notify);
        applet.lifecycle_manager.set_focus_handling_mode(suspend);
        applet.update_suspension_state_locked(true);
    }

    /// Port of ISelfController::SetOutOfFocusSuspendingEnabled
    pub fn set_out_of_focus_suspending_enabled(&self, enabled: bool) {
        log::info!("SetOutOfFocusSuspendingEnabled called, enabled={}", enabled);
        let mut applet = self.applet.lock().unwrap();
        applet
            .lifecycle_manager
            .set_out_of_focus_suspending_enabled(enabled);
        applet.update_suspension_state_locked(false);
    }

    /// Port of ISelfController::SetHandlesRequestToDisplay
    pub fn set_handles_request_to_display(&self, enable: bool) {
        log::warn!(
            "(STUBBED) SetHandlesRequestToDisplay called, enable={}",
            enable
        );
    }

    /// Port of ISelfController::ApproveToDisplay
    pub fn approve_to_display(&self) {
        log::warn!("(STUBBED) ApproveToDisplay called");
    }

    /// Port of ISelfController::SetAutoSleepDisabled
    pub fn set_auto_sleep_disabled(&self, is_auto_sleep_disabled: bool) {
        log::debug!(
            "SetAutoSleepDisabled called, is_auto_sleep_disabled={}",
            is_auto_sleep_disabled
        );
    }

    /// Port of ISelfController::IsAutoSleepDisabled
    pub fn is_auto_sleep_disabled(&self) -> bool {
        log::debug!("IsAutoSleepDisabled called");
        false
    }

    /// Port of ISelfController::ReportUserIsActive
    pub fn report_user_is_active(&self) {
        log::warn!("(STUBBED) ReportUserIsActive called");
    }

    fn exit_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        service.exit();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn lock_exit_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        service.lock_exit();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn unlock_exit_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        service.unlock_exit();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn enter_fatal_section_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        let mut applet = service.applet.lock().unwrap();
        applet.fatal_section_count += 1;
        log::debug!(
            "EnterFatalSection: fatal_section_count={}",
            applet.fatal_section_count
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn leave_fatal_section_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        let mut applet = service.applet.lock().unwrap();
        let result = if applet.fatal_section_count > 0 {
            applet.fatal_section_count -= 1;
            RESULT_SUCCESS
        } else {
            RESULT_FATAL_SECTION_COUNT_IMBALANCE
        };
        log::debug!(
            "LeaveFatalSection: fatal_section_count={} result=0x{:08X}",
            applet.fatal_section_count,
            result.0
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    fn set_operation_mode_changed_notification_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        let mut rp = RequestParser::new(ctx);
        let enabled = rp.pop_bool();
        service.set_operation_mode_changed_notification(enabled);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn set_performance_mode_changed_notification_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        let mut rp = RequestParser::new(ctx);
        let enabled = rp.pop_bool();
        service.set_performance_mode_changed_notification(enabled);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// SetFocusHandlingMode (cmd 13).
    /// Matches upstream: locks applet, sets notification enabled + handling mode,
    /// calls UpdateSuspensionStateLocked.
    fn set_focus_handling_mode_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        let mut rp = RequestParser::new(ctx);
        // CMIF packs consecutive bools as bytes in one u32 word.
        // Upstream D<> macro reads them byte-level via CmifReplyWrap.
        let packed = rp.pop_u32();
        let notify = (packed & 0xFF) != 0;
        let _background = ((packed >> 8) & 0xFF) != 0;
        let suspend = ((packed >> 16) & 0xFF) != 0;
        log::info!(
            "SetFocusHandlingMode: notify={} background={} suspend={}",
            notify,
            _background,
            suspend
        );

        service.set_focus_handling_mode(notify, _background, suspend);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// GetLibraryAppletLaunchableEvent (cmd 9).
    /// Matches upstream: signals event, returns handle.
    fn get_library_applet_launchable_event_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        log::info!("GetLibraryAppletLaunchableEvent called");
        let object_id = {
            let mut applet = service.applet.lock().unwrap();
            applet
                .ensure_library_applet_launchable_event_object_id(ctx)
                .unwrap_or(0)
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
                    .signal_library_applet_launchable_event(&mut process);
            }
        }
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object_id);
    }

    /// SetScreenShotPermission (cmd 10).
    /// Matches upstream: locks applet, stores permission.
    fn set_screen_shot_permission_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        let mut rp = RequestParser::new(ctx);
        let permission = rp.pop_u32();
        log::debug!("SetScreenShotPermission: permission={}", permission);

        let mut applet = service.applet.lock().unwrap();
        applet.screenshot_permission = match permission {
            0 => crate::hle::service::am::am_types::ScreenshotPermission::Inherit,
            1 => crate::hle::service::am::am_types::ScreenshotPermission::Enable,
            2 => crate::hle::service::am::am_types::ScreenshotPermission::Disable,
            _ => crate::hle::service::am::am_types::ScreenshotPermission::Inherit,
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// SetRestartMessageEnabled (cmd 14).
    /// Matches upstream: locks applet, sets resume notification.
    fn set_restart_message_enabled_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        let mut rp = RequestParser::new(ctx);
        let enabled = rp.pop_bool();
        log::info!("SetRestartMessageEnabled: enabled={}", enabled);

        let mut applet = service.applet.lock().unwrap();
        applet
            .lifecycle_manager
            .set_resume_notification_enabled(enabled);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn set_screen_shot_applet_identity_info_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        let mut rp = RequestParser::new(ctx);
        let applet_id = rp.pop_u32();
        let _padding = rp.pop_u32();
        let application_id = rp.pop_u64();
        let mut applet = service.applet.lock().unwrap();
        applet.screen_shot_identity = AppletIdentityInfo {
            applet_id,
            _padding: [0; 0x4],
            application_id,
        };
        log::warn!("(STUBBED) SetScreenShotAppletIdentityInfo called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// SetOutOfFocusSuspendingEnabled (cmd 16).
    /// Matches upstream: locks applet, sets out-of-focus suspending.
    fn set_out_of_focus_suspending_enabled_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        let mut rp = RequestParser::new(ctx);
        let enabled = rp.pop_bool();
        log::info!("SetOutOfFocusSuspendingEnabled: enabled={}", enabled);

        service.set_out_of_focus_suspending_enabled(enabled);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn set_album_image_orientation_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        let mut rp = RequestParser::new(ctx);
        let orientation = match rp.pop_u32() {
            1 => AlbumImageOrientation::Rotate90,
            2 => AlbumImageOrientation::Rotate180,
            3 => AlbumImageOrientation::Rotate270,
            _ => AlbumImageOrientation::None,
        };
        service.applet.lock().unwrap().album_image_orientation = orientation;
        log::warn!(
            "(STUBBED) SetAlbumImageOrientation called, orientation={:?}",
            orientation
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// CreateManagedDisplayLayer (cmd 40).
    /// Upstream calls `m_applet->display_layer_manager.CreateManagedDisplayLayer(&layer_id)`.
    fn create_managed_display_layer_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        let result = service
            .applet
            .lock()
            .unwrap()
            .display_layer_manager
            .create_managed_display_layer();

        match result {
            Ok(layer_id) => {
                log::info!("CreateManagedDisplayLayer: layer_id={}", layer_id);
                let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_u64(layer_id);
            }
            Err(err) => {
                log::error!("CreateManagedDisplayLayer failed: {:?}", err);
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(err);
            }
        }
    }

    fn is_system_buffer_sharing_enabled_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        let mut applet = service.applet.lock().unwrap();
        let result = applet
            .display_layer_manager
            .is_system_buffer_sharing_enabled();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    fn get_system_shared_buffer_handle_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        let mut applet = service.applet.lock().unwrap();
        let result = applet
            .display_layer_manager
            .get_system_shared_layer_handle()
            .map(|(buffer_id, _layer_id)| buffer_id);

        match result {
            Ok(buffer_id) => {
                let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_u64(buffer_id);
            }
            Err(err) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(err);
            }
        }
    }

    fn get_system_shared_layer_handle_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        let mut applet = service.applet.lock().unwrap();
        let result = applet
            .display_layer_manager
            .get_system_shared_layer_handle();

        match result {
            Ok((buffer_id, layer_id)) => {
                let mut rb = ResponseBuilder::new(ctx, 6, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_u64(buffer_id);
                rb.push_u64(layer_id);
            }
            Err(err) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(err);
            }
        }
    }

    fn create_managed_display_separable_layer_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        let result = service
            .applet
            .lock()
            .unwrap()
            .display_layer_manager
            .create_managed_display_separable_layer();

        match result {
            Ok((layer_id, recording_layer_id)) => {
                let mut rb = ResponseBuilder::new(ctx, 6, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_u64(layer_id);
                rb.push_u64(recording_layer_id);
            }
            Err(err) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(err);
            }
        }
    }

    fn set_handles_request_to_display_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        let mut rp = RequestParser::new(ctx);
        let enable = rp.pop_bool();
        service.set_handles_request_to_display(enable);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn approve_to_display_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        service.approve_to_display();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn override_auto_sleep_time_and_dimming_time_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut rp = RequestParser::new(ctx);
        let a = rp.pop_i32();
        let b = rp.pop_i32();
        let c = rp.pop_i32();
        let d = rp.pop_i32();
        log::warn!(
            "(STUBBED) OverrideAutoSleepTimeAndDimmingTime called, a={} b={} c={} d={}",
            a,
            b,
            c,
            d
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn set_media_playback_state_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let state = rp.pop_bool();
        log::warn!("(STUBBED) SetMediaPlaybackState called, state={}", state);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// SetIdleTimeDetectionExtension (cmd 62).
    /// Matches upstream: locks applet, stores value.
    fn set_idle_time_detection_extension_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        let mut rp = RequestParser::new(ctx);
        let extension = rp.pop_u32();
        log::debug!("SetIdleTimeDetectionExtension: extension={}", extension);

        let mut applet = service.applet.lock().unwrap();
        applet.idle_time_detection_extension = match extension {
            0 => crate::hle::service::am::am_types::IdleTimeDetectionExtension::Disabled,
            1 => crate::hle::service::am::am_types::IdleTimeDetectionExtension::Extended,
            2 => crate::hle::service::am::am_types::IdleTimeDetectionExtension::ExtendedUnsafe,
            _ => crate::hle::service::am::am_types::IdleTimeDetectionExtension::Disabled,
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// GetIdleTimeDetectionExtension (cmd 63).
    /// Matches upstream: locks applet, reads value.
    fn get_idle_time_detection_extension_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        let applet = service.applet.lock().unwrap();
        let extension = applet.idle_time_detection_extension as u32;
        log::debug!("GetIdleTimeDetectionExtension: extension={}", extension);

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(extension);
    }

    fn report_user_is_active_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        service.report_user_is_active();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn is_illuminance_available_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) IsIlluminanceAvailable called");
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(false);
    }

    /// SetAutoSleepDisabled (cmd 68).
    /// Matches upstream: locks applet, stores bool.
    fn set_auto_sleep_disabled_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        let mut rp = RequestParser::new(ctx);
        let disabled = rp.pop_bool();
        log::debug!("SetAutoSleepDisabled: disabled={}", disabled);

        let mut applet = service.applet.lock().unwrap();
        applet.auto_sleep_disabled = disabled;

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn is_auto_sleep_disabled_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        let disabled = service.applet.lock().unwrap().auto_sleep_disabled;
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(disabled);
    }

    fn set_input_detection_policy_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut rp = RequestParser::new(ctx);
        let policy = rp.pop_u32();
        log::warn!(
            "(STUBBED) SetInputDetectionPolicy called, policy={}",
            policy
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_accumulated_suspended_tick_value_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        let ticks = service.applet.lock().unwrap().suspended_ticks;
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(ticks);
    }

    /// GetAccumulatedSuspendedTickChangedEvent (cmd 91).
    /// Matches upstream: returns event handle from applet.
    fn get_accumulated_suspended_tick_changed_event_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        log::debug!("GetAccumulatedSuspendedTickChangedEvent called");
        let object_id = service
            .applet
            .lock()
            .unwrap()
            .ensure_accumulated_suspended_tick_changed_event_object_id(ctx)
            .unwrap_or(0);
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object_id);
    }

    fn set_album_image_taken_notification_enabled_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        let mut rp = RequestParser::new(ctx);
        let enabled = rp.pop_bool();
        service
            .applet
            .lock()
            .unwrap()
            .album_image_taken_notification_enabled = enabled;
        log::warn!(
            "(STUBBED) SetAlbumImageTakenNotificationEnabled called, enabled={}",
            enabled
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn save_current_screenshot_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        let mut rp = RequestParser::new(ctx);
        let report_option = match rp.pop_i32() {
            1 => AlbumReportOption::Enable,
            2 => AlbumReportOption::Unknown2,
            3 => AlbumReportOption::Unknown3,
            _ => AlbumReportOption::Disable,
        };
        log::info!(
            "SaveCurrentScreenshot called, report_option={:?}",
            report_option
        );

        if !service.system.is_null() {
            if let Some(service_manager) = service.system.get().service_manager() {
                if let Some(screenshot_service) =
                    service_manager.lock().unwrap().get_service("caps:su")
                {
                    if let Some(screenshot_service) = screenshot_service
                        .as_any()
                        .downcast_ref::<IScreenShotApplicationService>(
                    ) {
                        screenshot_service.capture_and_save_screenshot(report_option);
                    }
                }
            }
        }

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn set_record_volume_muted_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISelfController) };
        let mut rp = RequestParser::new(ctx);
        let muted = rp.pop_bool();
        service.applet.lock().unwrap().record_volume_muted = muted;
        log::warn!("(STUBBED) SetRecordVolumeMuted called, muted={}", muted);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn unknown_230_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let in_val = rp.pop_u32();
        log::warn!("(STUBBED) Unknown230 called, in_val={in_val}");

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u16(0);
    }

    fn unknown_240_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let in_val = rp.pop_u32();
        log::warn!("(STUBBED) Unknown240 called, in_val={in_val}");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }
}

impl Drop for ISelfController {
    fn drop(&mut self) {
        self.applet.lock().unwrap().display_layer_manager.finalize();
    }
}

impl SessionRequestHandler for ISelfController {
    fn handle_sync_request(&self, context: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, context)
    }
}

impl ServiceFramework for ISelfController {
    fn get_service_name(&self) -> &str {
        "am::ISelfController"
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
    use crate::hle::kernel::k_process::KProcess;
    use crate::hle::service::am::applet::Applet;
    use crate::hle::service::os::process::Process;

    #[test]
    fn display_manager_is_the_process_owner_and_late_commands_are_registered() {
        let system = Box::new(crate::core::System::new());
        let system_ref = SystemRef::from_ref(&system);
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let applet = Arc::new(Mutex::new(Applet::new(system_ref, Process::new(), false)));

        let controller =
            ISelfController::new(system_ref, Arc::clone(&applet), Some(Arc::clone(&process)));

        assert_eq!(Arc::strong_count(&process), 2);
        assert!(controller
            .handlers
            .get(&67)
            .unwrap()
            .handler_callback
            .is_some());
        assert!(controller
            .handlers
            .get(&230)
            .unwrap()
            .handler_callback
            .is_some());

        drop(controller);
        assert_eq!(Arc::strong_count(&process), 1);
    }
}
