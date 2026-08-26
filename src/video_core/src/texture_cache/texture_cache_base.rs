// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/texture_cache/texture_cache_base.h
//!
//! Defines the base data structures for the texture cache: channel info,
//! slot vectors, pending downloads, join-caching state, and the generic
//! `TextureCache<P>` skeleton.
//!
//! The upstream file is a ~510-line header that combines:
//!   - `TextureCacheChannelInfo` (per-channel descriptor tables)
//!   - `TextureCache<P>` template class definition
//!   - supporting inner types (`BlitImages`, `PendingDownload`, etc.)
//!
//! The template implementation lives in texture_cache.h (texture_cache.rs).

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};

use common::hash::{BuildIdentityHasher, BuildUnorderedDenseHasher};
use common::lru_cache::LeastRecentlyUsedCache;
use common::scratch_buffer::ScratchBuffer;
use common::thread::ThreadPlacement;
use common::thread_worker::ThreadWorker;
use parking_lot::{Mutex as ParkingMutex, ReentrantMutex};
use smallvec::SmallVec;

use common::slot_vector::SlotVector;

use super::descriptor_table::DescriptorTable;
use super::image_base::{
    GPUVAddr, ImageAllocBase, ImageBase, ImageFlagBits, ImageMapView, NullImageParams,
};
use super::image_view_base::{ImageViewBase, NullImageViewParams};
use super::image_view_info::ImageViewInfo;
use super::render_targets::RenderTargets;
use super::types::*;
use crate::control::channel_state::ChannelState;
use crate::control::channel_state_cache::{
    ChannelCacheAccessor, ChannelInfo, ChannelSetupCaches, FromChannelState,
};
use crate::delayed_destruction_ring::DelayedDestructionRing;
use crate::dirty_flags;
use crate::engines::draw_manager::Maxwell3DAccess;
use crate::engines::maxwell_3d::Maxwell3D;
use crate::memory_manager::MemoryManager;
use crate::renderer_base::GuestMemoryWriter;

// ── Constants ──────────────────────────────────────────────────────────

/// Address shift for caching images into a hash table.
pub(super) const YUZU_PAGEBITS: u64 = 20;

// ── ImageViewInOut ─────────────────────────────────────────────────────

/// In/out parameter for batch image-view resolution.
///
/// Port of `VideoCommon::ImageViewInOut`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ImageViewInOut {
    pub index: u32,
    pub blacklist: bool,
    pub id: ImageViewId,
}

/// Backend-independent result of `TextureCache<P>::TryFindFramebufferImageView`.
///
/// Upstream returns `P::ImageView*` plus the rescale flag. Rust returns the
/// stable typed-slot ID with a snapshot of its common base so callers do not
/// retain a borrow across backend preparation.
#[derive(Debug, Clone)]
pub struct FramebufferImageView {
    pub view_id: ImageViewId,
    pub view: ImageViewBase,
    pub scaled: bool,
}

pub type ImageDownloader =
    Arc<dyn Fn(ImageId, &ImageBase, &mut [u8]) -> bool + Send + Sync + 'static>;

// ── AsyncDecodeContext ─────────────────────────────────────────────────

/// State for an in-flight asynchronous texture decode.
///
/// Port of `VideoCommon::AsyncDecodeContext`.
pub struct AsyncDecodeContext {
    pub image_id: ImageId,
    pub output: Mutex<AsyncDecodeOutput>,
    pub complete: std::sync::atomic::AtomicBool,
}

pub struct AsyncDecodeOutput {
    pub decoded_data: ScratchBuffer<u8>,
    pub copies: SmallVec<[BufferImageCopy; 16]>,
}

/// State for an in-flight GPU block-linear 3D unswizzle.
///
/// Port of `TextureCache<P>::PendingUnswizzle`, including its backend-specific
/// `AsyncBuffer` staging allocation.
pub struct PendingUnswizzle<B = ()> {
    pub image_id: ImageId,
    pub info: super::image_info::ImageInfo,
    pub current_offset: usize,
    pub total_size: usize,
    pub staging_buffer: Option<B>,
    pub last_submitted_offset: usize,
    pub bytes_per_slice: usize,
    pub initialized: bool,
}

impl<B> PendingUnswizzle<B> {
    pub(super) fn new(image_id: ImageId, info: super::image_info::ImageInfo) -> Self {
        Self {
            image_id,
            info,
            current_offset: 0,
            total_size: 0,
            staging_buffer: None,
            last_submitted_offset: 0,
            bytes_per_slice: 0,
            initialized: false,
        }
    }
}

