// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Metal command-buffer submission and completion ordering.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSCopying;
use objc2_metal::{
    MTLBlitCommandEncoder, MTLBlitPassDescriptor, MTLCommandBuffer, MTLCommandBufferStatus,
    MTLCommandEncoder, MTLCommandQueue, MTLComputeCommandEncoder, MTLComputePassDescriptor,
    MTLRenderCommandEncoder, MTLRenderPassDescriptor, MTLSamplerState,
};
use parking_lot::Mutex;
use thiserror::Error;

use super::metal_device::MetalDevice;
use super::metal_command_journal::{CommandJournal, CommandWorkload};
use super::metal_framebuffer::MetalRenderPassKey;
use super::metal_gpu_profiler::{ComputeWork, MetalGpuProfiler, StageSamples};

#[derive(Debug, Error)]
pub enum MetalSchedulerError {
    #[error("Metal command queue failed to allocate a command buffer")]
    NoCommandBuffer,
    #[error("Metal command buffer failed to allocate a blit encoder")]
    NoBlitEncoder,
    #[error("Metal command buffer failed to allocate a compute encoder")]
    NoComputeEncoder,
    #[error("Metal command buffer failed to allocate a render encoder")]
    NoRenderEncoder,
    #[error("no Metal render encoder is active")]
    NoActiveRenderEncoder,
    #[error("Metal command buffer completed with status {0:?}: {1}")]
    CommandBufferFailed(MTLCommandBufferStatus, String),
}

/// Serial Metal queue owner.
///
/// Metal command buffers submitted to one queue execute in commit order. The
/// backend therefore records guest uploads, render/compute passes and copies
/// in their original Maxwell order without translating Vulkan barriers or
/// layouts into Metal concepts.
pub struct MetalScheduler {
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    active: Option<Retained<ProtocolObject<dyn MTLCommandBuffer>>>,
    active_upload: Option<(Retained<ProtocolObject<dyn MTLCommandBuffer>>, Retained<ProtocolObject<dyn MTLBlitCommandEncoder>>)>,
    active_encoder: Option<ActiveEncoder>,
    active_render_pass_key: Option<MetalRenderPassKey>,
    active_sampler_states: Option<Arc<Mutex<Vec<Retained<ProtocolObject<dyn MTLSamplerState>>>>>>,
    in_flight: VecDeque<InFlightCommandBuffer>,
    next_tick: u64,
    known_gpu_tick: u64,
    submission_profiler: Option<SubmissionProfiler>,
    stage_profiler: Option<MetalGpuProfiler>,
    active_stage_samples: Option<StageSamples>,
    non_render_serial: u64,
    command_journal: Option<Arc<CommandJournal>>,
    active_workload: Option<CommandWorkload>,
}

enum ActiveEncoder {
    Blit(Retained<ProtocolObject<dyn MTLBlitCommandEncoder>>),
    Compute(Retained<ProtocolObject<dyn MTLComputeCommandEncoder>>),
    Render(Retained<ProtocolObject<dyn MTLRenderCommandEncoder>>),
}

impl ActiveEncoder {
    fn end_encoding(self) {
        match self {
            Self::Blit(encoder) => encoder.endEncoding(),
            Self::Compute(encoder) => encoder.endEncoding(),
            Self::Render(encoder) => encoder.endEncoding(),
        }
    }
}

struct InFlightCommandBuffer {
    tick: u64,
    upload_prefix: Option<Retained<ProtocolObject<dyn MTLCommandBuffer>>>,
    command_buffer: Retained<ProtocolObject<dyn MTLCommandBuffer>>,
    kind: SubmissionKind,
    stage_samples: Option<StageSamples>,
}

#[derive(Clone, Copy, Debug)]
enum SubmissionKind {
    Guest,
    Presentation,
    External,
    Synchronous,
    UploadPrefix,
}

#[derive(Clone, Copy, Debug, Default)]
struct SubmissionTiming {
    completed: u64,
    measured: u64,
    gpu_ms: f64,
    peak_ms: f64,
    peak_tick: u64,
}

impl SubmissionTiming {
    fn observe(&mut self, tick: u64, start: f64, end: f64) {
        self.completed += 1;
        // Metal reports zero until its GPU completion timestamps are available.
        if !start.is_finite() || !end.is_finite() || start <= 0.0 || end < start {
            return;
        }
        let elapsed_ms = (end - start) * 1_000.0;
        self.measured += 1;
        self.gpu_ms += elapsed_ms;
        if elapsed_ms > self.peak_ms {
            self.peak_ms = elapsed_ms;
            self.peak_tick = tick;
        }
    }
}

struct SubmissionProfiler {
    interval_start: Instant,
    timings: [SubmissionTiming; 5],
    total_completed: u64,
}

impl SubmissionProfiler {
    fn new() -> Self {
        Self {
            interval_start: Instant::now(),
            timings: [SubmissionTiming::default(); 5],
            total_completed: 0,
        }
    }

    fn observe(&mut self, kind: SubmissionKind, tick: u64, start: f64, end: f64) {
        self.timings[kind as usize].observe(tick, start, end);
        self.total_completed += 1;
        let elapsed = self.interval_start.elapsed();
        if elapsed.as_secs() >= 1 {
            // Intervals describe completions observed by the CPU, not GPU busy
            // percentages; command-buffer execution spans may overlap.
            log::info!(
                "[METAL_GPU_TIME] observation_s={:.3} total_completed={} guest={:?} presentation={:?} external={:?} synchronous={:?} upload_prefix={:?}",
                elapsed.as_secs_f64(), self.total_completed, self.timings[0], self.timings[1],
                self.timings[2], self.timings[3], self.timings[4],
            );
            self.timings = [SubmissionTiming::default(); 5];
            self.interval_start = Instant::now();
        }
    }
}

impl MetalScheduler {
    pub fn new(device: &MetalDevice) -> Self {
        Self {
            queue: device.retained_command_queue(),
            active: None,
            active_upload: None,
            active_encoder: None,
            active_render_pass_key: None,
            active_sampler_states: None,
            in_flight: VecDeque::new(),
            next_tick: 1,
            known_gpu_tick: 0,
            submission_profiler: std::env::var_os("RUZU_PROFILE_METAL_SUBMISSIONS")
                .is_some()
                .then(SubmissionProfiler::new),
            stage_profiler: std::env::var_os("RUZU_PROFILE_METAL_STAGES")
                .is_some()
                .then(|| MetalGpuProfiler::new(device.device()))
                .flatten(),
            active_stage_samples: None,
            non_render_serial: 0,
            command_journal: CommandJournal::from_environment(),
            active_workload: None,
        }
    }

