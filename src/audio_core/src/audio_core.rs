// SPDX-FileCopyrightText: 2026 reden contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Audio subsystem root and service-facing adapters.
//!
//! The manager/session adapters live here because the `core` and `audio_core`
//! crates cannot depend on one another. Upstream owns those adapters in its HLE
//! audio services; this dependency inversion preserves that behavior without a
//! crate cycle.

use crate::adsp::ADSP;
use crate::audio_in_manager::Manager as AudioInManager;
use crate::audio_manager::AudioManager;
use crate::audio_out_manager::Manager as AudioOutManager;
use crate::audio_render_manager::Manager as AudioRenderManager;
use crate::common::audio_renderer_parameter::AudioRendererParameterInternal;
use crate::out::audio_out::Out as AudioOut;
use crate::out::audio_out_system::AudioOutParameter;
use crate::r#in::audio_in::In as AudioIn;
use crate::r#in::audio_in_system::AudioInParameter;
use crate::renderer::audio_device::AudioDevice;
use crate::renderer::{Renderer, System as RendererSystem};
use crate::sink::sink::{new_sink_handle, SinkHandle};
use crate::sink::sink_details::create_sink_from_id;
use crate::SharedSystem;
use common::settings_enums::AudioEngine;
use parking_lot::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

pub struct AudioCore {
    system: SharedSystem,
    audio_manager: Arc<AudioManager>,
    audio_in_manager: Arc<Mutex<AudioInManager>>,
    audio_out_manager: Arc<Mutex<AudioOutManager>>,
    audio_render_manager: Arc<Mutex<AudioRenderManager>>,
    output_sink: SinkHandle,
    input_sink: SinkHandle,
    opus_decoder_manager: Mutex<Option<crate::opus::OpusDecoderManager>>,
    adsp: ADSP,
}

impl AudioCore {
    pub fn new(system: SharedSystem) -> Self {
        let (output_sink, input_sink) = Self::create_sinks();
        let adsp = ADSP::new(system.clone(), output_sink.clone());
        let audio_manager = Arc::new(AudioManager::new());
        let audio_in_manager = Arc::new(Mutex::new(AudioInManager::new(
            system.clone(),
            audio_manager.clone(),
        )));
        let audio_out_manager = Arc::new(Mutex::new(AudioOutManager::new(
            system.clone(),
            audio_manager.clone(),
        )));
        let audio_render_manager = Arc::new(Mutex::new(AudioRenderManager::new(
            system.clone(),
            adsp.audio_renderer(),
        )));
        Self {
            system,
            audio_manager,
            audio_in_manager,
            audio_out_manager,
            audio_render_manager,
            output_sink,
            input_sink,
            opus_decoder_manager: Mutex::new(None),
            adsp,
        }
    }

    pub fn shutdown(&mut self) {
        self.audio_manager.shutdown();

        // Upstream destroys Renderer::SystemManager before ADSP. Rust owns the
        // render manager here, so reproduce that order explicitly: otherwise
        // its worker can remain in AudioRenderer::Wait while ADSP is stopping.
        self.audio_render_manager.lock().stop();
        self.adsp.shutdown();

        // Rust service sessions can retain SinkHandle Arcs beyond AudioCore,
        // so explicitly close streams before Core::System can be released.
        self.input_sink.lock().close_streams();
        self.output_sink.lock().close_streams();
    }

    pub fn get_audio_manager(&self) -> Arc<AudioManager> {
        self.audio_manager.clone()
    }

    pub fn get_output_sink(&self) -> SinkHandle {
        self.output_sink.clone()
    }

    pub fn get_input_sink(&self) -> SinkHandle {
        self.input_sink.clone()
    }

    pub fn adsp(&self) -> &ADSP {
        &self.adsp
    }

    fn create_sinks() -> (SinkHandle, SinkHandle) {
        let settings = common::settings::values();
        let mut sink_id = *settings.sink_id.get_value();
        if let Ok(raw_sink_id) = std::env::var("RUZU_AUDIO_SINK") {
            if let Some(override_sink_id) = AudioEngine::from_string(&raw_sink_id.to_lowercase()) {
                log::info!(
                    "audio_core: overriding sink via RUZU_AUDIO_SINK={}",
                    override_sink_id
                );
                sink_id = override_sink_id;
            } else {
                log::warn!("audio_core: ignoring invalid RUZU_AUDIO_SINK={raw_sink_id}");
            }
        }
        let output_id = settings.audio_output_device_id.get_value().clone();
        let input_id = settings.audio_input_device_id.get_value().clone();
        (
            new_sink_handle(create_sink_from_id(sink_id, &output_id)),
            new_sink_handle(create_sink_from_id(sink_id, &input_id)),
        )
    }
}