impl AsyncDecodeContext {
    pub fn new(image_id: ImageId) -> Self {
        Self {
            image_id,
            output: Mutex::new(AsyncDecodeOutput {
                decoded_data: ScratchBuffer::new(),
                copies: SmallVec::new(),
            }),
            complete: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

// ── TextureCacheGPUMap ─────────────────────────────────────────────────

/// GPU page table: maps a 20-bit-shifted GPU address to a vec of image ids.
///
/// Port of `VideoCommon::TextureCacheGPUMap`.
pub type TextureCacheGPUMap = HashMap<u64, Vec<ImageId>, BuildUnorderedDenseHasher>;

// ── DescriptorSyncRegs ─────────────────────────────────────────────────

/// Snapshot of the Maxwell3D registers consumed by
/// `TextureCacheBase::synchronize_graphics_descriptors`.
///
/// Mirrors the fields upstream `KConditionVariable<P>::SynchronizeGraphicsDescriptors`
/// reads off `maxwell3d->regs`:
/// * `regs.sampler_binding == SamplerBinding::ViaHeaderBinding`
/// * `regs.tex_header.limit` / `regs.tex_header.Address()`
/// * `regs.tex_sampler.limit` / `regs.tex_sampler.Address()`
///
/// Passing a snapshot keeps the texture cache from needing a back-reference
/// to `Maxwell3D` (which is owned by the GPU side of the channel).
#[derive(Debug, Clone, Copy, Default)]
pub struct DescriptorSyncRegs {
    pub sampler_binding_via_header: bool,
    pub tex_header_addr: GPUVAddr,
    pub tex_header_limit: u32,
    pub tex_sampler_addr: GPUVAddr,
    pub tex_sampler_limit: u32,
}

/// Snapshot of the KeplerCompute registers consumed by
/// `TextureCacheBase::synchronize_compute_descriptors`.
///
/// Mirrors upstream `TextureCache<P>::SynchronizeComputeDescriptors`:
/// * `kepler_compute->launch_description.linked_tsc`
/// * `kepler_compute->regs.tic.Address()` / `.limit`
/// * `kepler_compute->regs.tsc.Address()` / `.limit`
#[derive(Debug, Clone, Copy, Default)]
pub struct ComputeDescriptorSyncRegs {
    pub linked_tsc: bool,
    pub tic_addr: GPUVAddr,
    pub tic_limit: u32,
    pub tsc_addr: GPUVAddr,
    pub tsc_limit: u32,
}

// ── TextureCacheChannelInfo ────────────────────────────────────────────

/// Per-channel state for the texture cache.
///
/// Port of `VideoCommon::TextureCacheChannelInfo`.
///
/// The upstream class inherits from `ChannelInfo` and holds descriptor
/// tables plus cached sampler/image-view mappings.
pub struct TextureCacheChannelInfo {
    pub channel_info: ChannelInfo,

    // Descriptor tables — typed against the real `TicEntry`/`TscEntry`
    // structs from `video_core::textures::texture`. Reads are performed
    // through the bound channel GPU memory when available, matching
    // upstream's `DescriptorTable<T>{gpu_memory}` owner.
    pub graphics_image_table: DescriptorTable<crate::textures::texture::TicEntry>,
    pub graphics_sampler_table: DescriptorTable<crate::textures::texture::TscEntry>,

    pub compute_image_table: DescriptorTable<crate::textures::texture::TicEntry>,
    pub compute_sampler_table: DescriptorTable<crate::textures::texture::TscEntry>,

    // Per-channel caches. Upstream uses
    //   std::unordered_map<TICEntry, ImageViewId> image_views;
    //   std::unordered_map<TSCEntry, SamplerId>   samplers;
    // keyed by descriptor identity (raw `[u64; 4]`). `TicEntry`/`TscEntry`
    // expose manual `Hash`+`PartialEq` impls over `raw`, so the Rust
    // `HashMap` keys them directly — same lookup semantics as upstream's
    // `try_emplace(descriptor)`.
    pub image_views: HashMap<crate::textures::texture::TicEntry, ImageViewId, BuildIdentityHasher>,
    pub samplers: HashMap<crate::textures::texture::TscEntry, SamplerId, BuildIdentityHasher>,
    pub sampler_ids: HashMap<u32, SamplerId, BuildUnorderedDenseHasher>,
    pub image_view_ids: HashMap<u32, ImageViewId, BuildUnorderedDenseHasher>,

    pub gpu_page_table_index: Option<usize>,
    pub sparse_page_table_index: Option<usize>,
}

impl TextureCacheChannelInfo {
    pub fn new() -> Self {
        Self {
            channel_info: ChannelInfo {
                maxwell3d: 0,
                kepler_compute: 0,
                gpu_memory_index: 0,
                gpu_memory: None,
                program_id: 0,
            },
            graphics_image_table: DescriptorTable::new(),
            graphics_sampler_table: DescriptorTable::new(),
            compute_image_table: DescriptorTable::new(),
            compute_sampler_table: DescriptorTable::new(),
            image_views: HashMap::default(),
            samplers: HashMap::default(),
            sampler_ids: HashMap::default(),
            image_view_ids: HashMap::default(),
            gpu_page_table_index: None,
            sparse_page_table_index: None,
        }
    }
}

impl FromChannelState for TextureCacheChannelInfo {
    fn from_channel_state(state: &ChannelState) -> Self {
        let mut info = Self::new();
        info.channel_info = ChannelInfo::from_channel_state(state);
        info
    }
}

impl ChannelCacheAccessor for TextureCacheChannelInfo {
    fn maxwell3d_ref(&self) -> usize {
        self.channel_info.maxwell3d
    }

    fn kepler_compute_ref(&self) -> usize {
        self.channel_info.kepler_compute
    }

    fn gpu_memory_id(&self) -> usize {
        self.channel_info.gpu_memory_index
    }

    fn gpu_memory_arc(&self) -> Option<Arc<ParkingMutex<MemoryManager>>> {
        self.channel_info.gpu_memory.as_ref().map(Arc::clone)
    }

    fn program_id_val(&self) -> u64 {
        self.channel_info.program_id
    }
}

// ── BlitImages (private helper) ────────────────────────────────────────

/// Identifies a source/destination image pair for a blit.
///
/// Port of `TextureCache::BlitImages`.
#[derive(Debug, Clone, Copy)]
pub struct BlitImages {
    pub dst_id: ImageId,
    pub src_id: ImageId,
    pub dst_format: super::format_lookup_table::PixelFormat,
    pub src_format: super::format_lookup_table::PixelFormat,
}

// ── PendingDownload / BufferDownload ───────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct BufferDownload {
    pub address: GPUVAddr,
    pub size: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct PendingDownload {
    pub is_swizzle: bool,
    pub async_buffer_id: usize,
    pub object_id: common::slot_vector::SlotId,
}

// ── JoinCopy ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct JoinCopy {
    pub is_alias: bool,
    pub id: ImageId,
}

// ── TextureCache<P> ────────────────────────────────────────────────────

/// Backend policy used by upstream `TextureCache<P>`.
///
/// The associated types mirror `texture_cache_base.h`. Rust represents the
/// C++ base-class subobject and backend payload next to each other in one slot;
/// `Deref` exposes the common base to the templated cache implementation.
pub trait TextureCacheParams {
    type Runtime;
    type Image;
    type ImageAlloc;
    type ImageView;
    type Sampler;
    type Framebuffer;
    type FramebufferError;
    type AsyncBuffer;
    type BufferType;

    const ENABLE_VALIDATION: bool;
    const FRAMEBUFFER_BLITS: bool;
    const HAS_EMULATED_COPIES: bool;
    const HAS_DEVICE_MEMORY_INFO: bool;
    const IMPLEMENTS_ASYNC_DOWNLOADS: bool;

    /// Construct the concrete object that upstream inserts directly into
    /// `SlotVector<Image>`.
    fn create_image(
        runtime: Option<&mut Self::Runtime>,
        image_id: ImageId,
        base: std::ptr::NonNull<ImageBase>,
    ) -> Self::Image;

    fn set_image_allocation_tick(image: &mut Self::Image, allocation_tick: u64);

    /// Construct the concrete object that upstream inserts directly into
    /// `SlotVector<ImageView>`.
    fn create_image_view(
        runtime: Option<&mut Self::Runtime>,
        view_id: ImageViewId,
        info: &ImageViewInfo,
        base: std::ptr::NonNull<ImageViewBase>,
        image: Option<&Self::Image>,
    ) -> Self::ImageView;

    fn create_sampler(
        runtime: Option<&mut Self::Runtime>,
        config: &crate::textures::texture::TscEntry,
    ) -> Self::Sampler;

    /// Construct the concrete object inserted by upstream
    /// `TextureCache<P>::GetFramebufferId`.
    fn create_framebuffer(
        runtime: Option<&mut Self::Runtime>,
        color_buffers: [Option<std::ptr::NonNull<Self::ImageView>>; NUM_RT],
        depth_buffer: Option<std::ptr::NonNull<Self::ImageView>>,
        key: &RenderTargets,
    ) -> Result<Self::Framebuffer, Self::FramebufferError>;

    fn prepare_image_view(
        cache: &mut TextureCacheBase<Self>,
        image_view_id: ImageViewId,
        is_modification: bool,
        invalidate: bool,
    ) where
        Self: Sized;

    /// Backend-owned `P::Image::ScaleUp` operation. Upstream keeps the
    /// rescale decision, memory accounting and invalidation in the common
    /// `TextureCache<P>::ScaleUp` method.
    fn scale_up_image(cache: &mut TextureCacheBase<Self>, image_id: ImageId, ignore: bool) -> bool
    where
        Self: Sized;

    /// Backend-owned `P::Image::ScaleDown` operation. See `scale_up_image`.
    fn scale_down_image(
        cache: &mut TextureCacheBase<Self>,
        image_id: ImageId,
        ignore: bool,
    ) -> bool
    where
        Self: Sized;

    /// Backend `Runtime::UploadStagingBuffer` primitive used by the common
    /// `TextureCache<P>` upload paths.
    fn upload_staging_buffer(
        cache: &mut TextureCacheBase<Self>,
        size: usize,
        deferred: bool,
    ) -> Self::AsyncBuffer
    where
        Self: Sized;

    /// Mutable mapped span of the backend staging allocation.
    fn staging_mapped_span(buffer: &mut Self::AsyncBuffer) -> &mut [u8];

    /// Backend `Runtime::FreeDeferredStagingBuffer` primitive.
    fn free_deferred_staging_buffer(
        cache: &mut TextureCacheBase<Self>,
        buffer: &mut Self::AsyncBuffer,
    ) where
        Self: Sized;

    /// Backend `Runtime::CanUploadMSAA` primitive.
    fn can_upload_msaa(cache: &TextureCacheBase<Self>) -> bool
    where
        Self: Sized;

    /// Backend `Runtime::TransitionImageLayout` primitive.
    fn transition_image_layout(cache: &mut TextureCacheBase<Self>, image_id: ImageId)
    where
        Self: Sized;

    /// Backend `Image::UploadMemory` primitive.
    fn upload_image(
        cache: &mut TextureCacheBase<Self>,
        image_id: ImageId,
        staging: &Self::AsyncBuffer,
        copies: &[BufferImageCopy],
    ) where
        Self: Sized;

    /// Backend `Runtime::AccelerateImageUpload` primitive.
    fn accelerate_image_upload(
        cache: &mut TextureCacheBase<Self>,
        image_id: ImageId,
        staging: &Self::AsyncBuffer,
        swizzles: &[SwizzleParameters],
        z_start: u32,
        z_count: u32,
    ) where
        Self: Sized;

    /// Backend `Runtime::InsertUploadMemoryBarrier` primitive.
    fn insert_upload_memory_barrier(cache: &mut TextureCacheBase<Self>)
    where
        Self: Sized;

    /// Backend `Runtime::CanImageBeCopied` primitive used by the common
    /// `TextureCache<P>::CopyImage` policy.
    fn can_image_be_copied(
        _cache: &TextureCacheBase<Self>,
        _dst_id: ImageId,
        _src_id: ImageId,
    ) -> bool
    where
        Self: Sized,
    {
        true
    }

    /// Backend `Runtime::CopyImage` primitive used by the common
    /// `TextureCache<P>::CopyImage` policy.
    fn copy_image(
        cache: &mut TextureCacheBase<Self>,
        dst_id: ImageId,
        src_id: ImageId,
        copies: &[ImageCopy],
    ) where
        Self: Sized;

    /// Backend `Runtime::EmulateCopyImage` primitive used when
    /// `HAS_EMULATED_COPIES` is set and a direct copy is unsuitable.
    fn emulate_copy_image(
        cache: &mut TextureCacheBase<Self>,
        dst_id: ImageId,
        src_id: ImageId,
        copies: &[ImageCopy],
    ) where
        Self: Sized,
    {
        Self::copy_image(cache, dst_id, src_id, copies);
    }

    /// Backend `Runtime::ShouldReinterpret` primitive.
    fn should_reinterpret(
        _cache: &TextureCacheBase<Self>,
        _dst_id: ImageId,
        _src_id: ImageId,
    ) -> bool
    where
        Self: Sized,
    {
        false
    }

    /// Backend `Runtime::ReinterpretImage` primitive.
    fn reinterpret_image(
        _cache: &mut TextureCacheBase<Self>,
        _dst_id: ImageId,
        _src_id: ImageId,
        _copies: &[ImageCopy],
    ) where
        Self: Sized,
    {
    }

    /// Backend `Runtime::ConvertImage` primitive. The common cache owns
    /// image-view/framebuffer selection exactly as upstream does.
    fn convert_image(
        _cache: &mut TextureCacheBase<Self>,
        _dst_framebuffer_id: FramebufferId,
        _dst_view_id: ImageViewId,
        _src_view_id: ImageViewId,
    ) where
        Self: Sized,
    {
    }

    /// Backend operation used by the multisample branch of upstream
    /// `TextureCache<P>::JoinImages`.
    fn copy_image_msaa(
        cache: &mut TextureCacheBase<Self>,
        dst_id: ImageId,
        src_id: ImageId,
        copies: &[ImageCopy],
    ) where
        Self: Sized;

    /// Backend runtime operation selected by upstream
    /// `TextureCache<P>::BlitImage` after the common cache has resolved the
    /// images, views, framebuffers, scaling state and regions.
    fn blit_image(
        _cache: &mut TextureCacheBase<Self>,
        _dst_framebuffer_id: FramebufferId,
        _src_framebuffer_id: FramebufferId,
        _dst_view_id: ImageViewId,
        _src_view_id: ImageViewId,
        _dst_region: Region2D,
        _src_region: Region2D,
        _filter: crate::engines::fermi_2d::Filter,
        _operation: crate::engines::fermi_2d::Operation,
    ) where
        Self: Sized,
    {
    }
}

/// Rust representation of an upstream backend class inheriting `ImageBase`.
/// The common and backend portions occupy one slot and therefore share one
/// publication/destruction lifecycle.
#[derive(Debug)]
pub struct ImageSlot<B = ()> {
    /// The derived/backend portion is declared first so Rust drops it before
    /// the boxed base, matching C++ derived-destructor then base-destructor
    /// ordering.
    pub backend: Option<B>,
    /// Stable allocation corresponding to the `ImageBase` subobject inherited
    /// by upstream `P::Image`. Backend payloads may keep a non-owning pointer
    /// to it while the complete slot moves through slot vectors and delayed
    /// destruction rings.
    pub base: Box<ImageBase>,
}

impl<B> ImageSlot<B> {
    pub fn pending(base: ImageBase) -> Self {
        Self {
            backend: None,
            base: Box::new(base),
        }
    }
}

impl<B> Deref for ImageSlot<B> {
    type Target = ImageBase;

    fn deref(&self) -> &Self::Target {
        self.base.as_ref()
    }
}

impl<B> DerefMut for ImageSlot<B> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.base.as_mut()
    }
}

