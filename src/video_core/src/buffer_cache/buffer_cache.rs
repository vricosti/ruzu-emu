// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `video_core/buffer_cache/buffer_cache.h` and `buffer_cache.cpp`
//!
//! Concrete buffer cache implementation. This file contains the method bodies
//! for the `BufferCache<P>` template class.
//!
//! The C++ version splits the template definition across `buffer_cache_base.h`
//! (class declaration) and `buffer_cache.h` (template method implementations),
//! plus `buffer_cache.cpp` (explicit template instantiation and profiling macros).
//! In Rust, the struct definition lives in `buffer_cache_base.rs` and the
//! method implementations live here.

use std::collections::VecDeque;
use std::ptr::NonNull;
use std::sync::Arc;

use common::div_ceil::div_ceil;
use common::lru_cache::LeastRecentlyUsedCache;
use common::range_sets::{OverlapRangeSet, RangeSet};
use common::slot_vector::{SlotId, SlotVector};
use common::types::VAddr;
use parking_lot::ReentrantMutex;
use smallvec::SmallVec;

use crate::control::channel_state::ChannelState;
use crate::control::channel_state_cache::ChannelSetupCaches;
use crate::delayed_destruction_ring::DelayedDestructionRing;
use crate::engines::draw_manager::Maxwell3DAccess;
use crate::engines::kepler_compute::KeplerCompute;
use crate::engines::maxwell_3d::Maxwell3D;
use crate::surface::PixelFormat;

use super::buffer_cache_base::*;
use super::memory_tracker_base::MemoryTrackerBase;
use super::word_manager::DeviceTracker;

// ---------------------------------------------------------------------------
// Cache-level constants (from BufferCache<P> private section)
// ---------------------------------------------------------------------------

/// Page size for caching purposes (unrelated to CPU page size).
const CACHING_PAGEBITS: u32 = 16;

/// Caching page size in bytes.
const CACHING_PAGESIZE: u64 = 1u64 << CACHING_PAGEBITS;

/// Default expected device memory threshold (512 MiB).
const DEFAULT_EXPECTED_MEMORY: u64 = 512 * 1024 * 1024;

/// Default critical device memory threshold (1 GiB).
const DEFAULT_CRITICAL_MEMORY: u64 = 1024 * 1024 * 1024;

/// Target memory threshold (4 GiB).
const TARGET_THRESHOLD: u64 = 4 * 1024 * 1024 * 1024;

/// Debug flag: when true, GPU->CPU downloads are disabled.
const DISABLE_DOWNLOADS: bool = true;

/// Number of page-table entries: covers 2^34 bytes / CACHING_PAGESIZE.
const PAGE_TABLE_SIZE: usize = (1u64 << 34) as usize >> CACHING_PAGEBITS;

/// Stream score threshold above which a buffer region is treated as a stream buffer.
const STREAM_LEAP_THRESHOLD: i32 = 16;

/// Device page size (4 KiB). Matches `Core::DEVICE_PAGESIZE` upstream.
const DEVICE_PAGESIZE: u64 = 4096;

/// Address space bits used by the Maxwell device memory manager.
///
/// Upstream: `Tegra::MaxwellDeviceMemoryManager::AS_BITS = 34`.
const AS_BITS: u32 = 34;

// ---------------------------------------------------------------------------
// BufferCache<P>
// ---------------------------------------------------------------------------

/// The main buffer cache.
///
/// Corresponds to the C++ `BufferCache<P>` template. The generic parameter
/// `P` is the backend policy (see `BufferCacheParams` trait).
pub struct BufferCache<P: BufferCacheParams, DT: DeviceTracker> {
    /// Recursive mutex for external synchronization.
    pub mutex: Arc<ReentrantMutex<()>>,

    // -- Channel state (upstream inherits from ChannelSetupCaches) --
    pub channel_caches: ChannelSetupCaches<BufferCacheChannelInfo>,

    // -- Backend runtime --
    /// The backend runtime that performs actual GPU operations (bind, copy, clear, etc.).
    ///
    /// Upstream: `Runtime& runtime` — stored as a non-owning reference.
    pub(crate) runtime: P::Runtime,
    pub(crate) any_buffer_uploaded: bool,

    // -- GPU memory --
    /// GPU virtual address translation and memory reads.
    ///
    /// Upstream: `Tegra::MemoryManager* gpu_memory` — set per-channel.
    gpu_memory: Option<Box<dyn GpuMemoryAccess>>,

    // -- Device memory --
    /// Guest physical (CPU) memory access.
    ///
    /// Upstream: `Tegra::MaxwellDeviceMemoryManager& device_memory`.
    device_memory: Option<Box<dyn DeviceMemoryAccess>>,

    // -- Draw indirect state --
    /// Current draw indirect parameters.
    ///
    /// Upstream: `const Tegra::Engines::DrawManager::IndirectParams* current_draw_indirect`
    current_draw_indirect: Option<DrawIndirectParams>,

    // -- Slot storage --
    slot_buffers: SlotVector<P::Buffer>,

    // -- Page table: maps device page -> BufferId --
    page_table: Vec<BufferId>,

    // -- Vertex buffer slots --
    // Upstream: enabled_vertex_buffers_mask, vertex_buffers_serial, v_buffer.
    enabled_vertex_buffers_mask: u32,
    vertex_buffers_serial: u64,
    v_buffer: [Binding; 32],

    // -- Memory tracker --
    memory_tracker: MemoryTrackerBase<DT>,

    // -- GPU-modified range tracking --
    uncommitted_gpu_modified_ranges: RangeSet,
    gpu_modified_ranges: RangeSet,
    committed_gpu_modified_ranges: VecDeque<RangeSet>,

    // -- Async buffer downloads --
    async_buffers: VecDeque<Option<P::AsyncBuffer>>,
    pending_downloads: VecDeque<SmallVec<[BufferCopy; 4]>>,

    // -- Async buffers death ring --
    /// Staging buffers that are pending deferred free.
    ///
    /// Upstream: `std::deque<Async_Buffer> async_buffers_death_ring`
    async_buffers_death_ring: VecDeque<P::AsyncBuffer>,

    // -- Async download range tracking --
    /// Tracks ranges with pending async downloads.
    ///
    /// Upstream: `Common::OverlapRangeSet<DAddr> async_downloads`
    async_downloads: OverlapRangeSet,

    // -- Immediate buffer --
    immediate_buffer_alloc: Vec<u8>,

    // -- LRU / GC state --
    /// LRU cache tracking buffer access order for garbage collection.
    ///
    /// Upstream: `Common::LeastRecentlyUsedCache<LRUItemParams> lru_cache`
    /// where `LRUItemParams::ObjectType = BufferId` and `LRUItemParams::TickType = u64`.
    lru_cache: LeastRecentlyUsedCache<BufferId, u64>,

    /// Deferred destruction ring for buffers removed from the cache.
    ///
    /// Upstream: `DelayedDestructionRing<Buffer, 8> delayed_destruction_ring`
    delayed_destruction_ring: DelayedDestructionRing<P::Buffer, 8>,

    frame_tick: u64,
    total_used_memory: u64,
    minimum_memory: u64,
    critical_memory: u64,
    inline_buffer_id: BufferId,

    // -- Scratch buffer --
    tmp_buffer: Vec<u8>,

    /// Marker for the params type.
    _params: std::marker::PhantomData<P>,
}

impl<P: BufferCacheParams, DT: DeviceTracker> BufferCache<P, DT> {
    /// Create a new buffer cache.
    pub fn new(device_tracker: &DT, mut runtime: P::Runtime) -> Self {
        let mut slot_buffers = SlotVector::new();
        // Ensure the first slot is used for the null buffer
        let _null_id = slot_buffers.insert(P::Buffer::null(
            &mut runtime,
            super::buffer_base::NullBufferParams,
        ));
        let (minimum_memory, critical_memory) = if runtime.can_report_memory_usage() {
            let device_local_memory = runtime.get_device_local_memory() as i64;
            let min_spacing_expected = device_local_memory - 1024 * 1024 * 1024;
            let min_spacing_critical = device_local_memory - 512 * 1024 * 1024;
            let mem_threshold = device_local_memory.min(TARGET_THRESHOLD as i64);
            let min_vacancy_expected = (6 * mem_threshold) / 10;
            let min_vacancy_critical = (2 * mem_threshold) / 10;
            (
                (device_local_memory - min_vacancy_expected)
                    .min(min_spacing_expected)
                    .max(DEFAULT_EXPECTED_MEMORY as i64) as u64,
                (device_local_memory - min_vacancy_critical)
                    .min(min_spacing_critical)
                    .max(DEFAULT_CRITICAL_MEMORY as i64) as u64,
            )
        } else {
            (DEFAULT_EXPECTED_MEMORY, DEFAULT_CRITICAL_MEMORY)
        };

        Self {
            mutex: Arc::new(ReentrantMutex::new(())),
            channel_caches: ChannelSetupCaches::new(),
            runtime,
            any_buffer_uploaded: false,
            gpu_memory: None,
            device_memory: None,
            current_draw_indirect: None,
            slot_buffers,
            page_table: vec![SlotId::invalid(); PAGE_TABLE_SIZE],
            enabled_vertex_buffers_mask: 0,
            vertex_buffers_serial: 0,
            v_buffer: [NULL_BINDING; 32],
            memory_tracker: MemoryTrackerBase::new(device_tracker),
            uncommitted_gpu_modified_ranges: RangeSet::new(),
            gpu_modified_ranges: RangeSet::new(),
            committed_gpu_modified_ranges: VecDeque::new(),
            lru_cache: LeastRecentlyUsedCache::new(),
            delayed_destruction_ring: DelayedDestructionRing::new(),
            async_buffers: VecDeque::new(),
            pending_downloads: VecDeque::new(),
            async_buffers_death_ring: VecDeque::new(),
            async_downloads: OverlapRangeSet::new(),
            immediate_buffer_alloc: Vec::new(),
            frame_tick: 0,
            total_used_memory: 0,
            minimum_memory,
            critical_memory,
            inline_buffer_id: NULL_BUFFER_ID,
            tmp_buffer: Vec::new(),
            _params: std::marker::PhantomData,
        }
    }

    /// Set OpenGL texture/image-buffer output arrays.
    ///
    /// Upstream: `BufferCacheRuntime::SetImagePointers`.
    pub fn set_image_pointers(&mut self, texture_handles: *mut u32, image_handles: *mut u32) {
        self.runtime
            .set_image_pointers(texture_handles, image_handles);
    }

    /// Port of OpenGL `BufferCacheRuntime::BindTransformFeedbackObject`.
    pub fn bind_transform_feedback_object(&mut self, tfb_object_addr: u64) {
        self.runtime.bind_transform_feedback_object(tfb_object_addr);
    }

    /// Port of OpenGL `BufferCacheRuntime::GetTransformFeedbackObject`.
    pub fn get_transform_feedback_object(&mut self, tfb_object_addr: u64) -> u32 {
        self.runtime.get_transform_feedback_object(tfb_object_addr)
    }

    pub fn index_offset(&self) -> usize {
        self.runtime.index_offset()
    }

    /// Set the GPU memory manager for GPU->CPU address translation.
    ///
    /// Upstream: `gpu_memory` is set per-channel via channel setup caches.
    pub fn set_gpu_memory(&mut self, gpu_memory: Box<dyn GpuMemoryAccess>) {
        self.gpu_memory = Some(gpu_memory);
    }

    /// Clear the per-channel GPU-memory owner when the bound channel is released.
    pub fn clear_gpu_memory(&mut self) {
        self.gpu_memory = None;
    }

    /// Set the device memory accessor for reading/writing guest physical memory.
    ///
    /// Upstream: `device_memory` is bound at construction.
    pub fn set_device_memory(&mut self, device_memory: Box<dyn DeviceMemoryAccess>) {
        self.device_memory = Some(device_memory);
    }

    /// Port of inherited `ChannelSetupCaches::CreateChannel`.
    pub fn create_channel(&mut self, channel: &ChannelState) {
        self.channel_caches.create_channel(channel);
    }

    /// Port of inherited `ChannelSetupCaches::BindToChannel`.
    pub fn bind_to_channel(&mut self, channel_id: i32) {
        self.channel_caches.bind_to_channel(channel_id);
    }

    /// Port of inherited `ChannelSetupCaches::EraseChannel`.
    pub fn erase_channel(&mut self, channel_id: i32) {
        self.channel_caches.erase_channel(channel_id);
    }

    pub fn current_channel_state(&self) -> Option<&BufferCacheChannelInfo> {
        self.channel_caches.current_channel_state()
    }

    pub fn current_channel_state_mut(&mut self) -> Option<&mut BufferCacheChannelInfo> {
        self.channel_caches.current_channel_state_mut()
    }

    fn maxwell3d(&self) -> Option<&Maxwell3D> {
        let address = self.channel_caches.maxwell3d?;
        // ChannelState owns this boxed engine and outlives every registered
        // cache entry. Bind/erase keeps the raw address synchronized.
        (address != 0).then(|| unsafe { &*(address as *const Maxwell3D) })
    }

    fn maxwell3d_mut(&mut self) -> Option<&mut Maxwell3D> {
        let address = self.channel_caches.maxwell3d?;
        // GPU cache operations are serialized by the rasterizer/cache locks;
        // no second cache operation may dereference this engine concurrently.
        (address != 0).then(|| unsafe { &mut *(address as *mut Maxwell3D) })
    }

    fn kepler_compute(&self) -> Option<&KeplerCompute> {
        let address = self.channel_caches.kepler_compute?;
        // ChannelState owns this boxed engine and outlives every registered
        // cache entry. Bind/erase keeps the raw address synchronized.
        (address != 0).then(|| unsafe { &*(address as *const KeplerCompute) })
    }

    fn geometry_dirty_flag(flag: DirtyFlag) -> usize {
        match flag {
            DirtyFlag::IndexBuffer => crate::dirty_flags::flags::INDEX_BUFFER as usize,
            DirtyFlag::VertexBuffers => crate::dirty_flags::flags::VERTEX_BUFFERS as usize,
            DirtyFlag::VertexBuffer(index) => {
                (crate::dirty_flags::flags::VERTEX_BUFFER0 + index as u8) as usize
            }
        }
    }

    fn is_geometry_dirty(&self, flag: DirtyFlag) -> bool {
        self.maxwell3d()
            .is_some_and(|maxwell| maxwell.dirty_flags()[Self::geometry_dirty_flag(flag)])
    }

    fn clear_geometry_dirty(&mut self, flag: DirtyFlag) {
        if let Some(maxwell) = self.maxwell3d_mut() {
            maxwell.dirty_flags_mut()[Self::geometry_dirty_flag(flag)] = false;
        }
    }

    fn set_geometry_dirty(&mut self, flag: DirtyFlag) {
        if let Some(maxwell) = self.maxwell3d_mut() {
            maxwell.dirty_flags_mut()[Self::geometry_dirty_flag(flag)] = true;
        }
    }

    /// Set the current draw indirect parameters.
    ///
    /// Upstream: `BufferCache<P>::SetDrawIndirect`
    pub fn set_draw_indirect(&mut self, params: Option<DrawIndirectParams>) {
        self.current_draw_indirect = params;
    }

    // -----------------------------------------------------------------------
    // Public API — frame lifecycle
    // -----------------------------------------------------------------------

    /// Advance one frame: run GC, update cache statistics, tick delayed destruction.
    ///
    /// Upstream: `BufferCache<P>::TickFrame`
    ///
    pub fn tick_frame(&mut self) {
        // Homebrew console apps don't create or bind any channels, so this will be None.
        if !self.channel_caches.has_current_channel_state() {
            return;
        }

        self.runtime.tick_frame(&mut self.slot_buffers);

        // Calculate hits and shots and move hit bits to the right (shift history window).
        // Upstream: std::reduce + std::copy_n to shift history arrays left by one.
        if let Some(cs) = self.channel_caches.current_channel_state_mut() {
            let hits: u32 = cs.uniform_cache_hits.iter().copied().sum();
            let shots: u32 = cs.uniform_cache_shots.iter().copied().sum();

            // Shift history: copy [0..N-1] into [1..N], then zero slot 0.
            for i in (1..cs.uniform_cache_hits.len()).rev() {
                cs.uniform_cache_hits[i] = cs.uniform_cache_hits[i - 1];
            }
            for i in (1..cs.uniform_cache_shots.len()).rev() {
                cs.uniform_cache_shots[i] = cs.uniform_cache_shots[i - 1];
            }
            cs.uniform_cache_hits[0] = 0;
            cs.uniform_cache_shots[0] = 0;

            // Determine whether to skip the cache for small uniform buffers.
            // Upstream: skip_preferred = hits * 256 < shots * 251
            let skip_preferred = hits.wrapping_mul(256) < shots.wrapping_mul(251);
            cs.uniform_buffer_skip_cache_size = if skip_preferred {
                DEFAULT_SKIP_CACHE_SIZE
            } else {
                0
            };
        }

        if self.runtime.can_report_memory_usage() {
            self.total_used_memory = self.runtime.get_device_memory_usage();
        }

        if self.total_used_memory >= self.minimum_memory {
            self.run_garbage_collector();
        }

        self.frame_tick += 1;

        self.delayed_destruction_ring.tick();

        // Free deferred staging buffers from last frame.
        // Upstream: for (auto& buffer : async_buffers_death_ring) {
        //     runtime.FreeDeferredStagingBuffer(buffer);
        // }
        for buffer in self.async_buffers_death_ring.iter_mut() {
            self.runtime.free_deferred_staging_buffer(buffer);
        }
        self.async_buffers_death_ring.clear();
    }

    // -----------------------------------------------------------------------
    // Public API — memory writes
    // -----------------------------------------------------------------------

    /// Notify the cache that a CPU write happened at `[device_addr, device_addr+size)`.
    ///
    /// Upstream: `BufferCache<P>::WriteMemory`
    pub fn write_memory(&mut self, device_addr: VAddr, size: u64) {
        if self
            .memory_tracker
            .is_region_gpu_modified(device_addr, size)
        {
            self.clear_download(device_addr, size);
            self.gpu_modified_ranges
                .subtract(device_addr, size as usize);
        }
        self.memory_tracker
            .mark_region_as_cpu_modified(device_addr, size);
    }

    /// Notify the cache about a cached (deferred) CPU write.
    ///
    /// Upstream: `BufferCache<P>::CachedWriteMemory`
    ///
    /// NOTE: `device_memory.ReadBlockUnsafe` is not available; the inline path falls back to
    /// `write_memory` for non-GPU-modified regions and logs a warning for the inline path.
    pub fn cached_write_memory(&mut self, device_addr: VAddr, size: u64) {
        let is_dirty = self.is_region_registered(device_addr, size as usize);
        if !is_dirty {
            return;
        }
        let aligned_start = device_addr & !(DEVICE_PAGESIZE - 1);
        let aligned_end = (device_addr + size + DEVICE_PAGESIZE - 1) & !(DEVICE_PAGESIZE - 1);
        if !self.is_region_gpu_modified(aligned_start, (aligned_end - aligned_start) as usize) {
            self.write_memory(device_addr, size);
            return;
        }
        // Upstream: device_memory.ReadBlockUnsafe(device_addr, tmp_buffer.data(), size)
        //           InlineMemoryImplementation(device_addr, size, tmp_buffer)
        if let Some(ref dm) = self.device_memory {
            self.tmp_buffer.resize(size as usize, 0);
            dm.read_block_unsafe(device_addr, &mut self.tmp_buffer);
            let buf_copy: Vec<u8> = self.tmp_buffer[..size as usize].to_vec();
            self.inline_memory_implementation(device_addr, size as usize, &buf_copy);
        } else {
            log::warn!(
                "cached_write_memory: GPU-modified region at {:#x}+{} — device_memory \
                 not available; falling back to write_memory",
                device_addr,
                size
            );
            self.write_memory(device_addr, size);
        }
    }

    /// Called when a CPU write is detected. Returns true if the caller must
    /// flush GPU-modified data first.
    ///
    /// Upstream: `BufferCache<P>::OnCPUWrite`
    pub fn on_cpu_write(&mut self, device_addr: VAddr, size: u64) -> bool {
        let is_dirty = self.is_region_registered(device_addr, size as usize);
        if !is_dirty {
            return false;
        }
        if self
            .memory_tracker
            .is_region_gpu_modified(device_addr, size)
        {
            return true;
        }
        self.write_memory(device_addr, size);
        false
    }

    /// Download GPU-modified memory back to the CPU for the given range.
    ///
    /// Upstream: `BufferCache<P>::DownloadMemory`
    pub fn download_memory(&mut self, device_addr: VAddr, size: u64) {
        let mut buffer_ids = Vec::new();
        self.for_each_buffer_in_range(device_addr, size, |buffer_id, _buffer| {
            buffer_ids.push(buffer_id);
        });
        for buffer_id in buffer_ids {
            self.download_buffer_memory_range(buffer_id, device_addr, size);
        }
    }

    /// Get the flush area for a device address range.
    ///
    /// Upstream: `BufferCache<P>::GetFlushArea`
    pub fn get_flush_area(
        &mut self,
        device_addr: VAddr,
        size: u64,
    ) -> Option<RasterizerDownloadArea> {
        let device_addr_start_aligned = device_addr & !(DEVICE_PAGESIZE - 1);
        let device_addr_end_aligned =
            (device_addr + size + DEVICE_PAGESIZE - 1) & !(DEVICE_PAGESIZE - 1);

        if self
            .memory_tracker
            .is_region_preflushable(device_addr, size)
        {
            return Some(RasterizerDownloadArea {
                start_address: device_addr_start_aligned,
                end_address: device_addr_end_aligned,
                preemtive: true,
            });
        }

        let preemtive = !self.is_region_gpu_modified(
            device_addr_start_aligned,
            (device_addr_end_aligned - device_addr_start_aligned) as usize,
        );
        self.memory_tracker.mark_region_as_preflushable(
            device_addr_start_aligned,
            device_addr_end_aligned - device_addr_start_aligned,
        );

        Some(RasterizerDownloadArea {
            start_address: device_addr_start_aligned,
            end_address: device_addr_end_aligned,
            preemtive,
        })
    }