impl Drop for AudioCore {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct AudioRendererSession {
    renderer: Mutex<Renderer>,
}

impl AudioRendererSession {
    fn new(renderer: Renderer) -> Self {
        Self {
            renderer: Mutex::new(renderer),
        }
    }
}

impl ruzu_core::core::AudioRendererSessionInterface for AudioRendererSession {
    fn get_sample_rate(&self) -> u32 {
        self.renderer.lock().get_system().lock().get_sample_rate()
    }

    fn get_sample_count(&self) -> u32 {
        self.renderer.lock().get_system().lock().get_sample_count()
    }

    fn get_mix_buffer_count(&self) -> u32 {
        self.renderer
            .lock()
            .get_system()
            .lock()
            .get_mix_buffer_count()
    }

    fn get_state(&self) -> u32 {
        u32::from(!self.renderer.lock().get_system().lock().is_active())
    }

    fn request_update(
        &self,
        input: &[u8],
        performance: &mut [u8],
        output: &mut [u8],
    ) -> ruzu_core::hle::result::ResultCode {
        let result = self
            .renderer
            .lock()
            .request_update(input, performance, output);
        ruzu_core::hle::result::ResultCode::new(result.raw())
    }

    fn start(&self) {
        self.renderer.lock().start();
    }

    fn stop(&self) {
        self.renderer.lock().stop();
    }

    fn finalize(&self) {
        self.renderer.lock().finalize();
    }

    fn supports_system_event(&self) -> bool {
        self.renderer
            .lock()
            .get_system()
            .lock()
            .get_execution_mode()
            != crate::common::audio_renderer_parameter::ExecutionMode::Manual
    }

    fn set_process(&self, process: *mut ruzu_core::hle::kernel::k_process::KProcess) {
        self.renderer.lock().set_process(process);
    }

    fn set_rendering_time_limit(&self, rendering_time_limit: u32) {
        self.renderer
            .lock()
            .get_system()
            .lock()
            .set_rendering_time_limit(rendering_time_limit);
    }

    fn get_rendering_time_limit(&self) -> u32 {
        self.renderer
            .lock()
            .get_system()
            .lock()
            .get_rendering_time_limit()
    }

    fn set_voice_drop_parameter(&self, voice_drop_parameter: f32) {
        self.renderer
            .lock()
            .get_system()
            .lock()
            .set_voice_drop_parameter(voice_drop_parameter);
    }

    fn get_voice_drop_parameter(&self) -> f32 {
        self.renderer
            .lock()
            .get_system()
            .lock()
            .get_voice_drop_parameter()
    }

    fn set_rendered_readable_event(
        &self,
        event: std::sync::Arc<
            std::sync::Mutex<ruzu_core::hle::kernel::k_readable_event::KReadableEvent>,
        >,
    ) {
        self.renderer.lock().set_rendered_readable_event(event);
    }

    fn set_process_arc(
        &self,
        process: std::sync::Arc<ruzu_core::hle::kernel::k_process::ProcessLock>,
    ) {
        self.renderer.lock().set_process_arc(process);
    }
}

struct AudioInSession {
    session: Arc<AudioIn>,
}

impl AudioInSession {
    fn new(session: Arc<AudioIn>) -> Self {
        Self { session }
    }
}

impl ruzu_core::core::AudioInSessionImpl for AudioInSession {
    fn get_state(&self) -> u32 {
        self.session.get_state() as u32
    }

    fn start(&self) -> ruzu_core::hle::result::ResultCode {
        ruzu_core::hle::result::ResultCode::new(self.session.start_system().raw())
    }

    fn stop(&self) -> ruzu_core::hle::result::ResultCode {
        ruzu_core::hle::result::ResultCode::new(self.session.stop_system().raw())
    }

    fn free(&self) {
        self.session.free();
    }

    fn append_buffer(
        &self,
        buffer: ruzu_core::core::AudioInBufferWire,
        buffer_client_ptr: u64,
    ) -> ruzu_core::hle::result::ResultCode {
        let parsed = crate::r#in::AudioInBuffer {
            next: buffer.next,
            samples: buffer.samples,
            capacity: buffer.capacity,
            size: buffer.size,
            offset: buffer.offset,
        };
        ruzu_core::hle::result::ResultCode::new(
            self.session.append_buffer(parsed, buffer_client_ptr).raw(),
        )
    }

    fn get_released_buffers(&self, out_tags: &mut [u64]) -> u32 {
        self.session.get_released_buffers(out_tags)
    }

    fn contains_buffer(&self, buffer_client_ptr: u64) -> bool {
        self.session.contains_audio_buffer(buffer_client_ptr)
    }

