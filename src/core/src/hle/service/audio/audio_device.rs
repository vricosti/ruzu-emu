//! Port of zuyu/src/core/hle/service/audio/audio_device.h and audio_device.cpp
//!
//! IAudioDevice service.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::core::SystemRef;
use crate::hle::result::{ErrorModule, ResultCode, RESULT_SUCCESS};
use crate::hle::service::audio::errors::RESULT_INSUFFICIENT_BUFFER;
use crate::hle::service::cmif_serialization::{write_out_array_bytes, CmifRequest, CmifResponse};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::kernel_helpers::ServiceContext;
use crate::hle::service::os::event::Event;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command table for IAudioDevice:
///
/// | Cmd | Name                            |
/// |-----|---------------------------------|
/// | 0   | ListAudioDeviceName             |
/// | 1   | SetAudioDeviceOutputVolume      |
/// | 2   | GetAudioDeviceOutputVolume      |
/// | 3   | GetActiveAudioDeviceName        |
/// | 4   | QueryAudioDeviceSystemEvent     |
/// | 5   | GetActiveChannelCount           |
/// | 6   | ListAudioDeviceNameAuto         |
/// | 7   | SetAudioDeviceOutputVolumeAuto  |
/// | 8   | GetAudioDeviceOutputVolumeAuto  |
/// | 10  | GetActiveAudioDeviceNameAuto    |
/// | 11  | QueryAudioDeviceInputEvent      |
/// | 12  | QueryAudioDeviceOutputEvent     |
/// | 13  | GetActiveAudioOutputDeviceName  |
/// | 14  | ListAudioOutputDeviceName       |
/// | 15  | AcquireAudioInputDeviceNotification  | (17.0.0+)
/// | 16  | ReleaseAudioInputDeviceNotification  | (17.0.0+)
/// | 17  | AcquireAudioOutputDeviceNotification | (17.0.0+)
/// | 18  | ReleaseAudioOutputDeviceNotification | (17.0.0+)
/// | 19  | SetAudioDeviceOutputVolumeAutoTuneEnabled | (18.0.0+)
/// | 20  | IsAudioDeviceOutputVolumeAutoTuneEnabled  | (18.0.0+)
/// | 21  | IsActiveOutputDeviceEstimatedLowLatency   | (21.0.0+)
pub struct IAudioDevice {
    system: SystemRef,
    applet_resource_user_id: u64,
    revision: u32,
    _device_num: u32,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
    service_context: Mutex<ServiceContext>,
    event_handle: u32,
    notification_event_handle: u32,
}

impl IAudioDevice {
    const AUDIO_DEVICE_NAME_SIZE: usize = 0x100;

