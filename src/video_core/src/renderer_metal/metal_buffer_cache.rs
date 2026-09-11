// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Metal counterpart of Eden's `renderer_vulkan/vk_buffer_cache.{h,cpp}`.

use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use objc2_metal::MTLDevice as _;

use common::slot_vector::SlotVector;
use objc2_metal::MTLIndexType;

use crate::buffer_cache::buffer_base::{BufferBase, NullBufferParams};
use crate::buffer_cache::buffer_cache::BufferCache as CommonBufferCache;
use crate::buffer_cache::buffer_cache_base::{
    self as base, BufferCacheAsyncBuffer, BufferCacheBuffer, BufferCopy, HostBindings,
};
use crate::buffer_cache::usage_tracker::UsageTracker;
use crate::engines::maxwell_3d::{IndexFormat, PrimitiveTopology, MAX_CONST_BUFFER_SIZE};
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use crate::surface::PixelFormat;

use super::metal_buffer::MetalBuffer;
use super::metal_compute_pass::{QuadIndexedPass, Uint8Pass};
use super::metal_device::MetalDevice;
use super::metal_gpu_profiler::ComputeWork;
use super::metal_scheduler::MetalScheduler;
use super::metal_staging_buffer_pool::{MetalStagingBufferPool, StagingBufferRef};

// Native-only conversion storage; entries own allocations, never staging leases.
const MAX_CACHED_UINT8_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CACHED_UINT8_ENTRIES: u64 = 4096;

struct Uint8CacheProfile {
    last_report: Instant,
    requests: u64,
    hits: u64,
    uncacheable: u64,
    invalidated_entries: u64,
    invalidated_requested_range: u64,
    inserted: u64,
    fallback: u64,
}

impl Default for Uint8CacheProfile {
    fn default() -> Self {
        Self {
            last_report: Instant::now(),
            requests: 0,
            hits: 0,
            uncacheable: 0,
            invalidated_entries: 0,
            invalidated_requested_range: 0,
            inserted: 0,
            fallback: 0,
        }
    }
}

#[derive(Clone)]
pub struct MetalBufferBinding {
    pub buffer: Arc<MetalBuffer>,
    pub offset: usize,
    pub size: usize,
    pub is_written: bool,
}

#[derive(Clone)]
pub struct MetalVertexBinding {
    pub buffer: Arc<MetalBuffer>,
    pub offset: usize,
    pub size: usize,
    pub stride: usize,
}

#[derive(Clone)]
pub struct MetalTexelBufferBinding {
    pub buffer: Arc<MetalBuffer>,
    pub offset: usize,
    pub size: usize,
    pub format: PixelFormat,
}

#[derive(Clone)]
pub struct MetalIndexBinding {
    pub buffer: Arc<MetalBuffer>,
    pub offset: usize,
    pub index_type: MTLIndexType,
}

#[derive(Clone, Default)]
pub struct MetalGraphicsBufferBindings {
    pub uniform_buffers: [Vec<MetalBufferBinding>; base::NUM_STAGES as usize],
    pub storage_buffers: [Vec<MetalBufferBinding>; base::NUM_STAGES as usize],
    pub texture_buffers: Vec<MetalTexelBufferBinding>,
    pub image_buffers: Vec<MetalTexelBufferBinding>,
}

#[derive(Clone, Default)]
pub struct MetalComputeBufferBindings {
    pub uniform_buffers: Vec<MetalBufferBinding>,
    pub storage_buffers: Vec<MetalBufferBinding>,
    pub texture_buffers: Vec<MetalTexelBufferBinding>,
    pub image_buffers: Vec<MetalTexelBufferBinding>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum BindingTarget {
    #[default]
    Graphics,
    Compute,
}

/// Metal buffer object selected by the common cache specialization.
pub struct Buffer {
    base: BufferBase,
    allocation: Arc<MetalBuffer>,
    scheduler: NonNull<MetalScheduler>,
    tracker: UsageTracker,
    last_usage_tick: u64,
    allocation_bytes: u64,
    allocated_bytes: Arc<AtomicU64>,
    uint8_indices: HashMap<(u32, u32), Arc<MetalBuffer>>,
    uint8_generation: u64,
    uint8_bytes: u64,
    cached_index_bytes: Arc<AtomicU64>,
    cached_index_entries: Arc<AtomicU64>,
}

impl Buffer {
    fn null(runtime: &mut BufferCacheRuntime) -> Self {
        // MSL binds constant uint4*, including indirect CBUF reads. A four-byte
        // Vulkan-style null index buffer cannot satisfy that native contract.
        let buffer = Self::allocate(runtime, BufferBase::null(NullBufferParams),
            MAX_CONST_BUFFER_SIZE as u64);
        buffer.allocation.write(0, &vec![0; MAX_CONST_BUFFER_SIZE])
            .expect("initialize Metal null buffer");
        buffer
    }

    fn new(runtime: &mut BufferCacheRuntime, cpu_addr: u64, size_bytes: u64) -> Self {
        Self::allocate(
            runtime,
            BufferBase::new(cpu_addr, size_bytes),
            size_bytes.max(4),
        )
    }

    fn allocate(runtime: &mut BufferCacheRuntime, base: BufferBase, size: u64) -> Self {
        let allocation = Arc::new(
            MetalBuffer::new(&runtime.device, size as usize)
                .expect("Metal buffer-cache allocation failed"),
        );
        runtime.allocated_bytes.fetch_add(size, Ordering::Relaxed);
        Self {
            base,
            allocation,
            scheduler: runtime.scheduler,
            tracker: UsageTracker::new(size.max(4096) as usize),
            last_usage_tick: 0,
            allocation_bytes: size,
            allocated_bytes: Arc::clone(&runtime.allocated_bytes),
            uint8_indices: HashMap::new(),
            uint8_generation: 0,
            uint8_bytes: 0,
            cached_index_bytes: Arc::clone(&runtime.cached_index_bytes),
            cached_index_entries: Arc::clone(&runtime.cached_index_entries),
        }
    }

    pub fn handle(&self) -> Arc<MetalBuffer> {
        Arc::clone(&self.allocation)
    }

    fn clear_uint8_indices(&mut self) {
        self.cached_index_bytes.fetch_sub(self.uint8_bytes, Ordering::Relaxed);
        self.allocated_bytes.fetch_sub(self.uint8_bytes, Ordering::Relaxed);
        self.cached_index_entries.fetch_sub(self.uint8_indices.len() as u64, Ordering::Relaxed);
        self.uint8_bytes = 0;
        self.uint8_indices.clear();
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        self.clear_uint8_indices();
        self.allocated_bytes
            .fetch_sub(self.allocation_bytes, Ordering::Relaxed);
    }
}

impl Deref for Buffer {
    type Target = BufferBase;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl DerefMut for Buffer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

impl BufferCacheBuffer for Buffer {
    type Runtime = BufferCacheRuntime;

    fn set_write_tick(&mut self, tick: u64) {
        self.base.set_write_tick(tick);
        self.allocation.mark_content_modified();
    }

    fn mark_written_region(&mut self, tick: u64, offset: u64, size: u64) {
        self.base.set_write_tick(tick);
        self.allocation.mark_content_range_modified(offset, size);
    }

    fn null(runtime: &mut Self::Runtime, _params: NullBufferParams) -> Self {
        Self::null(runtime)
    }

    fn new(
        runtime: &mut Self::Runtime,
        cpu_addr: u64,
        size_bytes: u64,
        _sparse_compatible: bool,
    ) -> Self {
        Self::new(runtime, cpu_addr, size_bytes)
    }

    fn immediate_upload(&self, offset: u64, data: &[u8]) {
        self.allocation
            .write(offset as usize, data)
            .expect("Metal immediate buffer upload exceeded its allocation");
    }

    fn immediate_download(&self, offset: u64, data: &mut [u8]) {
        self.allocation
            .read(offset as usize, data)
            .expect("Metal immediate buffer download exceeded its allocation");
    }

    fn raw_handle(&self) -> u64 {
        self.allocation.raw_handle()
    }

    fn mark_usage(&mut self, offset: u64, size: u64) {
        self.tracker.track(offset, size);
        self.last_usage_tick = unsafe { self.scheduler.as_ref() }.current_tick();
    }

    fn is_region_used(&self, offset: u64, size: u64) -> bool {
        self.tracker.is_used(offset, size)
    }

    fn reset_usage_tracking(&mut self) {
        self.tracker.reset();
    }