    fn get_buffer_count(&self) -> u32 {
        self.session.get_buffer_count()
    }

    fn set_device_gain(&self, gain: f32) {
        self.session.set_volume(gain);
    }

    fn get_device_gain(&self) -> f32 {
        self.session.get_volume()
    }

    fn flush_audio_in_buffers(&self) -> bool {
        self.session.flush_audio_in_buffers()
    }

    fn set_buffer_readable_event(
        &self,
        event: Arc<StdMutex<ruzu_core::hle::kernel::k_readable_event::KReadableEvent>>,
    ) {
        self.session.set_buffer_readable_event(event);
    }

    fn set_process_arc(&self, process: Arc<ruzu_core::hle::kernel::k_process::ProcessLock>) {
        self.session.set_process_arc(process);
    }
}

struct AudioOutSession {
    session: Arc<AudioOut>,
}

impl AudioOutSession {
    fn new(session: Arc<AudioOut>) -> Self {
        Self { session }
    }
}

impl ruzu_core::core::AudioOutSessionImpl for AudioOutSession {
    fn get_state(&self) -> u32 {
        self.session.get_state() as u32
    }

    fn start(&self) -> ruzu_core::hle::result::ResultCode {
        ruzu_core::hle::result::ResultCode::new(self.session.start_system().raw())
    }

    fn stop(&self) -> ruzu_core::hle::result::ResultCode {
        ruzu_core::hle::result::ResultCode::new(self.session.stop_system().raw())
    }

    fn free(&self) {
        self.session.free();
    }

    fn append_buffer(
        &self,
        buffer: ruzu_core::core::AudioOutBufferWire,
        buffer_client_ptr: u64,
    ) -> ruzu_core::hle::result::ResultCode {
        let parsed = crate::out::AudioOutBuffer {
            next: buffer.next,
            samples: buffer.samples,
            capacity: buffer.capacity,
            size: buffer.size,
            offset: buffer.offset,
        };
        ruzu_core::hle::result::ResultCode::new(
            self.session.append_buffer(parsed, buffer_client_ptr).raw(),
        )
    }

    fn get_released_buffers(&self, out_tags: &mut [u64]) -> u32 {
        self.session.get_released_buffers(out_tags)
    }

    fn contains_buffer(&self, buffer_client_ptr: u64) -> bool {
        self.session.contains_audio_buffer(buffer_client_ptr)
    }

    fn get_buffer_count(&self) -> u32 {
        self.session.get_buffer_count()
    }

    fn get_played_sample_count(&self) -> u64 {
        self.session.get_played_sample_count()
    }

    fn flush_audio_out_buffers(&self) -> bool {
        self.session.flush_audio_out_buffers()
    }

    fn set_volume(&self, volume: f32) {
        self.session.set_volume(volume);
    }

    fn get_volume(&self) -> f32 {
        self.session.get_volume()
    }

    fn set_buffer_readable_event(
        &self,
        event: Arc<StdMutex<ruzu_core::hle::kernel::k_readable_event::KReadableEvent>>,
    ) {
        self.session.set_buffer_readable_event(event);
    }

    fn set_process_arc(&self, process: Arc<ruzu_core::hle::kernel::k_process::ProcessLock>) {
        self.session.set_process_arc(process);
    }
}

impl ruzu_core::core::AudioCoreInterface for AudioCore {
    fn set_audio_output_sink_volume(&self, volume: f32) {
        let mut sink = self.output_sink.lock();
        sink.set_system_volume(volume);
        sink.set_device_volume(volume);
    }

    fn set_all_audio_out_volume(&self, volume: f32) {
        // IAudioOutManager::SetAllAudioOutVolume lives on the HLE service
        // upstream. Its manager is behind this existing crate-cycle bridge in
        // Rust; keep the manager locked while updating the same live sessions.
        let manager = self.audio_out_manager.lock();
        let sessions = manager.sessions.lock().clone();
        for session in sessions.iter().flatten() {
            session.set_volume(volume);
        }
    }

    fn get_audio_renderer_work_buffer_size(&self, params: &[u8; 0x34]) -> Option<u64> {
        let parsed = unsafe {
            core::ptr::read_unaligned(params.as_ptr() as *const AudioRendererParameterInternal)
        };
        Some(RendererSystem::get_work_buffer_size(&parsed))
    }

    fn list_audio_device_name(
        &self,
        applet_resource_user_id: u64,
        revision: u32,
        out_names: &mut [[u8; 0x100]],
    ) -> u32 {
        let device = AudioDevice::new(self.get_output_sink(), applet_resource_user_id, revision);
        let mut names =
            vec![crate::renderer::audio_device::AudioDeviceName::new(); out_names.len()];
        let count = device.list_audio_device_name(&mut names) as usize;
        for (dst, src) in out_names.iter_mut().zip(names.iter()).take(count) {
            *dst = src.name;
        }
        count as u32
    }

