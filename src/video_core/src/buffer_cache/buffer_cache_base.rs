// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `video_core/buffer_cache/buffer_cache_base.h`
//!
//! Declares the types, constants, and structures used by the buffer cache.
//! The `BufferCache<P>` template is split between this file (data structures
//! and type definitions) and `buffer_cache.rs` (method implementations).

use common::slot_vector::SlotId;
use common::slot_vector::SlotVector;
use common::types::VAddr;
use smallvec::SmallVec;
use std::ptr::NonNull;

use super::buffer_base::BufferBase;
use crate::control::channel_state::ChannelState;
use crate::control::channel_state_cache::{ChannelCacheAccessor, ChannelInfo, FromChannelState};
use crate::engines::maxwell_3d::{IndexFormat, PrimitiveTopology};
use crate::surface::PixelFormat;

// ---------------------------------------------------------------------------
// Re-export BufferId
// ---------------------------------------------------------------------------

/// Identifier for a slot in the buffer cache's `SlotVector`.
pub type BufferId = SlotId;

// ---------------------------------------------------------------------------
// Constants — match upstream buffer_cache_base.h
// ---------------------------------------------------------------------------

/// Number of vertex buffer binding slots.
///
/// Upstream: 32 on non-Apple, 16 on Apple.
#[cfg(target_vendor = "apple")]
pub const NUM_VERTEX_BUFFERS: u32 = 16;
#[cfg(not(target_vendor = "apple"))]
pub const NUM_VERTEX_BUFFERS: u32 = 32;

/// Number of transform feedback buffer slots.
pub const NUM_TRANSFORM_FEEDBACK_BUFFERS: u32 = 4;

/// Number of uniform buffers per graphics stage.
pub const NUM_GRAPHICS_UNIFORM_BUFFERS: u32 = 18;

/// Number of uniform buffers for compute.
pub const NUM_COMPUTE_UNIFORM_BUFFERS: u32 = 8;

/// Number of storage buffer slots.
pub const NUM_STORAGE_BUFFERS: u32 = 16;

/// Number of texture buffer slots.
pub const NUM_TEXTURE_BUFFERS: u32 = 32;

/// Number of shader stages (vertex, tess_ctrl, tess_eval, geometry, fragment).
pub const NUM_STAGES: u32 = 5;

/// Uniform buffer sizes per stage (graphics).
pub type UniformBufferSizes = [[u32; NUM_GRAPHICS_UNIFORM_BUFFERS as usize]; NUM_STAGES as usize];

/// Uniform buffer sizes for compute.
pub type ComputeUniformBufferSizes = [u32; NUM_COMPUTE_UNIFORM_BUFFERS as usize];

// ---------------------------------------------------------------------------
// ObtainBuffer enums
// ---------------------------------------------------------------------------

/// Synchronization mode when obtaining a buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ObtainBufferSynchronize {
    NoSynchronize = 0,
    FullSynchronize = 1,
    SynchronizeNoDirty = 2,
}

/// Post-obtain operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ObtainBufferOperation {
    DoNothing = 0,
    MarkAsWritten = 1,
    DiscardWrite = 2,
    MarkQuery = 3,
}

// ---------------------------------------------------------------------------
// Null buffer sentinel
// ---------------------------------------------------------------------------

/// Sentinel `BufferId` representing the null buffer (slot 0).
pub const NULL_BUFFER_ID: BufferId = SlotId { index: 0 };

/// Default size threshold below which uniform buffer skip-cache is used (4 KiB).
pub const DEFAULT_SKIP_CACHE_SIZE: u32 = 4 * 1024;

// ---------------------------------------------------------------------------
// Binding structs
// ---------------------------------------------------------------------------

/// A binding from device address to buffer slot.
#[derive(Debug, Clone, Copy)]
pub struct Binding {
    /// Device address of the binding.
    pub device_addr: VAddr,
    /// Size of the binding in bytes.
    pub size: u32,
    /// Buffer slot that backs this binding.
    pub buffer_id: BufferId,
}

impl Default for Binding {
    fn default() -> Self {
        NULL_BINDING
    }
}

/// A texture buffer binding, which extends `Binding` with a pixel format.
#[derive(Debug, Clone, Copy)]
pub struct TextureBufferBinding {
    /// Device address of the binding.
    pub device_addr: VAddr,
    /// Size of the binding in bytes.
    pub size: u32,
    /// Buffer slot that backs this binding.
    pub buffer_id: BufferId,
    /// Pixel format of the texture buffer view.
    pub format: PixelFormat,
}

impl Default for TextureBufferBinding {
    fn default() -> Self {
        Self {
            device_addr: 0,
            size: 0,
            buffer_id: NULL_BUFFER_ID,
            format: PixelFormat::Invalid,
        }
    }
}

/// Sentinel null binding.
pub const NULL_BINDING: Binding = Binding {
    device_addr: 0,
    size: 0,
    buffer_id: NULL_BUFFER_ID,
};

// ---------------------------------------------------------------------------
// HostBindings
// ---------------------------------------------------------------------------

/// Collected host-side vertex buffer bindings ready for the backend.
///
/// Corresponds to the C++ `HostBindings<Buffer>` template struct.
pub struct HostBindings {
    /// Indices of bound buffers (backend-specific handles obtained separately).
    pub buffer_ids: SmallVec<[BufferId; NUM_VERTEX_BUFFERS as usize]>,
    /// Offsets within each buffer.
    pub offsets: SmallVec<[u64; NUM_VERTEX_BUFFERS as usize]>,
    /// Sizes of each binding.
    pub sizes: SmallVec<[u64; NUM_VERTEX_BUFFERS as usize]>,
    /// Strides for vertex buffers.
    pub strides: SmallVec<[u64; NUM_VERTEX_BUFFERS as usize]>,
    /// Minimum bound vertex buffer index.
    pub min_index: u32,
    /// Maximum bound vertex buffer index.
    pub max_index: u32,
}