impl<B> From<ImageBase> for ImageSlot<B> {
    fn from(value: ImageBase) -> Self {
        Self::pending(value)
    }
}

#[derive(Debug)]
pub struct ImageViewSlot<B = ()> {
    /// Drop the derived/backend portion before its base subobject.
    pub backend: Option<B>,
    /// Constructor input retained for backend rematerialisation. Upstream
    /// consumes this directly while constructing its derived `ImageView`.
    pub info: ImageViewInfo,
    /// Stable allocation corresponding to upstream's inherited
    /// `ImageViewBase` subobject. See `ImageSlot::base`.
    pub base: Box<ImageViewBase>,
}

impl<B> ImageViewSlot<B> {
    pub fn pending(info: ImageViewInfo, base: ImageViewBase) -> Self {
        Self {
            backend: None,
            info,
            base: Box::new(base),
        }
    }
}

impl<B> Deref for ImageViewSlot<B> {
    type Target = ImageViewBase;

    fn deref(&self) -> &Self::Target {
        self.base.as_ref()
    }
}

impl<B> DerefMut for ImageViewSlot<B> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.base.as_mut()
    }
}

#[derive(Debug, Clone, Default)]
pub struct ImageAllocSlot<B = ()> {
    pub base: ImageAllocBase,
    pub backend: Option<B>,
}

impl<B> Deref for ImageAllocSlot<B> {
    type Target = ImageAllocBase;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl<B> DerefMut for ImageAllocSlot<B> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

impl<B> From<ImageAllocBase> for ImageAllocSlot<B> {
    fn from(value: ImageAllocBase) -> Self {
        Self {
            base: value,
            backend: None,
        }
    }
}

pub struct SamplerSlot<B = ()> {
    pub config: crate::textures::texture::TscEntry,
    pub backend: Option<B>,
}

impl<B> From<crate::textures::texture::TscEntry> for SamplerSlot<B> {
    fn from(value: crate::textures::texture::TscEntry) -> Self {
        Self {
            config: value,
            backend: None,
        }
    }
}

impl<B> Deref for SamplerSlot<B> {
    type Target = crate::textures::texture::TscEntry;

    fn deref(&self) -> &Self::Target {
        &self.config
    }
}

/// Backend-neutral policy retained for common-cache unit tests.
pub struct CommonTextureCacheParams;

impl TextureCacheParams for CommonTextureCacheParams {
    type Runtime = ();
    type Image = ();
    type ImageAlloc = ();
    type ImageView = ();
    type Sampler = ();
    type Framebuffer = ();
    type FramebufferError = std::convert::Infallible;
    type AsyncBuffer = Vec<u8>;
    type BufferType = ();

    const ENABLE_VALIDATION: bool = true;
    const FRAMEBUFFER_BLITS: bool = false;
    const HAS_EMULATED_COPIES: bool = false;
    const HAS_DEVICE_MEMORY_INFO: bool = false;
    const IMPLEMENTS_ASYNC_DOWNLOADS: bool = false;

    fn create_image(_: Option<&mut ()>, _: ImageId, _: std::ptr::NonNull<ImageBase>) {}

    fn set_image_allocation_tick(_: &mut (), _: u64) {}

    fn create_image_view(
        _: Option<&mut ()>,
        _: ImageViewId,
        _: &ImageViewInfo,
        _: std::ptr::NonNull<ImageViewBase>,
        _: Option<&()>,
    ) {
    }

    fn create_sampler(_: Option<&mut ()>, _: &crate::textures::texture::TscEntry) {}

    fn create_framebuffer(
        _: Option<&mut ()>,
        _: [Option<std::ptr::NonNull<()>>; NUM_RT],
        _: Option<std::ptr::NonNull<()>>,
        _: &RenderTargets,
    ) -> Result<(), std::convert::Infallible> {
        Ok(())
    }

    fn prepare_image_view(_: &mut TextureCacheBase<Self>, _: ImageViewId, _: bool, _: bool) {}

    fn scale_up_image(_: &mut TextureCacheBase<Self>, _: ImageId, _: bool) -> bool {
        false
    }

    fn scale_down_image(_: &mut TextureCacheBase<Self>, _: ImageId, _: bool) -> bool {
        false
    }

    fn upload_staging_buffer(_: &mut TextureCacheBase<Self>, size: usize, _: bool) -> Vec<u8> {
        vec![0; size]
    }

    fn staging_mapped_span(buffer: &mut Vec<u8>) -> &mut [u8] {
        buffer
    }

    fn free_deferred_staging_buffer(_: &mut TextureCacheBase<Self>, _: &mut Vec<u8>) {}

    fn can_upload_msaa(_: &TextureCacheBase<Self>) -> bool {
        true
    }

    fn transition_image_layout(_: &mut TextureCacheBase<Self>, _: ImageId) {}