    fn last_usage_tick(&self) -> u64 {
        self.last_usage_tick
    }
}

pub struct BufferCacheParams;

impl base::BufferCacheParams for BufferCacheParams {
    type Runtime = BufferCacheRuntime;
    type Buffer = Buffer;
    type AsyncBuffer = StagingBufferRef;

    const IS_OPENGL: bool = false;
    const HAS_PERSISTENT_UNIFORM_BUFFER_BINDINGS: bool = false;
    const HAS_FULL_INDEX_AND_PRIMITIVE_SUPPORT: bool = false;
    const NEEDS_BIND_UNIFORM_INDEX: bool = false;
    const NEEDS_BIND_STORAGE_INDEX: bool = false;
    const USE_MEMORY_MAPS: bool = true;
    const SEPARATE_IMAGE_BUFFER_BINDINGS: bool = true;
    const USE_MEMORY_MAPS_FOR_UPLOADS: bool = true;
}

pub type MetalCommonBufferCache = CommonBufferCache<BufferCacheParams, MaxwellDeviceMemoryManager>;

/// Backend service owner corresponding to Eden's `Vulkan::BufferCacheRuntime`.
pub struct BufferCacheRuntime {
    device: MetalDevice,
    scheduler: NonNull<MetalScheduler>,
    staging_pool: NonNull<MetalStagingBufferPool>,
    allocated_bytes: Arc<AtomicU64>,
    cached_index_bytes: Arc<AtomicU64>,
    cached_index_entries: Arc<AtomicU64>,
    null_buffer: Arc<MetalBuffer>,
    uint8_profile: Option<Uint8CacheProfile>,
    index_binding: Option<MetalIndexBinding>,
    vertex_bindings: Vec<Option<MetalVertexBinding>>,
    transform_feedback_bindings: Vec<MetalBufferBinding>,
    binding_target: BindingTarget,
    graphics: MetalGraphicsBufferBindings,
    compute: MetalComputeBufferBindings,
    quad_array_index_buffer: Option<(Arc<MetalBuffer>, u32)>,
    quad_strip_index_buffer: Option<(Arc<MetalBuffer>, u32)>,
    uint8_pass: Option<Uint8Pass>,
    quad_index_pass: Option<QuadIndexedPass>,
}

impl BufferCacheRuntime {
    pub fn new(
        device: &MetalDevice,
        scheduler: &mut MetalScheduler,
        staging_pool: &mut MetalStagingBufferPool,
    ) -> Self {
        let null_buffer = Arc::new(MetalBuffer::new(device, MAX_CONST_BUFFER_SIZE)
            .expect("Metal null buffer"));
        null_buffer.write(0, &vec![0; MAX_CONST_BUFFER_SIZE])
            .expect("initialize Metal null buffer");
        Self {
            device: device.clone(),
            scheduler: NonNull::from(scheduler),
            staging_pool: NonNull::from(staging_pool),
            allocated_bytes: Arc::new(AtomicU64::new(0)),
            cached_index_bytes: Arc::new(AtomicU64::new(0)),
            cached_index_entries: Arc::new(AtomicU64::new(0)),
            null_buffer,
            uint8_profile: std::env::var_os("RUZU_PROFILE_METAL_SUBMISSIONS").is_some()
                .then(|| Uint8CacheProfile {
                    ..Default::default()
                }),
            index_binding: None,
            vertex_bindings: vec![None; base::NUM_VERTEX_BUFFERS as usize],
            transform_feedback_bindings: Vec::new(),
            binding_target: BindingTarget::Graphics,
            graphics: MetalGraphicsBufferBindings::default(),
            compute: MetalComputeBufferBindings::default(),
            quad_array_index_buffer: None,
            quad_strip_index_buffer: None,
            uint8_pass: None,
            quad_index_pass: None,
        }
    }

    fn scheduler(&mut self) -> &mut MetalScheduler {
        unsafe { self.scheduler.as_mut() }
    }

    fn staging_pool(&mut self) -> &mut MetalStagingBufferPool {
        unsafe { self.staging_pool.as_mut() }
    }

    pub fn begin_graphics_bindings(&mut self) {
        self.binding_target = BindingTarget::Graphics;
        self.graphics = MetalGraphicsBufferBindings::default();
        self.transform_feedback_bindings.clear();
    }

    pub fn begin_compute_bindings(&mut self) {
        self.binding_target = BindingTarget::Compute;
        self.compute = MetalComputeBufferBindings::default();
    }

    pub fn index_binding(&self) -> Option<&MetalIndexBinding> {
        self.index_binding.as_ref()
    }

    pub fn vertex_bindings(&self) -> &[Option<MetalVertexBinding>] {
        &self.vertex_bindings
    }

    pub fn graphics_bindings(&self) -> &MetalGraphicsBufferBindings {
        &self.graphics
    }

    pub fn compute_bindings(&self) -> &MetalComputeBufferBindings {
        &self.compute
    }

    pub fn null_buffer(&self) -> Arc<MetalBuffer> {
        Arc::clone(&self.null_buffer)
    }

    fn bind_buffer(
        buffer: &Buffer,
        offset: u32,
        size: u32,
        is_written: bool,
    ) -> MetalBufferBinding {
        MetalBufferBinding {
            buffer: buffer.handle(),
            offset: offset as usize,
            size: size as usize,
            is_written,
        }
    }

    fn bind_texel(
        buffer: &Buffer,
        offset: u32,
        size: u32,
        format: PixelFormat,
    ) -> MetalTexelBufferBinding {
        MetalTexelBufferBinding {
            buffer: buffer.handle(),
            offset: offset as usize,
            size: size as usize,
            format,
        }
    }

    fn encode_copies(
        &mut self,
        dst: &MetalBuffer,
        src: &MetalBuffer,
        copies: &[BufferCopy],
        work: ComputeWork,
        upload_prefix: bool,
    ) {
        if !upload_prefix {
            self.scheduler().request_outside_render_pass_operation_context_for(work);
        }
        for copy in copies {
            let encode = if upload_prefix { MetalBuffer::encode_upload_copy } else { MetalBuffer::encode_copy };
            encode(src,
                self.scheduler(),
                dst,
                copy.src_offset as usize,
                copy.dst_offset as usize,
                copy.size as usize,
            )
            .expect("Metal buffer-cache copy failed");
        }
    }

    fn converted_index_buffer(
        &mut self,
        source: &Buffer,
        source_offset: u32,
        index_format: IndexFormat,
        topology: PrimitiveTopology,
        base_vertex: u32,
        num_indices: u32,
    ) -> StagingBufferRef {
        if self.quad_index_pass.is_none() {
            self.quad_index_pass = Some(
                QuadIndexedPass::new(&self.device)
                    .expect("Metal quad index pass compilation failed"),
            );
        }
        // SAFETY: the runtime's scheduler/pool are distinct stable allocations
        // owned by the rasterizer; cache callers hold the runtime's mutex.
        self.quad_index_pass
            .as_ref()
            .unwrap()
            .assemble(
                unsafe { self.scheduler.as_mut() },
                unsafe { self.staging_pool.as_mut() },
                index_format,
                num_indices,
                base_vertex,
                &source.allocation,
                source_offset as usize,
                topology == PrimitiveTopology::QuadStrip,
            )
            .expect("Metal quad index assembly failed")
    }

