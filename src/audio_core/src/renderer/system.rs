use crate::adsp::adsp::AudioRendererHandle;
use crate::adsp::apps::audio_renderer::command_buffer::ProcessHandle;
use crate::common::audio_renderer_parameter::{AudioRendererParameterInternal, ExecutionMode};
use crate::common::common::{
    align_audio, MAX_CHANNELS, MAX_EFFECTS, MAX_WAVE_BUFFERS, TARGET_SAMPLE_COUNT,
};
use crate::common::feature_support::check_valid_revision;
use crate::errors::{RESULT_INSUFFICIENT_BUFFER, RESULT_INVALID_HANDLE, RESULT_INVALID_REVISION};
use crate::renderer::behavior::BehaviorInfo;
use crate::renderer::command::{
    CommandBuffer, CommandGenerator, CommandListHeader, CommandProcessingTimeEstimator,
};
use crate::renderer::effect::{EffectContext, EffectInfoBase, EffectResultState};
use crate::renderer::memory::{MemoryPoolInfo, PoolLocation, PoolMapper};
use crate::renderer::mix::{MixContext, MixInfo};
use crate::renderer::nodes::{EdgeMatrix, NodeStates};
use crate::renderer::performance::PerformanceManager;
use crate::renderer::sink::{SinkContext, SinkInfoBase};
use crate::renderer::splitter::SplitterContext;
use crate::renderer::upsampler::{UpsamplerInfo, UpsamplerManager};
use crate::renderer::voice::{VoiceChannelResource, VoiceContext, VoiceInfo, VoiceState};
use crate::{Result, SharedSystem};
use common::ResultCode;
use parking_lot::{Condvar, Mutex};
use ruzu_core::hle::kernel::k_event::KEvent;
use ruzu_core::hle::kernel::k_process::{KProcess, ProcessLock};
use ruzu_core::hle::kernel::k_readable_event::KReadableEvent;
use ruzu_core::hle::kernel::k_transfer_memory::KTransferMemory;
use std::mem::{size_of, size_of_val};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Started = 0,
    Stopped = 2,
}

pub(crate) struct TerminateEvent {
    signaled: Mutex<bool>,
    condvar: Condvar,
}

impl TerminateEvent {
    fn new() -> Self {
        Self {
            signaled: Mutex::new(false),
            condvar: Condvar::new(),
        }
    }

    fn reset(&self) {
        *self.signaled.lock() = false;
    }

    pub(crate) fn signal(&self) {
        *self.signaled.lock() = true;
        self.condvar.notify_all();
    }

    pub(crate) fn wait(&self) {
        let mut signaled = self.signaled.lock();
        while !*signaled {
            self.condvar.wait(&mut signaled);
        }
    }

    #[cfg(test)]
    fn is_signaled(&self) -> bool {
        *self.signaled.lock()
    }
}

pub struct System {
    core: SharedSystem,
    audio_renderer: AudioRendererHandle,
    initialized: bool,
    rendered_event: Arc<StdMutex<KEvent>>,
    /// Direct reference to the readable end of the rendered event.
    rendered_readable_event: Option<Arc<StdMutex<KReadableEvent>>>,
    /// Process Arc for safe signal_rendered_event access. The raw process_ptr
    /// bypasses Mutex synchronization, causing data races when the audio thread
    /// reads waiter state that the game thread writes under Mutex protection.
    process_arc: Option<Arc<ProcessLock>>,
    terminate_event: Arc<TerminateEvent>,
    behavior: BehaviorInfo,
    voice_context: VoiceContext,
    mix_context: MixContext,
    effect_context: EffectContext,
    sink_context: SinkContext,
    splitter_context: SplitterContext,
    performance_manager: PerformanceManager,
    memory_pool_workbuffer: Vec<MemoryPoolInfo>,
    samples_workbuffer: Vec<i32>,
    depop_buffer: Vec<i32>,
    voice_state_pool: MemoryPoolInfo,
    effect_state_pool: MemoryPoolInfo,
    effect_result_state_pool: MemoryPoolInfo,
    samples_workbuffer_pool: MemoryPoolInfo,
    depop_buffer_pool: MemoryPoolInfo,
    command_workbuffer: Vec<u8>,
    command_workbuffer_pool: MemoryPoolInfo,
    upsampler_manager: UpsamplerManager,
    frames_elapsed: u64,
    total_ticks_elapsed: u64,
    ticks_spent_updating: u64,
    num_command_lists_generated: u64,
    num_times_updated: u64,
    render_start_tick: u64,
    adsp_behind: bool,
    num_voices_dropped: u32,
    command_buffer_size: u64,
    reset_command_buffers: bool,
    state: State,
    active: bool,
    sample_rate: u32,
    sample_count: u32,
    mix_buffer_count: i16,
    voice_channels: i32,
    upsampler_count: u32,
    render_device: u32,
    execution_mode: ExecutionMode,
    applet_resource_user_id: u64,
    process: ProcessHandle,
    session_id: i32,
    rendering_time_limit_percent: u32,
    drop_voice_enabled: bool,
    voice_drop_parameter: f32,
}

impl System {
    pub fn new(
        core: SharedSystem,
        audio_renderer: AudioRendererHandle,
        rendered_event: Arc<StdMutex<KEvent>>,
    ) -> Self {
        Self {
            core,
            audio_renderer,
            initialized: false,
            rendered_event,
            rendered_readable_event: None,
            process_arc: None,
            terminate_event: Arc::new(TerminateEvent::new()),
            behavior: BehaviorInfo::new(),
            voice_context: VoiceContext::new(),
            mix_context: MixContext::new(),
            effect_context: EffectContext::new(),
            sink_context: SinkContext::default(),
            splitter_context: SplitterContext::new(),
            performance_manager: PerformanceManager::new(),
            memory_pool_workbuffer: Vec::new(),
            samples_workbuffer: Vec::new(),
            depop_buffer: Vec::new(),
            voice_state_pool: MemoryPoolInfo::new(PoolLocation::Dsp),
            effect_state_pool: MemoryPoolInfo::new(PoolLocation::Dsp),
            effect_result_state_pool: MemoryPoolInfo::new(PoolLocation::Dsp),
            samples_workbuffer_pool: MemoryPoolInfo::new(PoolLocation::Dsp),
            depop_buffer_pool: MemoryPoolInfo::new(PoolLocation::Dsp),
            command_workbuffer: Vec::new(),
            command_workbuffer_pool: MemoryPoolInfo::new(PoolLocation::Dsp),
            upsampler_manager: UpsamplerManager::default(),
            frames_elapsed: 0,
            total_ticks_elapsed: 0,
            ticks_spent_updating: 0,
            num_command_lists_generated: 0,
            num_times_updated: 0,
            render_start_tick: 0,
            adsp_behind: false,
            num_voices_dropped: 0,
            command_buffer_size: 0,
            reset_command_buffers: false,
            state: State::Stopped,
            active: false,
            sample_rate: 0,
            sample_count: 0,
            mix_buffer_count: 0,
            voice_channels: 0,
            upsampler_count: 0,
            render_device: 0,
            execution_mode: ExecutionMode::Auto,
            applet_resource_user_id: 0,
            process: ProcessHandle::default(),
            session_id: -1,
            rendering_time_limit_percent: 100,
            drop_voice_enabled: false,
            voice_drop_parameter: 1.0,
        }
    }

    #[cfg(test)]
    pub(crate) fn new_for_tests(core: SharedSystem, audio_renderer: AudioRendererHandle) -> Self {
        Self::new(core, audio_renderer, Arc::new(StdMutex::new(KEvent::new())))
    }

    /// Matches upstream `adsp_rendered_event->Clear()` at the end of Update().
    fn clear_rendered_event(&self) {
        if let Some(ref readable_event) = self.rendered_readable_event {
            if std::env::var_os("RUZU_TRACE_AUDIO_EVENT").is_some() {
                let object_id = readable_event.lock().unwrap().object_id;
                log::info!(
                    "audio_core::System::clear_rendered_event session_id={} readable_event_object_id={}",
                    self.session_id,
                    object_id
                );
            }
            readable_event.lock().unwrap().clear();
        }
    }