    fn upload_image(
        _: &mut TextureCacheBase<Self>,
        _: ImageId,
        _: &Vec<u8>,
        _: &[BufferImageCopy],
    ) {
    }

    fn accelerate_image_upload(
        _: &mut TextureCacheBase<Self>,
        _: ImageId,
        _: &Vec<u8>,
        _: &[SwizzleParameters],
        _: u32,
        _: u32,
    ) {
    }

    fn insert_upload_memory_barrier(_: &mut TextureCacheBase<Self>) {}

    fn copy_image(_: &mut TextureCacheBase<Self>, _: ImageId, _: ImageId, _: &[ImageCopy]) {}

    fn copy_image_msaa(_: &mut TextureCacheBase<Self>, _: ImageId, _: ImageId, _: &[ImageCopy]) {}
}

/// Memory thresholds (from upstream `TextureCache` template).
const TARGET_THRESHOLD: i64 = 4 * 1024 * 1024 * 1024; // 4 GiB
const DEFAULT_EXPECTED_MEMORY: i64 = 1024 * 1024 * 1024 + 125 * 1024 * 1024; // 1 GiB + 125 MiB
const DEFAULT_CRITICAL_MEMORY: i64 = 1024 * 1024 * 1024 + 625 * 1024 * 1024; // 1 GiB + 625 MiB
                                                                             // Preserved from upstream even though Eden does not currently consume it.
#[allow(dead_code)]
const GC_EMERGENCY_COUNTS: usize = 2;
pub(crate) const TICKS_TO_DESTROY: usize = 8;
// Preserved from upstream even though Eden does not currently consume it.
#[allow(dead_code)]
const UNSET_CHANNEL: usize = usize::MAX;

/// The main texture cache.
///
/// Port of the `TextureCache<P>` template class from texture_cache_base.h.
/// `P` in upstream is a policy class providing associated types
/// (`Runtime`, `Image`, `ImageView`, `Sampler`, `Framebuffer`, etc.).
///
pub struct TextureCacheBase<P: TextureCacheParams = CommonTextureCacheParams> {
    // Slot storage
    pub slot_images: SlotVector<ImageSlot<P::Image>>,
    pub slot_map_views: SlotVector<ImageMapView>,
    pub slot_image_views: SlotVector<ImageViewSlot<P::ImageView>>,
    pub slot_image_allocs: SlotVector<ImageAllocSlot<P::ImageAlloc>>,
    pub slot_samplers: SlotVector<SamplerSlot<P::Sampler>>,
    pub slot_framebuffers: SlotVector<P::Framebuffer>,
    pub slot_buffer_downloads: SlotVector<BufferDownload>,

    // TODO: Upstream notes that this async-download storage should be reworked.
    pub uncommitted_async_buffers: Vec<P::AsyncBuffer>,
    pub async_buffers: VecDeque<Vec<P::AsyncBuffer>>,
    pub async_buffers_death_ring: VecDeque<P::AsyncBuffer>,

    // Render state
    pub render_targets: RenderTargets,
    pub render_targets_serial: u64,
    pub rt_active_mask: u32,
    pub rt_image_id: [ImageId; 8],
    pub rt_depth_image_id: ImageId,
    pub texture_bindings_serial: u64,
    pub last_feedback_loop_serial: u64,
    pub last_feedback_texture_serial: u64,
    pub last_feedback_loop_result: bool,
    pub last_framebuffer_id: FramebufferId,
    pub last_framebuffer_serial: u64,
    /// Upstream keys cached framebuffers by the full `RenderTargets` object.
    pub framebuffers: HashMap<RenderTargets, FramebufferId, BuildUnorderedDenseHasher>,
    // Page tables
    pub page_table: HashMap<u64, Vec<ImageMapId>, BuildUnorderedDenseHasher>,
    pub sparse_views: HashMap<ImageId, SmallVec<[ImageMapId; 16]>, BuildUnorderedDenseHasher>,

    // Memory tracking
    pub has_deleted_images: bool,
    pub is_rescaling: bool,
    pub total_used_memory: u64,
    pub minimum_memory: u64,
    pub expected_memory: u64,
    pub critical_memory: u64,
    /// Upstream: `Common::LeastRecentlyUsedCache<LRUItemParams> lru_cache`.
    pub lru_cache: LeastRecentlyUsedCache<ImageId, u64>,
    /// Upstream: `DelayedDestructionRing<Image, TICKS_TO_DESTROY> sentenced_images`.
    pub sentenced_images: DelayedDestructionRing<ImageSlot<P::Image>, TICKS_TO_DESTROY>,
    /// Upstream: `DelayedDestructionRing<ImageView, TICKS_TO_DESTROY> sentenced_image_view`.
    pub sentenced_image_view: DelayedDestructionRing<ImageViewSlot<P::ImageView>, TICKS_TO_DESTROY>,
    pub sentenced_framebuffers: DelayedDestructionRing<P::Framebuffer, TICKS_TO_DESTROY>,
    pub has_broken_texture_view_formats: bool,
    pub has_native_bgr: bool,

    // Download tracking
    pub uncommitted_downloads: Vec<PendingDownload>,
    pub committed_downloads: VecDeque<Vec<PendingDownload>>,

    // Modification tick
    pub modification_tick: u64,
    pub frame_tick: u64,
    pub sampler_heap_budget: Option<usize>,
    pub last_sampler_gc_frame: u64,

    // Async decode
    pub async_decodes: Vec<Arc<AsyncDecodeContext>>,
    pub texture_decode_worker: ThreadWorker,

    // Async GPU unswizzle
    pub gpu_unswizzle_maxsize: usize,
    pub swizzle_chunk_size: usize,
    pub swizzle_slices_per_batch: u32,
    pub unswizzle_queue: VecDeque<PendingUnswizzle<P::AsyncBuffer>>,
    pub current_unswizzle_frame: u8,

    // Join caching
    pub join_overlap_ids: SmallVec<[ImageId; 4]>,
    pub join_overlaps_found: HashSet<ImageId, BuildUnorderedDenseHasher>,
    pub join_left_aliased_ids: SmallVec<[ImageId; 4]>,
    pub join_right_aliased_ids: SmallVec<[ImageId; 4]>,
    pub join_ignore_textures: HashSet<ImageId, BuildUnorderedDenseHasher>,
    pub join_bad_overlap_ids: SmallVec<[ImageId; 4]>,
    pub join_copies_to_do: SmallVec<[JoinCopy; 4]>,
    pub join_alias_indices: HashMap<ImageId, usize, BuildUnorderedDenseHasher>,

    // Image alloc table
    pub image_allocs_table: HashMap<GPUVAddr, ImageAllocId, BuildUnorderedDenseHasher>,
    /// Upstream `virtual_invalid_space`, used to allocate fake CPU ranges for
    /// images whose GPU address cannot be translated.
    pub virtual_invalid_space: u64,

    // Scratch buffers
    pub swizzle_data_buffer: ScratchBuffer<u8>,
    pub unswizzle_data_buffer: ScratchBuffer<u8>,

    // Rust adaptation of upstream `Runtime::DownloadStagingBuffer` +
    // backend `Image::DownloadMemory` and `Tegra::MemoryManager`.
    pub image_downloader: Option<ImageDownloader>,
    pub guest_memory_writer: Option<GuestMemoryWriter>,
    /// Channel-bound GPU memory manager.
    ///
    /// Upstream reaches this through `channel_state->gpu_memory`; sparse
    /// texture registration needs it for `GetSubmappedRange` and
    /// `GpuToCpuAddress`.
    pub channel_gpu_memory: Option<Arc<ParkingMutex<MemoryManager>>>,

    /// Shared `MaxwellDeviceMemoryManager` reference. Mirrors upstream
    /// `MaxwellDeviceMemoryManager& device_memory` member used by
    /// `TrackImage` / `UntrackImage` to drive `UpdatePagesCachedCount`.
    /// Same `Arc` as `Host1x::memory_manager()` and `ShaderCache::device_memory()`.
    pub device_memory:
        std::sync::Arc<crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager>,

    /// Upstream `gpu_page_table_storage`, two maps per registered GPU address
    /// space: dense pages at `storage_id * 2`, sparse pages at `storage_id * 2 + 1`.
    pub gpu_page_table_storage: Vec<TextureCacheGPUMap>,
    /// Dense GPU page-table owner captured when each image is registered.
    ///
    /// Upstream unregisters through `channel_state->gpu_page_table` while the
    /// originating channel is still current. Ruzu replays memory-manager
    /// notifications after releasing its Rust mutex, so another channel can be
    /// current by then; retaining the registration owner preserves the same
    /// page-table removal without changing image ownership.
    pub image_gpu_page_table_indices: HashMap<ImageId, usize, BuildUnorderedDenseHasher>,