    fn uint8_index_buffer(
        &mut self,
        source: &mut Buffer,
        source_offset: u32,
        num_indices: u32,
    ) -> (Arc<MetalBuffer>, usize) {
        source.allocation.enable_write_history();
        let generation = source.allocation.content_generation();
        // Common-cache GPU write declarations, CPU uploads and native blits all
        // advance the allocation generation, including repeated writes in one
        // submission. A historical write tick alone does not forbid reuse.
        let cacheable = generation != u64::MAX;
        if let Some(profile) = self.uint8_profile.as_mut() {
            profile.requests += 1;
            profile.uncacheable += u64::from(!cacheable);
        }
        if source.uint8_generation != generation || !cacheable {
            let key = (source_offset, num_indices);
            let requested_was_cached = source.uint8_indices.contains_key(&key);
            let old_count = source.uint8_indices.len();
            let mut removed_bytes = 0;
            source.uint8_indices.retain(|(offset, count), converted| {
                let unchanged = cacheable && source.allocation.region_unchanged_since(
                    source.uint8_generation, *offset as usize, *count as usize,
                );
                if !unchanged {
                    removed_bytes += converted.length() as u64;
                }
                unchanged
            });
            let removed_count = (old_count - source.uint8_indices.len()) as u64;
            source.uint8_bytes -= removed_bytes;
            source.cached_index_bytes.fetch_sub(removed_bytes, Ordering::Relaxed);
            source.allocated_bytes.fetch_sub(removed_bytes, Ordering::Relaxed);
            source.cached_index_entries.fetch_sub(removed_count, Ordering::Relaxed);
            if let Some(profile) = self.uint8_profile.as_mut() {
                profile.invalidated_entries += removed_count;
                profile.invalidated_requested_range += u64::from(requested_was_cached
                    && !source.uint8_indices.contains_key(&key),
                );
            }
            source.uint8_generation = generation;
        }
        let key = (source_offset, num_indices);
        if let Some(converted) = source.uint8_indices.get(&key) {
            if let Some(profile) = self.uint8_profile.as_mut() {
                profile.hits += 1;
            }
            return (Arc::clone(converted), 0);
        }
        if self.uint8_pass.is_none() {
            self.uint8_pass = Some(
                Uint8Pass::new(&self.device).expect("Metal uint8 index pass compilation failed"),
            );
        }
        let size = (u64::from(num_indices) * 2).max(4);
        if cacheable && num_indices != 0
            && size <= MAX_CACHED_UINT8_BYTES
            && self.cached_index_bytes.load(Ordering::Relaxed) <= MAX_CACHED_UINT8_BYTES - size
            && self.cached_index_entries.load(Ordering::Relaxed) < MAX_CACHED_UINT8_ENTRIES {
            let converted = Arc::new(MetalBuffer::new_private(&self.device, size as usize)
                .expect("Metal cached uint8 allocation failed"));
            self.uint8_pass.as_ref().unwrap().assemble_into(
                unsafe { self.scheduler.as_mut() }, num_indices, &source.allocation,
                source_offset as usize, &converted, 0,
            ).expect("Metal cached uint8 conversion failed");
            source.uint8_indices.insert(key, Arc::clone(&converted));
            source.uint8_bytes += size;
            self.cached_index_bytes.fetch_add(size, Ordering::Relaxed);
            self.cached_index_entries.fetch_add(1, Ordering::Relaxed);
            self.allocated_bytes.fetch_add(size, Ordering::Relaxed);
            if let Some(profile) = self.uint8_profile.as_mut() {
                profile.inserted += 1;
            }
            return (converted, 0);
        }
        if let Some(profile) = self.uint8_profile.as_mut() {
            profile.fallback += 1;
        }
        // SAFETY: same stable scheduler/pool ownership as the quad pass above.
        let staging = self.uint8_pass
            .as_ref()
            .unwrap()
            .assemble(
                unsafe { self.scheduler.as_mut() },
                unsafe { self.staging_pool.as_mut() },
                num_indices,
                &source.allocation,
                source_offset as usize,
            )
            .expect("Metal uint8 index assembly failed");
        (staging.buffer, staging.offset)
    }

    fn update_quad_lut(&mut self, topology: PrimitiveTopology, num_indices: u32) {
        let current = match topology {
            PrimitiveTopology::Quads => &self.quad_array_index_buffer,
            PrimitiveTopology::QuadStrip => &self.quad_strip_index_buffer,
            _ => return,
        };
        if current
            .as_ref()
            .is_some_and(|(_, count)| *count >= num_indices)
        {
            return;
        }
        let data = make_quad_lut(topology, num_indices);
        let buffer = Arc::new(
            MetalBuffer::new(&self.device, data.len().max(4)).expect("Metal quad LUT allocation"),
        );
        buffer.write(0, &data).expect("Metal quad LUT upload");
        match topology {
            PrimitiveTopology::Quads => self.quad_array_index_buffer = Some((buffer, num_indices)),
            PrimitiveTopology::QuadStrip => {
                self.quad_strip_index_buffer = Some((buffer, num_indices))
            }
            _ => {}
        }
    }
}

impl base::BufferCacheRuntime for BufferCacheRuntime {
    type Buffer = Buffer;
    type AsyncBuffer = StagingBufferRef;

    fn tick_frame(&mut self, slot_buffers: &mut SlotVector<Buffer>) {
        let scheduler_ptr = self.scheduler;
        let staging_ptr = self.staging_pool;
        unsafe { staging_ptr.as_ptr().as_mut().unwrap() }
            .tick_frame(unsafe { scheduler_ptr.as_ptr().as_mut().unwrap() })
            .expect("Metal staging frame tick failed");
        let known = unsafe { scheduler_ptr.as_ref() }.completed_tick();
        for (_, buffer) in slot_buffers.iter_mut() {
            if buffer.last_usage_tick() <= known {
                buffer.reset_usage_tracking();
            }
        }
        if let Some(profile) = self.uint8_profile.as_mut() {
            if profile.last_report.elapsed() >= Duration::from_secs(1) {
                let staging_usage = unsafe { staging_ptr.as_ref() }.cache_memory_usage(known);
                log::info!("[METAL_UINT8_CACHE] requests={} hits={} uncacheable={} invalidated_entries={} invalidated_requested_range={} inserted={} fallback={} cached_bytes={} entries={} buffer_bytes={} metal_allocated_bytes={}",
                    profile.requests, profile.hits, profile.uncacheable,
                    profile.invalidated_entries, profile.invalidated_requested_range,
                    profile.inserted, profile.fallback,
                    self.cached_index_bytes.load(Ordering::Relaxed),
                    self.cached_index_entries.load(Ordering::Relaxed),
                    self.allocated_bytes.load(Ordering::Relaxed),
                    self.device.device().currentAllocatedSize());
                log::info!("[METAL_STAGING_MEMORY] known_tick={known} stream_bytes={} cache_order=upload,download,device_local caches={staging_usage:?}",
                    unsafe { staging_ptr.as_ref() }.stream_buf().length());
                *profile = Uint8CacheProfile::default();
            }
        }
    }

    fn can_report_memory_usage(&self) -> bool {
        false
    }

    fn get_device_local_memory(&self) -> u64 {
        self.device.profile().recommended_resource_budget()
    }

    fn get_device_memory_usage(&self) -> u64 {
        self.allocated_bytes.load(Ordering::Relaxed)
    }

    fn get_storage_buffer_alignment(&self) -> u32 {
        16
    }

    fn finish(&mut self) {
        self.scheduler()
            .finish_all()
            .expect("Metal scheduler finish failed");
    }

    fn current_tick(&self) -> u64 {
        unsafe { self.scheduler.as_ref() }.current_tick()
    }

    fn known_gpu_tick(&self) -> u64 {
        unsafe { self.scheduler.as_ref() }.completed_tick()
    }

    fn wait(&mut self, tick: u64) {
        self.scheduler().wait(tick).expect("Metal tick wait failed");
    }

    fn upload_staging_buffer(&mut self, size: u64) -> StagingBufferRef {
        let scheduler_ptr = self.scheduler;
        self.staging_pool()
            .request_upload_buffer(
                unsafe { scheduler_ptr.as_ptr().as_mut().unwrap() },
                size as usize,
                false,
            )
            .expect("Metal upload staging allocation failed")
    }

    fn download_staging_buffer(&mut self, size: u64, deferred: bool) -> StagingBufferRef {
        let scheduler_ptr = self.scheduler;
        self.staging_pool()
            .request_download_buffer(
                unsafe { scheduler_ptr.as_ptr().as_mut().unwrap() },
                size as usize,
                deferred,
            )
            .expect("Metal download staging allocation failed")
    }

    fn free_deferred_staging_buffer(&mut self, buffer: &mut StagingBufferRef) {
        let scheduler_ptr = self.scheduler;
        self.staging_pool()
            .free_deferred(unsafe { scheduler_ptr.as_ref() }, buffer)
            .expect("Metal deferred staging release failed");
    }

    fn can_reorder_upload(&self, buffer: &Buffer, copies: &[BufferCopy]) -> bool {
        if *common::settings::values()
            .disable_buffer_reorder
            .get_value()
        {
            return false;
        }
        copies
            .iter()
            .all(|copy| !buffer.is_region_used(copy.dst_offset, copy.size))
    }

    fn pre_copy_barrier(&mut self) {
        // A single Metal command queue plus tracked resources establishes the
        // same write-to-blit dependency as Eden's Vulkan memory barrier.
        self.scheduler()
            .request_outside_render_pass_operation_context();
    }

