//! Port of zuyu/src/core/hle/service/audio/audio_out_manager.h and audio_out_manager.cpp
//!
//! IAudioOutManager service ("audout:u").

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::core::{AudioOutParameterWire, SystemRef};
use crate::hle::kernel::k_process::KProcess;
use crate::hle::kernel::k_process::ProcessLock;
use crate::hle::kernel::svc_common::PseudoHandle;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::cmif_serialization::{
    CmifInArrayBuffer, CmifOutArrayBuffer, CmifRequest, CmifResponse,
};
use crate::hle::service::cmif_types::{buffer_attr, InArray, OutArray};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command table for IAudioOutManager ("audout:u"):
///
/// | Cmd | Name              |
/// |-----|-------------------|
/// | 0   | ListAudioOuts     |
/// | 1   | OpenAudioOut      |
/// | 2   | ListAudioOutsAuto |
/// | 3   | OpenAudioOutAuto  |
pub struct IAudioOutManager {
    system: SystemRef,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct AudioDeviceNameWire {
    name: [u8; 0x100],
}

impl Default for AudioDeviceNameWire {
    fn default() -> Self {
        Self { name: [0; 0x100] }
    }
}

#[derive(Clone, Copy, Default)]
#[repr(C)]
struct AudioOutParameterInternalWire {
    sample_rate: u32,
    channel_count: u32,
    sample_format: u32,
    state: u32,
}

impl IAudioOutManager {
    /// Upstream IAudioOutManager::SetAllAudioOutVolume. The concrete manager is
    /// reached through AudioCoreInterface to avoid a core/audio_core crate cycle.
    pub fn set_all_audio_out_volume(&self, volume: f32) -> ResultCode {
        if let Some(audio_core) = self.system.get().audio_core() {
            audio_core.set_all_audio_out_volume(volume);
            RESULT_SUCCESS
        } else {
            ResultCode::from_module_description(
                crate::hle::result::ErrorModule::Audio,
                crate::hle::service::audio::errors::RESULT_OPERATION_FAILED.1,
            )
        }
    }