    fn list_audio_output_device_name(
        &self,
        applet_resource_user_id: u64,
        revision: u32,
        out_names: &mut [[u8; 0x100]],
    ) -> u32 {
        let device = AudioDevice::new(self.get_output_sink(), applet_resource_user_id, revision);
        let mut names =
            vec![crate::renderer::audio_device::AudioDeviceName::new(); out_names.len()];
        let count = device.list_audio_output_device_name(&mut names) as usize;
        for (dst, src) in out_names.iter_mut().zip(names.iter()).take(count) {
            *dst = src.name;
        }
        count as u32
    }

    fn set_audio_device_volume(&self, applet_resource_user_id: u64, revision: u32, volume: f32) {
        let device = AudioDevice::new(self.get_output_sink(), applet_resource_user_id, revision);
        device.set_device_volumes(volume);
    }

    fn get_audio_device_volume(
        &self,
        applet_resource_user_id: u64,
        revision: u32,
        name: &str,
    ) -> f32 {
        let device = AudioDevice::new(self.get_output_sink(), applet_resource_user_id, revision);
        device.get_device_volume(name)
    }

    fn get_audio_output_system_channels(&self) -> u32 {
        self.get_output_sink().lock().get_system_channels()
    }

    fn list_audio_input_device_name(&self, out_names: &mut [[u8; 0x100]], filter: bool) -> u32 {
        let mut names =
            vec![crate::renderer::audio_device::AudioDeviceName::new(); out_names.len()];
        let count = self
            .audio_in_manager
            .lock()
            .get_device_names(&mut names, filter) as usize;
        for (dst, src) in out_names.iter_mut().zip(names.iter()).take(count) {
            *dst = src.name;
        }
        count as u32
    }

    fn open_audio_input(
        &self,
        name: &[u8; 0x100],
        protocol: [u32; 2],
        params: ruzu_core::core::AudioInParameterWire,
        process: *mut ruzu_core::hle::kernel::k_process::KProcess,
        applet_resource_user_id: u64,
    ) -> std::result::Result<ruzu_core::core::AudioInOpenResponse, ruzu_core::hle::result::ResultCode>
    {
        let parsed = AudioInParameter {
            sample_rate: params.sample_rate,
            channel_count: params.channel_count,
            reserved: params.reserved,
        };
        let mut session_id = 0usize;
        let manager_arc = Arc::clone(&self.audio_in_manager);
        {
            let mut manager = manager_arc.lock();
            let result = manager.link_to_manager();
            if result.is_error() {
                return Err(ruzu_core::hle::result::ResultCode::new(result.raw()));
            }
            let result = manager.acquire_session_id(&mut session_id);
            if result.is_error() {
                return Err(ruzu_core::hle::result::ResultCode::new(result.raw()));
            }
        }

        let device_name = {
            let nul = name
                .iter()
                .position(|&byte| byte == 0)
                .unwrap_or(name.len());
            String::from_utf8_lossy(&name[..nul]).into_owned()
        };

        let buffer_event = Arc::new(AtomicBool::new(false));
        let mut system = crate::r#in::System::new(
            self.system.clone(),
            self.get_input_sink(),
            buffer_event.clone(),
            session_id,
        );
        if !process.is_null() {
            if let Some(memory) = unsafe { (*process).get_memory() } {
                system.set_guest_memory(Some(Arc::new(crate::device::ProcessMemoryProvider::new(
                    memory,
                ))));
            }
        }
        system.set_audio_manager(Some(self.audio_manager.clone()));
        let result = system.initialize(device_name, &parsed, applet_resource_user_id);
        if result.is_error() {
            manager_arc.lock().release_session_id(session_id);
            return Err(ruzu_core::hle::result::ResultCode::new(result.raw()));
        }

        let release_manager = Arc::clone(&manager_arc);
        let session = Arc::new(AudioIn::new(
            system,
            buffer_event,
            Arc::new(move |released_session_id| {
                release_manager
                    .lock()
                    .release_session_id(released_session_id);
            }),
        ));

        {
            let manager = manager_arc.lock();
            manager.set_session(session_id, Some(session.clone()));
        }
        {
            let mut manager = manager_arc.lock();
            manager.applet_resource_user_ids[session_id] = applet_resource_user_id as usize;
        }