    fn post_copy_barrier(&mut self) {
        // Eden needs an explicit TRANSFER_WRITE -> shader memory barrier.
        // Buffer-cache allocations use HazardTrackingModeTracked on one queue:
        // ordinary copies already end rendering to enter the blit encoder,
        // and eligible uploads are committed before the main command buffer.
        // Subsequent encoders therefore see the writes without closing an
        // unrelated active render pass. The untracked CPU-written upload
        // stream is only a source here; its leases remain live through the
        // scheduler completion tick. Untracked GPU destinations or an
        // MTL4CommandQueue would require an explicit synchronization path.
    }

    fn copy_buffer(
        &mut self,
        dst: &Buffer,
        src: &Buffer,
        copies: &[BufferCopy],
        _barrier: bool,
        _can_reorder_upload: bool,
    ) {
        self.encode_copies(&dst.allocation, &src.allocation, copies, ComputeWork::BufferCopy, false);
    }

    fn copy_buffer_from_staging(
        &mut self,
        dst: &Buffer,
        src: &StagingBufferRef,
        copies: &[BufferCopy],
        _barrier: bool,
        can_reorder_upload: bool,
    ) {
        let upload_prefix = can_reorder_upload
            && Arc::ptr_eq(&src.buffer, unsafe { self.staging_pool.as_ref() }.stream_buf());
        let work = if upload_prefix {
            ComputeWork::EligibleUpload
        } else if can_reorder_upload {
            ComputeWork::DedicatedUpload
        } else {
            ComputeWork::OrderedUpload
        };
        self.encode_copies(&dst.allocation, &src.buffer, copies, work, upload_prefix);
    }

    fn copy_buffer_to_staging(
        &mut self,
        dst: &StagingBufferRef,
        src: &Buffer,
        copies: &[BufferCopy],
        _barrier: bool,
    ) {
        self.encode_copies(&dst.buffer, &src.allocation, copies, ComputeWork::BufferDownload, false);
    }

    fn clear_buffer(&mut self, buffer: &Buffer, offset: u32, size: u64, value: u32) {
        let mut staging = self.upload_staging_buffer(size);
        for chunk in staging.mapped_span_mut().chunks_mut(4) {
            chunk.copy_from_slice(&value.to_ne_bytes()[..chunk.len()]);
        }
        let copies = [BufferCopy {
            src_offset: staging.offset(),
            dst_offset: offset as u64,
            size,
        }];
        self.encode_copies(&buffer.allocation, &staging.buffer, &copies, ComputeWork::BufferClear, false);
    }

    fn bind_index_buffer(
        &mut self,
        topology: PrimitiveTopology,
        index_format: IndexFormat,
        base_vertex: u32,
        num_indices: u32,
        buffer: &mut Buffer,
        offset: u32,
        _size: u32,
    ) {
        let (buffer, index_type, binding_offset) = if matches!(
            topology,
            PrimitiveTopology::Quads | PrimitiveTopology::QuadStrip
        ) {
            let staging = self.converted_index_buffer(
                buffer,
                offset,
                index_format,
                topology,
                base_vertex,
                num_indices,
            );
            (staging.buffer, MTLIndexType::UInt32, staging.offset)
        } else if index_format == IndexFormat::UnsignedByte {
            // The common cache passes index.first in base_vertex, and the
            // rasterizer retains that first_index in the eventual draw. Convert
            // its prefix too, so the converted binding preserves index origin.
            let end_index = base_vertex.checked_add(num_indices)
                .expect("Metal uint8 index range overflow");
            let (converted, offset) = self.uint8_index_buffer(buffer, offset, end_index);
            (converted, MTLIndexType::UInt16, offset)
        } else {
            (
                buffer.handle(),
                metal_index_type(index_format),
                offset as usize,
            )
        };
        self.index_binding = Some(MetalIndexBinding {
            buffer,
            offset: binding_offset,
            index_type,
        });
    }

    fn bind_quad_index_buffer(&mut self, topology: PrimitiveTopology, first: u32, count: u32) {
        if count == 0 {
            self.index_binding = Some(MetalIndexBinding {
                buffer: Arc::clone(&self.null_buffer),
                offset: 0,
                index_type: MTLIndexType::UInt32,
            });
            return;
        }
        self.update_quad_lut(topology, first.wrapping_add(count));
        let (buffer, num_indices) = match topology {
            PrimitiveTopology::Quads => self.quad_array_index_buffer.as_ref().unwrap(),
            PrimitiveTopology::QuadStrip => self.quad_strip_index_buffer.as_ref().unwrap(),
            _ => return,
        };
        let sub_first_offset =
            u64::from(first % 4) * u64::from(quad_count_for_topology(topology, *num_indices));
        let offset =
            (sub_first_offset + u64::from(quad_count_for_topology(topology, first))) * 6 * 4;
        self.index_binding = Some(MetalIndexBinding {
            buffer: Arc::clone(buffer),
            offset: offset as usize,
            index_type: MTLIndexType::UInt32,
        });
    }

    fn bind_vertex_buffer(
        &mut self,
        index: u32,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
        stride: u32,
    ) {
        let Some(binding) = self.vertex_bindings.get_mut(index as usize) else {
            return;
        };
        *binding = Some(MetalVertexBinding {
            buffer: buffer.handle(),
            offset: offset as usize,
            size: size as usize,
            stride: stride as usize,
        });
    }

    fn bind_vertex_buffers(&mut self, bindings: &HostBindings, buffers: &mut SlotVector<Buffer>) {
        for (slot, buffer_id) in bindings.buffer_ids.iter().enumerate() {
            let target = bindings.min_index as usize + slot;
            if target >= self.vertex_bindings.len() {
                break;
            }
            self.vertex_bindings[target] = if buffer_id.is_valid() {
                Some(MetalVertexBinding {
                    buffer: buffers[*buffer_id].handle(),
                    offset: bindings.offsets[slot] as usize,
                    size: bindings.sizes[slot] as usize,
                    stride: bindings.strides[slot] as usize,
                })
            } else {
                Some(MetalVertexBinding {
                    buffer: Arc::clone(&self.null_buffer),
                    offset: 0,
                    size: 4,
                    stride: 0,
                })
            };
        }
    }

    fn bind_uniform_buffer(
        &mut self,
        stage: usize,
        _binding_index: u32,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
    ) {
        self.graphics.uniform_buffers[stage].push(Self::bind_buffer(buffer, offset, size, false));
    }

    fn bind_storage_buffer(
        &mut self,
        stage: usize,
        _binding_index: u32,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
        is_written: bool,
    ) {
        self.graphics.storage_buffers[stage]
            .push(Self::bind_buffer(buffer, offset, size, is_written));
    }

    fn bind_texture_buffer(
        &mut self,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
        format: PixelFormat,
    ) {
        let binding = Self::bind_texel(buffer, offset, size, format);
        match self.binding_target {
            BindingTarget::Graphics => self.graphics.texture_buffers.push(binding),
            BindingTarget::Compute => self.compute.texture_buffers.push(binding),
        }
    }

    fn bind_image_buffer(
        &mut self,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
        format: PixelFormat,
    ) {
        let binding = Self::bind_texel(buffer, offset, size, format);
        match self.binding_target {
            BindingTarget::Graphics => self.graphics.image_buffers.push(binding),
            BindingTarget::Compute => self.compute.image_buffers.push(binding),
        }
    }

    fn bind_transform_feedback_buffers(
        &mut self,
        bindings: &HostBindings,
        buffers: &mut SlotVector<Buffer>,
    ) {
        self.transform_feedback_bindings = bindings
            .buffer_ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                if id.is_valid() {
                    MetalBufferBinding {
                        buffer: buffers[*id].handle(),
                        offset: bindings.offsets[index] as usize,
                        size: bindings.sizes[index] as usize,
                        is_written: true,
                    }
                } else {
                    MetalBufferBinding {
                        buffer: Arc::clone(&self.null_buffer),
                        offset: 0,
                        size: 4,
                        is_written: true,
                    }
                }
            })
            .collect();
    }

    fn bind_compute_uniform_buffer(
        &mut self,
        _binding_index: u32,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
    ) {
        self.compute
            .uniform_buffers
            .push(Self::bind_buffer(buffer, offset, size, false));
    }

    fn bind_compute_storage_buffer(
        &mut self,
        _binding_index: u32,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
        is_written: bool,
    ) {
        self.compute
            .storage_buffers
            .push(Self::bind_buffer(buffer, offset, size, is_written));
    }

