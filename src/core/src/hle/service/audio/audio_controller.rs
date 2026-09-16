//! Port of zuyu/src/core/hle/service/audio/audio_controller.h and audio_controller.cpp
//!
//! IAudioController service ("audctl").

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};
use super::errors::RESULT_INVALID_ARGUMENT;
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::cmif_serialization::{CmifRequest, CmifResponse};
use crate::hle::service::hle_ipc::{
    HLERequestContext, SessionRequestHandler, SessionRequestHandlerPtr,
};
use crate::hle::service::kernel_helpers::ServiceContext;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};
use crate::hle::service::set::settings_types::AudioOutputMode;
use crate::hle::service::set::system_settings_server::SystemSettingsService;
use crate::hle::service::sm::sm::ServiceManager;

/// Port of IAudioController::ForceMutePolicy
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ForceMutePolicy {
    Disable = 0,
    SpeakerMuteOnHeadphoneUnplugged = 1,
}

/// Port of IAudioController::HeadphoneOutputLevelMode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum HeadphoneOutputLevelMode {
    Normal = 0,
    HighPower = 1,
}

/// IPC command table for IAudioController ("audctl"):
///
/// | Cmd | Handler                                  | Name                                                  |
/// |-----|------------------------------------------|-------------------------------------------------------|
/// | 0   | nullptr                                  | GetTargetVolume                                       |
/// | 1   | nullptr                                  | SetTargetVolume                                       |
/// | 2   | get_target_volume_min                    | GetTargetVolumeMin                                    |
/// | 3   | get_target_volume_max                    | GetTargetVolumeMax                                    |
/// | 4   | nullptr                                  | IsTargetMute                                          |
/// | 5   | nullptr                                  | SetTargetMute                                         |
/// | 6   | nullptr                                  | IsTargetConnected                                     |
/// | 7   | nullptr                                  | SetDefaultTarget                                      |
/// | 8   | nullptr                                  | GetDefaultTarget                                      |
/// | 9   | get_audio_output_mode                    | GetAudioOutputMode                                    |
/// | 10  | set_audio_output_mode                    | SetAudioOutputMode                                    |
/// | 11  | nullptr                                  | SetForceMutePolicy                                    |
/// | 12  | get_force_mute_policy                    | GetForceMutePolicy                                    |
/// | 13  | get_output_mode_setting                  | GetOutputModeSetting                                  |
/// | 14  | set_output_mode_setting                  | SetOutputModeSetting                                  |
/// | 15  | nullptr                                  | SetOutputTarget                                       |
/// | 16  | nullptr                                  | SetInputTargetForceEnabled                             |
/// | 17  | set_headphone_output_level_mode          | SetHeadphoneOutputLevelMode                           |
/// | 18  | get_headphone_output_level_mode          | GetHeadphoneOutputLevelMode                           |
/// | 19  | nullptr                                  | AcquireAudioVolumeUpdateEventForPlayReport             |
/// | 20  | nullptr                                  | AcquireAudioOutputDeviceUpdateEventForPlayReport       |
/// | 21  | nullptr                                  | GetAudioOutputTargetForPlayReport                      |
/// | 22  | notify_headphone_volume_warning_displayed | NotifyHeadphoneVolumeWarningDisplayedEvent             |
/// | 23  | nullptr                                  | SetSystemOutputMasterVolume                            |
/// | 24  | nullptr                                  | GetSystemOutputMasterVolume                            |
/// | 25  | nullptr                                  | GetAudioVolumeDataForPlayReport                        |
/// | 26  | nullptr                                  | UpdateHeadphoneSettings                                |
/// | 27  | nullptr                                  | SetVolumeMappingTableForDev                            |
/// | 28  | nullptr                                  | GetAudioOutputChannelCountForPlayReport                |
/// | 29  | nullptr                                  | BindAudioOutputChannelCountUpdateEventForPlayReport    |
/// | 30  | set_speaker_auto_mute_enabled            | SetSpeakerAutoMuteEnabled                             |
/// | 31  | is_speaker_auto_mute_enabled             | IsSpeakerAutoMuteEnabled                              |
/// | 32  | nullptr                                  | GetActiveOutputTarget                                 |
/// | 33  | nullptr                                  | GetTargetDeviceInfo                                   |
/// | 34  | acquire_target_notification              | AcquireTargetNotification                             |
/// | 35-42, 10000-10106, 50000 | nullptr            | (various debug/play report commands)                  |
pub struct IAudioController {
    // Rust counterpart of upstream enable_shared_from_this; never owns itself.
    self_reference: Weak<Self>,
    // Upstream fields:
    //   service_context: KernelHelpers::ServiceContext — owns the notification_event lifecycle.
    //   notification_event: Kernel::KEvent* — created via service_context.CreateEvent("IAudioController:NotificationEvent").
    //   m_set_sys: std::shared_ptr<Service::Set::ISystemSettingsServer> — obtained via
    //     system.ServiceManager().GetService<Service::Set::ISystemSettingsServer>("set:sys", true).
    //
    // The erased Arc retains the actual registered set:sys owner, as upstream
    // shared_ptr does. Downcasting borrows it without duplicating settings.
    service_context: ServiceContext,
    notification_event_handle: u32,
    m_set_sys: SessionRequestHandlerPtr,
    system: crate::core::SystemRef,
    m_target_volumes: Mutex<[i32; 6]>,
    m_target_muted: Mutex<[bool; 6]>,
    m_active_target: u32,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IAudioController {
    pub fn new(system: crate::core::SystemRef) -> Arc<Self> {
        let manager = system
            .get()
            .service_manager()
            .expect("audio requires ServiceManager");
        let set_sys = ServiceManager::get_service_blocking(&manager, system, "set:sys");
        let mut service = Self::with_system_settings(set_sys);
        service.system = system;
        service.into_shared()
    }

    fn into_shared(mut self) -> Arc<Self> {
        Arc::new_cyclic(|reference| {
            self.self_reference = reference.clone();
            self
        })
    }

    fn unknown5000(&self) -> Arc<Self> {
        self.self_reference.upgrade().expect("audio controller requires shared ownership")
    }

    fn unknown5000_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let controller = Self::as_self(this).unknown5000();
        let mut response = ResponseBuilder::new(ctx, 2, 0, 1);
        response.push_result(RESULT_SUCCESS);
        response.push_ipc_interface(controller);
    }