        let out_system = session.get_system();
        let sample_rate = out_system.get_sample_rate();
        let channel_count = out_system.get_channel_count() as u32;
        let sample_format = out_system.get_sample_format() as u32;
        let state = match out_system.get_state() {
            crate::r#in::State::Started => 0,
            crate::r#in::State::Stopped => 1,
        };
        let is_uac = out_system.is_uac();
        let opened_name = out_system.get_name();
        let response_name = if protocol == [0, 0] {
            if is_uac {
                "UacIn"
            } else {
                "DeviceIn"
            }
        } else {
            opened_name.as_str()
        };
        let mut out_name = [0u8; 0x100];
        let response_bytes = response_name.as_bytes();
        let len = response_bytes.len().min(out_name.len().saturating_sub(1));
        out_name[..len].copy_from_slice(&response_bytes[..len]);
        drop(out_system);

        Ok(ruzu_core::core::AudioInOpenResponse {
            session: ruzu_core::core::AudioInSession::from_arc(std::sync::Arc::new(
                AudioInSession::new(session),
            )),
            sample_rate,
            channel_count,
            sample_format,
            state,
            name: out_name,
        })
    }

    fn open_audio_output(
        &self,
        name: &[u8; 0x100],
        params: ruzu_core::core::AudioOutParameterWire,
        process: *mut ruzu_core::hle::kernel::k_process::KProcess,
        applet_resource_user_id: u64,
    ) -> std::result::Result<
        ruzu_core::core::AudioOutOpenResponse,
        ruzu_core::hle::result::ResultCode,
    > {
        let parsed = AudioOutParameter {
            sample_rate: params.sample_rate,
            channel_count: params.channel_count,
            reserved: params.reserved,
        };
        let mut session_id = 0usize;
        let manager_arc = Arc::clone(&self.audio_out_manager);
        {
            let mut manager = manager_arc.lock();
            let result = manager.link_to_manager();
            if result.is_error() {
                return Err(ruzu_core::hle::result::ResultCode::new(result.raw()));
            }
            let result = manager.acquire_session_id(&mut session_id);
            if result.is_error() {
                return Err(ruzu_core::hle::result::ResultCode::new(result.raw()));
            }
        }

        let device_name = {
            let nul = name
                .iter()
                .position(|&byte| byte == 0)
                .unwrap_or(name.len());
            String::from_utf8_lossy(&name[..nul]).into_owned()
        };

        let buffer_event = Arc::new(AtomicBool::new(false));
        let mut system = crate::out::System::new(
            self.system.clone(),
            self.get_output_sink(),
            buffer_event.clone(),
            session_id,
        );
        if !process.is_null() {
            if let Some(memory) = unsafe { (*process).get_memory() } {
                system.set_guest_memory(Some(Arc::new(crate::device::ProcessMemoryProvider::new(
                    memory,
                ))));
            }
        }
        system.set_audio_manager(Some(self.audio_manager.clone()));
        let result = system.initialize(device_name, &parsed, applet_resource_user_id);
        if result.is_error() {
            manager_arc.lock().release_session_id(session_id);
            return Err(ruzu_core::hle::result::ResultCode::new(result.raw()));
        }

        let release_manager = Arc::clone(&manager_arc);
        let session = Arc::new(AudioOut::new(
            system,
            buffer_event,
            Arc::new(move |released_session_id| {
                release_manager
                    .lock()
                    .release_session_id(released_session_id);
            }),
        ));

        {
            let manager = manager_arc.lock();
            manager.set_session(session_id, Some(session.clone()));
        }
        {
            let mut manager = manager_arc.lock();
            manager.applet_resource_user_ids[session_id] = applet_resource_user_id as usize;
        }

        let out_system = session.get_system();
        let sample_rate = out_system.get_sample_rate();
        let channel_count = out_system.get_channel_count() as u32;
        let sample_format = out_system.get_sample_format() as u32;
        let state = match out_system.get_state() {
            crate::out::State::Started => 0,
            crate::out::State::Stopped => 1,
        };
        drop(out_system);

        Ok(ruzu_core::core::AudioOutOpenResponse {
            session: ruzu_core::core::AudioOutSession::from_arc(std::sync::Arc::new(
                AudioOutSession::new(session),
            )),
            sample_rate,
            channel_count,
            sample_format,
            state,
        })
    }

    fn open_audio_renderer(
        &self,
        params: &[u8; 0x34],
        transfer_memory: *mut ruzu_core::hle::kernel::k_transfer_memory::KTransferMemory,
        transfer_memory_size: u64,
        process: *mut ruzu_core::hle::kernel::k_process::KProcess,
        applet_resource_user_id: u64,
        rendered_event: Arc<StdMutex<ruzu_core::hle::kernel::k_event::KEvent>>,
    ) -> std::result::Result<
        Box<dyn ruzu_core::core::AudioRendererSessionInterface>,
        ruzu_core::hle::result::ResultCode,
    > {
        let parsed = unsafe {
            core::ptr::read_unaligned(params.as_ptr() as *const AudioRendererParameterInternal)
        };
        let session_id = {
            let mut manager = self.audio_render_manager.lock();
            manager.get_session_id()
        };
        if session_id < 0 {
            return Err(ruzu_core::hle::result::ResultCode::new(
                crate::errors::RESULT_OUT_OF_SESSIONS.raw(),
            ));
        }

        let renderer_system = Arc::new(Mutex::new(RendererSystem::new(
            self.system.clone(),
            self.adsp.audio_renderer(),
            rendered_event,
        )));
        let mut renderer = Renderer::new(self.audio_render_manager.clone(), renderer_system);
        let result = renderer.initialize(
            &parsed,
            transfer_memory,
            transfer_memory_size,
            process,
            applet_resource_user_id,
            session_id,
        );
        if result.is_error() {
            self.audio_render_manager
                .lock()
                .release_session_id(session_id);
            return Err(ruzu_core::hle::result::ResultCode::new(result.raw()));
        }

        Ok(Box::new(AudioRendererSession::new(renderer)))
    }

    fn open_opus_decoder(
        &self,
        sample_rate: u32,
        channel_count: u32,
        use_large_frame_size: bool,
        transfer_memory_size: u64,
    ) -> std::result::Result<
        Box<dyn ruzu_core::core::OpusDecoderInterface>,
        ruzu_core::hle::result::ResultCode,
    > {
        let hardware_opus = {
            let mut manager = self.opus_decoder_manager.lock();
            manager
                .get_or_insert_with(|| {
                    crate::opus::OpusDecoderManager::new_from_adsp(self.adsp.opus_decoder())
                })
                .get_hardware_opus()
                .clone()
        };
        let mut decoder = crate::opus::OpusDecoder::new(hardware_opus);
        let params = crate::opus::OpusParametersEx {
            sample_rate,
            channel_count,
            use_large_frame_size,
            padding: [0; 7],
        };
        let rc = decoder.initialize(&params, transfer_memory_size);
        if rc.is_error() {
            return Err(ruzu_core::hle::result::ResultCode::new(rc.raw()));
        }
        Ok(Box::new(OpusDecoderSession {
            inner: parking_lot::Mutex::new(decoder),
        }))
    }

    fn open_opus_decoder_for_multi_stream(
        &self,
        sample_rate: u32,
        channel_count: u32,
        total_stream_count: u32,
        stereo_stream_count: u32,
        use_large_frame_size: bool,
        mappings: &[u8; 256],
        transfer_memory_size: u64,
    ) -> std::result::Result<
        Box<dyn ruzu_core::core::OpusDecoderInterface>,
        ruzu_core::hle::result::ResultCode,
    > {
        let hardware_opus = {
            let mut manager = self.opus_decoder_manager.lock();
            manager
                .get_or_insert_with(|| {
                    crate::opus::OpusDecoderManager::new_from_adsp(self.adsp.opus_decoder())
                })
                .get_hardware_opus()
                .clone()
        };
        let mut decoder = crate::opus::OpusDecoder::new(hardware_opus);
        let mut params = crate::opus::OpusMultiStreamParametersEx::default();
        params.sample_rate = sample_rate;
        params.channel_count = channel_count;
        params.total_stream_count = total_stream_count;
        params.stereo_stream_count = stereo_stream_count;
        params.use_large_frame_size = use_large_frame_size;
        params.mappings[..mappings.len()].copy_from_slice(mappings);
        let rc = decoder.initialize_multi_stream(&params, transfer_memory_size);
        if rc.is_error() {
            return Err(ruzu_core::hle::result::ResultCode::new(rc.raw()));
        }
        Ok(Box::new(OpusDecoderSession {
            inner: parking_lot::Mutex::new(decoder),
        }))
    }

    fn get_opus_work_buffer_size(
        &self,
        sample_rate: u32,
        channel_count: u32,
        use_large_frame_size: bool,
    ) -> std::result::Result<u32, ruzu_core::hle::result::ResultCode> {
        let mut manager = self.opus_decoder_manager.lock();
        let manager = manager.get_or_insert_with(|| {
            crate::opus::OpusDecoderManager::new_from_adsp(self.adsp.opus_decoder())
        });
        let params = crate::opus::OpusParametersEx {
            sample_rate,
            channel_count,
            use_large_frame_size,
            padding: [0; 7],
        };
        let mut size: u32 = 0;
        let result = manager.get_work_buffer_size_ex(&params, &mut size);
        if result.is_error() {
            Err(ruzu_core::hle::result::ResultCode::new(result.raw()))
        } else {
            Ok(size)
        }
    }

    fn get_opus_work_buffer_size_for_multi_stream(
        &self,
        sample_rate: u32,
        channel_count: u32,
        total_stream_count: u32,
        stereo_stream_count: u32,
        use_large_frame_size: bool,
    ) -> std::result::Result<u32, ruzu_core::hle::result::ResultCode> {
        let mut manager = self.opus_decoder_manager.lock();
        let manager = manager.get_or_insert_with(|| {
            crate::opus::OpusDecoderManager::new_from_adsp(self.adsp.opus_decoder())
        });
        let mut params = crate::opus::OpusMultiStreamParametersEx::default();
        params.sample_rate = sample_rate;
        params.channel_count = channel_count;
        params.total_stream_count = total_stream_count;
        params.stereo_stream_count = stereo_stream_count;
        params.use_large_frame_size = use_large_frame_size;
        let mut size: u32 = 0;
        let result = manager.get_work_buffer_size_for_multi_stream_ex(&params, &mut size);
        if result.is_error() {
            Err(ruzu_core::hle::result::ResultCode::new(result.raw()))
        } else {
            Ok(size)
        }
    }
}