    fn uniform_buffer_alignment(&self) -> u32 {
        16
    }

    fn with_mapped_uniform_buffer(
        &mut self,
        stage: usize,
        _binding_index: u32,
        size: u32,
        write: &mut dyn FnMut(&mut [u8]),
    ) -> bool {
        let scheduler_ptr = self.scheduler;
        let mut staging = self
            .staging_pool()
            .request_upload_buffer_with_binding_span(
                unsafe { scheduler_ptr.as_ptr().as_mut().unwrap() },
                size as usize,
                MAX_CONST_BUFFER_SIZE,
            )
            .expect("Metal mapped uniform staging allocation failed");
        write(staging.mapped_span_mut());
        let binding = MetalBufferBinding {
            buffer: Arc::clone(&staging.buffer),
            offset: staging.offset,
            size: size as usize,
            is_written: false,
        };
        match self.binding_target {
            BindingTarget::Graphics => self.graphics.uniform_buffers[stage].push(binding),
            BindingTarget::Compute => self.compute.uniform_buffers.push(binding),
        }
        true
    }
}

fn metal_index_type(format: IndexFormat) -> MTLIndexType {
    match format {
        IndexFormat::UnsignedByte | IndexFormat::UnsignedShort => MTLIndexType::UInt16,
        IndexFormat::UnsignedInt => MTLIndexType::UInt32,
    }
}

fn quad_count_for_topology(topology: PrimitiveTopology, num_indices: u32) -> u32 {
    match topology {
        PrimitiveTopology::Quads => num_indices / 4,
        PrimitiveTopology::QuadStrip => num_indices.saturating_sub(2) / 2,
        _ => 0,
    }
}