    /// Per-channel descriptor state (TIC/TSC tables + cached id arrays).
    /// Upstream: inherited `ChannelSetupCaches<TextureCacheChannelInfo>`.
    pub channel_caches: ChannelSetupCaches<TextureCacheChannelInfo>,

    /// Fallback per-channel descriptor state for reduced tests that construct
    /// `TextureCacheBase` without creating/binding a real GPU channel first.
    /// Upstream: `TextureCache<P>` inherits from
    /// `VideoCommon::ChannelSetupCaches<TextureCacheChannelInfo>` which
    /// owns `channel_state` as a pointer to the currently-active channel.
    /// Ruzu currently models a single graphics channel, so the info is held
    /// inline; promote to `Option<Box<...>>` keyed by channel id when
    /// multi-channel support lands.
    pub channel_state: TextureCacheChannelInfo,

    // Mutex
    pub mutex: ReentrantMutex<()>,

    /// Owned storage behind upstream's `Runtime& runtime`. The Box keeps its
    /// address stable for backend objects which retain the runtime pointer.
    /// It is declared after every backend-owned object so it is destroyed
    /// last, matching the lifetime of upstream's external `Runtime&`.
    pub runtime: Option<Box<P::Runtime>>,
}

impl<P: TextureCacheParams> TextureCacheBase<P> {
    pub fn bind_runtime(&mut self, runtime: Box<P::Runtime>) {
        self.runtime = Some(runtime);
    }

    pub fn runtime(&self) -> &P::Runtime {
        self.runtime
            .as_deref()
            .expect("backend TextureCache runtime must be bound")
    }

    pub fn runtime_mut(&mut self) -> &mut P::Runtime {
        self.runtime
            .as_deref_mut()
            .expect("backend TextureCache runtime must be bound")
    }

    pub fn create_channel(&mut self, channel: &ChannelState) {
        {
            let channel_caches = &mut self.channel_caches;
            let gpu_page_table_storage = &mut self.gpu_page_table_storage;
            channel_caches.create_channel_with_on_gpu_as_register(channel, |_memory_id| {
                gpu_page_table_storage.push(TextureCacheGPUMap::default());
                gpu_page_table_storage.push(TextureCacheGPUMap::default());
            });
        }
        let Some(memory_manager) = channel.memory_manager.as_ref() else {
            return;
        };
        let memory_id = memory_manager.lock().get_id();
        let Some(storage_id) = self.channel_caches.get_storage_id(memory_id) else {
            return;
        };
        let dense_index = storage_id * 2;
        let sparse_index = dense_index + 1;
        if let Some(channel_state) = self
            .channel_caches
            .channel_state_by_bind_id_mut(channel.bind_id)
        {
            channel_state.gpu_page_table_index = Some(dense_index);
            channel_state.sparse_page_table_index = Some(sparse_index);
        }
    }

    pub fn bind_to_channel(&mut self, channel_id: i32) {
        self.channel_caches.bind_to_channel(channel_id);
        self.channel_gpu_memory = self
            .channel_caches
            .current_channel_state()
            .and_then(|channel| channel.channel_info.gpu_memory.as_ref().map(Arc::clone));
        self.rebase_virtual_invalid_images();
    }

    pub fn erase_channel(&mut self, channel_id: i32) {
        self.channel_caches.erase_channel(channel_id);
        self.channel_gpu_memory = self
            .channel_caches
            .current_channel_state()
            .and_then(|channel| channel.channel_info.gpu_memory.as_ref().map(Arc::clone));
    }

    pub(crate) fn current_channel_state(&self) -> &TextureCacheChannelInfo {
        self.channel_caches
            .current_channel_state()
            .unwrap_or(&self.channel_state)
    }

    pub(crate) fn current_channel_state_mut(&mut self) -> &mut TextureCacheChannelInfo {
        if self.channel_caches.has_current_channel_state() {
            self.channel_caches
                .current_channel_state_mut()
                .expect("current texture-cache channel must exist")
        } else {
            &mut self.channel_state
        }
    }

    pub(crate) fn for_each_active_channel_state_mut(
        &mut self,
        mut f: impl FnMut(&mut TextureCacheChannelInfo),
    ) {
        if self.channel_caches.has_current_channel_state() {
            self.channel_caches
                .for_each_active_channel_state_mut(|channel| f(channel));
        } else {
            f(&mut self.channel_state);
        }
    }

    pub(crate) fn current_gpu_page_table_index(&self, sparse: bool) -> Option<usize> {
        let channel_state = self.current_channel_state();
        if sparse {
            channel_state.sparse_page_table_index
        } else {
            channel_state.gpu_page_table_index
        }
    }

    pub(crate) fn mark_render_targets_dirty(&mut self) {
        let maxwell3d = self.current_channel_state().channel_info.maxwell3d;
        if maxwell3d == 0 {
            return;
        }

        // Upstream stores `Tegra::Engines::Maxwell3D* maxwell3d` on
        // `ChannelSetupCaches` and writes directly into `maxwell3d->dirty.flags`.
        // The Rust channel snapshot carries the same non-owning pointer.
        let maxwell3d = unsafe { &mut *(maxwell3d as *mut Maxwell3D) };
        maxwell3d.set_dirty_flag(dirty_flags::flags::RENDER_TARGETS);
        maxwell3d.set_dirty_flag(dirty_flags::flags::ZETA_BUFFER);
        for rt in 0..8 {
            maxwell3d.set_dirty_flag(dirty_flags::flags::COLOR_BUFFER0 + rt);
        }
    }

    /// Create a new texture cache.
    ///
    /// Port of `TextureCache<P>::TextureCache(Runtime&, MaxwellDeviceMemoryManager&)`.
    /// `device_memory` is the shared `Arc` from `Host1x::memory_manager()`.
    pub fn new_for_backend(
        device_memory: std::sync::Arc<
            crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager,
        >,
    ) -> Self {
        Self::new_with_caps_for_backend(device_memory, false, false)
    }