    pub fn begin(
        &self,
    ) -> Result<Retained<ProtocolObject<dyn MTLCommandBuffer>>, MetalSchedulerError> {
        if let Some(journal) = &self.command_journal {
            journal.record(format_args!(
                "allocation_begin next_tick={} pending={} known_gpu_tick={}",
                self.next_tick, self.in_flight.len(), self.known_gpu_tick));
        }
        let command_buffer = self.queue.commandBuffer();
        if let Some(journal) = &self.command_journal {
            journal.record(format_args!("allocation_returned object=0x{:x}",
                command_buffer.as_deref().map_or(0, |buffer| buffer as *const _ as usize)));
        }
        command_buffer.ok_or(MetalSchedulerError::NoCommandBuffer)
    }

    /// Diagnostic-only callback; never retain a command buffer through its own
    /// completion block or capture the scheduler, which may already be dropped.
    fn journal_submission(
        &self,
        command_buffer: &ProtocolObject<dyn MTLCommandBuffer>,
        tick: u64,
        kind: SubmissionKind,
        members: usize,
    ) {
        let Some(journal) = &self.command_journal else { return; };
        let object = command_buffer as *const _ as usize;
        journal.record(format_args!("submission_prepare object=0x{object:x} tick={tick} kind={kind:?} members={members}"));
        let completion_journal = Arc::clone(journal);
        let handler = RcBlock::new(move |buffer: std::ptr::NonNull<ProtocolObject<dyn MTLCommandBuffer>>| {
            // Metal supplies a live command buffer for the duration of the callback.
            let status = unsafe { buffer.as_ref() }.status().0;
            completion_journal.record(format_args!("completed object=0x{object:x} tick={tick} status={status} members={members}"));
        });
        // Metal copies the block. The only captured owner is the journal Arc.
        unsafe { command_buffer.addCompletedHandler(RcBlock::as_ptr(&handler)); }
        journal.record(format_args!("commit_begin object=0x{object:x} tick={tick} kind={kind:?}"));
    }

    fn journal_committed(&self, command_buffer: &ProtocolObject<dyn MTLCommandBuffer>, tick: u64) {
        if let Some(journal) = &self.command_journal {
            journal.record(format_args!("commit_returned object=0x{:x} tick={tick}",
                command_buffer as *const _ as usize));
        }
    }

    /// Return the command buffer that records the current guest batch.
    ///
    /// This is the Metal counterpart of Eden's scheduler chunk: buffer/image
    /// copies, compute passes and render encoders are appended in guest order,
    /// then `flush` commits the batch once. Returning a retained reference lets
    /// a short-lived encoder borrow the command buffer without exposing the
    /// scheduler's active-slot ownership.
    pub fn active_command_buffer(
        &mut self,
    ) -> Result<Retained<ProtocolObject<dyn MTLCommandBuffer>>, MetalSchedulerError> {
        if self.active.is_none() {
            self.active = Some(self.begin()?);
            self.active_workload = self.command_journal.as_ref().map(|_| CommandWorkload::default());
            // Acquire before any encoder, including upload prefixes. Never
            // start a "complete batch" sample halfway through a command buffer.
            self.active_stage_samples = self.stage_profiler.as_mut()
                .and_then(MetalGpuProfiler::acquire);
        }
        Ok(self.active.as_ref().unwrap().clone())
    }

    /// Encode a transfer while preserving Metal's one-active-encoder rule.
    /// Consecutive copies share one blit encoder and are therefore batched.
    ///
    /// This is the ordered path, not the reorderable upload prefix.
    #[track_caller]
    pub fn with_blit_encoder<R>(
        &mut self,
        record: impl FnOnce(&ProtocolObject<dyn MTLBlitCommandEncoder>) -> R,
    ) -> Result<R, MetalSchedulerError> {
        self.non_render_serial = self.non_render_serial.wrapping_add(1);
        if !matches!(self.active_encoder.as_ref(), Some(ActiveEncoder::Blit(_))) {
            self.end_active_encoder();
            let command_buffer = self.active_command_buffer()?;
            let encoder = if let Some(samples) = self.active_stage_samples.as_mut() {
                let descriptor = MTLBlitPassDescriptor::new();
                samples.attach_blit(&descriptor);
                command_buffer.blitCommandEncoderWithDescriptor(&descriptor)
            } else {
                command_buffer.blitCommandEncoder()
            }
            .ok_or(MetalSchedulerError::NoBlitEncoder)?;
            self.active_encoder = Some(ActiveEncoder::Blit(encoder));
        }
        let Some(ActiveEncoder::Blit(encoder)) = self.active_encoder.as_ref() else {
            unreachable!("blit encoder was installed above")
        };
        if let Some(workload) = self.active_workload.as_mut() {
            workload.observe_blit();
        }
        Ok(record(encoder))
    }

    /// Caller must prove the destination range has not been used by this batch
    /// and the source stream lease survives its shared completion tick.
    pub(crate) fn with_upload_blit_encoder<R>(
        &mut self,
        record: impl FnOnce(&ProtocolObject<dyn MTLBlitCommandEncoder>) -> R,
    ) -> Result<R, MetalSchedulerError> {
        self.non_render_serial = self.non_render_serial.wrapping_add(1);
        // Keep an active consumer even for upload-only batches, so flush/wait
        // and all staging leases continue to refer to one logical tick.
        self.active_command_buffer()?;
        if self.active_upload.is_none() {
            let command_buffer = self.begin()?;
            let encoder = command_buffer.blitCommandEncoder().ok_or(MetalSchedulerError::NoBlitEncoder)?;
            self.active_upload = Some((command_buffer, encoder));
        }
        Ok(record(&self.active_upload.as_ref().unwrap().1))
    }

    /// Encode compute work, ending a render or blit pass first when needed.
    #[track_caller]
    pub fn with_compute_encoder<R>(
        &mut self,
        record: impl FnOnce(&ProtocolObject<dyn MTLComputeCommandEncoder>) -> R,
    ) -> Result<R, MetalSchedulerError> {
        self.with_compute_encoder_for(ComputeWork::Other, record)
    }