fn make_quad_lut(topology: PrimitiveTopology, num_indices: u32) -> Vec<u8> {
    let num_quads = quad_count_for_topology(topology, num_indices);
    let mut output = Vec::with_capacity(num_quads as usize * 6 * 4 * 4);
    for first in 0u32..4 {
        for quad in 0..num_quads {
            let indices =
                match topology {
                    PrimitiveTopology::Quads => [0, 1, 2, 0, 2, 3]
                        .map(|index| first.wrapping_add(index).wrapping_add(quad * 4)),
                    PrimitiveTopology::QuadStrip => [0, 3, 1, 0, 2, 3]
                        .map(|index| first.wrapping_add(index).wrapping_add(quad * 2)),
                    _ => unreachable!("invalid quad topology"),
                };
            for index in indices {
                output.extend_from_slice(&index.to_ne_bytes());
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_copy_barrier_preserves_render_encoder_and_gpu_visibility() {
        use crate::buffer_cache::buffer_cache_base::BufferCacheRuntime as _;
        use crate::renderer_metal::metal_image::MetalImage;
        use crate::texture_cache::image_info::ImageInfo;
        use crate::texture_cache::types::{Extent3D, ImageType, SubresourceExtent};
        use objc2_foundation::NSString;
        use objc2_metal::{MTLComputeCommandEncoder, MTLDevice, MTLLibrary, MTLSize,
            MTLRenderPassDescriptor, MTLLoadAction, MTLStoreAction,
            MTLRenderPipelineDescriptor, MTLRenderCommandEncoder, MTLPixelFormat, MTLPrimitiveType};
        for prefix in [false, true] {
            let (device, mut scheduler, _pool, mut runtime) = runtime();
            let target = MetalImage::new(&device, &ImageInfo {
                format: PixelFormat::A8B8G8R8Unorm, image_type: ImageType::E2D,
                size: Extent3D { width: 4, height: 4, depth: 1 },
                resources: SubresourceExtent { levels: 1, layers: 1 }, num_samples: 1,
                ..ImageInfo::default()
            }).unwrap();
            let pass = MTLRenderPassDescriptor::new();
            let color = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
            color.setTexture(Some(target.handle()));
            color.setLoadAction(MTLLoadAction::Clear);
            color.setStoreAction(MTLStoreAction::Store);
            pass.setRenderTargetWidth(4);
            pass.setRenderTargetHeight(4);
            pass.setDefaultRasterSampleCount(1);
            scheduler.begin_render_pass(&pass).unwrap();
            let destination = Buffer::new(&mut runtime, 0x1000, 4);
            let mut upload = runtime.upload_staging_buffer(4);
            upload.mapped_span_mut()[..4].copy_from_slice(&0x12345678u32.to_ne_bytes());
            let copies = [BufferCopy { src_offset: upload.offset(), dst_offset: 0, size: 4 }];
            let tick = scheduler.current_tick();
            runtime.copy_buffer_from_staging(&destination, &upload, &copies, false, prefix);
            if !prefix {
                // Ordinary copies legitimately change encoder before this.
                scheduler.begin_render_pass(&pass).unwrap();
            }
            let before = scheduler.with_render_encoder(|e| e as *const _ as usize).unwrap();
            runtime.post_copy_barrier();
            let after = scheduler.with_render_encoder(|e| e as *const _ as usize).unwrap();
            assert_eq!(before, after);
            let library = device.device().newLibraryWithSource_options_error(
                &NSString::from_str("#include <metal_stdlib>\nusing namespace metal;\n\
                    vertex float4 full_screen(uint id [[vertex_id]]) {\n\
                        return float4(id == 1 ? 3.0 : -1.0, id == 2 ? 3.0 : -1.0, 0.0, 1.0);\n\
                    }\n\
                    fragment float4 shade(constant uint* src [[buffer(0)]]) {\n\
                        uint v = src[0];\n\
                        return float4(v & 255u, (v >> 8) & 255u, (v >> 16) & 255u, v >> 24) / 255.0;\n\
                    }\n\
                    kernel void consume(constant uint* src [[buffer(0)]], device uint* dst [[buffer(1)]],\n\
                        texture2d<float, access::read> image [[texture(0)]]) {\n\
                        dst[0] = src[0];\n\
                        uint4 pixel = uint4(round(image.read(uint2(1, 1)) * 255.0));\n\
                        dst[1] = pixel.x | (pixel.y << 8) | (pixel.z << 16) | (pixel.w << 24);\n\
                    }"), None).unwrap();
            let render_desc = MTLRenderPipelineDescriptor::new();
            render_desc.setVertexFunction(Some(&library.newFunctionWithName(&NSString::from_str("full_screen")).unwrap()));
            render_desc.setFragmentFunction(Some(&library.newFunctionWithName(&NSString::from_str("shade")).unwrap()));
            unsafe { render_desc.colorAttachments().objectAtIndexedSubscript(0) }
                .setPixelFormat(MTLPixelFormat::RGBA8Unorm);
            let render_pipeline = device.device().newRenderPipelineStateWithDescriptor_error(&render_desc).unwrap();
            scheduler.with_render_encoder(|encoder| unsafe {
                encoder.setRenderPipelineState(&render_pipeline);
                encoder.setFragmentBuffer_offset_atIndex(Some(destination.allocation.handle()), 0, 0);
                encoder.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 3);
            }).unwrap();
            let function = library.newFunctionWithName(&NSString::from_str("consume")).unwrap();
            let pipeline = device.device().newComputePipelineStateWithFunction_error(&function).unwrap();
            let output = MetalBuffer::new(&device, 8).unwrap();
            scheduler.with_compute_encoder(|encoder| unsafe {
                encoder.setComputePipelineState(&pipeline);
                encoder.setBuffer_offset_atIndex(Some(destination.allocation.handle()), 0, 0);
                encoder.setBuffer_offset_atIndex(Some(output.handle()), 0, 1);
                encoder.setTexture_atIndex(Some(target.handle()), 0);
                let one = MTLSize { width: 1, height: 1, depth: 1 };
                encoder.dispatchThreads_threadsPerThreadgroup(one, one);
            }).unwrap();
            assert_eq!(scheduler.current_tick(), tick);
            scheduler.finish_all().unwrap();
            let mut bytes = [0; 8];
            output.read(0, &mut bytes).unwrap();
            assert_eq!(u32::from_ne_bytes(bytes[..4].try_into().unwrap()), 0x12345678);
            assert_eq!(u32::from_ne_bytes(bytes[4..].try_into().unwrap()), 0x12345678);
        }
    }

    #[test]
    fn mapped_compute_constants_preserve_binding_order_and_target() {
        use crate::buffer_cache::buffer_cache_base::BufferCacheRuntime as _;
        let (_device, mut scheduler, _pool, mut runtime) = runtime();
        runtime.begin_compute_bindings();
        assert!(runtime.with_mapped_uniform_buffer(0, 0, 4, &mut |bytes| {
            bytes.copy_from_slice(&11u32.to_ne_bytes());
        }));
        let mut direct = Buffer::new(&mut runtime, 0x1000, 16);
        direct.immediate_upload(0, &22u32.to_ne_bytes());
        runtime.bind_compute_uniform_buffer(1, &mut direct, 0, 4);
        assert!(runtime.with_mapped_uniform_buffer(0, 2, 4, &mut |bytes| {
            bytes.copy_from_slice(&33u32.to_ne_bytes());
        }));
        assert!(runtime.graphics.uniform_buffers.iter().all(Vec::is_empty));
        assert_eq!(runtime.compute.uniform_buffers.len(), 3);
        for (binding, expected) in runtime.compute.uniform_buffers.iter().zip([11, 22, 33]) {
            let mut bytes = [0; 4];
            binding.buffer.read(binding.offset, &mut bytes).unwrap();
            assert_eq!(u32::from_ne_bytes(bytes), expected);
        }
        runtime.begin_graphics_bindings();
        assert!(runtime.with_mapped_uniform_buffer(4, 0, 4, &mut |bytes| bytes.fill(7)));
        assert_eq!(runtime.graphics.uniform_buffers[4].len(), 1);
        assert_eq!(runtime.compute.uniform_buffers.len(), 3);
        scheduler.finish_all().unwrap();
    }

    #[test]
    fn null_buffers_cover_native_vector_and_indirect_constant_reads() {
        use crate::buffer_cache::buffer_cache_base::BufferCacheRuntime as _;
        use objc2_foundation::NSString;
        use objc2_metal::{MTLComputeCommandEncoder, MTLDevice, MTLLibrary, MTLSize};
        let (device, mut scheduler, _pool, mut runtime) = runtime();
        let mut null = Buffer::null(&mut runtime);
        runtime.bind_compute_uniform_buffer(0, &mut null, 0, 0);
        let bound = runtime.compute.uniform_buffers[0].buffer.clone();
        let library = device.device().newLibraryWithSource_options_error(
            &NSString::from_str("#include <metal_stdlib>\nusing namespace metal;\n\
                kernel void read_null(constant uint4* c [[buffer(0)]],\n\
                device uint4* out [[buffer(1)]]) { out[0] = c[0]; out[1] = c[4095]; }"),
            None).unwrap();
        let function = library.newFunctionWithName(&NSString::from_str("read_null")).unwrap();
        let pipeline = device.device().newComputePipelineStateWithFunction_error(&function).unwrap();
        for source in [bound, runtime.null_buffer()] {
            assert_eq!(source.length(), MAX_CONST_BUFFER_SIZE);
            let output = MetalBuffer::new(&device, 32).unwrap();
            output.write(0, &[0xff; 32]).unwrap();
            scheduler.with_compute_encoder(|encoder| unsafe {
                encoder.setComputePipelineState(&pipeline);
                encoder.setBuffer_offset_atIndex(Some(source.handle()), 0, 0);
                encoder.setBuffer_offset_atIndex(Some(output.handle()), 0, 1);
                let one = MTLSize { width: 1, height: 1, depth: 1 };
                encoder.dispatchThreads_threadsPerThreadgroup(one, one);
            }).unwrap();
            scheduler.finish_all().unwrap();
            let mut bytes = [0xff; 32];
            output.read(0, &mut bytes).unwrap();
            assert_eq!(bytes, [0; 32]);
        }
    }

    #[test]
    fn immutable_uint8_reuses_owned_output_without_recording_work() {
        let (_device, mut scheduler, _pool, mut runtime) = runtime();
        let mut source = Buffer::new(&mut runtime, 0x1000, 8);
        source.immediate_upload(0, &[1, 2, 3, 4, 5, 6, 7, 8]);
        let (first, offset) = runtime.uint8_index_buffer(&mut source, 2, 3);
        assert_eq!(offset, 0);
        scheduler.finish_all().unwrap();
        assert!(!scheduler.has_active_work());
        let (second, _) = runtime.uint8_index_buffer(&mut source, 2, 3);
        assert!(Arc::ptr_eq(&first, &second));
        assert!(!scheduler.has_active_work());
        let (different_range, _) = runtime.uint8_index_buffer(&mut source, 3, 3);
        assert!(!Arc::ptr_eq(&first, &different_range));
        assert_eq!(runtime.cached_index_bytes.load(Ordering::Relaxed), 12);
        scheduler.finish_all().unwrap();
        drop(source);
        assert_eq!(runtime.cached_index_bytes.load(Ordering::Relaxed), 0);
        assert_eq!(runtime.cached_index_entries.load(Ordering::Relaxed), 0);
        assert_eq!(runtime.allocated_bytes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn uint8_profile_distinguishes_reuse_invalidation_and_fallback() {
        let (_device, mut scheduler, _pool, mut runtime) = runtime();
        runtime.uint8_profile = Some(Uint8CacheProfile::default());
        let mut source = Buffer::new(&mut runtime, 0x1000, 8);
        source.immediate_upload(0, &[1; 8]);
        runtime.uint8_index_buffer(&mut source, 0, 4);
        runtime.uint8_index_buffer(&mut source, 0, 4);
        runtime.uint8_index_buffer(&mut source, 4, 4);
        source.immediate_upload(0, &[2; 4]);
        runtime.uint8_index_buffer(&mut source, 0, 4);
        runtime.uint8_index_buffer(&mut source, 0, 0);
        let profile = runtime.uint8_profile.as_ref().unwrap();
        assert_eq!(profile.requests, 5);
        assert_eq!(profile.hits, 1);
        assert_eq!(profile.invalidated_entries, 1);
        assert_eq!(profile.invalidated_requested_range, 1);
        assert_eq!(profile.inserted, 3);
        assert_eq!(profile.fallback, 1);
        assert_eq!(profile.uncacheable, 0);
        scheduler.finish_all().unwrap();
    }

    #[test]
    fn region_notifications_keep_whole_buffer_invalidation_until_range_tracking() {
        let (_device, _scheduler, _pool, mut runtime) = runtime();
        let mut buffer = Buffer::new(&mut runtime, 0x1000, 8);
        let initial = buffer.allocation.content_generation();
        buffer.mark_written_region(9, 0, 4);
        assert_eq!(buffer.write_tick(), 9);
        assert_eq!(buffer.allocation.content_generation(), initial + 1);
        buffer.mark_written_region(9, 4, 4);
        assert_eq!(buffer.allocation.content_generation(), initial + 2);
        buffer.mark_written_region(10, u64::MAX, 4);
        assert_eq!(buffer.write_tick(), 10);
        assert_eq!(buffer.allocation.content_generation(), initial + 3);
    }

    #[test]
    fn uint8_invalidates_on_cpu_upload_and_same_tick_native_copies() {
        let (device, mut scheduler, _pool, mut runtime) = runtime();
        let mut source = Buffer::new(&mut runtime, 0x1000, 4);
        source.immediate_upload(0, &[1, 2, 3, 4]);
        let (old, _) = runtime.uint8_index_buffer(&mut source, 0, 4);
        scheduler.finish_all().unwrap();
        source.immediate_upload(0, &[5, 6, 7, 8]);
        let (cpu_updated, _) = runtime.uint8_index_buffer(&mut source, 0, 4);
        assert!(!Arc::ptr_eq(&old, &cpu_updated));

        let upload = MetalBuffer::new(&device, 8).unwrap();
        upload.write(0, &[9, 10, 11, 12, 13, 14, 15, 0xff]).unwrap();
        let tick = scheduler.current_tick();
        upload.encode_copy(&mut scheduler, &source.allocation, 0, 0, 4).unwrap();
        let (copy_one, _) = runtime.uint8_index_buffer(&mut source, 0, 4);
        upload.encode_copy(&mut scheduler, &source.allocation, 4, 0, 4).unwrap();
        let (copy_two, _) = runtime.uint8_index_buffer(&mut source, 0, 4);
        assert_eq!(scheduler.current_tick(), tick);
        assert!(!Arc::ptr_eq(&copy_one, &copy_two));
        assert_eq!(source.uint8_indices.len(), 1);
        // Prior conversions remain valid for their already-recorded consumers.
        let download = MetalBuffer::new(&device, 32).unwrap();
        for (index, converted) in [old, cpu_updated, copy_one, copy_two].iter().enumerate() {
            converted.encode_copy(&mut scheduler, &download, 0, index * 8, 8).unwrap();
        }
        scheduler.finish_all().unwrap();
        let mut bytes = [0; 32];
        download.read(0, &mut bytes).unwrap();
        let actual: Vec<u16> = bytes.chunks_exact(2)
            .map(|word| u16::from_ne_bytes(word.try_into().unwrap())).collect();
        assert_eq!(actual, [1,2,3,4, 5,6,7,8, 9,10,11,12, 13,14,15,0xffff]);
    }

    #[test]
    fn uint8_retains_disjoint_gpu_ranges_and_reconverts_overlaps() {
        let (device, mut scheduler, _pool, mut runtime) = runtime();
        let mut source = Buffer::new(&mut runtime, 0x1000, 8);
        source.immediate_upload(0, &[1, 2, 3, 4, 5, 6, 7, 8]);
        let left = runtime.uint8_index_buffer(&mut source, 0, 4).0;
        let right = runtime.uint8_index_buffer(&mut source, 4, 4).0;
        let upload = MetalBuffer::new(&device, 4).unwrap();
        upload.write(0, &[9, 10, 11, 0xff]).unwrap();
        let tick = scheduler.current_tick();
        source.mark_written_region(tick, 4, 4);
        upload.encode_copy(&mut scheduler, &source.allocation, 0, 4, 4).unwrap();
        let reused = runtime.uint8_index_buffer(&mut source, 0, 4).0;
        let updated = runtime.uint8_index_buffer(&mut source, 4, 4).0;
        assert!(Arc::ptr_eq(&left, &reused));
        assert!(!Arc::ptr_eq(&right, &updated));
        assert_eq!(scheduler.current_tick(), tick);
        assert_eq!(runtime.cached_index_bytes.load(Ordering::Relaxed), 16);
        assert_eq!(runtime.cached_index_entries.load(Ordering::Relaxed), 2);
        let download = MetalBuffer::new(&device, 24).unwrap();
        for (index, converted) in [left, right, updated].iter().enumerate() {
            converted.encode_copy(&mut scheduler, &download, 0, index * 8, 8).unwrap();
        }
        scheduler.finish_all().unwrap();
        let mut bytes = [0; 24];
        download.read(0, &mut bytes).unwrap();
        let actual: Vec<u16> = bytes.chunks_exact(2)
            .map(|word| u16::from_ne_bytes(word.try_into().unwrap())).collect();
        assert_eq!(actual, [1,2,3,4, 5,6,7,8, 9,10,11,0xffff]);
        drop(source);
        assert_eq!(runtime.cached_index_bytes.load(Ordering::Relaxed), 0);
        assert_eq!(runtime.cached_index_entries.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn uint8_history_eviction_and_unknown_writes_force_reconversion() {
        let (_device, mut scheduler, _pool, mut runtime) = runtime();
        let mut source = Buffer::new(&mut runtime, 0x1000, 8);
        source.immediate_upload(0, &[1; 8]);
        let first = runtime.uint8_index_buffer(&mut source, 0, 4).0;
        scheduler.finish_all().unwrap();
        source.immediate_upload(4, &[2; 4]);
        assert!(Arc::ptr_eq(&first, &runtime.uint8_index_buffer(&mut source, 0, 4).0));
        assert!(!scheduler.has_active_work());
        for _ in 0..65 { source.immediate_upload(4, &[3; 4]); }
        let evicted = runtime.uint8_index_buffer(&mut source, 0, 4).0;
        assert!(!Arc::ptr_eq(&first, &evicted));
        source.set_write_tick(scheduler.current_tick());
        let unknown = runtime.uint8_index_buffer(&mut source, 0, 4).0;
        assert!(!Arc::ptr_eq(&evicted, &unknown));
        scheduler.finish_all().unwrap();
    }

    #[test]
    fn uint8_reuse_observes_partial_runtime_clear() {
        let (device, mut scheduler, _pool, mut runtime) = runtime();
        let mut source = Buffer::new(&mut runtime, 0x1000, 8);
        source.immediate_upload(0, &[1, 2, 3, 4, 5, 6, 7, 8]);
        let left = runtime.uint8_index_buffer(&mut source, 0, 4).0;
        let right = runtime.uint8_index_buffer(&mut source, 4, 4).0;
        base::BufferCacheRuntime::clear_buffer(&mut runtime, &source, 4, 4, 0xff001234);
        assert!(Arc::ptr_eq(&left, &runtime.uint8_index_buffer(&mut source, 0, 4).0));
        let cleared = runtime.uint8_index_buffer(&mut source, 4, 4).0;
        assert!(!Arc::ptr_eq(&right, &cleared));
        let download = MetalBuffer::new(&device, 24).unwrap();
        for (index, converted) in [left, right, cleared].iter().enumerate() {
            converted.encode_copy(&mut scheduler, &download, 0, index * 8, 8).unwrap();
        }
        scheduler.finish_all().unwrap();
        let mut bytes = [0; 24];
        download.read(0, &mut bytes).unwrap();
        let actual: Vec<u16> = bytes.chunks_exact(2)
            .map(|word| u16::from_ne_bytes(word.try_into().unwrap())).collect();
        assert_eq!(actual, [1,2,3,4, 5,6,7,8, 0x34,0x12,0,0xffff]);
        assert_eq!(runtime.cached_index_bytes.load(Ordering::Relaxed), 16);
        assert_eq!(runtime.cached_index_entries.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn uint8_reuse_tracks_same_tick_compute_writes_and_preserves_old_outputs() {
        use objc2_foundation::NSString;
        use objc2_metal::{MTLBarrierScope, MTLComputeCommandEncoder, MTLDevice, MTLLibrary, MTLSize};
        let (device, mut scheduler, _pool, mut runtime) = runtime();
        let mut source = Buffer::new(&mut runtime, 0x1000, 4);
        source.immediate_upload(0, &[1, 2, 3, 4]);
        let first = runtime.uint8_index_buffer(&mut source, 0, 4).0;
        let mut outputs = vec![first];
        let library = device.device().newLibraryWithSource_options_error(
            &NSString::from_str("#include <metal_stdlib>\nusing namespace metal;\n\
                kernel void write_indices(device uint* out [[buffer(0)]],\n\
                constant uint& value [[buffer(1)]]) { out[0] = value; }"),
            None).unwrap();
        let function = library.newFunctionWithName(&NSString::from_str("write_indices")).unwrap();
        let pipeline = device.device().newComputePipelineStateWithFunction_error(&function).unwrap();
        let tick = scheduler.current_tick();
        for value in [0x08070605u32, 0xff0b0a09] {
            // Same declaration point as MarkWrittenBuffer before binding a
            // writable shader resource. No CPU write or blit invalidates it.
            BufferCacheBuffer::set_write_tick(&mut source, tick);
            scheduler.with_compute_encoder(|encoder| unsafe {
                encoder.memoryBarrierWithScope(MTLBarrierScope::Buffers);
                encoder.setComputePipelineState(&pipeline);
                encoder.setBuffer_offset_atIndex(Some(source.allocation.handle()), 0, 0);
                encoder.setBytes_length_atIndex(NonNull::from(&value).cast(), 4, 1);
                let one = MTLSize { width: 1, height: 1, depth: 1 };
                encoder.dispatchThreads_threadsPerThreadgroup(one, one);
                encoder.memoryBarrierWithScope(MTLBarrierScope::Buffers);
            }).unwrap();
            let converted = runtime.uint8_index_buffer(&mut source, 0, 4).0;
            assert!(!Arc::ptr_eq(outputs.last().unwrap(), &converted));
            let reused = runtime.uint8_index_buffer(&mut source, 0, 4).0;
            assert!(Arc::ptr_eq(&converted, &reused));
            outputs.push(converted);
            assert_eq!(source.uint8_indices.len(), 1);
            assert_eq!(source.write_tick(), tick);
            assert_eq!(scheduler.current_tick(), tick);
        }
        let download = MetalBuffer::new(&device, 24).unwrap();
        for (index, converted) in outputs.iter().enumerate() {
            converted.encode_copy(&mut scheduler, &download, 0, index * 8, 8).unwrap();
        }
        scheduler.finish_all().unwrap();
        let mut bytes = [0; 24];
        download.read(0, &mut bytes).unwrap();
        let actual: Vec<u16> = bytes.chunks_exact(2)
            .map(|word| u16::from_ne_bytes(word.try_into().unwrap())).collect();
        assert_eq!(actual, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 0xffff]);
        let reused = runtime.uint8_index_buffer(&mut source, 0, 4).0;
        assert!(Arc::ptr_eq(outputs.last().unwrap(), &reused));
        assert!(!scheduler.has_active_work());
    }

    #[test]
    fn uint8_cache_budget_falls_back_to_existing_conversion() {
        let (_device, mut scheduler, _pool, mut runtime) = runtime();
        let mut source = Buffer::new(&mut runtime, 0x1000, 4);
        source.immediate_upload(0, &[1, 2, 3, 4]);
        runtime.cached_index_bytes.store(MAX_CACHED_UINT8_BYTES, Ordering::Relaxed);
        runtime.uint8_index_buffer(&mut source, 0, 4);
        assert!(source.uint8_indices.is_empty());
        runtime.cached_index_bytes.store(0, Ordering::Relaxed);
        runtime.cached_index_entries.store(MAX_CACHED_UINT8_ENTRIES, Ordering::Relaxed);
        runtime.uint8_index_buffer(&mut source, 0, 4);
        assert!(source.uint8_indices.is_empty());
        runtime.cached_index_entries.store(0, Ordering::Relaxed);
        runtime.uint8_index_buffer(&mut source, 0, 4);
        assert_eq!(source.uint8_indices.len(), 1);
        scheduler.finish_all().unwrap();
    }

    fn runtime() -> (
        MetalDevice,
        Box<MetalScheduler>,
        Box<MetalStagingBufferPool>,
        BufferCacheRuntime,
    ) {
        let device = MetalDevice::new().expect("Metal device");
        let mut scheduler = Box::new(MetalScheduler::new(&device));
        let mut staging_pool =
            Box::new(MetalStagingBufferPool::new(&device).expect("Metal staging pool"));
        let runtime = BufferCacheRuntime::new(&device, &mut scheduler, &mut staging_pool);
        (device, scheduler, staging_pool, runtime)
    }

    #[test]
    fn metal_policy_matches_non_gl_upstream_cache_contract() {
        use crate::buffer_cache::buffer_cache_base::BufferCacheParams as _;
        assert!(!BufferCacheParams::IS_OPENGL);
        assert!(!BufferCacheParams::HAS_FULL_INDEX_AND_PRIMITIVE_SUPPORT);
        assert!(BufferCacheParams::USE_MEMORY_MAPS);
        assert!(BufferCacheParams::USE_MEMORY_MAPS_FOR_UPLOADS);
        assert!(BufferCacheParams::SEPARATE_IMAGE_BUFFER_BINDINGS);
    }

    #[test]
    fn uint8_restart_expands_to_uint16_restart() {
        let input = [0, 7, 0xff];
        let expanded = input.map(|index| {
            if index == 0xff {
                u16::MAX
            } else {
                index as u16
            }
        });
        assert_eq!(expanded, [0, 7, 0xffff]);
    }

    #[test]
    fn quad_lut_matches_eden_swizzles() {
        let quad = make_quad_lut(PrimitiveTopology::Quads, 4);
        let values = quad
            .chunks_exact(4)
            .map(|bytes| u32::from_ne_bytes(bytes.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(&values[..6], &[0, 1, 2, 0, 2, 3]);
        assert_eq!(&values[6..12], &[1, 2, 3, 1, 3, 4]);

        let strip = make_quad_lut(PrimitiveTopology::QuadStrip, 4);
        let values = strip
            .chunks_exact(4)
            .map(|bytes| u32::from_ne_bytes(bytes.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(&values[..6], &[0, 3, 1, 0, 2, 3]);
    }

    #[test]
    fn runtime_copies_and_clears_in_scheduler_order() {
        use crate::buffer_cache::buffer_cache_base::BufferCacheRuntime as _;

        let (_device, _scheduler, _staging_pool, mut runtime) = runtime();
        let source = Buffer::new(&mut runtime, 0x1000, 32);
        let destination = Buffer::new(&mut runtime, 0x2000, 32);
        source.immediate_upload(0, &[1, 2, 3, 4, 5, 6, 7, 8]);
        runtime.copy_buffer(
            &destination,
            &source,
            &[BufferCopy {
                src_offset: 2,
                dst_offset: 8,
                size: 4,
            }],
            true,
            false,
        );
        runtime.clear_buffer(&destination, 16, 8, 0x4433_2211);
        runtime.finish();

        let mut copied = [0; 4];
        destination.immediate_download(8, &mut copied);
        assert_eq!(copied, [3, 4, 5, 6]);
        let mut cleared = [0; 8];
        destination.immediate_download(16, &mut cleared);
        assert_eq!(cleared, [0x11, 0x22, 0x33, 0x44, 0x11, 0x22, 0x33, 0x44]);
    }

    #[test]
    fn runtime_expands_uint8_restart_for_metal() {
        use crate::buffer_cache::buffer_cache_base::BufferCacheRuntime as _;

        let (device, mut scheduler, _staging_pool, mut runtime) = runtime();
        let mut source = Buffer::new(&mut runtime, 0x1000, 4);
        source.immediate_upload(0, &[1, 0xff, 9, 3]);
        runtime.bind_index_buffer(
            PrimitiveTopology::Triangles,
            IndexFormat::UnsignedByte,
            0,
            4,
            &mut source,
            0,
            4,
        );
        let binding = runtime.index_binding().unwrap();
        assert_eq!(binding.index_type, MTLIndexType::UInt16);
        let download = MetalBuffer::new(&device, 8).unwrap();
        binding
            .buffer
            .encode_copy(&mut scheduler, &download, binding.offset, 0, 8)
            .unwrap();
        scheduler.finish_all().unwrap();
        let mut bytes = [0; 8];
        download.read(0, &mut bytes).unwrap();
        assert_eq!(
            bytes
                .chunks_exact(2)
                .map(|value| u16::from_ne_bytes(value.try_into().unwrap()))
                .collect::<Vec<_>>(),
            [1, 0xffff, 9, 3]
        );
    }

    #[test]
    fn runtime_uint8_binding_preserves_nonzero_first_index() {
        use crate::buffer_cache::buffer_cache_base::BufferCacheRuntime as _;

        let (device, mut scheduler, _pool, mut runtime) = runtime();
        for gpu_written in [false, true] {
            let mut source = Buffer::new(&mut runtime, 0x1000, 16);
            source.immediate_upload(0, &[90, 91, 92, 93, 1, 2, 3, 7, 8, 0xff, 10, 11, 12, 13, 14, 15]);
            if gpu_written {
                source.set_write_tick(scheduler.current_tick());
            }
            runtime.bind_index_buffer(PrimitiveTopology::Triangles,
                IndexFormat::UnsignedByte, 3, 3, &mut source, 4, 6);
            let binding = runtime.index_binding().unwrap().clone();
            let first_index = 3usize;
            assert!(binding.offset + (first_index + 3) * 2 <= binding.buffer.length(),
                "converted allocation must cover the draw's first_index + count");
            let download = MetalBuffer::new(&device, 6).unwrap();
            binding.buffer.encode_copy(&mut scheduler, &download,
                binding.offset + first_index * 2, 0, 6).unwrap();
            scheduler.finish_all().unwrap();
            let mut bytes = [0; 6];
            download.read(0, &mut bytes).unwrap();
            assert_eq!(bytes, [7, 0, 8, 0, 0xff, 0xff]);
            runtime.bind_index_buffer(PrimitiveTopology::Triangles,
                IndexFormat::UnsignedByte, 3, 3, &mut source, 4, 6);
            if !gpu_written {
                assert!(Arc::ptr_eq(&binding.buffer, &runtime.index_binding().unwrap().buffer));
                assert!(!scheduler.has_active_work());
            }
            scheduler.finish_all().unwrap();
        }
    }

    #[test]
    fn beginning_graphics_bindings_preserves_non_dirty_vertex_bindings() {
        let (_device, _scheduler, _staging_pool, mut runtime) = runtime();
        runtime.vertex_bindings[0] = Some(MetalVertexBinding {
            buffer: Arc::clone(&runtime.null_buffer),
            offset: 0,
            size: 4,
            stride: 4,
        });

        runtime.begin_graphics_bindings();

        assert!(runtime.vertex_bindings[0].is_some());
    }
}