    pub fn new_with_caps_for_backend(
        device_memory: std::sync::Arc<
            crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager,
        >,
        has_broken_texture_view_formats: bool,
        has_native_bgr: bool,
    ) -> Self {
        let mut fallback_channel_state = TextureCacheChannelInfo::new();
        fallback_channel_state.gpu_page_table_index = Some(0);
        fallback_channel_state.sparse_page_table_index = Some(1);

        use common::settings_enums::{GpuUnswizzle, GpuUnswizzleChunk, GpuUnswizzleSize};

        let settings = common::settings::values();
        let (gpu_unswizzle_maxsize, swizzle_chunk_size, swizzle_slices_per_batch) =
            if *settings.gpu_unswizzle_enabled.get_value() {
                let max_size = match *settings.gpu_unswizzle_texture_size.get_value() {
                    GpuUnswizzleSize::VerySmall => 16 * 1024 * 1024,
                    GpuUnswizzleSize::Small => 32 * 1024 * 1024,
                    GpuUnswizzleSize::Normal => 128 * 1024 * 1024,
                    GpuUnswizzleSize::Large => 256 * 1024 * 1024,
                    GpuUnswizzleSize::VeryLarge => 512 * 1024 * 1024,
                };
                let chunk_size = match *settings.gpu_unswizzle_stream_size.get_value() {
                    GpuUnswizzle::VeryLow => 4 * 1024 * 1024,
                    GpuUnswizzle::Low => 8 * 1024 * 1024,
                    GpuUnswizzle::Normal => 16 * 1024 * 1024,
                    GpuUnswizzle::Medium => 32 * 1024 * 1024,
                    GpuUnswizzle::High => 64 * 1024 * 1024,
                };
                let slices = match *settings.gpu_unswizzle_chunk_size.get_value() {
                    GpuUnswizzleChunk::VeryLow => 32,
                    GpuUnswizzleChunk::Low => 64,
                    GpuUnswizzleChunk::Normal => 128,
                    GpuUnswizzleChunk::Medium => 256,
                    GpuUnswizzleChunk::High => 512,
                };
                (max_size, chunk_size, slices)
            } else {
                (0, 0, 0)
            };

        let mut cache = Self {
            slot_images: SlotVector::new(),
            slot_map_views: SlotVector::new(),
            slot_image_views: SlotVector::new(),
            slot_image_allocs: SlotVector::new(),
            slot_samplers: SlotVector::new(),
            slot_framebuffers: SlotVector::new(),
            slot_buffer_downloads: SlotVector::new(),
            uncommitted_async_buffers: Vec::new(),
            async_buffers: VecDeque::new(),
            async_buffers_death_ring: VecDeque::new(),
            render_targets: RenderTargets::default(),
            render_targets_serial: 0,
            rt_active_mask: 0,
            rt_image_id: [ImageId::default(); 8],
            rt_depth_image_id: ImageId::default(),
            texture_bindings_serial: 0,
            last_feedback_loop_serial: 0,
            last_feedback_texture_serial: 0,
            last_feedback_loop_result: false,
            last_framebuffer_id: FramebufferId::default(),
            last_framebuffer_serial: 0,
            framebuffers: HashMap::default(),
            page_table: HashMap::default(),
            sparse_views: HashMap::default(),
            has_deleted_images: false,
            is_rescaling: false,
            total_used_memory: 0,
            minimum_memory: 0,
            expected_memory: DEFAULT_EXPECTED_MEMORY as u64 + 512 * 1024 * 1024,
            critical_memory: DEFAULT_CRITICAL_MEMORY as u64 + 1024 * 1024 * 1024,
            lru_cache: LeastRecentlyUsedCache::new(),
            sentenced_images: DelayedDestructionRing::new(),
            sentenced_image_view: DelayedDestructionRing::new(),
            sentenced_framebuffers: DelayedDestructionRing::new(),
            has_broken_texture_view_formats,
            has_native_bgr,
            uncommitted_downloads: Vec::new(),
            committed_downloads: VecDeque::new(),
            modification_tick: 0,
            frame_tick: 0,
            sampler_heap_budget: None,
            last_sampler_gc_frame: u64::MAX,
            async_decodes: Vec::new(),
            texture_decode_worker: ThreadWorker::new_stateless_with_placement(
                1,
                "TextureDecoder".to_owned(),
                ThreadPlacement::Efficiency,
            ),
            gpu_unswizzle_maxsize,
            swizzle_chunk_size,
            swizzle_slices_per_batch,
            unswizzle_queue: VecDeque::new(),
            current_unswizzle_frame: 0,
            join_overlap_ids: SmallVec::new(),
            join_overlaps_found: HashSet::default(),
            join_left_aliased_ids: SmallVec::new(),
            join_right_aliased_ids: SmallVec::new(),
            join_ignore_textures: HashSet::default(),
            join_bad_overlap_ids: SmallVec::new(),
            join_copies_to_do: SmallVec::new(),
            join_alias_indices: HashMap::default(),
            image_allocs_table: HashMap::default(),
            virtual_invalid_space: 0,
            swizzle_data_buffer: ScratchBuffer::with_capacity(8 * 1024 * 1024),
            unswizzle_data_buffer: ScratchBuffer::with_capacity(1024 * 1024),
            image_downloader: None,
            guest_memory_writer: None,
            channel_gpu_memory: None,
            device_memory,
            gpu_page_table_storage: vec![
                TextureCacheGPUMap::default(),
                TextureCacheGPUMap::default(),
            ],
            image_gpu_page_table_indices: HashMap::default(),
            channel_caches: ChannelSetupCaches::new(),
            channel_state: fallback_channel_state,
            mutex: ReentrantMutex::new(()),
            runtime: None,
        };

        // Upstream reserves slot 0 for all null resources in
        // `TextureCache<P>::TextureCache`, making NULL_*_ID{0} compile-time
        // constants that are never returned for real resources.
        let null_image_id = cache
            .slot_images
            .insert(ImageBase::null(NullImageParams).into());
        debug_assert_eq!(null_image_id, crate::texture_cache::types::NULL_IMAGE_ID);
        let null_view_id = cache.slot_image_views.insert(ImageViewSlot::pending(
            ImageViewInfo::default(),
            ImageViewBase::null(NullImageViewParams),
        ));
        debug_assert_eq!(
            null_view_id,
            crate::texture_cache::types::NULL_IMAGE_VIEW_ID
        );

        // Ruzu's base stores raw `TSCEntry`s rather than backend `Sampler`s,
        // so reserve sampler id 0 with the upstream null sampler descriptor.
        let mut null_sampler = crate::textures::texture::TscEntry::default();
        let word1 = (crate::textures::texture::TextureFilter::Linear as u32)
            | ((crate::textures::texture::TextureFilter::Linear as u32) << 4)
            | ((crate::textures::texture::TextureMipmapFilter::Linear as u32) << 6)
            | (1 << 8);
        null_sampler.raw[0] = (word1 as u64) << 32;
        let null_id = cache.slot_samplers.insert(null_sampler.into());
        debug_assert_eq!(null_id, crate::texture_cache::types::NULL_SAMPLER_ID);

        cache
    }

    pub fn set_image_downloader(&mut self, downloader: ImageDownloader) {
        self.image_downloader = Some(downloader);
    }

    pub fn set_guest_memory_writer(&mut self, writer: GuestMemoryWriter) {
        self.guest_memory_writer = Some(writer);
    }

    /// Port of the `HAS_DEVICE_MEMORY_INFO` branch in
    /// `TextureCache<P>::TextureCache`.
    pub fn configure_device_memory_budget(&mut self, device_local_memory: u64) {
        let device_local_memory = device_local_memory as i64;
        let min_spacing_expected = device_local_memory - 1024 * 1024 * 1024;
        let min_spacing_critical = device_local_memory - 512 * 1024 * 1024;
        let mem_threshold = device_local_memory.min(TARGET_THRESHOLD);
        let min_vacancy_expected = (6 * mem_threshold) / 10;
        let min_vacancy_critical = (2 * mem_threshold) / 10;

        self.expected_memory = (device_local_memory - min_vacancy_expected)
            .min(min_spacing_expected)
            .max(DEFAULT_EXPECTED_MEMORY) as u64;
        self.critical_memory = (device_local_memory - min_vacancy_critical)
            .min(min_spacing_critical)
            .max(DEFAULT_CRITICAL_MEMORY) as u64;
        self.minimum_memory = ((device_local_memory - mem_threshold) / 2) as u64;
    }

    pub fn update_total_used_memory_from_runtime(&mut self, device_memory_usage: u64) {
        self.total_used_memory = device_memory_usage;
    }

    pub fn set_sampler_heap_budget(&mut self, budget: Option<usize>) {
        self.sampler_heap_budget = budget;
    }

    /// Notify the cache that a new frame has been queued.
    ///
    /// Port of `TextureCache<P>::TickFrame`.
    ///
    /// The OpenGL wrapper calls `tick_delayed_destruction_rings`,
    /// backend framebuffer rings, async decode, and runtime tick before
    /// this method so the frame counter advances in upstream order.
    pub fn tick_frame(&mut self) {
        self.frame_tick = self.frame_tick.wrapping_add(1);
    }

    pub fn tick_delayed_destruction_rings(&mut self) {
        self.sentenced_images.tick();
        self.sentenced_framebuffers.tick();
        self.sentenced_image_view.tick();
    }

    /// Collect every `ImageId` whose registered GPU pages overlap the given
    /// region. Mirrors upstream `ForEachImageInRegionGPU` /
    /// `ForEachSparseImageInRegion`; the caller selects which per-channel GPU
    /// page table to inspect.
    pub fn collect_images_in_gpu_region(
        &mut self,
        gpu_addr: GPUVAddr,
        size: usize,
        sparse: bool,
    ) -> SmallVec<[ImageId; 8]> {
        let Some(table_index) = self.current_gpu_page_table_index(sparse) else {
            return SmallVec::new();
        };
        let Some(table) = self.gpu_page_table_storage.get(table_index) else {
            return SmallVec::new();
        };

        let mut image_ids = SmallVec::new();
        let slot_images = &mut self.slot_images;
        Self::for_each_gpu_page(gpu_addr, size, |page| {
            if let Some(ids) = table.get(&page) {
                for &image_id in ids {
                    let image = &mut slot_images[image_id];
                    if image.flags.contains(ImageFlagBits::PICKED)
                        || !image.overlaps_gpu(gpu_addr, size)
                    {
                        continue;
                    }
                    image.flags.insert(ImageFlagBits::PICKED);
                    image_ids.push(image_id);
                }
            }
        });
        for &image_id in &image_ids {
            slot_images[image_id].flags.remove(ImageFlagBits::PICKED);
        }
        image_ids
    }