    pub fn new(
        system: SystemRef,
        applet_resource_user_id: u64,
        revision: u32,
        device_num: u32,
    ) -> Self {
        let handlers = build_handler_map(&[
            (
                0,
                Some(Self::list_audio_device_name_handler),
                "ListAudioDeviceName",
            ),
            (
                1,
                Some(Self::set_audio_device_output_volume_handler),
                "SetAudioDeviceOutputVolume",
            ),
            (
                2,
                Some(Self::get_audio_device_output_volume_handler),
                "GetAudioDeviceOutputVolume",
            ),
            (
                3,
                Some(Self::get_active_audio_device_name_handler),
                "GetActiveAudioDeviceName",
            ),
            (
                4,
                Some(Self::query_audio_device_system_event_handler),
                "QueryAudioDeviceSystemEvent",
            ),
            (
                5,
                Some(Self::get_active_channel_count_handler),
                "GetActiveChannelCount",
            ),
            (
                6,
                Some(Self::list_audio_device_name_auto_handler),
                "ListAudioDeviceNameAuto",
            ),
            (
                7,
                Some(Self::set_audio_device_output_volume_auto_handler),
                "SetAudioDeviceOutputVolumeAuto",
            ),
            (
                8,
                Some(Self::get_audio_device_output_volume_auto_handler),
                "GetAudioDeviceOutputVolumeAuto",
            ),
            (
                10,
                Some(Self::get_active_audio_device_name_auto_handler),
                "GetActiveAudioDeviceNameAuto",
            ),
            (
                11,
                Some(Self::query_audio_device_input_event_handler),
                "QueryAudioDeviceInputEvent",
            ),
            (
                12,
                Some(Self::query_audio_device_output_event_handler),
                "QueryAudioDeviceOutputEvent",
            ),
            (
                13,
                Some(Self::get_active_audio_output_device_name_handler),
                "GetActiveAudioOutputDeviceName",
            ),
            (
                14,
                Some(Self::list_audio_output_device_name_handler),
                "ListAudioOutputDeviceName",
            ),
            (9, Some(Self::stub_reply_zeroed_handler), "Unknown9"),
            (
                15,
                Some(Self::acquire_audio_device_notification_handler),
                "AcquireAudioInputDeviceNotification",
            ),
            (
                16,
                Some(Self::release_audio_device_notification_handler),
                "ReleaseAudioInputDeviceNotification",
            ),
            (
                17,
                Some(Self::acquire_audio_device_notification_handler),
                "AcquireAudioOutputDeviceNotification",
            ),
            (
                18,
                Some(Self::release_audio_device_notification_handler),
                "ReleaseAudioOutputDeviceNotification",
            ),
            (
                19,
                Some(Self::set_audio_device_output_volume_auto_tune_enabled_handler),
                "SetAudioDeviceOutputVolumeAutoTuneEnabled",
            ),
            (
                20,
                Some(Self::is_audio_device_output_volume_auto_tune_enabled_handler),
                "IsAudioDeviceOutputVolumeAutoTuneEnabled",
            ),
            (21, Some(Self::stub_reply_zeroed_handler), "IsActiveOutputDeviceEstimatedLowLatency"),
        ]);
        let mut service_context = ServiceContext::new("IAudioDevice".to_string());
        let event_handle =
            service_context.create_event(format!("IAudioDeviceEvent-{}", device_num));
        // The device-set never changes in HLE; upstream leaves this event
        // unsignaled and hands it out via AcquireAudioDeviceNotification.
        let notification_event_handle = service_context
            .create_event(format!("IAudioDeviceNotification-{}", device_num));
        let event = service_context
            .get_event(event_handle)
            .expect("IAudioDevice event handle must resolve");
        event.signal();

        Self {
            system,
            applet_resource_user_id,
            revision,
            _device_num: device_num,
            handlers,
            handlers_tipc: BTreeMap::new(),
            service_context: Mutex::new(service_context),
            event_handle,
            notification_event_handle,
        }
    }

    fn as_self(this: &dyn ServiceFramework) -> &Self {
        unsafe { &*(this as *const dyn ServiceFramework as *const Self) }
    }

    fn event(&self) -> std::sync::Arc<Event> {
        self.service_context
            .lock()
            .unwrap()
            .get_event(self.event_handle)
            .expect("IAudioDevice event must stay alive for service lifetime")
    }

    /// Return the object id for the service-owned readable event.
    ///
    /// Upstream creates/signals the event in the constructor and every Query*Event
    /// returns the readable end of that same owner event through OutCopyHandle.
    fn copy_event_object_id(this: &dyn ServiceFramework, ctx: &HLERequestContext) -> Option<u64> {
        let svc = Self::as_self(this);
        svc.event().copy_object_id(ctx)
    }

    fn name_array_bytes(names: &[[u8; 0x100]]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(names.len() * 0x100);
        for name in names {
            bytes.extend_from_slice(name);
        }
        bytes
    }

    fn insufficient_buffer_result() -> ResultCode {
        ResultCode::from_module_description(ErrorModule::Audio, RESULT_INSUFFICIENT_BUFFER.1)
    }

    fn write_single_name(ctx: &mut HLERequestContext, name: &[u8; 0x100]) -> ResultCode {
        if Self::output_name_capacity(ctx) == 0 {
            return Self::insufficient_buffer_result();
        }
        ctx.write_buffer(name, 0);
        RESULT_SUCCESS
    }

