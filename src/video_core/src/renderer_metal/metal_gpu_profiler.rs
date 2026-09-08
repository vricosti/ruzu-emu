// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Optional native stage-boundary timing. No Vulkan query/barrier emulation.
//! Apple: Sampling GPU data into counter sample buffers.

use std::time::{Duration, Instant};
use std::ptr::NonNull;
use std::panic::Location;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSRange;
use objc2_metal::{
    MTLBlitPassDescriptor, MTLCommonCounterSetTimestamp, MTLComputePassDescriptor,
    MTLCounterSampleBuffer, MTLCounterSampleBufferDescriptor, MTLCounterSamplingPoint,
    MTLCounterSet, MTLDevice, MTLRenderPassDescriptor, MTLStorageMode, MTLTexture,
};
use crate::texture_cache::types::NUM_RT;
use super::metal_pipeline_cache::MetalDepthStencilKey;

const SAMPLE_COUNT: usize = 1024;
const SAMPLE_PAGES: usize = 16;
const TOTAL_SAMPLE_COUNT: usize = SAMPLE_COUNT * SAMPLE_PAGES;
const MAX_RENDER_END_SITES: usize = 64;
const DEPTH_ALIAS_KEY_WORDS: usize = 29;

#[derive(Default)]
struct DepthFeedbackSamples {
    // Native mip/slice ranges, texture types, and effective depth/stencil state.
    // Scalar metadata only: profiling must not extend resource lifetimes.
    aliases: Vec<([usize; DEPTH_ALIAS_KEY_WORDS], u64)>,
    omitted: u64,
    checks: u64,
    requested: u64,
}

fn texture_root(texture: &ProtocolObject<dyn MTLTexture>) -> (usize, usize, usize) {
    if let Some(parent) = texture.parentTexture() {
        let (root, level, slice) = texture_root(&parent);
        (root, level + texture.parentRelativeLevel(), slice + texture.parentRelativeSlice())
    } else {
        (texture as *const _ as usize, 0, 0)
    }
}

impl DepthFeedbackSamples {
    fn observe_alias(&mut self, key: [usize; DEPTH_ALIAS_KEY_WORDS]) {
        if let Some((_, count)) = self.aliases.iter_mut().find(|(existing, _)| *existing == key) {
            *count = count.saturating_add(1);
        } else if self.aliases.len() < MAX_RENDER_END_SITES {
            self.aliases.push((key, 1));
        } else {
            self.omitted = self.omitted.saturating_add(1);
        }
    }
}