    /// Diagnostic attribution only: the tag never changes encoder reuse/order.
    #[track_caller]
    pub(crate) fn with_compute_encoder_for<R>(
        &mut self,
        work: ComputeWork,
        record: impl FnOnce(&ProtocolObject<dyn MTLComputeCommandEncoder>) -> R,
    ) -> Result<R, MetalSchedulerError> {
        self.non_render_serial = self.non_render_serial.wrapping_add(1);
        let ended_render = matches!(self.active_encoder.as_ref(), Some(ActiveEncoder::Render(_)));
        if !matches!(
            self.active_encoder.as_ref(),
            Some(ActiveEncoder::Compute(_))
        ) {
            self.end_active_encoder();
            let command_buffer = self.active_command_buffer()?;
            let encoder = if let Some(samples) = self.active_stage_samples.as_mut() {
                let descriptor = MTLComputePassDescriptor::new();
                samples.attach_compute(&descriptor);
                command_buffer.computeCommandEncoderWithDescriptor(&descriptor)
            } else {
                command_buffer.computeCommandEncoder()
            }
            .ok_or(MetalSchedulerError::NoComputeEncoder)?;
            self.active_encoder = Some(ActiveEncoder::Compute(encoder));
        }
        if let Some(samples) = self.active_stage_samples.as_mut() {
            samples.observe_compute(work, ended_render);
        }
        let Some(ActiveEncoder::Compute(encoder)) = self.active_encoder.as_ref() else {
            unreachable!("compute encoder was installed above")
        };
        if let Some(workload) = self.active_workload.as_mut() {
            workload.observe_compute(work);
        }
        Ok(record(encoder))
    }

    /// Start a native render pass. Render-pass compatibility is owned by the
    /// framebuffer runtime, so a new descriptor always closes the prior pass.
    #[track_caller]
    pub fn begin_render_pass(
        &mut self,
        descriptor: &MTLRenderPassDescriptor,
    ) -> Result<(), MetalSchedulerError> {
        self.end_active_encoder();
        self.install_render_encoder(descriptor)
    }

    /// Keep the current render encoder when the complete attachment identity
    /// is unchanged. This is the Metal equivalent of retaining Eden's active
    /// render-pass operation context across compatible draws.
    #[track_caller]
    pub fn begin_or_reuse_render_pass(
        &mut self,
        descriptor: &MTLRenderPassDescriptor,
        key: MetalRenderPassKey,
    ) -> Result<(), MetalSchedulerError> {
        if matches!(self.active_encoder.as_ref(), Some(ActiveEncoder::Render(_)))
            && self.active_render_pass_key == Some(key)
        {
            return Ok(());
        }
        self.end_active_encoder();
        self.install_render_encoder(descriptor)?;
        self.active_render_pass_key = Some(key);
        Ok(())
    }

    fn install_render_encoder(
        &mut self,
        descriptor: &MTLRenderPassDescriptor,
    ) -> Result<(), MetalSchedulerError> {
        let command_buffer = self.active_command_buffer()?;
        // Never leave diagnostic attachments on a descriptor reused by callers.
        let profiled_descriptor = self.active_stage_samples.as_mut().map(|samples| {
            let copy = descriptor.copy();
            samples.attach_render(&copy);
            copy
        });
        let encoder = command_buffer
            .renderCommandEncoderWithDescriptor(
                profiled_descriptor.as_deref().unwrap_or(descriptor),
            )
            .ok_or(MetalSchedulerError::NoRenderEncoder)?;
        self.active_encoder = Some(ActiveEncoder::Render(encoder));
        Ok(())
    }

    pub fn with_render_encoder<R>(
        &mut self,
        record: impl FnOnce(&ProtocolObject<dyn MTLRenderCommandEncoder>) -> R,
    ) -> Result<R, MetalSchedulerError> {
        let Some(ActiveEncoder::Render(encoder)) = self.active_encoder.as_ref() else {
            return Err(MetalSchedulerError::NoActiveRenderEncoder);
        };
        Ok(record(encoder))
    }

    pub(crate) fn profile_graphics_draw(&mut self, shaders: [u64; 6]) {
        if matches!(self.active_encoder.as_ref(), Some(ActiveEncoder::Render(_))) {
            if let Some(workload) = self.active_workload.as_mut() {
                workload.observe_draw(shaders);
            }
            if let Some(samples) = self.active_stage_samples.as_mut() {
                samples.observe_graphics_draw(shaders);
            }
        }
    }

    pub(super) fn profile_helper_draw(&mut self, work: super::metal_gpu_profiler::RenderHelper) {
        if matches!(self.active_encoder.as_ref(), Some(ActiveEncoder::Render(_))) {
            if let Some(samples) = self.active_stage_samples.as_mut() {
                samples.observe_helper_draw(work);
            }
        }
    }

    pub(crate) fn profile_depth_feedback<'a>(
        &mut self,
        descriptor: &objc2_metal::MTLRenderPassDescriptor,
        requested: bool,
        state: &super::metal_pipeline_cache::MetalDepthStencilKey,
        textures: impl Iterator<Item = (usize, u32, u64, &'a ProtocolObject<dyn objc2_metal::MTLTexture>)>,
    ) {
        if let Some(samples) = self.active_stage_samples.as_mut() {
            samples.observe_depth_feedback(descriptor, requested, state, textures);
        }
    }

    /// Unlike directly bound resources, argument-buffer samplers are not retained
    /// by an encoder or covered by useResource. The command buffer owns this
    /// cohort through its completion handler, even if the scheduler is dropped.
    pub fn retain_sampler_states(
        &mut self,
        states: impl IntoIterator<Item = Retained<ProtocolObject<dyn MTLSamplerState>>>,
    ) -> Result<(), MetalSchedulerError> {
        if self.active_sampler_states.is_none() {
            let states = Arc::new(Mutex::new(Vec::new()));
            let completed = Arc::clone(&states);
            let handler = RcBlock::new(
                move |_: std::ptr::NonNull<ProtocolObject<dyn MTLCommandBuffer>>| {
                    completed.lock().clear();
                },
            );
            // Metal copies this block before the call returns.
            unsafe {
                self.active_command_buffer()?
                    .addCompletedHandler(RcBlock::as_ptr(&handler))
            };
            self.active_sampler_states = Some(states);
        }
        self.active_sampler_states
            .as_ref()
            .unwrap()
            .lock()
            .extend(states);
        Ok(())
    }

    #[track_caller]
    pub fn end_render_pass(&mut self) {
        if matches!(self.active_encoder.as_ref(), Some(ActiveEncoder::Render(_))) {
            self.end_active_encoder();
        }
    }

    #[track_caller]
    pub fn request_outside_render_pass_operation_context(&mut self) {
        self.request_outside_render_pass_operation_context_for(ComputeWork::Other);
    }

    #[track_caller]
    pub(crate) fn request_outside_render_pass_operation_context_for(&mut self, work: ComputeWork) {
        if matches!(self.active_encoder.as_ref(), Some(ActiveEncoder::Render(_))) {
            if let Some(samples) = self.active_stage_samples.as_mut() {
                samples.observe_render_break(work);
            }
        }
        self.end_render_pass();
    }