    fn read_first_name(ctx: &HLERequestContext) -> String {
        let bytes = ctx.read_buffer(0);
        let raw_name = bytes.get(..0x100).unwrap_or(&[]);
        let nul = raw_name
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(raw_name.len());
        String::from_utf8_lossy(&raw_name[..nul]).into_owned()
    }

    fn make_audio_device_name(name: &str) -> [u8; 0x100] {
        let mut out = [0u8; 0x100];
        let bytes = name.as_bytes();
        let len = bytes.len().min(out.len().saturating_sub(1));
        out[..len].copy_from_slice(&bytes[..len]);
        out
    }

    fn default_active_device_name() -> [u8; 0x100] {
        Self::make_audio_device_name("AudioTvOutput")
    }

    fn output_name_capacity(ctx: &HLERequestContext) -> usize {
        ctx.get_write_buffer_size(0) / Self::AUDIO_DEVICE_NAME_SIZE
    }

    fn input_name_capacity(ctx: &HLERequestContext) -> usize {
        ctx.get_read_buffer_size(0) / Self::AUDIO_DEVICE_NAME_SIZE
    }

    fn list_audio_device_names(&self, max_names: usize) -> Vec<[u8; 0x100]> {
        if max_names == 0 {
            return Vec::new();
        }

        let mut names = vec![[0u8; 0x100]; max_names];
        let count = (if self.system.is_null() {
            None
        } else {
            self.system.get().audio_core()
        })
        .map(|audio_core| {
            audio_core.list_audio_device_name(
                self.applet_resource_user_id,
                self.revision,
                &mut names,
            ) as usize
        })
        .unwrap_or_else(|| {
            names[0] = Self::default_active_device_name();
            1
        });
        names.truncate(count);
        names
    }

    fn list_audio_output_device_names(&self, max_names: usize) -> Vec<[u8; 0x100]> {
        if max_names == 0 {
            return Vec::new();
        }

        let mut names = vec![[0u8; 0x100]; max_names];
        let count = (if self.system.is_null() {
            None
        } else {
            self.system.get().audio_core()
        })
        .map(|audio_core| {
            audio_core.list_audio_output_device_name(
                self.applet_resource_user_id,
                self.revision,
                &mut names,
            ) as usize
        })
        .unwrap_or_else(|| {
            names[0] = Self::default_active_device_name();
            1
        });
        names.truncate(count);
        names
    }

