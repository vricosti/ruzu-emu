// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Optional native stage-boundary timing. No Vulkan query/barrier emulation.
//! Apple: Sampling GPU data into counter sample buffers.

use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSRange;
use objc2_metal::{
    MTLBlitPassDescriptor, MTLCommonCounterSetTimestamp, MTLComputePassDescriptor,
    MTLCounterSampleBuffer, MTLCounterSampleBufferDescriptor, MTLCounterSamplingPoint,
    MTLCounterSet, MTLDevice, MTLRenderPassDescriptor, MTLStorageMode,
};

const SAMPLE_COUNT: usize = 1024;

pub(super) struct MetalGpuProfiler {
    available: Option<StageSamples>,
    last_capture: Option<Instant>,
}

pub(super) struct StageSamples {
    buffer: Retained<ProtocolObject<dyn MTLCounterSampleBuffer>>,
    stages: Vec<Stage>,
    count: usize,
    omitted: usize,
}

#[derive(Clone, Copy, Debug)]
enum StageKind {
    Blit,
    Compute,
    Vertex,
    Fragment,
}

struct Stage {
    kind: StageKind,
    index: usize,
}

impl MetalGpuProfiler {
    pub(super) fn new(device: &ProtocolObject<dyn MTLDevice>) -> Option<Self> {
        if !device.supportsCounterSampling(MTLCounterSamplingPoint::AtStageBoundary) {
            log::warn!("Metal stage profiling unavailable: no stage-boundary sampling");
            return None;
        }
        let sets = device.counterSets()?;
        // SAFETY: framework-exported immutable counter set name.
        let set = sets
            .iter()
            .find(|set| &*set.name() == unsafe { MTLCommonCounterSetTimestamp })?;
        let descriptor = MTLCounterSampleBufferDescriptor::new();
        descriptor.setCounterSet(Some(&set));
        descriptor.setStorageMode(MTLStorageMode::Shared);
        // SAFETY: bounded 8 KiB timestamp storage, below Apple's 32 KiB limit.
        unsafe { descriptor.setSampleCount(SAMPLE_COUNT) };
        let buffer = match device.newCounterSampleBufferWithDescriptor_error(&descriptor) {
            Ok(buffer) => buffer,
            Err(error) => {
                log::warn!("Metal stage profiling unavailable: {error}");
                return None;
            }
        };
        log::info!(
            "Metal stage profiling enabled: one {SAMPLE_COUNT}-sample buffer, at most one batch/s"
        );
        Some(Self {
            available: Some(StageSamples {
                buffer,
                stages: Vec::new(),
                count: 0,
                omitted: 0,
            }),
            last_capture: None,
        })
    }

    pub(super) fn acquire(&mut self) -> Option<StageSamples> {
        if self
            .last_capture
            .is_some_and(|time| time.elapsed() < Duration::from_secs(1))
        {
            return None;
        }
        let samples = self.available.take()?;
        self.last_capture = Some(Instant::now());
        Some(samples)
    }

    /// Caller has already observed successful command-buffer completion. Never
    /// reuse sample indices while their producing command buffer is in flight.
    pub(super) fn completed(&mut self, tick: u64, mut samples: StageSamples) {
        let mut totals = [0u64; 4];
        let mut measured = [0usize; 4];
        let mut worst = [(0u64, 0usize); 4];
        if samples.count != 0 {
            // SAFETY: reserve bounds every attached sample index; GPU completed.
            let data = unsafe {
                samples
                    .buffer
                    .resolveCounterRange(NSRange::new(0, samples.count))
            };
            if let Some(data) = data {
                // SAFETY: NSData is retained and immutable throughout decoding.
                let bytes = unsafe { data.as_bytes_unchecked() };
                for stage in &samples.stages {
                    if let Some(delta) = sample_delta(bytes, stage.index) {
                        let bucket = stage.kind as usize;
                        measured[bucket] += 1;
                        totals[bucket] = totals[bucket].saturating_add(delta);
                        if delta > worst[bucket].0 {
                            worst[bucket] = (delta, stage.index);
                        }
                    }
                }
            }
        }
        // Raw GPU clock ticks, not nanoseconds or utilization. Stages may overlap.
        log::info!("[METAL_STAGE_TIME] tick={tick} order=blit,compute,vertex,fragment measured={measured:?} gpu_ticks={totals:?} worst_ticks_sample={worst:?} omitted={}", samples.omitted);
        samples.stages.clear();
        samples.count = 0;
        samples.omitted = 0;
        self.available = Some(samples);
    }
}

impl StageSamples {
    fn reserve(&mut self, kinds: &[StageKind]) -> Option<usize> {
        let start = self.count;
        if start + kinds.len() * 2 > SAMPLE_COUNT {
            self.omitted += kinds.len();
            return None;
        }
        for kind in kinds {
            self.stages.push(Stage {
                kind: *kind,
                index: self.count,
            });
            self.count += 2;
        }
        Some(start)
    }

    pub(super) fn attach_render(&mut self, descriptor: &MTLRenderPassDescriptor) {
        let Some(index) = self.reserve(&[StageKind::Vertex, StageKind::Fragment]) else {
            return;
        };
        // SAFETY: attachment slot 0 exists; reserve validated all four indices.
        unsafe {
            let attachment = descriptor
                .sampleBufferAttachments()
                .objectAtIndexedSubscript(0);
            attachment.setSampleBuffer(Some(&self.buffer));
            attachment.setStartOfVertexSampleIndex(index);
            attachment.setEndOfVertexSampleIndex(index + 1);
            attachment.setStartOfFragmentSampleIndex(index + 2);
            attachment.setEndOfFragmentSampleIndex(index + 3);
        }
    }