    /// Return true when there are uncommitted images to be downloaded.
    pub fn has_uncommitted_flushes(&self) -> bool {
        !self.uncommitted_downloads.is_empty()
    }

    /// Return true when the caller should wait for async downloads.
    pub fn should_wait_async_flushes(&self) -> bool {
        self.committed_downloads
            .front()
            .is_some_and(|downloads| !downloads.is_empty())
    }

    /// Commit asynchronous downloads.
    ///
    /// Port of `TextureCache<P>::CommitAsyncFlushes`.
    ///
    /// Moves uncommitted downloads into the committed queue for later
    /// completion and readback.
    pub fn commit_async_flushes(&mut self) {
        self.committed_downloads
            .push_back(std::mem::take(&mut self.uncommitted_downloads));
    }

    /// Pop asynchronous downloads.
    ///
    /// Port of `TextureCache<P>::PopAsyncFlushes`.
    ///
    /// Completes the oldest committed download batch, reading back pixel data
    /// to guest memory.
    pub fn pop_async_flushes(&mut self) {
        if let Some(_batch) = self.committed_downloads.pop_front() {
            // In full implementation: for each download in batch,
            // read back the staging buffer and copy to guest memory.
        }
    }

    // ── Page iteration helpers ─────────────────────────────────────────

    /// Iterate over all page indices in a CPU address range.
    pub fn for_each_cpu_page(addr: u64, size: usize, mut func: impl FnMut(u64)) {
        Self::for_each_cpu_page_until(addr, size, |page| {
            func(page);
            false
        });
    }

    /// Bool-returning specialization of upstream `ForEachCPUPage`.
    pub(super) fn for_each_cpu_page_until(
        addr: u64,
        size: usize,
        mut func: impl FnMut(u64) -> bool,
    ) -> bool {
        let page_end = addr.wrapping_add(size as u64).wrapping_sub(1) >> YUZU_PAGEBITS;
        let mut page = addr >> YUZU_PAGEBITS;
        while page <= page_end {
            if func(page) {
                return true;
            }
            page += 1;
        }
        false
    }

    /// Iterate over all page indices in a GPU address range.
    pub fn for_each_gpu_page(addr: GPUVAddr, size: usize, mut func: impl FnMut(u64)) {
        Self::for_each_gpu_page_until(addr, size, |page| {
            func(page);
            false
        });
    }

    /// Bool-returning specialization of upstream `ForEachGPUPage`.
    pub(super) fn for_each_gpu_page_until(
        addr: GPUVAddr,
        size: usize,
        mut func: impl FnMut(u64) -> bool,
    ) -> bool {
        let page_end = addr.wrapping_add(size as u64).wrapping_sub(1) >> YUZU_PAGEBITS;
        let mut page = addr >> YUZU_PAGEBITS;
        while page <= page_end {
            if func(page) {
                return true;
            }
            page += 1;
        }
        false
    }
}

impl TextureCacheBase<CommonTextureCacheParams> {
    pub fn new(
        device_memory: std::sync::Arc<
            crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager,
        >,
    ) -> Self {
        Self::new_for_backend(device_memory)
    }

