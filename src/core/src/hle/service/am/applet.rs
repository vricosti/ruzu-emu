// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/am/applet.h
//! Port of zuyu/src/core/hle/service/am/applet.cpp

use std::sync::{Arc, Mutex, Weak};

use crate::core::SystemRef;
use crate::hle::kernel::k_process::KProcess;
use crate::hle::kernel::k_readable_event::KReadableEvent;
use crate::hle::service::caps::caps_types::AlbumImageOrientation;
use crate::hle::service::hle_ipc::{HLERequestContext, Handle};
use crate::hle::service::os::process::Process;

use super::am_types::*;
use super::applet_data_broker::AppletDataBroker;
use super::display_layer_manager::DisplayLayerManager;
use super::frontend::applets::FrontendApplet;
use super::lifecycle_manager::LifecycleManager;

/// Port of the Applet struct from upstream applet.h.
pub struct Applet {
    // Lifecycle manager
    pub lifecycle_manager: LifecycleManager,

    // Process — upstream: std::unique_ptr<Process> process
    pub process: Process,
    pub is_process_running: bool,

    // Creation state
    pub applet_id: AppletId,
    pub aruid: AppletResourceUserId,
    pub launch_reason: AppletProcessLaunchReason,
    pub applet_type: AppletType,
    pub program_id: ProgramId,
    pub library_applet_mode: LibraryAppletMode,
    pub previous_program_index: i32,
    pub previous_screenshot_permission: ScreenshotPermission,

    pub screen_shot_identity: AppletIdentityInfo,

    // vi state — upstream: DisplayLayerManager display_layer_manager
    pub display_layer_manager: DisplayLayerManager,

    // Applet common functions
    pub terminate_result: u32,
    pub display_logical_width: i32,
    pub display_logical_height: i32,
    pub home_button_double_click_enabled: bool,
    pub home_button_short_pressed_blocked: bool,
    pub home_button_long_pressed_blocked: bool,
    pub vr_mode_curtain_required: bool,
    pub sleep_required_by_high_temperature: bool,
    pub sleep_required_by_low_battery: bool,
    pub cpu_boost_request_priority: i32,
    pub handling_capture_button_short_pressed_message_enabled_for_applet: bool,
    pub handling_capture_button_long_pressed_message_enabled_for_applet: bool,
    pub application_core_usage_mode: u32,

    // Application functions
    pub game_play_recording_supported: bool,
    pub game_play_recording_state: GamePlayRecordingState,
    pub jit_service_launched: bool,
    pub application_crash_report_enabled: bool,

    // Common state
    pub sleep_lock_enabled: bool,
    pub vr_mode_enabled: bool,
    pub lcd_backlight_off_enabled: bool,
    pub request_exit_to_library_applet_at_execute_next_program_enabled: bool,

    // Channels
    pub user_channel_launch_parameter: std::collections::VecDeque<Vec<u8>>,
    pub preselected_user_launch_parameter: std::collections::VecDeque<Vec<u8>>,

    // Caller applet — upstream: std::weak_ptr<Applet> caller_applet
    pub caller_applet: Weak<Mutex<Applet>>,
    pub caller_applet_broker: Option<Arc<AppletDataBroker>>,
    pub frontend: Option<Box<dyn FrontendApplet>>,
    // Child applets — upstream: std::list<std::shared_ptr<Applet>> child_applets
    pub child_applets: Vec<Arc<Mutex<Applet>>>,
    pub is_completed: bool,

    // Self state
    pub exit_locked: bool,
    pub fatal_section_count: i32,
    pub handles_request_to_display: bool,
    pub screenshot_permission: ScreenshotPermission,
    pub album_image_orientation: AlbumImageOrientation,
    pub idle_time_detection_extension: IdleTimeDetectionExtension,
    pub auto_sleep_disabled: bool,
    pub suspended_ticks: u64,
    pub album_image_taken_notification_enabled: bool,
    pub record_volume_muted: bool,
    pub is_activity_runnable: bool,
    pub is_interactible: bool,
    pub window_visible: bool,
    pub overlay_in_foreground: bool,