#[derive(Default)]
struct RenderEndSites {
    sites: Vec<(&'static str, u32, u32, u64)>,
    omitted: u64,
}

impl RenderEndSites {
    fn observe(&mut self, file: &'static str, line: u32, column: u32) {
        if let Some(site) = self.sites.iter_mut().find(|s| (s.0, s.1, s.2) == (file, line, column)) {
            site.3 = site.3.saturating_add(1);
        } else if self.sites.len() < MAX_RENDER_END_SITES {
            self.sites.push((file, line, column, 1));
        } else {
            self.omitted = self.omitted.saturating_add(1);
        }
    }
}

/// Bounded operation counters, not Metal pipeline labels or emulation state.
#[derive(Clone, Copy)]
pub(crate) enum ComputeWork {
    Other,
    Guest,
    GeometryVertex,
    PrimitiveAssembly,
    ConditionalResolve,
    ConditionalArguments,
    VisibilityResolve,
    IndexConversion,
    QuadIndexConversion,
    EligibleUpload,
    OrderedUpload,
    DedicatedUpload,
    BufferCopy,
    BufferDownload,
    BufferClear,
}

pub(super) const COMPUTE_WORK_COUNT: usize = 15;

#[derive(Clone, Copy)]
pub(super) enum RenderHelper {
    DepthStencilBlit,
    ColorBlit,
    Clear,
}

pub(super) struct MetalGpuProfiler {
    available: Option<StageSamples>,
    last_capture: Option<Instant>,
    capture_count: u64,
    batches_to_skip: u64,
}

pub(super) struct StageSamples {
    buffers: Vec<Retained<ProtocolObject<dyn MTLCounterSampleBuffer>>>,
    stages: Vec<Stage>,
    count: usize,
    omitted: usize,
    clock_start: [u64; 2],
    seen: [usize; 4],
    compute_calls: [usize; COMPUTE_WORK_COUNT],
    render_breaks: [usize; COMPUTE_WORK_COUNT],
    render_samples: Vec<RenderSample>,
    current_render: Option<usize>,
    render_ends: RenderEndSites,
    depth_feedback: DepthFeedbackSamples,
}

#[derive(Debug)]
struct RenderSample {
    index: usize,
    // Width, height and native pixel format; no resource is retained by metadata.
    colors: [[usize; 3]; NUM_RT],
    depth: [usize; 3],
    draws: u32,
    helper_draws: [u32; 3],
    first_shaders: [u64; 6],
    last_shaders: [u64; 6],
    mixed_shaders: bool,
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
        let mut buffers = Vec::with_capacity(SAMPLE_PAGES);
        for _ in 0..SAMPLE_PAGES {
            match device.newCounterSampleBufferWithDescriptor_error(&descriptor) {
                Ok(buffer) => buffers.push(buffer),
                Err(error) => {
                    log::warn!("Metal stage profiling unavailable: {error}");
                    return None;
                }
            }
        }
        log::info!(
            "Metal stage profiling enabled: {SAMPLE_PAGES} pages of {SAMPLE_COUNT} samples, at most one batch/s"
        );
        Some(Self {
            available: Some(StageSamples {
                buffers,
                stages: Vec::new(),
                count: 0,
                omitted: 0,
                clock_start: [0; 2],
                seen: [0; 4],
                compute_calls: [0; COMPUTE_WORK_COUNT],
                render_breaks: [0; COMPUTE_WORK_COUNT],
                render_samples: Vec::new(),
                current_render: None,
                render_ends: RenderEndSites::default(),
                depth_feedback: DepthFeedbackSamples::default(),
            }),
            last_capture: None,
            capture_count: 0,
            batches_to_skip: 0,
        })
    }

    pub(super) fn acquire(&mut self) -> Option<StageSamples> {
        if self
            .last_capture
            .is_some_and(|time| time.elapsed() < Duration::from_millis(1000 + (self.capture_count % 7) * 37))
        {
            return None;
        }
        // CPU recording arrives in bursts after GPU waits. A wall-clock-only
        // cadence repeatedly picks their first (often transfer-only) batch.
        // Rotate the batch ordinal too, without ever sampling a partial batch.
        if self.available.is_none() {
            return None;
        }
        if self.batches_to_skip != 0 {
            self.batches_to_skip -= 1;
            return None;
        }
        let mut samples = self.available.take()?;
        samples.clock_start = sample_clocks(&samples.buffers[0].device());
        self.last_capture = Some(Instant::now());
        self.capture_count = self.capture_count.wrapping_add(1);
        self.batches_to_skip = self.capture_count % 17;
        Some(samples)
    }