impl Default for HostBindings {
    fn default() -> Self {
        Self {
            buffer_ids: SmallVec::new(),
            offsets: SmallVec::new(),
            sizes: SmallVec::new(),
            strides: SmallVec::new(),
            min_index: NUM_VERTEX_BUFFERS,
            max_index: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// BufferCacheChannelInfo
// ---------------------------------------------------------------------------

/// Per-channel state for the buffer cache.
///
/// Corresponds to the C++ `BufferCacheChannelInfo` class.
pub struct BufferCacheChannelInfo {
    /// Upstream `BufferCacheChannelInfo : public ChannelInfo`.
    pub channel_info: ChannelInfo,

    // -- Graphics bindings --
    pub index_buffer: Binding,
    pub vertex_buffers: [Binding; NUM_VERTEX_BUFFERS as usize],
    pub uniform_buffers: [[Binding; NUM_GRAPHICS_UNIFORM_BUFFERS as usize]; NUM_STAGES as usize],
    pub storage_buffers: [[Binding; NUM_STORAGE_BUFFERS as usize]; NUM_STAGES as usize],
    pub texture_buffers:
        [[TextureBufferBinding; NUM_TEXTURE_BUFFERS as usize]; NUM_STAGES as usize],
    pub transform_feedback_buffers: [Binding; NUM_TRANSFORM_FEEDBACK_BUFFERS as usize],
    pub count_buffer_binding: Binding,
    pub indirect_buffer_binding: Binding,

    // -- Compute bindings --
    pub compute_uniform_buffers: [Binding; NUM_COMPUTE_UNIFORM_BUFFERS as usize],
    pub compute_storage_buffers: [Binding; NUM_STORAGE_BUFFERS as usize],
    pub compute_texture_buffers: [TextureBufferBinding; NUM_TEXTURE_BUFFERS as usize],

    // -- Enabled masks --
    pub enabled_uniform_buffer_masks: [u32; NUM_STAGES as usize],
    pub enabled_compute_uniform_buffer_mask: u32,

    // -- Uniform buffer sizes (non-owning pointers into stable pipeline state) --
    pub uniform_buffer_sizes: Option<NonNull<UniformBufferSizes>>,
    pub compute_uniform_buffer_sizes: Option<NonNull<ComputeUniformBufferSizes>>,

    // -- Storage buffer masks --
    pub enabled_storage_buffers: [u32; NUM_STAGES as usize],
    pub written_storage_buffers: [u32; NUM_STAGES as usize],
    pub enabled_compute_storage_buffers: u32,
    pub written_compute_storage_buffers: u32,
    pub total_graphics_storage_buffers: u32,
    pub total_compute_storage_buffers: u32,

    // -- Texture buffer masks --
    pub enabled_texture_buffers: [u32; NUM_STAGES as usize],
    pub written_texture_buffers: [u32; NUM_STAGES as usize],
    pub image_texture_buffers: [u32; NUM_STAGES as usize],
    pub enabled_compute_texture_buffers: u32,
    pub written_compute_texture_buffers: u32,
    pub image_compute_texture_buffers: u32,

    // -- Uniform cache statistics --
    pub uniform_cache_hits: [u32; NUM_GRAPHICS_UNIFORM_BUFFERS as usize],
    pub uniform_cache_shots: [u32; NUM_GRAPHICS_UNIFORM_BUFFERS as usize],

    /// Size threshold for uniform buffer skip-cache.
    pub uniform_buffer_skip_cache_size: u32,

    /// Whether any buffers were deleted this frame.
    pub has_deleted_buffers: bool,

    // -- Dirty / fast-bound tracking --
    pub dirty_uniform_buffers: [u32; NUM_STAGES as usize],
    pub fast_bound_uniform_buffers: [u32; NUM_STAGES as usize],
    pub uniform_buffer_binding_sizes:
        [[u32; NUM_GRAPHICS_UNIFORM_BUFFERS as usize]; NUM_STAGES as usize],
}

#[cfg(test)]
impl Default for BufferCacheChannelInfo {
    fn default() -> Self {
        Self::from_channel_info(ChannelInfo {
            maxwell3d: 0,
            kepler_compute: 0,
            gpu_memory_index: 0,
            gpu_memory: None,
            program_id: 0,
        })
    }
}

impl BufferCacheChannelInfo {
    fn from_channel_info(channel_info: ChannelInfo) -> Self {
        Self {
            channel_info,
            index_buffer: Binding::default(),
            vertex_buffers: [Binding::default(); NUM_VERTEX_BUFFERS as usize],
            uniform_buffers: [[Binding::default(); NUM_GRAPHICS_UNIFORM_BUFFERS as usize];
                NUM_STAGES as usize],
            storage_buffers: [[Binding::default(); NUM_STORAGE_BUFFERS as usize];
                NUM_STAGES as usize],
            texture_buffers: [[TextureBufferBinding::default(); NUM_TEXTURE_BUFFERS as usize];
                NUM_STAGES as usize],
            transform_feedback_buffers: [Binding::default();
                NUM_TRANSFORM_FEEDBACK_BUFFERS as usize],
            count_buffer_binding: Binding::default(),
            indirect_buffer_binding: Binding::default(),

            compute_uniform_buffers: [Binding::default(); NUM_COMPUTE_UNIFORM_BUFFERS as usize],
            compute_storage_buffers: [Binding::default(); NUM_STORAGE_BUFFERS as usize],
            compute_texture_buffers: [TextureBufferBinding::default();
                NUM_TEXTURE_BUFFERS as usize],

            enabled_uniform_buffer_masks: [0; NUM_STAGES as usize],
            enabled_compute_uniform_buffer_mask: 0,

            uniform_buffer_sizes: None,
            compute_uniform_buffer_sizes: None,

            enabled_storage_buffers: [0; NUM_STAGES as usize],
            written_storage_buffers: [0; NUM_STAGES as usize],
            enabled_compute_storage_buffers: 0,
            written_compute_storage_buffers: 0,
            total_graphics_storage_buffers: 0,
            total_compute_storage_buffers: 0,

            enabled_texture_buffers: [0; NUM_STAGES as usize],
            written_texture_buffers: [0; NUM_STAGES as usize],
            image_texture_buffers: [0; NUM_STAGES as usize],
            enabled_compute_texture_buffers: 0,
            written_compute_texture_buffers: 0,
            image_compute_texture_buffers: 0,

            uniform_cache_hits: [0; NUM_GRAPHICS_UNIFORM_BUFFERS as usize],
            uniform_cache_shots: [0; NUM_GRAPHICS_UNIFORM_BUFFERS as usize],

            uniform_buffer_skip_cache_size: DEFAULT_SKIP_CACHE_SIZE,

            has_deleted_buffers: false,

            dirty_uniform_buffers: [0; NUM_STAGES as usize],
            fast_bound_uniform_buffers: [0; NUM_STAGES as usize],
            uniform_buffer_binding_sizes: [[0; NUM_GRAPHICS_UNIFORM_BUFFERS as usize];
                NUM_STAGES as usize],
        }
    }
}

impl FromChannelState for BufferCacheChannelInfo {
    fn from_channel_state(state: &ChannelState) -> Self {
        Self::from_channel_info(ChannelInfo::from_channel_state(state))
    }
}

impl ChannelCacheAccessor for BufferCacheChannelInfo {
    fn maxwell3d_ref(&self) -> usize {
        self.channel_info.maxwell3d
    }

    fn kepler_compute_ref(&self) -> usize {
        self.channel_info.kepler_compute
    }

    fn gpu_memory_ref(&self) -> usize {
        self.channel_info.gpu_memory_index
    }

    fn gpu_memory_arc(
        &self,
    ) -> Option<std::sync::Arc<parking_lot::Mutex<crate::memory_manager::MemoryManager>>> {
        self.channel_info
            .gpu_memory
            .as_ref()
            .map(std::sync::Arc::clone)
    }

    fn program_id_val(&self) -> u64 {
        self.channel_info.program_id
    }
}

// ---------------------------------------------------------------------------
// BufferCacheParams trait — the policy template parameter
// ---------------------------------------------------------------------------

/// Trait replacing the C++ `class P` template parameter.
///
/// Each rendering backend (OpenGL, Vulkan, Null) provides a concrete
/// implementation of this trait, supplying its buffer type, runtime,
/// and various capability flags.
pub trait BufferCacheParams {
    /// Backend runtime, matching `P::Runtime` upstream.
    type Runtime: BufferCacheRuntime<Buffer = Self::Buffer, AsyncBuffer = Self::AsyncBuffer>;
    /// Backend buffer, matching `P::Buffer` upstream.
    type Buffer: BufferCacheBuffer<Runtime = Self::Runtime>;
    /// Backend staging allocation, matching `P::Async_Buffer` upstream.
    type AsyncBuffer: BufferCacheAsyncBuffer;

    /// Whether this is the OpenGL backend.
    const IS_OPENGL: bool;
    /// Whether persistent uniform buffer bindings are supported.
    const HAS_PERSISTENT_UNIFORM_BUFFER_BINDINGS: bool;
    /// Whether all index formats and primitive topologies are natively supported.
    const HAS_FULL_INDEX_AND_PRIMITIVE_SUPPORT: bool;
    /// Whether uniform buffers must be bound by index.
    const NEEDS_BIND_UNIFORM_INDEX: bool;
    /// Whether storage buffers must be bound by index.
    const NEEDS_BIND_STORAGE_INDEX: bool;
    /// Whether memory-mapped uploads are used.
    const USE_MEMORY_MAPS: bool;
    /// Whether image buffer bindings are separate from texture buffer bindings.
    const SEPARATE_IMAGE_BUFFER_BINDINGS: bool;
    /// Whether memory maps are used for uploads.
    const USE_MEMORY_MAPS_FOR_UPLOADS: bool;
}

/// Backend buffer interface required by the common `BufferCache<P>` template.
///
/// The concrete OpenGL and Vulkan objects own their API handles and backend
/// state. `Deref<Target = BufferBase>` preserves the inheritance relationship
/// from the C++ port without moving backend state into the common base.
pub trait BufferCacheBuffer:
    std::ops::Deref<Target = BufferBase> + std::ops::DerefMut<Target = BufferBase> + Sized
{
    type Runtime: BufferCacheRuntime<Buffer = Self>;

    fn null(runtime: &mut Self::Runtime, params: super::buffer_base::NullBufferParams) -> Self;
    fn new(runtime: &mut Self::Runtime, cpu_addr: VAddr, size_bytes: u64) -> Self;

    fn immediate_upload(&self, offset: u64, data: &[u8]);
    fn immediate_download(&self, offset: u64, data: &mut [u8]);

    /// Backend API handle used by same-backend rasterizer helpers.
    fn raw_handle(&self) -> u64;

    /// Backend-specific usage tracking. OpenGL intentionally implements these
    /// as no-ops; Vulkan owns the range tracker on its concrete `Buffer`.
    fn mark_usage(&mut self, _offset: u64, _size: u64) {}
    fn is_region_used(&self, _offset: u64, _size: u64) -> bool {
        false
    }
    fn reset_usage_tracking(&mut self) {}
    fn last_usage_tick(&self) -> u64 {
        0
    }
}

/// Common accessors used by `BufferCache<P>` for the backend-specific staging
/// type selected by `P::Async_Buffer` upstream.
pub trait BufferCacheAsyncBuffer: Sized {
    fn offset(&self) -> u64;
    fn mapped_span(&self) -> &[u8];
    fn mapped_span_mut(&mut self) -> &mut [u8];

    #[cfg(test)]
    fn empty_for_test() -> Self;
}

// ---------------------------------------------------------------------------
// BufferCopy — copy descriptor
// ---------------------------------------------------------------------------

/// Describes a single copy operation within a buffer.
#[derive(Debug, Clone, Copy, Default)]
pub struct BufferCopy {
    /// Source offset within the source buffer.
    pub src_offset: u64,
    /// Destination offset within the destination buffer.
    pub dst_offset: u64,
    /// Number of bytes to copy.
    pub size: u64,
}

// ---------------------------------------------------------------------------
// OverlapResult (private to BufferCache, but declared here for parity)
// ---------------------------------------------------------------------------

/// Result of resolving overlapping buffers for a new allocation.
pub struct OverlapResult {
    /// Buffer IDs that overlap the requested range.
    pub ids: SmallVec<[BufferId; 16]>,
    /// Start of the merged range.
    pub begin: VAddr,
    /// End of the merged range.
    pub end: VAddr,
    /// Whether any overlapping buffer was a stream buffer.
    pub has_stream_leap: bool,
}

impl Default for OverlapResult {
    fn default() -> Self {
        Self {
            ids: SmallVec::new(),
            begin: 0,
            end: 0,
            has_stream_leap: false,
        }
    }
}

// ---------------------------------------------------------------------------
// StagingBufferRef — staging buffer allocation handle
// ---------------------------------------------------------------------------

/// A reference to a staging buffer allocation.
///
/// Upstream: `StagingBufferRef` (Vulkan) / `StagingBufferMap` (OpenGL).
/// This is a backend-agnostic handle returned by `BufferCacheRuntime::upload_staging_buffer`
/// and `BufferCacheRuntime::download_staging_buffer`.
pub struct StagingBufferRef {
    /// Opaque buffer handle (backend interprets this as a buffer ID for copy operations).
    pub buffer: BufferId,
    /// Backend-native buffer handle used by runtimes whose staging buffers do not
    /// live in the generic slot vector.
    pub gpu_handle: u64,
    /// Offset within the staging buffer.
    pub offset: u64,
    /// Index in the backend staging allocation pool.
    pub index: usize,
    mapped_ptr: *mut u8,
    mapped_size: usize,
    sync: *mut gl::types::GLsync,
    host_mapping: Option<Vec<u8>>,
}

impl StagingBufferRef {
    /// Host-backed fallback used by non-GL test runtimes.
    pub fn host(size: usize) -> Self {
        let mut host_mapping = vec![0u8; size];
        let mapped_ptr = host_mapping.as_mut_ptr();
        Self {
            buffer: BufferId::invalid(),
            gpu_handle: 0,
            offset: 0,
            index: usize::MAX,
            mapped_ptr,
            mapped_size: size,
            sync: std::ptr::null_mut(),
            host_mapping: Some(host_mapping),
        }
    }

    /// Backend-backed mapping. The caller owns the lifetime contract for
    /// `mapped_ptr` and `sync`, matching upstream `StagingBufferMap`.
    pub unsafe fn from_mapped_backend(
        buffer: BufferId,
        gpu_handle: u64,
        offset: u64,
        index: usize,
        mapped_ptr: *mut u8,
        mapped_size: usize,
        sync: *mut gl::types::GLsync,
    ) -> Self {
        Self {
            buffer,
            gpu_handle,
            offset,
            index,
            mapped_ptr,
            mapped_size,
            sync,
            host_mapping: None,
        }
    }

    pub fn mapped_span(&self) -> &[u8] {
        if let Some(host_mapping) = &self.host_mapping {
            return host_mapping;
        }
        if self.mapped_size == 0 {
            return &[];
        }
        assert!(
            !self.mapped_ptr.is_null(),
            "staging buffer has a non-zero mapped size but no mapped pointer"
        );
        unsafe { std::slice::from_raw_parts(self.mapped_ptr, self.mapped_size) }
    }

    pub fn mapped_span_mut(&mut self) -> &mut [u8] {
        if let Some(host_mapping) = &mut self.host_mapping {
            return host_mapping;
        }
        if self.mapped_size == 0 {
            return &mut [];
        }
        assert!(
            !self.mapped_ptr.is_null(),
            "staging buffer has a non-zero mapped size but no mapped pointer"
        );
        unsafe { std::slice::from_raw_parts_mut(self.mapped_ptr, self.mapped_size) }
    }
}

impl Drop for StagingBufferRef {
    fn drop(&mut self) {
        if self.sync.is_null() {
            return;
        }
        unsafe {
            // Matches OpenGL::OGLSync::Create: keep an existing fence alive.
            // Replacing it would allow the staging allocation to be reused
            // before the commands protected by the original fence complete.
            if (*self.sync).is_null() {
                *self.sync = gl::FenceSync(gl::SYNC_GPU_COMMANDS_COMPLETE, 0);
            }
        }
    }
}

impl BufferCacheAsyncBuffer for StagingBufferRef {
    fn offset(&self) -> u64 {
        self.offset
    }

    fn mapped_span(&self) -> &[u8] {
        StagingBufferRef::mapped_span(self)
    }

    fn mapped_span_mut(&mut self) -> &mut [u8] {
        StagingBufferRef::mapped_span_mut(self)
    }

    #[cfg(test)]
    fn empty_for_test() -> Self {
        StagingBufferRef::host(0)
    }
}

// ---------------------------------------------------------------------------
// BufferCacheRuntime trait — the Runtime template parameter interface
// ---------------------------------------------------------------------------

/// Trait replacing the C++ `Runtime` type parameter used by `BufferCache<P>`.
///
/// Each rendering backend (OpenGL, Vulkan, Null) provides a concrete
/// implementation of this trait. The upstream C++ uses duck-typing via the
/// template parameter `P::Runtime`; in Rust we formalize the interface as a trait.
///
/// Method signatures are derived from the union of methods called on `runtime`
/// in upstream `buffer_cache.h` (template method implementations).
pub trait BufferCacheRuntime {
    type Buffer: BufferCacheBuffer<Runtime = Self>;
    type AsyncBuffer: BufferCacheAsyncBuffer;

    // -- Frame lifecycle --

    /// Called once per frame to allow the runtime to reclaim resources.
    ///
    /// Upstream: `Runtime::TickFrame(SlotVector<Buffer>&)`
    fn tick_frame(&mut self, slot_buffers: &mut SlotVector<Self::Buffer>);

    /// Whether the runtime can report actual device memory usage.
    ///
    /// Upstream: `Runtime::CanReportMemoryUsage()`
    fn can_report_memory_usage(&self) -> bool;

    /// Return the amount of device-local memory available.
    ///
    /// Upstream: `Runtime::GetDeviceLocalMemory()`
    fn get_device_local_memory(&self) -> u64;

    /// Return current device memory usage in bytes.
    ///
    /// Upstream: `Runtime::GetDeviceMemoryUsage()`
    fn get_device_memory_usage(&self) -> u64;

    /// Return the alignment requirement for storage buffer offsets.
    ///
    /// Upstream: `Runtime::GetStorageBufferAlignment()`
    fn get_storage_buffer_alignment(&self) -> u32;

    /// Wait for all pending GPU operations to complete.
    ///
    /// Upstream: `Runtime::Finish()`
    fn finish(&mut self);

    /// Tick assigned to the current command submission.
    fn current_tick(&self) -> u64 {
        0
    }

    /// Last tick completed by the host GPU.
    fn known_gpu_tick(&self) -> u64 {
        0
    }

    /// Wait until the host GPU reaches `tick`.
    fn wait(&mut self, _tick: u64) {}

    // -- Staging buffers --

    /// Allocate a staging buffer for CPU→GPU upload.
    ///
    /// Upstream: `Runtime::UploadStagingBuffer(size)`
    fn upload_staging_buffer(&mut self, size: u64) -> Self::AsyncBuffer;

    /// Allocate a staging buffer for GPU→CPU download.
    ///
    /// Upstream: `Runtime::DownloadStagingBuffer(size, deferred)`
    fn download_staging_buffer(&mut self, size: u64, deferred: bool) -> Self::AsyncBuffer;

    /// Free a deferred staging buffer.
    ///
    /// Upstream: `Runtime::FreeDeferredStagingBuffer(ref)`
    fn free_deferred_staging_buffer(&mut self, buffer: &mut Self::AsyncBuffer);

    /// Whether uploads to `buffer` with given `copies` can be reordered.
    ///
    /// Upstream: `Runtime::CanReorderUpload(buffer, copies)`
    fn can_reorder_upload(&self, buffer: &Self::Buffer, copies: &[BufferCopy]) -> bool;

    // -- Copy / Clear --

    /// Insert a barrier before a batch of copy operations.
    ///
    /// Upstream: `Runtime::PreCopyBarrier()`
    fn pre_copy_barrier(&mut self);

    /// Insert a barrier after a batch of copy operations.
    ///
    /// Upstream: `Runtime::PostCopyBarrier()`
    fn post_copy_barrier(&mut self);

    /// Copy data between two buffers.
    ///
    /// Upstream: `Runtime::CopyBuffer(dst, src, copies, barrier, can_reorder_upload)`
    fn copy_buffer(
        &mut self,
        dst_buffer: &Self::Buffer,
        src_buffer: &Self::Buffer,
        copies: &[BufferCopy],
        barrier: bool,
        can_reorder_upload: bool,
    );

    /// Copy from a backend staging allocation into a cached buffer. This is a
    /// named Rust counterpart of the C++ `CopyBuffer(Buffer&, APIHandle, ...)`
    /// overload selected by the template.
    fn copy_buffer_from_staging(
        &mut self,
        dst_buffer: &Self::Buffer,
        src_buffer: &Self::AsyncBuffer,
        copies: &[BufferCopy],
        barrier: bool,
        can_reorder_upload: bool,
    );

    /// Copy from a cached buffer into a backend staging allocation.
    fn copy_buffer_to_staging(
        &mut self,
        dst_buffer: &Self::AsyncBuffer,
        src_buffer: &Self::Buffer,
        copies: &[BufferCopy],
        barrier: bool,
    );

    /// Clear a buffer region to a uniform value.
    ///
    /// Upstream: `Runtime::ClearBuffer(buffer, offset, size, value)`
    fn clear_buffer(&mut self, buffer: &Self::Buffer, offset: u32, size: u64, value: u32);

    // -- Index buffer binding --

    /// Bind an index buffer for draw calls.
    ///
    /// Upstream: `Runtime::BindIndexBuffer(buffer, offset, size)`.
    /// The runtime receives the backend buffer object, matching upstream.
    fn bind_index_buffer(
        &mut self,
        topology: PrimitiveTopology,
        index_format: IndexFormat,
        base_vertex: u32,
        num_indices: u32,
        buffer: &mut Self::Buffer,
        offset: u32,
        size: u32,
    );

    /// Bind the generated index buffer used to emulate non-indexed quads.
    ///
    /// Upstream: `Runtime::BindQuadIndexBuffer(topology, first, count)` when
    /// `HAS_FULL_INDEX_AND_PRIMITIVE_SUPPORT` is false.
    fn bind_quad_index_buffer(&mut self, _topology: PrimitiveTopology, _first: u32, _count: u32) {
        unreachable!("quad index emulation is unavailable for this backend")
    }

    /// Return the backend index offset consumed by the rasterizer draw call.
    ///
    /// Upstream OpenGL exposes this as `BufferCacheRuntime::IndexOffset()`.
    fn index_offset(&self) -> usize {
        0
    }

    // -- Vertex buffer binding --

    /// Bind one vertex buffer.
    ///
    /// Upstream: `Runtime::BindVertexBuffer(index, buffer, offset, size, stride)`
    /// through `BufferCache<P>::BindHostVertexBuffer`.
    fn bind_vertex_buffer(
        &mut self,
        index: u32,
        buffer: &mut Self::Buffer,
        offset: u32,
        size: u32,
        stride: u32,
    );

    /// Bind vertex buffers collected in `HostBindings`.
    ///
    /// Upstream: `Runtime::BindVertexBuffers(host_bindings)`.
    /// `buffers` provides the backend buffer objects referenced by
    /// `bindings.buffer_ids`, matching upstream's `HostBindings<Buffer>`.
    fn bind_vertex_buffers(
        &mut self,
        bindings: &HostBindings,
        buffers: &mut SlotVector<Self::Buffer>,
    );

    // -- Uniform buffer binding (graphics) --

    /// Bind a graphics-stage uniform buffer.
    ///
    /// Upstream (OpenGL): `Runtime::BindUniformBuffer(stage, binding_index, buffer, offset, size)`
    /// Upstream (Vulkan): `Runtime::BindUniformBuffer(buffer, offset, size)`
    fn bind_uniform_buffer(
        &mut self,
        stage: usize,
        binding_index: u32,
        buffer: &mut Self::Buffer,
        offset: u32,
        size: u32,
    );

    /// Set per-stage base uniform binding points.
    ///
    /// Upstream OpenGL stores this on `BufferCacheRuntime` and adds the base
    /// to each stage-local uniform binding index.
    fn set_base_uniform_bindings(&mut self, _bindings: &[u32; NUM_STAGES as usize]) {}

    /// Set per-stage base storage binding points.
    ///
    /// Upstream OpenGL stores this on `BufferCacheRuntime` and adds the base
    /// to each stage-local storage binding index.
    fn set_base_storage_bindings(&mut self, _bindings: &[u32; NUM_STAGES as usize]) {}

    /// Set the output arrays used by OpenGL texture/image-buffer binding.
    ///
    /// Upstream: `BufferCacheRuntime::SetImagePointers(GLuint*, GLuint*)`.
    /// `BindHostStageBuffers` writes TBO/image-buffer texture names through
    /// these pointers before the pipeline bulk-calls `glBindTextures` /
    /// `glBindImageTextures`.
    fn set_image_pointers(&mut self, _texture_handles: *mut u32, _image_handles: *mut u32) {}

    /// Select GL SSBO binding mode for graphics/compute storage buffers.
    ///
    /// Upstream OpenGL uses real `GL_SHADER_STORAGE_BUFFER` bindings for GLSL
    /// and bindless program-local parameters for GLASM when necessary.
    fn set_enable_storage_buffers(&mut self, _enable: bool) {}

    /// Vulkan driver workaround from `BufferCacheRuntime` upstream. Backends
    /// without the named methods take the `if constexpr`-absent path.
    fn should_limit_dynamic_storage_buffers(&self) -> bool {
        false
    }

    fn max_dynamic_storage_buffers(&self) -> u32 {
        u32::MAX
    }

    // -- Storage buffer binding (graphics) --

    /// Bind a graphics-stage storage buffer.
    ///
    /// Upstream (OpenGL): `Runtime::BindStorageBuffer(stage, binding_index, buffer, offset, size, is_written)`
    /// Upstream (Vulkan): `Runtime::BindStorageBuffer(buffer, offset, size, is_written)`
    fn bind_storage_buffer(
        &mut self,
        stage: usize,
        binding_index: u32,
        buffer: &mut Self::Buffer,
        offset: u32,
        size: u32,
        is_written: bool,
    );

    // -- Texture / Image buffer binding --

    /// Bind a texture buffer view.
    ///
    /// Upstream: `Runtime::BindTextureBuffer(buffer, offset, size, format)`
    fn bind_texture_buffer(
        &mut self,
        buffer: &mut Self::Buffer,
        offset: u32,
        size: u32,
        format: PixelFormat,
    );

    /// Bind an image buffer view (separate from texture on some backends).
    ///
    /// Upstream: `Runtime::BindImageBuffer(buffer, offset, size, format)`
    fn bind_image_buffer(
        &mut self,
        buffer: &mut Self::Buffer,
        offset: u32,
        size: u32,
        format: PixelFormat,
    );

    // -- Transform feedback --

    /// Bind transform feedback buffers.
    ///
    /// Upstream: `Runtime::BindTransformFeedbackBuffers(host_bindings)`
    fn bind_transform_feedback_buffers(
        &mut self,
        bindings: &HostBindings,
        buffers: &mut SlotVector<Self::Buffer>,
    );

    /// Create and bind the backend transform-feedback object associated with
    /// a guest address.
    ///
    /// Upstream OpenGL: `BufferCacheRuntime::BindTransformFeedbackObject`.
    fn bind_transform_feedback_object(&mut self, _tfb_object_addr: u64) {}

    /// Return the backend transform-feedback object associated with a guest
    /// address.
    ///
    /// Upstream OpenGL: `BufferCacheRuntime::GetTransformFeedbackObject`.
    fn get_transform_feedback_object(&mut self, _tfb_object_addr: u64) -> u32 {
        0
    }

    // -- Compute buffer binding --

    /// Bind a compute-stage uniform buffer.
    ///
    /// Upstream: `Runtime::BindComputeUniformBuffer(binding_index, buffer, offset, size)`
    fn bind_compute_uniform_buffer(
        &mut self,
        binding_index: u32,
        buffer: &mut Self::Buffer,
        offset: u32,
        size: u32,
    );

    /// Bind a compute-stage storage buffer.
    ///
    /// Upstream: `Runtime::BindComputeStorageBuffer(binding_index, buffer, offset, size, is_written)`
    fn bind_compute_storage_buffer(
        &mut self,
        binding_index: u32,
        buffer: &mut Self::Buffer,
        offset: u32,
        size: u32,
        is_written: bool,
    );

    // -- OpenGL-specific fast uniform buffer path --

    /// Whether the runtime supports `glBufferSubData`-like fast path.
    ///
    /// Upstream (OpenGL): `Runtime::HasFastBufferSubData()`
    fn has_fast_buffer_sub_data(&self) -> bool {
        false
    }

    /// Whether non-zero uniform buffer offsets are supported.
    ///
    /// Upstream (OpenGL): `Runtime::SupportsNonZeroUniformOffset()`
    fn supports_non_zero_uniform_offset(&self) -> bool {
        true
    }

    /// Required alignment for host uniform-buffer offsets.
    ///
    /// Upstream (Vulkan): `Runtime::GetUniformBufferAlignment()`.
    fn uniform_buffer_alignment(&self) -> u32 {
        1
    }

    /// Bind a fast uniform buffer (OpenGL assembly shader path).
    ///
    /// Upstream (OpenGL): `Runtime::BindFastUniformBuffer(stage, binding_index, size)`
    fn bind_fast_uniform_buffer(&mut self, _stage: usize, _binding_index: u32, _size: u32) {}

    /// Push data into a fast uniform buffer (OpenGL path).
    ///
    /// Upstream (OpenGL): `Runtime::PushFastUniformBuffer(stage, binding_index, data)`
    fn push_fast_uniform_buffer(&mut self, _stage: usize, _binding_index: u32, _data: &[u8]) {}

    /// Bind a mapped uniform buffer and let the caller write uniform data into it.
    ///
    /// Upstream (OpenGL): `Runtime::BindMappedUniformBuffer(stage, binding_index, size)`
    fn with_mapped_uniform_buffer(
        &mut self,
        _stage: usize,
        _binding_index: u32,
        _size: u32,
        _write: &mut dyn FnMut(&mut [u8]),
    ) -> bool {
        false
    }
}

// Test-only concrete specialization of the upstream BufferCache template. It
// keeps backend state on the concrete buffer, so unit tests exercise the same
// ownership shape as the OpenGL and Vulkan specializations without requiring a
// graphics context.
#[cfg(test)]
pub(crate) struct TestBuffer {
    base: BufferBase,
    storage: parking_lot::Mutex<Vec<u8>>,
    tracker: super::usage_tracker::UsageTracker,
    last_usage_tick: u64,
}

#[cfg(test)]
impl std::ops::Deref for TestBuffer {
    type Target = BufferBase;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

#[cfg(test)]
impl std::ops::DerefMut for TestBuffer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

#[cfg(test)]
impl BufferCacheBuffer for TestBuffer {
    type Runtime = TestBufferCacheRuntime;

    fn null(_runtime: &mut Self::Runtime, params: super::buffer_base::NullBufferParams) -> Self {
        Self {
            base: BufferBase::null(params),
            storage: parking_lot::Mutex::new(Vec::new()),
            tracker: super::usage_tracker::UsageTracker::new(4096),
            last_usage_tick: 0,
        }
    }

    fn new(_runtime: &mut Self::Runtime, cpu_addr: VAddr, size_bytes: u64) -> Self {
        Self {
            base: BufferBase::new(cpu_addr, size_bytes),
            storage: parking_lot::Mutex::new(vec![0; size_bytes as usize]),
            tracker: super::usage_tracker::UsageTracker::new(size_bytes as usize),
            last_usage_tick: 0,
        }
    }

    fn immediate_upload(&self, offset: u64, data: &[u8]) {
        let offset = offset as usize;
        self.storage.lock()[offset..offset + data.len()].copy_from_slice(data);
    }

    fn immediate_download(&self, offset: u64, data: &mut [u8]) {
        let offset = offset as usize;
        data.copy_from_slice(&self.storage.lock()[offset..offset + data.len()]);
    }

    fn raw_handle(&self) -> u64 {
        0
    }

    fn mark_usage(&mut self, offset: u64, size: u64) {
        self.tracker.track(offset, size);
        self.last_usage_tick = self.last_usage_tick.wrapping_add(1);
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

#[cfg(test)]
#[derive(Default)]
pub(crate) struct TestBufferCacheRuntime {
    limit_dynamic_storage_buffers: bool,
    max_dynamic_storage_buffers: u32,
    can_report_memory_usage: bool,
    device_local_memory: u64,
}

#[cfg(test)]
impl TestBufferCacheRuntime {
    pub(crate) fn with_dynamic_storage_limit(max: u32) -> Self {
        Self {
            limit_dynamic_storage_buffers: true,
            max_dynamic_storage_buffers: max,
            ..Self::default()
        }
    }

    pub(crate) fn with_device_local_memory(device_local_memory: u64) -> Self {
        Self {
            can_report_memory_usage: true,
            device_local_memory,
            ..Self::default()
        }
    }
}

#[cfg(test)]
impl TestBufferCacheRuntime {
    fn copy_cached_buffers(dst: &TestBuffer, src: &TestBuffer, copies: &[BufferCopy]) {
        let src_storage = src.storage.lock();
        let mut dst_storage = dst.storage.lock();
        for copy in copies {
            let src_start = copy.src_offset as usize;
            let dst_start = copy.dst_offset as usize;
            let size = copy.size as usize;
            dst_storage[dst_start..dst_start + size]
                .copy_from_slice(&src_storage[src_start..src_start + size]);
        }
    }
}

#[cfg(test)]
impl BufferCacheRuntime for TestBufferCacheRuntime {
    type Buffer = TestBuffer;
    type AsyncBuffer = StagingBufferRef;

    fn tick_frame(&mut self, _slot_buffers: &mut SlotVector<Self::Buffer>) {}

    fn can_report_memory_usage(&self) -> bool {
        self.can_report_memory_usage
    }

    fn get_device_local_memory(&self) -> u64 {
        self.device_local_memory
    }

    fn get_device_memory_usage(&self) -> u64 {
        0
    }

    fn get_storage_buffer_alignment(&self) -> u32 {
        0x100
    }

    fn should_limit_dynamic_storage_buffers(&self) -> bool {
        self.limit_dynamic_storage_buffers
    }

    fn max_dynamic_storage_buffers(&self) -> u32 {
        if self.limit_dynamic_storage_buffers {
            self.max_dynamic_storage_buffers
        } else {
            u32::MAX
        }
    }

    fn finish(&mut self) {}

    fn upload_staging_buffer(&mut self, size: u64) -> Self::AsyncBuffer {
        StagingBufferRef::host(size as usize)
    }

    fn download_staging_buffer(&mut self, size: u64, _deferred: bool) -> Self::AsyncBuffer {
        StagingBufferRef::host(size as usize)
    }

    fn free_deferred_staging_buffer(&mut self, _buffer: &mut Self::AsyncBuffer) {}

    fn can_reorder_upload(&self, _buffer: &Self::Buffer, _copies: &[BufferCopy]) -> bool {
        false
    }

    fn pre_copy_barrier(&mut self) {}

    fn post_copy_barrier(&mut self) {}

    fn copy_buffer(
        &mut self,
        dst_buffer: &Self::Buffer,
        src_buffer: &Self::Buffer,
        copies: &[BufferCopy],
        _barrier: bool,
        _can_reorder_upload: bool,
    ) {
        Self::copy_cached_buffers(dst_buffer, src_buffer, copies);
    }

    fn copy_buffer_from_staging(
        &mut self,
        dst_buffer: &Self::Buffer,
        src_buffer: &Self::AsyncBuffer,
        copies: &[BufferCopy],
        _barrier: bool,
        _can_reorder_upload: bool,
    ) {
        let src_storage = src_buffer.mapped_span();
        let mut dst_storage = dst_buffer.storage.lock();
        for copy in copies {
            let src_start = copy.src_offset as usize;
            let dst_start = copy.dst_offset as usize;
            let size = copy.size as usize;
            dst_storage[dst_start..dst_start + size]
                .copy_from_slice(&src_storage[src_start..src_start + size]);
        }
    }

    fn copy_buffer_to_staging(
        &mut self,
        _dst_buffer: &Self::AsyncBuffer,
        _src_buffer: &Self::Buffer,
        _copies: &[BufferCopy],
        _barrier: bool,
    ) {
    }

    fn clear_buffer(&mut self, buffer: &Self::Buffer, offset: u32, size: u64, value: u32) {
        let pattern = value.to_ne_bytes();
        let mut storage = buffer.storage.lock();
        let start = offset as usize;
        let end = start + size as usize;
        for (index, byte) in storage[start..end].iter_mut().enumerate() {
            *byte = pattern[index & 3];
        }
    }

    fn bind_index_buffer(
        &mut self,
        _topology: PrimitiveTopology,
        _index_format: IndexFormat,
        _base_vertex: u32,
        _num_indices: u32,
        _buffer: &mut Self::Buffer,
        _offset: u32,
        _size: u32,
    ) {
    }

    fn bind_vertex_buffer(
        &mut self,
        _index: u32,
        _buffer: &mut Self::Buffer,
        _offset: u32,
        _size: u32,
        _stride: u32,
    ) {
    }

    fn bind_vertex_buffers(
        &mut self,
        _bindings: &HostBindings,
        _buffers: &mut SlotVector<Self::Buffer>,
    ) {
    }

    fn bind_uniform_buffer(
        &mut self,
        _stage: usize,
        _binding_index: u32,
        _buffer: &mut Self::Buffer,
        _offset: u32,
        _size: u32,
    ) {
    }

    fn bind_storage_buffer(
        &mut self,
        _stage: usize,
        _binding_index: u32,
        _buffer: &mut Self::Buffer,
        _offset: u32,
        _size: u32,
        _is_written: bool,
    ) {
    }

    fn bind_texture_buffer(
        &mut self,
        _buffer: &mut Self::Buffer,
        _offset: u32,
        _size: u32,
        _format: PixelFormat,
    ) {
    }

    fn bind_image_buffer(
        &mut self,
        _buffer: &mut Self::Buffer,
        _offset: u32,
        _size: u32,
        _format: PixelFormat,
    ) {
    }

    fn bind_transform_feedback_buffers(
        &mut self,
        _bindings: &HostBindings,
        _buffers: &mut SlotVector<Self::Buffer>,
    ) {
    }

    fn bind_compute_uniform_buffer(
        &mut self,
        _binding_index: u32,
        _buffer: &mut Self::Buffer,
        _offset: u32,
        _size: u32,
    ) {
    }

    fn bind_compute_storage_buffer(
        &mut self,
        _binding_index: u32,
        _buffer: &mut Self::Buffer,
        _offset: u32,
        _size: u32,
        _is_written: bool,
    ) {
    }
}

// ---------------------------------------------------------------------------
// GpuMemoryAccess trait — GPU address translation
// ---------------------------------------------------------------------------

/// Trait for GPU virtual address translation and memory reads.
///
/// Upstream: these operations are performed via `gpu_memory` (a `Tegra::MemoryManager*`)
/// which is set per-channel. The buffer cache calls:
/// - `GpuToCpuAddress(gpu_addr)` — translate GPU VA to device/CPU address
/// - `Read<T>(gpu_addr)` — read a typed value from GPU VA space
/// - `IsWithinGPUAddressRange(gpu_addr)` — bounds check
/// - `MaxContinuousRange(gpu_addr, size)` — find max mapped range
/// - `GetMemoryLayoutSize(gpu_addr)` — get mapped size from an address
pub trait GpuMemoryAccess {
    /// Translate a GPU virtual address to a device (CPU) address.
    ///
    /// Upstream: `gpu_memory->GpuToCpuAddress(gpu_addr)`
    fn gpu_to_cpu_address(&self, gpu_addr: u64) -> Option<u64>;

    /// Read a `u64` from GPU virtual address space.
    ///
    /// Upstream: `gpu_memory->Read<u64>(gpu_addr)`
    fn read_u64(&self, gpu_addr: u64) -> Option<u64>;

    /// Read a `u32` from GPU virtual address space.
    ///
    /// Upstream: `gpu_memory->Read<u32>(gpu_addr)`
    fn read_u32(&self, gpu_addr: u64) -> Option<u32>;

    /// Check if a GPU address is within the valid address range.
    ///
    /// Upstream: `gpu_memory->IsWithinGPUAddressRange(gpu_addr)`
    fn is_within_gpu_address_range(&self, gpu_addr: u64) -> bool;

    /// Return the maximum continuous mapped range from `gpu_addr`.
    ///
    /// Upstream: `gpu_memory->MaxContinuousRange(gpu_addr, size)`
    fn max_continuous_range(&self, gpu_addr: u64, size: u64) -> u64;

    /// Return the total mapped size starting from a GPU address.
    ///
    /// Upstream: `gpu_memory->GetMemoryLayoutSize(gpu_addr)`
    fn get_memory_layout_size(&self, gpu_addr: u64) -> u64;
}

// ---------------------------------------------------------------------------
// DeviceMemoryAccess trait — host CPU memory read/write
// ---------------------------------------------------------------------------

/// Trait for reading/writing guest physical (device) memory.
///
/// Upstream: these operations are performed via `device_memory`
/// (a `Tegra::MaxwellDeviceMemoryManager&`).
pub trait DeviceMemoryAccess {
    /// Get a pointer to guest memory at `device_addr`.
    ///
    /// Upstream: `device_memory.GetPointer<u8>(device_addr)`
    /// Returns None if the address is not directly accessible.
    fn get_pointer(&self, device_addr: u64) -> Option<*const u8>;

    /// Read a block of bytes from guest memory.
    ///
    /// Upstream: `device_memory.ReadBlockUnsafe(device_addr, dst, size)`
    fn read_block_unsafe(&self, device_addr: u64, dst: &mut [u8]);

    /// Write a block of bytes to guest memory.
    ///
    /// Upstream: `device_memory.WriteBlockUnsafe(device_addr, src, size)`
    fn write_block_unsafe(&self, device_addr: u64, src: &[u8]);
}

// ---------------------------------------------------------------------------
// DrawIndirectParams — draw indirect state
// ---------------------------------------------------------------------------

/// Parameters for indirect draw calls.
///
/// Upstream: `Tegra::Engines::DrawManager::IndirectParams`
#[derive(Debug, Clone, Copy)]
pub struct DrawIndirectParams {
    /// GPU address of the indirect buffer.
    pub indirect_start_address: u64,
    /// GPU address of the count buffer.
    pub count_start_address: u64,
    /// Total size of the indirect buffer in bytes.
    pub buffer_size: u64,
    /// Maximum number of draw calls.
    pub max_draw_counts: u32,
    /// Stride between draw commands.
    pub stride: u32,
    /// Whether to include a count buffer.
    pub include_count: bool,
}

/// Index buffer reference from the draw state.
///
/// Upstream: `maxwell3d->draw_manager->GetDrawState().index_buffer`
#[derive(Debug, Clone, Copy, Default)]
pub struct IndexBufferRef {
    /// GPU virtual address of the start of the index buffer.
    pub start_address: u64,
    /// GPU virtual address of the end of the index buffer.
    pub end_address: u64,
    /// Number of indices.
    pub count: u32,
    /// First index offset.
    pub first: u32,
    /// Bytes per index element (1, 2, or 4).
    pub format_size_in_bytes: u32,
}

/// Dirty flags used by the buffer cache.
///
/// Upstream: `VideoCommon::Dirty::*` flags used in `maxwell3d->dirty.flags[]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirtyFlag {
    IndexBuffer,
    VertexBuffers,
    VertexBuffer(u32), // VertexBuffer0 + index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_null_binding() {
        assert_eq!(NULL_BINDING.device_addr, 0);
        assert_eq!(NULL_BINDING.size, 0);
        assert_eq!(NULL_BINDING.buffer_id, NULL_BUFFER_ID);
    }

    #[test]
    fn test_null_buffer_id() {
        assert_eq!(NULL_BUFFER_ID.index, 0);
    }

    #[test]
    fn obtain_buffer_enum_values_match_upstream() {
        assert_eq!(ObtainBufferSynchronize::NoSynchronize as u32, 0);
        assert_eq!(ObtainBufferSynchronize::FullSynchronize as u32, 1);
        assert_eq!(ObtainBufferSynchronize::SynchronizeNoDirty as u32, 2);
        assert_eq!(ObtainBufferOperation::DoNothing as u32, 0);
        assert_eq!(ObtainBufferOperation::MarkAsWritten as u32, 1);
        assert_eq!(ObtainBufferOperation::DiscardWrite as u32, 2);
        assert_eq!(ObtainBufferOperation::MarkQuery as u32, 3);
    }

    #[test]
    fn test_channel_info_default() {
        let info = BufferCacheChannelInfo::default();
        assert_eq!(info.enabled_compute_uniform_buffer_mask, 0);
        assert!(!info.has_deleted_buffers);
        assert_eq!(info.uniform_buffer_skip_cache_size, DEFAULT_SKIP_CACHE_SIZE);
        assert_eq!(
            info.uniform_cache_hits.len(),
            NUM_GRAPHICS_UNIFORM_BUFFERS as usize
        );
        assert_eq!(
            info.uniform_cache_shots.len(),
            NUM_GRAPHICS_UNIFORM_BUFFERS as usize
        );
    }

    #[test]
    fn test_host_bindings_default() {
        let mut bindings = HostBindings::default();
        assert_eq!(bindings.min_index, NUM_VERTEX_BUFFERS);
        assert_eq!(bindings.max_index, 0);
        assert!(bindings.buffer_ids.is_empty());
        for _ in 0..NUM_VERTEX_BUFFERS {
            bindings.buffer_ids.push(NULL_BUFFER_ID);
            bindings.offsets.push(0);
            bindings.sizes.push(0);
            bindings.strides.push(0);
        }
        assert!(!bindings.buffer_ids.spilled());
        assert!(!bindings.offsets.spilled());
        assert!(!bindings.sizes.spilled());
        assert!(!bindings.strides.spilled());
    }

    #[test]
    fn overlap_result_keeps_upstream_inline_capacity() {
        let mut overlap = OverlapResult::default();
        for index in 0..16 {
            overlap.ids.push(SlotId { index });
        }
        assert!(!overlap.ids.spilled());
        overlap.ids.push(SlotId { index: 16 });
        assert!(overlap.ids.spilled());
    }

    #[test]
    fn runtime_exposes_upstream_single_vertex_binding_contract() {
        fn require_signature(
            _: fn(&mut TestBufferCacheRuntime, u32, &mut TestBuffer, u32, u32, u32),
        ) {
        }

        require_signature(TestBufferCacheRuntime::bind_vertex_buffer);
    }

    #[test]
    fn staging_buffer_ref_preserves_native_64_bit_backend_handle() {
        let native_handle = u64::from(u32::MAX) + 0x1234;
        let staging = unsafe {
            StagingBufferRef::from_mapped_backend(
                NULL_BUFFER_ID,
                native_handle,
                0,
                0,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(staging.gpu_handle, native_handle);
    }
}