    #[track_caller]
    fn end_active_encoder(&mut self) {
        if matches!(self.active_encoder.as_ref(), Some(ActiveEncoder::Render(_))) {
            if let Some(samples) = self.active_stage_samples.as_mut() {
                samples.observe_render_end();
            }
        }
        self.active_render_pass_key = None;
        if let Some(encoder) = self.active_encoder.take() {
            encoder.end_encoding();
        }
    }

    /// Submit the current guest batch, if any.
    #[track_caller]
    pub fn flush(&mut self) -> Result<Option<u64>, MetalSchedulerError> {
        self.end_active_encoder();
        let Some(command_buffer) = self.active.take() else {
            return Ok(None);
        };
        // Only this internally recorded buffer owns the summary. Presentation
        // and caller-owned buffers must not consume an unrelated guest batch.
        if let (Some(journal), Some(workload)) = (&self.command_journal, self.active_workload.take()) {
            workload.record(journal, &*command_buffer as *const _ as usize, self.next_tick);
        }
        self.active_sampler_states = None;
        let prefix = self.active_upload.take().map(|(buffer, encoder)| {
            encoder.endEncoding();
            buffer
        });
        self.commit_cohort(prefix, command_buffer, SubmissionKind::Guest)
            .map(Some)
    }

    pub fn commit(
        &mut self,
        command_buffer: Retained<ProtocolObject<dyn MTLCommandBuffer>>,
    ) -> Result<u64, MetalSchedulerError> {
        self.flush()?;
        self.commit_with_kind(command_buffer, SubmissionKind::External)
    }

    pub fn commit_presentation(
        &mut self,
        command_buffer: Retained<ProtocolObject<dyn MTLCommandBuffer>>,
    ) -> Result<u64, MetalSchedulerError> {
        self.flush()?;
        self.commit_with_kind(command_buffer, SubmissionKind::Presentation)
    }

    fn commit_with_kind(
        &mut self,
        command_buffer: Retained<ProtocolObject<dyn MTLCommandBuffer>>,
        kind: SubmissionKind,
    ) -> Result<u64, MetalSchedulerError> {
        self.commit_cohort(None, command_buffer, kind)
    }

    // Both native buffers share the tick captured by staging leases. Only the
    // full successful cohort may advance resource retirement.
    fn commit_cohort(
        &mut self,
        upload_prefix: Option<Retained<ProtocolObject<dyn MTLCommandBuffer>>>,
        command_buffer: Retained<ProtocolObject<dyn MTLCommandBuffer>>,
        kind: SubmissionKind,
    ) -> Result<u64, MetalSchedulerError> {
        self.poll_completed()?;
        let tick = self.next_tick;
        self.next_tick = self.next_tick.wrapping_add(1);
        if let Some(prefix) = &upload_prefix {
            self.journal_submission(prefix, tick, SubmissionKind::UploadPrefix, 2);
            prefix.commit();
            self.journal_committed(prefix, tick);
        }
        self.journal_submission(&command_buffer, tick, kind, 1 + usize::from(upload_prefix.is_some()));
        command_buffer.commit();
        self.journal_committed(&command_buffer, tick);
        self.in_flight.push_back(InFlightCommandBuffer {
            tick,
            upload_prefix,
            command_buffer,
            kind,
            stage_samples: if matches!(kind, SubmissionKind::Guest) {
                self.active_stage_samples.take()
            } else {
                None
            },
        });
        Ok(tick)
    }

    pub fn finish(
        &mut self,
        command_buffer: Retained<ProtocolObject<dyn MTLCommandBuffer>>,
    ) -> Result<u64, MetalSchedulerError> {
        // Metal executes command buffers in commit order, not allocation
        // order. Preserve the guest order by committing the current batch
        // before the caller-owned buffer that must complete synchronously.
        self.flush()?;
        let tick = self.commit_with_kind(command_buffer, SubmissionKind::Synchronous)?;
        self.wait(tick)?;
        Ok(tick)
    }

    pub fn current_tick(&self) -> u64 {
        self.next_tick
    }

    /// Direct conditional argument groups cannot cross a submission or a
    /// potential predicate producer, including work in a reused encoder.
    pub(crate) fn conditional_batch_token(&self) -> (u64, u64) {
        (self.next_tick, self.non_render_serial)
    }

    pub fn has_active_work(&self) -> bool {
        self.active.is_some()
    }

    /// Last completion observed by the scheduler without polling Metal.
    /// `BufferCacheRuntime::KnownGpuTick` is a const query in the common
    /// cache; frame ticks poll through the staging pool before using it.
    pub fn completed_tick(&self) -> u64 {
        self.known_gpu_tick
    }

    pub fn known_gpu_tick(&mut self) -> Result<u64, MetalSchedulerError> {
        self.poll_completed()?;
        Ok(self.known_gpu_tick)
    }

    pub fn is_free(&mut self, tick: u64) -> Result<bool, MetalSchedulerError> {
        Ok(tick == 0 || tick <= self.known_gpu_tick()?)
    }

    pub fn wait(&mut self, tick: u64) -> Result<(), MetalSchedulerError> {
        if tick >= self.next_tick && self.active.is_some() {
            self.flush()?;
        }
        if tick == 0 || tick <= self.known_gpu_tick()? {
            return Ok(());
        }
        while self
            .in_flight
            .front()
            .is_some_and(|front| front.tick <= tick)
        {
            Self::wait_for_cohort(self.in_flight.front().unwrap())?;
            self.retire_completed_front()?;
        }
        Ok(())
    }

    pub fn finish_all(&mut self) -> Result<(), MetalSchedulerError> {
        self.flush()?;
        while let Some(in_flight) = self.in_flight.front() {
            Self::wait_for_cohort(in_flight)?;
            self.retire_completed_front()?;
        }
        Ok(())
    }

    fn poll_completed(&mut self) -> Result<(), MetalSchedulerError> {
        while self
            .in_flight
            .front()
            .is_some_and(|entry| cohort_ready_to_validate(
                entry.upload_prefix.as_ref().map(|prefix| prefix.status()),
                entry.command_buffer.status()))
        {
            self.retire_completed_front()?;
        }
        Ok(())
    }