    // Events
    pub gpu_error_detected_event: Option<Arc<Mutex<KReadableEvent>>>,
    pub gpu_error_detected_event_handle: Option<Handle>,
    pub friend_invitation_storage_channel_event: Option<Arc<Mutex<KReadableEvent>>>,
    pub friend_invitation_storage_channel_event_handle: Option<Handle>,
    pub health_warning_disappeared_system_event: Option<Arc<Mutex<KReadableEvent>>>,
    pub health_warning_disappeared_system_event_handle: Option<Handle>,
    pub unknown_event: Option<Arc<Mutex<KReadableEvent>>>,
    pub unknown_event_handle: Option<Handle>,
    pub library_applet_launchable_event: Option<Arc<Mutex<KReadableEvent>>>,
    pub library_applet_launchable_event_handle: Option<Handle>,
    pub accumulated_suspended_tick_changed_event: Option<Arc<Mutex<KReadableEvent>>>,
    pub accumulated_suspended_tick_changed_event_handle: Option<Handle>,
    pub sleep_lock_event: Option<Arc<Mutex<KReadableEvent>>>,
    pub sleep_lock_event_handle: Option<Handle>,
    pub state_changed_event: Option<Arc<Mutex<KReadableEvent>>>,
    pub state_changed_event_handle: Option<Handle>,

    // HID registration — upstream: HidRegistration hid_registration
    pub hid_registration: super::hid_registration::HidRegistration,
}

impl Applet {
    pub fn new(system: SystemRef, process: Process, is_application: bool) -> Self {
        let process_id = process.get_process_id();
        let program_id = process.get_program_id();
        let hid_registration = super::hid_registration::HidRegistration::new(system, &process);

        Self {
            lifecycle_manager: LifecycleManager::new(is_application),
            process,
            is_process_running: false,
            applet_id: AppletId::default(),
            aruid: AppletResourceUserId { pid: process_id },
            launch_reason: AppletProcessLaunchReason::default(),
            applet_type: AppletType::default(),
            program_id,
            library_applet_mode: LibraryAppletMode::default(),
            previous_program_index: -1,
            previous_screenshot_permission: ScreenshotPermission::Enable,
            screen_shot_identity: AppletIdentityInfo::default(),
            display_layer_manager: DisplayLayerManager::new(),
            terminate_result: 0,
            display_logical_width: 0,
            display_logical_height: 0,
            home_button_double_click_enabled: false,
            home_button_short_pressed_blocked: false,
            home_button_long_pressed_blocked: false,
            vr_mode_curtain_required: false,
            sleep_required_by_high_temperature: false,
            sleep_required_by_low_battery: false,
            cpu_boost_request_priority: -1,
            handling_capture_button_short_pressed_message_enabled_for_applet: false,
            handling_capture_button_long_pressed_message_enabled_for_applet: false,
            application_core_usage_mode: 0,
            game_play_recording_supported: false,
            game_play_recording_state: GamePlayRecordingState::Disabled,
            jit_service_launched: false,
            application_crash_report_enabled: false,
            sleep_lock_enabled: false,
            vr_mode_enabled: false,
            lcd_backlight_off_enabled: false,
            request_exit_to_library_applet_at_execute_next_program_enabled: false,
            user_channel_launch_parameter: std::collections::VecDeque::new(),
            preselected_user_launch_parameter: std::collections::VecDeque::new(),
            caller_applet: Weak::new(),
            caller_applet_broker: None,
            frontend: None,
            child_applets: Vec::new(),
            is_completed: false,
            exit_locked: false,
            fatal_section_count: 0,
            handles_request_to_display: false,
            screenshot_permission: ScreenshotPermission::default(),
            album_image_orientation: AlbumImageOrientation::default(),
            idle_time_detection_extension: IdleTimeDetectionExtension::default(),
            auto_sleep_disabled: false,
            suspended_ticks: 0,
            album_image_taken_notification_enabled: false,
            record_volume_muted: false,
            is_activity_runnable: false,
            is_interactible: true,
            window_visible: true,
            overlay_in_foreground: false,
            gpu_error_detected_event: None,
            gpu_error_detected_event_handle: None,
            friend_invitation_storage_channel_event: None,
            friend_invitation_storage_channel_event_handle: None,
            health_warning_disappeared_system_event: None,
            health_warning_disappeared_system_event_handle: None,
            unknown_event: None,
            unknown_event_handle: None,
            library_applet_launchable_event: None,
            library_applet_launchable_event_handle: None,
            accumulated_suspended_tick_changed_event: None,
            accumulated_suspended_tick_changed_event_handle: None,
            sleep_lock_event: None,
            sleep_lock_event_handle: None,
            state_changed_event: None,
            state_changed_event_handle: None,
            hid_registration,
        }
    }

    fn ensure_persistent_readable_event(
        ctx: &HLERequestContext,
        readable_event: &mut Option<Arc<Mutex<KReadableEvent>>>,
        handle: &mut Option<Handle>,
        signaled: bool,
    ) -> Option<Handle> {
        if let Some(existing) = *handle {
            return Some(existing);
        }

        let (new_handle, new_event) = ctx.create_readable_event(signaled)?;
        *readable_event = Some(new_event);
        *handle = Some(new_handle);
        Some(new_handle)
    }