    /// Inline a small memory write directly into the buffer.
    ///
    /// Upstream: `BufferCache<P>::InlineMemory`
    pub fn inline_memory(
        &mut self,
        dest_address: VAddr,
        copy_size: usize,
        inlined_buffer: &[u8],
    ) -> bool {
        let is_dirty = self.is_region_registered(dest_address, copy_size);
        if !is_dirty {
            return false;
        }
        let aligned_start = dest_address & !(DEVICE_PAGESIZE - 1);
        let aligned_end =
            (dest_address + copy_size as u64 + DEVICE_PAGESIZE - 1) & !(DEVICE_PAGESIZE - 1);
        if !self.is_region_gpu_modified(aligned_start, (aligned_end - aligned_start) as usize) {
            return false;
        }
        self.inline_memory_implementation(dest_address, copy_size, inlined_buffer);
        true
    }

    // -----------------------------------------------------------------------
    // Public API — buffer binding (graphics)
    // -----------------------------------------------------------------------

    /// Bind a graphics uniform buffer.
    ///
    /// Upstream: `BufferCache<P>::BindGraphicsUniformBuffer`
    pub fn bind_graphics_uniform_buffer(
        &mut self,
        stage: usize,
        index: u32,
        gpu_addr: u64,
        size: u32,
    ) {
        // Upstream: const std::optional<DAddr> device_addr = gpu_memory->GpuToCpuAddress(gpu_addr);
        let device_addr = self
            .gpu_memory
            .as_ref()
            .and_then(|gm| gm.gpu_to_cpu_address(gpu_addr))
            .expect("uniform-buffer GPU address must map to device memory");
        self.bind_graphics_uniform_buffer_with_device_addr(stage, index, device_addr, size);
    }

    /// Bind a graphics uniform buffer with an already-resolved device address.
    ///
    /// This mirrors `BindGraphicsUniformBuffer` but lets the OpenGL rasterizer
    /// reuse an already-held channel memory-manager guard while it holds the
    /// cache mutexes. Upstream reads `gpu_memory->GpuToCpuAddress` through a
    /// raw Maxwell/GPU pointer, not through an extra Rust mutex.
    pub fn bind_graphics_uniform_buffer_with_device_addr(
        &mut self,
        stage: usize,
        index: u32,
        device_addr: u64,
        size: u32,
    ) {
        let Some(cs) = self.channel_caches.current_channel_state_mut() else {
            return;
        };
        let binding = Binding {
            device_addr,
            size,
            buffer_id: NULL_BUFFER_ID,
        };
        if stage < NUM_STAGES as usize && (index as usize) < NUM_GRAPHICS_UNIFORM_BUFFERS as usize {
            cs.uniform_buffers[stage][index as usize] = binding;
        }
    }

    /// Disable a graphics uniform buffer.
    ///
    /// Upstream: `BufferCache<P>::DisableGraphicsUniformBuffer`
    pub fn disable_graphics_uniform_buffer(&mut self, stage: usize, index: u32) {
        let Some(cs) = self.channel_caches.current_channel_state_mut() else {
            return;
        };
        if stage < NUM_STAGES as usize && (index as usize) < NUM_GRAPHICS_UNIFORM_BUFFERS as usize {
            cs.uniform_buffers[stage][index as usize] = NULL_BINDING;
        }
    }