    pub(super) fn attach_compute(&mut self, descriptor: &MTLComputePassDescriptor) {
        let Some(index) = self.reserve(&[StageKind::Compute]) else {
            return;
        };
        // SAFETY: attachment slot 0 exists; reserve validated both indices.
        unsafe {
            let attachment = descriptor
                .sampleBufferAttachments()
                .objectAtIndexedSubscript(0);
            attachment.setSampleBuffer(Some(&self.buffer));
            attachment.setStartOfEncoderSampleIndex(index);
            attachment.setEndOfEncoderSampleIndex(index + 1);
        }
    }

    pub(super) fn attach_blit(&mut self, descriptor: &MTLBlitPassDescriptor) {
        let Some(index) = self.reserve(&[StageKind::Blit]) else {
            return;
        };
        // SAFETY: attachment slot 0 exists; reserve validated both indices.
        unsafe {
            let attachment = descriptor
                .sampleBufferAttachments()
                .objectAtIndexedSubscript(0);
            attachment.setSampleBuffer(Some(&self.buffer));
            attachment.setStartOfEncoderSampleIndex(index);
            attachment.setEndOfEncoderSampleIndex(index + 1);
        }
    }
}

fn sample_delta(bytes: &[u8], index: usize) -> Option<u64> {
    let start = index.checked_mul(8)?;
    let pair = bytes.get(start..start.checked_add(16)?)?;
    let a = u64::from_ne_bytes(pair[..8].try_into().ok()?);
    let b = u64::from_ne_bytes(pair[8..].try_into().ok()?);
    if a == 0 || b == 0 || a == u64::MAX || b == u64::MAX {
        return None;
    }
    b.checked_sub(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_completed_blit_resolves_timestamp_pairs_and_preserves_output() {
        use objc2_metal::{MTLBlitCommandEncoder, MTLCommandBuffer, MTLCommandEncoder};
        let device = super::super::metal_device::MetalDevice::new().unwrap();
        let Some(mut profiler) = MetalGpuProfiler::new(device.device()) else {
            return;
        };
        let mut samples = profiler.acquire().unwrap();
        let output = super::super::metal_buffer::MetalBuffer::new(&device, 4096).unwrap();
        let scheduler = super::super::metal_scheduler::MetalScheduler::new(&device);
        let command_buffer = scheduler.begin().unwrap();
        let descriptor = MTLBlitPassDescriptor::new();
        samples.attach_blit(&descriptor);
        let encoder = command_buffer
            .blitCommandEncoderWithDescriptor(&descriptor)
            .unwrap();
        encoder.fillBuffer_range_value(output.handle(), NSRange::new(0, 4096), 0x5a);
        encoder.endEncoding();
        command_buffer.commit();
        command_buffer.waitUntilCompleted();
        assert_eq!(
            command_buffer.status(),
            objc2_metal::MTLCommandBufferStatus::Completed
        );
        let data = unsafe { samples.buffer.resolveCounterRange(NSRange::new(0, 2)) }.unwrap();
        let delta = sample_delta(unsafe { data.as_bytes_unchecked() }, 0);
        assert!(
            delta.is_some(),
            "completed native blit must supply timestamps"
        );
        let mut bytes = [0u8; 4096];
        output.read(0, &mut bytes).unwrap();
        assert_eq!(bytes, [0x5a; 4096]);
        profiler.completed(1, samples);
    }

    #[test]
    fn single_buffer_lease_is_bounded_and_cannot_be_reacquired_in_flight() {
        let device = super::super::metal_device::MetalDevice::new().unwrap();
        let Some(mut profiler) = MetalGpuProfiler::new(device.device()) else {
            return;
        };
        let mut samples = profiler.acquire().unwrap();
        profiler.last_capture = None;
        assert!(profiler.acquire().is_none());
        for i in 0..SAMPLE_COUNT / 4 {
            assert_eq!(
                samples.reserve(&[StageKind::Vertex, StageKind::Fragment]),
                Some(i * 4)
            );
        }
        assert_eq!(samples.reserve(&[StageKind::Compute]), None);
        assert_eq!(samples.count, SAMPLE_COUNT);
        assert_eq!(samples.omitted, 1);
        // No commands reference this lease, so return it without GPU resolution.
        samples.count = 0;
        samples.stages.clear();
        profiler.completed(0, samples);
        let samples = profiler.acquire().unwrap();
        assert_eq!(samples.count, 0);
        assert_eq!(samples.omitted, 0);
    }

    #[test]
    fn timestamps_reject_unavailable_reversed_and_truncated_results() {
        for (a, b, expected) in [
            (10u64, 27u64, Some(17)),
            (27, 10, None),
            (0, 27, None),
            (10, u64::MAX, None),
            (u64::MAX, u64::MAX, None),
        ] {
            let bytes = [a.to_ne_bytes(), b.to_ne_bytes()].concat();
            assert_eq!(sample_delta(&bytes, 0), expected);
            assert_eq!(sample_delta(&bytes[..15], 0), None);
            assert_eq!(sample_delta(&bytes, usize::MAX), None);
        }
    }
}
