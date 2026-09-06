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
use super::metal_framebuffer::MetalRenderPassKey;
use super::metal_gpu_profiler::{MetalGpuProfiler, StageSamples};

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
    #[error("Metal command buffer completed with status {0:?}")]
    CommandBufferFailed(MTLCommandBufferStatus),
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
    active_encoder: Option<ActiveEncoder>,
    active_render_pass_key: Option<MetalRenderPassKey>,
    active_sampler_states: Option<Arc<Mutex<Vec<Retained<ProtocolObject<dyn MTLSamplerState>>>>>>,
    in_flight: VecDeque<InFlightCommandBuffer>,
    next_tick: u64,
    known_gpu_tick: u64,
    submission_profiler: Option<SubmissionProfiler>,
    stage_profiler: Option<MetalGpuProfiler>,
    active_stage_samples: Option<StageSamples>,
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
    command_buffer: Retained<ProtocolObject<dyn MTLCommandBuffer>>,
    kind: SubmissionKind,
    stage_samples: Option<StageSamples>,
}

#[derive(Clone, Copy)]
enum SubmissionKind {
    Guest,
    Presentation,
    External,
    Synchronous,
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
    timings: [SubmissionTiming; 4],
    total_completed: u64,
}

impl SubmissionProfiler {
    fn new() -> Self {
        Self {
            interval_start: Instant::now(),
            timings: [SubmissionTiming::default(); 4],
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
                "[METAL_GPU_TIME] observation_s={:.3} total_completed={} guest={:?} presentation={:?} external={:?} synchronous={:?}",
                elapsed.as_secs_f64(), self.total_completed, self.timings[0], self.timings[1],
                self.timings[2], self.timings[3],
            );
            self.timings = [SubmissionTiming::default(); 4];
            self.interval_start = Instant::now();
        }
    }
}

impl MetalScheduler {
    pub fn new(device: &MetalDevice) -> Self {
        Self {
            queue: device.retained_command_queue(),
            active: None,
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
        }
    }