/// Per-session Opus decoder adapter implementing `OpusDecoderInterface`.
/// Wraps `audio_core::opus::OpusDecoder` (the audio_core-private type) so
/// the HLE service in `core` can dispatch via the trait without depending
/// on `audio_core` directly (which would create a crate cycle).
struct OpusDecoderSession {
    inner: parking_lot::Mutex<crate::opus::OpusDecoder>,
}

impl ruzu_core::core::OpusDecoderInterface for OpusDecoderSession {
    fn decode_interleaved(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        reset: bool,
    ) -> std::result::Result<(u32, u32, u64), ruzu_core::hle::result::ResultCode> {
        let mut data_size: u32 = 0;
        let mut sample_count: u32 = 0;
        let mut time_taken: u64 = 0;
        let rc = self.inner.lock().decode_interleaved(
            &mut data_size,
            Some(&mut time_taken),
            &mut sample_count,
            input,
            output,
            reset,
        );
        if rc.is_error() {
            return Err(ruzu_core::hle::result::ResultCode::new(rc.raw()));
        }
        Ok((data_size, sample_count, time_taken))
    }

    fn decode_interleaved_for_multi_stream(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        reset: bool,
    ) -> std::result::Result<(u32, u32, u64), ruzu_core::hle::result::ResultCode> {
        let mut data_size: u32 = 0;
        let mut sample_count: u32 = 0;
        let mut time_taken: u64 = 0;
        let rc = self.inner.lock().decode_interleaved_for_multi_stream(
            &mut data_size,
            Some(&mut time_taken),
            &mut sample_count,
            input,
            output,
            reset,
        );
        if rc.is_error() {
            return Err(ruzu_core::hle::result::ResultCode::new(rc.raw()));
        }
        Ok((data_size, sample_count, time_taken))
    }