    fn ensure_persistent_readable_event_object_id(
        ctx: &HLERequestContext,
        readable_event: &mut Option<Arc<Mutex<KReadableEvent>>>,
        signaled: bool,
    ) -> Option<u64> {
        if let Some(existing) = readable_event.as_ref() {
            return ctx.register_readable_event_object(Arc::clone(existing));
        }

        let (object_id, new_event) = ctx.create_readable_event_object(signaled)?;
        *readable_event = Some(new_event);
        Some(object_id)
    }

    fn signal_persistent_readable_event(
        _process: &mut KProcess,
        readable_event: &Arc<Mutex<KReadableEvent>>,
    ) {
        readable_event.lock().unwrap().signal();
    }

    pub fn ensure_sleep_lock_event(&mut self, ctx: &HLERequestContext) -> Option<Handle> {
        Self::ensure_persistent_readable_event(
            ctx,
            &mut self.sleep_lock_event,
            &mut self.sleep_lock_event_handle,
            false,
        )
    }

    pub fn ensure_sleep_lock_event_object_id(&mut self, ctx: &HLERequestContext) -> Option<u64> {
        Self::ensure_persistent_readable_event_object_id(ctx, &mut self.sleep_lock_event, false)
    }

    pub fn signal_sleep_lock_event(&mut self, process: &mut KProcess) {
        if let Some(event) = self.sleep_lock_event.as_ref() {
            Self::signal_persistent_readable_event(process, event);
        }
    }

    pub fn ensure_library_applet_launchable_event(
        &mut self,
        ctx: &HLERequestContext,
    ) -> Option<Handle> {
        Self::ensure_persistent_readable_event(
            ctx,
            &mut self.library_applet_launchable_event,
            &mut self.library_applet_launchable_event_handle,
            false,
        )
    }

    pub fn ensure_library_applet_launchable_event_object_id(
        &mut self,
        ctx: &HLERequestContext,
    ) -> Option<u64> {
        Self::ensure_persistent_readable_event_object_id(
            ctx,
            &mut self.library_applet_launchable_event,
            false,
        )
    }

    pub fn signal_library_applet_launchable_event(&mut self, process: &mut KProcess) {
        if let Some(event) = self.library_applet_launchable_event.as_ref() {
            Self::signal_persistent_readable_event(process, event);
        }
    }

    pub fn signal_state_changed_event_without_process(&mut self) {
        if let Some(event) = self.state_changed_event.as_ref() {
            event.lock().unwrap().signal();
        }
    }

    pub fn ensure_accumulated_suspended_tick_changed_event(
        &mut self,
        ctx: &HLERequestContext,
    ) -> Option<Handle> {
        Self::ensure_persistent_readable_event(
            ctx,
            &mut self.accumulated_suspended_tick_changed_event,
            &mut self.accumulated_suspended_tick_changed_event_handle,
            false,
        )
    }

    pub fn ensure_accumulated_suspended_tick_changed_event_object_id(
        &mut self,
        ctx: &HLERequestContext,
    ) -> Option<u64> {
        Self::ensure_persistent_readable_event_object_id(
            ctx,
            &mut self.accumulated_suspended_tick_changed_event,
            false,
        )
    }

    pub fn ensure_gpu_error_detected_system_event(
        &mut self,
        ctx: &HLERequestContext,
    ) -> Option<Handle> {
        Self::ensure_persistent_readable_event(
            ctx,
            &mut self.gpu_error_detected_event,
            &mut self.gpu_error_detected_event_handle,
            false,
        )
    }

    pub fn ensure_gpu_error_detected_system_event_object_id(
        &mut self,
        ctx: &HLERequestContext,
    ) -> Option<u64> {
        Self::ensure_persistent_readable_event_object_id(
            ctx,
            &mut self.gpu_error_detected_event,
            false,
        )
    }

    pub fn ensure_friend_invitation_storage_channel_event(
        &mut self,
        ctx: &HLERequestContext,
    ) -> Option<Handle> {
        Self::ensure_persistent_readable_event(
            ctx,
            &mut self.friend_invitation_storage_channel_event,
            &mut self.friend_invitation_storage_channel_event_handle,
            false,
        )
    }

    pub fn ensure_friend_invitation_storage_channel_event_object_id(
        &mut self,
        ctx: &HLERequestContext,
    ) -> Option<u64> {
        Self::ensure_persistent_readable_event_object_id(
            ctx,
            &mut self.friend_invitation_storage_channel_event,
            false,
        )
    }