    fn with_system_settings(set_sys: SessionRequestHandlerPtr) -> Self {
        assert!(set_sys.as_any().is::<SystemSettingsService>());
        let handlers = build_handler_map(&[
            (0, Some(Self::get_target_volume_handler), "GetTargetVolume"),
            (1, Some(Self::set_target_volume_handler), "SetTargetVolume"),
            (
                2,
                Some(Self::get_target_volume_min_handler),
                "GetTargetVolumeMin",
            ),
            (
                3,
                Some(Self::get_target_volume_max_handler),
                "GetTargetVolumeMax",
            ),
            (4, Some(Self::is_target_mute_handler), "IsTargetMute"),
            (5, Some(Self::set_target_mute_handler), "SetTargetMute"),
            (6, None, "IsTargetConnected"),
            (7, None, "SetDefaultTarget"),
            (8, None, "GetDefaultTarget"),
            (
                9,
                Some(Self::get_audio_output_mode_handler),
                "GetAudioOutputMode",
            ),
            (
                10,
                Some(Self::set_audio_output_mode_handler),
                "SetAudioOutputMode",
            ),
            (11, None, "SetForceMutePolicy"),
            (
                12,
                Some(Self::get_force_mute_policy_handler),
                "GetForceMutePolicy",
            ),
            (
                13,
                Some(Self::get_output_mode_setting_handler),
                "GetOutputModeSetting",
            ),
            (
                14,
                Some(Self::set_output_mode_setting_handler),
                "SetOutputModeSetting",
            ),
            (15, None, "SetOutputTarget"),
            (16, None, "SetInputTargetForceEnabled"),
            (
                17,
                Some(Self::set_headphone_output_level_mode_handler),
                "SetHeadphoneOutputLevelMode",
            ),
            (
                18,
                Some(Self::get_headphone_output_level_mode_handler),
                "GetHeadphoneOutputLevelMode",
            ),
            (19, None, "AcquireAudioVolumeUpdateEventForPlayReport"),
            (20, None, "AcquireAudioOutputDeviceUpdateEventForPlayReport"),
            (21, None, "GetAudioOutputTargetForPlayReport"),
            (
                22,
                Some(Self::notify_headphone_volume_warning_displayed_event_handler),
                "NotifyHeadphoneVolumeWarningDisplayedEvent",
            ),
            (23, None, "SetSystemOutputMasterVolume"),
            (24, None, "GetSystemOutputMasterVolume"),
            (25, None, "GetAudioVolumeDataForPlayReport"),
            (26, None, "UpdateHeadphoneSettings"),
            (27, None, "SetVolumeMappingTableForDev"),
            (28, None, "GetAudioOutputChannelCountForPlayReport"),
            (
                29,
                None,
                "BindAudioOutputChannelCountUpdateEventForPlayReport",
            ),
            (
                30,
                Some(Self::set_speaker_auto_mute_enabled_handler),
                "SetSpeakerAutoMuteEnabled",
            ),
            (
                31,
                Some(Self::is_speaker_auto_mute_enabled_handler),
                "IsSpeakerAutoMuteEnabled",
            ),
            (32, Some(Self::get_active_output_target_handler), "GetActiveOutputTarget"),
            (33, None, "GetTargetDeviceInfo"),
            (
                34,
                Some(Self::acquire_target_notification_handler),
                "AcquireTargetNotification",
            ),
            (
                35,
                None,
                "SetHearingProtectionSafeguardTimerRemainingTimeForDebug",
            ),
            (
                36,
                None,
                "GetHearingProtectionSafeguardTimerRemainingTimeForDebug",
            ),
            (37, None, "SetHearingProtectionSafeguardEnabled"),
            (38, None, "IsHearingProtectionSafeguardEnabled"),
            (
                39,
                None,
                "IsHearingProtectionSafeguardMonitoringOutputForDebug",
            ),
            (40, None, "GetSystemInformationForDebug"),
            (41, None, "SetVolumeButtonLongPressTime"),
            (42, None, "SetNativeVolumeForDebug"),
            (43, None, "Unknown43"),
            (5000, Some(Self::unknown5000_handler), "Unknown5000"),
            (10000, None, "NotifyAudioOutputTargetForPlayReport"),
            (10001, None, "NotifyAudioOutputChannelCountForPlayReport"),
            (
                10002,
                None,
                "NotifyUnsupportedUsbOutputDeviceAttachedForPlayReport",
            ),
            (10100, None, "GetAudioVolumeDataForPlayReport"),
            (10101, None, "BindAudioVolumeUpdateEventForPlayReport"),
            (10102, None, "BindAudioOutputTargetUpdateEventForPlayReport"),
            (10103, None, "GetAudioOutputTargetForPlayReport"),
            (10104, None, "GetAudioOutputChannelCountForPlayReport"),
            (
                10105,
                None,
                "BindAudioOutputChannelCountUpdateEventForPlayReport",
            ),
            (10106, None, "GetDefaultAudioOutputTargetForPlayReport"),
            (10200, None, "Unknown10200"),
            (50000, None, "SetAnalogInputBoostGainForPrototyping"),
            (50001, None, "OverrideDefaultTargetForDebug"),
            (50003, None, "SetForceOverrideExternalDeviceNameForDebug"),
            (50004, None, "ClearForceOverrideExternalDeviceNameForDebug"),
        ]);

        let mut service_context = ServiceContext::new("audctl".to_string());
        let notification_event_handle =
            service_context.create_event("IAudioController:NotificationEvent".to_string());

        Self {
            self_reference: Weak::new(),
            service_context,
            notification_event_handle,
            m_set_sys: set_sys,
            system: crate::core::SystemRef::null(),
            m_target_volumes: Mutex::new([15; 6]),
            m_target_muted: Mutex::new([false; 6]),
            m_active_target: crate::hle::service::set::settings_types::AudioOutputModeTarget::Speaker as u32,
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// Hardcoded to 0 as of FW 8.0.0.
    pub fn get_target_volume_min(&self) -> i32 {
        0
    }

    /// Hardcoded to 15 as of FW 8.0.0.
    pub fn get_target_volume_max(&self) -> i32 {
        15
    }

    /// Removed on FW 13.2.1+. Returns ForceMutePolicy::Disable.
    pub fn get_force_mute_policy(&self) -> ForceMutePolicy {
        ForceMutePolicy::Disable
    }

    pub fn get_headphone_output_level_mode(&self) -> HeadphoneOutputLevelMode {
        HeadphoneOutputLevelMode::Normal
    }

    pub fn set_headphone_output_level_mode(&self, _mode: HeadphoneOutputLevelMode) {
        log::warn!("(STUBBED) SetHeadphoneOutputLevelMode called");
    }

    pub fn notify_headphone_volume_warning_displayed_event(&self) {
        log::warn!("(STUBBED) NotifyHeadphoneVolumeWarningDisplayedEvent called");
    }

    fn as_self(this: &dyn ServiceFramework) -> &Self {
        unsafe { &*(this as *const dyn ServiceFramework as *const Self) }
    }

    fn system_settings(&self) -> &SystemSettingsService {
        self.m_set_sys
            .as_any()
            .downcast_ref()
            .expect("set:sys has the wrong service type")
    }

    fn get_audio_output_mode(&self, target: u32) -> u32 {
        self.system_settings()
            .inner
            .lock()
            .unwrap()
            .get_audio_output_mode(target)
    }

    fn set_audio_output_mode(&self, target: u32, output_mode: u32) {
        self.system_settings()
            .inner
            .lock()
            .unwrap()
            .set_audio_output_mode(target, output_mode);
    }

    fn get_target_volume_min_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        log::debug!("IAudioController::GetTargetVolumeMin");
        let mut response = CmifResponse::new(ctx, 3, 0, 0);
        response.push_result(RESULT_SUCCESS);
        response.push_i32(service.get_target_volume_min());
    }

    fn get_target_volume(&self, target: u32) -> Result<i32, ResultCode> {
        self.m_target_volumes.lock().unwrap().get(target as usize).copied().ok_or(RESULT_INVALID_ARGUMENT)
    }

    fn set_target_volume(&self, target: u32, volume: i32) -> ResultCode {
        let volume = volume.clamp(0, 15);
        {
            let mut volumes = self.m_target_volumes.lock().unwrap();
            let Some(slot) = volumes.get_mut(target as usize) else { return RESULT_INVALID_ARGUMENT; };
            *slot = volume;
        }
        if target == self.m_active_target && !self.m_target_muted.lock().unwrap()[target as usize] {
            let volume = volume as f32 / 15.0;
            if !self.system.is_null() {
                if let Some(audio) = self.system.get().audio_core() {
                    audio.set_audio_output_sink_volume(volume);
                }
                let service = self.system.get().service_manager().and_then(|manager| manager.lock().unwrap().get_service("audout:u"));
                if let Some(service) = service {
                    if let Some(manager) = service.as_any().downcast_ref::<super::audio_out_manager::IAudioOutManager>() {
                        manager.set_all_audio_out_volume(volume);
                    }
                }
            }
        }
        if target == self.m_active_target {
            let ui_volume = ((volume as f64 / 15.0) * 100.0).round() as u8;
            common::settings::values_mut().volume.set_value(ui_volume);
        }
        RESULT_SUCCESS
    }

    fn is_target_mute(&self, target: u32) -> Result<bool, ResultCode> {
        self.m_target_muted.lock().unwrap().get(target as usize).copied().ok_or(RESULT_INVALID_ARGUMENT)
    }

    fn set_target_mute(&self, muted: bool, target: u32) -> ResultCode {
        {
            let mut states = self.m_target_muted.lock().unwrap();
            let Some(slot) = states.get_mut(target as usize) else { return RESULT_INVALID_ARGUMENT; };
            *slot = muted;
        }
        if target == self.m_active_target {
            let volume = if muted { 0.0 } else { self.m_target_volumes.lock().unwrap()[target as usize] as f32 / 15.0 };
            if !self.system.is_null() {
                if let Some(audio) = self.system.get().audio_core() {
                    audio.set_audio_output_sink_volume(volume);
                }
                let service = self.system.get().service_manager().and_then(|manager| manager.lock().unwrap().get_service("audout:u"));
                if let Some(service) = service {
                    if let Some(manager) = service.as_any().downcast_ref::<super::audio_out_manager::IAudioOutManager>() {
                        manager.set_all_audio_out_volume(volume);
                    }
                }
            }
        }
        common::settings::values_mut().audio_muted.set_value(muted);
        RESULT_SUCCESS
    }

    fn get_target_volume_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let target = RequestParser::new(ctx).pop_u32();
        let result = Self::as_self(this).get_target_volume(target);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result.as_ref().map_or_else(|e| *e, |_| RESULT_SUCCESS));
        rb.push_i32(result.unwrap_or(0));
    }

    fn set_target_volume_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let target = rp.pop_u32();
        let volume = rp.pop_i32();
        let result = Self::as_self(this).set_target_volume(target, volume);
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }

    fn is_target_mute_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let target = RequestParser::new(ctx).pop_u32();
        let result = Self::as_self(this).is_target_mute(target);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result.as_ref().map_or_else(|e| *e, |_| RESULT_SUCCESS));
        rb.push_raw(&(result.unwrap_or(false) as u8));
    }

    fn set_target_mute_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let muted = rp.pop_raw::<u8>() != 0;
        let target = rp.pop_u32();
        let result = Self::as_self(this).set_target_mute(muted, target);
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }

    fn get_active_output_target_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(Self::as_self(this).m_active_target);
    }

    fn get_target_volume_max_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        log::debug!("IAudioController::GetTargetVolumeMax");
        let mut response = CmifResponse::new(ctx, 3, 0, 0);
        response.push_result(RESULT_SUCCESS);
        response.push_i32(service.get_target_volume_max());
    }

    fn get_audio_output_mode_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut request = CmifRequest::new(ctx);
        let target = request.u32();
        let output_mode = service.get_audio_output_mode(target);
        log::info!(
            "IAudioController::GetAudioOutputMode target={:?} output_mode={:?}",
            target,
            output_mode
        );
        let mut response = CmifResponse::new(ctx, 3, 0, 0);
        response.push_result(RESULT_SUCCESS);
        response.push_u32(output_mode as u32);
    }

    fn set_audio_output_mode_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut request = CmifRequest::new(ctx);
        let target = request.u32();
        let output_mode = request.u32();
        log::info!(
            "IAudioController::SetAudioOutputMode target={:?} output_mode={:?}",
            target,
            output_mode
        );
        service.set_audio_output_mode(target, output_mode);
        let mut response = CmifResponse::new(ctx, 2, 0, 0);
        response.push_result(RESULT_SUCCESS);
    }

    fn get_force_mute_policy_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        log::warn!("IAudioController::GetForceMutePolicy (STUBBED)");
        let mut response = CmifResponse::new(ctx, 3, 0, 0);
        response.push_result(RESULT_SUCCESS);
        response.push_u32(service.get_force_mute_policy() as u32);
    }

    fn get_output_mode_setting_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut request = CmifRequest::new(ctx);
        let target = request.u32();
        log::warn!(
            "IAudioController::GetOutputModeSetting (STUBBED) target={:?}",
            target
        );
        let mut response = CmifResponse::new(ctx, 3, 0, 0);
        response.push_result(RESULT_SUCCESS);
        response.push_u32(AudioOutputMode::Ch7_1 as u32);
    }

    fn set_output_mode_setting_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut request = CmifRequest::new(ctx);
        let target = request.u32();
        let output_mode = request.u32();
        log::info!(
            "IAudioController::SetOutputModeSetting target={:?} output_mode={:?}",
            target,
            output_mode
        );
        let mut response = CmifResponse::new(ctx, 2, 0, 0);
        response.push_result(RESULT_SUCCESS);
    }

    fn set_headphone_output_level_mode_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let mut request = CmifRequest::new(ctx);
        let mode = match request.u32() {
            0 => HeadphoneOutputLevelMode::Normal,
            1 => HeadphoneOutputLevelMode::HighPower,
            raw => {
                log::error!("Invalid headphone output level mode {}", raw);
                HeadphoneOutputLevelMode::Normal
            }
        };
        service.set_headphone_output_level_mode(mode);
        let mut response = CmifResponse::new(ctx, 2, 0, 0);
        response.push_result(RESULT_SUCCESS);
    }

    fn get_headphone_output_level_mode_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        log::info!("IAudioController::GetHeadphoneOutputLevelMode");
        let mut response = CmifResponse::new(ctx, 3, 0, 0);
        response.push_result(RESULT_SUCCESS);
        response.push_u32(service.get_headphone_output_level_mode() as u32);
    }

    fn notify_headphone_volume_warning_displayed_event_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        service.notify_headphone_volume_warning_displayed_event();
        let mut response = CmifResponse::new(ctx, 2, 0, 0);
        response.push_result(RESULT_SUCCESS);
    }

    fn set_speaker_auto_mute_enabled_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let mut request = CmifRequest::new(ctx);
        let enabled = request.raw::<u8>() != 0;
        log::info!(
            "IAudioController::SetSpeakerAutoMuteEnabled enabled={}",
            enabled
        );
        service
            .system_settings()
            .inner
            .lock()
            .unwrap()
            .set_speaker_auto_mute_flag(enabled);
        let mut response = CmifResponse::new(ctx, 2, 0, 0);
        response.push_result(RESULT_SUCCESS);
    }

    fn is_speaker_auto_mute_enabled_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let enabled = service
            .system_settings()
            .inner
            .lock()
            .unwrap()
            .get_speaker_auto_mute_flag();
        log::info!(
            "IAudioController::IsSpeakerAutoMuteEnabled enabled={}",
            enabled
        );
        let mut response = CmifResponse::new(ctx, 3, 0, 0);
        response.push_result(RESULT_SUCCESS);
        response.push_bool(enabled);
    }

    fn acquire_target_notification_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        log::warn!("IAudioController::AcquireTargetNotification (STUBBED)");
        let object_id = service
            .service_context
            .get_event(service.notification_event_handle)
            .and_then(|event| event.copy_object_id(ctx))
            .unwrap_or(0);
        let mut response = CmifResponse::new(ctx, 2, 1, 0);
        response.push_result(RESULT_SUCCESS);
        response.push_copy_object_id(object_id);
    }
}