    // A failed submission remains at the queue head: subsequent callers must
    // observe the error rather than reclaim resources behind a later tick.
    fn retire_completed_front(&mut self) -> Result<(), MetalSchedulerError> {
        let front = self.in_flight.front().expect("pending submission");
        Self::check_cohort_errors(front)?;
        if let Some(prefix) = &front.upload_prefix { Self::check_completed(prefix)?; }
        Self::check_completed(&front.command_buffer)?;
        let completed = self.in_flight.pop_front().unwrap();
        if let Some(prefix) = &completed.upload_prefix {
            self.profile_completed(prefix, completed.tick, SubmissionKind::UploadPrefix);
        }
        if let (Some(profiler), Some(samples)) =
            (&mut self.stage_profiler, completed.stage_samples)
        {
            profiler.completed(completed.tick, samples, Some(
                (completed.command_buffer.GPUEndTime() - completed.command_buffer.GPUStartTime()) * 1000.0));
        }
        self.profile_completed(&completed.command_buffer, completed.tick, completed.kind);
        self.known_gpu_tick = self.known_gpu_tick.max(completed.tick);
        if let Some(journal) = &self.command_journal {
            journal.record(format_args!("cohort_retired tick={} members={}",
                completed.tick, 1 + usize::from(completed.upload_prefix.is_some())));
        }
        Ok(())
    }

    fn wait_for_cohort(entry: &InFlightCommandBuffer) -> Result<(), MetalSchedulerError> {
        Self::check_cohort_errors(entry)?;
        if let Some(prefix) = &entry.upload_prefix {
            prefix.waitUntilCompleted();
            Self::check_completed(prefix)?;
        }
        entry.command_buffer.waitUntilCompleted();
        Self::check_completed(&entry.command_buffer)
    }

    fn check_cohort_errors(entry: &InFlightCommandBuffer) -> Result<(), MetalSchedulerError> {
        for buffer in entry.upload_prefix.iter().chain(std::iter::once(&entry.command_buffer)) {
            if buffer.status() == MTLCommandBufferStatus::Error {
                Self::check_completed(buffer)?;
            }
        }
        Ok(())
    }

    fn profile_completed(
        &mut self,
        command_buffer: &ProtocolObject<dyn MTLCommandBuffer>,
        tick: u64,
        kind: SubmissionKind,
    ) {
        if let Some(profiler) = self.submission_profiler.as_mut() {
            profiler.observe(
                kind,
                tick,
                command_buffer.GPUStartTime(),
                command_buffer.GPUEndTime(),
            );
        }
    }

    fn check_completed(
        command_buffer: &ProtocolObject<dyn MTLCommandBuffer>,
    ) -> Result<(), MetalSchedulerError> {
        let status = command_buffer.status();
        if status == MTLCommandBufferStatus::Completed {
            Ok(())
        } else {
            let reason = command_buffer.error()
                .map(|error| error.to_string())
                .unwrap_or_else(|| "no driver error description".to_owned());
            Err(MetalSchedulerError::CommandBufferFailed(status, reason))
        }
    }
}

impl Drop for MetalScheduler {
    fn drop(&mut self) {
        // Metal requires every command encoder to receive `endEncoding`
        // before its final release, including while unwinding from an error.
        self.end_active_encoder();
        if let Some((buffer, encoder)) = self.active_upload.take() {
            encoder.endEncoding();
            drop(buffer);
        }
    }
}

fn is_terminal_status(status: MTLCommandBufferStatus) -> bool {
    status == MTLCommandBufferStatus::Completed || status == MTLCommandBufferStatus::Error
}