    /// Caller has already observed successful command-buffer completion. Never
    /// reuse sample indices while their producing command buffer is in flight.
    pub(super) fn completed(&mut self, tick: u64, mut samples: StageSamples, command_ms: Option<f64>) {
        let clock_end = sample_clocks(&samples.buffers[0].device());
        let ns_per_tick = clock_scale(samples.clock_start, clock_end);
        let mut totals = [0u64; 4];
        let mut measured = [0usize; 4];
        let mut worst = [(0u64, 0usize); 4];
        if samples.count != 0 {
            // Missing/short page results remain invalid, never plausible zeroes.
            let mut bytes = vec![0xff; samples.count * 8];
            for (page, buffer) in samples.buffers.iter().enumerate().take(samples.count.div_ceil(SAMPLE_COUNT)) {
                let count = (samples.count - page * SAMPLE_COUNT).min(SAMPLE_COUNT);
                // SAFETY: page-local indices are bounded; GPU already completed.
                if let Some(data) = unsafe { buffer.resolveCounterRange(NSRange::new(0, count)) } {
                    let resolved = unsafe { data.as_bytes_unchecked() };
                    if resolved.len() == count * 8 {
                        let offset = page * SAMPLE_COUNT * 8;
                        bytes[offset..offset + resolved.len()].copy_from_slice(resolved);
                    }
                }
            }
            {
                let bytes = &bytes[..];
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
                let mut ranked: Vec<_> = samples.render_samples.iter().filter_map(|pass| {
                    let vertex = sample_delta(bytes, pass.index)?;
                    let fragment = sample_delta(bytes, pass.index + 2)?;
                    Some((vertex.saturating_add(fragment), vertex, fragment, pass))
                }).collect();
                ranked.sort_unstable_by_key(|entry| std::cmp::Reverse(entry.0));
                // Stages can overlap: this ranks sampled cost, not wall time.
                for (_, vertex, fragment, pass) in ranked.into_iter().take(8) {
                    log::info!("[METAL_RENDER_PASS_TIME] tick={tick} sample={} vertex_ticks={vertex} fragment_ticks={fragment} vertex_ms={:?} fragment_ms={:?} draws={} first_shaders={:X?} last_shaders={:X?} mixed={} colors={:?} depth={:?} helper_order=depth_stencil_blit,color_blit,clear helper_draws={:?}",
                        pass.index, ns_per_tick.map(|scale| vertex as f64 * scale / 1e6),
                        ns_per_tick.map(|scale| fragment as f64 * scale / 1e6),
                        pass.draws, pass.first_shaders, pass.last_shaders,
                        pass.mixed_shaders, pass.colors, pass.depth, pass.helper_draws);
                }
                let intervals = samples.stages.iter().filter_map(|stage| sample_pair(bytes, stage.index)).collect();
                let (covered, span, max_gap) = interval_coverage(intervals);
                log::info!("[METAL_BATCH_COVERAGE] tick={tick} command_ms={command_ms:?} ns_per_tick={ns_per_tick:?} covered_ms={:?} span_ms={:?} max_gap_ms={:?} stage_ms={:?} pages={} complete={}",
                    ns_per_tick.map(|s| covered as f64 * s / 1e6),
                    ns_per_tick.map(|s| span as f64 * s / 1e6),
                    ns_per_tick.map(|s| max_gap as f64 * s / 1e6),
                    ns_per_tick.map(|s| totals.map(|t| t as f64 * s / 1e6)),
                    samples.count.div_ceil(SAMPLE_COUNT), samples.omitted == 0 && measured == samples.seen);
            }
        }
        // Raw GPU clock ticks, not nanoseconds or utilization. Stages may overlap.
        log::info!("[METAL_STAGE_TIME] tick={tick} order=blit,compute,vertex,fragment seen={:?} measured={measured:?} gpu_ticks={totals:?} worst_ticks_sample={worst:?} omitted={}", samples.seen, samples.omitted);
        log::info!("[METAL_COMPUTE_WORK] tick={tick} order=other,guest,geometry_vertex,primitive_assembly,conditional_resolve,conditional_arguments,visibility_resolve,uint8_conversion,quad_index_conversion,eligible_upload,ordered_upload,dedicated_upload,buffer_copy,buffer_download,buffer_clear calls={:?} render_breaks={:?}", samples.compute_calls, samples.render_breaks);
        samples.stages.clear();
        for (file, line, column, count) in &samples.render_ends.sites {
            log::info!("[METAL_RENDER_END_SITE] tick={tick} file={file} line={line} column={column} count={count}");
        }
        log::info!("[METAL_RENDER_END_COVERAGE] tick={tick} omitted={}", samples.render_ends.omitted);
        samples.render_ends.sites.clear();
        samples.render_ends.omitted = 0;
        for (key, count) in &samples.depth_feedback.aliases {
            log::info!("[METAL_DEPTH_ALIAS] tick={tick} order=sample_level,mips,slice,array_length,type,depth_level,slice,render_layers,type,compare,write,stencil,barrier,stage,slot,shader_hash,pixel_format,front_compare,front_fail,front_depth_fail,front_pass,front_read_mask,front_write_mask,back_compare,back_fail,back_depth_fail,back_pass,back_read_mask,back_write_mask values={key:?} count={count}");
        }
        log::info!("[METAL_DEPTH_ALIAS_COVERAGE] tick={tick} checks={} requested={} omitted={}", samples.depth_feedback.checks, samples.depth_feedback.requested, samples.depth_feedback.omitted);
        samples.depth_feedback = DepthFeedbackSamples::default();
        samples.count = 0;
        samples.omitted = 0;
        samples.seen = [0; 4];
        samples.compute_calls = [0; COMPUTE_WORK_COUNT];
        samples.render_breaks = [0; COMPUTE_WORK_COUNT];
        samples.render_samples.clear();
        samples.current_render = None;
        self.available = Some(samples);
    }
}