    pub fn ensure_health_warning_disappeared_system_event(
        &mut self,
        ctx: &HLERequestContext,
    ) -> Option<Handle> {
        Self::ensure_persistent_readable_event(
            ctx,
            &mut self.health_warning_disappeared_system_event,
            &mut self.health_warning_disappeared_system_event_handle,
            false,
        )
    }

    pub fn ensure_health_warning_disappeared_system_event_object_id(
        &mut self,
        ctx: &HLERequestContext,
    ) -> Option<u64> {
        Self::ensure_persistent_readable_event_object_id(
            ctx,
            &mut self.health_warning_disappeared_system_event,
            false,
        )
    }

    pub fn ensure_unknown_event(&mut self, ctx: &HLERequestContext) -> Option<Handle> {
        Self::ensure_persistent_readable_event(
            ctx,
            &mut self.unknown_event,
            &mut self.unknown_event_handle,
            false,
        )
    }

    pub fn ensure_unknown_event_object_id(&mut self, ctx: &HLERequestContext) -> Option<u64> {
        Self::ensure_persistent_readable_event_object_id(ctx, &mut self.unknown_event, false)
    }

    /// Port of Applet::UpdateSuspensionStateLocked
    pub fn update_suspension_state_locked(&mut self, force_message: bool) {
        self.lifecycle_manager.remove_force_resume_if_possible();

        let update_requested_focus_state = self.lifecycle_manager.update_requested_focus_state();
        let curr_activity_runnable = self.lifecycle_manager.is_runnable();
        let prev_activity_runnable = self.is_activity_runnable;
        let was_changed = curr_activity_runnable != prev_activity_runnable;

        if was_changed {
            if curr_activity_runnable {
                self.process.suspend(false);
            } else {
                self.process.suspend(true);
                self.lifecycle_manager.request_resume_notification();
            }
            self.is_activity_runnable = curr_activity_runnable;
        }

        if self.lifecycle_manager.get_forced_suspend() {
            return;
        }

        if update_requested_focus_state || was_changed || force_message {
            self.lifecycle_manager.signal_system_event_if_needed();
        }
    }

    /// Port of Applet::SetInteractibleLocked
    pub fn set_interactible_locked(&mut self, interactible: bool) {
        if self.is_interactible == interactible {
            return;
        }
        self.is_interactible = interactible;
        self.hid_registration.enable_applet_to_get_input(
            interactible && !self.lifecycle_manager.get_exit_requested(),
        );
    }

    pub fn ensure_state_changed_event(&mut self, ctx: &HLERequestContext) -> Option<Handle> {
        Self::ensure_persistent_readable_event(
            ctx,
            &mut self.state_changed_event,
            &mut self.state_changed_event_handle,
            false,
        )
    }

    pub fn ensure_state_changed_event_object_id(&mut self, ctx: &HLERequestContext) -> Option<u64> {
        Self::ensure_persistent_readable_event_object_id(ctx, &mut self.state_changed_event, false)
    }

    pub fn signal_state_changed_event(&mut self, process: &mut KProcess) {
        if let Some(event) = self.state_changed_event.as_ref() {
            Self::signal_persistent_readable_event(process, event);
        }
    }

    /// Port of `Applet::OnProcessTerminatedLocked`.
    pub fn on_process_terminated_locked(&mut self) {
        self.is_completed = true;
        self.signal_state_changed_event_without_process();
    }
}

#[cfg(test)]
mod tests {
    use super::Applet;
    use crate::hle::kernel::k_process::ProcessLock;
    use crate::hle::kernel::k_readable_event::KReadableEvent;
    use crate::hle::service::os::process::Process;
    use std::sync::{Arc, Mutex};

    #[test]
    fn new_initializes_aruid_and_program_id_from_process() {
        let process = Process::with_process(Arc::new(ProcessLock::from_value({
            let mut process = crate::hle::kernel::k_process::KProcess::new();
            process.process_id = 0x1234;
            process.program_id = 0x0102_0304_0506_0708;
            process
        })));

        let applet = Applet::new(crate::core::SystemRef::null(), process, false);

        assert_eq!(applet.aruid.pid, 0x1234);
        assert_eq!(applet.program_id, 0x0102_0304_0506_0708);
    }

    #[test]
    fn process_termination_completes_applet_and_signals_state_change() {
        let process = Process::with_process(Arc::new(ProcessLock::from_value(
            crate::hle::kernel::k_process::KProcess::new(),
        )));
        let mut applet = Applet::new(crate::core::SystemRef::null(), process, false);
        let state_changed_event = Arc::new(Mutex::new(KReadableEvent::new()));
        applet.state_changed_event = Some(Arc::clone(&state_changed_event));

        applet.on_process_terminated_locked();

        assert!(applet.is_completed);
        assert!(state_changed_event.lock().unwrap().is_signaled());
    }
}