    pub fn new_with_caps(
        device_memory: std::sync::Arc<
            crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager,
        >,
        has_broken_texture_view_formats: bool,
        has_native_bgr: bool,
    ) -> Self {
        Self::new_with_caps_for_backend(
            device_memory,
            has_broken_texture_view_formats,
            has_native_bgr,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CommonTextureCacheParams, ImageSlot, ImageViewSlot, TextureCacheBase, TextureCacheGPUMap,
        TextureCacheParams, TICKS_TO_DESTROY,
    };
    use crate::framebuffer_config::FramebufferConfig;
    use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
    use crate::surface::PixelFormat;
    use crate::texture_cache::image_base::ImageBase;
    use crate::texture_cache::image_info::ImageInfo;
    use crate::texture_cache::image_view_base::ImageViewBase;
    use crate::texture_cache::image_view_info::ImageViewInfo;
    use crate::texture_cache::render_targets::RenderTargets;
    use crate::texture_cache::types::{
        Extent3D, ImageId, ImageType, ImageViewType, SubresourceRange, NUM_RT,
    };

    struct DropProbe(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    struct TypedSlotTestParams;

    impl TextureCacheParams for TypedSlotTestParams {
        type Runtime = ();
        type Image = DropProbe;
        type ImageAlloc = ();
        type ImageView = ();
        type Sampler = DropProbe;
        type Framebuffer = ();
        type FramebufferError = std::convert::Infallible;
        type AsyncBuffer = DropProbe;
        type BufferType = ();

        const ENABLE_VALIDATION: bool = true;
        const FRAMEBUFFER_BLITS: bool = false;
        const HAS_EMULATED_COPIES: bool = false;
        const HAS_DEVICE_MEMORY_INFO: bool = false;
        const IMPLEMENTS_ASYNC_DOWNLOADS: bool = false;

        fn create_image(
            _: Option<&mut ()>,
            _: ImageId,
            _: std::ptr::NonNull<ImageBase>,
        ) -> DropProbe {
            unreachable!("typed-slot destruction test inserts its payload explicitly")
        }

        fn set_image_allocation_tick(_: &mut DropProbe, _: u64) {}

        fn create_image_view(
            _: Option<&mut ()>,
            _: crate::texture_cache::types::ImageViewId,
            _: &ImageViewInfo,
            _: std::ptr::NonNull<ImageViewBase>,
            _: Option<&DropProbe>,
        ) {
        }

        fn create_sampler(_: Option<&mut ()>, _: &crate::textures::texture::TscEntry) -> DropProbe {
            unreachable!("typed-slot destruction test inserts its payload explicitly")
        }

        fn create_framebuffer(
            _: Option<&mut ()>,
            _: [Option<std::ptr::NonNull<()>>; NUM_RT],
            _: Option<std::ptr::NonNull<()>>,
            _: &RenderTargets,
        ) -> Result<(), std::convert::Infallible> {
            Ok(())
        }

        fn prepare_image_view(
            _: &mut TextureCacheBase<Self>,
            _: crate::texture_cache::types::ImageViewId,
            _: bool,
            _: bool,
        ) {
        }

        fn scale_up_image(_: &mut TextureCacheBase<Self>, _: ImageId, _: bool) -> bool {
            false
        }

        fn scale_down_image(_: &mut TextureCacheBase<Self>, _: ImageId, _: bool) -> bool {
            false
        }

        fn upload_staging_buffer(_: &mut TextureCacheBase<Self>, _: usize, _: bool) -> DropProbe {
            unreachable!("typed-slot destruction test does not upload images")
        }

        fn staging_mapped_span(_: &mut DropProbe) -> &mut [u8] {
            unreachable!("typed-slot destruction test does not map staging buffers")
        }

        fn free_deferred_staging_buffer(_: &mut TextureCacheBase<Self>, _: &mut DropProbe) {}

        fn can_upload_msaa(_: &TextureCacheBase<Self>) -> bool {
            true
        }

        fn transition_image_layout(_: &mut TextureCacheBase<Self>, _: ImageId) {}

        fn upload_image(
            _: &mut TextureCacheBase<Self>,
            _: ImageId,
            _: &DropProbe,
            _: &[crate::texture_cache::types::BufferImageCopy],
        ) {
        }

        fn accelerate_image_upload(
            _: &mut TextureCacheBase<Self>,
            _: ImageId,
            _: &DropProbe,
            _: &[crate::texture_cache::types::SwizzleParameters],
            _: u32,
            _: u32,
        ) {
        }

        fn insert_upload_memory_barrier(_: &mut TextureCacheBase<Self>) {}

        fn copy_image(
            _: &mut TextureCacheBase<Self>,
            _: ImageId,
            _: ImageId,
            _: &[crate::texture_cache::types::ImageCopy],
        ) {
        }

        fn copy_image_msaa(
            _: &mut TextureCacheBase<Self>,
            _: ImageId,
            _: ImageId,
            _: &[crate::texture_cache::types::ImageCopy],
        ) {
        }
    }

    #[test]
    fn typed_backend_payload_follows_upstream_slot_destruction_lifecycle() {
        use std::sync::atomic::Ordering;

        let drops = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut cache = TextureCacheBase::<TypedSlotTestParams>::new_for_backend(
            std::sync::Arc::new(MaxwellDeviceMemoryManager::default()),
        );
        let info = ImageInfo {
            format: PixelFormat::A8B8G8R8Unorm,
            size: Extent3D {
                width: 1,
                height: 1,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        let image_id = cache.slot_images.insert(ImageSlot {
            base: Box::new(ImageBase::new(info, 0x1000, 0x2000)),
            backend: Some(DropProbe(drops.clone())),
        });
        let image = cache.slot_images.take(image_id);
        cache.sentenced_images.push(image);

        for _ in 1..TICKS_TO_DESTROY {
            cache.tick_delayed_destruction_rings();
            assert_eq!(drops.load(Ordering::SeqCst), 0);
        }
        cache.tick_delayed_destruction_rings();
        assert_eq!(drops.load(Ordering::SeqCst), 1);

        let sampler_id = cache.slot_samplers.insert(super::SamplerSlot {
            config: crate::textures::texture::TscEntry::default(),
            backend: Some(DropProbe(drops.clone())),
        });
        cache.slot_samplers.erase(sampler_id);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn generic_cache_owns_async_buffers_and_pending_unswizzle_staging() {
        use std::sync::atomic::Ordering;

        let drops = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut cache = TextureCacheBase::<TypedSlotTestParams>::new_for_backend(
            std::sync::Arc::new(MaxwellDeviceMemoryManager::default()),
        );
        cache
            .uncommitted_async_buffers
            .push(DropProbe(drops.clone()));
        let mut task = super::PendingUnswizzle::new(ImageId::default(), ImageInfo::default());
        task.staging_buffer = Some(DropProbe(drops.clone()));
        cache.unswizzle_queue.push_back(task);

        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(cache);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn common_backend_staging_buffer_exposes_the_requested_mapped_span() {
        let mut cache = TextureCacheBase::<CommonTextureCacheParams>::new(std::sync::Arc::new(
            MaxwellDeviceMemoryManager::default(),
        ));
        let mut staging = CommonTextureCacheParams::upload_staging_buffer(&mut cache, 37, false);

        assert_eq!(
            CommonTextureCacheParams::staging_mapped_span(&mut staging).len(),
            37
        );
    }

    use std::hash::{BuildHasher, Hash, Hasher};
    use std::sync::Arc;

    fn finish_hash(build_hasher: &impl BuildHasher, value: u64) -> u64 {
        let mut hasher = build_hasher.build_hasher();
        value.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn texture_page_tables_use_upstream_unordered_dense_post_mix() {
        let key = 0x1234_5678_9abc_def0;
        let expected = 0xc27a_443d_5ff2_18e0;
        let gpu_page_table = TextureCacheGPUMap::default();
        assert_eq!(finish_hash(gpu_page_table.hasher(), key), expected);

        let cache = TextureCacheBase::new(Arc::new(MaxwellDeviceMemoryManager::default()));
        assert_eq!(finish_hash(cache.page_table.hasher(), key), expected);
    }

    #[test]
    fn page_iteration_preserves_upstream_unsigned_range_wraparound() {
        let mut cpu_visits = 0;
        TextureCacheBase::<CommonTextureCacheParams>::for_each_cpu_page(u64::MAX - 3, 8, |_| {
            cpu_visits += 1
        });
        assert_eq!(cpu_visits, 0);

        let mut gpu_visits = 0;
        TextureCacheBase::<CommonTextureCacheParams>::for_each_gpu_page(u64::MAX - 3, 8, |_| {
            gpu_visits += 1
        });
        assert_eq!(gpu_visits, 0);
    }

    #[test]
    fn texture_cache_mutex_is_reentrant() {
        let cache = TextureCacheBase::new(Arc::new(MaxwellDeviceMemoryManager::default()));
        let _lock_a = cache.mutex.lock();
        let _lock_b = cache.mutex.lock();
    }

    #[test]
    fn erasing_an_unbound_channel_preserves_bound_gpu_memory() {
        use crate::control::channel_state::ChannelState;
        use crate::memory_manager::MemoryManager;
        use parking_lot::Mutex;

        let mut cache = TextureCacheBase::new(Arc::new(MaxwellDeviceMemoryManager::default()));
        let first_memory = Arc::new(Mutex::new(MemoryManager::new(41)));
        let second_memory = Arc::new(Mutex::new(MemoryManager::new(42)));
        let mut first = ChannelState::new(1);
        first.memory_manager = Some(Arc::clone(&first_memory));
        let mut second = ChannelState::new(2);
        second.memory_manager = Some(second_memory);

        cache.create_channel(&first);
        cache.create_channel(&second);
        cache.bind_to_channel(1);
        cache.erase_channel(2);
        assert_eq!(
            cache.channel_gpu_memory.as_ref().unwrap().lock().get_id(),
            41
        );

        cache.erase_channel(1);
        assert!(cache.channel_gpu_memory.is_none());
    }

    #[test]
    fn empty_committed_download_batch_does_not_require_wait() {
        let mut cache = TextureCacheBase::new(Arc::new(MaxwellDeviceMemoryManager::default()));

        cache.commit_async_flushes();

        assert!(!cache.should_wait_async_flushes());
    }

    #[test]
    fn configure_device_memory_budget_matches_upstream_formula() {
        let mut cache = TextureCacheBase::new(Arc::new(MaxwellDeviceMemoryManager::default()));
        cache.configure_device_memory_budget(8 * 1024 * 1024 * 1024);

        assert_eq!(cache.minimum_memory, 2 * 1024 * 1024 * 1024);
        assert_eq!(cache.expected_memory, 6_012_954_215);
        assert_eq!(cache.critical_memory, 7_730_941_133);
    }

    #[test]
    fn cache_scratch_and_inline_containers_match_upstream_owners() {
        fn assert_common_worker(_: &common::thread_worker::ThreadWorker) {}

        let cache = TextureCacheBase::new(Arc::new(MaxwellDeviceMemoryManager::default()));
        assert_eq!(cache.swizzle_data_buffer.size(), 8 * 1024 * 1024);
        assert_eq!(cache.unswizzle_data_buffer.size(), 1024 * 1024);
        assert!(cache.join_overlap_ids.capacity() >= 4);
        assert!(cache.join_left_aliased_ids.capacity() >= 4);
        assert!(cache.join_right_aliased_ids.capacity() >= 4);
        assert!(cache.join_bad_overlap_ids.capacity() >= 4);
        assert!(cache.join_copies_to_do.capacity() >= 4);
        assert_common_worker(&cache.texture_decode_worker);

        let decode = super::AsyncDecodeContext::new(ImageId::default());
        let output = decode.output.lock().unwrap();
        assert!(output.copies.capacity() >= 16);
    }

    #[test]
    fn framebuffer_lookup_uses_most_recent_image_for_shared_cpu_address() {
        fn insert_presentable_image(
            cache: &mut TextureCacheBase,
            gpu_addr: u64,
            cpu_addr: u64,
            width: u32,
            height: u32,
            modification_tick: u64,
        ) -> ImageId {
            let info = ImageInfo {
                format: PixelFormat::A8B8G8R8Unorm,
                image_type: ImageType::E2D,
                size: Extent3D {
                    width,
                    height,
                    depth: 1,
                },
                ..ImageInfo::default()
            };
            let mut image = ImageBase::new(info.clone(), gpu_addr, cpu_addr);
            image.modification_tick = modification_tick;
            let image_id = cache.slot_images.insert(image.into());
            let view_info = ImageViewInfo::for_render_target(
                ImageViewType::E2D,
                PixelFormat::A8B8G8R8Unorm,
                SubresourceRange::default(),
            );
            let view = ImageViewBase::new(&view_info, &info, image_id, gpu_addr);
            let view_id = cache
                .slot_image_views
                .insert(ImageViewSlot::pending(view_info, view));
            cache.slot_images[image_id].insert_view(view_info, view_id);
            cache.register_image(image_id);
            image_id
        }

        let mut cache = TextureCacheBase::new(Arc::new(MaxwellDeviceMemoryManager::default()));
        let cpu_addr = 0x2b71_e000;
        let old_id = insert_presentable_image(&mut cache, 0x5205_1000, cpu_addr, 1920, 1080, 10);
        let recent_id = insert_presentable_image(&mut cache, 0x5205_1000, cpu_addr, 480, 270, 20);

        let selected = cache
            .try_find_framebuffer_image_view(&FramebufferConfig::default(), cpu_addr)
            .expect("a registered framebuffer image must be found");

        assert_eq!(selected.view.image_id, recent_id);
        assert_ne!(selected.view.image_id, old_id);
    }
}