impl StageSamples {
    pub(super) fn observe_depth_feedback<'a>(
        &mut self,
        descriptor: &MTLRenderPassDescriptor,
        requested: bool,
        state: &MetalDepthStencilKey,
        textures: impl Iterator<Item = (usize, u32, u64, &'a ProtocolObject<dyn MTLTexture>)>,
    ) {
        self.depth_feedback.checks += 1;
        self.depth_feedback.requested += u64::from(requested);
        let attachment = descriptor.depthAttachment();
        let Some(depth) = attachment.texture() else { return };
        let (depth_root, depth_level, depth_slice) = texture_root(&depth);
        for (stage, slot, shader_hash, texture) in textures {
            let (root, level, slice) = texture_root(texture);
            if root != depth_root { continue; }
            self.depth_feedback.observe_alias([
                level, texture.mipmapLevelCount(), slice, texture.arrayLength(), texture.textureType().0,
                depth_level + attachment.level(), depth_slice + attachment.slice(),
                descriptor.renderTargetArrayLength(), depth.textureType().0,
                state.depth_compare.0, usize::from(state.depth_write_enabled), usize::from(state.stencil_enabled), usize::from(requested),
                stage, slot as usize, shader_hash as usize, texture.pixelFormat().0,
                state.front.compare.0, state.front.stencil_fail.0, state.front.depth_fail.0,
                state.front.depth_stencil_pass.0, state.front.read_mask as usize, state.front.write_mask as usize,
                state.back.compare.0, state.back.stencil_fail.0, state.back.depth_fail.0,
                state.back.depth_stencil_pass.0, state.back.read_mask as usize, state.back.write_mask as usize,
            ]);
        }
    }

    #[track_caller]
    pub(super) fn observe_render_end(&mut self) {
        let site = Location::caller();
        self.render_ends.observe(site.file(), site.line(), site.column());
    }

    #[cfg(test)]
    pub(super) fn render_end_count(&self) -> u64 {
        self.render_ends.sites.iter().map(|s| s.3).sum::<u64>() + self.render_ends.omitted
    }

    pub(super) fn observe_graphics_draw(&mut self, shaders: [u64; 6]) {
        let Some(index) = self.current_render else { return };
        let pass = &mut self.render_samples[index];
        if pass.draws == 0 {
            pass.first_shaders = shaders;
        } else {
            pass.mixed_shaders |= pass.first_shaders != shaders;
        }
        pass.last_shaders = shaders;
        pass.draws = pass.draws.saturating_add(1);
    }

    pub(super) fn observe_helper_draw(&mut self, work: RenderHelper) {
        let Some(index) = self.current_render else { return };
        let count = &mut self.render_samples[index].helper_draws[work as usize];
        *count = count.saturating_add(1);
    }

    pub(super) fn observe_compute(&mut self, work: ComputeWork, ended_render: bool) {
        self.compute_calls[work as usize] += 1;
        self.render_breaks[work as usize] += usize::from(ended_render);
    }

    pub(super) fn observe_render_break(&mut self, work: ComputeWork) {
        self.render_breaks[work as usize] += 1;
    }

    #[cfg(test)]
    pub(super) fn work_counts(&self, work: ComputeWork) -> (usize, usize) {
        (self.compute_calls[work as usize], self.render_breaks[work as usize])
    }