    fn list_audio_device_name_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("IAudioDevice::ListAudioDeviceName");
        let svc = Self::as_self(this);
        let names = svc.list_audio_device_names(Self::output_name_capacity(ctx));
        let bytes = Self::name_array_bytes(&names);
        write_out_array_bytes(ctx, 0, &bytes);
        let mut response = CmifResponse::new(ctx, 3, 0, 0);
        response.push_result(RESULT_SUCCESS);
        response.push_u32(names.len() as u32);
    }

    fn set_audio_device_output_volume_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("IAudioDevice::SetAudioDeviceOutputVolume");
        let svc = Self::as_self(this);
        let mut request = CmifRequest::new(ctx);
        let volume = request.f32();
        if Self::input_name_capacity(ctx) == 0 {
            let mut response = CmifResponse::new(ctx, 2, 0, 0);
            response.push_result(Self::insufficient_buffer_result());
            return;
        }
        let device_name = Self::read_first_name(ctx);
        if device_name == "AudioTvOutput" {
            if let Some(audio_core) = (!svc.system.is_null())
                .then(|| svc.system.get())
                .and_then(|system| system.audio_core())
            {
                audio_core.set_audio_device_volume(
                    svc.applet_resource_user_id,
                    svc.revision,
                    volume,
                );
            }
        }
        let mut response = CmifResponse::new(ctx, 2, 0, 0);
        response.push_result(RESULT_SUCCESS);
    }

    fn get_audio_device_output_volume_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("IAudioDevice::GetAudioDeviceOutputVolume");
        let svc = Self::as_self(this);
        if Self::input_name_capacity(ctx) == 0 {
            let mut response = CmifResponse::new(ctx, 2, 0, 0);
            response.push_result(Self::insufficient_buffer_result());
            return;
        }
        let device_name = Self::read_first_name(ctx);
        let volume = if device_name == "AudioTvOutput" {
            (!svc.system.is_null())
                .then(|| svc.system.get())
                .and_then(|system| system.audio_core())
                .map(|audio_core| {
                    audio_core.get_audio_device_volume(
                        svc.applet_resource_user_id,
                        svc.revision,
                        &device_name,
                    )
                })
                .unwrap_or(1.0)
        } else {
            1.0
        };
        let mut response = CmifResponse::new(ctx, 3, 0, 0);
        response.push_result(RESULT_SUCCESS);
        response.push_f32(volume);
    }

    fn get_active_audio_device_name_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("IAudioDevice::GetActiveAudioDeviceName");
        let result = Self::write_single_name(ctx, &Self::default_active_device_name());
        let mut response = CmifResponse::new(ctx, 2, 0, 0);
        response.push_result(result);
    }

    /// Port of upstream `IAudioDevice::QueryAudioDeviceSystemEvent`.
    /// Upstream: signals the event then returns `&event->GetReadableEvent()`.
    fn query_audio_device_system_event_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::info!("IAudioDevice::QueryAudioDeviceSystemEvent");
        let svc = Self::as_self(this);
        svc.event().signal();
        if let Some(object_id) = Self::copy_event_object_id(this, ctx) {
            log::info!(
                "IAudioDevice::QueryAudioDeviceSystemEvent readable_event_object_id={:#x}",
                object_id
            );
            let mut response = CmifResponse::new(ctx, 2, 1, 0);
            response.push_result(RESULT_SUCCESS);
            response.push_copy_object_id(object_id);
        } else {
            log::error!("IAudioDevice::QueryAudioDeviceSystemEvent failed to create event");
            let mut response = CmifResponse::new(ctx, 2, 0, 0);
            response.push_result(RESULT_SUCCESS);
        }
    }

    fn get_active_channel_count_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::info!("IAudioDevice::GetActiveChannelCount");
        let svc = Self::as_self(_this);
        let active_channel_count = if let Some(audio_core) = (!svc.system.is_null())
            .then(|| svc.system.get())
            .and_then(|system| system.audio_core())
        {
            audio_core.get_audio_output_system_channels()
        } else {
            2
        };
        let mut response = CmifResponse::new(ctx, 3, 0, 0);
        response.push_result(RESULT_SUCCESS);
        response.push_u32(active_channel_count);
    }

    fn list_audio_device_name_auto_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::info!("IAudioDevice::ListAudioDeviceNameAuto");
        Self::list_audio_device_name_handler(this, ctx);
    }

    fn set_audio_device_output_volume_auto_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("IAudioDevice::SetAudioDeviceOutputVolumeAuto");
        Self::set_audio_device_output_volume_handler(this, ctx);
    }

    fn get_audio_device_output_volume_auto_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("IAudioDevice::GetAudioDeviceOutputVolumeAuto");
        Self::get_audio_device_output_volume_handler(this, ctx);
    }

    fn get_active_audio_device_name_auto_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::info!("IAudioDevice::GetActiveAudioDeviceNameAuto");
        let result = Self::write_single_name(ctx, &Self::default_active_device_name());
        let mut response = CmifResponse::new(ctx, 2, 0, 0);
        response.push_result(result);
    }

    /// Port of upstream `IAudioDevice::QueryAudioDeviceInputEvent`.
    /// Upstream returns `&event->GetReadableEvent()`.
    fn query_audio_device_input_event_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::info!("IAudioDevice::QueryAudioDeviceInputEvent");
        if let Some(object_id) = Self::copy_event_object_id(this, ctx) {
            let mut response = CmifResponse::new(ctx, 2, 1, 0);
            response.push_result(RESULT_SUCCESS);
            response.push_copy_object_id(object_id);
        } else {
            log::error!("IAudioDevice::QueryAudioDeviceInputEvent failed to create event");
            let mut response = CmifResponse::new(ctx, 2, 0, 0);
            response.push_result(RESULT_SUCCESS);
        }
    }

    /// Port of upstream `IAudioDevice::QueryAudioDeviceOutputEvent`.
    /// Upstream returns `&event->GetReadableEvent()`.
    fn query_audio_device_output_event_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::info!("IAudioDevice::QueryAudioDeviceOutputEvent");
        if let Some(object_id) = Self::copy_event_object_id(this, ctx) {
            let mut response = CmifResponse::new(ctx, 2, 1, 0);
            response.push_result(RESULT_SUCCESS);
            response.push_copy_object_id(object_id);
        } else {
            log::error!("IAudioDevice::QueryAudioDeviceOutputEvent failed to create event");
            let mut response = CmifResponse::new(ctx, 2, 0, 0);
            response.push_result(RESULT_SUCCESS);
        }
    }

    fn get_active_audio_output_device_name_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::info!("IAudioDevice::GetActiveAudioOutputDeviceName");
        let result = Self::write_single_name(ctx, &Self::default_active_device_name());
        let mut response = CmifResponse::new(ctx, 2, 0, 0);
        response.push_result(result);
    }

    fn list_audio_output_device_name_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::info!("IAudioDevice::ListAudioOutputDeviceName");
        let svc = Self::as_self(this);
        let names = svc.list_audio_output_device_names(Self::output_name_capacity(ctx));
        let bytes = Self::name_array_bytes(&names);
        write_out_array_bytes(ctx, 0, &bytes);
        let mut response = CmifResponse::new(ctx, 3, 0, 0);
        response.push_result(RESULT_SUCCESS);
        response.push_u32(names.len() as u32);
    }

    fn notification_event(&self) -> std::sync::Arc<Event> {
        self.service_context
            .lock()
            .unwrap()
            .get_event(self.notification_event_handle)
            .expect("IAudioDevice notification event must stay alive for service lifetime")
    }

    fn copy_notification_event_object_id(
        this: &dyn ServiceFramework,
        ctx: &HLERequestContext,
    ) -> Option<u64> {
        let svc = Self::as_self(this);
        svc.notification_event().copy_object_id(ctx)
    }

    /// Port of upstream `IAudioDevice::AcquireAudioDeviceNotification`.
    /// The HLE device set never changes, so the notification event is simply
    /// signaled once and handed out for commands 15 (input) and 17 (output).
    /// The auto stub previously replied without a handle, breaking Switch 2
    /// titles that wait on the event afterwards.
    fn acquire_audio_device_notification_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut request = CmifRequest::new(ctx);
        let unknown = request.u64();
        log::debug!(
            "IAudioDevice::AcquireAudioDeviceNotification unknown={:#x}",
            unknown
        );
        if let Some(object_id) = Self::copy_notification_event_object_id(this, ctx) {
            let mut response = CmifResponse::new(ctx, 2, 1, 0);
            response.push_result(RESULT_SUCCESS);
            response.push_copy_object_id(object_id);
        } else {
            log::error!(
                "IAudioDevice::AcquireAudioDeviceNotification failed to resolve notification event"
            );
            let mut response = CmifResponse::new(ctx, 2, 0, 0);
            response.push_result(RESULT_SUCCESS);
        }
    }

    /// Port of upstream `IAudioDevice::ReleaseAudioDeviceNotification`
    /// (commands 16 (input) and 18 (output)).
    fn release_audio_device_notification_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut request = CmifRequest::new(ctx);
        let unknown = request.u64();
        log::debug!(
            "IAudioDevice::ReleaseAudioDeviceNotification unknown={:#x}",
            unknown
        );
        let mut response = CmifResponse::new(ctx, 2, 0, 0);
        response.push_result(RESULT_SUCCESS);
    }

    /// Port of upstream `IAudioDevice::SetAudioDeviceOutputVolumeAutoTuneEnabled`.
    fn set_audio_device_output_volume_auto_tune_enabled_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("IAudioDevice::SetAudioDeviceOutputVolumeAutoTuneEnabled (STUBBED) called");
        let _ = this;
        let mut response = CmifResponse::new(ctx, 2, 0, 0);
        response.push_result(RESULT_SUCCESS);
    }

    /// Port of upstream `IAudioDevice::IsAudioDeviceOutputVolumeAutoTuneEnabled`.
    fn is_audio_device_output_volume_auto_tune_enabled_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("IAudioDevice::IsAudioDeviceOutputVolumeAutoTuneEnabled (STUBBED) called");
        let _ = this;
        let mut response = CmifResponse::new(ctx, 3, 0, 0);
        response.push_result(RESULT_SUCCESS);
        response.push_bool(false);
    }

    /// Port of upstream `IAudioDevice::StubReplyZeroed` (Unknown9 and
    /// IsActiveOutputDeviceEstimatedLowLatency): an empty success reply.
    fn stub_reply_zeroed_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut response = CmifResponse::new(ctx, 2, 0, 0);
        response.push_result(RESULT_SUCCESS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_keeps_applet_resource_user_id_and_revision() {
        let service = IAudioDevice::new(SystemRef::null(), 0x51, 0x5245_5631, 3);
        assert_eq!(service.applet_resource_user_id, 0x51);
        assert_eq!(service.revision, 0x5245_5631);
    }

    #[test]
    fn fallback_lists_keep_upstream_default_tv_name() {
        let service = IAudioDevice::new(SystemRef::null(), 0, 0x5245_5631, 0);
        let names = service.list_audio_device_names(1);
        assert_eq!(names.len(), 1);
        assert_eq!(
            &names[0][..14],
            &IAudioDevice::make_audio_device_name("AudioTvOutput")[..14]
        );
    }

    #[test]
    fn list_audio_device_names_respects_out_array_capacity() {
        let service = IAudioDevice::new(SystemRef::null(), 0, 0x5245_5631, 0);
        let names = service.list_audio_device_names(3);
        assert_eq!(names.len(), 1);

        let output_names = service.list_audio_output_device_names(2);
        assert_eq!(output_names.len(), 1);
    }

    #[test]
    fn output_name_capacity_uses_real_buffer_size() {
        let mut ctx = HLERequestContext::new();
        ctx.set_buffer_b_descriptors_for_test(vec![crate::hle::ipc::BufferDescriptorABW {
            size_bits_0_31: (IAudioDevice::AUDIO_DEVICE_NAME_SIZE * 3) as u32,
            address_bits_0_31: 0,
            raw_word2: 0,
        }]);
        assert_eq!(IAudioDevice::output_name_capacity(&ctx), 3);
    }

    #[test]
    fn constructor_creates_and_signals_service_owned_event() {
        let service = IAudioDevice::new(SystemRef::null(), 0, 0x5245_5631, 7);
        let event = service
            .service_context
            .lock()
            .unwrap()
            .get_event(service.event_handle)
            .expect("IAudioDevice constructor must create event");

        assert!(event.is_signaled());
    }

    #[test]
    fn service_registers_upstream_command_ids() {
        let service = IAudioDevice::new(SystemRef::null(), 0, 0x5245_5631, 0);
        for cmd in [
            0_u32, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21,
        ] {
            assert!(service.handlers.contains_key(&cmd));
        }
    }

    #[test]
    fn acquire_audio_device_notification_has_a_dedicated_unsignaled_event() {
        // Switch 2 titles (OOT, KAR) acquire the
        // output-device notification event and wait on it; the auto stub used
        // to reply without a handle and the title aborted with Kernel/120.
        let service = IAudioDevice::new(SystemRef::null(), 0, 0x5245_5631, 2);
        let notification = service.notification_event();
        let main_event = service
            .service_context
            .lock()
            .unwrap()
            .get_event(service.event_handle)
            .expect("IAudioDevice constructor must create event");
        assert!(
            !std::sync::Arc::ptr_eq(&notification, &main_event),
            "notification event must be distinct from the query event"
        );
        // Upstream leaves the notification event unsignaled: the device set
        // never changes.
        assert!(!notification.is_signaled());
        assert!(main_event.is_signaled());
    }
}

impl SessionRequestHandler for IAudioDevice {
    fn handle_sync_request(&self, context: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, context)
    }
}

impl ServiceFramework for IAudioDevice {
    fn get_service_name(&self) -> &str {
        "IAudioDevice"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}