    /// Update all graphics buffer bindings.
    ///
    /// Upstream: `BufferCache<P>::UpdateGraphicsBuffers`
    pub fn update_graphics_buffers(&mut self, is_indexed: bool) {
        if !self.channel_caches.has_current_channel_state() {
            return;
        }
        loop {
            if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                cs.has_deleted_buffers = false;
            }
            self.do_update_graphics_buffers(is_indexed);
            if let Some(cs) = self.channel_caches.current_channel_state() {
                if !cs.has_deleted_buffers {
                    break;
                }
            } else {
                break;
            }
        }
    }

    /// Update graphics buffer bindings using caller-provided GPU address helpers.
    ///
    /// This mirrors `UpdateGraphicsBuffers` but lets the OpenGL rasterizer reuse
    /// an already-held channel memory-manager guard while cache mutexes are held.
    /// Upstream reads these values through `gpu_memory` without an extra Rust
    /// mutex, so this preserves ordering without re-entering the channel lock.
    pub fn update_graphics_buffers_with_gpu_resolver(
        &mut self,
        is_indexed: bool,
        mut gpu_to_cpu_address: impl FnMut(u64) -> Option<u64>,
        mut is_within_gpu_address_range: impl FnMut(u64) -> bool,
        mut max_continuous_range: impl FnMut(u64, u64) -> u64,
    ) {
        if !self.channel_caches.has_current_channel_state() {
            return;
        }
        loop {
            if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                cs.has_deleted_buffers = false;
            }
            self.do_update_graphics_buffers_with_gpu_resolver(
                is_indexed,
                &mut gpu_to_cpu_address,
                &mut is_within_gpu_address_range,
                &mut max_continuous_range,
            );
            if let Some(cs) = self.channel_caches.current_channel_state() {
                if !cs.has_deleted_buffers {
                    break;
                }
            } else {
                break;
            }
        }
    }

    /// Update all compute buffer bindings.
    ///
    /// Upstream: `BufferCache<P>::UpdateComputeBuffers`
    pub fn update_compute_buffers(&mut self) {
        if !self.channel_caches.has_current_channel_state() {
            return;
        }
        loop {
            if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                cs.has_deleted_buffers = false;
            }
            self.do_update_compute_buffers();
            if let Some(cs) = self.channel_caches.current_channel_state() {
                if !cs.has_deleted_buffers {
                    break;
                }
            } else {
                break;
            }
        }
    }

    /// Bind host geometry buffers (index + vertex).
    ///
    /// Upstream: `BufferCache<P>::BindHostGeometryBuffers`
    ///
    pub fn bind_host_geometry_buffers(&mut self, is_indexed: bool) {
        if is_indexed {
            self.bind_host_index_buffer();
        } else if !P::HAS_FULL_INDEX_AND_PRIMITIVE_SUPPORT {
            let quad_draw = self.maxwell3d().and_then(|maxwell| {
                let draw_state = maxwell.draw_manager_state();
                matches!(
                    draw_state.topology,
                    crate::engines::maxwell_3d::PrimitiveTopology::Quads
                        | crate::engines::maxwell_3d::PrimitiveTopology::QuadStrip
                )
                .then_some((
                    draw_state.topology,
                    draw_state.vertex_buffer.first,
                    draw_state.vertex_buffer.count,
                ))
            });
            if let Some((topology, first, count)) = quad_draw {
                self.runtime.bind_quad_index_buffer(topology, first, count);
            }
        }
        self.bind_host_vertex_buffers();
        self.bind_host_transform_feedback_buffers();
        // Upstream: if (current_draw_indirect) { BindHostDrawIndirectBuffers(); }
        if self.current_draw_indirect.is_some() {
            self.bind_host_draw_indirect_buffers();
        }
    }

    /// Bind host stage buffers.
    ///
    /// Upstream: `BufferCache<P>::BindHostStageBuffers`
    pub fn bind_host_stage_buffers(&mut self, stage: usize) {
        self.bind_host_graphics_uniform_buffers(stage);
        self.bind_host_graphics_storage_buffers(stage);
        self.bind_host_graphics_texture_buffers(stage);
    }

    /// Bind host compute buffers.
    ///
    /// Upstream: `BufferCache<P>::BindHostComputeBuffers`
    pub fn bind_host_compute_buffers(&mut self) {
        self.bind_host_compute_uniform_buffers();
        self.bind_host_compute_storage_buffers();
        self.bind_host_compute_texture_buffers();
        if self.any_buffer_uploaded {
            self.runtime.post_copy_barrier();
            self.any_buffer_uploaded = false;
        }
    }

    /// Set the uniform buffer state for graphics stages.
    ///
    /// Upstream: `BufferCache<P>::SetUniformBuffersState`
    /// # Safety
    ///
    /// `sizes` must remain at a stable address until another graphics
    /// pipeline replaces it or the channel is destroyed. Upstream stores the
    /// same non-owning pointer in `BufferCacheChannelInfo`.
    pub unsafe fn set_uniform_buffers_state(
        &mut self,
        mask: &[u32; NUM_STAGES as usize],
        sizes: &UniformBufferSizes,
    ) {
        let Some(cs) = self.channel_caches.current_channel_state_mut() else {
            return;
        };
        if cs.enabled_uniform_buffer_masks != *mask {
            cs.fast_bound_uniform_buffers.fill(0);
            if P::HAS_PERSISTENT_UNIFORM_BUFFER_BINDINGS {
                cs.dirty_uniform_buffers.fill(!0u32);
                cs.uniform_buffer_binding_sizes =
                    [[0u32; NUM_GRAPHICS_UNIFORM_BUFFERS as usize]; NUM_STAGES as usize];
            }
        }
        cs.enabled_uniform_buffer_masks = *mask;
        cs.uniform_buffer_sizes = Some(NonNull::from(sizes));
    }

    /// Set the uniform buffer state for compute.
    ///
    /// Upstream: `BufferCache<P>::SetComputeUniformBufferState`
    /// # Safety
    ///
    /// `sizes` must remain at a stable address until another compute pipeline
    /// replaces it or the channel is destroyed. Upstream stores the same
    /// non-owning pointer in `BufferCacheChannelInfo`.
    pub unsafe fn set_compute_uniform_buffer_state(
        &mut self,
        mask: u32,
        sizes: &ComputeUniformBufferSizes,
    ) {
        let Some(cs) = self.channel_caches.current_channel_state_mut() else {
            return;
        };
        cs.enabled_compute_uniform_buffer_mask = mask;
        cs.compute_uniform_buffer_sizes = Some(NonNull::from(sizes));
    }

    // -----------------------------------------------------------------------
    // Public API — storage buffers
    // -----------------------------------------------------------------------

    /// Unbind all graphics storage buffers for a stage.
    ///
    /// Upstream: `BufferCache<P>::UnbindGraphicsStorageBuffers`
    pub fn unbind_graphics_storage_buffers(&mut self, stage: usize) {
        let limit_dynamic_storage_buffers = self.runtime.should_limit_dynamic_storage_buffers();
        let Some(cs) = self.channel_caches.current_channel_state_mut() else {
            return;
        };
        if stage < NUM_STAGES as usize {
            if limit_dynamic_storage_buffers {
                cs.total_graphics_storage_buffers = cs
                    .total_graphics_storage_buffers
                    .wrapping_sub(cs.enabled_storage_buffers[stage].count_ones());
            }
            cs.enabled_storage_buffers[stage] = 0;
            cs.written_storage_buffers[stage] = 0;
        }
    }

    /// Bind a graphics storage buffer.
    ///
    /// Upstream: `BufferCache<P>::BindGraphicsStorageBuffer`
    ///
    pub fn bind_graphics_storage_buffer(
        &mut self,
        stage: usize,
        ssbo_index: usize,
        cbuf_index: u32,
        cbuf_offset: u32,
        is_written: bool,
    ) -> bool {
        if stage >= NUM_STAGES as usize || ssbo_index >= NUM_STORAGE_BUFFERS as usize {
            return false;
        }
        let limit_dynamic_storage_buffers = self.runtime.should_limit_dynamic_storage_buffers();
        let max_dynamic_storage_buffers = self.runtime.max_dynamic_storage_buffers();
        {
            let Some(cs) = self.channel_caches.current_channel_state_mut() else {
                return false;
            };
            let already_enabled = ((cs.enabled_storage_buffers[stage] >> ssbo_index) & 1) != 0;
            if limit_dynamic_storage_buffers && !already_enabled {
                if cs.total_graphics_storage_buffers >= max_dynamic_storage_buffers {
                    log::warn!(
                        "Skipping graphics storage buffer {} due to driver limit {}",
                        ssbo_index,
                        max_dynamic_storage_buffers
                    );
                    return false;
                }
            }
            cs.enabled_storage_buffers[stage] |= 1u32 << ssbo_index;
            cs.written_storage_buffers[stage] |=
                (if is_written { 1u32 } else { 0u32 }) << ssbo_index;
            if limit_dynamic_storage_buffers && !already_enabled {
                cs.total_graphics_storage_buffers =
                    cs.total_graphics_storage_buffers.wrapping_add(1);
            }
        }

        // Upstream: const auto& cbufs = maxwell3d->state.shader_stages[stage];
        //           const GPUVAddr ssbo_addr = cbufs.const_buffers[cbuf_index].address + cbuf_offset;
        //           channel_state->storage_buffers[stage][ssbo_index] =
        //               StorageBufferBinding(ssbo_addr, cbuf_index, is_written);
        let binding = if let Some(maxwell) = self.maxwell3d() {
            let cbuf = maxwell.const_buffer_bindings(stage)[cbuf_index as usize];
            let ssbo_addr = cbuf.address.wrapping_add(cbuf_offset as u64);
            self.storage_buffer_binding(ssbo_addr, cbuf_index, is_written)
        } else {
            NULL_BINDING
        };
        if let Some(cs) = self.channel_caches.current_channel_state_mut() {
            cs.storage_buffers[stage][ssbo_index] = binding;
        }
        binding.buffer_id != NULL_BUFFER_ID
    }

    /// Bind a graphics storage buffer using a caller-provided GPU-memory reader.
    ///
    /// This mirrors `BindGraphicsStorageBuffer` but lets the OpenGL rasterizer
    /// reuse an already-held channel memory-manager guard while it holds the
    /// cache mutexes. Upstream reads through `gpu_memory->Read<T>` without an
    /// extra Rust mutex; this path preserves the same behavior without
    /// re-locking the channel memory manager.
    pub fn bind_graphics_storage_buffer_with_gpu_reader(
        &mut self,
        stage: usize,
        ssbo_index: usize,
        cbuf_index: u32,
        cbuf_offset: u32,
        is_written: bool,
        mut gpu_to_cpu_address: impl FnMut(u64) -> Option<u64>,
        mut get_memory_layout_size: impl FnMut(u64) -> u64,
        mut read_block: impl FnMut(u64, &mut [u8]) -> bool,
    ) -> bool {
        if stage >= NUM_STAGES as usize || ssbo_index >= NUM_STORAGE_BUFFERS as usize {
            return false;
        }
        let limit_dynamic_storage_buffers = self.runtime.should_limit_dynamic_storage_buffers();
        let max_dynamic_storage_buffers = self.runtime.max_dynamic_storage_buffers();
        {
            let Some(cs) = self.channel_caches.current_channel_state_mut() else {
                return false;
            };
            let already_enabled = ((cs.enabled_storage_buffers[stage] >> ssbo_index) & 1) != 0;
            if limit_dynamic_storage_buffers && !already_enabled {
                if cs.total_graphics_storage_buffers >= max_dynamic_storage_buffers {
                    log::warn!(
                        "Skipping graphics storage buffer {} due to driver limit {}",
                        ssbo_index,
                        max_dynamic_storage_buffers
                    );
                    return false;
                }
            }
            cs.enabled_storage_buffers[stage] |= 1u32 << ssbo_index;
            cs.written_storage_buffers[stage] |=
                (if is_written { 1u32 } else { 0u32 }) << ssbo_index;
            if limit_dynamic_storage_buffers && !already_enabled {
                cs.total_graphics_storage_buffers =
                    cs.total_graphics_storage_buffers.wrapping_add(1);
            }
        }

        let binding = if let Some(maxwell) = self.maxwell3d() {
            let cbuf = maxwell.const_buffer_bindings(stage)[cbuf_index as usize];
            let ssbo_addr = cbuf.address.wrapping_add(cbuf_offset as u64);
            self.storage_buffer_binding_with_gpu_reader(
                ssbo_addr,
                cbuf_index,
                is_written,
                &mut gpu_to_cpu_address,
                &mut get_memory_layout_size,
                &mut read_block,
            )
        } else {
            NULL_BINDING
        };
        if let Some(cs) = self.channel_caches.current_channel_state_mut() {
            cs.storage_buffers[stage][ssbo_index] = binding;
        }
        binding.buffer_id != NULL_BUFFER_ID
    }

    /// Unbind all compute storage buffers.
    ///
    /// Upstream: `BufferCache<P>::UnbindComputeStorageBuffers`
    pub fn unbind_compute_storage_buffers(&mut self) {
        let limit_dynamic_storage_buffers = self.runtime.should_limit_dynamic_storage_buffers();
        let Some(cs) = self.channel_caches.current_channel_state_mut() else {
            return;
        };
        if limit_dynamic_storage_buffers {
            cs.total_compute_storage_buffers = cs
                .total_compute_storage_buffers
                .wrapping_sub(cs.enabled_compute_storage_buffers.count_ones());
        }
        cs.enabled_compute_storage_buffers = 0;
        cs.written_compute_storage_buffers = 0;
        cs.image_compute_texture_buffers = 0;
    }

    /// Bind a compute storage buffer.
    ///
    /// Upstream: `BufferCache<P>::BindComputeStorageBuffer`
    pub fn bind_compute_storage_buffer(
        &mut self,
        ssbo_index: usize,
        cbuf_index: u32,
        cbuf_offset: u32,
        is_written: bool,
    ) {
        let limit_dynamic_storage_buffers = self.runtime.should_limit_dynamic_storage_buffers();
        let max_dynamic_storage_buffers = self.runtime.max_dynamic_storage_buffers();
        let Some(cs) = self.channel_caches.current_channel_state_mut() else {
            return;
        };
        if ssbo_index >= cs.compute_storage_buffers.len() {
            log::error!(
                "bind_compute_storage_buffer: index {} exceeds maximum storage buffer count",
                ssbo_index
            );
            return;
        }
        let already_enabled = ((cs.enabled_compute_storage_buffers >> ssbo_index) & 1) != 0;
        if limit_dynamic_storage_buffers && !already_enabled {
            if cs.total_compute_storage_buffers >= max_dynamic_storage_buffers {
                log::warn!(
                    "Skipping compute storage buffer {} due to driver limit {}",
                    ssbo_index,
                    max_dynamic_storage_buffers
                );
                return;
            }
        }
        cs.enabled_compute_storage_buffers |= 1u32 << ssbo_index;
        cs.written_compute_storage_buffers |= (if is_written { 1u32 } else { 0u32 }) << ssbo_index;
        if limit_dynamic_storage_buffers && !already_enabled {
            cs.total_compute_storage_buffers = cs.total_compute_storage_buffers.wrapping_add(1);
        }

        let ssbo_addr = {
            let launch_desc = self
                .kepler_compute()
                .expect("bound BufferCache channel must own KeplerCompute")
                .launch_description();
            if ((launch_desc.const_buffer_enable_mask >> cbuf_index) & 1) == 0 {
                log::warn!("Skipped binding SSBO: cbuf index {cbuf_index} is not enabled");
                return;
            }
            assert_ne!((launch_desc.const_buffer_enable_mask >> cbuf_index) & 1, 0);
            launch_desc.const_buffers[cbuf_index as usize]
                .address
                .wrapping_add(cbuf_offset as u64)
        };
        let binding = self.storage_buffer_binding(ssbo_addr, cbuf_index, is_written);
        if let Some(cs) = self.channel_caches.current_channel_state_mut() {
            cs.compute_storage_buffers[ssbo_index] = binding;
        }
    }

    // -----------------------------------------------------------------------
    // Public API — texture buffers
    // -----------------------------------------------------------------------

    /// Unbind all graphics texture buffers for a stage.
    ///
    /// Upstream: `BufferCache<P>::UnbindGraphicsTextureBuffers`
    pub fn unbind_graphics_texture_buffers(&mut self, stage: usize) {
        let Some(cs) = self.channel_caches.current_channel_state_mut() else {
            return;
        };
        if stage < NUM_STAGES as usize {
            cs.enabled_texture_buffers[stage] = 0;
            cs.written_texture_buffers[stage] = 0;
            cs.image_texture_buffers[stage] = 0;
        }
    }

    /// Bind a graphics texture buffer.
    ///
    /// Upstream: `BufferCache<P>::BindGraphicsTextureBuffer`
    pub fn bind_graphics_texture_buffer(
        &mut self,
        stage: usize,
        tbo_index: usize,
        gpu_addr: u64,
        size: u32,
        format: PixelFormat,
        is_written: bool,
        is_image: bool,
    ) {
        {
            let Some(cs) = self.channel_caches.current_channel_state_mut() else {
                return;
            };
            cs.enabled_texture_buffers[stage] |= 1u32 << tbo_index;
            cs.written_texture_buffers[stage] |=
                (if is_written { 1u32 } else { 0u32 }) << tbo_index;
            if P::SEPARATE_IMAGE_BUFFER_BINDINGS {
                cs.image_texture_buffers[stage] |=
                    (if is_image { 1u32 } else { 0u32 }) << tbo_index;
            }
        }
        let binding = self.get_texture_buffer_binding(gpu_addr, size, format);
        if let Some(cs) = self.channel_caches.current_channel_state_mut() {
            cs.texture_buffers[stage][tbo_index] = binding;
        }
    }

    /// Unbind all compute texture buffers.
    ///
    /// Upstream: `BufferCache<P>::UnbindComputeTextureBuffers`
    pub fn unbind_compute_texture_buffers(&mut self) {
        let Some(cs) = self.channel_caches.current_channel_state_mut() else {
            return;
        };
        cs.enabled_compute_texture_buffers = 0;
        cs.written_compute_texture_buffers = 0;
        cs.image_compute_texture_buffers = 0;
    }

    /// Bind a compute texture buffer.
    ///
    /// Upstream: `BufferCache<P>::BindComputeTextureBuffer`
    pub fn bind_compute_texture_buffer(
        &mut self,
        tbo_index: usize,
        gpu_addr: u64,
        size: u32,
        format: PixelFormat,
        is_written: bool,
        is_image: bool,
    ) {
        if tbo_index >= NUM_TEXTURE_BUFFERS as usize {
            log::error!(
                "bind_compute_texture_buffer: index {} exceeds maximum texture buffer count",
                tbo_index
            );
            return;
        }
        {
            let Some(cs) = self.channel_caches.current_channel_state_mut() else {
                return;
            };
            cs.enabled_compute_texture_buffers |= 1u32 << tbo_index;
            cs.written_compute_texture_buffers |=
                (if is_written { 1u32 } else { 0u32 }) << tbo_index;
            if P::SEPARATE_IMAGE_BUFFER_BINDINGS {
                cs.image_compute_texture_buffers |=
                    (if is_image { 1u32 } else { 0u32 }) << tbo_index;
            }
        }
        let binding = self.get_texture_buffer_binding(gpu_addr, size, format);
        if let Some(cs) = self.channel_caches.current_channel_state_mut() {
            cs.compute_texture_buffers[tbo_index] = binding;
        }
    }

    // -----------------------------------------------------------------------
    // Public API — obtain buffers
    // -----------------------------------------------------------------------

    /// Obtain a buffer by GPU virtual address.
    ///
    /// Returns `(buffer_id, offset)` within the buffer.
    ///
    /// Upstream: `BufferCache<P>::ObtainBuffer`
    pub fn obtain_buffer(
        &mut self,
        gpu_addr: u64,
        size: u32,
        sync_info: ObtainBufferSynchronize,
        post_op: ObtainBufferOperation,
    ) -> (BufferId, u32) {
        // Upstream: const std::optional<DAddr> device_addr = gpu_memory->GpuToCpuAddress(gpu_addr);
        let device_addr = self
            .gpu_memory
            .as_ref()
            .and_then(|gm| gm.gpu_to_cpu_address(gpu_addr));
        match device_addr {
            Some(addr) => self.obtain_cpu_buffer(addr, size, sync_info, post_op),
            None => (NULL_BUFFER_ID, 0),
        }
    }

    /// Obtain a buffer by CPU/device address.
    ///
    /// Upstream: `BufferCache<P>::ObtainCPUBuffer`
    pub fn obtain_cpu_buffer(
        &mut self,
        device_addr: VAddr,
        size: u32,
        sync_info: ObtainBufferSynchronize,
        post_op: ObtainBufferOperation,
    ) -> (BufferId, u32) {
        let buffer_id = self.find_buffer(device_addr, size);

        match sync_info {
            ObtainBufferSynchronize::FullSynchronize => {
                self.synchronize_buffer(buffer_id, device_addr, size);
            }
            _ => {}
        }

        match post_op {
            ObtainBufferOperation::MarkAsWritten => {
                self.mark_written_buffer(buffer_id, device_addr, size);
            }
            ObtainBufferOperation::DiscardWrite => {
                let device_addr_start = device_addr & !63u64; // AlignDown(device_addr, 64)
                let device_addr_end = (device_addr + size as u64 + 63) & !63u64;
                let new_size = device_addr_end - device_addr_start;
                self.clear_download(device_addr_start, new_size);
                self.gpu_modified_ranges
                    .subtract(device_addr_start, new_size as usize);
            }
            _ => {}
        }

        let offset = self.slot_buffers[buffer_id].offset(device_addr);
        (buffer_id, offset)
    }

    // -----------------------------------------------------------------------
    // Public API — flush / commit
    // -----------------------------------------------------------------------

    /// Flush all cached CPU writes.
    ///
    /// Upstream: `BufferCache<P>::FlushCachedWrites`
    pub fn flush_cached_writes(&mut self) {
        self.memory_tracker.flush_cached_writes();
    }

    /// Return true when there are uncommitted buffers to be downloaded.
    pub fn has_uncommitted_flushes(&self) -> bool {
        !self.uncommitted_gpu_modified_ranges.empty()
            || !self.committed_gpu_modified_ranges.is_empty()
    }

    /// Accumulate current uncommitted ranges into committed.
    ///
    /// Upstream: `BufferCache<P>::AccumulateFlushes`
    pub fn accumulate_flushes(&mut self) {
        if self.uncommitted_gpu_modified_ranges.empty() {
            return;
        }
        // Move uncommitted ranges into a new committed slot.
        let ranges = std::mem::replace(&mut self.uncommitted_gpu_modified_ranges, RangeSet::new());
        self.committed_gpu_modified_ranges.push_back(ranges);
    }

    /// Return true when the caller should wait for async flushes.
    pub fn should_wait_async_flushes(&self) -> bool {
        self.async_buffers
            .front()
            .is_some_and(|buffer| buffer.is_some())
    }

    /// Commit asynchronous downloads.
    ///
    /// Upstream: `BufferCache<P>::CommitAsyncFlushes` delegates to CommitAsyncFlushesHigh.
    pub fn commit_async_flushes(&mut self) {
        self.commit_async_flushes_high();
    }

    /// Commit asynchronous downloads (high priority).
    ///
    /// Upstream: `BufferCache<P>::CommitAsyncFlushesHigh`
    ///
    pub fn commit_async_flushes_high(&mut self) {
        self.accumulate_flushes();

        if self.committed_gpu_modified_ranges.is_empty() {
            self.async_buffers.push_back(None);
            return;
        }

        // Upstream: subtract later committed ranges from earlier ones to avoid double-downloads.
        let committed_ranges = self.committed_gpu_modified_ranges.make_contiguous();
        for index in 0..committed_ranges.len() {
            let (current_and_previous, later_ranges) = committed_ranges.split_at_mut(index + 1);
            let current_intervals = &mut current_and_previous[index];
            for later in later_ranges {
                later.for_each(|start, end| {
                    current_intervals.subtract(start, (end - start) as usize);
                });
            }
        }

        let mut downloads: SmallVec<[(BufferCopy, BufferId); 16]> = SmallVec::new();
        let mut total_size_bytes = 0u64;
        let mut _largest_copy = 0u64;
        {
            let committed_ranges = &self.committed_gpu_modified_ranges;
            let page_table = &self.page_table;
            let slot_buffers = &mut self.slot_buffers;
            let memory_tracker = &mut self.memory_tracker;
            let gpu_modified_ranges = &self.gpu_modified_ranges;

            for range_set in committed_ranges {
                range_set.for_each(|interval_lower, interval_upper| {
                    let size = interval_upper - interval_lower;
                    let device_addr = interval_lower;
                    Self::for_each_buffer_in_range_impl(
                        page_table,
                        slot_buffers,
                        device_addr,
                        size,
                        |buffer_id, buffer| {
                            let buffer_start = buffer.cpu_addr();
                            let buffer_end = buffer_start + buffer.size_bytes() as u64;
                            let new_start = buffer_start.max(device_addr);
                            let new_end = buffer_end.min(device_addr + size);
                            memory_tracker.for_each_download_range(
                                new_start,
                                new_end - new_start,
                                false,
                                &mut |device_addr_out, range_size| {
                                    let buffer_addr = buffer.cpu_addr();
                                    gpu_modified_ranges.for_each_in_range(
                                        device_addr_out,
                                        range_size as usize,
                                        |start, end| {
                                            let new_offset = start - buffer_addr;
                                            let new_size = end - start;
                                            downloads.push((
                                                BufferCopy {
                                                    src_offset: new_offset,
                                                    dst_offset: total_size_bytes,
                                                    size: new_size,
                                                },
                                                buffer_id,
                                            ));
                                            const ALIGN: u64 = 64;
                                            const MASK: u64 = !(ALIGN - 1);
                                            total_size_bytes += (new_size + ALIGN - 1) & MASK;
                                            _largest_copy = _largest_copy.max(new_size);
                                        },
                                    );
                                },
                            );
                        },
                    );
                });
            }
        }
        self.committed_gpu_modified_ranges.clear();

        if downloads.is_empty() {
            self.async_buffers.push_back(None);
            return;
        }

        // Upstream: allocate staging, copy GPU→staging, track for async pop.
        let download_staging = self.runtime.download_staging_buffer(total_size_bytes, true);
        let mut normalized_copies: SmallVec<[BufferCopy; 4]> = SmallVec::new();
        self.runtime.pre_copy_barrier();
        for (copy, buffer_id) in &mut downloads {
            copy.dst_offset += download_staging.offset();
            let copies = [*copy];
            let mut normalized_copy = *copy;
            normalized_copy.src_offset = self.slot_buffers[*buffer_id].cpu_addr() + copy.src_offset;
            let orig_device_addr = normalized_copy.src_offset;
            self.async_downloads
                .add(orig_device_addr, copy.size as usize);
            self.slot_buffers[*buffer_id].mark_usage(copy.src_offset, copy.size);
            self.runtime.copy_buffer_to_staging(
                &download_staging,
                &self.slot_buffers[*buffer_id],
                &copies,
                false,
            );
            normalized_copies.push(normalized_copy);
        }
        self.runtime.post_copy_barrier();

        self.pending_downloads.push_back(normalized_copies);
        self.async_buffers.push_back(Some(download_staging));
    }

    /// Pop completed asynchronous downloads.
    ///
    /// Upstream: `BufferCache<P>::PopAsyncFlushes` delegates to PopAsyncBuffers.
    pub fn pop_async_flushes(&mut self) {
        self.pop_async_buffers();
    }

    #[cfg(test)]
    pub fn test_add_uncommitted_gpu_modified_range(&mut self, addr: u64, size: usize) {
        self.uncommitted_gpu_modified_ranges.add(addr, size);
    }

    #[cfg(test)]
    pub fn test_uncommitted_gpu_modified_ranges_empty(&self) -> bool {
        self.uncommitted_gpu_modified_ranges.empty()
    }

    #[cfg(test)]
    pub fn test_committed_gpu_modified_range_count(&self) -> usize {
        self.committed_gpu_modified_ranges.len()
    }

    #[cfg(test)]
    pub fn test_push_async_flush_buffer(&mut self) {
        self.async_buffers
            .push_back(Some(P::AsyncBuffer::empty_for_test()));
        self.pending_downloads.push_back(SmallVec::new());
    }

    /// Pop completed asynchronous buffers.
    ///
    /// Upstream: `BufferCache<P>::PopAsyncBuffers`
    ///
    pub fn pop_async_buffers(&mut self) {
        let Some(async_buffer) = self.async_buffers.pop_front() else {
            return;
        };
        let Some(async_buffer) = async_buffer else {
            return;
        };
        let downloads = self
            .pending_downloads
            .pop_front()
            .expect("async buffer and download queues must remain synchronized");
        let base_offset = async_buffer.offset();
        let mapped_memory = async_buffer.mapped_span();
        if let Some(ref dm) = self.device_memory {
            for copy in &downloads {
                let device_addr = copy.src_offset;
                let dst_offset = copy.dst_offset.wrapping_sub(base_offset) as usize;
                let copy_size = copy.size as usize;
                let read_mapped_memory = &mapped_memory[dst_offset..dst_offset + copy_size];
                let mut write_ranges = Vec::new();
                self.async_downloads.for_each_in_range(
                    device_addr,
                    copy_size,
                    |start, end, _count| {
                        write_ranges.push((start, end));
                    },
                );
                for (start, end) in &write_ranges {
                    let src_start = (*start - device_addr) as usize;
                    let src_end = (*end - device_addr) as usize;
                    dm.write_block_unsafe(*start, &read_mapped_memory[src_start..src_end]);
                }
                self.async_downloads.subtract_with_on_delete(
                    device_addr,
                    copy_size,
                    |start, end| {
                        self.gpu_modified_ranges
                            .subtract(start, (end - start) as usize);
                    },
                );
            }
        }
        self.async_buffers_death_ring.push_back(async_buffer);
    }

    // -----------------------------------------------------------------------
    // Public API — DMA
    // -----------------------------------------------------------------------

    /// Perform a DMA copy between two GPU virtual addresses.
    ///
    /// Upstream: `BufferCache<P>::DMACopy`
    pub fn dma_copy(&mut self, src_address: u64, dest_address: u64, amount: u64) -> bool {
        let cpu_src_address = match self
            .gpu_memory
            .as_ref()
            .and_then(|gm| gm.gpu_to_cpu_address(src_address))
        {
            Some(a) => a,
            None => return false,
        };
        let cpu_dest_address = match self
            .gpu_memory
            .as_ref()
            .and_then(|gm| gm.gpu_to_cpu_address(dest_address))
        {
            Some(a) => a,
            None => return false,
        };

        let source_dirty = self.is_region_registered(cpu_src_address, amount as usize);
        let dest_dirty = self.is_region_registered(cpu_dest_address, amount as usize);
        if !source_dirty && !dest_dirty {
            return false;
        }

        self.clear_download(cpu_dest_address, amount);

        // Find (or create) buffers covering source and destination.
        let (buffer_a, buffer_b) = loop {
            if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                cs.has_deleted_buffers = false;
            }
            let buffer_a = self.find_buffer(cpu_src_address, amount as u32);
            let buffer_b = self.find_buffer(cpu_dest_address, amount as u32);
            if let Some(cs) = self.channel_caches.current_channel_state() {
                if !cs.has_deleted_buffers {
                    break (buffer_a, buffer_b);
                }
            } else {
                break (buffer_a, buffer_b);
            }
        };

        self.synchronize_buffer(buffer_a, cpu_src_address, amount as u32);
        self.synchronize_buffer(buffer_b, cpu_dest_address, amount as u32);

        let src_offset = self.slot_buffers[buffer_a].offset(cpu_src_address);
        let dst_offset = self.slot_buffers[buffer_b].offset(cpu_dest_address);
        let copies = [BufferCopy {
            src_offset: src_offset as u64,
            dst_offset: dst_offset as u64,
            size: amount,
        }];

        // Mirror GPU-modified ranges from source to destination.
        let mut tmp_intervals: Vec<(VAddr, u64)> = Vec::new();
        self.gpu_modified_ranges.for_each_in_range(
            cpu_src_address,
            amount as usize,
            |base_start, base_end| {
                let range_size = base_end - base_start;
                let diff = base_start - cpu_src_address;
                let new_base_address = cpu_dest_address + diff;
                tmp_intervals.push((new_base_address, range_size));
                // Also add to uncommitted.
            },
        );
        for &(addr, sz) in &tmp_intervals {
            self.uncommitted_gpu_modified_ranges.add(addr, sz as usize);
        }
        // Subtraction in this order is important for overlapping copies.
        self.gpu_modified_ranges
            .subtract(cpu_dest_address, amount as usize);
        let has_new_downloads = !tmp_intervals.is_empty();
        for &(addr, sz) in &tmp_intervals {
            self.gpu_modified_ranges.add(addr, sz as usize);
        }

        self.slot_buffers[buffer_a].mark_usage(src_offset as u64, amount);
        self.slot_buffers[buffer_b].mark_usage(dst_offset as u64, amount);

        self.runtime.copy_buffer(
            &self.slot_buffers[buffer_b],
            &self.slot_buffers[buffer_a],
            &copies,
            true,
            false,
        );

        if has_new_downloads {
            self.memory_tracker
                .mark_region_as_gpu_modified(cpu_dest_address, amount);
        }

        // Match DeviceGuestMemoryScoped<UnsafeReadWrite>: the host buffer copy
        // must also be reflected in device memory before returning.
        if let Some(ref device_memory) = self.device_memory {
            self.tmp_buffer.resize(amount as usize, 0);
            device_memory.read_block_unsafe(cpu_src_address, &mut self.tmp_buffer);
            device_memory.write_block_unsafe(cpu_dest_address, &self.tmp_buffer);
        }

        true
    }

    /// Perform a DMA clear.
    ///
    /// Upstream: `BufferCache<P>::DMAClear`
    pub fn dma_clear(&mut self, dst_address: u64, amount: u64, value: u32) -> bool {
        let cpu_dst_address = match self
            .gpu_memory
            .as_ref()
            .and_then(|gm| gm.gpu_to_cpu_address(dst_address))
        {
            Some(a) => a,
            None => return false,
        };
        let dest_dirty = self.is_region_registered(cpu_dst_address, amount as usize);
        if !dest_dirty {
            return false;
        }

        // Upstream: const size_t size = amount * sizeof(u32);
        let size = amount * 4;
        self.clear_download(cpu_dst_address, size);
        self.gpu_modified_ranges
            .subtract(cpu_dst_address, size as usize);

        let buffer_id = self.find_buffer(cpu_dst_address, size as u32);
        let offset = self.slot_buffers[buffer_id].offset(cpu_dst_address);
        self.runtime
            .clear_buffer(&self.slot_buffers[buffer_id], offset, size, value);
        self.slot_buffers[buffer_id].mark_usage(offset as u64, size);
        true
    }

    // -----------------------------------------------------------------------
    // Public API — region queries
    // -----------------------------------------------------------------------

    /// Return true when a device region is GPU-modified.
    ///
    /// Upstream: `BufferCache<P>::IsRegionGpuModified`
    pub fn is_region_gpu_modified(&self, addr: VAddr, size: usize) -> bool {
        let mut found = false;
        self.gpu_modified_ranges
            .for_each_in_range(addr, size, |_start, _end| {
                found = true;
            });
        found
    }

    /// Return true when a region is registered in the cache.
    ///
    /// Upstream: `BufferCache<P>::IsRegionRegistered`
    pub fn is_region_registered(&self, addr: VAddr, size: usize) -> bool {
        let end_addr = addr + size as u64;
        let page_end = div_ceil(end_addr, CACHING_PAGESIZE);
        let mut page = addr >> CACHING_PAGEBITS;
        while page < page_end {
            let buffer_id = self.page_table[page as usize];
            if !buffer_id.is_valid() {
                page += 1;
                continue;
            }
            let buffer = &self.slot_buffers[buffer_id];
            let buf_start = buffer.cpu_addr();
            let buf_end = buf_start + buffer.size_bytes() as u64;
            if buf_start < end_addr && addr < buf_end {
                return true;
            }
            page = div_ceil(end_addr, CACHING_PAGESIZE);
        }
        false
    }

    /// Return true when a device region is CPU-modified.
    ///
    /// Upstream: `BufferCache<P>::IsRegionCpuModified`
    pub fn is_region_cpu_modified(&mut self, addr: VAddr, size: usize) -> bool {
        self.memory_tracker
            .is_region_cpu_modified(addr, size as u64)
    }

    // -----------------------------------------------------------------------
    // Public API — draw indirect
    // -----------------------------------------------------------------------

    /// Get the draw indirect count buffer.
    ///
    /// Upstream: `BufferCache<P>::GetDrawIndirectCount`
    pub fn get_draw_indirect_count(&mut self) -> (BufferId, u32) {
        let Some(cs) = self.channel_caches.current_channel_state() else {
            return (NULL_BUFFER_ID, 0);
        };
        let binding = cs.count_buffer_binding;
        let offset = self.slot_buffers[binding.buffer_id].offset(binding.device_addr);
        (binding.buffer_id, offset)
    }

    /// Get the draw indirect buffer.
    ///
    /// Upstream: `BufferCache<P>::GetDrawIndirectBuffer`
    pub fn get_draw_indirect_buffer(&mut self) -> (BufferId, u32) {
        let Some(cs) = self.channel_caches.current_channel_state() else {
            return (NULL_BUFFER_ID, 0);
        };
        let binding = cs.indirect_buffer_binding;
        let offset = self.slot_buffers[binding.buffer_id].offset(binding.device_addr);
        (binding.buffer_id, offset)
    }

    /// Return the backend buffer handle for a cached buffer id.
    pub fn get_buffer_gpu_handle(&self, buffer_id: BufferId) -> u32 {
        if !buffer_id.is_valid() {
            return 0;
        }
        self.slot_buffers[buffer_id].raw_handle() as u32
    }

    /// Return the native backend buffer handle for same-backend consumers.
    pub fn resolve_backend_buffer_raw(&self, buffer_id: BufferId) -> u64 {
        if !buffer_id.is_valid() {
            return 0;
        }
        self.slot_buffers[buffer_id].raw_handle()
    }

    /// Borrow the backend-owned buffer selected by a cache id.
    ///
    /// This is the typed counterpart of `resolve_backend_buffer_raw` for
    /// backends such as Metal whose command encoder requires the native object
    /// rather than an integer handle.
    pub fn backend_buffer(&self, buffer_id: BufferId) -> Option<&P::Buffer> {
        self.slot_buffers
            .contains(buffer_id)
            .then(|| &self.slot_buffers[buffer_id])
    }

    // -----------------------------------------------------------------------
    // Public API — buffer operations retry loop
    // -----------------------------------------------------------------------

    /// Execute `func` in a retry loop: if any buffers are deleted during the
    /// operation, re-run it.
    pub fn buffer_operations<F>(&mut self, mut func: F)
    where
        F: FnMut(&mut Self),
    {
        loop {
            if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                cs.has_deleted_buffers = false;
            }
            func(self);
            if let Some(cs) = self.channel_caches.current_channel_state() {
                if !cs.has_deleted_buffers {
                    break;
                }
            } else {
                break;
            }
        }
    }

    // -----------------------------------------------------------------------
    // Private helpers — static
    // -----------------------------------------------------------------------

    /// Call `func` for each set bit in `enabled_mask`.
    fn for_each_enabled_bit<F>(mut enabled_mask: u32, mut func: F)
    where
        F: FnMut(u32),
    {
        let mut index: u32 = 0;
        while enabled_mask != 0 {
            let disabled_bits = enabled_mask.trailing_zeros();
            index += disabled_bits;
            enabled_mask >>= disabled_bits;
            func(index);
            index += 1;
            enabled_mask >>= 1;
        }
    }

    /// Iterate over all buffers overlapping `[device_addr, device_addr+size)`.
    fn for_each_buffer_in_range<F>(&mut self, device_addr: VAddr, size: u64, mut func: F)
    where
        F: FnMut(BufferId, &mut P::Buffer),
    {
        Self::for_each_buffer_in_range_impl(
            &self.page_table,
            &mut self.slot_buffers,
            device_addr,
            size,
            &mut func,
        );
    }

    /// Borrow-split implementation of upstream `ForEachBufferInRange`.
    ///
    /// Passing the page table and slots separately lets callers traverse buffers while mutating
    /// another cache member from the callback, matching the C++ member helper without raw pointers.
    fn for_each_buffer_in_range_impl<F>(
        page_table: &[BufferId],
        slot_buffers: &mut SlotVector<P::Buffer>,
        device_addr: VAddr,
        size: u64,
        mut func: F,
    ) where
        F: FnMut(BufferId, &mut P::Buffer),
    {
        let page_end = div_ceil(device_addr + size, CACHING_PAGESIZE);
        let mut page = device_addr >> CACHING_PAGEBITS;
        while page < page_end {
            let buffer_id = page_table[page as usize];
            if !buffer_id.is_valid() {
                page += 1;
                continue;
            }
            let buffer = &mut slot_buffers[buffer_id];
            func(buffer_id, buffer);
            let end_addr = buffer.cpu_addr() + buffer.size_bytes() as u64;
            page = div_ceil(end_addr, CACHING_PAGESIZE);
        }
    }

    /// Check if a range fits within a single device page.
    fn is_range_granular(device_addr: VAddr, size: usize) -> bool {
        let device_pagemask = 4096u64 - 1; // Core::DEVICE_PAGEMASK
        (device_addr & !device_pagemask) == ((device_addr + size as u64) & !device_pagemask)
    }

    // -----------------------------------------------------------------------
    // Range-set helpers — no longer needed; using common::range_sets::RangeSet directly.
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Private helpers — operations
    // -----------------------------------------------------------------------

    /// Run the garbage collector: destroy LRU buffers until memory pressure is reduced.
    ///
    /// Upstream: `BufferCache<P>::RunGarbageCollector`
    fn run_garbage_collector(&mut self) {
        let aggressive_gc = self.total_used_memory >= self.critical_memory;
        let ticks_to_destroy: u64 = if aggressive_gc { 60 } else { 120 };
        let num_iterations: usize = if aggressive_gc { 64 } else { 32 };

        // Upstream: lru_cache.ForEachItemBelow(frame_tick - ticks_to_destroy, clean_up)
        // The callback downloads buffer memory and then deletes the buffer,
        // stopping after num_iterations buffers.
        let tick_threshold = self.frame_tick.wrapping_sub(ticks_to_destroy);
        let mut remaining = num_iterations;
        let mut to_delete: Vec<BufferId> = Vec::new();
        self.lru_cache
            .for_each_item_below(tick_threshold, |buffer_id| {
                if remaining == 0 {
                    return true; // stop
                }
                remaining -= 1;
                to_delete.push(buffer_id);
                false // continue
            });
        for buffer_id in to_delete {
            self.download_buffer_memory(buffer_id);
            self.delete_buffer(buffer_id, false);
        }
    }

    fn bind_host_index_buffer(&mut self) {
        let Some(cs) = self.channel_caches.current_channel_state() else {
            return;
        };
        let binding = cs.index_buffer;
        let buffer_id = binding.buffer_id;
        let device_addr = binding.device_addr;
        let size = binding.size;

        let inline_indexes = self
            .maxwell3d()
            .map(|maxwell| {
                maxwell
                    .draw_manager_state()
                    .inline_index_draw_indexes
                    .clone()
            })
            .unwrap_or_default();

        self.touch_buffer(buffer_id);
        if inline_indexes.is_empty() {
            self.synchronize_buffer(buffer_id, device_addr, size);
        } else if P::USE_MEMORY_MAPS_FOR_UPLOADS {
            let mut upload_staging = self.runtime.upload_staging_buffer(size as u64);
            upload_staging.mapped_span_mut()[..size as usize]
                .copy_from_slice(&inline_indexes[..size as usize]);
            let copies = [BufferCopy {
                src_offset: upload_staging.offset(),
                dst_offset: 0,
                size: size as u64,
            }];
            self.runtime.copy_buffer_from_staging(
                &self.slot_buffers[buffer_id],
                &upload_staging,
                &copies,
                true,
                false,
            );
        } else {
            self.slot_buffers[buffer_id].immediate_upload(0, &inline_indexes);
        }

        let offset = self.slot_buffers[buffer_id].offset(device_addr);
        let (topology, index_format, first, count, format_size) = self
            .maxwell3d()
            .map(|maxwell| {
                let draw_state = maxwell.draw_manager_state();
                let index = draw_state.index_buffer;
                (
                    draw_state.topology,
                    index.format,
                    index.first,
                    index.count,
                    index.format.size_bytes() as u32,
                )
            })
            .unwrap_or((
                crate::engines::maxwell_3d::PrimitiveTopology::Triangles,
                crate::engines::maxwell_3d::IndexFormat::UnsignedInt,
                0,
                0,
                4,
            ));
        let offset = if P::HAS_FULL_INDEX_AND_PRIMITIVE_SUPPORT {
            offset + first * format_size
        } else {
            offset
        };
        let buffer = &mut self.slot_buffers[buffer_id];
        self.runtime
            .bind_index_buffer(topology, index_format, first, count, buffer, offset, size);
    }

    /// Upstream: `BufferCache<P>::VertexBufferSlot`.
    fn vertex_buffer_slot(&self, index: u32) -> Binding {
        assert!(index < NUM_VERTEX_BUFFERS);
        self.v_buffer[index as usize]
    }

    /// Upstream: `BufferCache<P>::UpdateVertexBufferSlot`.
    fn update_vertex_buffer_slot(&mut self, index: u32, binding: Binding) {
        let slot = &mut self.v_buffer[index as usize];
        if slot.device_addr != binding.device_addr || slot.size != binding.size {
            self.vertex_buffers_serial = self.vertex_buffers_serial.wrapping_add(1);
        }
        *slot = binding;
        if binding.buffer_id != NULL_BUFFER_ID && binding.size != 0 {
            self.enabled_vertex_buffers_mask |= 1u32 << index;
        } else {
            self.enabled_vertex_buffers_mask &= !(1u32 << index);
        }
    }

    fn bind_host_vertex_buffers(&mut self) {
        let mut enabled_mask = self.enabled_vertex_buffers_mask;
        let mut bindings = HostBindings::default();
        let mut last_index = u32::MAX;
        let flush_bindings =
            |cache: &mut Self, bindings: &mut HostBindings, last_index: &mut u32| {
                if bindings.buffer_ids.is_empty() {
                    return;
                }
                bindings.max_index = bindings.min_index + bindings.buffer_ids.len() as u32;
                cache
                    .runtime
                    .bind_vertex_buffers(bindings, &mut cache.slot_buffers);
                *bindings = HostBindings::default();
                *last_index = u32::MAX;
            };

        while enabled_mask != 0 {
            let index = enabled_mask.trailing_zeros();
            enabled_mask &= enabled_mask - 1;
            let binding = self.vertex_buffer_slot(index);
            self.touch_buffer(binding.buffer_id);
            self.synchronize_buffer(binding.buffer_id, binding.device_addr, binding.size);
            if !self.is_geometry_dirty(DirtyFlag::VertexBuffer(index)) {
                flush_bindings(self, &mut bindings, &mut last_index);
                continue;
            }
            self.clear_geometry_dirty(DirtyFlag::VertexBuffer(index));
            let stride = self
                .maxwell3d()
                .map(|maxwell| maxwell.vertex_stream_info(index).stride as u64)
                .unwrap_or_default();
            let offset = self.slot_buffers[binding.buffer_id].offset(binding.device_addr);
            if !P::IS_OPENGL {
                self.slot_buffers[binding.buffer_id].mark_usage(offset as u64, binding.size as u64);
            }
            if !bindings.buffer_ids.is_empty() && index != last_index.wrapping_add(1) {
                flush_bindings(self, &mut bindings, &mut last_index);
            }
            if bindings.buffer_ids.is_empty() {
                bindings.min_index = index;
            }
            bindings.buffer_ids.push(binding.buffer_id);
            bindings.offsets.push(offset as u64);
            bindings.sizes.push(binding.size as u64);
            bindings.strides.push(stride);
            last_index = index;
        }
        flush_bindings(self, &mut bindings, &mut last_index);
    }

    fn bind_host_draw_indirect_buffers(&mut self) {
        let include_count = self
            .current_draw_indirect
            .is_some_and(|params| params.include_count);
        let Some(cs) = self.channel_caches.current_channel_state() else {
            return;
        };
        let count_binding = cs.count_buffer_binding;
        let indirect_binding = cs.indirect_buffer_binding;

        if include_count {
            self.touch_buffer(count_binding.buffer_id);
            self.synchronize_buffer(
                count_binding.buffer_id,
                count_binding.device_addr,
                count_binding.size,
            );
        }
        self.touch_buffer(indirect_binding.buffer_id);
        self.synchronize_buffer(
            indirect_binding.buffer_id,
            indirect_binding.device_addr,
            indirect_binding.size,
        );
    }

    /// Upstream: `BufferCache<P>::BindHostGraphicsUniformBuffers`
    fn bind_host_graphics_uniform_buffers(&mut self, stage: usize) {
        let Some(cs) = self.channel_caches.current_channel_state() else {
            return;
        };
        let dirty = if P::HAS_PERSISTENT_UNIFORM_BUFFER_BINDINGS {
            cs.dirty_uniform_buffers[stage]
        } else {
            !0u32
        };
        let mask = cs.enabled_uniform_buffer_masks[stage];

        if P::HAS_PERSISTENT_UNIFORM_BUFFER_BINDINGS {
            if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                cs.dirty_uniform_buffers[stage] = 0;
            }
        }

        let mut binding_index = 0u32;
        Self::for_each_enabled_bit(mask, |idx| {
            let needs_bind = ((dirty >> idx) & 1) != 0;
            self.bind_host_graphics_uniform_buffer(stage, idx, binding_index, needs_bind);
            if P::NEEDS_BIND_UNIFORM_INDEX {
                binding_index += 1;
            }
        });
    }

    /// Upstream: `BufferCache<P>::BindHostGraphicsUniformBuffer`
    fn bind_host_graphics_uniform_buffer(
        &mut self,
        stage: usize,
        index: u32,
        binding_index: u32,
        needs_bind: bool,
    ) {
        let Some(cs) = self.channel_caches.current_channel_state_mut() else {
            return;
        };
        cs.uniform_cache_shots[0] = cs.uniform_cache_shots[0].wrapping_add(1);
        let binding = cs.uniform_buffers[stage][index as usize];
        let skip_cache_size = cs.uniform_buffer_skip_cache_size;
        let ub_sizes = cs
            .uniform_buffer_sizes
            .expect("graphics pipeline must set uniform-buffer sizes before binding");

        let device_addr = binding.device_addr;
        // SAFETY: SetUniformBuffersState receives the address-stable
        // per-pipeline array. Pipeline caches own every configured pipeline
        // until after the buffer cache is no longer used.
        let size = binding
            .size
            .min(unsafe { ub_sizes.as_ref() }[stage][index as usize]);
        // Touch the buffer.
        self.touch_buffer(binding.buffer_id);

        let has_host_buffer = binding.buffer_id != NULL_BUFFER_ID;
        let offset = if has_host_buffer {
            self.slot_buffers[binding.buffer_id].offset(device_addr)
        } else {
            0
        };
        let needs_alignment_stream = if P::IS_OPENGL || !has_host_buffer {
            false
        } else {
            let alignment = self.runtime.uniform_buffer_alignment();
            alignment > 1 && offset % alignment != 0
        };
        let use_fast_buffer = needs_alignment_stream
            || (has_host_buffer
                && size <= skip_cache_size
                && !self
                    .memory_tracker
                    .is_region_gpu_modified(device_addr, size as u64));

        if use_fast_buffer {
            // Upstream fast path: either BindMappedUniformBuffer or PushFastUniformBuffer.
            let mut fast_buffer_bound = false;
            if P::IS_OPENGL {
                if self.runtime.has_fast_buffer_sub_data() {
                    // Upstream: runtime.PushFastUniformBuffer(stage, binding_index, span)
                    // Read device memory and push it.
                    if let Some(ref dm) = self.device_memory {
                        let should_fast_bind = self
                            .channel_caches
                            .current_channel_state()
                            .is_none_or(|cs| {
                                ((cs.fast_bound_uniform_buffers[stage] >> binding_index) & 1) == 0
                                    || cs.uniform_buffer_binding_sizes[stage]
                                        [binding_index as usize]
                                        != size
                            });
                        if should_fast_bind {
                            self.runtime
                                .bind_fast_uniform_buffer(stage, binding_index, size);
                        }
                        let span = Self::immediate_buffer_with_data(
                            dm.as_ref(),
                            &mut self.immediate_buffer_alloc,
                            device_addr,
                            size as usize,
                        );
                        self.runtime
                            .push_fast_uniform_buffer(stage, binding_index, span);
                        fast_buffer_bound = true;
                    }
                } else {
                    // Upstream: runtime.BindMappedUniformBuffer(stage, binding_index, size)
                    // then copies device memory into the mapped span.
                    if let Some(ref dm) = self.device_memory {
                        let mut write = |span: &mut [u8]| {
                            dm.read_block_unsafe(device_addr, span);
                        };
                        fast_buffer_bound = self.runtime.with_mapped_uniform_buffer(
                            stage,
                            binding_index,
                            size,
                            &mut write,
                        );
                    }
                }
            }
            if fast_buffer_bound {
                if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                    cs.fast_bound_uniform_buffers[stage] |= 1u32 << binding_index;
                    cs.uniform_buffer_binding_sizes[stage][binding_index as usize] = size;
                }
                return;
            }
            if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                cs.fast_bound_uniform_buffers[stage] |= 1u32 << binding_index;
                cs.uniform_buffer_binding_sizes[stage][binding_index as usize] = size;
            }
            // Upstream stream-buffer path is shared by non-Nvidia OpenGL and Vulkan.
            if let Some(ref dm) = self.device_memory {
                let mut write = |span: &mut [u8]| {
                    dm.read_block_unsafe(device_addr, span);
                };
                if self
                    .runtime
                    .with_mapped_uniform_buffer(stage, binding_index, size, &mut write)
                {
                    return;
                }
            }
        }

        // Classic cached path.
        let sync_cached = self.synchronize_buffer(binding.buffer_id, device_addr, size);
        if let Some(cs) = self.channel_caches.current_channel_state_mut() {
            if sync_cached {
                cs.uniform_cache_hits[0] = cs.uniform_cache_hits[0].wrapping_add(1);
            }
        }

        let has_fast_bound = self.has_fast_uniform_buffer_bound(stage, binding_index);
        let binding_size_differs = if P::HAS_PERSISTENT_UNIFORM_BUFFER_BINDINGS {
            self.channel_caches
                .current_channel_state()
                .map_or(false, |cs| {
                    cs.uniform_buffer_binding_sizes[stage][binding_index as usize] != size
                })
        } else {
            false
        };
        let needs_bind = needs_bind | has_fast_bound | binding_size_differs;
        if !needs_bind {
            return;
        }

        if P::IS_OPENGL {
            let is_copy_bind = if offset != 0 {
                !self.runtime.supports_non_zero_uniform_offset()
            } else {
                false
            };
            if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                if is_copy_bind {
                    cs.dirty_uniform_buffers[stage] |= 1u32 << index;
                }
            }
        }
        if P::HAS_PERSISTENT_UNIFORM_BUFFER_BINDINGS {
            if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                cs.uniform_buffer_binding_sizes[stage][binding_index as usize] = size;
            }
        }
        self.slot_buffers[binding.buffer_id].mark_usage(offset as u64, size as u64);
        self.runtime.bind_uniform_buffer(
            stage,
            binding_index,
            &mut self.slot_buffers[binding.buffer_id],
            offset,
            size,
        );
        if let Some(cs) = self.channel_caches.current_channel_state_mut() {
            cs.fast_bound_uniform_buffers[stage] &= !(1u32 << binding_index);
        }
    }

    pub fn set_graphics_base_uniform_bindings(
        &mut self,
        bindings: &[u32; super::buffer_cache_base::NUM_STAGES as usize],
    ) {
        self.runtime.set_base_uniform_bindings(bindings);
    }

    pub fn set_graphics_base_storage_bindings(
        &mut self,
        bindings: &[u32; super::buffer_cache_base::NUM_STAGES as usize],
    ) {
        self.runtime.set_base_storage_bindings(bindings);
    }

    pub fn set_enable_storage_buffers(&mut self, enable: bool) {
        self.runtime.set_enable_storage_buffers(enable);
    }

    fn bind_host_graphics_storage_buffers(&mut self, stage: usize) {
        // Upstream: iterates enabled storage buffers, synchronizes, then calls
        // runtime.BindStorageBuffer.
        let Some(cs) = self.channel_caches.current_channel_state() else {
            return;
        };
        let mask = cs.enabled_storage_buffers[stage];
        let written_mask = cs.written_storage_buffers[stage];
        let bindings: Vec<Binding> = cs.storage_buffers[stage].to_vec();

        let mut binding_index = 0u32;
        Self::for_each_enabled_bit(mask, |idx| {
            let binding = bindings[idx as usize];
            self.touch_buffer(binding.buffer_id);
            self.synchronize_buffer(binding.buffer_id, binding.device_addr, binding.size);

            let offset = self.slot_buffers[binding.buffer_id].offset(binding.device_addr);
            self.slot_buffers[binding.buffer_id].mark_usage(offset as u64, binding.size as u64);

            let is_written = ((written_mask >> idx) & 1) != 0;
            if is_written {
                self.mark_written_buffer(binding.buffer_id, binding.device_addr, binding.size);
            }

            let buffer = &mut self.slot_buffers[binding.buffer_id];
            self.runtime.bind_storage_buffer(
                stage,
                binding_index,
                buffer,
                offset,
                binding.size,
                is_written,
            );
            if P::NEEDS_BIND_STORAGE_INDEX {
                binding_index += 1;
            }
        });
    }

    fn bind_host_graphics_texture_buffers(&mut self, stage: usize) {
        let Some(cs) = self.channel_caches.current_channel_state() else {
            return;
        };
        let mask = cs.enabled_texture_buffers[stage];
        let written_mask = cs.written_texture_buffers[stage];
        let image_mask = cs.image_texture_buffers[stage];
        let bindings: Vec<TextureBufferBinding> = cs.texture_buffers[stage].to_vec();

        Self::for_each_enabled_bit(mask, |idx| {
            let binding = bindings[idx as usize];
            self.synchronize_buffer(binding.buffer_id, binding.device_addr, binding.size);

            let is_written = ((written_mask >> idx) & 1) != 0;
            if is_written {
                self.mark_written_buffer(binding.buffer_id, binding.device_addr, binding.size);
            }
            let is_image = P::SEPARATE_IMAGE_BUFFER_BINDINGS && ((image_mask >> idx) & 1) != 0;

            let offset = self.slot_buffers[binding.buffer_id].offset(binding.device_addr);
            self.slot_buffers[binding.buffer_id].mark_usage(offset as u64, binding.size as u64);
            let buffer = &mut self.slot_buffers[binding.buffer_id];
            if is_image {
                self.runtime
                    .bind_image_buffer(buffer, offset, binding.size, binding.format);
            } else {
                self.runtime
                    .bind_texture_buffer(buffer, offset, binding.size, binding.format);
            }
        });
    }

    fn bind_host_transform_feedback_buffers(&mut self) {
        let Some((enabled, layouts)) = self.maxwell3d().map(|maxwell| {
            (
                maxwell.transform_feedback_enabled(),
                maxwell.transform_feedback_state().layouts,
            )
        }) else {
            return;
        };
        if !enabled {
            return;
        }
        let Some(cs) = self.channel_caches.current_channel_state() else {
            return;
        };
        let bindings: Vec<Binding> = cs.transform_feedback_buffers.to_vec();
        let mut host_bindings = HostBindings::default();
        for (index, binding) in bindings.into_iter().enumerate() {
            let layout = layouts[index];
            let has_layout = layout.varying_count != 0 || layout.stride != 0;
            let mut buffer_id = NULL_BUFFER_ID;
            let mut offset = 0;
            let mut size = 0;
            if has_layout
                && binding.buffer_id.is_valid()
                && binding.buffer_id != NULL_BUFFER_ID
                && binding.size != 0
            {
                self.touch_buffer(binding.buffer_id);
                self.synchronize_buffer(binding.buffer_id, binding.device_addr, binding.size);
                self.mark_written_buffer(binding.buffer_id, binding.device_addr, binding.size);
                buffer_id = binding.buffer_id;
                offset = self.slot_buffers[buffer_id].offset(binding.device_addr);
                size = binding.size;
                self.slot_buffers[buffer_id].mark_usage(offset as u64, size as u64);
            }
            host_bindings.buffer_ids.push(buffer_id);
            host_bindings.offsets.push(offset as u64);
            host_bindings.sizes.push(size as u64);
            host_bindings.strides.push(0);
        }
        self.runtime
            .bind_transform_feedback_buffers(&host_bindings, &mut self.slot_buffers);
    }

    fn bind_host_compute_uniform_buffers(&mut self) {
        // Upstream: marks all uniform buffers dirty (persistent bindings), then
        // iterates and calls runtime.BindComputeUniformBuffer.
        if P::HAS_PERSISTENT_UNIFORM_BUFFER_BINDINGS {
            if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                cs.dirty_uniform_buffers.fill(!0u32);
                cs.fast_bound_uniform_buffers.fill(0);
            }
        }

        let Some(cs) = self.channel_caches.current_channel_state() else {
            return;
        };
        let mask = cs.enabled_compute_uniform_buffer_mask;
        let ub_sizes = cs
            .compute_uniform_buffer_sizes
            .expect("compute pipeline must set uniform-buffer sizes before binding");
        let bindings = cs.compute_uniform_buffers;

        let mut binding_index = 0u32;
        Self::for_each_enabled_bit(mask, |idx| {
            let binding = bindings[idx as usize];
            self.touch_buffer(binding.buffer_id);
            // SAFETY: SetComputeUniformBufferState receives the address-stable
            // array owned by the cached compute pipeline.
            let size = binding.size.min(unsafe { ub_sizes.as_ref() }[idx as usize]);
            let has_host_buffer = binding.buffer_id != NULL_BUFFER_ID;
            let offset = if has_host_buffer {
                self.slot_buffers[binding.buffer_id].offset(binding.device_addr)
            } else {
                0
            };
            let needs_alignment_stream = if P::IS_OPENGL || !has_host_buffer {
                false
            } else {
                let alignment = self.runtime.uniform_buffer_alignment();
                alignment > 1 && offset % alignment != 0
            };
            if needs_alignment_stream {
                let streamed = if let Some(ref dm) = self.device_memory {
                    let mut write = |span: &mut [u8]| {
                        dm.read_block_unsafe(binding.device_addr, span);
                    };
                    self.runtime
                        .with_mapped_uniform_buffer(0, binding_index, size, &mut write)
                } else {
                    false
                };
                if streamed {
                    return;
                }
            }
            self.synchronize_buffer(binding.buffer_id, binding.device_addr, size);

            self.slot_buffers[binding.buffer_id].mark_usage(offset as u64, size as u64);

            let buffer = &mut self.slot_buffers[binding.buffer_id];
            if P::NEEDS_BIND_UNIFORM_INDEX {
                self.runtime
                    .bind_compute_uniform_buffer(binding_index, buffer, offset, size);
            } else {
                self.runtime
                    .bind_uniform_buffer(0, binding_index, buffer, offset, size);
            }
            if P::NEEDS_BIND_UNIFORM_INDEX {
                binding_index += 1;
            }
        });
    }

    fn bind_host_compute_storage_buffers(&mut self) {
        let Some(cs) = self.channel_caches.current_channel_state() else {
            return;
        };
        let mask = cs.enabled_compute_storage_buffers;
        let written_mask = cs.written_compute_storage_buffers;
        let bindings: Vec<Binding> = cs.compute_storage_buffers.to_vec();

        let mut binding_index = 0u32;
        Self::for_each_enabled_bit(mask, |idx| {
            let binding = bindings[idx as usize];
            self.touch_buffer(binding.buffer_id);
            self.synchronize_buffer(binding.buffer_id, binding.device_addr, binding.size);

            let offset = self.slot_buffers[binding.buffer_id].offset(binding.device_addr);
            self.slot_buffers[binding.buffer_id].mark_usage(offset as u64, binding.size as u64);

            let is_written = ((written_mask >> idx) & 1) != 0;
            if is_written {
                self.mark_written_buffer(binding.buffer_id, binding.device_addr, binding.size);
            }

            let buffer = &mut self.slot_buffers[binding.buffer_id];
            // Upstream: NEEDS_BIND_STORAGE_INDEX (OpenGL) uses the indexed
            // compute path; everything else (Vulkan) shares BindStorageBuffer.
            if P::NEEDS_BIND_STORAGE_INDEX {
                self.runtime.bind_compute_storage_buffer(
                    binding_index,
                    buffer,
                    offset,
                    binding.size,
                    is_written,
                );
            } else {
                self.runtime.bind_storage_buffer(
                    0,
                    binding_index,
                    buffer,
                    offset,
                    binding.size,
                    is_written,
                );
            }
            if P::NEEDS_BIND_STORAGE_INDEX {
                binding_index += 1;
            }
        });
    }

    fn bind_host_compute_texture_buffers(&mut self) {
        let Some(cs) = self.channel_caches.current_channel_state() else {
            return;
        };
        let mask = cs.enabled_compute_texture_buffers;
        let written_mask = cs.written_compute_texture_buffers;
        let image_mask = cs.image_compute_texture_buffers;
        let bindings: Vec<TextureBufferBinding> = cs.compute_texture_buffers.to_vec();

        Self::for_each_enabled_bit(mask, |idx| {
            let binding = bindings[idx as usize];
            self.synchronize_buffer(binding.buffer_id, binding.device_addr, binding.size);

            let is_written = ((written_mask >> idx) & 1) != 0;
            if is_written {
                self.mark_written_buffer(binding.buffer_id, binding.device_addr, binding.size);
            }
            let is_image = P::SEPARATE_IMAGE_BUFFER_BINDINGS && ((image_mask >> idx) & 1) != 0;

            let offset = self.slot_buffers[binding.buffer_id].offset(binding.device_addr);
            self.slot_buffers[binding.buffer_id].mark_usage(offset as u64, binding.size as u64);
            let buffer = &mut self.slot_buffers[binding.buffer_id];
            if is_image {
                self.runtime
                    .bind_image_buffer(buffer, offset, binding.size, binding.format);
            } else {
                self.runtime
                    .bind_texture_buffer(buffer, offset, binding.size, binding.format);
            }
        });
    }

    /// Upstream: `BufferCache<P>::DoUpdateGraphicsBuffers`
    fn do_update_graphics_buffers(&mut self, is_indexed: bool) {
        self.buffer_operations(|cache| {
            if is_indexed {
                cache.update_index_buffer();
            }
            cache.update_vertex_buffers();
            cache.update_transform_feedback_buffers();
            for stage in 0..NUM_STAGES as usize {
                cache.update_uniform_buffers(stage);
                cache.update_storage_buffers(stage);
                cache.update_texture_buffers(stage);
            }
            // Upstream: if (current_draw_indirect) { UpdateDrawIndirect(); }
            if cache.current_draw_indirect.is_some() {
                cache.update_draw_indirect();
            }
        });
    }

    fn do_update_graphics_buffers_with_gpu_resolver<G, I, M>(
        &mut self,
        is_indexed: bool,
        gpu_to_cpu_address: &mut G,
        is_within_gpu_address_range: &mut I,
        max_continuous_range: &mut M,
    ) where
        G: FnMut(u64) -> Option<u64>,
        I: FnMut(u64) -> bool,
        M: FnMut(u64, u64) -> u64,
    {
        self.buffer_operations(|cache| {
            if is_indexed {
                cache.update_index_buffer_with_gpu_resolver(gpu_to_cpu_address);
            }
            cache.update_vertex_buffers_with_gpu_resolver(
                gpu_to_cpu_address,
                is_within_gpu_address_range,
                max_continuous_range,
            );
            cache.update_transform_feedback_buffers();
            for stage in 0..NUM_STAGES as usize {
                cache.update_uniform_buffers(stage);
                cache.update_storage_buffers(stage);
                cache.update_texture_buffers(stage);
            }
            if cache.current_draw_indirect.is_some() {
                cache.update_draw_indirect_with_gpu_resolver(gpu_to_cpu_address);
            }
        });
    }

    /// Upstream: `BufferCache<P>::DoUpdateComputeBuffers`
    fn do_update_compute_buffers(&mut self) {
        self.buffer_operations(|cache| {
            cache.update_compute_uniform_buffers();
            cache.update_compute_storage_buffers();
            cache.update_compute_texture_buffers();
        });
    }

    /// Upstream: `BufferCache<P>::UpdateIndexBuffer`
    fn update_index_buffer(&mut self) {
        if self.maxwell3d().is_none() {
            return;
        }
        if !self.is_geometry_dirty(DirtyFlag::IndexBuffer) {
            return;
        }
        self.clear_geometry_dirty(DirtyFlag::IndexBuffer);

        let inline_index_size = self
            .maxwell3d()
            .expect("checked above")
            .draw_manager_state()
            .inline_index_draw_indexes
            .len() as u32;
        if inline_index_size != 0 {
            let buffer_size =
                (inline_index_size + CACHING_PAGESIZE as u32 - 1) & !(CACHING_PAGESIZE as u32 - 1);
            if self.inline_buffer_id == NULL_BUFFER_ID {
                self.inline_buffer_id = self.create_buffer(0, buffer_size);
            }
            if (self.slot_buffers[self.inline_buffer_id].size_bytes() as u32) < buffer_size {
                let old_id = self.inline_buffer_id;
                self.slot_buffers.erase(old_id);
                self.inline_buffer_id = self.create_buffer(0, buffer_size);
            }
            if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                cs.index_buffer = Binding {
                    device_addr: 0,
                    size: inline_index_size,
                    buffer_id: self.inline_buffer_id,
                };
            }
            return;
        }

        let index_buffer_ref = {
            let maxwell = self.maxwell3d().expect("checked above");
            let index = maxwell.draw_manager_state().index_buffer;
            let format_size_in_bytes = index.format.size_bytes() as u32;
            IndexBufferRef {
                start_address: maxwell.index_buffer_addr(),
                end_address: Maxwell3DAccess::index_buffer_addr_end(maxwell),
                count: index.count,
                first: index.first,
                format_size_in_bytes,
            }
        };

        let gpu_addr_begin = index_buffer_ref.start_address;
        let gpu_addr_end = index_buffer_ref.end_address;
        let device_addr = self
            .gpu_memory
            .as_ref()
            .and_then(|gm| gm.gpu_to_cpu_address(gpu_addr_begin));
        let address_size = (gpu_addr_end - gpu_addr_begin) as u32;
        let draw_size = (index_buffer_ref.count + index_buffer_ref.first)
            * index_buffer_ref.format_size_in_bytes;
        let size = address_size.min(draw_size);
        if size == 0 || device_addr.is_none() {
            if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                cs.index_buffer = NULL_BINDING;
            }
            return;
        }
        let device_addr = device_addr.unwrap();
        let buffer_id = self.find_buffer(device_addr, size);
        if let Some(cs) = self.channel_caches.current_channel_state_mut() {
            cs.index_buffer = Binding {
                device_addr,
                size,
                buffer_id,
            };
        }
    }

    fn update_index_buffer_with_gpu_resolver(
        &mut self,
        gpu_to_cpu_address: &mut impl FnMut(u64) -> Option<u64>,
    ) {
        if self.maxwell3d().is_none() {
            return;
        }
        if !self.is_geometry_dirty(DirtyFlag::IndexBuffer) {
            return;
        }
        self.clear_geometry_dirty(DirtyFlag::IndexBuffer);

        let inline_index_size = self
            .maxwell3d()
            .expect("checked above")
            .draw_manager_state()
            .inline_index_draw_indexes
            .len() as u32;
        if inline_index_size != 0 {
            let buffer_size =
                (inline_index_size + CACHING_PAGESIZE as u32 - 1) & !(CACHING_PAGESIZE as u32 - 1);
            if self.inline_buffer_id == NULL_BUFFER_ID {
                self.inline_buffer_id = self.create_buffer(0, buffer_size);
            }
            if (self.slot_buffers[self.inline_buffer_id].size_bytes() as u32) < buffer_size {
                let old_id = self.inline_buffer_id;
                self.slot_buffers.erase(old_id);
                self.inline_buffer_id = self.create_buffer(0, buffer_size);
            }
            if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                cs.index_buffer = Binding {
                    device_addr: 0,
                    size: inline_index_size,
                    buffer_id: self.inline_buffer_id,
                };
            }
            return;
        }

        let index_buffer_ref = {
            let maxwell = self.maxwell3d().expect("checked above");
            let index = maxwell.draw_manager_state().index_buffer;
            let format_size_in_bytes = index.format.size_bytes() as u32;
            IndexBufferRef {
                start_address: maxwell.index_buffer_addr(),
                end_address: Maxwell3DAccess::index_buffer_addr_end(maxwell),
                count: index.count,
                first: index.first,
                format_size_in_bytes,
            }
        };

        let gpu_addr_begin = index_buffer_ref.start_address;
        let gpu_addr_end = index_buffer_ref.end_address;
        let device_addr = gpu_to_cpu_address(gpu_addr_begin);
        let address_size = (gpu_addr_end - gpu_addr_begin) as u32;
        let draw_size = (index_buffer_ref.count + index_buffer_ref.first)
            * index_buffer_ref.format_size_in_bytes;
        let size = address_size.min(draw_size);
        if size == 0 || device_addr.is_none() {
            if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                cs.index_buffer = NULL_BINDING;
            }
            return;
        }
        let device_addr = device_addr.unwrap();
        let buffer_id = self.find_buffer(device_addr, size);
        if let Some(cs) = self.channel_caches.current_channel_state_mut() {
            cs.index_buffer = Binding {
                device_addr,
                size,
                buffer_id,
            };
        }
    }

    /// Upstream: `BufferCache<P>::UpdateVertexBuffers`
    fn update_vertex_buffers(&mut self) {
        // Upstream: auto& flags = maxwell3d->dirty.flags;
        //           if (!flags[Dirty::VertexBuffers]) { return; }
        //           flags[Dirty::VertexBuffers] = false;
        if !self.is_geometry_dirty(DirtyFlag::VertexBuffers) {
            return;
        }
        self.clear_geometry_dirty(DirtyFlag::VertexBuffers);
        for index in 0..NUM_VERTEX_BUFFERS {
            self.update_vertex_buffer(index);
        }
    }

    fn update_vertex_buffers_with_gpu_resolver(
        &mut self,
        gpu_to_cpu_address: &mut impl FnMut(u64) -> Option<u64>,
        is_within_gpu_address_range: &mut impl FnMut(u64) -> bool,
        max_continuous_range: &mut impl FnMut(u64, u64) -> u64,
    ) {
        if !self.is_geometry_dirty(DirtyFlag::VertexBuffers) {
            return;
        }
        self.clear_geometry_dirty(DirtyFlag::VertexBuffers);
        for index in 0..NUM_VERTEX_BUFFERS {
            self.update_vertex_buffer_with_gpu_resolver(
                index,
                gpu_to_cpu_address,
                is_within_gpu_address_range,
                max_continuous_range,
            );
        }
    }

    fn update_vertex_buffer_with_gpu_resolver(
        &mut self,
        index: u32,
        gpu_to_cpu_address: &mut impl FnMut(u64) -> Option<u64>,
        is_within_gpu_address_range: &mut impl FnMut(u64) -> bool,
        max_continuous_range: &mut impl FnMut(u64, u64) -> u64,
    ) {
        if !self.is_geometry_dirty(DirtyFlag::VertexBuffer(index)) {
            return;
        }
        let Some((array, limit)) = self.maxwell3d().map(|maxwell| {
            (
                maxwell.vertex_stream_info(index),
                maxwell.vertex_stream_limit(index),
            )
        }) else {
            return;
        };

        let gpu_addr_begin = array.address;
        let gpu_addr_end = limit.address + 1;
        let device_addr = gpu_to_cpu_address(gpu_addr_begin);
        let address_size = (gpu_addr_end - gpu_addr_begin) as u32;
        let mut size = address_size;

        if !array.enabled || size == 0 || device_addr.is_none() {
            if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                cs.vertex_buffers[index as usize] = NULL_BINDING;
            }
            self.update_vertex_buffer_slot(index, NULL_BINDING);
            return;
        }

        let mib_64 = 64 * 1024 * 1024;
        if !is_within_gpu_address_range(gpu_addr_end) || size >= mib_64 {
            size = max_continuous_range(gpu_addr_begin, size as u64) as u32;
        }

        let device_addr = device_addr.unwrap();
        let buffer_id = self.find_buffer(device_addr, size);
        let binding = Binding {
            device_addr,
            size,
            buffer_id,
        };
        if let Some(cs) = self.channel_caches.current_channel_state_mut() {
            cs.vertex_buffers[index as usize] = binding;
        }
        self.update_vertex_buffer_slot(index, binding);
    }

    /// Upstream: `BufferCache<P>::UpdateVertexBuffer`
    fn update_vertex_buffer(&mut self, index: u32) {
        if !self.is_geometry_dirty(DirtyFlag::VertexBuffer(index)) {
            return;
        }
        let Some((array, limit)) = self.maxwell3d().map(|maxwell| {
            (
                maxwell.vertex_stream_info(index),
                maxwell.vertex_stream_limit(index),
            )
        }) else {
            return;
        };

        let gpu_addr_begin = array.address;
        let gpu_addr_end = limit.address + 1;
        let device_addr = self
            .gpu_memory
            .as_ref()
            .and_then(|gm| gm.gpu_to_cpu_address(gpu_addr_begin));
        let address_size = (gpu_addr_end - gpu_addr_begin) as u32;
        let mut size = address_size;

        if !array.enabled || size == 0 || device_addr.is_none() {
            if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                cs.vertex_buffers[index as usize] = NULL_BINDING;
            }
            self.update_vertex_buffer_slot(index, NULL_BINDING);
            return;
        }

        // Upstream: if (!gpu_memory->IsWithinGPUAddressRange(gpu_addr_end) || size >= 64_MiB)
        //     size = gpu_memory->MaxContinuousRange(gpu_addr_begin, size);
        let mib_64 = 64 * 1024 * 1024;
        if let Some(ref gm) = self.gpu_memory {
            if !gm.is_within_gpu_address_range(gpu_addr_end) || size >= mib_64 {
                size = gm.max_continuous_range(gpu_addr_begin, size as u64) as u32;
            }
        }

        let device_addr = device_addr.unwrap();
        let buffer_id = self.find_buffer(device_addr, size);
        let binding = Binding {
            device_addr,
            size,
            buffer_id,
        };
        if let Some(cs) = self.channel_caches.current_channel_state_mut() {
            cs.vertex_buffers[index as usize] = binding;
        }
        self.update_vertex_buffer_slot(index, binding);
    }

    /// Upstream: `BufferCache<P>::UpdateDrawIndirect`
    fn update_draw_indirect(&mut self) {
        let Some(params) = self.current_draw_indirect else {
            return;
        };

        // Helper closure: translate GPU address and create binding.
        let resolve_binding = |cache: &mut Self, gpu_addr: u64, size: u64| -> Binding {
            let device_addr = cache
                .gpu_memory
                .as_ref()
                .and_then(|gm| gm.gpu_to_cpu_address(gpu_addr));
            match device_addr {
                Some(addr) => {
                    let buffer_id = cache.find_buffer(addr, size as u32);
                    Binding {
                        device_addr: addr,
                        size: size as u32,
                        buffer_id,
                    }
                }
                None => NULL_BINDING,
            }
        };

        // Upstream: if (current_draw_indirect->include_count) { update count binding }
        if params.include_count {
            let binding = resolve_binding(self, params.count_start_address, 4); // sizeof(u32)
            if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                cs.count_buffer_binding = binding;
            }
        }

        let binding = resolve_binding(self, params.indirect_start_address, params.buffer_size);
        if let Some(cs) = self.channel_caches.current_channel_state_mut() {
            cs.indirect_buffer_binding = binding;
        }
    }

    fn update_draw_indirect_with_gpu_resolver(
        &mut self,
        gpu_to_cpu_address: &mut impl FnMut(u64) -> Option<u64>,
    ) {
        let Some(params) = self.current_draw_indirect else {
            return;
        };

        let mut resolve_binding = |cache: &mut Self, gpu_addr: u64, size: u64| -> Binding {
            match gpu_to_cpu_address(gpu_addr) {
                Some(addr) => {
                    let buffer_id = cache.find_buffer(addr, size as u32);
                    Binding {
                        device_addr: addr,
                        size: size as u32,
                        buffer_id,
                    }
                }
                None => NULL_BINDING,
            }
        };

        if params.include_count {
            let binding = resolve_binding(self, params.count_start_address, 4);
            if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                cs.count_buffer_binding = binding;
            }
        }

        let binding = resolve_binding(self, params.indirect_start_address, params.buffer_size);
        if let Some(cs) = self.channel_caches.current_channel_state_mut() {
            cs.indirect_buffer_binding = binding;
        }
    }

    /// Upstream: `BufferCache<P>::UpdateUniformBuffers`
    fn update_uniform_buffers(&mut self, stage: usize) {
        let Some(cs) = self.channel_caches.current_channel_state() else {
            return;
        };
        let mask = cs.enabled_uniform_buffer_masks[stage];

        Self::for_each_enabled_bit(mask, |idx| {
            if let Some(cs) = self.channel_caches.current_channel_state() {
                let binding = cs.uniform_buffers[stage][idx as usize];
                // If already resolved, skip.
                if binding.buffer_id.is_valid() && binding.buffer_id != NULL_BUFFER_ID {
                    return;
                }
            }

            // Mark as dirty and resolve buffer_id.
            if P::HAS_PERSISTENT_UNIFORM_BUFFER_BINDINGS {
                if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                    cs.dirty_uniform_buffers[stage] |= 1u32 << idx;
                }
            }

            let (device_addr, size) = if let Some(cs) = self.channel_caches.current_channel_state()
            {
                let b = cs.uniform_buffers[stage][idx as usize];
                (b.device_addr, b.size)
            } else {
                return;
            };
            let buffer_id = self.find_buffer(device_addr, size);
            if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                cs.uniform_buffers[stage][idx as usize].buffer_id = buffer_id;
            }
        });
    }

    /// Upstream: `BufferCache<P>::UpdateStorageBuffers`
    fn update_storage_buffers(&mut self, stage: usize) {
        let Some(cs) = self.channel_caches.current_channel_state() else {
            return;
        };
        let mask = cs.enabled_storage_buffers[stage];

        Self::for_each_enabled_bit(mask, |idx| {
            let (device_addr, size) = if let Some(cs) = self.channel_caches.current_channel_state()
            {
                let b = cs.storage_buffers[stage][idx as usize];
                (b.device_addr, b.size)
            } else {
                return;
            };
            let buffer_id = self.find_buffer(device_addr, size);
            if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                cs.storage_buffers[stage][idx as usize].buffer_id = buffer_id;
            }
        });
    }

    /// Upstream: `BufferCache<P>::UpdateTextureBuffers`
    fn update_texture_buffers(&mut self, stage: usize) {
        let Some(cs) = self.channel_caches.current_channel_state() else {
            return;
        };
        let mask = cs.enabled_texture_buffers[stage];

        Self::for_each_enabled_bit(mask, |idx| {
            let (device_addr, size) = if let Some(cs) = self.channel_caches.current_channel_state()
            {
                let b = cs.texture_buffers[stage][idx as usize];
                (b.device_addr, b.size)
            } else {
                return;
            };
            let buffer_id = self.find_buffer(device_addr, size);
            if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                cs.texture_buffers[stage][idx as usize].buffer_id = buffer_id;
            }
        });
    }

    /// Upstream: `BufferCache<P>::UpdateTransformFeedbackBuffers`
    fn update_transform_feedback_buffers(&mut self) {
        // Upstream: if (maxwell3d->regs.transform_feedback_enabled == 0) { return; }
        if !self
            .maxwell3d()
            .is_some_and(Maxwell3D::transform_feedback_enabled)
        {
            return;
        }
        for index in 0..NUM_TRANSFORM_FEEDBACK_BUFFERS {
            self.update_transform_feedback_buffer(index);
        }
    }

    /// Upstream: `BufferCache<P>::UpdateTransformFeedbackBuffer`
    fn update_transform_feedback_buffer(&mut self, index: u32) {
        let Some(tfb_info) = self
            .maxwell3d()
            .map(|maxwell| maxwell.transform_feedback_buffer_info(index))
        else {
            return;
        };

        let gpu_addr = tfb_info.address.wrapping_add(tfb_info.start_offset as u64);
        let size = tfb_info.size as u32;
        let device_addr = self
            .gpu_memory
            .as_ref()
            .and_then(|gm| gm.gpu_to_cpu_address(gpu_addr));

        if tfb_info.enable == 0 || size == 0 || device_addr.is_none() {
            if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                cs.transform_feedback_buffers[index as usize] = NULL_BINDING;
            }
            return;
        }
        let device_addr = device_addr.unwrap();
        let buffer_id = self.find_buffer(device_addr, size);
        if let Some(cs) = self.channel_caches.current_channel_state_mut() {
            cs.transform_feedback_buffers[index as usize] = Binding {
                device_addr,
                size,
                buffer_id,
            };
        }
    }

    /// Upstream: `BufferCache<P>::UpdateComputeUniformBuffers`
    fn update_compute_uniform_buffers(&mut self) {
        let Some(cs) = self.channel_caches.current_channel_state() else {
            return;
        };
        let mask = cs.enabled_compute_uniform_buffer_mask;

        Self::for_each_enabled_bit(mask, |idx| {
            // Upstream: binding = NULL_BINDING;
            //   if (((launch_desc.const_buffer_enable_mask >> index) & 1) != 0) {
            //       const auto& cbuf = launch_desc.const_buffer_config[index];
            //       const std::optional<DAddr> device_addr = gpu_memory->GpuToCpuAddress(cbuf.Address());
            //       if (device_addr) { binding.device_addr = *device_addr; binding.size = cbuf.size; }
            //   }
            //   binding.buffer_id = FindBuffer(binding.device_addr, binding.size);
            let cbuf = {
                let launch_desc = self
                    .kepler_compute()
                    .expect("bound BufferCache channel must own KeplerCompute")
                    .launch_description();
                if ((launch_desc.const_buffer_enable_mask >> idx) & 1) != 0 {
                    Some(launch_desc.const_buffers[idx as usize])
                } else {
                    None
                }
            };
            let mut binding = NULL_BINDING;
            if let Some(cbuf) = cbuf {
                if let Some(ref gm) = self.gpu_memory {
                    if let Some(device_addr) = gm.gpu_to_cpu_address(cbuf.address) {
                        binding.device_addr = device_addr;
                        binding.size = cbuf.size;
                    }
                }
            }
            binding.buffer_id = self.find_buffer(binding.device_addr, binding.size);
            if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                cs.compute_uniform_buffers[idx as usize] = binding;
            }
        });
    }

    /// Upstream: `BufferCache<P>::UpdateComputeStorageBuffers`
    fn update_compute_storage_buffers(&mut self) {
        let Some(cs) = self.channel_caches.current_channel_state() else {
            return;
        };
        let mask = cs.enabled_compute_storage_buffers;

        Self::for_each_enabled_bit(mask, |idx| {
            let (device_addr, size) = if let Some(cs) = self.channel_caches.current_channel_state()
            {
                let b = cs.compute_storage_buffers[idx as usize];
                (b.device_addr, b.size)
            } else {
                return;
            };
            let buffer_id = self.find_buffer(device_addr, size);
            if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                cs.compute_storage_buffers[idx as usize].buffer_id = buffer_id;
            }
        });
    }

    /// Upstream: `BufferCache<P>::UpdateComputeTextureBuffers`
    fn update_compute_texture_buffers(&mut self) {
        let Some(cs) = self.channel_caches.current_channel_state() else {
            return;
        };
        let mask = cs.enabled_compute_texture_buffers;

        Self::for_each_enabled_bit(mask, |idx| {
            let (device_addr, size) = if let Some(cs) = self.channel_caches.current_channel_state()
            {
                let b = cs.compute_texture_buffers[idx as usize];
                (b.device_addr, b.size)
            } else {
                return;
            };
            let buffer_id = self.find_buffer(device_addr, size);
            if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                cs.compute_texture_buffers[idx as usize].buffer_id = buffer_id;
            }
        });
    }

    /// Mark a buffer region as GPU-written.
    ///
    /// Upstream: `BufferCache<P>::MarkWrittenBuffer`
    fn mark_written_buffer(&mut self, buffer_id: BufferId, device_addr: VAddr, size: u32) {
        if !P::IS_OPENGL {
            self.slot_buffers[buffer_id].set_write_tick(self.runtime.current_tick());
        }
        self.memory_tracker
            .mark_region_as_gpu_modified(device_addr, size as u64);
        self.gpu_modified_ranges.add(device_addr, size as usize);
        self.uncommitted_gpu_modified_ranges
            .add(device_addr, size as usize);
    }

    /// Find or create a buffer covering `[device_addr, device_addr+size)`.
    ///
    /// Upstream: `BufferCache<P>::FindBuffer`
    fn find_buffer(&mut self, device_addr: VAddr, size: u32) -> BufferId {
        if device_addr == 0 {
            return NULL_BUFFER_ID;
        }
        let page = device_addr >> CACHING_PAGEBITS;
        let buffer_id = self.page_table[page as usize];
        if !buffer_id.is_valid() {
            return self.create_buffer(device_addr, size);
        }
        self.wait_for_gpu_fence_if_needed(buffer_id);
        if self.slot_buffers[buffer_id].is_in_bounds(device_addr, size as u64) {
            return buffer_id;
        }
        self.create_buffer(device_addr, size)
    }

    /// Port of `BufferCache<P>::WaitForGpuFenceIfNeeded`.
    fn wait_for_gpu_fence_if_needed(&mut self, buffer_id: BufferId) {
        if P::IS_OPENGL {
            return;
        }
        let (accurate, strict) = {
            let values = common::settings::values();
            (
                common::settings::is_gpu_fence_behavior_accurate(&values),
                common::settings::is_gpu_fence_behavior_strict(&values),
            )
        };
        if !accurate && !strict {
            return;
        }
        let gpu_tick_delay = if strict { 0 } else { 3 };
        let buffer_tick = self.slot_buffers[buffer_id].write_tick();
        let gpu_tick = self.runtime.known_gpu_tick();
        if buffer_tick > gpu_tick.wrapping_add(gpu_tick_delay) {
            self.runtime.wait(buffer_tick);
        }
    }

    /// Collect all buffers that overlap `[device_addr, device_addr+wanted_size)`.
    ///
    /// Upstream: `BufferCache<P>::ResolveOverlaps`
    fn resolve_overlaps(&mut self, device_addr: VAddr, wanted_size: u32) -> OverlapResult {
        let mut overlap_ids: Vec<BufferId> = Vec::new();
        let mut begin = device_addr;
        let mut end = device_addr + wanted_size as u64;

        let max_page: u64 = 1u64 << AS_BITS;

        let expand_begin = |begin: &mut u64, addr: &mut u64, add_value: u64| {
            let min_page = CACHING_PAGESIZE + DEVICE_PAGESIZE;
            if add_value > *begin - min_page {
                *begin = min_page;
                *addr = DEVICE_PAGESIZE;
            } else {
                *begin -= add_value;
                *addr = *begin - CACHING_PAGESIZE;
            }
        };

        let expand_end = |end: &mut u64, add_value: u64| {
            if add_value > max_page - *end {
                *end = max_page;
            } else {
                *end += add_value;
            }
        };

        if begin == 0 {
            return OverlapResult {
                ids: overlap_ids,
                begin,
                end,
                has_stream_leap: false,
            };
        }

        let mut stream_score: i32 = 0;
        let mut has_stream_leap = false;
        let mut scan_addr = device_addr;

        loop {
            if scan_addr >> CACHING_PAGEBITS >= div_ceil(end, CACHING_PAGESIZE) {
                break;
            }

            let overlap_id = self.page_table[(scan_addr >> CACHING_PAGEBITS) as usize];
            if overlap_id.is_valid() && !self.slot_buffers[overlap_id].is_picked() {
                overlap_ids.push(overlap_id);
                self.slot_buffers[overlap_id].pick();

                let overlap_device_addr = self.slot_buffers[overlap_id].cpu_addr();
                let expands_left = overlap_device_addr < begin;
                if expands_left {
                    begin = overlap_device_addr;
                }
                let overlap_end =
                    overlap_device_addr + self.slot_buffers[overlap_id].size_bytes() as u64;
                let expands_right = overlap_end > end;
                if expands_right {
                    end = overlap_end;
                }

                stream_score += self.slot_buffers[overlap_id].stream_score();
                if stream_score > STREAM_LEAP_THRESHOLD && !has_stream_leap {
                    has_stream_leap = true;
                    if expands_right {
                        // Upstream mutates the loop's `device_addr` here. The
                        // following loop increment then resumes at `begin`,
                        // rescanning the newly included left-hand range so
                        // every old buffer in it is joined into the expanded
                        // stream buffer.
                        expand_begin(&mut begin, &mut scan_addr, CACHING_PAGESIZE * 128);
                    }
                    if expands_left {
                        expand_end(&mut end, CACHING_PAGESIZE * 128);
                    }
                }
            }

            scan_addr = scan_addr.wrapping_add(CACHING_PAGESIZE);
        }

        // Unmark picked buffers.
        for &id in &overlap_ids {
            self.slot_buffers[id].unpick();
        }

        OverlapResult {
            ids: overlap_ids,
            begin,
            end,
            has_stream_leap,
        }
    }

    /// Copy an overlapping buffer into `new_buffer_id` and delete the overlap.
    ///
    /// Upstream: `BufferCache<P>::JoinOverlap`
    ///
    fn join_overlap(
        &mut self,
        new_buffer_id: BufferId,
        overlap_id: BufferId,
        accumulate_stream_score: bool,
    ) {
        if accumulate_stream_score {
            let score = self.slot_buffers[overlap_id].stream_score() + 1;
            self.slot_buffers[new_buffer_id].increase_stream_score(score);
        }

        // Copy data from the overlap buffer into the new buffer.
        let overlap_start = self.slot_buffers[overlap_id].cpu_addr();
        let overlap_size = self.slot_buffers[overlap_id].size_bytes() as u64;
        let new_start = self.slot_buffers[new_buffer_id].cpu_addr();
        let dst_offset = overlap_start.wrapping_sub(new_start);
        let copies = [BufferCopy {
            src_offset: 0,
            dst_offset,
            size: overlap_size,
        }];
        self.slot_buffers[new_buffer_id].mark_usage(dst_offset, overlap_size);
        self.runtime.copy_buffer(
            &self.slot_buffers[new_buffer_id],
            &self.slot_buffers[overlap_id],
            &copies,
            true,
            false,
        );
        self.delete_buffer(overlap_id, true);
    }

    /// Allocate a new buffer that covers `[device_addr, device_addr+wanted_size)`,
    /// merging any overlapping buffers.
    ///
    /// Upstream: `BufferCache<P>::CreateBuffer`
    ///
    fn create_buffer(&mut self, device_addr: VAddr, wanted_size: u32) -> BufferId {
        // Align start and end to caching page boundaries.
        let device_addr_end =
            (device_addr + wanted_size as u64 + CACHING_PAGESIZE - 1) & !(CACHING_PAGESIZE - 1);
        let device_addr = device_addr & !(CACHING_PAGESIZE - 1);
        let wanted_size = (device_addr_end - device_addr) as u32;

        let overlap = self.resolve_overlaps(device_addr, wanted_size);
        let size = (overlap.end - overlap.begin) as u32;

        let new_buffer = P::Buffer::new(&mut self.runtime, overlap.begin, size as u64);

        let new_buffer_id = self.slot_buffers.insert(new_buffer);
        self.runtime
            .clear_buffer(&self.slot_buffers[new_buffer_id], 0, size as u64, 0);
        self.slot_buffers[new_buffer_id].mark_usage(0, size as u64);

        let overlap_ids: Vec<BufferId> = overlap.ids.clone();
        let has_stream_leap = overlap.has_stream_leap;
        for overlap_id in overlap_ids {
            self.join_overlap(new_buffer_id, overlap_id, !has_stream_leap);
        }

        self.register(new_buffer_id);
        self.touch_buffer(new_buffer_id);
        new_buffer_id
    }

    /// Register a buffer in the page table and update memory accounting.
    ///
    /// Upstream: `BufferCache<P>::Register`
    fn register(&mut self, buffer_id: BufferId) {
        self.change_register(buffer_id, true);
    }

    /// Unregister a buffer from the page table and update memory accounting.
    ///
    /// Upstream: `BufferCache<P>::Unregister`
    fn unregister(&mut self, buffer_id: BufferId) {
        self.change_register(buffer_id, false);
    }

    /// Insert or remove a buffer from the page table.
    ///
    /// Upstream: `BufferCache<P>::ChangeRegister<insert>`
    fn change_register(&mut self, buffer_id: BufferId, insert: bool) {
        let (device_addr_begin, size) = {
            let buffer = &self.slot_buffers[buffer_id];
            (buffer.cpu_addr(), buffer.size_bytes())
        };

        if insert {
            self.total_used_memory += (size + 1023) as u64 & !1023u64; // AlignUp(size, 1024)
            let lru_id = self.lru_cache.insert(buffer_id, self.frame_tick);
            self.slot_buffers[buffer_id].set_lru_id(lru_id);
        } else {
            let aligned = (size + 1023) as u64 & !1023u64;
            self.total_used_memory = self.total_used_memory.wrapping_sub(aligned);
            let lru_id = self.slot_buffers[buffer_id].get_lru_id();
            self.lru_cache.free(lru_id);
        }

        let device_addr_end = device_addr_begin + size as u64;
        let page_begin = device_addr_begin / CACHING_PAGESIZE;
        let page_end = div_ceil(device_addr_end, CACHING_PAGESIZE);

        for page in page_begin..page_end {
            if insert {
                self.page_table[page as usize] = buffer_id;
            } else {
                self.page_table[page as usize] = SlotId::invalid();
            }
        }
    }

    /// Update the LRU position of a buffer.
    ///
    /// Upstream: `BufferCache<P>::TouchBuffer`
    fn touch_buffer(&mut self, buffer_id: BufferId) {
        if buffer_id != NULL_BUFFER_ID && buffer_id.is_valid() {
            let lru_id = self.slot_buffers[buffer_id].get_lru_id();
            self.lru_cache.touch(lru_id, self.frame_tick);
        }
    }

    /// Synchronize CPU-modified data to the GPU buffer.
    ///
    /// Upstream: `BufferCache<P>::SynchronizeBuffer`
    ///
    /// Returns `true` if no upload was needed (region was already clean).
    fn synchronize_buffer(&mut self, buffer_id: BufferId, device_addr: VAddr, size: u32) -> bool {
        let mut copies: Vec<BufferCopy> = Vec::new();
        let mut total_size_bytes: u64 = 0;
        let mut largest_copy: u64 = 0;

        let buffer_start = self.slot_buffers[buffer_id].cpu_addr();

        self.memory_tracker.for_each_upload_range(
            device_addr,
            size as u64,
            &mut |device_addr_out, range_size| {
                copies.push(BufferCopy {
                    src_offset: total_size_bytes,
                    dst_offset: device_addr_out - buffer_start,
                    size: range_size,
                });
                total_size_bytes += range_size;
                largest_copy = largest_copy.max(range_size);
            },
        );

        if total_size_bytes == 0 {
            return true;
        }

        self.upload_memory(buffer_id, total_size_bytes, largest_copy, &mut copies);
        self.any_buffer_uploaded = true;
        false
    }

    /// Upload CPU data to a GPU buffer.
    ///
    /// Upstream: `BufferCache<P>::UploadMemory`
    fn upload_memory(
        &mut self,
        buffer_id: BufferId,
        total_size_bytes: u64,
        largest_copy: u64,
        copies: &mut [BufferCopy],
    ) {
        if P::USE_MEMORY_MAPS_FOR_UPLOADS {
            self.mapped_upload_memory(buffer_id, total_size_bytes, copies);
        } else {
            self.immediate_upload_memory(buffer_id, largest_copy, copies);
        }
    }

    /// Upload memory via direct buffer writes.
    ///
    /// Upstream: `BufferCache<P>::ImmediateUploadMemory`
    fn immediate_upload_memory(
        &mut self,
        _buffer_id: BufferId,
        largest_copy: u64,
        copies: &[BufferCopy],
    ) {
        if P::USE_MEMORY_MAPS_FOR_UPLOADS {
            return; // This path is only for the non-memory-map case.
        }
        if self.device_memory.is_none() {
            return;
        }
        for copy in copies {
            let device_addr = self.slot_buffers[_buffer_id].cpu_addr() + copy.dst_offset;
            if Self::is_range_granular(device_addr, copy.size as usize) {
                if let Some(ptr) = self
                    .device_memory
                    .as_ref()
                    .and_then(|dm| dm.get_pointer(device_addr))
                {
                    let upload_span =
                        unsafe { std::slice::from_raw_parts(ptr, copy.size as usize) };
                    self.slot_buffers[_buffer_id].immediate_upload(copy.dst_offset, upload_span);
                    continue;
                }
            }
            if *common::settings::values()
                .enable_gpu_buffer_readback
                .get_value()
            {
                self.download_buffer_memory_range(_buffer_id, device_addr, copy.size);
            }
            if let Some(ref dm) = self.device_memory {
                let immediate_buffer =
                    Self::immediate_buffer(&mut self.immediate_buffer_alloc, largest_copy as usize);
                dm.read_block_unsafe(device_addr, &mut immediate_buffer[..copy.size as usize]);
                self.slot_buffers[_buffer_id]
                    .immediate_upload(copy.dst_offset, &immediate_buffer[..copy.size as usize]);
            }
        }
    }

    /// Upload memory via staging buffer.
    ///
    /// Upstream: `BufferCache<P>::MappedUploadMemory`
    fn mapped_upload_memory(
        &mut self,
        buffer_id: BufferId,
        total_size_bytes: u64,
        copies: &mut [BufferCopy],
    ) {
        if !P::USE_MEMORY_MAPS {
            return;
        }
        let mut staging = self.runtime.upload_staging_buffer(total_size_bytes);
        for copy in copies.iter_mut() {
            let device_addr = self.slot_buffers[buffer_id].cpu_addr() + copy.dst_offset;
            if *common::settings::values()
                .enable_gpu_buffer_readback
                .get_value()
            {
                self.download_buffer_memory_range(buffer_id, device_addr, copy.size);
            }
            if let Some(ref dm) = self.device_memory {
                let src_start = copy.src_offset as usize;
                let src_end = src_start + copy.size as usize;
                let mapped_span = staging.mapped_span_mut();
                dm.read_block_unsafe(device_addr, &mut mapped_span[src_start..src_end]);
            }
            copy.src_offset += staging.offset();
        }
        let can_reorder = self
            .runtime
            .can_reorder_upload(&self.slot_buffers[buffer_id], copies);
        self.runtime.copy_buffer_from_staging(
            &self.slot_buffers[buffer_id],
            &staging,
            copies,
            true,
            can_reorder,
        );
    }

    /// Download buffer memory back to the CPU (full buffer).
    ///
    /// Upstream: `BufferCache<P>::DownloadBufferMemory(Buffer&)`
    fn download_buffer_memory(&mut self, buffer_id: BufferId) {
        let (cpu_addr, size_bytes) = {
            let b = &self.slot_buffers[buffer_id];
            (b.cpu_addr(), b.size_bytes() as u64)
        };
        self.download_buffer_memory_range(buffer_id, cpu_addr, size_bytes);
    }

    /// Download a sub-range of buffer memory back to the CPU.
    ///
    /// Upstream: `BufferCache<P>::DownloadBufferMemory(Buffer&, DAddr, u64)`
    fn download_buffer_memory_range(&mut self, buffer_id: BufferId, device_addr: VAddr, size: u64) {
        // Collect the ranges that need to be downloaded via memory_tracker.
        // We split the logic into two phases to avoid the borrow conflict:
        // Phase 1: collect download ranges from memory_tracker (borrows memory_tracker).
        // Phase 2: apply range subtractions and build copy list (borrows gpu_modified_ranges).
        let buffer_addr = self.slot_buffers[buffer_id].cpu_addr();

        let mut download_ranges: Vec<(VAddr, u64)> = Vec::new();
        self.memory_tracker.for_each_download_range_and_clear(
            device_addr,
            size,
            &mut |device_addr_out: VAddr, range_size: u64| {
                download_ranges.push((device_addr_out, range_size));
            },
        );

        let mut copies: Vec<BufferCopy> = Vec::new();
        let mut total_size_bytes: u64 = 0;
        let mut largest_copy: u64 = 0;

        for (device_addr_out, range_size) in download_ranges {
            // Iterate GPU-modified sub-ranges using RangeSet::for_each_in_range.
            let mut sub_intervals: Vec<(VAddr, VAddr)> = Vec::new();
            self.gpu_modified_ranges.for_each_in_range(
                device_addr_out,
                range_size as usize,
                |new_start, new_end| {
                    sub_intervals.push((new_start, new_end));
                },
            );

            for (new_start, new_end) in sub_intervals {
                let new_offset = new_start - buffer_addr;
                let new_size = new_end - new_start;
                copies.push(BufferCopy {
                    src_offset: new_offset,
                    dst_offset: total_size_bytes,
                    size: new_size,
                });
                constexpr_align_up(&mut total_size_bytes, new_size, 64);
                largest_copy = largest_copy.max(new_size);
            }

            self.clear_download(device_addr_out, range_size);
            self.gpu_modified_ranges
                .subtract(device_addr_out, range_size as usize);
        }

        if total_size_bytes == 0 {
            return;
        }

        if DISABLE_DOWNLOADS {
            return;
        }

        // Upstream: download GPU data to device memory via staging or immediate path.
        if P::USE_MEMORY_MAPS {
            // Memory-mapped download path.
            let download_staging = self
                .runtime
                .download_staging_buffer(total_size_bytes, false);
            let mut adjusted_copies = copies.clone();
            for copy in adjusted_copies.iter_mut() {
                copy.dst_offset += download_staging.offset();
                self.slot_buffers[buffer_id].mark_usage(copy.src_offset, copy.size);
            }
            self.runtime.copy_buffer_to_staging(
                &download_staging,
                &self.slot_buffers[buffer_id],
                &adjusted_copies,
                true,
            );
            self.runtime.finish();
            if let Some(ref dm) = self.device_memory {
                let mapped_memory = download_staging.mapped_span();
                for (i, copy) in adjusted_copies.iter().enumerate() {
                    let copy_device_addr =
                        self.slot_buffers[buffer_id].cpu_addr() + copies[i].src_offset;
                    let dst_offset = (copy.dst_offset - download_staging.offset()) as usize;
                    let end = dst_offset + copies[i].size as usize;
                    dm.write_block_unsafe(copy_device_addr, &mapped_memory[dst_offset..end]);
                }
            }
        } else {
            // Immediate download path.
            if let Some(ref dm) = self.device_memory {
                let immediate_buffer =
                    Self::immediate_buffer(&mut self.immediate_buffer_alloc, largest_copy as usize);
                for copy in &copies {
                    self.slot_buffers[buffer_id].immediate_download(
                        copy.src_offset,
                        &mut immediate_buffer[..copy.size as usize],
                    );
                    let copy_device_addr =
                        self.slot_buffers[buffer_id].cpu_addr() + copy.src_offset;
                    dm.write_block_unsafe(
                        copy_device_addr,
                        &immediate_buffer[..copy.size as usize],
                    );
                }
            }
        }
    }

    /// Delete a buffer, cleaning up all state that references it.
    ///
    /// Upstream: `BufferCache<P>::DeleteBuffer`
    fn delete_buffer(&mut self, buffer_id: BufferId, do_not_mark: bool) {
        let Some(cs) = self.channel_caches.current_channel_state_mut() else {
            return;
        };

        // Clear any bindings that reference this buffer.
        let mut dirty_index = false;
        let mut dirty_vertex_buffers = Vec::new();
        let mut updated_vertex_slots = Vec::new();
        if cs.index_buffer.buffer_id == buffer_id {
            cs.index_buffer.buffer_id = NULL_BUFFER_ID;
            dirty_index = true;
        }
        for (index, binding) in cs.vertex_buffers.iter_mut().enumerate() {
            if binding.buffer_id == buffer_id {
                binding.buffer_id = NULL_BUFFER_ID;
                dirty_vertex_buffers.push(index as u32);
                updated_vertex_slots.push((index as u32, *binding));
            }
        }
        for stage_buffers in cs.uniform_buffers.iter_mut() {
            for binding in stage_buffers.iter_mut() {
                if binding.buffer_id == buffer_id {
                    binding.buffer_id = NULL_BUFFER_ID;
                }
            }
        }
        for stage_buffers in cs.storage_buffers.iter_mut() {
            for binding in stage_buffers.iter_mut() {
                if binding.buffer_id == buffer_id {
                    binding.buffer_id = NULL_BUFFER_ID;
                }
            }
        }
        for binding in cs.transform_feedback_buffers.iter_mut() {
            if binding.buffer_id == buffer_id {
                binding.buffer_id = NULL_BUFFER_ID;
            }
        }
        for binding in cs.compute_uniform_buffers.iter_mut() {
            if binding.buffer_id == buffer_id {
                binding.buffer_id = NULL_BUFFER_ID;
            }
        }
        for binding in cs.compute_storage_buffers.iter_mut() {
            if binding.buffer_id == buffer_id {
                binding.buffer_id = NULL_BUFFER_ID;
            }
        }
        for (index, binding) in updated_vertex_slots {
            self.update_vertex_buffer_slot(index, binding);
        }

        // Mark the whole buffer as CPU-modified to stop tracking.
        if !do_not_mark {
            let (cpu_addr, size_bytes) = {
                let b = &self.slot_buffers[buffer_id];
                (b.cpu_addr(), b.size_bytes() as u64)
            };
            self.memory_tracker
                .mark_region_as_cpu_modified(cpu_addr, size_bytes);
        }

        self.unregister(buffer_id);
        let buffer = self.slot_buffers.take(buffer_id);
        self.delayed_destruction_ring.push(buffer);

        if P::HAS_PERSISTENT_UNIFORM_BUFFER_BINDINGS {
            if let Some(cs) = self.channel_caches.current_channel_state_mut() {
                cs.dirty_uniform_buffers.fill(!0u32);
                cs.uniform_buffer_binding_sizes =
                    [[0; NUM_GRAPHICS_UNIFORM_BUFFERS as usize]; NUM_STAGES as usize];
            }
        }

        if dirty_index {
            self.set_geometry_dirty(DirtyFlag::IndexBuffer);
        }
        if !dirty_vertex_buffers.is_empty() {
            self.set_geometry_dirty(DirtyFlag::VertexBuffers);
            for index in dirty_vertex_buffers {
                self.set_geometry_dirty(DirtyFlag::VertexBuffer(index));
            }
        }
        if let Some(cs) = self.channel_caches.current_channel_state_mut() {
            cs.has_deleted_buffers = true;
        }
    }

    /// Build a storage buffer binding from a GPU virtual SSBO address.
    ///
    /// Upstream: `BufferCache<P>::StorageBufferBinding`
    fn storage_buffer_binding(&self, ssbo_addr: u64, cbuf_index: u32, is_written: bool) -> Binding {
        let Some(ref gm) = self.gpu_memory else {
            log::warn!(
                "storage_buffer_binding: gpu_memory not available for cbuf_index {}",
                cbuf_index
            );
            return NULL_BINDING;
        };

        let read_u64 = |gpu_addr: u64| -> Option<u64> {
            if let (Some(device_addr), Some(device_memory)) =
                (gm.gpu_to_cpu_address(gpu_addr), self.device_memory.as_ref())
            {
                let mut buf = [0u8; 8];
                device_memory.read_block_unsafe(device_addr, &mut buf);
                return Some(u64::from_le_bytes(buf));
            }
            gm.read_u64(gpu_addr)
        };
        let read_u32 = |gpu_addr: u64| -> Option<u32> {
            if let (Some(device_addr), Some(device_memory)) =
                (gm.gpu_to_cpu_address(gpu_addr), self.device_memory.as_ref())
            {
                let mut buf = [0u8; 4];
                device_memory.read_block_unsafe(device_addr, &mut buf);
                return Some(u32::from_le_bytes(buf));
            }
            gm.read_u32(gpu_addr)
        };

        // Upstream: const GPUVAddr gpu_addr = gpu_memory->Read<u64>(ssbo_addr);
        self.storage_buffer_binding_with_gpu_reader(
            ssbo_addr,
            cbuf_index,
            is_written,
            |gpu_addr| gm.gpu_to_cpu_address(gpu_addr),
            |gpu_addr| gm.get_memory_layout_size(gpu_addr),
            |gpu_addr, out| match out.len() {
                4 => {
                    let Some(value) = read_u32(gpu_addr) else {
                        return false;
                    };
                    out.copy_from_slice(&value.to_le_bytes());
                    true
                }
                8 => {
                    let Some(value) = read_u64(gpu_addr) else {
                        return false;
                    };
                    out.copy_from_slice(&value.to_le_bytes());
                    true
                }
                _ => false,
            },
        )
    }

    fn storage_buffer_binding_with_gpu_reader(
        &self,
        ssbo_addr: u64,
        cbuf_index: u32,
        is_written: bool,
        mut gpu_to_cpu_address: impl FnMut(u64) -> Option<u64>,
        mut get_memory_layout_size: impl FnMut(u64) -> u64,
        mut read_block: impl FnMut(u64, &mut [u8]) -> bool,
    ) -> Binding {
        let mut gpu_addr_bytes = [0u8; 8];
        let gpu_addr = match read_block(ssbo_addr, &mut gpu_addr_bytes) {
            true => u64::from_le_bytes(gpu_addr_bytes),
            false => return NULL_BINDING,
        };
        if gpu_addr == 0 {
            return NULL_BINDING;
        }

        // Upstream accepts the next qword as a packed size only when its high
        // half is zero and the size fits in the mapped memory layout. This is
        // independent of the constant-buffer index.
        let size = {
            let memory_layout_size = get_memory_layout_size(gpu_addr) as u32;
            let mut next_qword_bytes = [0u8; 8];
            let next_qword = if read_block(ssbo_addr + 8, &mut next_qword_bytes) {
                u64::from_le_bytes(next_qword_bytes)
            } else {
                0
            };
            let packed_size = next_qword as u32;
            let next_qword_is_size = (next_qword >> 32) as u32 == 0
                && packed_size != 0
                && packed_size <= memory_layout_size;
            if next_qword_is_size {
                packed_size
            } else {
                memory_layout_size.min(8 * 1024 * 1024)
            }
        };

        // Upstream: alignment only applies to the offset of the buffer.
        let alignment = self.runtime.get_storage_buffer_alignment();
        let aligned_gpu_addr = gpu_addr & !(alignment as u64 - 1);
        let aligned_size = (gpu_addr - aligned_gpu_addr) as u32 + size;

        let aligned_device_addr = gpu_to_cpu_address(aligned_gpu_addr);
        if aligned_device_addr.is_none() || size == 0 {
            log::warn!(
                "storage_buffer_binding: Failed to find storage buffer for cbuf index {}",
                cbuf_index
            );
            return NULL_BINDING;
        }
        let device_addr = gpu_to_cpu_address(gpu_addr);
        if device_addr.is_none() {
            log::warn!(
                "storage_buffer_binding: Unaligned storage buffer address not found for cbuf index {}",
                cbuf_index
            );
            return NULL_BINDING;
        }

        // Upstream: const DAddr cpu_end = Common::AlignUp(*device_addr + size, Core::DEVICE_PAGESIZE);
        let cpu_end =
            (device_addr.unwrap() + size as u64 + DEVICE_PAGESIZE - 1) & !(DEVICE_PAGESIZE - 1);

        Binding {
            device_addr: aligned_device_addr.unwrap(),
            size: if is_written {
                aligned_size
            } else {
                (cpu_end - aligned_device_addr.unwrap()) as u32
            },
            buffer_id: NULL_BUFFER_ID,
        }
    }

    /// Build a texture buffer binding from a GPU virtual address.
    ///
    /// Upstream: `BufferCache<P>::GetTextureBufferBinding`
    fn get_texture_buffer_binding(
        &mut self,
        gpu_addr: u64,
        size: u32,
        format: PixelFormat,
    ) -> TextureBufferBinding {
        if size == 0 {
            return TextureBufferBinding::default();
        }
        let device_addr = self
            .gpu_memory
            .as_ref()
            .and_then(|gm| gm.gpu_to_cpu_address(gpu_addr));
        match device_addr {
            Some(addr) => TextureBufferBinding {
                device_addr: addr,
                size,
                buffer_id: NULL_BUFFER_ID,
                format,
            },
            None => TextureBufferBinding::default(),
        }
    }

    /// Get an immediate buffer slice backed by device memory at `device_addr`.
    ///
    /// Upstream: `BufferCache<P>::ImmediateBufferWithData`
    fn immediate_buffer_with_data<'a>(
        device_memory: &'a dyn DeviceMemoryAccess,
        immediate_buffer_alloc: &'a mut Vec<u8>,
        device_addr: VAddr,
        size: usize,
    ) -> &'a [u8] {
        if let Some(base_pointer) = device_memory.get_pointer(device_addr) {
            let contiguous = Self::is_range_granular(device_addr, size)
                || device_memory
                    .get_pointer(device_addr.wrapping_add(size as u64))
                    .is_some_and(|end_pointer| base_pointer.wrapping_add(size) == end_pointer);
            if contiguous {
                // SAFETY: `get_pointer` exposes the guest-memory backing for
                // this range, and the same page/continuity checks as Eden's
                // `ImmediateBufferWithData` prove all `size` bytes contiguous.
                return unsafe { std::slice::from_raw_parts(base_pointer, size) };
            }
        }
        let span = Self::immediate_buffer(immediate_buffer_alloc, size);
        device_memory.read_block_unsafe(device_addr, span);
        span
    }

    /// Ensure `immediate_buffer_alloc` has at least `wanted_capacity` bytes and return a slice.
    ///
    /// Upstream: `BufferCache<P>::ImmediateBuffer`
    fn immediate_buffer(immediate_buffer_alloc: &mut Vec<u8>, wanted_capacity: usize) -> &mut [u8] {
        if immediate_buffer_alloc.len() < wanted_capacity {
            immediate_buffer_alloc.resize(wanted_capacity, 0u8);
        }
        &mut immediate_buffer_alloc[..wanted_capacity]
    }

    /// Return true if a fast uniform buffer is currently bound at `(stage, binding_index)`.
    ///
    /// Upstream: `BufferCache<P>::HasFastUniformBufferBound`
    fn has_fast_uniform_buffer_bound(&self, stage: usize, binding_index: u32) -> bool {
        if P::IS_OPENGL {
            self.channel_caches
                .current_channel_state()
                .map_or(false, |cs| {
                    ((cs.fast_bound_uniform_buffers[stage] >> binding_index) & 1) != 0
                })
        } else {
            // Only OpenGL has fast uniform buffers.
            false
        }
    }

    /// Remove `[base_addr, base_addr+size)` from all download tracking structures.
    ///
    /// Upstream: `BufferCache<P>::ClearDownload`
    fn clear_download(&mut self, base_addr: VAddr, size: u64) {
        // Upstream: async_downloads.DeleteAll(base_addr, size);
        self.async_downloads.delete_all(base_addr, size as usize);
        self.uncommitted_gpu_modified_ranges
            .subtract(base_addr, size as usize);
        for range_set in self.committed_gpu_modified_ranges.iter_mut() {
            range_set.subtract(base_addr, size as usize);
        }
    }

    /// Perform the inline memory write into the buffer cache.
    ///
    /// Upstream: `BufferCache<P>::InlineMemoryImplementation`
    fn inline_memory_implementation(
        &mut self,
        dest_address: VAddr,
        copy_size: usize,
        inlined_buffer: &[u8],
    ) {
        self.clear_download(dest_address, copy_size as u64);
        self.gpu_modified_ranges.subtract(dest_address, copy_size);

        let buffer_id = self.find_buffer(dest_address, copy_size as u32);
        self.synchronize_buffer(buffer_id, dest_address, copy_size as u32);

        if P::USE_MEMORY_MAPS_FOR_UPLOADS {
            let mut staging = self.runtime.upload_staging_buffer(copy_size as u64);
            let mapped_span = staging.mapped_span_mut();
            mapped_span[..copy_size].copy_from_slice(&inlined_buffer[..copy_size]);

            let offset = self.slot_buffers[buffer_id].offset(dest_address);
            let copies = [BufferCopy {
                src_offset: staging.offset(),
                dst_offset: offset as u64,
                size: copy_size as u64,
            }];
            self.runtime.copy_buffer_from_staging(
                &self.slot_buffers[buffer_id],
                &staging,
                &copies,
                true,
                false,
            );
        } else {
            let offset = self.slot_buffers[buffer_id].offset(dest_address);
            self.slot_buffers[buffer_id]
                .immediate_upload(offset as u64, &inlined_buffer[..copy_size]);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Align `total` up by `new_size` rounded to `align`.
#[inline]
fn constexpr_align_up(total: &mut u64, new_size: u64, align: u64) {
    *total += (new_size + align - 1) & !(align - 1);
}

// ---------------------------------------------------------------------------
// RasterizerDownloadArea — return type for GetFlushArea
// ---------------------------------------------------------------------------

/// Describes an area that must be flushed from GPU to CPU.
#[derive(Debug, Clone, Copy)]
pub struct RasterizerDownloadArea {
    pub start_address: VAddr,
    pub end_address: VAddr,
    pub preemtive: bool,
}

#[cfg(test)]
mod tests {
    use super::super::word_manager::DeviceTracker;
    use super::*;

    struct DummyTracker;
    impl DeviceTracker for DummyTracker {
        fn update_pages_cached_batch(&self, _ranges: &[(VAddr, usize)], _delta: i32) {}
    }

    struct TestParams;
    impl BufferCacheParams for TestParams {
        type Runtime = TestBufferCacheRuntime;
        type Buffer = TestBuffer;
        type AsyncBuffer = StagingBufferRef;

        const IS_OPENGL: bool = false;
        const HAS_PERSISTENT_UNIFORM_BUFFER_BINDINGS: bool = false;
        const HAS_FULL_INDEX_AND_PRIMITIVE_SUPPORT: bool = true;
        const NEEDS_BIND_UNIFORM_INDEX: bool = false;
        const NEEDS_BIND_STORAGE_INDEX: bool = false;
        const USE_MEMORY_MAPS: bool = false;
        const SEPARATE_IMAGE_BUFFER_BINDINGS: bool = false;
        const USE_MEMORY_MAPS_FOR_UPLOADS: bool = false;
    }

    struct IdentityGpuMemory;

    impl GpuMemoryAccess for IdentityGpuMemory {
        fn gpu_to_cpu_address(&self, gpu_addr: u64) -> Option<u64> {
            Some(gpu_addr)
        }

        fn read_u64(&self, _gpu_addr: u64) -> Option<u64> {
            None
        }

        fn read_u32(&self, _gpu_addr: u64) -> Option<u32> {
            None
        }

        fn is_within_gpu_address_range(&self, _gpu_addr: u64) -> bool {
            true
        }

        fn max_continuous_range(&self, _gpu_addr: u64, size: u64) -> u64 {
            size
        }

        fn get_memory_layout_size(&self, _gpu_addr: u64) -> u64 {
            0x1_0000
        }
    }

    struct SharedDeviceMemory {
        bytes: std::sync::Arc<parking_lot::Mutex<Vec<u8>>>,
    }

    impl DeviceMemoryAccess for SharedDeviceMemory {
        fn get_pointer(&self, _device_addr: u64) -> Option<*const u8> {
            None
        }

        fn read_block_unsafe(&self, device_addr: u64, dst: &mut [u8]) {
            let start = device_addr as usize;
            let end = start + dst.len();
            dst.copy_from_slice(&self.bytes.lock()[start..end]);
        }

        fn write_block_unsafe(&self, device_addr: u64, src: &[u8]) {
            let start = device_addr as usize;
            let end = start + src.len();
            self.bytes.lock()[start..end].copy_from_slice(src);
        }
    }

    struct ImmediateDeviceMemory {
        bytes: Vec<u8>,
        expose_pointer: bool,
    }

    impl DeviceMemoryAccess for ImmediateDeviceMemory {
        fn get_pointer(&self, device_addr: u64) -> Option<*const u8> {
            let offset = usize::try_from(device_addr).ok()?;
            if !self.expose_pointer || offset > self.bytes.len() {
                return None;
            }
            Some(self.bytes.as_ptr().wrapping_add(offset))
        }

        fn read_block_unsafe(&self, device_addr: u64, dst: &mut [u8]) {
            let start = device_addr as usize;
            dst.copy_from_slice(&self.bytes[start..start + dst.len()]);
        }

        fn write_block_unsafe(&self, _device_addr: u64, _src: &[u8]) {}
    }

    fn bind_test_channel(cache: &mut BufferCache<TestParams, DummyTracker>, bind_id: i32) {
        let channel = ChannelState::new(bind_id);
        cache.create_channel(&channel);
        cache.bind_to_channel(bind_id);
    }

    #[test]
    fn test_buffer_cache_construction() {
        let tracker = DummyTracker;
        let cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        assert!(!cache.has_uncommitted_flushes());
        assert!(!cache.should_wait_async_flushes());
    }

    #[test]
    fn construction_uses_upstream_reported_device_memory_thresholds() {
        let tracker = DummyTracker;
        let cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::with_device_local_memory(8 * 1024 * 1024 * 1024),
        );

        assert_eq!(cache.minimum_memory, 6_012_954_215);
        assert_eq!(cache.critical_memory, 7_730_941_133);
    }

    #[test]
    fn changing_uniform_buffer_masks_clears_fast_bindings_without_persistent_bindings() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        bind_test_channel(&mut cache, 1);
        let sizes = [[0; NUM_GRAPHICS_UNIFORM_BUFFERS as usize]; NUM_STAGES as usize];
        let first_mask = [1; NUM_STAGES as usize];

        unsafe { cache.set_uniform_buffers_state(&first_mask, &sizes) };
        assert_eq!(
            cache
                .current_channel_state()
                .unwrap()
                .uniform_buffer_sizes
                .unwrap()
                .as_ptr()
                .cast_const(),
            std::ptr::from_ref(&sizes)
        );
        cache
            .current_channel_state_mut()
            .unwrap()
            .fast_bound_uniform_buffers
            .fill(0xFFFF_FFFF);

        unsafe { cache.set_uniform_buffers_state(&first_mask, &sizes) };
        assert_eq!(
            cache
                .current_channel_state()
                .unwrap()
                .fast_bound_uniform_buffers,
            [0xFFFF_FFFF; NUM_STAGES as usize]
        );

        let second_mask = [2; NUM_STAGES as usize];
        unsafe { cache.set_uniform_buffers_state(&second_mask, &sizes) };
        assert_eq!(
            cache
                .current_channel_state()
                .unwrap()
                .fast_bound_uniform_buffers,
            [0; NUM_STAGES as usize]
        );
    }

    #[test]
    fn compute_uniform_buffer_sizes_keep_the_upstream_pipeline_pointer() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        bind_test_channel(&mut cache, 3);
        let sizes = [0x40; NUM_COMPUTE_UNIFORM_BUFFERS as usize];

        unsafe { cache.set_compute_uniform_buffer_state(1, &sizes) };

        assert_eq!(
            cache
                .current_channel_state()
                .unwrap()
                .compute_uniform_buffer_sizes
                .unwrap()
                .as_ptr()
                .cast_const(),
            std::ptr::from_ref(&sizes)
        );
    }

    #[test]
    fn channel_binding_rebinds_live_engines_and_preserves_per_channel_payload() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        let mut channel_a = ChannelState::new(11);
        channel_a.maxwell_3d = Some(Box::new(crate::engines::maxwell_3d::Maxwell3D::new()));
        channel_a.kepler_compute = Some(Box::default());
        let mut channel_b = ChannelState::new(12);
        channel_b.maxwell_3d = Some(Box::new(crate::engines::maxwell_3d::Maxwell3D::new()));
        channel_b.kepler_compute = Some(Box::default());
        let maxwell_a = (&**channel_a.maxwell_3d.as_ref().unwrap() as *const _) as usize;
        let maxwell_b = (&**channel_b.maxwell_3d.as_ref().unwrap() as *const _) as usize;

        cache.create_channel(&channel_a);
        cache.create_channel(&channel_b);
        cache.bind_to_channel(channel_a.bind_id);
        assert_eq!(cache.channel_caches.maxwell3d, Some(maxwell_a));
        cache
            .current_channel_state_mut()
            .unwrap()
            .enabled_uniform_buffer_masks[0] = 0x55;

        cache.bind_to_channel(channel_b.bind_id);
        assert_eq!(cache.channel_caches.maxwell3d, Some(maxwell_b));
        assert_eq!(
            cache
                .current_channel_state()
                .unwrap()
                .enabled_uniform_buffer_masks[0],
            0
        );

        cache.bind_to_channel(channel_a.bind_id);
        assert_eq!(
            cache
                .current_channel_state()
                .unwrap()
                .enabled_uniform_buffer_masks[0],
            0x55
        );
    }

    #[test]
    fn graphics_dirty_state_is_read_from_live_channel_maxwell() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        let mut owner = ChannelState::new(13);
        owner.maxwell_3d = Some(Box::new(Maxwell3D::new()));
        owner
            .maxwell_3d
            .as_mut()
            .unwrap()
            .dirty_flags_mut()
            .fill(false);

        cache.create_channel(&owner);
        cache.bind_to_channel(owner.bind_id);
        assert!(!cache.is_geometry_dirty(DirtyFlag::IndexBuffer));

        owner.maxwell_3d.as_mut().unwrap().dirty_flags_mut()
            [crate::dirty_flags::flags::INDEX_BUFFER as usize] = true;
        assert!(cache.is_geometry_dirty(DirtyFlag::IndexBuffer));

        cache.clear_geometry_dirty(DirtyFlag::IndexBuffer);
        assert!(
            !owner.maxwell_3d.as_ref().unwrap().dirty_flags()
                [crate::dirty_flags::flags::INDEX_BUFFER as usize]
        );
    }

    #[test]
    fn test_buffer_cache_mutex_is_reentrant() {
        let tracker = DummyTracker;
        let cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        let _lock_a = cache.mutex.lock();
        let _lock_b = cache.mutex.lock();
    }

    #[test]
    fn test_buffer_cache_mutex_can_be_held_during_mutation() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        let mutex = Arc::clone(&cache.mutex);
        let _lock = mutex.lock();

        cache.set_draw_indirect(None);
    }

    #[test]
    fn test_for_each_enabled_bit() {
        let mut bits = Vec::new();
        BufferCache::<TestParams, DummyTracker>::for_each_enabled_bit(0b1010_0101, |idx| {
            bits.push(idx);
        });
        assert_eq!(bits, vec![0, 2, 5, 7]);
    }

    #[test]
    fn immediate_buffer_uses_contiguous_guest_memory_across_page_boundary() {
        let memory = ImmediateDeviceMemory {
            bytes: (0..0x1020).map(|value| value as u8).collect(),
            expose_pointer: true,
        };
        let mut allocation = Vec::new();
        let span = BufferCache::<TestParams, DummyTracker>::immediate_buffer_with_data(
            &memory,
            &mut allocation,
            0xff0,
            0x20,
        );
        assert_eq!(span, &memory.bytes[0xff0..0x1010]);
        assert!(allocation.is_empty());
    }

    #[test]
    fn immediate_buffer_reads_non_contiguous_guest_memory_into_scratch() {
        let memory = ImmediateDeviceMemory {
            bytes: (0..64).map(|value| value as u8).collect(),
            expose_pointer: false,
        };
        let mut allocation = Vec::new();
        let span = BufferCache::<TestParams, DummyTracker>::immediate_buffer_with_data(
            &memory,
            &mut allocation,
            7,
            13,
        );
        assert_eq!(span, &memory.bytes[7..20]);
        assert_eq!(allocation.len(), 13);
    }

    #[test]
    fn test_is_range_granular() {
        // Same page
        assert!(BufferCache::<TestParams, DummyTracker>::is_range_granular(
            0x1000, 0x100
        ));
        // Cross page
        assert!(!BufferCache::<TestParams, DummyTracker>::is_range_granular(
            0x1F00, 0x200
        ));
    }

    #[test]
    fn test_tick_frame_no_channel() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        // Should return early without panicking when channel_state is None.
        cache.tick_frame();
        assert_eq!(cache.frame_tick, 0);
    }

    #[test]
    fn tick_frame_preserves_upstream_unsigned_cache_ratio_overflow() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        bind_test_channel(&mut cache, 2);
        let channel = cache.current_channel_state_mut().unwrap();
        channel.uniform_cache_hits[0] = 1 << 24;
        channel.uniform_cache_shots[0] = 1;

        cache.tick_frame();

        assert_eq!(
            cache
                .current_channel_state()
                .unwrap()
                .uniform_buffer_skip_cache_size,
            DEFAULT_SKIP_CACHE_SIZE
        );
    }

    #[test]
    fn test_write_memory_marks_cpu_modified() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        // write_memory should not panic.
        cache.write_memory(0x10000, 0x1000);
    }

    #[test]
    fn dma_copy_cancels_pending_destination_download() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        cache.set_gpu_memory(Box::new(IdentityGpuMemory));
        let src = 0x1_0000;
        let dst = 0x2_0000;
        cache.create_buffer(src, 0x1000);
        cache.create_buffer(dst, 0x1000);
        cache.async_downloads.add(dst, 0x200);
        cache.uncommitted_gpu_modified_ranges.add(dst, 0x200);
        let mut committed = RangeSet::new();
        committed.add(dst, 0x200);
        cache.committed_gpu_modified_ranges.push_back(committed);

        assert!(cache.dma_copy(src, dst, 0x200));

        assert!(cache.async_downloads.empty());
        assert!(cache.uncommitted_gpu_modified_ranges.empty());
        assert!(cache.committed_gpu_modified_ranges[0].empty());
    }

    #[test]
    fn dma_copy_mirrors_source_into_device_memory_destination() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        cache.set_gpu_memory(Box::new(IdentityGpuMemory));

        let src = 0x1_0000;
        let dst = 0x2_0000;
        let amount = 0x80;
        let bytes = std::sync::Arc::new(parking_lot::Mutex::new(vec![0u8; 0x3_0000]));
        {
            let mut memory = bytes.lock();
            for (index, byte) in memory[src..src + amount].iter_mut().enumerate() {
                *byte = index as u8 ^ 0x5a;
            }
            memory[dst..dst + amount].fill(0xcc);
        }
        cache.set_device_memory(Box::new(SharedDeviceMemory {
            bytes: std::sync::Arc::clone(&bytes),
        }));
        let src_buffer = cache.create_buffer(src as u64, 0x1000);
        let dst_buffer = cache.create_buffer(dst as u64, 0x1000);

        assert!(cache.dma_copy(src as u64, dst as u64, amount as u64));
        assert!(cache.slot_buffers[src_buffer].is_region_used(0, amount as u64));
        assert!(cache.slot_buffers[dst_buffer].is_region_used(0, amount as u64));

        let memory = bytes.lock();
        assert_eq!(&memory[dst..dst + amount], &memory[src..src + amount]);
    }

    #[test]
    fn dma_clear_marks_destination_usage() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        cache.set_gpu_memory(Box::new(IdentityGpuMemory));
        let dst = 0x2_0000;
        let buffer = cache.create_buffer(dst, 0x1000);

        assert!(cache.dma_clear(dst, 0x20, 0x3f80_0000));
        assert!(cache.slot_buffers[buffer].is_region_used(0, 0x80));
    }

    #[test]
    fn graphics_storage_binding_marks_the_bound_range_used() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        bind_test_channel(&mut cache, 20);
        let address = 0x2_0000;
        let size = 0x180;
        let buffer_id = cache.create_buffer(address, 0x1000);
        cache.slot_buffers[buffer_id].reset_usage_tracking();
        let channel = cache.current_channel_state_mut().unwrap();
        channel.enabled_storage_buffers[0] = 1;
        channel.storage_buffers[0][0] = Binding {
            device_addr: address,
            size,
            buffer_id,
        };

        cache.bind_host_graphics_storage_buffers(0);

        assert!(cache.slot_buffers[buffer_id].is_region_used(0, size as u64));
    }

    #[test]
    fn disabled_transform_feedback_does_not_mark_stale_bindings_written() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        let mut owner = ChannelState::new(21);
        owner.maxwell_3d = Some(Box::new(Maxwell3D::new()));
        cache.create_channel(&owner);
        cache.bind_to_channel(owner.bind_id);
        let address = 0x3_0000;
        let size = 0x200;
        let buffer_id = cache.create_buffer(address, 0x1000);
        cache.slot_buffers[buffer_id].reset_usage_tracking();
        cache
            .current_channel_state_mut()
            .unwrap()
            .transform_feedback_buffers[0] = Binding {
            device_addr: address,
            size,
            buffer_id,
        };

        cache.bind_host_transform_feedback_buffers();

        assert!(!cache.is_region_gpu_modified(address, size as usize));
        assert!(!cache.slot_buffers[buffer_id].is_region_used(0, size as u64));
    }

    #[test]
    fn test_on_cpu_write_unregistered() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        // An unregistered region should return false (no GPU data to flush).
        let result = cache.on_cpu_write(0x20000, 0x100);
        assert!(!result);
    }

    #[test]
    fn delete_buffer_replaces_channel_references_with_null_buffer() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        let mut owner = ChannelState::new(1);
        owner.maxwell_3d = Some(Box::new(Maxwell3D::new()));
        cache.create_channel(&owner);
        cache.bind_to_channel(owner.bind_id);
        let buffer_id = cache.create_buffer(0x10000, 0x1000);
        let channel = cache.current_channel_state_mut().unwrap();
        channel.index_buffer.buffer_id = buffer_id;
        channel.vertex_buffers[0].buffer_id = buffer_id;
        channel.vertex_buffers[7].buffer_id = buffer_id;
        channel.uniform_buffers[0][0].buffer_id = buffer_id;
        channel.storage_buffers[0][0].buffer_id = buffer_id;
        let vertex_0 = channel.vertex_buffers[0];
        let vertex_7 = channel.vertex_buffers[7];
        cache.update_vertex_buffer_slot(0, vertex_0);
        cache.update_vertex_buffer_slot(7, vertex_7);

        cache.delete_buffer(buffer_id, true);

        let channel = cache.current_channel_state().unwrap();
        assert_eq!(channel.index_buffer.buffer_id, NULL_BUFFER_ID);
        assert_eq!(channel.vertex_buffers[0].buffer_id, NULL_BUFFER_ID);
        assert_eq!(channel.vertex_buffers[7].buffer_id, NULL_BUFFER_ID);
        assert_eq!(channel.uniform_buffers[0][0].buffer_id, NULL_BUFFER_ID);
        assert_eq!(channel.storage_buffers[0][0].buffer_id, NULL_BUFFER_ID);
        let dirty = owner.maxwell_3d.as_ref().unwrap().dirty_flags();
        assert!(dirty[crate::dirty_flags::flags::INDEX_BUFFER as usize]);
        assert!(dirty[crate::dirty_flags::flags::VERTEX_BUFFERS as usize]);
        assert!(dirty[crate::dirty_flags::flags::VERTEX_BUFFER0 as usize]);
        assert!(dirty[crate::dirty_flags::flags::VERTEX_BUFFER0 as usize + 7]);
        assert_eq!(cache.enabled_vertex_buffers_mask & ((1 << 0) | (1 << 7)), 0);
        assert_eq!(cache.vertex_buffer_slot(0).buffer_id, NULL_BUFFER_ID);
        assert_eq!(cache.vertex_buffer_slot(7).buffer_id, NULL_BUFFER_ID);
    }

    #[test]
    fn update_vertex_buffer_slot_matches_upstream_mask_and_serial_contract() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        let buffer_id = cache.create_buffer(0x10000, 0x1000);
        let binding = Binding {
            device_addr: 0x10100,
            size: 0x80,
            buffer_id,
        };

        cache.update_vertex_buffer_slot(3, binding);
        assert_eq!(cache.vertex_buffers_serial, 1);
        assert_eq!(cache.enabled_vertex_buffers_mask, 1 << 3);
        assert_eq!(cache.vertex_buffer_slot(3).device_addr, binding.device_addr);

        cache.update_vertex_buffer_slot(
            3,
            Binding {
                buffer_id: NULL_BUFFER_ID,
                ..binding
            },
        );
        assert_eq!(cache.vertex_buffers_serial, 1);
        assert_eq!(cache.enabled_vertex_buffers_mask, 0);

        cache.update_vertex_buffer_slot(
            3,
            Binding {
                size: binding.size + 1,
                ..binding
            },
        );
        assert_eq!(cache.vertex_buffers_serial, 2);
        assert_eq!(cache.enabled_vertex_buffers_mask, 1 << 3);
    }

    #[test]
    fn stream_leap_rescans_buffers_in_expanded_left_range() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        let left = cache.create_buffer(0x00A0_0000, 0x1_0000);
        let right = cache.create_buffer(0x0120_0000, 0x2_0000);
        cache.slot_buffers[right].increase_stream_score(STREAM_LEAP_THRESHOLD + 1);

        let overlap = cache.resolve_overlaps(0x0120_0000, 0x1_0000);

        assert!(overlap.has_stream_leap);
        assert_eq!(overlap.begin, 0x00A0_0000);
        assert!(overlap.ids.contains(&right));
        assert!(
            overlap.ids.contains(&left),
            "the expanded left range must be rescanned so its buffer is copied"
        );
    }

    #[test]
    fn for_each_buffer_in_range_visits_a_multi_page_buffer_once() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        let buffer_id = cache.create_buffer(0x10_0000, 3 * CACHING_PAGESIZE as u32);
        let mut visited: SmallVec<[BufferId; 4]> = SmallVec::new();

        cache.for_each_buffer_in_range(0x10_1000, 2 * CACHING_PAGESIZE, |id, _| {
            visited.push(id);
        });

        assert_eq!(visited.as_slice(), &[buffer_id]);
    }

    #[test]
    fn commit_async_flushes_subtracts_later_ranges_and_queues_empty_fence_slot() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        let mut earlier = RangeSet::new();
        earlier.add(0x10_0000, 0x3000);
        let mut later = RangeSet::new();
        later.add(0x10_1000, 0x1000);
        cache.committed_gpu_modified_ranges.push_back(earlier);
        cache.committed_gpu_modified_ranges.push_back(later);

        cache.commit_async_flushes_high();

        assert!(cache.committed_gpu_modified_ranges.is_empty());
        assert_eq!(cache.async_buffers.len(), 1);
        assert!(cache.async_buffers.front().unwrap().is_none());
    }

    #[test]
    fn test_get_flush_area() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        let area = cache.get_flush_area(0x1234, 0x100);
        assert!(area.is_some());
        let a = area.unwrap();
        // Should be aligned to DEVICE_PAGESIZE (4096).
        assert_eq!(a.start_address % DEVICE_PAGESIZE, 0);
        assert_eq!(a.end_address % DEVICE_PAGESIZE, 0);
        assert!(a.start_address <= 0x1234);
        assert!(a.end_address >= 0x1234 + 0x100);
    }

    #[test]
    fn accumulated_flushes_still_require_a_real_fence() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        // Add something to uncommitted ranges.
        cache.uncommitted_gpu_modified_ranges.add(0x1000, 0x1000);
        assert!(cache.has_uncommitted_flushes());
        cache.accumulate_flushes();
        assert!(
            cache.has_uncommitted_flushes(),
            "SignalOrdering must not make the next fence stubbed"
        );
        cache.committed_gpu_modified_ranges.clear();
        assert!(!cache.has_uncommitted_flushes());
        // Upstream ShouldWaitAsyncFlushes checks the committed async buffer
        // queue, not committed_gpu_modified_ranges directly.
        assert!(!cache.should_wait_async_flushes());
    }

    #[test]
    fn test_disable_graphics_uniform_buffer() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        bind_test_channel(&mut cache, 1);
        cache.disable_graphics_uniform_buffer(0, 0);
        let cs = cache.current_channel_state().unwrap();
        assert_eq!(cs.uniform_buffers[0][0].device_addr, 0);
        assert_eq!(cs.uniform_buffers[0][0].buffer_id, NULL_BUFFER_ID);
    }

    #[test]
    fn test_unbind_graphics_storage_buffers() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::with_dynamic_storage_limit(8),
        );
        bind_test_channel(&mut cache, 1);
        if let Some(cs) = cache.current_channel_state_mut() {
            cs.enabled_storage_buffers[0] = 0xFF;
            cs.written_storage_buffers[0] = 0x0F;
            cs.total_graphics_storage_buffers = 8;
        }
        cache.unbind_graphics_storage_buffers(0);
        let cs = cache.current_channel_state().unwrap();
        assert_eq!(cs.enabled_storage_buffers[0], 0);
        assert_eq!(cs.written_storage_buffers[0], 0);
        assert_eq!(cs.total_graphics_storage_buffers, 0);
    }

    #[test]
    fn test_unbind_compute_storage_buffers() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::with_dynamic_storage_limit(8),
        );
        bind_test_channel(&mut cache, 1);
        if let Some(cs) = cache.current_channel_state_mut() {
            cs.enabled_compute_storage_buffers = 0xFF;
            cs.written_compute_storage_buffers = 0x0F;
            cs.total_compute_storage_buffers = 8;
        }
        cache.unbind_compute_storage_buffers();
        let cs = cache.current_channel_state().unwrap();
        assert_eq!(cs.enabled_compute_storage_buffers, 0);
        assert_eq!(cs.written_compute_storage_buffers, 0);
        assert_eq!(cs.total_compute_storage_buffers, 0);
    }

    #[test]
    fn dynamic_storage_buffer_limit_rejects_new_graphics_and_compute_bindings() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::with_dynamic_storage_limit(1),
        );
        bind_test_channel(&mut cache, 1);
        {
            let cs = cache.current_channel_state_mut().unwrap();
            cs.total_graphics_storage_buffers = 1;
            cs.total_compute_storage_buffers = 1;
        }

        assert!(!cache.bind_graphics_storage_buffer(0, 0, 0, 0, false));
        cache.bind_compute_storage_buffer(0, 0, 0, false);

        let cs = cache.current_channel_state().unwrap();
        assert_eq!(cs.enabled_storage_buffers[0], 0);
        assert_eq!(cs.written_storage_buffers[0], 0);
        assert_eq!(cs.total_graphics_storage_buffers, 1);
        assert_eq!(cs.enabled_compute_storage_buffers, 0);
        assert_eq!(cs.written_compute_storage_buffers, 0);
        assert_eq!(cs.total_compute_storage_buffers, 1);
    }

    #[test]
    fn test_range_subtract() {
        let mut rs = RangeSet::new();
        rs.add(0, 100);
        rs.subtract(20, 30);
        // Should split into [0, 20) and [50, 100).
        let mut result = Vec::new();
        rs.for_each(|s, e| result.push((s, e)));
        assert_eq!(result, vec![(0, 20), (50, 100)]);
    }

    #[test]
    fn test_range_subtract_full_removal() {
        let mut rs = RangeSet::new();
        rs.add(10, 40);
        rs.subtract(0, 100);
        assert!(rs.empty());
    }

    #[test]
    fn test_is_region_registered_empty() {
        let tracker = DummyTracker;
        let cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        assert!(!cache.is_region_registered(0x1000, 0x100));
    }

    #[test]
    fn test_find_buffer_null_addr() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        let id = cache.find_buffer(0, 0x100);
        assert_eq!(id, NULL_BUFFER_ID);
    }

    #[test]
    fn test_create_and_find_buffer() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        let addr = 0x0001_0000u64;
        let size = 0x1000u32;
        let id1 = cache.find_buffer(addr, size);
        // Finding again should return the same buffer.
        let id2 = cache.find_buffer(addr, size);
        assert_eq!(id1, id2);
        assert_ne!(id1, NULL_BUFFER_ID);
        assert_eq!(cache.slot_buffers[id1].raw_handle(), 0);
    }

    fn test_storage_binding(next_qword: u64, cbuf_index: u32) -> Binding {
        let tracker = DummyTracker;
        let cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        let ssbo_addr = 0x8000;
        let gpu_addr = 0x1234u64;
        cache.storage_buffer_binding_with_gpu_reader(
            ssbo_addr,
            cbuf_index,
            true,
            Some,
            |_| 0x1000,
            |address, output| {
                let value = if address == ssbo_addr {
                    gpu_addr
                } else if address == ssbo_addr + 8 {
                    next_qword
                } else {
                    return false;
                };
                output.copy_from_slice(&value.to_le_bytes()[..output.len()]);
                true
            },
        )
    }

    #[test]
    fn storage_buffer_packed_size_applies_to_every_cbuf_index() {
        let binding = test_storage_binding(0x80, 9);
        assert_eq!(binding.device_addr, 0x1200);
        assert_eq!(binding.size, 0x34 + 0x80);
    }

    #[test]
    fn storage_buffer_rejects_packed_size_with_nonzero_high_qword() {
        let binding = test_storage_binding((1u64 << 32) | 0x80, 0);
        assert_eq!(binding.device_addr, 0x1200);
        assert_eq!(binding.size, 0x34 + 0x1000);
    }

    #[test]
    fn storage_buffer_rejects_packed_size_larger_than_mapping() {
        let binding = test_storage_binding(0x2000, 0);
        assert_eq!(binding.device_addr, 0x1200);
        assert_eq!(binding.size, 0x34 + 0x1000);
    }

    #[test]
    fn test_inline_memory_unregistered() {
        let tracker = DummyTracker;
        let mut cache = BufferCache::<TestParams, DummyTracker>::new(
            &tracker,
            TestBufferCacheRuntime::default(),
        );
        // Unregistered region should return false.
        let result = cache.inline_memory(0x5000, 0x10, &[0u8; 16]);
        assert!(!result);
    }
}