    fn reserve(&mut self, kinds: &[StageKind]) -> Option<usize> {
        for kind in kinds {
            self.seen[*kind as usize] += 1;
        }
        let needed = kinds.len() * 2;
        let start = if self.count % SAMPLE_COUNT + needed > SAMPLE_COUNT {
            self.count.next_multiple_of(SAMPLE_COUNT)
        } else { self.count };
        if start + needed > TOTAL_SAMPLE_COUNT {
            self.omitted += kinds.len();
            return None;
        }
        self.count = start;
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
        self.current_render = None;
        let Some(index) = self.reserve(&[StageKind::Vertex, StageKind::Fragment]) else {
            return;
        };
        let describe = |texture: Option<Retained<ProtocolObject<dyn MTLTexture>>>| {
            texture.map_or([0; 3], |texture| [texture.width(), texture.height(), texture.pixelFormat().0])
        };
        let colors = std::array::from_fn(|i| {
            describe(unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(i) }.texture())
        });
        let depth = describe(descriptor.depthAttachment().texture());
        self.current_render = Some(self.render_samples.len());
        self.render_samples.push(RenderSample {
            index, colors, depth, draws: 0, helper_draws: [0; 3], first_shaders: [0; 6],
            last_shaders: [0; 6], mixed_shaders: false,
        });
        // SAFETY: attachment slot 0 exists; reserve validated all four indices.
        unsafe {
            let attachment = descriptor
                .sampleBufferAttachments()
                .objectAtIndexedSubscript(0);
            attachment.setSampleBuffer(Some(&self.buffers[index / SAMPLE_COUNT]));
            let index = index % SAMPLE_COUNT;
            attachment.setStartOfVertexSampleIndex(index);
            attachment.setEndOfVertexSampleIndex(index + 1);
            attachment.setStartOfFragmentSampleIndex(index + 2);
            attachment.setEndOfFragmentSampleIndex(index + 3);
        }
    }

    pub(super) fn attach_compute(&mut self, descriptor: &MTLComputePassDescriptor) {
        self.current_render = None;
        let Some(index) = self.reserve(&[StageKind::Compute]) else {
            return;
        };
        // SAFETY: attachment slot 0 exists; reserve validated both indices.
        unsafe {
            let attachment = descriptor
                .sampleBufferAttachments()
                .objectAtIndexedSubscript(0);
            attachment.setSampleBuffer(Some(&self.buffers[index / SAMPLE_COUNT]));
            let index = index % SAMPLE_COUNT;
            attachment.setStartOfEncoderSampleIndex(index);
            attachment.setEndOfEncoderSampleIndex(index + 1);
        }
    }

    pub(super) fn attach_blit(&mut self, descriptor: &MTLBlitPassDescriptor) {
        self.current_render = None;
        let Some(index) = self.reserve(&[StageKind::Blit]) else {
            return;
        };
        // SAFETY: attachment slot 0 exists; reserve validated both indices.
        unsafe {
            let attachment = descriptor
                .sampleBufferAttachments()
                .objectAtIndexedSubscript(0);
            attachment.setSampleBuffer(Some(&self.buffers[index / SAMPLE_COUNT]));
            let index = index % SAMPLE_COUNT;
            attachment.setStartOfEncoderSampleIndex(index);
            attachment.setEndOfEncoderSampleIndex(index + 1);
        }
    }
}

fn sample_clocks(device: &ProtocolObject<dyn MTLDevice>) -> [u64; 2] {
    let (mut cpu, mut gpu) = (0, 0);
    // Apple sampleTimestamps returns CPU nanoseconds, not mach_absolute_time ticks.
    // Only called before/after the sampled batch, never for each encoder.
    unsafe { device.sampleTimestamps_gpuTimestamp(NonNull::from(&mut cpu), NonNull::from(&mut gpu)) };
    [cpu, gpu]
}

fn clock_scale(start: [u64; 2], end: [u64; 2]) -> Option<f64> {
    let cpu = end[0].checked_sub(start[0])?;
    let gpu = end[1].checked_sub(start[1])?;
    (cpu != 0 && gpu != 0).then(|| cpu as f64 / gpu as f64)
}