    fn set_context(&mut self, context: &[u8]) -> ruzu_core::hle::result::ResultCode {
        let rc = self.inner.lock().set_context(context);
        ruzu_core::hle::result::ResultCode::new(rc.raw())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::audio_renderer_parameter::ExecutionMode;
    use crate::common::feature_support::CURRENT_REVISION;
    use ruzu_core::core::AudioCoreInterface;
    use std::sync::Arc;

    fn make_audio_core() -> AudioCore {
        AudioCore::new(crate::make_test_system())
    }

    #[test]
    fn bulk_output_volume_updates_live_sessions_without_changing_membership() {
        let audio_core = make_audio_core();
        let create_session = |id| {
            let event = Arc::new(AtomicBool::new(false));
            let system = crate::out::System::new(
                audio_core.system.clone(),
                new_sink_handle(Box::new(crate::sink::NullSink::new("null"))),
                event.clone(),
                id,
            );
            Arc::new(AudioOut::new(system, event, Arc::new(|_| {})))
        };
        let first = create_session(0);
        let second = create_session(4);
        audio_core.audio_out_manager.lock().set_session(0, Some(first.clone()));
        audio_core.audio_out_manager.lock().set_session(4, Some(second.clone()));
        for volume in [0.25, 0.0, 1.0] {
            audio_core.set_all_audio_out_volume(volume);
            assert_eq!(first.get_volume(), volume);
            assert_eq!(second.get_volume(), volume);
        }
        audio_core.audio_out_manager.lock().set_session(0, None);
        audio_core.set_all_audio_out_volume(0.5);
        assert_eq!(first.get_volume(), 1.0);
        assert_eq!(second.get_volume(), 0.5);
        let manager = audio_core.audio_out_manager.lock();
        let sessions = manager.sessions.lock();
        assert_eq!(sessions.iter().flatten().count(), 1);
        assert!(Arc::ptr_eq(sessions[4].as_ref().unwrap(), &second));
    }

    #[test]
    fn audio_core_bridge_reports_renderer_work_buffer_size() {
        let audio_core = make_audio_core();
        let params = AudioRendererParameterInternal {
            sample_rate: 48_000,
            sample_count: 240,
            mixes: 1,
            sub_mixes: 0,
            voices: 24,
            sinks: 1,
            effects: 0,
            perf_frames: 0,
            voice_drop_enabled: 0,
            unk_21: 0,
            rendering_device: 0,
            execution_mode: ExecutionMode::Auto,
            splitter_infos: 0,
            splitter_destinations: 0,
            external_context_size: 0,
            revision: CURRENT_REVISION,
        };

        let raw = unsafe {
            *(core::ptr::addr_of!(params)
                as *const [u8; core::mem::size_of::<AudioRendererParameterInternal>()])
        };

        assert_eq!(
            audio_core.get_audio_renderer_work_buffer_size(&raw),
            Some(RendererSystem::get_work_buffer_size(&params))
        );
    }

    #[test]
    fn audio_core_bridge_opens_renderer_session_with_real_backend() {
        let audio_core = make_audio_core();
        let params = AudioRendererParameterInternal {
            sample_rate: 48_000,
            sample_count: 240,
            mixes: 1,
            sub_mixes: 0,
            voices: 24,
            sinks: 1,
            effects: 0,
            perf_frames: 0,
            voice_drop_enabled: 0,
            unk_21: 0,
            rendering_device: 0,
            execution_mode: ExecutionMode::Auto,
            splitter_infos: 0,
            splitter_destinations: 0,
            external_context_size: 0,
            revision: CURRENT_REVISION,
        };
        let raw = unsafe {
            *(core::ptr::addr_of!(params)
                as *const [u8; core::mem::size_of::<AudioRendererParameterInternal>()])
        };

        let process = Box::into_raw(Box::new(ruzu_core::hle::kernel::k_process::KProcess::new()));
        let transfer_memory = Box::into_raw(Box::new(
            ruzu_core::hle::kernel::k_transfer_memory::KTransferMemory::new(),
        ));
        let session = audio_core
            .open_audio_renderer(
                &raw,
                transfer_memory,
                RendererSystem::get_work_buffer_size(&params),
                process,
                1,
                Arc::new(StdMutex::new(ruzu_core::hle::kernel::k_event::KEvent::new())),
            )
            .expect("audio renderer session should initialize");

        assert_eq!(session.get_sample_rate(), 48_000);
        assert_eq!(session.get_sample_count(), 240);
        assert_eq!(session.get_mix_buffer_count(), 1);
        assert_eq!(session.get_state(), 1);
        assert!(session.supports_system_event());
        drop(session);
        unsafe {
            drop(Box::from_raw(process));
            drop(Box::from_raw(transfer_memory));
        }
    }

    #[test]
    fn audio_core_bridge_decodes_non_silent_opus_in_independent_sessions() {
        let audio_core = make_audio_core();
        let work_buffer_size = audio_core
            .get_opus_work_buffer_size(48_000, 2, false)
            .expect("valid stereo Opus workbuffer size");
        let mut first = audio_core
            .open_opus_decoder(48_000, 2, false, work_buffer_size as u64)
            .expect("first Opus session");
        let mut second = audio_core
            .open_opus_decoder(48_000, 2, false, work_buffer_size as u64)
            .expect("second Opus session");

        let payload = crate::adsp::apps::opus::opus_decode_object::tests::encoded_stereo_packet();
        let mut packet = Vec::with_capacity(8 + payload.len());
        packet.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        packet.extend_from_slice(&0u32.to_be_bytes());
        packet.extend_from_slice(&payload);

        for decoder in [&mut first, &mut second] {
            let mut output = vec![0; 960 * 2 * core::mem::size_of::<i16>()];
            let (_, sample_count, _) = decoder
                .decode_interleaved(&packet, &mut output, false)
                .expect("Opus packet should decode through the service bridge");
            assert_eq!(sample_count, 960);
            assert!(output.chunks_exact(2).any(|sample| sample != [0, 0]));
        }

        let mut output = vec![0; 960 * 2 * core::mem::size_of::<i16>()];
        let (_, sample_count, _) = first
            .decode_interleaved(&packet, &mut output, false)
            .expect("first Opus session should remain independent");
        assert_eq!(sample_count, 960);
        assert!(output.chunks_exact(2).any(|sample| sample != [0, 0]));
    }
}