impl SessionRequestHandler for IAudioController {
    fn as_any(&self) -> &dyn std::any::Any { self }
    fn handle_sync_request(&self, context: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, context)
    }
}

impl ServiceFramework for IAudioController {
    fn get_service_name(&self) -> &str {
        "audctl"
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
    use crate::hle::service::set::settings_types::AudioOutputModeTarget;
    use std::sync::Arc;

    #[test]
    fn duplicate_controller_preserves_identity_state_and_lifetime() {
        let controller = IAudioController::with_system_settings(
            Arc::new(SystemSettingsService::new_for_test())).into_shared();
        let duplicate = controller.unknown5000();
        assert!(Arc::ptr_eq(&controller, &duplicate));
        assert!(controller.handlers[&5000].handler_callback.is_some());
        // A non-active target avoids changing global UI settings in this test.
        assert_eq!(controller.set_target_volume(0, 7), RESULT_SUCCESS);
        assert_eq!(duplicate.get_target_volume(0), Ok(7));
        let weak = Arc::downgrade(&controller);
        drop(controller);
        assert_eq!(duplicate.get_target_volume(0), Ok(7));
        assert!(weak.upgrade().is_some());
        drop(duplicate);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn audio_controller_registers_currently_ported_command_ids() {
        let service =
            IAudioController::with_system_settings(Arc::new(SystemSettingsService::new_for_test()));

        for cmd in [0_u32, 1, 2, 3, 4, 5, 9, 10, 12, 13, 14, 17, 18, 22, 30, 31, 32, 34] {
            assert!(service.handlers.contains_key(&cmd));
            assert!(service.handlers[&cmd].handler_callback.is_some());
        }

        for cmd in [
            6_u32, 7, 8, 11, 15, 16, 19, 20, 21, 23, 24, 25, 26, 27, 28, 29, 33,
            35, 36, 37, 38, 39, 40, 41, 42, 10000, 10001, 10002, 10100, 10101, 10102, 10103, 10104,
            10105, 10106, 50000,
        ] {
            assert!(service.handlers.contains_key(&cmd));
            assert!(service.handlers[&cmd].handler_callback.is_none());
        }
    }

    #[test]
    fn target_volume_and_mute_validate_indices_and_round_trip_ipc() {
        let service = IAudioController::with_system_settings(Arc::new(SystemSettingsService::new_for_test()));
        let original_mute = *common::settings::values().audio_muted.get_value();
        for target in [0, 1, 3, 4, 5] {
            assert_eq!(service.get_target_volume(target), Ok(15));
            for (value, expected) in [(-5, 0), (7, 7), (99, 15)] {
                let mut ctx = HLERequestContext::new();
                ctx.cmd_buf[2] = target;
                ctx.cmd_buf[3] = value as u32;
                service.handlers[&1].handler_callback.unwrap()(&service, &mut ctx);
                assert_eq!(ctx.cmd_buf[6], 0);
                assert_eq!(service.get_target_volume(target), Ok(expected));
            }
            for muted in [true, false] {
                let mut ctx = HLERequestContext::new();
                ctx.cmd_buf[2] = u32::from(muted);
                ctx.cmd_buf[3] = target;
                service.handlers[&5].handler_callback.unwrap()(&service, &mut ctx);
                assert_eq!(service.is_target_mute(target), Ok(muted));
            }
        }
        for target in [6, u32::MAX] {
            assert_eq!(service.get_target_volume(target), Err(RESULT_INVALID_ARGUMENT));
            assert_eq!(service.set_target_volume(target, 1), RESULT_INVALID_ARGUMENT);
            assert_eq!(service.is_target_mute(target), Err(RESULT_INVALID_ARGUMENT));
            assert_eq!(service.set_target_mute(true, target), RESULT_INVALID_ARGUMENT);
        }
        let mut ctx = HLERequestContext::new();
        service.handlers[&32].handler_callback.unwrap()(&service, &mut ctx);
        assert_eq!(ctx.cmd_buf[8], AudioOutputModeTarget::Speaker as u32);
        common::settings::values_mut().audio_muted.set_value(original_mute);
    }

    #[test]
    fn audio_controller_and_system_settings_share_modes_and_mute() {
        let settings = Arc::new(SystemSettingsService::new_for_test());
        let first = IAudioController::with_system_settings(settings.clone());
        let second = IAudioController::with_system_settings(settings.clone());
        let speaker = AudioOutputModeTarget::Speaker as u32;
        let hdmi = AudioOutputModeTarget::Hdmi as u32;
        let initial_hdmi = settings.inner.lock().unwrap().get_audio_output_mode(hdmi);
        for mode in [0, 1, 2, 3, u32::MAX] {
            let mut ctx = HLERequestContext::new();
            ctx.command_buffer_mut()[2] = speaker;
            ctx.command_buffer_mut()[3] = mode;
            first.handlers[&10].handler_callback.unwrap()(&first, &mut ctx);
            assert_eq!(ctx.command_buffer()[6], 0);
            assert_eq!(
                settings
                    .inner
                    .lock()
                    .unwrap()
                    .get_audio_output_mode(speaker),
                mode
            );
            assert_eq!(second.get_audio_output_mode(speaker), mode);
            assert_eq!(second.get_audio_output_mode(hdmi), initial_hdmi);

            settings
                .inner
                .lock()
                .unwrap()
                .set_audio_output_mode(speaker, mode ^ 1);
            let mut ctx = HLERequestContext::new();
            ctx.command_buffer_mut()[2] = speaker;
            second.handlers[&9].handler_callback.unwrap()(&second, &mut ctx);
            assert_eq!(ctx.command_buffer()[6], 0);
            assert_eq!(ctx.command_buffer()[8], mode ^ 1);
        }
        for enabled in [true, false] {
            let mut ctx = HLERequestContext::new();
            // A CMIF bool occupies one byte; padding must not enable it.
            ctx.command_buffer_mut()[2] = 0xFFFF_FF00 | u32::from(enabled);
            first.handlers[&30].handler_callback.unwrap()(&first, &mut ctx);
            assert_eq!(
                settings.inner.lock().unwrap().get_speaker_auto_mute_flag(),
                enabled
            );
            settings
                .inner
                .lock()
                .unwrap()
                .set_speaker_auto_mute_flag(!enabled);
            let mut ctx = HLERequestContext::new();
            second.handlers[&31].handler_callback.unwrap()(&second, &mut ctx);
            assert_eq!(ctx.command_buffer()[8], u32::from(!enabled));
        }
        // Retaining either controller keeps the actual settings owner alive.
        let weak = Arc::downgrade(&settings);
        drop(settings);
        drop(first);
        assert!(weak.upgrade().is_some());
        drop(second);
        assert!(weak.upgrade().is_none());
    }
}