    fn signal_rendered_event(&self) {
        let rendered_event_initialized = self.rendered_event.lock().unwrap().is_initialized();
        // RUZU_PROFILE_AUDIO_EVENT=1: count fires + skips, log rate every 100 calls.
        // Measures whether `send_command_to_dsp` reaches this signal often enough
        // (target ~200 Hz for 5ms audio periods). 50× too low here would explain
        if std::env::var_os("RUZU_PROFILE_AUDIO_EVENT").is_some() {
            use std::sync::atomic::{AtomicU64, Ordering};
            use std::sync::OnceLock;
            use std::time::Instant;
            static FIRES: AtomicU64 = AtomicU64::new(0);
            static SKIPS: AtomicU64 = AtomicU64::new(0);
            static START: OnceLock<Instant> = OnceLock::new();
            let start = START.get_or_init(Instant::now);
            if rendered_event_initialized {
                let n = FIRES.fetch_add(1, Ordering::Relaxed) + 1;
                if n % 10 == 0 {
                    let elapsed = start.elapsed().as_secs_f64();
                    let skips = SKIPS.load(Ordering::Relaxed);
                    eprintln!(
                        "[AUDIO_EVENT] fires={} skips={} t={:.2}s rate_fire={:.1}Hz",
                        n,
                        skips,
                        elapsed,
                        n as f64 / elapsed
                    );
                }
            } else {
                let n = SKIPS.fetch_add(1, Ordering::Relaxed) + 1;
                if n % 50 == 0 {
                    eprintln!(
                        "[AUDIO_EVENT] skip-only count={} (rendered_event not initialized)",
                        n
                    );
                }
            }
        }
        if std::env::var_os("RUZU_TRACE_AUDIO_EVENT").is_some() {
            log::info!(
                "signal_rendered_event: initialized={} readable_event_present={} session_id={}",
                rendered_event_initialized,
                self.rendered_readable_event.is_some(),
                self.session_id,
            );
        }
        if !rendered_event_initialized {
            return;
        }

        if std::env::var_os("RUZU_TRACE_AUDIO_EVENT").is_some() {
            let readable_event_object_id = self
                .rendered_readable_event
                .as_ref()
                .map(|event| event.lock().unwrap().object_id);
            log::info!(
                "audio_core::System::signal_rendered_event session_id={} readable_event_object_id={:?}",
                self.session_id,
                readable_event_object_id
            );
        }

        // Signal the readable event directly using the stored Arc references.
        // Upstream parity: `KReadableEvent::Signal` walks waiters under the
        // scheduler lock; no KProcess Mutex is needed in the signal path.
        if let Some(ref readable_event) = self.rendered_readable_event {
            readable_event.lock().unwrap().signal_from_host();
            return;
        }

        // Fallback: signal through KEvent (also scheduler-lock-only).
        let process_ptr = self.process.as_ptr() as *const KProcess;
        if process_ptr.is_null() {
            return;
        }
        unsafe {
            let process = &*process_ptr;
            let _ = self.rendered_event.lock().unwrap().signal(process);
        }
    }

    fn get_minimum_command_workbuffer_size(
        behavior: &BehaviorInfo,
        params: &AudioRendererParameterInternal,
    ) -> u64 {
        if behavior.is_variadic_command_buffer_size_supported() {
            CommandGenerator::calculate_command_buffer_size(behavior, params)
                + ((0x40 - 1) * 2) as u64
        } else {
            0x18000 + ((0x40 - 1) * 2)
        }
    }

    fn get_pre_command_workbuffer_size(
        behavior: &BehaviorInfo,
        params: &AudioRendererParameterInternal,
    ) -> u64 {
        let voice_channels = MAX_CHANNELS as u64;
        let upsampler_count = (params.sinks + params.sub_mixes) as u64;
        let mut size = 0u64;
        size += align_audio(params.mixes as u64 * size_of::<i32>() as u64, 0x40);
        size += params.sub_mixes as u64 * MAX_EFFECTS as u64 * size_of::<i32>() as u64;
        size += (params.sub_mixes + 1) as u64 * size_of::<MixInfo>() as u64;
        size += params.voices as u64
            * (size_of::<VoiceInfo>() as u64
                + size_of::<VoiceChannelResource>() as u64
                + size_of::<VoiceState>() as u64);
        size += align_audio(
            (params.sub_mixes + 1) as u64 * size_of::<*const MixInfo>() as u64,
            0x10,
        );
        size += align_audio(
            params.voices as u64 * size_of::<*const VoiceInfo>() as u64,
            0x10,
        );
        size += align_audio(
            ((params.sinks + params.sub_mixes) as u64
                * TARGET_SAMPLE_COUNT as u64
                * size_of::<i32>() as u64
                + params.sample_count as u64 * size_of::<i32>() as u64)
                * (params.mixes as u64 + voice_channels),
            0x40,
        );
        if behavior.is_splitter_supported() {
            let node_size = NodeStates::get_work_buffer_size(params.sub_mixes + 1);
            let edge_size = EdgeMatrix::get_work_buffer_size(params.sub_mixes + 1);
            size += align_audio(node_size + edge_size, 0x10);
        }
        size += SplitterContext::calc_work_buffer_size(&behavior, params);
        size += (params.effects + params.voices * MAX_WAVE_BUFFERS) as u64
            * size_of::<MemoryPoolInfo>() as u64;
        if behavior.is_effect_info_version2_supported() {
            size += params.effects as u64 * size_of::<EffectResultState>() as u64;
        }
        size += 0x50;
        size = align_audio(size, 0x40);
        size += upsampler_count * size_of::<UpsamplerInfo>() as u64;
        size += params.effects as u64 * size_of::<EffectInfoBase>() as u64;
        size += align_audio(params.voices as u64 * size_of::<VoiceState>() as u64, 0x40);
        size += params.sinks as u64 * size_of::<SinkInfoBase>() as u64;
        if behavior.is_effect_info_version2_supported() {
            size += params.effects as u64 * size_of::<EffectResultState>() as u64;
        }
        if params.perf_frames > 0 {
            let perf_size =
                PerformanceManager::get_required_buffer_size_for_performance_metrics_per_frame(
                    behavior, params,
                );
            size += align_audio(perf_size * (params.perf_frames as u64 + 1) + 0xC0, 0x100);
        }
        align_audio(size, 0x40)
    }

    pub fn get_work_buffer_size(params: &AudioRendererParameterInternal) -> u64 {
        let mut behavior = BehaviorInfo::new();
        behavior.set_user_lib_revision(params.revision);
        let size = Self::get_pre_command_workbuffer_size(&behavior, params)
            + Self::get_minimum_command_workbuffer_size(&behavior, params);
        align_audio(size, 0x1000)
    }