    pub fn new(system: SystemRef) -> Self {
        let handlers = build_handler_map(&[
            (0, Some(Self::list_audio_outs_handler), "ListAudioOuts"),
            (1, Some(Self::open_audio_out_handler), "OpenAudioOut"),
            (
                2,
                Some(Self::list_audio_outs_auto_handler),
                "ListAudioOutsAuto",
            ),
            (
                3,
                Some(Self::open_audio_out_auto_handler),
                "OpenAudioOutAuto",
            ),
        ]);
        Self {
            system,
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn audio_out_name_bytes() -> &'static [u8] {
        b"DeviceOut\0"
    }

    fn list_audio_outs_auto<const A: i32>(
        &self,
        out_audio_outs: &mut OutArray<AudioDeviceNameWire, A>,
        out_count: &mut u32,
    ) -> ResultCode {
        if out_audio_outs.is_empty() {
            *out_count = 0;
            return RESULT_SUCCESS;
        }

        let name = Self::audio_out_name_bytes();
        out_audio_outs[0].name[..name.len()].copy_from_slice(name);
        *out_count = 1;
        RESULT_SUCCESS
    }

    fn resolve_process_handle(
        owner_process: &Arc<ProcessLock>,
        handle: u32,
    ) -> Option<*mut KProcess> {
        let mut process_guard = owner_process.lock().unwrap();
        if handle == PseudoHandle::CurrentProcess as u32 || handle == 0 {
            return Some(&mut *process_guard as *mut KProcess);
        }

        process_guard.handle_table.get_object(handle)?;
        Some(&mut *process_guard as *mut KProcess)
    }

    fn open_audio_out_auto<const AIN: i32, const AOUT: i32>(
        &self,
        ctx: &mut HLERequestContext,
        out_parameter_internal: &mut AudioOutParameterInternalWire,
        out_name: &mut OutArray<AudioDeviceNameWire, AOUT>,
        name: &InArray<AudioDeviceNameWire, AIN>,
        params: AudioOutParameterWire,
    ) -> ResultCode {
        if name.is_empty() || out_name.is_empty() {
            return ResultCode::from_module_description(
                crate::hle::result::ErrorModule::Audio,
                crate::hle::service::audio::errors::RESULT_OPERATION_FAILED.1,
            );
        }

        let mut request = CmifRequest::new(ctx);
        request.align_for::<u64>();
        let applet_resource_user_id = request.u64();

        let Some(owner_process) = ctx.owner_process_arc() else {
            return ResultCode::from_module_description(
                crate::hle::result::ErrorModule::Audio,
                crate::hle::service::audio::errors::RESULT_INVALID_HANDLE.1,
            );
        };
        let process_handle = ctx.get_copy_handle(0);
        let process = Self::resolve_process_handle(&owner_process, process_handle);
        let Some(process) = process else {
            return ResultCode::from_module_description(
                crate::hle::result::ErrorModule::Audio,
                crate::hle::service::audio::errors::RESULT_INVALID_HANDLE.1,
            );
        };

        let result = self
            .system
            .get()
            .audio_core()
            .ok_or_else(|| {
                ResultCode::from_module_description(
                    crate::hle::result::ErrorModule::Audio,
                    crate::hle::service::audio::errors::RESULT_OPERATION_FAILED.1,
                )
            })
            .and_then(|audio_core| {
                audio_core.open_audio_output(
                    &name[0].name,
                    params,
                    process,
                    applet_resource_user_id,
                )
            });

        let Ok(opened) = result else {
            return result.err().unwrap_or_else(|| {
                ResultCode::from_module_description(
                    crate::hle::result::ErrorModule::Audio,
                    crate::hle::service::audio::errors::RESULT_OPERATION_FAILED.1,
                )
            });
        };

        let Some(kernel) = self.system.get().kernel() else {
            return ResultCode::from_module_description(
                crate::hle::result::ErrorModule::Audio,
                crate::hle::service::audio::errors::RESULT_OPERATION_FAILED.1,
            );
        };
        let (
            buffer_event_object_id,
            buffer_readable_event_object_id,
            buffer_event,
            buffer_readable_event,
        ) = super::audio_out::IAudioOut::create_buffer_event(kernel, &owner_process);

        let audio_out = Arc::new(super::audio_out::IAudioOut::new(
            opened.session,
            owner_process,
            buffer_event_object_id,
            buffer_readable_event_object_id,
            buffer_event,
            buffer_readable_event,
        ));

        *out_parameter_internal = AudioOutParameterInternalWire {
            sample_rate: opened.sample_rate,
            channel_count: opened.channel_count,
            sample_format: opened.sample_format,
            state: opened.state,
        };
        out_name[0].name = name[0].name;

        let mut response = CmifResponse::new(ctx, 6, 0, 1);
        response.push_result(RESULT_SUCCESS);
        response.push_raw(out_parameter_internal);
        response.push_interface(audio_out);
        RESULT_SUCCESS
    }

    fn list_audio_outs_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const IAudioOutManager) };
        log::debug!("IAudioOutManager::ListAudioOuts");
        let mut out_count = 0u32;
        let mut out_storage = CmifOutArrayBuffer::<
            AudioDeviceNameWire,
            { buffer_attr::BufferAttr_HipcMapAlias },
        >::from_ctx(ctx, 0);
        let mut out_audio_outs = out_storage.as_out_array();
        let result = service.list_audio_outs_auto(&mut out_audio_outs, &mut out_count);
        out_storage.write_back(ctx, 0, out_count as usize);
        let mut response = CmifResponse::new(ctx, 3, 0, 0);
        response.push_result(result);
        response.push_u32(out_count);
    }

    fn open_audio_out_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const IAudioOutManager) };
        log::info!("IAudioOutManager::OpenAudioOut");
        let mut request = CmifRequest::new(ctx);
        let params: AudioOutParameterWire = request.raw();
        let mut out_name_storage = CmifOutArrayBuffer::<
            AudioDeviceNameWire,
            { buffer_attr::BufferAttr_HipcMapAlias },
        >::from_ctx(ctx, 0);
        let mut out_name = out_name_storage.as_out_array();
        let mut in_name_storage = CmifInArrayBuffer::<
            AudioDeviceNameWire,
            { buffer_attr::BufferAttr_HipcMapAlias },
        >::from_ctx(ctx, 0);
        let in_name = in_name_storage.as_in_array();
        let mut out_parameter_internal = AudioOutParameterInternalWire::default();
        let result = service.open_audio_out_auto(
            ctx,
            &mut out_parameter_internal,
            &mut out_name,
            &in_name,
            params,
        );
        out_name_storage.write_back(ctx, 0, 1);
        if result.is_error() {
            CmifResponse::result_only(ctx, result);
        }
    }

    fn list_audio_outs_auto_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const IAudioOutManager) };
        log::debug!("IAudioOutManager::ListAudioOutsAuto");
        let mut out_count = 0u32;
        let mut out_storage = CmifOutArrayBuffer::<
            AudioDeviceNameWire,
            { buffer_attr::BufferAttr_HipcAutoSelect },
        >::from_ctx(ctx, 0);
        let mut out_audio_outs = out_storage.as_out_array();
        let result = service.list_audio_outs_auto(&mut out_audio_outs, &mut out_count);
        out_storage.write_back(ctx, 0, out_count as usize);
        let mut response = CmifResponse::new(ctx, 3, 0, 0);
        response.push_result(result);
        response.push_u32(out_count);
    }

    fn open_audio_out_auto_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const IAudioOutManager) };
        log::info!("IAudioOutManager::OpenAudioOutAuto");
        let mut request = CmifRequest::new(ctx);
        let params: AudioOutParameterWire = request.raw();
        let mut out_name_storage = CmifOutArrayBuffer::<
            AudioDeviceNameWire,
            { buffer_attr::BufferAttr_HipcAutoSelect },
        >::from_ctx(ctx, 0);
        let mut out_name = out_name_storage.as_out_array();
        let mut in_name_storage = CmifInArrayBuffer::<
            AudioDeviceNameWire,
            { buffer_attr::BufferAttr_HipcAutoSelect },
        >::from_ctx(ctx, 0);
        let in_name = in_name_storage.as_in_array();
        let mut out_parameter_internal = AudioOutParameterInternalWire::default();
        let result = service.open_audio_out_auto(
            ctx,
            &mut out_parameter_internal,
            &mut out_name,
            &in_name,
            params,
        );
        out_name_storage.write_back(ctx, 0, 1);
        if result.is_error() {
            CmifResponse::result_only(ctx, result);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_out_manager_registers_upstream_command_ids() {
        let service = IAudioOutManager::new(SystemRef::null());

        for cmd in [0_u32, 1, 2, 3] {
            assert!(service.handlers.contains_key(&cmd));
            assert!(service.handlers[&cmd].handler_callback.is_some());
        }
    }

    #[test]
    fn wire_layouts_match_upstream_sizes() {
        assert_eq!(std::mem::size_of::<AudioDeviceNameWire>(), 0x100);
        assert_eq!(std::mem::size_of::<AudioOutParameterWire>(), 0x8);
        assert_eq!(std::mem::size_of::<AudioOutParameterInternalWire>(), 0x10);
    }
}

impl SessionRequestHandler for IAudioOutManager {
    fn handle_sync_request(&self, context: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, context)
    }
}

impl ServiceFramework for IAudioOutManager {
    fn get_service_name(&self) -> &str {
        "audout:u"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}