fn cohort_ready_to_validate(prefix: Option<MTLCommandBufferStatus>, render: MTLCommandBufferStatus) -> bool {
    prefix == Some(MTLCommandBufferStatus::Error) || render == MTLCommandBufferStatus::Error
        || (prefix.is_none_or(is_terminal_status) && is_terminal_status(render))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_cache_upload_prefix_requires_stream_identity_and_unused_range() {
        use crate::buffer_cache::buffer_cache_base::{BufferCacheRuntime as _, BufferCacheBuffer as _, BufferCacheAsyncBuffer as _, BufferCopy};
        use super::super::metal_buffer_cache::{Buffer, BufferCacheRuntime};
        use super::super::metal_staging_buffer_pool::MetalStagingBufferPool;
        let device = MetalDevice::new().unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let mut pool = MetalStagingBufferPool::new(&device).unwrap();
        let mut runtime = BufferCacheRuntime::new(&device, &mut scheduler, &mut pool);
        let mut destination = Buffer::new(&mut runtime, 0x1000, 128);
        let mut stream = pool.request_upload_buffer(&mut scheduler, 4, false).unwrap();
        let mut dedicated = pool.request_upload_buffer(&mut scheduler, 4, true).unwrap();
        stream.mapped_span_mut().fill(1);
        dedicated.mapped_span_mut().fill(2);
        let copy = BufferCopy { src_offset: stream.offset as u64, dst_offset: 0, size: 4 };
        assert!(runtime.can_reorder_upload(&destination, &[copy]));
        runtime.copy_buffer_from_staging(&destination, &stream, &[copy], true, false);
        assert!(scheduler.active_upload.is_none());
        scheduler.finish_all().unwrap();
        runtime.copy_buffer_from_staging(&destination, &dedicated,
            &[BufferCopy { src_offset: dedicated.offset as u64, ..copy }], true, true);
        assert!(scheduler.active_upload.is_none());
        scheduler.finish_all().unwrap();
        runtime.copy_buffer_from_staging(&destination, &stream, &[copy], true, true);
        assert!(scheduler.active_upload.is_some());
        destination.mark_usage(0, 64);
        assert!(!runtime.can_reorder_upload(&destination, &[copy]));
        // Use one complete UsageTracker granule; sub-granule writes have
        // conservative over-marking around upstream's shift-by-64 case.
        assert!(!runtime.can_reorder_upload(&destination,
            &[BufferCopy { dst_offset: 8, ..copy }]));
        assert!(runtime.can_reorder_upload(&destination,
            &[BufferCopy { dst_offset: 64, ..copy }]));
        scheduler.finish_all().unwrap();
    }

    #[test]
    fn active_upload_keeps_render_encoder_and_shares_flush_tick() {
        use super::super::metal_buffer::MetalBuffer;
        let device = MetalDevice::new().unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let source = MetalBuffer::new(&device, 4).unwrap();
        let destination = MetalBuffer::new(&device, 4).unwrap();
        let output = MetalBuffer::new(&device, 4).unwrap();
        source.write(0, &[9, 8, 7, 6]).unwrap();
        let (descriptor, _texture) = render_pass_descriptor(&device);
        scheduler.begin_render_pass(&descriptor).unwrap();
        let before = scheduler.with_render_encoder(|encoder| encoder as *const _ as usize).unwrap();
        source.encode_upload_copy(&mut scheduler, &destination, 0, 0, 4).unwrap();
        let after = scheduler.with_render_encoder(|encoder| encoder as *const _ as usize).unwrap();
        assert_eq!(before, after);
        assert_eq!(scheduler.current_tick(), 1);
        destination.encode_copy(&mut scheduler, &output, 0, 0, 4).unwrap();
        assert_eq!(scheduler.flush().unwrap(), Some(1));
        assert_eq!(scheduler.current_tick(), 2);
        assert!(scheduler.in_flight.front().unwrap().upload_prefix.is_some());
        scheduler.wait(1).unwrap();
        let mut actual = [0; 4];
        output.read(0, &mut actual).unwrap();
        assert_eq!(actual, [9, 8, 7, 6]);
    }

    #[test]
    fn upload_only_batch_flushes_before_external_commit_and_ends_on_drop() {
        let device = MetalDevice::new().unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        scheduler.with_upload_blit_encoder(|_| {}).unwrap();
        assert!(scheduler.has_active_work());
        let external = scheduler.begin().unwrap();
        assert_eq!(scheduler.commit(external).unwrap(), 2);
        scheduler.wait(2).unwrap();
        assert_eq!(scheduler.completed_tick(), 2);
        scheduler.with_upload_blit_encoder(|_| {}).unwrap();
        // Metal validation rejects releasing a live encoder without ending it.
        drop(scheduler);
    }

    #[test]
    fn cohort_status_requires_both_members_or_reports_either_error() {
        use MTLCommandBufferStatus as S;
        for pending in [S::NotEnqueued, S::Enqueued, S::Committed, S::Scheduled] {
            assert!(!cohort_ready_to_validate(Some(pending), S::Completed));
            assert!(!cohort_ready_to_validate(Some(S::Completed), pending));
            assert!(cohort_ready_to_validate(Some(pending), S::Error));
            assert!(cohort_ready_to_validate(Some(S::Error), pending));
        }
        assert!(cohort_ready_to_validate(Some(S::Completed), S::Completed));
        assert!(cohort_ready_to_validate(None, S::Completed));
        assert!(cohort_ready_to_validate(None, S::Error));
    }

    #[test]
    fn native_upload_cohort_preserves_write_order_and_one_tick() {
        use super::super::metal_buffer::MetalBuffer;
        let device = MetalDevice::new().unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let source = MetalBuffer::new(&device, 4).unwrap();
        let intermediate = MetalBuffer::new(&device, 4).unwrap();
        let output = MetalBuffer::new(&device, 4).unwrap();
        source.write(0, &[1, 2, 3, 4]).unwrap();
        let prefix = scheduler.begin().unwrap();
        let consumer = scheduler.begin().unwrap();
        let encoder = prefix.blitCommandEncoder().unwrap();
        unsafe {
            encoder.copyFromBuffer_sourceOffset_toBuffer_destinationOffset_size(
                source.handle(), 0, intermediate.handle(), 0, 4);
            encoder.copyFromBuffer_sourceOffset_toBuffer_destinationOffset_size(
                source.handle(), 2, intermediate.handle(), 0, 2);
        }
        encoder.endEncoding();
        let encoder = consumer.blitCommandEncoder().unwrap();
        unsafe {
            encoder.copyFromBuffer_sourceOffset_toBuffer_destinationOffset_size(
                intermediate.handle(), 0, output.handle(), 0, 4);
        }
        encoder.endEncoding();
        let tick = scheduler.current_tick();
        assert_eq!(scheduler.commit_cohort(Some(prefix), consumer, SubmissionKind::Guest).unwrap(), tick);
        assert_eq!(scheduler.current_tick(), tick + 1);
        assert_eq!(scheduler.in_flight.len(), 1);
        drop(source);
        drop(intermediate);
        scheduler.wait(tick).unwrap();
        assert_eq!(scheduler.completed_tick(), tick);
        let mut actual = [0; 4];
        output.read(0, &mut actual).unwrap();
        assert_eq!(actual, [3, 4, 3, 4]);
    }

    #[test]
    fn native_cohort_never_retires_when_only_one_member_completed() {
        use super::super::metal_staging_buffer_pool::MetalStagingBufferPool;
        let device = MetalDevice::new().unwrap();
        for prefix_first in [true, false] {
            let mut scheduler = MetalScheduler::new(&device);
            let mut pool = MetalStagingBufferPool::new(&device).unwrap();
            let mut lease = pool.request_download_buffer(&mut scheduler, 32, true).unwrap();
            pool.free_deferred(&scheduler, &mut lease).unwrap();
            let prefix = scheduler.begin().unwrap();
            let render = scheduler.begin().unwrap();
            scheduler.in_flight.push_back(InFlightCommandBuffer {
                tick: 1, upload_prefix: Some(prefix.clone()), command_buffer: render.clone(),
                kind: SubmissionKind::Guest, stage_samples: None,
            });
            scheduler.next_tick = 2;
            let (first, last) = if prefix_first { (&prefix, &render) } else { (&render, &prefix) };
            first.commit();
            first.waitUntilCompleted();
            for _ in 0..2 {
                assert!(!scheduler.is_free(1).unwrap());
                assert_eq!(scheduler.completed_tick(), 0);
                assert_eq!(scheduler.in_flight.len(), 1);
                assert_eq!(pool.cache_memory_usage(scheduler.completed_tick())[1].reusable_bytes, 0);
                assert!(scheduler.retire_completed_front().is_err());
            }
            last.commit();
            scheduler.wait(1).unwrap();
            assert_eq!(scheduler.completed_tick(), 1);
            assert!(scheduler.in_flight.is_empty());
            assert_eq!(pool.cache_memory_usage(scheduler.completed_tick())[1].reusable_bytes, 32);
            let reused = pool.request_download_buffer(&mut scheduler, 32, false).unwrap();
            assert!(Arc::ptr_eq(&lease.buffer, &reused.buffer));
        }
    }

    #[test]
    fn rejected_retirement_preserves_owner_and_completion_tick() {
        let device = MetalDevice::new().unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let first = scheduler.begin().unwrap();
        let second = scheduler.begin().unwrap();
        for (tick, command_buffer) in [(1, first.clone()), (2, second.clone())] {
            scheduler.in_flight.push_back(InFlightCommandBuffer {
                tick, upload_prefix: None, command_buffer, kind: SubmissionKind::Guest, stage_samples: None,
            });
        }
        scheduler.next_tick = 3;
        // Exercise the same failed validation path without provoking a GPU
        // fault: these real native buffers have not been submitted yet.
        for _ in 0..2 {
            assert!(matches!(scheduler.retire_completed_front(),
                Err(MetalSchedulerError::CommandBufferFailed(MTLCommandBufferStatus::NotEnqueued, _))));
            assert_eq!(scheduler.in_flight.len(), 2);
            assert_eq!(scheduler.in_flight.front().unwrap().tick, 1);
            assert_eq!(scheduler.completed_tick(), 0);
        }
        first.commit();
        first.waitUntilCompleted();
        scheduler.retire_completed_front().unwrap();
        assert_eq!(scheduler.completed_tick(), 1);
        assert!(scheduler.retire_completed_front().is_err());
        assert_eq!(scheduler.completed_tick(), 1);
        assert_eq!(scheduler.in_flight.len(), 1);
        second.commit();
        scheduler.wait(2).unwrap();
        assert_eq!(scheduler.completed_tick(), 2);
        assert!(scheduler.in_flight.is_empty());
    }

    #[test]
    fn conditional_batch_token_changes_for_reused_encoders_and_submission() {
        let device = MetalDevice::new().unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let mut token = scheduler.conditional_batch_token();
        for _ in 0..2 {
            scheduler.with_blit_encoder(|_| {}).unwrap();
            assert_ne!(scheduler.conditional_batch_token(), token);
            token = scheduler.conditional_batch_token();
        }
        for _ in 0..2 {
            scheduler.with_compute_encoder(|_| {}).unwrap();
            assert_ne!(scheduler.conditional_batch_token(), token);
            token = scheduler.conditional_batch_token();
        }
        scheduler.flush().unwrap();
        assert_ne!(scheduler.conditional_batch_token(), token);
        scheduler.finish_all().unwrap();
    }

    #[test]
    fn submission_timing_excludes_unavailable_and_invalid_timestamps() {
        let mut timing = SubmissionTiming::default();
        for (start, end) in [
            (0.0, 0.0),
            (0.0, 2.0),
            (2.0, 1.0),
            (f64::NAN, 3.0),
            (1.0, f64::INFINITY),
        ] {
            timing.observe(1, start, end);
        }
        assert_eq!(timing.completed, 5);
        assert_eq!(timing.measured, 0);
        assert_eq!(timing.gpu_ms, 0.0);
        timing.observe(12, 4.0, 4.125);
        timing.observe(13, 5.0, 5.0625);
        assert_eq!(timing.completed, 7);
        assert_eq!(timing.measured, 2);
        assert_eq!(timing.gpu_ms, 187.5);
        assert_eq!(timing.peak_ms, 125.0);
        assert_eq!(timing.peak_tick, 12);
    }

    #[test]
    fn submission_profiling_preserves_ticks_and_observes_each_completion_once() {
        let device = MetalDevice::new().expect("Metal device");
        let mut scheduler = MetalScheduler::new(&device);
        scheduler.submission_profiler = Some(SubmissionProfiler::new());
        scheduler.active_command_buffer().unwrap();
        assert_eq!(scheduler.flush().unwrap(), Some(1));
        let present = scheduler.begin().unwrap();
        assert_eq!(scheduler.commit_presentation(present).unwrap(), 2);
        let external = scheduler.begin().unwrap();
        assert_eq!(scheduler.commit(external).unwrap(), 3);
        let synchronous = scheduler.begin().unwrap();
        assert_eq!(scheduler.finish(synchronous).unwrap(), 4);
        scheduler.finish_all().unwrap();
        assert_eq!(scheduler.known_gpu_tick().unwrap(), 4);
        assert_eq!(
            scheduler
                .submission_profiler
                .as_ref()
                .unwrap()
                .total_completed,
            4
        );
        scheduler.wait(4).unwrap();
        assert_eq!(
            scheduler
                .submission_profiler
                .as_ref()
                .unwrap()
                .total_completed,
            4
        );
    }

    #[test]
    fn native_stage_counters_preserve_passes_descriptors_and_submission_ticks() {
        use objc2_metal::MTLLoadAction;
        let device = MetalDevice::new().unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let Some(profiler) = MetalGpuProfiler::new(device.device()) else {
            return;
        };
        scheduler.stage_profiler = Some(profiler);
        scheduler.with_blit_encoder(|_| {}).unwrap();
        assert!(scheduler.active_stage_samples.is_some());
        scheduler.with_compute_encoder(|_| {}).unwrap();
        let (descriptor, _texture) = render_pass_descriptor(&device);
        unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) }
            .setLoadAction(MTLLoadAction::Clear);
        scheduler.begin_render_pass(&descriptor).unwrap();
        assert!(scheduler.active_stage_samples.is_some());
        let original = unsafe {
            descriptor
                .sampleBufferAttachments()
                .objectAtIndexedSubscript(0)
        };
        assert!(original.sampleBuffer().is_none());
        assert_eq!(scheduler.current_tick(), 1);
        assert_eq!(scheduler.flush().unwrap(), Some(1));
        assert!(scheduler.active_stage_samples.is_none());
        assert!(scheduler.in_flight.back().unwrap().stage_samples.is_some());
        scheduler.with_blit_encoder(|_| {}).unwrap();
        assert!(scheduler.active_stage_samples.is_none());
        assert_eq!(scheduler.flush().unwrap(), Some(2));
        assert!(scheduler.in_flight.back().unwrap().stage_samples.is_none());
        scheduler.wait(2).unwrap();
        assert_eq!(scheduler.known_gpu_tick().unwrap(), 2);
        assert!(scheduler.in_flight.is_empty());
    }

    #[test]
    fn compute_attribution_preserves_reuse_and_counts_each_render_exit_once() {
        let device = MetalDevice::new().unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let Some(profiler) = MetalGpuProfiler::new(device.device()) else { return };
        scheduler.stage_profiler = Some(profiler);
        let (descriptor, _texture) = render_pass_descriptor(&device);
        scheduler.begin_render_pass(&descriptor).unwrap();
        scheduler.request_outside_render_pass_operation_context_for(ComputeWork::ConditionalArguments);
        let first = scheduler.with_compute_encoder_for(ComputeWork::ConditionalArguments,
            |encoder| encoder as *const _ as *const ()).unwrap();
        let second = scheduler.with_compute_encoder_for(ComputeWork::GeometryVertex,
            |encoder| encoder as *const _ as *const ()).unwrap();
        assert_eq!(first, second, "a diagnostic tag must not split an encoder");
        let samples = scheduler.active_stage_samples.as_ref().unwrap();
        assert_eq!(samples.work_counts(ComputeWork::ConditionalArguments), (1, 1));
        assert_eq!(samples.work_counts(ComputeWork::GeometryVertex), (1, 0));
        assert_eq!(samples.render_end_count(), 1);
        scheduler.begin_render_pass(&descriptor).unwrap();
        scheduler.with_compute_encoder_for(ComputeWork::GeometryVertex, |_| {}).unwrap();
        assert_eq!(scheduler.active_stage_samples.as_ref().unwrap().work_counts(ComputeWork::GeometryVertex), (2, 1));
        assert_eq!(scheduler.active_stage_samples.as_ref().unwrap().render_end_count(), 2);
        assert_eq!(scheduler.current_tick(), 1);
        scheduler.finish_all().unwrap();
        assert_eq!(scheduler.known_gpu_tick().unwrap(), 1);
    }

    fn render_pass_descriptor(
        device: &MetalDevice,
    ) -> (
        Retained<MTLRenderPassDescriptor>,
        Retained<ProtocolObject<dyn objc2_metal::MTLTexture>>,
    ) {
        use objc2_metal::{
            MTLDevice, MTLLoadAction, MTLPixelFormat, MTLStoreAction, MTLTextureDescriptor,
            MTLTextureUsage,
        };

        let texture_descriptor = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::RGBA8Unorm,
                1,
                1,
                false,
            )
        };
        texture_descriptor.setUsage(MTLTextureUsage::RenderTarget);
        let texture = device
            .device()
            .newTextureWithDescriptor(&texture_descriptor)
            .expect("render-target texture");
        let descriptor = MTLRenderPassDescriptor::renderPassDescriptor();
        let color = unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) };
        color.setTexture(Some(&texture));
        color.setLoadAction(MTLLoadAction::DontCare);
        color.setStoreAction(MTLStoreAction::DontCare);
        (descriptor, texture)
    }

    #[test]
    fn indirect_sampler_cohort_survives_flush_and_clears_after_gpu_completion() {
        use objc2_metal::{MTLDevice, MTLSamplerDescriptor};
        let device = MetalDevice::new().unwrap();
        let descriptor = MTLSamplerDescriptor::new();
        descriptor.setSupportArgumentBuffers(true);
        let sampler = device
            .device()
            .newSamplerStateWithDescriptor(&descriptor)
            .unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        scheduler.retain_sampler_states([sampler.clone()]).unwrap();
        scheduler.retain_sampler_states([sampler]).unwrap();
        let cohort = Arc::clone(scheduler.active_sampler_states.as_ref().unwrap());
        assert_eq!(cohort.lock().len(), 2);
        scheduler.flush().unwrap();
        assert!(scheduler.active_sampler_states.is_none());
        scheduler.finish_all().unwrap();
        // Completed handlers may run just after waitUntilCompleted returns.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !cohort.lock().is_empty() && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(cohort.lock().is_empty());
    }

    #[test]
    fn commits_and_waits_for_an_empty_native_command_buffer() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let mut scheduler = MetalScheduler::new(&device);
        let command_buffer = scheduler.begin().expect("command buffer must be allocated");
        scheduler
            .finish(command_buffer)
            .expect("empty command buffer must complete");
        assert_eq!(scheduler.known_gpu_tick().unwrap(), 1);
    }

    #[test]
    fn tracks_asynchronous_submission_ticks_in_commit_order() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let mut scheduler = MetalScheduler::new(&device);
        let first = scheduler.begin().unwrap();
        let second = scheduler.begin().unwrap();
        assert_eq!(scheduler.commit(first).unwrap(), 1);
        assert_eq!(scheduler.commit(second).unwrap(), 2);
        scheduler.wait(2).unwrap();
        assert_eq!(scheduler.known_gpu_tick().unwrap(), 2);
    }

    #[test]
    fn batches_active_work_until_flush() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let mut scheduler = MetalScheduler::new(&device);
        let first = scheduler.active_command_buffer().unwrap();
        let second = scheduler.active_command_buffer().unwrap();
        assert_eq!(Retained::as_ptr(&first), Retained::as_ptr(&second));
        assert_eq!(scheduler.flush().unwrap(), Some(1));
        assert_eq!(scheduler.flush().unwrap(), None);
        scheduler.wait(1).unwrap();
    }

    #[test]
    fn reuses_only_matching_render_pass_keys() {
        let device = MetalDevice::new().expect("Metal device");
        let mut scheduler = MetalScheduler::new(&device);
        let (descriptor, _texture) = render_pass_descriptor(&device);
        let first_key = MetalRenderPassKey::default();
        scheduler
            .begin_or_reuse_render_pass(&descriptor, first_key)
            .unwrap();
        let first = scheduler
            .with_render_encoder(|encoder| {
                let pointer: *const ProtocolObject<dyn MTLRenderCommandEncoder> = encoder;
                // Keep the ended encoder alive until after the replacement is
                // allocated. Otherwise Metal may reuse the same object address
                // immediately, making pointer identity an invalid assertion.
                unsafe { Retained::retain(pointer.cast_mut()) }.unwrap()
            })
            .unwrap();
        scheduler
            .begin_or_reuse_render_pass(&descriptor, first_key)
            .unwrap();
        let reused = scheduler
            .with_render_encoder(|encoder| {
                let pointer: *const ProtocolObject<dyn MTLRenderCommandEncoder> = encoder;
                unsafe { Retained::retain(pointer.cast_mut()) }.unwrap()
            })
            .unwrap();
        assert_eq!(Retained::as_ptr(&first), Retained::as_ptr(&reused));

        let changed_key = first_key.with_visibility_result_buffer(1);
        scheduler
            .begin_or_reuse_render_pass(&descriptor, changed_key)
            .unwrap();
        let replaced = scheduler
            .with_render_encoder(|encoder| {
                let pointer: *const ProtocolObject<dyn MTLRenderCommandEncoder> = encoder;
                unsafe { Retained::retain(pointer.cast_mut()) }.unwrap()
            })
            .unwrap();
        assert_ne!(Retained::as_ptr(&first), Retained::as_ptr(&replaced));
        scheduler.finish_all().unwrap();
    }

    #[test]
    fn dropping_scheduler_ends_active_encoder() {
        let device = MetalDevice::new().expect("Metal device");
        let (descriptor, _texture) = render_pass_descriptor(&device);
        let mut scheduler = MetalScheduler::new(&device);
        scheduler.begin_render_pass(&descriptor).unwrap();

        // Metal aborts the process if the final encoder reference is released
        // without `endEncoding`; successful scope exit verifies `Drop`.
        drop(scheduler);
    }

    #[test]
    fn command_buffer_error_is_a_terminal_status() {
        assert!(is_terminal_status(MTLCommandBufferStatus::Completed));
        assert!(is_terminal_status(MTLCommandBufferStatus::Error));
        assert!(!is_terminal_status(MTLCommandBufferStatus::Scheduled));
    }

    #[test]
    fn finish_preserves_active_batch_order_and_monotonic_ticks() {
        let device = MetalDevice::new().expect("Metal device");
        let mut scheduler = MetalScheduler::new(&device);
        scheduler.active_command_buffer().unwrap();
        let synchronous = scheduler.begin().unwrap();
        assert_eq!(scheduler.finish(synchronous).unwrap(), 2);
        assert_eq!(scheduler.known_gpu_tick().unwrap(), 2);
    }
}