    pub fn initialize(
        &mut self,
        params: &AudioRendererParameterInternal,
        transfer_memory: *mut KTransferMemory,
        transfer_memory_size: u64,
        process: *mut KProcess,
        applet_resource_user_id: u64,
        session_id: i32,
    ) -> Result {
        log::info!(
            "audio_core::renderer::System::initialize revision=0x{:08X} sample_rate={} sample_count={} mixes={} sub_mixes={} voices={} sinks={} effects={} perf_frames={} exec_mode={:?} splitter_infos={} splitter_destinations={} external_context_size=0x{:X}",
            params.revision,
            params.sample_rate,
            params.sample_count,
            params.mixes,
            params.sub_mixes,
            params.voices,
            params.sinks,
            params.effects,
            params.perf_frames,
            params.execution_mode,
            params.splitter_infos,
            params.splitter_destinations,
            params.external_context_size
        );
        if !check_valid_revision(params.revision) {
            return RESULT_INVALID_REVISION;
        }
        if Self::get_work_buffer_size(params) > transfer_memory_size {
            return RESULT_INSUFFICIENT_BUFFER;
        }
        if process.is_null() {
            return RESULT_INVALID_HANDLE;
        }
        if transfer_memory.is_null() {
            return RESULT_INVALID_HANDLE;
        }

        self.sample_rate = params.sample_rate;
        self.sample_count = params.sample_count;
        self.mix_buffer_count = params.mixes as i16;
        self.voice_channels = MAX_CHANNELS as i32;
        self.upsampler_count = params.sinks + params.sub_mixes;
        self.render_device = params.rendering_device as u32;
        self.execution_mode = params.execution_mode;
        self.applet_resource_user_id = applet_resource_user_id;
        self.session_id = session_id;
        self.set_process(process);
        let transfer_memory_source_address = unsafe { (*transfer_memory).get_source_address() };
        if let Some(memory) = self.core.get().get_svc_memory() {
            memory.access().unwrap().zero_block(
                transfer_memory_source_address,
                transfer_memory_size as usize,
            );
        }
        self.drop_voice_enabled =
            params.voice_drop_enabled != 0 && params.execution_mode == ExecutionMode::Auto;
        self.behavior.set_user_lib_revision(params.revision);
        self.voice_context
            .initialize(params.voices, params.voices, params.voices);
        self.mix_context
            .initialize(params.sub_mixes + 1, params.effects, &self.behavior);
        self.effect_context
            .initialize(params.effects, params.effects as usize);
        self.sink_context.initialize(
            vec![SinkInfoBase::default(); params.sinks as usize],
            params.sinks,
        );
        let _ = self.splitter_context.initialize(&self.behavior, params);
        self.memory_pool_workbuffer = vec![
            MemoryPoolInfo::new(PoolLocation::Dsp);
            (params.effects + params.voices * MAX_WAVE_BUFFERS)
                as usize
        ];
        self.samples_workbuffer = vec![
            0;
            (self.voice_channels.max(0) as usize + params.mixes as usize)
                * params.sample_count as usize
        ];
        let aligned_depop_samples = align_audio(params.mixes as u64, 0x40);
        self.depop_buffer = vec![0; aligned_depop_samples as usize];
        self.voice_state_pool = MemoryPoolInfo::new(PoolLocation::Dsp);
        self.effect_state_pool = MemoryPoolInfo::new(PoolLocation::Dsp);
        self.effect_result_state_pool = MemoryPoolInfo::new(PoolLocation::Dsp);
        self.samples_workbuffer_pool = MemoryPoolInfo::new(PoolLocation::Dsp);
        self.depop_buffer_pool = MemoryPoolInfo::new(PoolLocation::Dsp);
        self.upsampler_manager = UpsamplerManager::new(params.sinks + params.sub_mixes);
        if params.perf_frames > 0 {
            let perf_workbuffer_size =
                PerformanceManager::get_required_buffer_size_for_performance_metrics_per_frame(
                    &self.behavior,
                    params,
                ) * (params.perf_frames as u64 + 1)
                    + 0xC;
            let mut perf_workbuffer = vec![0u8; perf_workbuffer_size as usize];
            self.performance_manager.initialize(
                &mut perf_workbuffer,
                perf_workbuffer_size,
                params,
                &self.behavior,
            );
        } else {
            self.performance_manager = PerformanceManager::new();
        }
        let command_workbuffer_size = transfer_memory_size.saturating_sub(
            Self::get_pre_command_workbuffer_size(&self.behavior, params),
        );
        self.command_workbuffer = vec![0u8; command_workbuffer_size as usize];
        self.command_workbuffer_pool = MemoryPoolInfo::new(PoolLocation::Dsp);
        let pool_mapper = PoolMapper::new(None, false);
        let voice_state_bytes = typed_slice_as_bytes(self.voice_context.dsp_shared_states());
        let _ = pool_mapper.initialize_system_pool(
            &mut self.voice_state_pool,
            voice_state_bytes,
            size_of_val(voice_state_bytes) as u64,
        );
        let effect_state_bytes = typed_slice_as_bytes(self.effect_context.infos());
        let _ = pool_mapper.initialize_system_pool(
            &mut self.effect_state_pool,
            effect_state_bytes,
            size_of_val(effect_state_bytes) as u64,
        );
        let effect_result_state_bytes =
            typed_slice_as_bytes(self.effect_context.dsp_shared_result_states());
        let _ = pool_mapper.initialize_system_pool(
            &mut self.effect_result_state_pool,
            effect_result_state_bytes,
            size_of_val(effect_result_state_bytes) as u64,
        );
        let samples_workbuffer_bytes = typed_slice_as_bytes(&self.samples_workbuffer);
        let _ = pool_mapper.initialize_system_pool(
            &mut self.samples_workbuffer_pool,
            samples_workbuffer_bytes,
            size_of_val(samples_workbuffer_bytes) as u64,
        );
        let depop_buffer_bytes = typed_slice_as_bytes(&self.depop_buffer);
        let _ = pool_mapper.initialize_system_pool(
            &mut self.depop_buffer_pool,
            depop_buffer_bytes,
            size_of_val(depop_buffer_bytes) as u64,
        );
        let _ = pool_mapper.initialize_system_pool(
            &mut self.command_workbuffer_pool,
            &self.command_workbuffer,
            command_workbuffer_size,
        );
        self.frames_elapsed = 0;
        self.total_ticks_elapsed = 0;
        self.ticks_spent_updating = 0;
        self.num_command_lists_generated = 0;
        self.num_times_updated = 0;
        self.render_start_tick = 0;
        self.adsp_behind = false;
        self.num_voices_dropped = 0;
        self.rendering_time_limit_percent = 100;
        self.voice_drop_parameter = 1.0;
        self.command_buffer_size = 0;
        self.reset_command_buffers = true;
        self.active = false;
        self.clear_rendered_event();
        self.terminate_event.reset();
        self.initialized = true;
        ResultCode::SUCCESS
    }

    pub fn finalize(&mut self) {
        if !self.initialized {
            return;
        }

        if self.is_active() {
            self.stop();
        }
        self.applet_resource_user_id = 0;
        self.process = ProcessHandle::default();
        self.sample_rate = 0;
        self.sample_count = 0;
        self.mix_buffer_count = 0;
        self.voice_channels = 0;
        self.upsampler_count = 0;
        self.render_device = 0;
        self.execution_mode = ExecutionMode::Auto;
        self.session_id = -1;
        self.rendering_time_limit_percent = 100;
        self.drop_voice_enabled = false;
        self.voice_drop_parameter = 1.0;
        let memory_pool_count = self.memory_pool_workbuffer.len() as u32;
        PoolMapper::clear_use_state(&mut self.memory_pool_workbuffer, memory_pool_count);
        let pool_mapper = PoolMapper::new(None, false);
        for pool in &mut self.memory_pool_workbuffer {
            if pool.is_mapped() {
                let _ = pool_mapper.unmap_pool(pool);
            }
        }
        self.memory_pool_workbuffer.clear();
        self.command_workbuffer.clear();
        self.samples_workbuffer.clear();
        self.depop_buffer.clear();
        self.voice_state_pool.set_cpu_address(0, 0);
        self.voice_state_pool.set_dsp_address(0);
        self.effect_state_pool.set_cpu_address(0, 0);
        self.effect_state_pool.set_dsp_address(0);
        self.effect_result_state_pool.set_cpu_address(0, 0);
        self.effect_result_state_pool.set_dsp_address(0);
        self.samples_workbuffer_pool.set_cpu_address(0, 0);
        self.samples_workbuffer_pool.set_dsp_address(0);
        self.depop_buffer_pool.set_cpu_address(0, 0);
        self.depop_buffer_pool.set_dsp_address(0);
        self.command_workbuffer_pool.set_cpu_address(0, 0);
        self.command_workbuffer_pool.set_dsp_address(0);
        self.frames_elapsed = 0;
        self.total_ticks_elapsed = 0;
        self.ticks_spent_updating = 0;
        self.num_command_lists_generated = 0;
        self.num_times_updated = 0;
        self.render_start_tick = 0;
        self.adsp_behind = false;
        self.num_voices_dropped = 0;
        self.command_buffer_size = 0;
        self.reset_command_buffers = false;
        self.active = false;
        self.clear_rendered_event();
        self.terminate_event.reset();
        self.initialized = false;
    }

    pub fn start(&mut self) {
        log::info!(
            "audio_core::renderer::System::start session_id={} active_before={} exec_mode={:?}",
            self.session_id,
            self.active,
            self.execution_mode
        );
        self.frames_elapsed = 0;
        self.state = State::Started;
        self.active = true;
        self.terminate_event.reset();
    }

    pub fn stop(&mut self) {
        let terminate_event = self.request_stop();
        if let Some(terminate_event) = terminate_event {
            terminate_event.wait();
        }
    }

    /// Perform the state transition from upstream `System::Stop` while the
    /// Rust wrapper mutex is held, returning the event that must be waited on
    /// after releasing that mutex.
    pub(crate) fn request_stop(&mut self) -> Option<Arc<TerminateEvent>> {
        self.state = State::Stopped;
        self.active = false;
        if self.execution_mode == ExecutionMode::Auto {
            Some(Arc::clone(&self.terminate_event))
        } else {
            None
        }
    }