    pub fn begin(
        &self,
    ) -> Result<Retained<ProtocolObject<dyn MTLCommandBuffer>>, MetalSchedulerError> {
        self.queue
            .commandBuffer()
            .ok_or(MetalSchedulerError::NoCommandBuffer)
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
        }
        Ok(self.active.as_ref().unwrap().clone())
    }

    /// Encode a transfer while preserving Metal's one-active-encoder rule.
    /// Consecutive copies share one blit encoder and are therefore batched.
    pub fn with_blit_encoder<R>(
        &mut self,
        record: impl FnOnce(&ProtocolObject<dyn MTLBlitCommandEncoder>) -> R,
    ) -> Result<R, MetalSchedulerError> {
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
        Ok(record(encoder))
    }

    /// Encode compute work, ending a render or blit pass first when needed.
    pub fn with_compute_encoder<R>(
        &mut self,
        record: impl FnOnce(&ProtocolObject<dyn MTLComputeCommandEncoder>) -> R,
    ) -> Result<R, MetalSchedulerError> {
        if !matches!(
            self.active_encoder.as_ref(),
            Some(ActiveEncoder::Compute(_))
        ) {
            self.end_active_encoder();
            let command_buffer = self.active_command_buffer()?;
            self.acquire_stage_samples();
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
        let Some(ActiveEncoder::Compute(encoder)) = self.active_encoder.as_ref() else {
            unreachable!("compute encoder was installed above")
        };
        Ok(record(encoder))
    }

    /// Start a native render pass. Render-pass compatibility is owned by the
    /// framebuffer runtime, so a new descriptor always closes the prior pass.
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
        self.acquire_stage_samples();
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

    fn acquire_stage_samples(&mut self) {
        // Start on real compute/render work, not a tiny transfer-only batch
        // that can phase-lock to the once-per-second sampling cadence.
        if self.active_stage_samples.is_none() {
            self.active_stage_samples = self
                .stage_profiler
                .as_mut()
                .and_then(MetalGpuProfiler::acquire);
        }
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

    pub fn end_render_pass(&mut self) {
        if matches!(self.active_encoder.as_ref(), Some(ActiveEncoder::Render(_))) {
            self.end_active_encoder();
        }
    }

    pub fn request_outside_render_pass_operation_context(&mut self) {
        self.end_render_pass();
    }

    fn end_active_encoder(&mut self) {
        self.active_render_pass_key = None;
        if let Some(encoder) = self.active_encoder.take() {
            encoder.end_encoding();
        }
    }

    /// Submit the current guest batch, if any.
    pub fn flush(&mut self) -> Result<Option<u64>, MetalSchedulerError> {
        self.end_active_encoder();
        let Some(command_buffer) = self.active.take() else {
            return Ok(None);
        };
        self.active_sampler_states = None;
        self.commit_with_kind(command_buffer, SubmissionKind::Guest)
            .map(Some)
    }

    pub fn commit(
        &mut self,
        command_buffer: Retained<ProtocolObject<dyn MTLCommandBuffer>>,
    ) -> Result<u64, MetalSchedulerError> {
        self.commit_with_kind(command_buffer, SubmissionKind::External)
    }

    pub fn commit_presentation(
        &mut self,
        command_buffer: Retained<ProtocolObject<dyn MTLCommandBuffer>>,
    ) -> Result<u64, MetalSchedulerError> {
        self.commit_with_kind(command_buffer, SubmissionKind::Presentation)
    }

    fn commit_with_kind(
        &mut self,
        command_buffer: Retained<ProtocolObject<dyn MTLCommandBuffer>>,
        kind: SubmissionKind,
    ) -> Result<u64, MetalSchedulerError> {
        self.poll_completed()?;
        let tick = self.next_tick;
        self.next_tick = self.next_tick.wrapping_add(1);
        command_buffer.commit();
        self.in_flight.push_back(InFlightCommandBuffer {
            tick,
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
        let tick = self.next_tick;
        self.next_tick = self.next_tick.wrapping_add(1);
        command_buffer.commit();
        command_buffer.waitUntilCompleted();
        Self::check_completed(&command_buffer)?;
        self.profile_completed(&command_buffer, tick, SubmissionKind::Synchronous);
        self.known_gpu_tick = self.known_gpu_tick.max(tick);
        self.poll_completed()?;
        Ok(tick)
    }

    pub fn current_tick(&self) -> u64 {
        self.next_tick
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
            let completed = self.in_flight.pop_front().unwrap();
            completed.command_buffer.waitUntilCompleted();
            Self::check_completed(&completed.command_buffer)?;
            if let (Some(profiler), Some(samples)) =
                (&mut self.stage_profiler, completed.stage_samples)
            {
                profiler.completed(completed.tick, samples);
            }
            self.profile_completed(&completed.command_buffer, completed.tick, completed.kind);
            self.known_gpu_tick = self.known_gpu_tick.max(completed.tick);
        }
        Ok(())
    }

    pub fn finish_all(&mut self) -> Result<(), MetalSchedulerError> {
        self.flush()?;
        while let Some(in_flight) = self.in_flight.pop_front() {
            in_flight.command_buffer.waitUntilCompleted();
            Self::check_completed(&in_flight.command_buffer)?;
            if let (Some(profiler), Some(samples)) =
                (&mut self.stage_profiler, in_flight.stage_samples)
            {
                profiler.completed(in_flight.tick, samples);
            }
            self.profile_completed(&in_flight.command_buffer, in_flight.tick, in_flight.kind);
            self.known_gpu_tick = self.known_gpu_tick.max(in_flight.tick);
        }
        Ok(())
    }

    fn poll_completed(&mut self) -> Result<(), MetalSchedulerError> {
        while self
            .in_flight
            .front()
            .is_some_and(|entry| is_terminal_status(entry.command_buffer.status()))
        {
            let completed = self.in_flight.pop_front().unwrap();
            Self::check_completed(&completed.command_buffer)?;
            if let (Some(profiler), Some(samples)) =
                (&mut self.stage_profiler, completed.stage_samples)
            {
                profiler.completed(completed.tick, samples);
            }
            self.profile_completed(&completed.command_buffer, completed.tick, completed.kind);
            self.known_gpu_tick = self.known_gpu_tick.max(completed.tick);
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
            Err(MetalSchedulerError::CommandBufferFailed(status))
        }
    }
}

impl Drop for MetalScheduler {
    fn drop(&mut self) {
        // Metal requires every command encoder to receive `endEncoding`
        // before its final release, including while unwinding from an error.
        self.end_active_encoder();
    }
}

fn is_terminal_status(status: MTLCommandBufferStatus) -> bool {
    status == MTLCommandBufferStatus::Completed || status == MTLCommandBufferStatus::Error
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(scheduler.active_stage_samples.is_none());
        assert_eq!(scheduler.flush().unwrap(), Some(1));
        assert!(scheduler.in_flight.front().unwrap().stage_samples.is_none());
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
        assert_eq!(scheduler.current_tick(), 2);
        assert_eq!(scheduler.flush().unwrap(), Some(2));
        assert!(scheduler.active_stage_samples.is_none());
        assert!(scheduler.in_flight.back().unwrap().stage_samples.is_some());
        scheduler.wait(2).unwrap();
        assert_eq!(scheduler.known_gpu_tick().unwrap(), 2);
        assert!(scheduler.in_flight.is_empty());
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