fn interval_coverage(mut intervals: Vec<(u64, u64)>) -> (u64, u64, u64) {
    intervals.sort_unstable();
    let Some(&(first, mut end)) = intervals.first() else { return (0, 0, 0) };
    let mut covered = end - first;
    let mut max_gap = 0;
    for &(a, b) in &intervals[1..] {
        if a > end {
            max_gap = max_gap.max(a - end);
            covered += b - a;
        } else if b > end {
            covered += b - end;
        }
        end = end.max(b);
    }
    (covered, end - first, max_gap)
}

fn sample_delta(bytes: &[u8], index: usize) -> Option<u64> {
    sample_pair(bytes, index).map(|(a, b)| b - a)
}

fn sample_pair(bytes: &[u8], index: usize) -> Option<(u64, u64)> {
    let start = index.checked_mul(8)?;
    let pair = bytes.get(start..start.checked_add(16)?)?;
    let a = u64::from_ne_bytes(pair[..8].try_into().ok()?);
    let b = u64::from_ne_bytes(pair[8..].try_into().ok()?);
    if a == 0 || b == 0 || a == u64::MAX || b == u64::MAX {
        return None;
    }
    (b >= a).then_some((a, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_metadata_tracks_mixed_draws_and_stops_at_sample_budget() {
        let device = super::super::metal_device::MetalDevice::new().unwrap();
        let Some(mut profiler) = MetalGpuProfiler::new(device.device()) else { return };
        let mut samples = profiler.acquire().unwrap();
        let descriptor = MTLRenderPassDescriptor::new();
        samples.attach_render(&descriptor);
        samples.observe_graphics_draw([1; 6]);
        samples.observe_graphics_draw([2; 6]);
        samples.observe_graphics_draw([1; 6]);
        samples.observe_helper_draw(RenderHelper::Clear);
        samples.observe_helper_draw(RenderHelper::Clear);
        samples.observe_helper_draw(RenderHelper::ColorBlit);
        samples.observe_helper_draw(RenderHelper::DepthStencilBlit);
        let pass = &samples.render_samples[0];
        assert_eq!(pass.draws, 3);
        assert_eq!(pass.helper_draws, [1, 1, 2]);
        assert_eq!(pass.first_shaders, [1; 6]);
        assert_eq!(pass.last_shaders, [1; 6]);
        assert!(pass.mixed_shaders);
        assert_eq!(pass.colors, [[0; 3]; NUM_RT]);
        samples.attach_compute(&MTLComputePassDescriptor::new());
        samples.observe_graphics_draw([3; 6]);
        samples.observe_helper_draw(RenderHelper::Clear);
        assert_eq!(samples.render_samples[0].draws, 3);
        assert_eq!(samples.render_samples[0].helper_draws, [1, 1, 2]);
        for _ in 0..TOTAL_SAMPLE_COUNT {
            samples.attach_render(&descriptor);
            samples.observe_graphics_draw([4; 6]);
        }
        assert!(samples.render_samples.len() <= TOTAL_SAMPLE_COUNT / 4);
        assert!(samples.current_render.is_none());
        assert_eq!(samples.render_samples.last().unwrap().draws, 1);
        samples.observe_helper_draw(RenderHelper::Clear);
        assert_eq!(samples.render_samples.last().unwrap().helper_draws, [0; 3]);
        samples.count = 0;
        profiler.completed(1, samples, None);
        let returned = profiler.available.as_ref().unwrap();
        assert!(returned.render_samples.is_empty());
        assert!(returned.current_render.is_none());
    }

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
        // Exercise a second native page; the first encoder still references page 0.
        samples.count = SAMPLE_COUNT;
        let descriptor = MTLBlitPassDescriptor::new();
        samples.attach_blit(&descriptor);
        let encoder = command_buffer.blitCommandEncoderWithDescriptor(&descriptor).unwrap();
        encoder.fillBuffer_range_value(output.handle(), NSRange::new(16, 16), 0x3c);
        encoder.endEncoding();
        command_buffer.commit();
        command_buffer.waitUntilCompleted();
        assert_eq!(
            command_buffer.status(),
            objc2_metal::MTLCommandBufferStatus::Completed
        );
        let data = unsafe { samples.buffers[0].resolveCounterRange(NSRange::new(0, 2)) }.unwrap();
        let delta = sample_delta(unsafe { data.as_bytes_unchecked() }, 0);
        assert!(
            delta.is_some(),
            "completed native blit must supply timestamps"
        );
        let mut bytes = [0u8; 4096];
        output.read(0, &mut bytes).unwrap();
        let mut expected = [0x5a; 4096];
        expected[16..32].fill(0x3c);
        assert_eq!(bytes, expected);
        let data = unsafe { samples.buffers[1].resolveCounterRange(NSRange::new(0, 2)) }.unwrap();
        assert!(sample_delta(unsafe { data.as_bytes_unchecked() }, 0).is_some());
        assert!(clock_scale(samples.clock_start, sample_clocks(device.device())).is_some());
        profiler.completed(1, samples, Some((command_buffer.GPUEndTime() - command_buffer.GPUStartTime()) * 1000.0));
    }

    #[test]
    fn paged_lease_is_bounded_and_cannot_be_reacquired_in_flight() {
        let device = super::super::metal_device::MetalDevice::new().unwrap();
        let Some(mut profiler) = MetalGpuProfiler::new(device.device()) else {
            return;
        };
        let mut samples = profiler.acquire().unwrap();
        profiler.last_capture = None;
        assert!(profiler.acquire().is_none());
        for i in 0..TOTAL_SAMPLE_COUNT / 4 {
            assert_eq!(
                samples.reserve(&[StageKind::Vertex, StageKind::Fragment]),
                Some(i * 4)
            );
        }
        assert_eq!(samples.reserve(&[StageKind::Compute]), None);
        assert_eq!(samples.count, TOTAL_SAMPLE_COUNT);
        assert_eq!(samples.omitted, 1);
        // No commands reference this lease, so return it without GPU resolution.
        samples.count = 0;
        samples.stages.clear();
        profiler.completed(0, samples, None);
        assert!(profiler.acquire().is_none());
        let samples = profiler.acquire().unwrap();
        assert_eq!(samples.count, 0);
        assert_eq!(samples.omitted, 0);
    }

    #[test]
    fn batch_selection_varies_ordinal_without_consuming_skips_in_flight() {
        let device = super::super::metal_device::MetalDevice::new().unwrap();
        let Some(mut profiler) = MetalGpuProfiler::new(device.device()) else { return };
        for capture in 0..20 {
            profiler.last_capture = None;
            for remaining in (0..capture % 17).rev() {
                assert!(profiler.acquire().is_none());
                assert_eq!(profiler.batches_to_skip, remaining);
            }
            let samples = profiler.acquire().unwrap();
            profiler.last_capture = None;
            let pending = profiler.batches_to_skip;
            assert!(profiler.acquire().is_none());
            assert_eq!(profiler.batches_to_skip, pending);
            // Empty lease: no GPU commands reference these sample buffers.
            profiler.completed(capture, samples, None);
        }
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

    #[test]
    fn clock_calibration_and_coverage_do_not_double_count_overlap() {
        assert_eq!(clock_scale([100, 50], [300, 60]), Some(20.0));
        assert_eq!(clock_scale([100, 50], [100, 60]), None);
        assert_eq!(clock_scale([100, 50], [300, 50]), None);
        assert_eq!(clock_scale([100, 50], [99, 60]), None);
        assert_eq!(clock_scale([100, 50], [300, 49]), None);
        assert_eq!(interval_coverage(vec![(30, 50), (10, 20), (15, 18), (18, 25)]), (35, 40, 5));
        assert_eq!(interval_coverage(vec![]), (0, 0, 0));
    }

    #[test]
    fn depth_alias_samples_are_bounded_and_keep_state_distinct() {
        let mut samples = DepthFeedbackSamples::default();
        for level in 0..MAX_RENDER_END_SITES {
            let mut key = [0; DEPTH_ALIAS_KEY_WORDS];
            key[0] = level;
            samples.observe_alias(key);
        }
        samples.observe_alias([0; DEPTH_ALIAS_KEY_WORDS]);
        let mut writable = [0; DEPTH_ALIAS_KEY_WORDS];
        writable[10] = 1;
        samples.observe_alias(writable);
        assert_eq!(samples.aliases.len(), MAX_RENDER_END_SITES);
        assert_eq!(samples.aliases[0].1, 2);
        assert_eq!(samples.omitted, 1);
    }

    #[test]
    fn depth_alias_samples_distinguish_descriptor_and_stencil_details() {
        let mut samples = DepthFeedbackSamples::default();
        samples.observe_alias([0; DEPTH_ALIAS_KEY_WORDS]);
        for index in 13..DEPTH_ALIAS_KEY_WORDS {
            let mut key = [0; DEPTH_ALIAS_KEY_WORDS];
            key[index] = 1;
            samples.observe_alias(key);
            samples.observe_alias(key);
        }
        assert_eq!(samples.aliases.len(), 17);
        assert_eq!(samples.aliases[0].1, 1);
        assert!(samples.aliases[1..].iter().all(|(_, count)| *count == 2));
        assert_eq!(samples.omitted, 0);
    }

    #[test]
    fn render_end_sites_are_bounded_and_keep_counting_existing_sites() {
        let mut sites = RenderEndSites::default();
        for line in 0..MAX_RENDER_END_SITES as u32 {
            sites.observe("source.rs", line, 1);
        }
        sites.observe("source.rs", 0, 1);
        sites.observe("source.rs", 0, 2);
        sites.observe("other.rs", 0, 1);
        assert_eq!(sites.sites.len(), MAX_RENDER_END_SITES);
        assert_eq!(sites.sites[0].3, 2);
        assert_eq!(sites.omitted, 2);
    }

    #[test]
    fn paged_samples_preserve_encoder_pairs_and_count_overflow() {
        let device = super::super::metal_device::MetalDevice::new().unwrap();
        let Some(mut profiler) = MetalGpuProfiler::new(device.device()) else { return };
        let mut samples = profiler.acquire().unwrap();
        for i in 0..SAMPLE_COUNT / 2 - 1 {
            assert_eq!(samples.reserve(&[StageKind::Compute]), Some(i * 2));
            samples.observe_compute(ComputeWork::ConditionalArguments, i % 2 == 0);
        }
        // Four render timestamps cannot straddle two counter buffers.
        assert_eq!(samples.reserve(&[StageKind::Vertex, StageKind::Fragment]), Some(SAMPLE_COUNT));
        assert_eq!(samples.count, SAMPLE_COUNT + 4);
        while samples.count < TOTAL_SAMPLE_COUNT {
            assert!(samples.reserve(&[StageKind::Compute]).is_some());
        }
        assert_eq!(samples.reserve(&[StageKind::Blit]), None);
        assert_eq!(samples.count, TOTAL_SAMPLE_COUNT);
        assert_eq!(samples.omitted, 1);
        assert_eq!(samples.seen[0], 1);
        assert_eq!(samples.compute_calls[ComputeWork::ConditionalArguments as usize], 511);
        assert_eq!(samples.render_breaks[ComputeWork::ConditionalArguments as usize], 256);
        samples.observe_compute(ComputeWork::IndexConversion, true);
        samples.observe_compute(ComputeWork::QuadIndexConversion, false);
        samples.observe_compute(ComputeWork::QuadIndexConversion, true);
        assert_eq!(samples.work_counts(ComputeWork::IndexConversion), (1, 1));
        assert_eq!(samples.work_counts(ComputeWork::QuadIndexConversion), (2, 1));
        samples.observe_render_break(ComputeWork::EligibleUpload);
        assert_eq!(samples.work_counts(ComputeWork::EligibleUpload), (0, 1));
        for work in [ComputeWork::OrderedUpload, ComputeWork::DedicatedUpload,
            ComputeWork::BufferCopy, ComputeWork::BufferDownload, ComputeWork::BufferClear] {
            samples.observe_render_break(work);
            assert_eq!(samples.work_counts(work), (0, 1));
        }
        assert_eq!(samples.work_counts(ComputeWork::Other), (0, 0));
    }
}