    pub fn update(&mut self, input: &[u8], performance: &mut [u8], output: &mut [u8]) -> Result {
        let start_time = self.current_time_ns();
        output.fill(0);
        let pool_count = self.memory_pool_workbuffer.len() as u32;
        let active = self.is_active();
        let splitter_supported = self.behavior.is_splitter_supported();
        let elapsed_frame_count_supported = self.behavior.is_elapsed_frame_count_supported();
        let mix_buffer_count = self.get_mix_buffer_count();
        let mut updater = crate::renderer::behavior::info_updater::InfoUpdater::new(
            input,
            output,
            Some(self.get_process() as usize),
            &mut self.behavior,
        );

        let mut result = updater.update_behavior_info();
        if result.is_error() {
            log::error!(
                "audio_core::renderer::System::update fail stage=behavior_info result=0x{:08X}",
                result.raw()
            );
            return result;
        }
        result = updater.update_memory_pools(&mut self.memory_pool_workbuffer, pool_count);
        if result.is_error() {
            log::error!(
                "audio_core::renderer::System::update fail stage=memory_pools result=0x{:08X}",
                result.raw()
            );
            return result;
        }
        result = updater.update_voice_channel_resources(&mut self.voice_context);
        if result.is_error() {
            log::error!(
                "audio_core::renderer::System::update fail stage=voice_channel_resources result=0x{:08X}",
                result.raw()
            );
            return result;
        }
        result = updater.update_voices(
            &mut self.voice_context,
            &mut self.memory_pool_workbuffer,
            pool_count,
        );
        if result.is_error() {
            log::error!(
                "audio_core::renderer::System::update fail stage=voices result=0x{:08X}",
                result.raw()
            );
            return result;
        }
        result = updater.update_effects(
            &mut self.effect_context,
            active,
            &mut self.memory_pool_workbuffer,
            pool_count,
        );
        if result.is_error() {
            log::error!(
                "audio_core::renderer::System::update fail stage=effects result=0x{:08X}",
                result.raw()
            );
            return result;
        }
        if splitter_supported {
            result = updater.update_splitter_info(&mut self.splitter_context);
            if result.is_error() {
                log::error!(
                    "audio_core::renderer::System::update fail stage=splitter_info result=0x{:08X}",
                    result.raw()
                );
                return result;
            }
        }
        result = updater.update_mixes(
            &mut self.mix_context,
            mix_buffer_count,
            &self.effect_context,
            &mut self.splitter_context,
        );
        if result.is_error() {
            log::error!(
                "audio_core::renderer::System::update fail stage=mixes result=0x{:08X}",
                result.raw()
            );
            return result;
        }
        result = updater.update_sinks(
            &mut self.sink_context,
            &mut self.memory_pool_workbuffer,
            pool_count,
            &mut self.upsampler_manager,
        );
        if result.is_error() {
            log::error!(
                "audio_core::renderer::System::update fail stage=sinks result=0x{:08X}",
                result.raw()
            );
            return result;
        }
        result = updater.update_performance_buffer(
            performance,
            self.performance_manager
                .is_initialized()
                .then_some(&mut self.performance_manager),
        );
        if result.is_error() {
            log::error!(
                "audio_core::renderer::System::update fail stage=performance result=0x{:08X}",
                result.raw()
            );
            return result;
        }
        result = updater.update_error_info();
        if result.is_error() {
            log::error!(
                "audio_core::renderer::System::update fail stage=error_info result=0x{:08X}",
                result.raw()
            );
            return result;
        }
        if elapsed_frame_count_supported {
            result = updater.update_renderer_info(self.frames_elapsed);
            if result.is_error() {
                log::error!(
                    "audio_core::renderer::System::update fail stage=renderer_info result=0x{:08X}",
                    result.raw()
                );
                return result;
            }
        }
        result = updater.check_consumed_size();
        if result.is_error() {
            log::error!(
                "audio_core::renderer::System::update fail stage=check_consumed_size result=0x{:08X}",
                result.raw()
            );
            return result;
        }
        drop(updater);

        self.clear_rendered_event();
        self.num_times_updated = self.num_times_updated.saturating_add(1);
        self.ticks_spent_updating = self
            .ticks_spent_updating
            .saturating_add(self.current_time_ns().saturating_sub(start_time));
        ResultCode::SUCCESS
    }

    pub fn get_rendering_time_limit(&self) -> u32 {
        self.rendering_time_limit_percent
    }

    pub fn set_rendering_time_limit(&mut self, limit: u32) {
        self.rendering_time_limit_percent = limit;
    }

    pub fn get_session_id(&self) -> u32 {
        self.session_id as u32
    }

    pub fn get_sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn get_sample_count(&self) -> u32 {
        self.sample_count
    }

    pub fn get_mix_buffer_count(&self) -> u32 {
        u32::try_from(self.mix_buffer_count).unwrap_or(0)
    }

    pub fn get_execution_mode(&self) -> ExecutionMode {
        self.execution_mode
    }

    pub fn get_rendering_device(&self) -> u32 {
        self.render_device
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn send_command_to_dsp(&mut self) {
        if !self.initialized {
            log::info!("audio_core::renderer::System::send_command_to_dsp skip uninitialized");
            return;
        }

        if !self.is_active() || self.session_id < 0 {
            if self.session_id >= 0 {
                self.audio_renderer
                    .lock()
                    .clear_remain_command_count(self.session_id);
            }
            self.terminate_event.signal();
            return;
        }

        self.terminate_event.reset();
        let remaining_command_count = self
            .audio_renderer
            .lock()
            .get_remain_command_count(self.session_id);
        log::trace!(
            "audio_core::renderer::System::send_command_to_dsp session_id={} remaining={} reset={}",
            self.session_id,
            remaining_command_count,
            self.reset_command_buffers
        );
        let mut command_size = self.command_buffer_size;
        if remaining_command_count > 0 {
            self.adsp_behind = true;
        }
        if remaining_command_count == 0 && !self.command_workbuffer.is_empty() {
            command_size = self.generate_command_workbuffer();
        }

        let time_limit_percent = if self
            .behavior
            .is_audio_renderer_processing_time_limit80_percent_supported()
        {
            80.0
        } else if self
            .behavior
            .is_audio_renderer_processing_time_limit75_percent_supported()
        {
            75.0
        } else {
            70.0
        };
        let time_limit = ((time_limit_percent / 100.0) * 2_880_000.0)
            * (self.rendering_time_limit_percent as f32 / 100.0);

        let translated_command_buffer = self
            .command_workbuffer_pool
            .translate(self.command_workbuffer.as_ptr() as usize, command_size)
            as usize;

        self.audio_renderer.lock().set_command_buffer(
            self.session_id,
            translated_command_buffer,
            command_size,
            time_limit.max(0.0) as u64,
            self.applet_resource_user_id,
            self.process.as_ptr(),
            self.reset_command_buffers,
        );
        self.command_buffer_size = command_size;
        self.reset_command_buffers = false;
        if remaining_command_count == 0 {
            self.signal_rendered_event();
        }
    }

    pub fn generate_command(&mut self, command_buffer: &mut [u8], command_buffer_size: u64) -> u64 {
        if !self.initialized {
            return 0;
        }

        let memory_pool_count = self.memory_pool_workbuffer.len() as u32;
        PoolMapper::clear_use_state(&mut self.memory_pool_workbuffer, memory_pool_count);
        let start_time = self.current_time_ns();
        if self.performance_manager.is_initialized() {
            self.performance_manager.tap_frame(
                self.adsp_behind,
                self.num_voices_dropped,
                self.render_start_tick,
            );
            self.adsp_behind = false;
            self.num_voices_dropped = 0;
            self.render_start_tick = 0;
        }

        let estimator = CommandProcessingTimeEstimator::new(
            &self.behavior,
            self.sample_count,
            self.get_mix_buffer_count(),
        );
        let mut generated = CommandBuffer::new(command_buffer_size as usize, estimator);
        let mut header = CommandListHeader {
            buffer_size: command_buffer_size,
            command_count: 0,
            samples_buffer: self.samples_workbuffer_pool.translate(
                self.samples_workbuffer.as_ptr() as usize,
                size_of_val(self.samples_workbuffer.as_slice()) as u64,
            ),
            buffer_count: self.mix_buffer_count + self.voice_channels as i16,
            sample_count: self.sample_count,
            sample_rate: self.sample_rate,
        };
        let _ = generated.generate_clear_mix_command(u32::MAX, self.get_mix_buffer_count());
        {
            let render_channels = if self.execution_mode == ExecutionMode::Auto {
                self.audio_renderer.lock().get_device_channels() as i8
            } else {
                2
            };
            let mut generator = CommandGenerator::new(
                &mut generated,
                &mut header,
                &self.behavior,
                &mut self.voice_context,
                &mut self.mix_context,
                &mut self.effect_context,
                &mut self.sink_context,
                &mut self.splitter_context,
                self.performance_manager
                    .is_initialized()
                    .then_some(&mut self.performance_manager),
                &self.voice_state_pool,
                &self.effect_state_pool,
                &self.effect_result_state_pool,
                &mut self.upsampler_manager,
                self.session_id,
                render_channels,
                &self.depop_buffer,
                &self.depop_buffer_pool,
                self.mix_buffer_count,
            );
            generator.generate_voice_commands();
        }

        let start_estimated_time =
            self.voice_drop_parameter * generated.estimated_process_time() as f32;

        {
            let render_channels = if self.execution_mode == ExecutionMode::Auto {
                self.audio_renderer.lock().get_device_channels() as i8
            } else {
                2
            };
            let mut generator = CommandGenerator::new(
                &mut generated,
                &mut header,
                &self.behavior,
                &mut self.voice_context,
                &mut self.mix_context,
                &mut self.effect_context,
                &mut self.sink_context,
                &mut self.splitter_context,
                self.performance_manager
                    .is_initialized()
                    .then_some(&mut self.performance_manager),
                &self.voice_state_pool,
                &self.effect_state_pool,
                &self.effect_result_state_pool,
                &mut self.upsampler_manager,
                self.session_id,
                render_channels,
                &self.depop_buffer,
                &self.depop_buffer_pool,
                self.mix_buffer_count,
            );
            generator.generate_submix_commands();
            generator.generate_final_mix_commands();
            generator.generate_sink_commands();
        }

        if self.drop_voice_enabled {
            let time_limit_percent = if self
                .behavior
                .is_audio_renderer_processing_time_limit80_percent_supported()
            {
                80.0
            } else if self
                .behavior
                .is_audio_renderer_processing_time_limit75_percent_supported()
            {
                75.0
            } else {
                70.0
            };

            let end_estimated_time =
                self.voice_drop_parameter * generated.estimated_process_time() as f32;
            let dsp_time_limit = ((time_limit_percent / 100.0) * 2_880_000.0)
                * (self.rendering_time_limit_percent as f32 / 100.0);
            let estimated_time = start_estimated_time - end_estimated_time;
            let time_limit = (dsp_time_limit + estimated_time).max(0.0) as u32;
            self.num_voices_dropped =
                self.drop_voices(&mut generated, start_estimated_time as u32, time_limit);
        } else {
            self.num_voices_dropped = 0;
        }

        header.buffer_size = generated.size() as u64;
        header.command_count = generated.count();
        self.voice_context.update_state_by_dsp_shared();
        if self.behavior.is_effect_info_version2_supported() {
            self.effect_context.update_state_by_dsp_shared();
        }
        self.total_ticks_elapsed = self
            .total_ticks_elapsed
            .saturating_add(self.current_time_ns().saturating_sub(start_time));
        self.num_command_lists_generated = self.num_command_lists_generated.saturating_add(1);
        if self.session_id >= 0 {
            self.render_start_tick = self
                .audio_renderer
                .lock()
                .get_rendering_start_tick(self.session_id);
        }
        self.frames_elapsed = self.frames_elapsed.saturating_add(1);
        let serialized_size = generated.serialize_into(&header, command_buffer) as u64;
        serialized_size
    }

    pub fn set_process(&mut self, process: *mut KProcess) {
        self.process = ProcessHandle::from_ptr(process as *mut ());
    }

    pub fn set_rendered_readable_event(&mut self, event: Arc<StdMutex<KReadableEvent>>) {
        self.rendered_readable_event = Some(event);
    }

    pub fn set_process_arc(&mut self, process: Arc<ProcessLock>) {
        self.process_arc = Some(process);
    }

    pub fn get_process(&self) -> *mut KProcess {
        self.process.as_ptr() as *mut KProcess
    }

    fn generate_command_workbuffer(&mut self) -> u64 {
        if self.command_workbuffer.is_empty() {
            return 0;
        }

        let mut workbuffer = std::mem::take(&mut self.command_workbuffer);
        let workbuffer_len = workbuffer.len() as u64;
        let size = self.generate_command(&mut workbuffer, workbuffer_len);
        self.command_workbuffer = workbuffer;
        size
    }

    fn drop_voices(
        &mut self,
        command_buffer: &mut CommandBuffer,
        mut estimated_process_time: u32,
        time_limit: u32,
    ) -> u32 {
        let entries = command_buffer.entries().to_vec();
        if entries.is_empty() {
            return 0;
        }

        let mut index = entries.iter().position(|entry| {
            entry.command.is_performance() || entry.command.is_voice_data_source()
        });
        let Some(mut i) = index.take() else {
            return 0;
        };

        let mut voices_dropped = 0;
        while i < entries.len() {
            let node_id = entries[i].node_id;
            let node_id_type = node_id >> 28;
            let node_id_base = (node_id >> 16) & 0x0FFF;

            if estimated_process_time <= time_limit || node_id_type != 1 {
                break;
            }

            let Some(voice_info) = self.voice_context.get_info_mut(node_id_base) else {
                break;
            };
            if voice_info.priority == crate::common::common::HIGHEST_VOICE_PRIORITY {
                break;
            }

            voices_dropped += 1;
            voice_info.voice_dropped = true;

            while i < entries.len() && entries[i].node_id == node_id {
                let entry = &entries[i];
                if entry.command.is_depop_prepare() {
                    if let Some(cmd) = command_buffer.entries_mut().get_mut(i) {
                        cmd.enabled = true;
                    }
                } else if entry.enabled && !entry.command.is_performance() {
                    if command_buffer.disable(i) {
                        let dropped =
                            self.voice_drop_parameter * entry.estimated_process_time as f32;
                        estimated_process_time =
                            estimated_process_time.saturating_sub(dropped as u32);
                    }
                }
                i += 1;
            }
        }

        voices_dropped
    }

    pub fn get_voice_drop_parameter(&self) -> f32 {
        self.voice_drop_parameter
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    pub fn get_rendered_event(&self) -> Arc<StdMutex<KEvent>> {
        self.rendered_event.clone()
    }

    #[cfg(test)]
    pub(crate) fn get_terminate_event(&self) -> Arc<TerminateEvent> {
        self.terminate_event.clone()
    }

    pub fn set_voice_drop_parameter(&mut self, voice_drop: f32) {
        self.voice_drop_parameter = voice_drop;
    }

    pub fn get_total_ticks_elapsed(&self) -> u64 {
        self.total_ticks_elapsed
    }

    pub fn get_ticks_spent_updating(&self) -> u64 {
        self.ticks_spent_updating
    }

    pub fn get_num_command_lists_generated(&self) -> u64 {
        self.num_command_lists_generated
    }

    pub fn get_num_times_updated(&self) -> u64 {
        self.num_times_updated
    }

    fn current_time_ns(&self) -> u64 {
        self.core
            .get()
            .core_timing()
            .get_global_time_ns()
            .as_nanos() as u64
    }
}

fn typed_slice_as_bytes<T>(slice: &[T]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(slice.as_ptr() as *const u8, size_of_val(slice)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adsp::apps::audio_renderer::AudioRenderer;
    use crate::common::feature_support::CURRENT_REVISION;
    use crate::renderer::behavior::behavior_info::{
        InParameter as BehaviorInParameter, OutStatus as BehaviorOutStatus,
    };
    use crate::renderer::behavior::info_updater::UpdateDataHeader;
    use crate::renderer::mix::mix_info::InParameter as MixInParameter;
    use crate::renderer::performance::performance_manager::{
        InParameter as PerfInParameter, OutStatus as PerfOutStatus,
    };
    use crate::renderer::{
        EdgeMatrix, EffectInfoBase, EffectResultState, MemoryPoolInfo, MixInfo, NodeStates,
        SinkInfoBase, UpsamplerInfo, VoiceChannelResource, VoiceInfo, VoiceState,
    };
    use crate::sink::null_sink::NullSink;
    use crate::sink::sink::new_sink_handle;
    use common::common_funcs::make_magic;
    use parking_lot::Mutex;
    use ruzu_core::hle::kernel::k_event::KEvent;
    use ruzu_core::hle::kernel::k_readable_event::KReadableEvent;
    use ruzu_core::hle::kernel::k_scheduler::KScheduler;
    use std::mem::size_of;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::sync::Mutex as StdMutex;

    fn make_shared_system() -> SharedSystem {
        crate::make_test_system()
    }

    fn make_renderer_handle(core: SharedSystem) -> AudioRendererHandle {
        Arc::new(Mutex::new(AudioRenderer::new(
            core,
            new_sink_handle(Box::new(NullSink::new("test"))),
        )))
    }

    fn make_kernel_rendered_event() -> (
        Arc<StdMutex<KEvent>>,
        Box<KProcess>,
        Arc<StdMutex<KScheduler>>,
        u64,
    ) {
        let event_object_id = 1;
        let readable_event_object_id = 2;
        let scheduler = Arc::new(StdMutex::new(KScheduler::new(0)));
        let mut process = Box::new(KProcess::new());
        process.attach_scheduler(&scheduler);

        let event = Arc::new(StdMutex::new(KEvent::new()));
        let readable = Arc::new(StdMutex::new(KReadableEvent::new()));
        event
            .lock()
            .unwrap()
            .initialize(process.get_process_id(), readable_event_object_id);
        readable
            .lock()
            .unwrap()
            .initialize(event_object_id, readable_event_object_id);
        process.register_event_object(event_object_id, Arc::clone(&event));
        process.register_readable_event_object(readable_event_object_id, readable);

        (event, process, scheduler, readable_event_object_id)
    }

    fn make_params() -> AudioRendererParameterInternal {
        AudioRendererParameterInternal {
            sample_rate: 48_000,
            sample_count: 160,
            mixes: 1,
            sub_mixes: 0,
            voices: 0,
            sinks: 0,
            effects: 0,
            perf_frames: 0,
            voice_drop_enabled: 0,
            unk_21: 0,
            rendering_device: 0,
            execution_mode: ExecutionMode::Manual,
            splitter_infos: 0,
            splitter_destinations: 0,
            external_context_size: 0,
            revision: CURRENT_REVISION,
        }
    }

    fn initialize_system_for_test(
        system: &mut System,
        params: &AudioRendererParameterInternal,
        transfer_memory_size: u64,
        applet_resource_user_id: u64,
        session_id: i32,
    ) -> (ResultCode, Box<KTransferMemory>, Box<KProcess>) {
        let mut transfer_memory = Box::new(KTransferMemory::new());
        let mut process = Box::new(KProcess::new());
        let result = system.initialize(
            params,
            &mut *transfer_memory,
            transfer_memory_size,
            &mut *process,
            applet_resource_user_id,
            session_id,
        );
        (result, transfer_memory, process)
    }

    fn push_pod_bytes<T: Copy>(out: &mut Vec<u8>, value: &T) {
        let bytes =
            unsafe { std::slice::from_raw_parts(value as *const T as *const u8, size_of::<T>()) };
        out.extend_from_slice(bytes);
    }

    fn read_header_buffer_count(input: &[u8]) -> Option<i16> {
        let bytes = input.get(20..22)?;
        Some(i16::from_le_bytes(bytes.try_into().ok()?))
    }

    fn expected_work_buffer_size(params: &AudioRendererParameterInternal) -> u64 {
        let mut behavior = BehaviorInfo::new();
        behavior.set_user_lib_revision(params.revision);
        let mut size = 0u64;
        size += align_audio(params.mixes as u64 * size_of::<i32>() as u64, 0x40);
        size += params.sub_mixes as u64 * MAX_EFFECTS as u64 * size_of::<i32>() as u64;
        size += (params.sub_mixes + 1) as u64 * size_of::<MixInfo>() as u64;
        size += params.voices as u64
            * (size_of::<VoiceInfo>() as u64
                + size_of::<VoiceChannelResource>() as u64
                + size_of::<VoiceState>() as u64);
        size += align_audio(
            (params.sub_mixes + 1) as u64 * size_of::<*const MixInfo>() as u64,
            0x10,
        );
        size += align_audio(
            params.voices as u64 * size_of::<*const VoiceInfo>() as u64,
            0x10,
        );
        size += align_audio(
            ((params.sinks + params.sub_mixes) as u64
                * TARGET_SAMPLE_COUNT as u64
                * size_of::<i32>() as u64
                + params.sample_count as u64 * size_of::<i32>() as u64)
                * (params.mixes as u64 + MAX_CHANNELS as u64),
            0x40,
        );
        if behavior.is_splitter_supported() {
            let node_size = NodeStates::get_work_buffer_size(params.sub_mixes + 1);
            let edge_size = EdgeMatrix::get_work_buffer_size(params.sub_mixes + 1);
            size += align_audio(node_size + edge_size, 0x10);
        }
        size += SplitterContext::calc_work_buffer_size(&behavior, params);
        size += (params.effects + params.voices * MAX_WAVE_BUFFERS) as u64
            * size_of::<MemoryPoolInfo>() as u64;
        if behavior.is_effect_info_version2_supported() {
            size += params.effects as u64 * size_of::<EffectResultState>() as u64;
        }
        size += 0x50;
        size = align_audio(size, 0x40);
        size += (params.sinks + params.sub_mixes) as u64 * size_of::<UpsamplerInfo>() as u64;
        size += params.effects as u64 * size_of::<EffectInfoBase>() as u64;
        size += align_audio(params.voices as u64 * size_of::<VoiceState>() as u64, 0x40);
        size += params.sinks as u64 * size_of::<SinkInfoBase>() as u64;
        if behavior.is_effect_info_version2_supported() {
            size += params.effects as u64 * size_of::<EffectResultState>() as u64;
        }
        if params.perf_frames > 0 {
            let perf_size =
                PerformanceManager::get_required_buffer_size_for_performance_metrics_per_frame(
                    &behavior, params,
                );
            size += align_audio(perf_size * (params.perf_frames as u64 + 1) + 0xC0, 0x100);
        }
        size = align_audio(size, 0x40);
        size += System::get_minimum_command_workbuffer_size(&behavior, params);
        align_audio(size, 0x1000)
    }

    fn expected_pre_command_workbuffer_size(params: &AudioRendererParameterInternal) -> u64 {
        let mut behavior = BehaviorInfo::new();
        behavior.set_user_lib_revision(params.revision);
        let mut size = 0u64;
        size += align_audio(params.mixes as u64 * size_of::<i32>() as u64, 0x40);
        size += params.sub_mixes as u64 * MAX_EFFECTS as u64 * size_of::<i32>() as u64;
        size += (params.sub_mixes + 1) as u64 * size_of::<MixInfo>() as u64;
        size += params.voices as u64
            * (size_of::<VoiceInfo>() as u64
                + size_of::<VoiceChannelResource>() as u64
                + size_of::<VoiceState>() as u64);
        size += align_audio(
            (params.sub_mixes + 1) as u64 * size_of::<*const MixInfo>() as u64,
            0x10,
        );
        size += align_audio(
            params.voices as u64 * size_of::<*const VoiceInfo>() as u64,
            0x10,
        );
        size += align_audio(
            ((params.sinks + params.sub_mixes) as u64
                * TARGET_SAMPLE_COUNT as u64
                * size_of::<i32>() as u64
                + params.sample_count as u64 * size_of::<i32>() as u64)
                * (params.mixes as u64 + MAX_CHANNELS as u64),
            0x40,
        );
        if behavior.is_splitter_supported() {
            let node_size = NodeStates::get_work_buffer_size(params.sub_mixes + 1);
            let edge_size = EdgeMatrix::get_work_buffer_size(params.sub_mixes + 1);
            size += align_audio(node_size + edge_size, 0x10);
        }
        size += SplitterContext::calc_work_buffer_size(&behavior, params);
        size += (params.effects + params.voices * MAX_WAVE_BUFFERS) as u64
            * size_of::<MemoryPoolInfo>() as u64;
        if behavior.is_effect_info_version2_supported() {
            size += params.effects as u64 * size_of::<EffectResultState>() as u64;
        }
        size += 0x50;
        size = align_audio(size, 0x40);
        size += (params.sinks + params.sub_mixes) as u64 * size_of::<UpsamplerInfo>() as u64;
        size += params.effects as u64 * size_of::<EffectInfoBase>() as u64;
        size += align_audio(params.voices as u64 * size_of::<VoiceState>() as u64, 0x40);
        size += params.sinks as u64 * size_of::<SinkInfoBase>() as u64;
        if behavior.is_effect_info_version2_supported() {
            size += params.effects as u64 * size_of::<EffectResultState>() as u64;
        }
        if params.perf_frames > 0 {
            let perf_size =
                PerformanceManager::get_required_buffer_size_for_performance_metrics_per_frame(
                    &behavior, params,
                );
            size += align_audio(perf_size * (params.perf_frames as u64 + 1) + 0xC0, 0x100);
        }
        align_audio(size, 0x40)
    }

    #[test]
    fn start_resets_elapsed_frames() {
        let core = make_shared_system();
        let audio_renderer = make_renderer_handle(core.clone());
        let mut system = System::new_for_tests(core, audio_renderer);
        let params = make_params();
        let transfer_size = System::get_work_buffer_size(&params);

        let (result, _transfer_memory, _process) =
            initialize_system_for_test(&mut system, &params, transfer_size, 1, 0);
        assert_eq!(result, ResultCode::SUCCESS);

        system.frames_elapsed = 7;
        system.start();

        assert_eq!(system.frames_elapsed, 0);
        assert!(system.is_active());
    }

    #[test]
    fn initialize_sets_user_rendering_time_limit_to_one_hundred_percent() {
        let core = make_shared_system();
        let audio_renderer = make_renderer_handle(core.clone());
        let mut system = System::new_for_tests(core, audio_renderer);
        let mut params = make_params();
        params.revision = 1;
        let transfer_size = System::get_work_buffer_size(&params);

        let (result, _transfer_memory, _process) =
            initialize_system_for_test(&mut system, &params, transfer_size, 1, 0);
        assert_eq!(result, ResultCode::SUCCESS);

        assert_eq!(system.get_rendering_time_limit(), 100);
    }

    #[test]
    fn generate_command_increments_elapsed_frames_and_clears_pool_use_state() {
        let core = make_shared_system();
        let audio_renderer = make_renderer_handle(core.clone());
        let mut system = System::new_for_tests(core, audio_renderer);
        let params = make_params();
        let transfer_size = System::get_work_buffer_size(&params);

        let (result, _transfer_memory, _process) =
            initialize_system_for_test(&mut system, &params, transfer_size, 1, 0);
        assert_eq!(result, ResultCode::SUCCESS);
        system.start();
        system.memory_pool_workbuffer.push({
            let mut pool = MemoryPoolInfo::new(PoolLocation::Dsp);
            pool.set_used(true);
            pool
        });

        let mut command_buffer = vec![0u8; 0x2000];
        let command_buffer_len = command_buffer.len() as u64;
        let written = system.generate_command(&mut command_buffer, command_buffer_len);

        assert!(written > 0);
        assert_eq!(system.frames_elapsed, 1);
        assert!(!system.memory_pool_workbuffer[0].is_used());
    }

    #[test]
    fn update_increments_update_counters_on_success() {
        let core = make_shared_system();
        let audio_renderer = make_renderer_handle(core.clone());
        let mut system = System::new_for_tests(core, audio_renderer);
        let mut params = make_params();
        params.revision = make_magic(b'R', b'E', b'V', b'1');
        let transfer_size = System::get_work_buffer_size(&params);

        let (result, _transfer_memory, _process) =
            initialize_system_for_test(&mut system, &params, transfer_size, 1, 0);
        assert_eq!(result, ResultCode::SUCCESS);

        let mut input = Vec::new();
        let header = UpdateDataHeader {
            revision: params.revision,
            behaviour_size: size_of::<BehaviorInParameter>() as u32,
            mix_size: size_of::<MixInParameter>() as u32,
            performance_buffer_size: size_of::<PerfInParameter>() as u32,
            size: (size_of::<UpdateDataHeader>()
                + size_of::<BehaviorInParameter>()
                + size_of::<MixInParameter>()
                + size_of::<PerfInParameter>()) as u32,
            ..Default::default()
        };
        let behavior = BehaviorInParameter {
            revision: params.revision,
            ..Default::default()
        };
        let mix = MixInParameter::default();
        let perf = PerfInParameter::default();
        push_pod_bytes(&mut input, &header);
        push_pod_bytes(&mut input, &behavior);
        push_pod_bytes(&mut input, &mix);
        push_pod_bytes(&mut input, &perf);

        let mut performance = Vec::new();
        let mut output = vec![
            0u8;
            size_of::<UpdateDataHeader>()
                + size_of::<PerfOutStatus>()
                + size_of::<BehaviorOutStatus>()
        ];

        assert_eq!(
            system.update(&input, &mut performance, &mut output),
            ResultCode::SUCCESS
        );

        assert_eq!(system.get_num_times_updated(), 1);
        assert_eq!(system.get_num_command_lists_generated(), 0);
        assert_eq!(system.frames_elapsed, 0);
    }

    #[test]
    fn finalize_clears_initialized_runtime_state() {
        let core = make_shared_system();
        let audio_renderer = make_renderer_handle(core.clone());
        let mut system = System::new_for_tests(core, audio_renderer);
        let mut params = make_params();
        params.voices = 1;
        params.effects = 1;
        let transfer_size = System::get_work_buffer_size(&params);

        let (result, _transfer_memory, _process) =
            initialize_system_for_test(&mut system, &params, transfer_size, 77, 9);
        assert_eq!(result, ResultCode::SUCCESS);

        system.start();
        system.memory_pool_workbuffer[0].set_cpu_address(0x1000, 0x1000);
        system.memory_pool_workbuffer[0].set_dsp_address(0x1000);
        system.memory_pool_workbuffer[0].set_used(true);

        system.finalize();

        assert!(!system.is_initialized());
        assert_eq!(system.applet_resource_user_id, 0);
        assert_eq!(system.session_id, -1);
        assert_eq!(system.sample_rate, 0);
        assert_eq!(system.sample_count, 0);
        assert_eq!(system.mix_buffer_count, 0);
        assert!(system.memory_pool_workbuffer.is_empty());
        assert!(system.command_workbuffer.is_empty());
        assert!(system.samples_workbuffer.is_empty());
        assert!(system.depop_buffer.is_empty());
        assert!(!system.voice_state_pool.is_mapped());
        assert!(!system.effect_state_pool.is_mapped());
        assert!(!system.effect_result_state_pool.is_mapped());
        assert!(!system.samples_workbuffer_pool.is_mapped());
        assert!(!system.depop_buffer_pool.is_mapped());
        assert!(!system.command_workbuffer_pool.is_mapped());
    }

    #[test]
    fn finalize_does_not_wait_when_auto_mode_was_never_started() {
        let core = make_shared_system();
        let audio_renderer = make_renderer_handle(core.clone());
        let mut system = System::new_for_tests(core, audio_renderer);
        let mut params = make_params();
        params.execution_mode = ExecutionMode::Auto;
        let transfer_size = System::get_work_buffer_size(&params);

        let (result, _transfer_memory, _process) =
            initialize_system_for_test(&mut system, &params, transfer_size, 77, 9);
        assert_eq!(result, ResultCode::SUCCESS);

        system.finalize();

        assert!(!system.is_initialized());
    }

    #[test]
    fn initialize_aligns_depop_buffer_to_workbuffer_formula() {
        let core = make_shared_system();
        let audio_renderer = make_renderer_handle(core.clone());
        let mut system = System::new_for_tests(core, audio_renderer);
        let mut params = make_params();
        params.mixes = 3;
        let transfer_size = System::get_work_buffer_size(&params);

        let (result, _transfer_memory, _process) =
            initialize_system_for_test(&mut system, &params, transfer_size, 1, 0);
        assert_eq!(result, ResultCode::SUCCESS);

        assert_eq!(system.voice_channels, MAX_CHANNELS as i32);
        assert_eq!(system.upsampler_count, params.sinks + params.sub_mixes);
        assert_eq!(
            system.samples_workbuffer.len(),
            (params.mixes as usize + MAX_CHANNELS) * params.sample_count as usize
        );
        let expected_depop_bytes = align_audio(params.mixes as u64, 0x40) * size_of::<i32>() as u64;
        assert_eq!(
            system.depop_buffer.len() as u64 * size_of::<i32>() as u64,
            expected_depop_bytes
        );
        assert_eq!(system.depop_buffer_pool.get_size(), expected_depop_bytes);
    }

    #[test]
    fn generate_command_returns_zero_when_not_initialized() {
        let core = make_shared_system();
        let audio_renderer = make_renderer_handle(core.clone());
        let mut system = System::new_for_tests(core, audio_renderer);
        let mut command_buffer = vec![0u8; 0x2000];
        let command_buffer_len = command_buffer.len() as u64;

        assert_eq!(
            system.generate_command(&mut command_buffer, command_buffer_len),
            0
        );
    }

    #[test]
    fn get_session_id_preserves_negative_bits() {
        let core = make_shared_system();
        let audio_renderer = make_renderer_handle(core.clone());
        let system = System::new_for_tests(core, audio_renderer);

        assert_eq!(system.get_session_id(), u32::MAX);
    }

    #[test]
    fn get_mix_buffer_count_does_not_alias_negative_values() {
        let core = make_shared_system();
        let audio_renderer = make_renderer_handle(core.clone());
        let mut system = System::new_for_tests(core, audio_renderer);
        system.mix_buffer_count = -1;

        assert_eq!(system.get_mix_buffer_count(), 0);
    }

    #[test]
    fn initialize_gives_command_workbuffer_all_remaining_transfer_memory() {
        let core = make_shared_system();
        let audio_renderer = make_renderer_handle(core.clone());
        let mut system = System::new_for_tests(core, audio_renderer);
        let mut params = make_params();
        params.voices = 2;
        params.sub_mixes = 1;
        params.sinks = 1;
        params.effects = 1;
        params.perf_frames = 1;
        let minimum_transfer_size = System::get_work_buffer_size(&params);
        let extra_transfer_size = 0x2000;
        let transfer_size = minimum_transfer_size + extra_transfer_size;

        let (result, _transfer_memory, _process) =
            initialize_system_for_test(&mut system, &params, transfer_size, 1, 0);
        assert_eq!(result, ResultCode::SUCCESS);

        let expected_command_workbuffer_size =
            transfer_size - expected_pre_command_workbuffer_size(&params);
        assert_eq!(
            system.command_workbuffer.len() as u64,
            expected_command_workbuffer_size
        );
        assert_eq!(
            system.command_workbuffer_pool.get_size(),
            expected_command_workbuffer_size
        );
    }

    #[test]
    fn update_clears_rendered_event_and_send_command_to_dsp_signals_it() {
        let core = make_shared_system();
        let audio_renderer = make_renderer_handle(core.clone());
        let (rendered_event, mut process, _scheduler, readable_event_object_id) =
            make_kernel_rendered_event();
        let mut system = System::new(core, audio_renderer, rendered_event);
        let readable_event = process
            .get_readable_event_by_object_id(readable_event_object_id)
            .unwrap();
        system.set_rendered_readable_event(readable_event.clone());
        let mut params = make_params();
        params.revision = make_magic(b'R', b'E', b'V', b'1');
        let transfer_size = System::get_work_buffer_size(&params);
        let mut transfer_memory = Box::new(KTransferMemory::new());

        assert_eq!(
            system.initialize(
                &params,
                &mut *transfer_memory,
                transfer_size,
                &mut *process,
                1,
                0,
            ),
            ResultCode::SUCCESS
        );

        readable_event.lock().unwrap().signal();

        let mut input = Vec::new();
        let header = UpdateDataHeader {
            revision: params.revision,
            behaviour_size: size_of::<BehaviorInParameter>() as u32,
            mix_size: size_of::<MixInParameter>() as u32,
            performance_buffer_size: size_of::<PerfInParameter>() as u32,
            size: (size_of::<UpdateDataHeader>()
                + size_of::<BehaviorInParameter>()
                + size_of::<MixInParameter>()
                + size_of::<PerfInParameter>()) as u32,
            ..Default::default()
        };
        let behavior = BehaviorInParameter {
            revision: params.revision,
            ..Default::default()
        };
        let mix = MixInParameter::default();
        let perf = PerfInParameter::default();
        push_pod_bytes(&mut input, &header);
        push_pod_bytes(&mut input, &behavior);
        push_pod_bytes(&mut input, &mix);
        push_pod_bytes(&mut input, &perf);

        let mut performance = Vec::new();
        let mut output = vec![
            0u8;
            size_of::<UpdateDataHeader>()
                + size_of::<PerfOutStatus>()
                + size_of::<BehaviorOutStatus>()
        ];

        assert_eq!(
            system.update(&input, &mut performance, &mut output),
            ResultCode::SUCCESS
        );
        assert!(!readable_event.lock().unwrap().is_signaled());

        system.start();
        system.send_command_to_dsp();

        assert!(readable_event.lock().unwrap().is_signaled());
    }

    #[test]
    fn generate_command_uses_voice_channels_plus_mixes_for_buffer_count() {
        let core = make_shared_system();
        let audio_renderer = make_renderer_handle(core.clone());
        let mut system = System::new_for_tests(core, audio_renderer);
        let params = make_params();
        let transfer_size = System::get_work_buffer_size(&params);

        let (result, _transfer_memory, _process) =
            initialize_system_for_test(&mut system, &params, transfer_size, 1, 0);
        assert_eq!(result, ResultCode::SUCCESS);
        system.start();

        let mut command_buffer = vec![0u8; 0x4000];
        let command_buffer_len = command_buffer.len() as u64;
        let written = system.generate_command(&mut command_buffer, command_buffer_len);

        assert!(written > 0);
        let buffer_count = read_header_buffer_count(&command_buffer).unwrap();
        assert_eq!(
            buffer_count,
            system.voice_channels as i16 + system.mix_buffer_count
        );
    }

    #[test]
    fn inactive_send_command_to_dsp_signals_terminate_event() {
        let core = make_shared_system();
        let audio_renderer = make_renderer_handle(core.clone());
        let mut system = System::new_for_tests(core, audio_renderer);
        let mut params = make_params();
        params.execution_mode = ExecutionMode::Auto;
        let transfer_size = System::get_work_buffer_size(&params);

        let (result, _transfer_memory, _process) =
            initialize_system_for_test(&mut system, &params, transfer_size, 1, 0);
        assert_eq!(result, ResultCode::SUCCESS);

        system.start();
        let terminate_event = system.get_terminate_event();
        assert!(!terminate_event.is_signaled());

        system.state = State::Stopped;
        system.active = false;
        system.send_command_to_dsp();

        assert!(terminate_event.is_signaled());
    }

    #[test]
    fn send_command_to_dsp_forwards_process_pointer() {
        let core = make_shared_system();
        let audio_renderer = make_renderer_handle(core.clone());
        let mut system = System::new_for_tests(core, audio_renderer.clone());
        let params = make_params();
        let transfer_size = System::get_work_buffer_size(&params);

        let (result, _transfer_memory, _initial_process) =
            initialize_system_for_test(&mut system, &params, transfer_size, 1, 0);
        assert_eq!(result, ResultCode::SUCCESS);

        let process = Box::into_raw(Box::new(ruzu_core::hle::kernel::k_process::KProcess::new()));
        system.set_process(process);
        system.start();
        system.send_command_to_dsp();

        assert_eq!(
            audio_renderer.lock().get_command_buffer_process(0),
            process as *mut ()
        );

        unsafe {
            drop(Box::from_raw(process));
        }
    }

    #[test]
    fn stop_waits_for_terminate_event_in_auto_mode() {
        use std::time::Duration;

        let core = make_shared_system();
        let audio_renderer = make_renderer_handle(core.clone());
        let mut system = System::new_for_tests(core, audio_renderer);
        let mut params = make_params();
        params.execution_mode = ExecutionMode::Auto;
        let transfer_size = System::get_work_buffer_size(&params);

        let (result, _transfer_memory, _process) =
            initialize_system_for_test(&mut system, &params, transfer_size, 1, 0);
        assert_eq!(result, ResultCode::SUCCESS);

        system.start();

        let terminate_event = system.get_terminate_event();
        let stop_finished = Arc::new(AtomicBool::new(false));
        let stop_finished_thread = stop_finished.clone();

        let stop_thread = std::thread::spawn(move || {
            system.stop();
            stop_finished_thread.store(true, Ordering::SeqCst);
        });

        std::thread::sleep(Duration::from_millis(20));
        assert!(!stop_finished.load(Ordering::SeqCst));

        terminate_event.signal();
        stop_thread.join().unwrap();

        assert!(stop_finished.load(Ordering::SeqCst));
    }

    #[test]
    fn work_buffer_size_matches_zuyu_formula_for_revision_one() {
        let params = AudioRendererParameterInternal {
            sample_rate: 48_000,
            sample_count: 160,
            mixes: 3,
            sub_mixes: 2,
            voices: 4,
            sinks: 2,
            effects: 3,
            perf_frames: 0,
            voice_drop_enabled: 0,
            unk_21: 0,
            rendering_device: 0,
            execution_mode: ExecutionMode::Manual,
            splitter_infos: 0,
            splitter_destinations: 0,
            external_context_size: 0,
            revision: 1,
        };

        assert_eq!(
            System::get_work_buffer_size(&params),
            expected_work_buffer_size(&params)
        );
    }

    #[test]
    fn work_buffer_size_matches_zuyu_formula_with_all_optional_regions() {
        let params = AudioRendererParameterInternal {
            sample_rate: 48_000,
            sample_count: 240,
            mixes: 4,
            sub_mixes: 3,
            voices: 5,
            sinks: 2,
            effects: 4,
            perf_frames: 2,
            voice_drop_enabled: 1,
            unk_21: 0,
            rendering_device: 0,
            execution_mode: ExecutionMode::Auto,
            splitter_infos: 2,
            splitter_destinations: 4,
            external_context_size: 0,
            revision: CURRENT_REVISION,
        };

        assert_eq!(
            System::get_work_buffer_size(&params),
            expected_work_buffer_size(&params)
        );
    }
}
