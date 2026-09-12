// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Metal ownership counterpart of Eden's `vk_texture_cache.cpp` runtime.

use std::ptr::NonNull;
use std::sync::Arc;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;

use objc2_metal::{
    MTLBlitCommandEncoder, MTLLoadAction, MTLOrigin, MTLRenderPassDescriptor, MTLSize,
    MTLStoreAction, MTLDevice, MTLHeap, MTLHeapDescriptor, MTLHazardTrackingMode,
    MTLStorageMode, MTLTexture, MTLResource,
};
use thiserror::Error;

use crate::buffer_cache::buffer_cache_base::BufferCacheAsyncBuffer;
use crate::engines::fermi_2d::{Filter, Operation};
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use crate::surface::{get_format_type, PixelFormat, SurfaceType};
use crate::texture_cache::image_base::ImageBase;
use crate::texture_cache::image_info::ImageInfo;
use crate::texture_cache::image_view_base::ImageViewBase;
use crate::texture_cache::image_view_info::ImageViewInfo;
use crate::texture_cache::render_targets::RenderTargets;
use crate::texture_cache::texture_cache_base::{
    DescriptorSyncRegs, ImageViewInOut, TextureCacheBase as CommonTextureCache, TextureCacheParams,
};
use crate::texture_cache::types::{
    BufferImageCopy, Extent2D, Extent3D, FramebufferId, ImageCopy, ImageId, ImageType, ImageViewId,
    ImageViewType, Region2D, SamplerId, SubresourceBase, SubresourceExtent, SubresourceRange,
    NULL_IMAGE_ID, NULL_IMAGE_VIEW_ID, NULL_SAMPLER_ID, NUM_RT,
};
use shader_recompiler::shader_info::TextureType;

use super::metal_blit_helper::{
    MetalBlitError, MetalBlitHelper, MetalBlitRegion, MetalDepthStencilBufferCopy,
    MetalDepthStencilCopy,
};
use super::metal_buffer::{MetalBuffer, MetalBufferError};
use super::metal_device::MetalDevice;
use super::metal_framebuffer::{MetalFramebuffer, MetalFramebufferError};
use super::metal_image::{MetalImage, MetalImageError};
use super::metal_image_view::{MetalImageView, MetalImageViewError};
use super::metal_sampler::MetalSampler;
use super::metal_scheduler::{MetalScheduler, MetalSchedulerError};
use super::metal_staging_buffer_pool::{
    MetalStagingBufferError, MetalStagingBufferPool, StagingBufferRef,
};

#[derive(Debug, Error)]
pub enum MetalTextureCacheError {
    #[error(transparent)]
    Blit(#[from] MetalBlitError),
    #[error(transparent)]
    Scheduler(#[from] MetalSchedulerError),
    #[error(transparent)]
    Staging(#[from] MetalStagingBufferError),
    #[error(transparent)]
    Buffer(#[from] MetalBufferError),
    #[error(transparent)]
    Image(#[from] MetalImageError),
    #[error(transparent)]
    ImageView(#[from] MetalImageViewError),
    #[error(transparent)]
    Framebuffer(#[from] MetalFramebufferError),
    #[error("Metal image copy requires byte-compatible formats")]
    IncompatibleFormats,
    #[error("native Metal image copy does not support multisample textures")]
    MultisampleCopyRequiresShader,
    #[error("native Metal multisample resolve requires a color MSAA source and single-sample destination")]
    InvalidMultisampleResolve,
    #[error("invalid Metal image copy: {0}")]
    InvalidCopy(&'static str),
}

// Native-only optimization budget. Allocation failure keeps the ordinary path.
const SAMPLING_SNAPSHOT_HEAP_BYTES: usize = 64 * 1024 * 1024;
const SAMPLING_SNAPSHOT_VIEW_LIMIT: usize = 64;
const MAX_UNUSED_MSAA_SCRATCH_FRAMES: u32 = 60;

/// Pooled scratch identity for MSAA downloads. Eden has no Metal backend;
/// Vulkan `TextureCacheRuntime::MsaaScratchKey` is the cache-contract reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MsaaScratchKey {
    format: PixelFormat,
    image_type: ImageType,
    width: u32,
    height: u32,
    depth: u32,
    levels: u32,
    layers: u32,
}

impl MsaaScratchKey {
    fn from_info(info: &ImageInfo) -> Self {
        Self {
            format: info.format,
            image_type: info.image_type,
            width: info.size.width.max(1),
            height: info.size.height.max(1),
            depth: info.size.depth.max(1),
            levels: info.resources.levels.max(1) as u32,
            layers: info.resources.layers.max(1) as u32,
        }
    }
}

/// Pooled single-sample scratch used by MSAA downloads. Eden has no Metal
/// backend; Vulkan `TextureCacheRuntime::MsaaScratchImage` is the reference.
struct MsaaScratchImage {
    key: MsaaScratchKey,
    image: MetalImage,
    tick: u64,
    unused_frames: u32,
}

struct SamplingSnapshot {
    // Holding the source prevents pointer reuse from aliasing an old cache key.
    _source: Retained<ProtocolObject<dyn MTLTexture>>,
    revision: u64,
    image: Arc<MetalImage>,
}

#[derive(Default)]
struct SamplingSnapshotCache {
    heap: Option<Retained<ProtocolObject<dyn MTLHeap>>>,
    attempted_allocation: bool,
    entries: HashMap<usize, SamplingSnapshot>,
    // Both handles are retained: neither half of the pointer key can be recycled.
    views: HashMap<(usize, usize), (
        Retained<ProtocolObject<dyn MTLTexture>>,
        Retained<ProtocolObject<dyn MTLTexture>>,
    )>,
}

impl SamplingSnapshotCache {
    fn get(
        &mut self,
        device: &MetalDevice,
        scheduler: &mut MetalScheduler,
        source: &MetalImage,
    ) -> Result<Option<Arc<MetalImage>>, MetalImageError> {
        if !source.supports_sampling_snapshot() {
            return Ok(None);
        }
        let Some(revision) = source.content_revision() else {
            return Ok(None);
        };
        let key = source.handle() as *const _ as usize;
        if let Some(entry) = self.entries.get(&key) {
            if entry.revision == revision {
                return Ok(Some(Arc::clone(&entry.image)));
            }
        }
        self.entries.remove(&key);
        if !self.attempted_allocation {
            self.attempted_allocation = true;
            let descriptor = MTLHeapDescriptor::new();
            descriptor.setSize(SAMPLING_SNAPSHOT_HEAP_BYTES);
            descriptor.setStorageMode(MTLStorageMode::Private);
            // Heap defaults are untracked, unlike ordinary texture allocations.
            descriptor.setHazardTrackingMode(MTLHazardTrackingMode::Tracked);
            self.heap = device.device().newHeapWithDescriptor(&descriptor);
        }
        let Some(heap) = self.heap.as_ref() else {
            return Ok(None);
        };
        let snapshot = match source.create_sampling_snapshot_in_heap(device, scheduler, heap) {
            Ok(snapshot) => snapshot,
            Err(MetalImageError::AllocationFailed { .. }) => {
                // Eviction cannot reclaim textures still retained by GPU batches.
                // Never mark them aliasable or wait for space: fall back if full.
                self.views.clear();
                self.entries.clear();
                match source.create_sampling_snapshot_in_heap(device, scheduler, heap) {
                    Ok(snapshot) => snapshot,
                    Err(MetalImageError::AllocationFailed { .. }) => return Ok(None),
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        };
        let image = Arc::new(snapshot);
        self.entries.insert(key, SamplingSnapshot {
            _source: source.retained_handle(), revision, image: Arc::clone(&image),
        });
        Ok(Some(image))
    }
}

pub struct MetalTextureCacheRuntime {
    device: MetalDevice,
    scheduler: NonNull<MetalScheduler>,
    staging_buffer_pool: NonNull<MetalStagingBufferPool>,
    blit_image_helper: NonNull<MetalBlitHelper>,
    depth_stencil_copy: Option<MetalDepthStencilCopy>,
    sampling_snapshots: SamplingSnapshotCache,
    msaa_scratch_images: Vec<MsaaScratchImage>,
    memory_profile_last_report: Option<Instant>,
}

impl MetalTextureCacheRuntime {
    pub fn new(
        device: MetalDevice,
        scheduler: &mut MetalScheduler,
        staging_buffer_pool: &mut MetalStagingBufferPool,
        blit_image_helper: &mut MetalBlitHelper,
    ) -> Self {
        Self {
            device,
            scheduler: NonNull::from(scheduler),
            staging_buffer_pool: NonNull::from(staging_buffer_pool),
            blit_image_helper: NonNull::from(blit_image_helper),
            depth_stencil_copy: None,
            sampling_snapshots: SamplingSnapshotCache::default(),
            msaa_scratch_images: Vec::new(),
            memory_profile_last_report: std::env::var_os("RUZU_PROFILE_METAL_SUBMISSIONS")
                .is_some().then(Instant::now),
        }
    }

    pub fn device(&self) -> &MetalDevice {
        &self.device
    }

    pub fn sampling_snapshot(
        &mut self,
        source: &MetalImage,
    ) -> Result<Option<Arc<MetalImage>>, MetalImageError> {
        self.sampling_snapshots.get(&self.device, unsafe { self.scheduler.as_mut() }, source)
    }

    pub fn scheduler(&mut self) -> &mut MetalScheduler {
        unsafe { self.scheduler.as_mut() }
    }

    pub fn staging_buffer_pool(&mut self) -> &mut MetalStagingBufferPool {
        unsafe { self.staging_buffer_pool.as_mut() }
    }

    pub fn finish(&mut self) -> Result<(), MetalTextureCacheError> {
        self.scheduler().finish_all()?;
        Ok(())
    }

    pub fn upload_staging_buffer(
        &mut self,
        size: usize,
        deferred: bool,
    ) -> Result<StagingBufferRef, MetalTextureCacheError> {
        let scheduler = unsafe { self.scheduler.as_mut() };
        let pool = unsafe { self.staging_buffer_pool.as_mut() };
        Ok(pool.request_upload_buffer(scheduler, size, deferred)?)
    }

    pub fn free_deferred_staging_buffer(
        &mut self,
        buffer: &mut StagingBufferRef,
    ) -> Result<(), MetalTextureCacheError> {
        let scheduler = unsafe { self.scheduler.as_ref() };
        let pool = unsafe { self.staging_buffer_pool.as_mut() };
        pool.free_deferred(scheduler, buffer)?;
        Ok(())
    }

    /// Native counterpart of TextureCacheRuntime::DownloadStagingBuffer.
    pub fn download_staging_buffer(
        &mut self,
        size: usize,
        deferred: bool,
    ) -> Result<StagingBufferRef, MetalTextureCacheError> {
        let scheduler = unsafe { self.scheduler.as_mut() };
        let pool = unsafe { self.staging_buffer_pool.as_mut() };
        Ok(pool.request_download_buffer(scheduler, size, deferred)?)
    }

    /// CPU writeback portion of Image::DownloadMemory for Metal's converted
    /// D24S8 storage. Completion is required only because this call reads bytes.
    pub fn download_depth24_stencil8_memory(
        &mut self,
        image: &MetalImage,
        output: &mut [u8],
        copies: &[BufferImageCopy],
    ) -> Result<(), MetalTextureCacheError> {
        let planes = converted_depth_stencil_copies(copies);
        let buffer = self.download_staging_buffer(converted_depth_stencil_linear_size(copies), false)?;
        image.download_depth_stencil_memory(self.scheduler(), &buffer.buffer, buffer.offset, &planes.depth, &planes.stencil)?;
        self.finish()?;
        convert_depth24_stencil8_download(image.guest_format(), buffer.mapped_span(), output, copies)?;
        Ok(())
    }

    /// Inverse of the packed color upload conversion. Only copied texels
    /// overwrite the guest output; native row/layer padding is unspecified.
    pub fn download_packed16_memory(
        &mut self,
        image: &MetalImage,
        output: &mut [u8],
        copies: &[BufferImageCopy],
    ) -> Result<(), MetalTextureCacheError> {
        let buffer = self.download_staging_buffer(output.len(), false)?;
        image.download_packed16_memory(self.scheduler(), &buffer.buffer, buffer.offset, copies)?;
        self.finish()?;
        convert_packed16_download(image.guest_format(), buffer.mapped_span(), output, copies)?;
        Ok(())
    }

    pub fn tick_frame(&mut self) -> Result<(), MetalTextureCacheError> {
        let scheduler = unsafe { self.scheduler.as_mut() };
        let pool = unsafe { self.staging_buffer_pool.as_mut() };
        pool.tick_frame(scheduler)?;
        self.msaa_scratch_images.retain_mut(|scratch| {
            if !scheduler.is_free(scratch.tick).unwrap_or(false) {
                scratch.unused_frames = 0;
                return true;
            }
            scratch.unused_frames += 1;
            scratch.unused_frames <= MAX_UNUSED_MSAA_SCRATCH_FRAMES
        });
        Ok(())
    }

    /// Common-cache `Runtime::CanDownloadMsaa`. Eden has no Metal backend;
    /// Vulkan's aspect rules are the reference. Metal only expands float color
    /// through `CopyMSAA` into a single-sample scratch image.
    pub fn can_download_msaa(&self, info: &ImageInfo) -> bool {
        can_download_msaa_info(info, self.device.profile().best_supported_sample_count(info.num_samples))
    }

    /// CPU-facing download. MSAA images go through a single-sample scratch
    /// expansion (`CopyMSAA`) before the ordinary buffer readback.
    pub fn download_single_sample_memory(
        &mut self,
        image: &MetalImage,
        output: &mut [u8],
        copies: &[BufferImageCopy],
    ) -> Result<(), MetalTextureCacheError> {
        if image.guest_samples() > 1 || image.samples() > 1 {
            return self.download_msaa_memory(image, output, copies);
        }
        self.download_resolved_memory(image, output, copies)
    }

    fn download_resolved_memory(
        &mut self,
        image: &MetalImage,
        output: &mut [u8],
        copies: &[BufferImageCopy],
    ) -> Result<(), MetalTextureCacheError> {
        match image.guest_format() {
            PixelFormat::D24UnormS8Uint | PixelFormat::S8UintD24Unorm => {
                return self.download_depth24_stencil8_memory(image, output, copies);
            }
            PixelFormat::B5G6R5Unorm | PixelFormat::A1B5G5R5Unorm => {
                return self.download_packed16_memory(image, output, copies);
            }
            format @ (PixelFormat::R32G32B32Float | PixelFormat::G4R4Unorm | PixelFormat::X8D24Unorm) => {
                let (_, native_bytes) = converted_texel_bytes(format)?;
                let (native_copies, size) = converted_copy_layout(copies, native_bytes)?;
                let buffer = self.download_staging_buffer(size, false)?;
                image.download_converted_memory(self.scheduler(), &buffer.buffer, buffer.offset, &native_copies)?;
                self.finish()?;
                convert_uncompressed_memory(format, buffer.mapped_span(), output, copies, false)?;
                return Ok(());
            }
            _ => {}
        }
        let mut buffer = self.download_staging_buffer(output.len(), false)?;
        // Native blits/compute kernels write only the specified texels, not
        // padding or gaps. Seed those bytes before copying the result back.
        buffer.mapped_span_mut().copy_from_slice(output);
        if image.guest_format() == PixelFormat::D32FloatS8Uint {
            self.transfer_depth32_stencil8_memory(image, &buffer.buffer, buffer.offset, copies, false)?;
        } else {
            image.download_memory(self.scheduler(), &buffer.buffer, buffer.offset, copies)?;
        }
        self.finish()?;
        output.copy_from_slice(buffer.mapped_span());
        Ok(())
    }

    fn download_msaa_memory(
        &mut self,
        image: &MetalImage,
        output: &mut [u8],
        copies: &[BufferImageCopy],
    ) -> Result<(), MetalTextureCacheError> {
        let source_info = guest_image_info(image);
        if !self.can_download_msaa(&source_info) {
            return Err(MetalImageError::InvalidCopy(
                "download requires a resolved single-sample image",
            )
            .into());
        }
        let scratch_info = msaa_scratch_info(&source_info);
        let key = MsaaScratchKey::from_info(&scratch_info);
        let tick = self.scheduler().current_tick();
        let scratch = self.acquire_msaa_scratch_image(&scratch_info)?;
        let result = self
            .copy_msaa_color_to_scratch(image, &scratch, copies)
            .and_then(|()| self.download_resolved_memory(&scratch, output, copies));
        self.release_msaa_scratch_image(key, scratch, tick);
        result
    }

    fn acquire_msaa_scratch_image(
        &mut self,
        info: &ImageInfo,
    ) -> Result<MetalImage, MetalTextureCacheError> {
        let key = MsaaScratchKey::from_info(info);
        let scheduler = unsafe { self.scheduler.as_mut() };
        let index = self.msaa_scratch_images.iter().position(|scratch| {
            scratch.key == key && scheduler.is_free(scratch.tick).unwrap_or(false)
        });
        if let Some(index) = index {
            return Ok(self.msaa_scratch_images.swap_remove(index).image);
        }
        Ok(MetalImage::new(&self.device, info)?)
    }

    fn release_msaa_scratch_image(&mut self, key: MsaaScratchKey, image: MetalImage, tick: u64) {
        self.msaa_scratch_images.push(MsaaScratchImage {
            key,
            image,
            tick,
            unused_frames: 0,
        });
    }

    fn copy_msaa_color_to_scratch(
        &mut self,
        source: &MetalImage,
        destination: &MetalImage,
        copies: &[BufferImageCopy],
    ) -> Result<(), MetalTextureCacheError> {
        let source_info = guest_image_info(source);
        let destination_info = guest_image_info(destination);
        for copy in copies {
            if copy.image_offset.z != 0 || copy.image_extent.depth > 1 {
                return Err(MetalTextureCacheError::InvalidCopy(
                    "MSAA download copies must be 2D",
                ));
            }
            let layers = copy.image_subresource.num_layers.max(1);
            for layer in 0..layers {
                let range = SubresourceRange {
                    base: SubresourceBase {
                        level: copy.image_subresource.base_level,
                        layer: copy.image_subresource.base_layer.saturating_add(layer),
                    },
                    extent: SubresourceExtent {
                        levels: 1,
                        layers: 1,
                    },
                };
                let view_info = ImageViewInfo::for_render_target(
                    ImageViewType::E2D,
                    source.guest_format(),
                    range,
                );
                let mut source_base = Box::new(ImageViewBase::new(
                    &view_info,
                    &source_info,
                    ImageId { index: 1 },
                    0,
                ));
                let mut destination_base = Box::new(ImageViewBase::new(
                    &view_info,
                    &destination_info,
                    ImageId { index: 2 },
                    0,
                ));
                let source_view = MetalImageView::new(
                    NonNull::from(source_base.as_mut()),
                    &view_info,
                    source,
                )?;
                let destination_view = MetalImageView::new(
                    NonNull::from(destination_base.as_mut()),
                    &view_info,
                    destination,
                )?;
                let mut colors = [None; NUM_RT];
                colors[0] = Some(&destination_view);
                let level = copy.image_subresource.base_level.max(0) as u32;
                let framebuffer = MetalFramebuffer::new(
                    colors,
                    None,
                    &RenderTargets {
                        size: Extent2D {
                            width: (destination_info.size.width >> level).max(1),
                            height: (destination_info.size.height >> level).max(1),
                        },
                        ..RenderTargets::default()
                    },
                )?;
                let region = MetalBlitRegion {
                    start: (copy.image_offset.x, copy.image_offset.y),
                    end: (
                        copy.image_offset.x.saturating_add(copy.image_extent.width as i32),
                        copy.image_offset.y.saturating_add(copy.image_extent.height as i32),
                    ),
                };
                let scheduler = unsafe { self.scheduler.as_mut() };
                let helper = unsafe { self.blit_image_helper.as_mut() };
                helper.copy_msaa_to_single_sample_color(
                    scheduler,
                    &framebuffer,
                    &source_view,
                    region,
                    region,
                    source.samples(),
                )?;
                destination.mark_contents_modified();
            }
        }
        Ok(())
    }

    pub fn transfer_depth32_stencil8_memory(
        &mut self,
        image: &MetalImage,
        buffer: &MetalBuffer,
        base_offset: usize,
        copies: &[BufferImageCopy],
        upload: bool,
    ) -> Result<(), MetalImageError> {
        if self.depth_stencil_copy.is_none() {
            self.depth_stencil_copy = Some(MetalDepthStencilCopy::new(&self.device)?);
        }
        image.transfer_depth32_stencil8_memory(
            unsafe { self.scheduler.as_mut() },
            self.depth_stencil_copy.as_ref().unwrap(),
            buffer,
            base_offset,
            copies,
            upload,
        )
    }

    /// Native counterpart of TextureCacheRuntime::BlitImage. The common cache
    /// has already resolved image identity, subresources, scaling and aliases.
    #[allow(clippy::too_many_arguments)]
    pub fn blit_image(
        &mut self,
        framebuffer: &MetalFramebuffer,
        destination: &MetalImageView,
        source: &MetalImageView,
        dst_region: MetalBlitRegion,
        src_region: MetalBlitRegion,
        filter: Filter,
        operation: Operation,
    ) -> Result<(), MetalTextureCacheError> {
        let aspect = get_format_type(source.base().format);
        if aspect != get_format_type(destination.base().format) {
            return Err(MetalTextureCacheError::InvalidCopy("blit aspects differ"));
        }
        let source_msaa = source.samples() > 1;
        let destination_msaa = destination.samples() > 1;
        if (destination_msaa && !source_msaa)
            || (source_msaa && destination_msaa && source.samples() != destination.samples())
        {
            return Err(MetalTextureCacheError::InvalidCopy(
                "incompatible blit sample counts",
            ));
        }
        let color = aspect == SurfaceType::ColorTexture;
        if color {
            let numeric_type = |format| {
                (
                    crate::surface::is_pixel_format_integer(format),
                    crate::surface::is_pixel_format_signed_integer(format),
                )
            };
            let source_type = numeric_type(source.base().format);
            if source_type != numeric_type(destination.base().format)
                || (source_type.0 && filter != Filter::Point)
            {
                return Err(MetalTextureCacheError::InvalidCopy(
                    "integer blits require matching numeric types and point filtering",
                ));
            }
        }
        if !color || source_msaa || destination_msaa {
            if source.base().format != destination.base().format || operation != Operation::SrcCopy
            {
                return Err(MetalTextureCacheError::InvalidCopy(
                    "non-color/MSAA blits require matching formats and SrcCopy",
                ));
            }
        }
        if !color && filter != Filter::Point {
            return Err(MetalTextureCacheError::InvalidCopy(
                "depth/stencil blits require point filtering",
            ));
        }
        // Both pointers refer to stable, independently boxed rasterizer owners.
        let scheduler = unsafe { self.scheduler.as_mut() };
        let helper = unsafe { self.blit_image_helper.as_mut() };
        if color {
            if source_msaa {
                helper.blit_color_msaa(scheduler, framebuffer, source, dst_region, src_region)?;
            } else {
                helper.blit_color(
                    scheduler,
                    framebuffer,
                    source,
                    dst_region,
                    src_region,
                    filter,
                    operation,
                )?;
            }
        } else if source_msaa && !destination_msaa {
            helper.resolve_depth_stencil(scheduler, framebuffer, source, dst_region, src_region)?;
        } else {
            helper.blit_depth_stencil(
                scheduler,
                framebuffer,
                source,
                dst_region,
                src_region,
                filter,
                operation,
            )?;
        }
        Ok(())
    }

    /// Port of Eden `TextureCacheRuntime::CopyImage` for native Metal copies.
    pub fn copy_image(
        &mut self,
        destination: &MetalImage,
        source: &MetalImage,
        copies: &[ImageCopy],
    ) -> Result<(), MetalTextureCacheError> {
        if source.samples() != 1 || destination.samples() != 1 {
            return Err(MetalTextureCacheError::MultisampleCopyRequiresShader);
        }
        {
            let scheduler = self.scheduler();
            source.ensure_native_storage(scheduler)?;
            destination.ensure_native_storage(scheduler)?;
        }
        let native_copies = copies
            .iter()
            .map(|copy| make_native_image_copies(source, destination, copy))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        let same_native_representation = source.format().pixel_format
            == destination.format().pixel_format
            && (source.guest_format() == destination.guest_format()
                || (!source.format().requires_conversion
                    && !destination.format().requires_conversion));
        if !native_copies.is_empty() { destination.mark_contents_modified(); }
        if !same_native_representation {
            return self.copy_image_through_buffer(destination, source, &native_copies);
        }
        let scheduler = self.scheduler();
        scheduler.request_outside_render_pass_operation_context();
        scheduler.with_blit_encoder(|encoder| {
            for copy in native_copies {
                unsafe {
                    encoder.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                        source.handle(),
                        copy.source_slice,
                        copy.source_level,
                        copy.source_origin,
                        copy.source_size,
                        destination.handle(),
                        copy.destination_slice,
                        copy.destination_level,
                        copy.destination_origin,
                    );
                }
            }
        })?;
        destination.mark_native_modified();
        Ok(())
    }

    fn copy_image_through_buffer(
        &mut self,
        destination: &MetalImage,
        source: &MetalImage,
        copies: &[NativeImageCopy],
    ) -> Result<(), MetalTextureCacheError> {
        if source.format().requires_conversion || destination.format().requires_conversion {
            return Err(MetalTextureCacheError::IncompatibleFormats);
        }
        let source_block = (
            crate::surface::default_block_width(source.guest_format()).max(1) as usize,
            crate::surface::default_block_height(source.guest_format()).max(1) as usize,
            crate::surface::bytes_per_block(source.guest_format()).max(1) as usize,
        );
        let destination_block = (
            crate::surface::default_block_width(destination.guest_format()).max(1) as usize,
            crate::surface::default_block_height(destination.guest_format()).max(1) as usize,
            crate::surface::bytes_per_block(destination.guest_format()).max(1) as usize,
        );
        if source_block.2 != destination_block.2 {
            return Err(MetalTextureCacheError::IncompatibleFormats);
        }

        let mut offset = 0usize;
        let mut layouts = Vec::with_capacity(copies.len());
        for copy in copies {
            let blocks_per_row = copy.source_size.width.div_ceil(source_block.0);
            let block_rows = copy.source_size.height.div_ceil(source_block.1);
            let bytes_per_row = align_up(blocks_per_row.saturating_mul(source_block.2), 256);
            let bytes_per_image = bytes_per_row.saturating_mul(block_rows);
            offset = align_up(offset, 256);
            layouts.push(NativeBufferCopy {
                image: *copy,
                buffer_offset: offset,
                bytes_per_row,
                bytes_per_image,
            });
            offset = offset.saturating_add(bytes_per_image.saturating_mul(copy.source_size.depth));
        }
        let intermediate = MetalBuffer::new_private(&self.device, offset)?;
        if matches!(
            (source.guest_format(), destination.guest_format()),
            (PixelFormat::D32FloatS8Uint, PixelFormat::R32G32Float)
                | (PixelFormat::R32G32Float, PixelFormat::D32FloatS8Uint)
        ) {
            if self.depth_stencil_copy.is_none() {
                self.depth_stencil_copy = Some(MetalDepthStencilCopy::new(&self.device)?);
            }
            let helper = self.depth_stencil_copy.as_ref().unwrap();
            // The scheduler is independently owned by the rasterizer.
            let scheduler = unsafe { self.scheduler.as_mut() };
            for layout in &layouts {
                let copy = layout.image;
                if source.guest_format() == PixelFormat::D32FloatS8Uint {
                    helper.copy(
                        scheduler,
                        source.handle(),
                        &intermediate,
                        MetalDepthStencilBufferCopy {
                            buffer_offset: layout.buffer_offset,
                            bytes_per_row: layout.bytes_per_row,
                            bytes_per_image: layout.bytes_per_image,
                            slice: copy.source_slice,
                            level: copy.source_level,
                            origin: copy.source_origin,
                            size: copy.source_size,
                        },
                        false,
                    )?;
                } else {
                    scheduler.with_blit_encoder(|encoder| unsafe {
                        encoder.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage(
                            source.handle(), copy.source_slice, copy.source_level, copy.source_origin, copy.source_size,
                            intermediate.handle(), layout.buffer_offset, layout.bytes_per_row, layout.bytes_per_image);
                    })?;
                }
            }
            for layout in &layouts {
                let copy = layout.image;
                if destination.guest_format() == PixelFormat::D32FloatS8Uint {
                    helper.copy(
                        scheduler,
                        destination.handle(),
                        &intermediate,
                        MetalDepthStencilBufferCopy {
                            buffer_offset: layout.buffer_offset,
                            bytes_per_row: layout.bytes_per_row,
                            bytes_per_image: layout.bytes_per_image,
                            slice: copy.destination_slice,
                            level: copy.destination_level,
                            origin: copy.destination_origin,
                            size: copy.destination_size,
                        },
                        true,
                    )?;
                } else {
                    scheduler.with_blit_encoder(|encoder| unsafe {
                        encoder.copyFromBuffer_sourceOffset_sourceBytesPerRow_sourceBytesPerImage_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                            intermediate.handle(), layout.buffer_offset, layout.bytes_per_row, layout.bytes_per_image, copy.destination_size,
                            destination.handle(), copy.destination_slice, copy.destination_level, copy.destination_origin);
                    })?;
                }
            }
            destination.mark_native_modified();
            return Ok(());
        }
        let scheduler = self.scheduler();
        scheduler.request_outside_render_pass_operation_context();
        scheduler.with_blit_encoder(|encoder| {
            for layout in &layouts {
                let copy = layout.image;
                unsafe {
                    encoder.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage(
                        source.handle(),
                        copy.source_slice,
                        copy.source_level,
                        copy.source_origin,
                        copy.source_size,
                        intermediate.handle(),
                        layout.buffer_offset,
                        layout.bytes_per_row,
                        layout.bytes_per_image,
                    );
                }
            }
            for layout in &layouts {
                let copy = layout.image;
                unsafe {
                    encoder.copyFromBuffer_sourceOffset_sourceBytesPerRow_sourceBytesPerImage_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                        intermediate.handle(),
                        layout.buffer_offset,
                        layout.bytes_per_row,
                        layout.bytes_per_image,
                        copy.destination_size,
                        destination.handle(),
                        copy.destination_slice,
                        copy.destination_level,
                        copy.destination_origin,
                    );
                }
            }
        })?;
        destination.mark_native_modified();
        Ok(())
    }

    /// Resolve a color multisample image into a single-sample image.
    ///
    /// Metal exposes resolve as a render-pass store action rather than a blit
    /// command. Partial resolves and single-sample-to-MSAA copies require the
    /// shader copy path, matching Eden's `BlitImageHelper::CopyMSAA` fallback.
    pub fn resolve_image_msaa(
        &mut self,
        destination: &MetalImage,
        source: &MetalImage,
        copies: &[ImageCopy],
    ) -> Result<(), MetalTextureCacheError> {
        if source.samples() <= 1
            || destination.samples() != 1
            || source.format().requires_conversion
            || destination.format().requires_conversion
            || source.format().pixel_format != destination.format().pixel_format
            || crate::surface::get_format_type(source.guest_format())
                != crate::surface::SurfaceType::ColorTexture
            || crate::surface::is_pixel_format_integer(source.guest_format())
        {
            return Err(MetalTextureCacheError::InvalidMultisampleResolve);
        }

        let native_copies = copies
            .iter()
            .map(|copy| make_native_image_copies(source, destination, copy))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        for copy in native_copies {
            let source_size = mip_size(source, copy.source_level);
            let destination_size = mip_size(destination, copy.destination_level);
            if copy.source_origin != (MTLOrigin { x: 0, y: 0, z: 0 })
                || copy.destination_origin != (MTLOrigin { x: 0, y: 0, z: 0 })
                || copy.source_size != source_size
                || copy.destination_size != destination_size
                || copy.source_level != 0
            {
                return Err(MetalTextureCacheError::MultisampleCopyRequiresShader);
            }

            let descriptor = MTLRenderPassDescriptor::renderPassDescriptor();
            let attachment = unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) };
            attachment.setTexture(Some(source.handle()));
            attachment.setResolveTexture(Some(destination.handle()));
            attachment.setSlice(copy.source_slice);
            attachment.setResolveSlice(copy.destination_slice);
            attachment.setResolveLevel(copy.destination_level);
            descriptor.setRenderTargetWidth(copy.source_size.width);
            descriptor.setRenderTargetHeight(copy.source_size.height);
            descriptor.setRenderTargetArrayLength(1);
            descriptor.setDefaultRasterSampleCount(source.samples() as usize);
            attachment.setLoadAction(MTLLoadAction::Load);
            attachment.setStoreAction(MTLStoreAction::StoreAndMultisampleResolve);
            destination.mark_contents_modified();
            self.scheduler().begin_render_pass(&descriptor)?;
            self.scheduler().end_render_pass();
        }
        Ok(())
    }
}

/// Backend image-view payload kept in the common texture-cache slots.
///
/// Buffer views are materialized from `MetalBuffer` when descriptors are
/// consumed, because Metal requires the final byte offset and row pitch at
/// `newTextureWithDescriptor` time. Null descriptors likewise remain an
/// explicit sentinel and are bound through the rasterizer's fallback image.
pub enum MetalCachedImageView {
    Image(MetalImageView),
    Buffer(NonNull<ImageViewBase>),
    Null(NonNull<ImageViewBase>),
}

impl MetalCachedImageView {
    pub fn base(&self) -> &ImageViewBase {
        let base = match self {
            Self::Image(view) => return view.base(),
            Self::Buffer(base) | Self::Null(base) => base,
        };
        unsafe { base.as_ref() }
    }

    pub fn image(&self) -> Option<&MetalImageView> {
        match self {
            Self::Image(view) => Some(view),
            Self::Buffer(_) | Self::Null(_) => None,
        }
    }
}

pub struct MetalTextureCacheParams;

fn synchronize_image_storage(
    cache: &mut CommonTextureCache<MetalTextureCacheParams>,
    image_id: ImageId,
    texture_type: TextureType,
    is_modification: bool,
) -> bool {
    if !image_id.is_valid() || image_id == NULL_IMAGE_ID || !cache.slot_images.contains(image_id) {
        return false;
    }
    let Some(image) = cache.slot_images[image_id].backend.take() else {
        return false;
    };
    let result =
        image.ensure_storage_for_texture_type(cache.runtime_mut().scheduler(), texture_type);
    if result.is_ok() && is_modification {
        image.mark_modified_for_texture_type(texture_type);
    }
    cache.slot_images[image_id].backend = Some(image);
    if let Err(error) = result {
        log::error!(
            "Metal image storage synchronization failed for {}: {error}",
            image_id.index
        );
        return false;
    }
    true
}

impl TextureCacheParams for MetalTextureCacheParams {
    type Runtime = MetalTextureCacheRuntime;
    type Image = MetalImage;
    type ImageAlloc = ();
    type ImageView = MetalCachedImageView;
    type Sampler = MetalSampler;
    type Framebuffer = Box<MetalFramebuffer>;
    type FramebufferError = MetalFramebufferError;
    type AsyncBuffer = StagingBufferRef;
    type BufferType = Arc<MetalBuffer>;

    const ENABLE_VALIDATION: bool = true;
    const FRAMEBUFFER_BLITS: bool = true;
    const HAS_EMULATED_COPIES: bool = false;
    const HAS_DEVICE_MEMORY_INFO: bool = false;
    const IMPLEMENTS_ASYNC_DOWNLOADS: bool = false;
    // Eden has no Metal backend. Vulkan sets this true after scratch CopyMSAA
    // downloads; OpenGL leaves it false. Metal follows the Vulkan contract.
    const HAS_MSAA_DOWNLOADS: bool = true;

    fn create_image(
        runtime: Option<&mut Self::Runtime>,
        _image_id: ImageId,
        base: NonNull<ImageBase>,
    ) -> Self::Image {
        let runtime = runtime.expect("Metal texture-cache runtime must be bound");
        MetalImage::new(runtime.device(), &unsafe { base.as_ref() }.info)
            .unwrap_or_else(|error| panic!("Metal image construction failed: {error}"))
    }

    fn blit_image(
        cache: &mut CommonTextureCache<Self>,
        dst_framebuffer_id: FramebufferId,
        _src_framebuffer_id: FramebufferId,
        dst_view_id: ImageViewId,
        src_view_id: ImageViewId,
        dst_region: Region2D,
        src_region: Region2D,
        filter: Filter,
        operation: Operation,
    ) {
        let src_image_id = cache.slot_image_views[src_view_id].image_id;
        let dst_image_id = cache.slot_image_views[dst_view_id].image_id;
        for image_id in [src_image_id, dst_image_id] {
            if !synchronize_image_storage(cache, image_id, TextureType::Color2D, false) {
                log::error!("Metal blit image storage preparation failed");
                return;
            }
        }
        let source = cache.slot_image_views[src_view_id]
            .backend
            .as_ref()
            .and_then(MetalCachedImageView::image)
            .expect("common blit source view must exist");
        let destination = cache.slot_image_views[dst_view_id]
            .backend
            .as_ref()
            .and_then(MetalCachedImageView::image)
            .expect("common blit destination view must exist");
        let region = |region: Region2D| MetalBlitRegion {
            start: (region.start.x, region.start.y),
            end: (region.end.x, region.end.y),
        };
        // Disjoint field borrows keep the views/framebuffer alive across runtime recording.
        if let Some(image) = cache.slot_images[dst_image_id].backend.as_ref() {
            image.mark_contents_modified();
        }
        let result = cache
            .runtime
            .as_deref_mut()
            .expect("Metal runtime must be bound")
            .blit_image(
                &cache.slot_framebuffers[dst_framebuffer_id],
                destination,
                source,
                region(dst_region),
                region(src_region),
                filter,
                operation,
            );
        if let Err(error) = result {
            log::error!("Metal TextureCacheRuntime::BlitImage failed: {error}");
        } else if let Some(image) = cache.slot_images[dst_image_id].backend.as_ref() {
            image.mark_modified_for_texture_type(TextureType::Color2D);
        }
    }

    fn set_image_allocation_tick(image: &mut Self::Image, allocation_tick: u64) {
        image.set_allocation_tick(allocation_tick);
    }

    fn create_image_view(
        _runtime: Option<&mut Self::Runtime>,
        view_id: ImageViewId,
        info: &ImageViewInfo,
        base: NonNull<ImageViewBase>,
        image: Option<&Self::Image>,
    ) -> Self::ImageView {
        if view_id == NULL_IMAGE_VIEW_ID {
            return MetalCachedImageView::Null(base);
        }
        if unsafe { base.as_ref() }.is_buffer() {
            return MetalCachedImageView::Buffer(base);
        }
        MetalCachedImageView::Image(
            MetalImageView::new(
                base,
                info,
                image.expect("non-buffer Metal image view requires its parent image"),
            )
            .unwrap_or_else(|error| panic!("Metal image-view construction failed: {error}")),
        )
    }

    fn create_sampler(
        runtime: Option<&mut Self::Runtime>,
        config: &crate::textures::texture::TscEntry,
    ) -> Self::Sampler {
        let runtime = runtime.expect("Metal texture-cache runtime must be bound");
        MetalSampler::new(runtime.device(), config)
            .unwrap_or_else(|error| panic!("Metal sampler construction failed: {error}"))
    }

    fn create_framebuffer(
        _runtime: Option<&mut Self::Runtime>,
        color_buffers: [Option<NonNull<Self::ImageView>>; NUM_RT],
        depth_buffer: Option<NonNull<Self::ImageView>>,
        key: &RenderTargets,
    ) -> Result<Self::Framebuffer, Self::FramebufferError> {
        let colors = std::array::from_fn(|index| {
            color_buffers[index].and_then(|view| unsafe { view.as_ref() }.image())
        });
        let depth = depth_buffer.and_then(|view| unsafe { view.as_ref() }.image());
        Ok(Box::new(MetalFramebuffer::new(colors, depth, key)?))
    }

    fn prepare_image_view(
        cache: &mut CommonTextureCache<Self>,
        image_view_id: ImageViewId,
        is_modification: bool,
        invalidate: bool,
    ) {
        if !image_view_id.is_valid()
            || image_view_id == NULL_IMAGE_VIEW_ID
            || !cache.slot_image_views.contains(image_view_id)
        {
            return;
        }
        let view = &cache.slot_image_views[image_view_id];
        if view.is_buffer() {
            return;
        }
        let image_id = view.image_id;
        let texture_type = if view.view_type == ImageViewType::E3D {
            TextureType::Color3D
        } else {
            TextureType::Color2D
        };
        cache.prepare_image(image_id, is_modification, invalidate);
        synchronize_image_storage(cache, image_id, texture_type, is_modification);
    }

    fn scale_up_image(
        _cache: &mut CommonTextureCache<Self>,
        _image_id: ImageId,
        _ignore: bool,
    ) -> bool {
        // Native scaling requires the Metal blit-shader prerequisite. Returning
        // false is the common-cache capability contract: it keeps the original
        // image active and does not mark it rescaled.
        false
    }

    fn scale_down_image(
        _cache: &mut CommonTextureCache<Self>,
        _image_id: ImageId,
        _ignore: bool,
    ) -> bool {
        false
    }

    fn upload_staging_buffer(
        cache: &mut CommonTextureCache<Self>,
        size: usize,
        deferred: bool,
    ) -> Self::AsyncBuffer {
        cache
            .runtime_mut()
            .upload_staging_buffer(size, deferred)
            .unwrap_or_else(|error| panic!("Metal staging allocation failed: {error}"))
    }

    fn staging_mapped_span(buffer: &mut Self::AsyncBuffer) -> &mut [u8] {
        buffer.mapped_span_mut()
    }

    fn free_deferred_staging_buffer(
        cache: &mut CommonTextureCache<Self>,
        buffer: &mut Self::AsyncBuffer,
    ) {
        cache
            .runtime_mut()
            .free_deferred_staging_buffer(buffer)
            .unwrap_or_else(|error| panic!("Metal deferred staging release failed: {error}"));
    }

    fn can_upload_msaa(_cache: &CommonTextureCache<Self>) -> bool {
        false
    }

    fn can_download_msaa(cache: &CommonTextureCache<Self>, info: &ImageInfo) -> bool {
        cache.runtime().can_download_msaa(info)
    }

    fn transition_image_layout(_cache: &mut CommonTextureCache<Self>, _image_id: ImageId) {
        // Metal has no explicit image layouts. Resource hazards are tracked by
        // the command queue and render/compute encoder boundaries.
    }

    fn upload_image(
        cache: &mut CommonTextureCache<Self>,
        image_id: ImageId,
        staging: &Self::AsyncBuffer,
        copies: &[BufferImageCopy],
    ) {
        let image = cache.slot_images[image_id]
            .backend
            .take()
            .expect("Metal image backend must be materialized");
        let result = match image.guest_format() {
            PixelFormat::D32FloatS8Uint => cache.runtime_mut().transfer_depth32_stencil8_memory(
                &image,
                &staging.buffer,
                staging.offset,
                copies,
                true,
            ),
            PixelFormat::D24UnormS8Uint | PixelFormat::S8UintD24Unorm => {
                let converted_size = converted_depth_stencil_linear_size(copies);
                let mut converted = cache
                    .runtime_mut()
                    .upload_staging_buffer(converted_size, false);
                match converted.as_mut() {
                    Ok(converted) => {
                        let converted_copies = convert_depth24_stencil8_upload(
                            image.guest_format(),
                            staging.mapped_span(),
                            converted.mapped_span_mut(),
                            copies,
                        );
                        match converted_copies {
                            Ok(converted_copies) => image.upload_depth_stencil_memory(
                                cache.runtime_mut().scheduler(),
                                &converted.buffer,
                                converted.offset,
                                &converted_copies.depth,
                                &converted_copies.stencil,
                            ),
                            Err(error) => Err(error),
                        }
                    }
                    Err(error) => {
                        cache.slot_images[image_id].backend = Some(image);
                        log::error!("Metal depth/stencil staging allocation failed: {error}");
                        return;
                    }
                }
            }
            format @ (PixelFormat::B5G6R5Unorm | PixelFormat::A1B5G5R5Unorm) => {
                let converted_size = converted_linear_size(copies, 2);
                let mut converted = cache
                    .runtime_mut()
                    .upload_staging_buffer(converted_size, false);
                match converted.as_mut() {
                    Ok(converted) => {
                        let converted_copies = convert_packed16_upload(
                            format,
                            staging.mapped_span(),
                            converted.mapped_span_mut(),
                            copies,
                        );
                        match converted_copies {
                            Ok(converted_copies) => image.upload_converted_memory(
                                cache.runtime_mut().scheduler(),
                                &converted.buffer,
                                converted.offset,
                                &converted_copies,
                                2,
                            ),
                            Err(error) => Err(error),
                        }
                    }
                    Err(error) => {
                        cache.slot_images[image_id].backend = Some(image);
                        log::error!("Metal packed16 staging allocation failed: {error}");
                        return;
                    }
                }
            }
            format @ (PixelFormat::R32G32B32Float | PixelFormat::G4R4Unorm | PixelFormat::X8D24Unorm) => {
                let native_bytes = match format {
                    PixelFormat::R32G32B32Float => 16,
                    PixelFormat::X8D24Unorm => 4,
                    _ => 2,
                };
                let allocation = converted_copy_layout(copies, native_bytes)
                    .map_err(MetalTextureCacheError::from)
                    .and_then(|(_, size)| cache.runtime_mut().upload_staging_buffer(size, false));
                match allocation {
                    Ok(mut converted) => match convert_uncompressed_memory(
                        format, staging.mapped_span(), converted.mapped_span_mut(), copies, true,
                    ) {
                        Ok(native_copies) => image.upload_converted_memory(
                            cache.runtime_mut().scheduler(), &converted.buffer, converted.offset, &native_copies, native_bytes,
                        ),
                        Err(error) => Err(error),
                    },
                    Err(error) => {
                        cache.slot_images[image_id].backend = Some(image);
                        log::error!("Metal expanded color staging allocation failed for {format:?}: {error}");
                        return;
                    }
                }
            }
            _ => image.upload_memory(
                cache.runtime_mut().scheduler(),
                &staging.buffer,
                staging.offset,
                copies,
            ),
        };
        cache.slot_images[image_id].backend = Some(image);
        if let Err(error) = result {
            log::error!("Metal image upload failed for {}: {error}", image_id.index);
        }
    }

    fn accelerate_image_upload(
        cache: &mut CommonTextureCache<Self>,
        image_id: ImageId,
        _staging: &Self::AsyncBuffer,
        _swizzles: &[crate::texture_cache::types::SwizzleParameters],
        _z_start: u32,
        _z_count: u32,
    ) {
        let image = &cache.slot_images[image_id];
        assert!(
            !image
                .flags
                .contains(crate::texture_cache::image_base::ImageFlagBits::ACCELERATED_UPLOAD),
            "Metal accelerated upload reached without MetalImage advertising the capability"
        );
        unreachable!("the common cache only calls this method for accelerated images");
    }

    fn insert_upload_memory_barrier(_cache: &mut CommonTextureCache<Self>) {
        // Upload and render encoders share one serial Metal command queue.
    }

    fn copy_image(
        cache: &mut CommonTextureCache<Self>,
        dst_id: ImageId,
        src_id: ImageId,
        copies: &[ImageCopy],
    ) {
        if dst_id == src_id {
            return;
        }
        let destination = cache.slot_images[dst_id]
            .backend
            .take()
            .expect("Metal destination image backend must be materialized");
        let source = cache.slot_images[src_id]
            .backend
            .take()
            .expect("Metal source image backend must be materialized");
        let result = cache
            .runtime_mut()
            .copy_image(&destination, &source, copies);
        if let Err(error) = result {
            log::error!(
                "Metal image copy failed: dst={} ({:?}/{:?}, converted={}, type={:?}, size={:?}) src={} ({:?}/{:?}, converted={}, type={:?}, size={:?}) first_copy={:?}: {error}",
                dst_id.index,
                destination.guest_format(),
                destination.format().pixel_format,
                destination.format().requires_conversion,
                destination.image_type(),
                destination.size(),
                src_id.index,
                source.guest_format(),
                source.format().pixel_format,
                source.format().requires_conversion,
                source.image_type(),
                source.size(),
                copies.first().map(|copy| (
                    copy.src_offset,
                    copy.dst_offset,
                    copy.extent,
                    copy.src_subresource,
                    copy.dst_subresource,
                )),
            );
        }
        cache.slot_images[src_id].backend = Some(source);
        cache.slot_images[dst_id].backend = Some(destination);
    }

    fn copy_image_msaa(
        cache: &mut CommonTextureCache<Self>,
        dst_id: ImageId,
        src_id: ImageId,
        copies: &[ImageCopy],
    ) {
        if dst_id == src_id {
            return;
        }
        let destination = cache.slot_images[dst_id]
            .backend
            .take()
            .expect("Metal destination image backend must be materialized");
        let source = cache.slot_images[src_id]
            .backend
            .take()
            .expect("Metal source image backend must be materialized");
        let result = cache
            .runtime_mut()
            .resolve_image_msaa(&destination, &source, copies);
        cache.slot_images[src_id].backend = Some(source);
        cache.slot_images[dst_id].backend = Some(destination);
        if let Err(error) = result {
            log::error!(
                "Metal multisample image copy failed: dst={} src={}: {error}",
                dst_id.index,
                src_id.index
            );
        }
    }
}

fn converted_linear_size(copies: &[BufferImageCopy], bytes_per_texel: usize) -> usize {
    copies
        .iter()
        .map(|copy| {
            let row_texels = if copy.buffer_row_length == 0 {
                copy.image_extent.width
            } else {
                copy.buffer_row_length
            } as usize;
            let rows = if copy.buffer_image_height == 0 {
                copy.image_extent.height
            } else {
                copy.buffer_image_height
            } as usize;
            let planes = if copy.image_extent.depth > 1 {
                copy.image_extent.depth as usize
            } else {
                copy.image_subresource.num_layers.max(1) as usize
            };
            row_texels
                .saturating_mul(rows)
                .saturating_mul(planes)
                .saturating_mul(bytes_per_texel)
        })
        .sum()
}

fn converted_depth_stencil_linear_size(copies: &[BufferImageCopy]) -> usize {
    copies.iter().fold(0usize, |offset, copy| {
        let texels = copy_linear_texel_count(copy);
        let depth_offset = align_up(offset, 8);
        let stencil_offset = align_up(depth_offset.saturating_add(texels.saturating_mul(4)), 8);
        stencil_offset.saturating_add(texels)
    })
}

fn copy_linear_texel_count(copy: &BufferImageCopy) -> usize {
    let row_texels = if copy.buffer_row_length == 0 {
        copy.image_extent.width
    } else {
        copy.buffer_row_length
    } as usize;
    let rows = if copy.buffer_image_height == 0 {
        copy.image_extent.height
    } else {
        copy.buffer_image_height
    } as usize;
    let planes = if copy.image_extent.depth > 1 {
        copy.image_extent.depth as usize
    } else {
        copy.image_subresource.num_layers.max(1) as usize
    };
    row_texels.saturating_mul(rows).saturating_mul(planes)
}

fn convert_packed16_upload(
    format: PixelFormat,
    input: &[u8],
    output: &mut [u8],
    copies: &[BufferImageCopy],
) -> Result<Vec<BufferImageCopy>, super::metal_image::MetalImageError> {
    let (shift, preserved) = packed16_channel_layout(format)?;
    let mut output_offset = 0usize;
    let mut converted_copies = Vec::with_capacity(copies.len());
    for copy in copies {
        let row_texels = if copy.buffer_row_length == 0 {
            copy.image_extent.width
        } else {
            copy.buffer_row_length
        } as usize;
        let rows = if copy.buffer_image_height == 0 {
            copy.image_extent.height
        } else {
            copy.buffer_image_height
        } as usize;
        let planes = if copy.image_extent.depth > 1 {
            copy.image_extent.depth as usize
        } else {
            copy.image_subresource.num_layers.max(1) as usize
        };
        let byte_count = row_texels
            .saturating_mul(rows)
            .saturating_mul(planes)
            .saturating_mul(2);
        let input_end = copy.buffer_offset.saturating_add(byte_count);
        let output_end = output_offset.saturating_add(byte_count);
        if input_end > input.len() || output_end > output.len() {
            return Err(super::metal_image::MetalImageError::InvalidCopy(
                "converted B5G6R5 staging range",
            ));
        }
        for (source, destination) in input[copy.buffer_offset..input_end]
            .chunks_exact(2)
            .zip(output[output_offset..output_end].chunks_exact_mut(2))
        {
            let packed = u16::from_le_bytes(source.try_into().unwrap());
            let converted =
                ((packed & 31) << shift) | (packed & preserved) | ((packed >> shift) & 31);
            destination.copy_from_slice(&converted.to_le_bytes());
        }
        let mut converted = *copy;
        converted.buffer_offset = output_offset;
        converted.buffer_size = byte_count;
        converted_copies.push(converted);
        output_offset = output_end;
    }
    Ok(converted_copies)
}

fn converted_texel_bytes(format: PixelFormat) -> Result<(usize, usize), MetalImageError> {
    match format {
        PixelFormat::R32G32B32Float => Ok((12, 16)),
        PixelFormat::G4R4Unorm => Ok((1, 2)),
        PixelFormat::X8D24Unorm => Ok((4, 4)),
        _ => Err(MetalImageError::InvalidCopy("expanded color conversion format")),
    }
}

fn converted_copy_layout(copies: &[BufferImageCopy], native_bytes: usize) -> Result<(Vec<BufferImageCopy>, usize), MetalImageError> {
    let mut end = 0usize;
    let mut native = Vec::with_capacity(copies.len());
    for copy in copies {
        let width = copy.image_extent.width as usize;
        let height = copy.image_extent.height as usize;
        let depth = copy.image_extent.depth as usize;
        let row = if copy.buffer_row_length == 0 { width } else { copy.buffer_row_length as usize };
        let rows = if copy.buffer_image_height == 0 { height } else { copy.buffer_image_height as usize };
        if width == 0 || height == 0 || depth == 0 || copy.image_subresource.num_layers <= 0
            || row < width || rows < height
        {
            return Err(MetalImageError::InvalidCopy("expanded color copy extent"));
        }
        let layers = if depth > 1 { depth } else { copy.image_subresource.num_layers as usize };
        let size = row.checked_mul(rows).and_then(|v| v.checked_mul(layers))
            .and_then(|v| v.checked_mul(native_bytes))
            .ok_or(MetalImageError::InvalidCopy("expanded color copy size overflow"))?;
        native.push(BufferImageCopy { buffer_offset: end, buffer_size: size, ..*copy });
        end = end.checked_add(size).ok_or(MetalImageError::InvalidCopy("expanded color staging size overflow"))?;
    }
    Ok((native, end))
}

// Native transfer conversion for uncompressed formats. RGB32 payload bits
// remain unchanged; packed UNORM components are expanded to native channels.
fn convert_uncompressed_memory(
    format: PixelFormat, input: &[u8], output: &mut [u8], copies: &[BufferImageCopy], upload: bool,
) -> Result<Vec<BufferImageCopy>, MetalImageError> {
    let (guest_bytes, native_bytes) = converted_texel_bytes(format)?;
    let (native, _) = converted_copy_layout(copies, native_bytes)?;
    for (copy, converted) in copies.iter().zip(&native) {
        let row = if copy.buffer_row_length == 0 { copy.image_extent.width } else { copy.buffer_row_length } as usize;
        let rows = if copy.buffer_image_height == 0 { copy.image_extent.height } else { copy.buffer_image_height } as usize;
        let layers = if copy.image_extent.depth > 1 { copy.image_extent.depth as usize }
            else { copy.image_subresource.num_layers as usize };
        // Full native layout was checked above, so these smaller spans fit.
        let texels = ((layers - 1) * rows + copy.image_extent.height as usize - 1) * row
            + copy.image_extent.width as usize;
        let guest_end = copy.buffer_offset.checked_add(texels * guest_bytes);
        let native_end = converted.buffer_offset + texels * native_bytes;
        let (guest_len, native_len) = if upload { (input.len(), output.len()) } else { (output.len(), input.len()) };
        if guest_end.is_none_or(|end| end > guest_len) || native_end > native_len
            || (copy.buffer_size != 0 && texels * guest_bytes > copy.buffer_size)
        {
            return Err(MetalImageError::InvalidCopy("expanded color conversion buffer range"));
        }
    }
    for (copy, converted) in copies.iter().zip(&native) {
        let row = if copy.buffer_row_length == 0 { copy.image_extent.width } else { copy.buffer_row_length } as usize;
        let rows = if copy.buffer_image_height == 0 { copy.image_extent.height } else { copy.buffer_image_height } as usize;
        let layers = if copy.image_extent.depth > 1 { copy.image_extent.depth as usize }
            else { copy.image_subresource.num_layers as usize };
        for layer in 0..layers {
            for y in 0..copy.image_extent.height as usize {
                for x in 0..copy.image_extent.width as usize {
                    let index = (layer * rows + y) * row + x;
                    let guest = copy.buffer_offset + index * guest_bytes;
                    let host = converted.buffer_offset + index * native_bytes;
                    if format == PixelFormat::X8D24Unorm && upload {
                        let packed = u32::from_le_bytes(input[guest..guest + 4].try_into().unwrap());
                        let depth = (packed & 0x00ff_ffff) as f32 / 16_777_215.0;
                        output[host..host + 4].copy_from_slice(&depth.to_le_bytes());
                    } else if format == PixelFormat::X8D24Unorm {
                        let depth = f32::from_le_bytes(input[host..host + 4].try_into().unwrap());
                        output[guest..guest + 4].copy_from_slice(&depth_float_to_unorm24(depth).to_le_bytes());
                    } else if format == PixelFormat::G4R4Unorm && upload {
                        output[host] = (input[guest] & 15) * 17;
                        output[host + 1] = (input[guest] >> 4) * 17;
                    } else if format == PixelFormat::G4R4Unorm {
                        let r = ((u16::from(input[host]) + 8) / 17) as u8;
                        let g = ((u16::from(input[host + 1]) + 8) / 17) as u8;
                        output[guest] = r | (g << 4);
                    } else if upload {
                        output[host..host + 12].copy_from_slice(&input[guest..guest + 12]);
                        output[host + 12..host + 16].copy_from_slice(&1.0f32.to_le_bytes());
                    } else {
                        output[guest..guest + 12].copy_from_slice(&input[host..host + 12]);
                    }
                }
            }
        }
    }
    Ok(native)
}

// Eden swaps the view's R/B channels for A1B5G5R5. Metal uses converted
// storage instead; the bit-15 alpha must survive both transfer directions.
fn packed16_channel_layout(format: PixelFormat) -> Result<(u32, u16), MetalImageError> {
    match format {
        PixelFormat::B5G6R5Unorm => Ok((11, 0x07e0)),
        PixelFormat::A1B5G5R5Unorm => Ok((10, 0x83e0)),
        _ => Err(MetalImageError::InvalidCopy("packed16 conversion format")),
    }
}

fn convert_packed16_download(
    format: PixelFormat,
    input: &[u8],
    output: &mut [u8],
    copies: &[BufferImageCopy],
) -> Result<(), MetalImageError> {
    let (shift, preserved) = packed16_channel_layout(format)?;
    // Validate every copy before changing guest output, including overflow.
    for copy in copies {
        let width = copy.image_extent.width as usize;
        let height = copy.image_extent.height as usize;
        let depth = copy.image_extent.depth as usize;
        let row = if copy.buffer_row_length == 0 { width } else { copy.buffer_row_length as usize };
        let rows = if copy.buffer_image_height == 0 { height } else { copy.buffer_image_height as usize };
        if width == 0 || height == 0 || depth == 0 || copy.image_subresource.num_layers <= 0
            || row < width || rows < height
        {
            return Err(MetalImageError::InvalidCopy("B5G6R5 download extent"));
        }
        let layers = if depth > 1 { depth } else { copy.image_subresource.num_layers as usize };
        let span = (layers - 1).checked_mul(rows)
            .and_then(|v| v.checked_add(height - 1))
            .and_then(|v| v.checked_mul(row))
            .and_then(|v| v.checked_add(width))
            .and_then(|v| v.checked_mul(2));
        let end = span.and_then(|v| copy.buffer_offset.checked_add(v));
        if end.is_none_or(|end| end > input.len() || end > output.len())
            || (copy.buffer_size != 0 && span.is_none_or(|span| span > copy.buffer_size))
        {
            return Err(MetalImageError::InvalidCopy("B5G6R5 download buffer range"));
        }
    }
    for copy in copies {
        let row = if copy.buffer_row_length == 0 { copy.image_extent.width } else { copy.buffer_row_length } as usize;
        let rows = if copy.buffer_image_height == 0 { copy.image_extent.height } else { copy.buffer_image_height } as usize;
        let layers = if copy.image_extent.depth > 1 { copy.image_extent.depth as usize }
            else { copy.image_subresource.num_layers as usize };
        for layer in 0..layers {
            for y in 0..copy.image_extent.height as usize {
                for x in 0..copy.image_extent.width as usize {
                    let offset = copy.buffer_offset + ((layer * rows + y) * row + x) * 2;
                    let packed = u16::from_le_bytes(input[offset..offset + 2].try_into().unwrap());
                    let guest = ((packed & 31) << shift) | (packed & preserved) | ((packed >> shift) & 31);
                    output[offset..offset + 2].copy_from_slice(&guest.to_le_bytes());
                }
            }
        }
    }
    Ok(())
}

struct ConvertedDepthStencilCopies {
    depth: Vec<BufferImageCopy>,
    stencil: Vec<BufferImageCopy>,
}

fn converted_depth_stencil_copies(copies: &[BufferImageCopy]) -> ConvertedDepthStencilCopies {
    let mut offset = 0;
    let mut depth = Vec::with_capacity(copies.len());
    let mut stencil = Vec::with_capacity(copies.len());
    for copy in copies {
        let texels = copy_linear_texel_count(copy);
        let mut depth_copy = *copy;
        depth_copy.buffer_offset = align_up(offset, 8);
        depth_copy.buffer_size = texels.saturating_mul(4);
        let mut stencil_copy = *copy;
        stencil_copy.buffer_offset = align_up(depth_copy.buffer_offset.saturating_add(depth_copy.buffer_size), 8);
        stencil_copy.buffer_size = texels;
        offset = stencil_copy.buffer_offset.saturating_add(texels);
        depth.push(depth_copy);
        stencil.push(stencil_copy);
    }
    ConvertedDepthStencilCopies { depth, stencil }
}

fn depth_float_to_unorm24(depth: f32) -> u32 {
    // UNORM conversion: clamp and round to nearest. Use double precision for
    // the product to avoid a second f32 rounding before integer quantization.
    // NaN has no UNORM representation; Rust's saturating cast maps it to zero.
    ((depth as f64).clamp(0.0, 1.0) * 16_777_215.0).round() as u32
}

fn convert_depth24_stencil8_download(
    format: PixelFormat,
    input: &[u8],
    output: &mut [u8],
    copies: &[BufferImageCopy],
) -> Result<(), MetalImageError> {
    if !matches!(format, PixelFormat::D24UnormS8Uint | PixelFormat::S8UintD24Unorm) {
        return Err(MetalImageError::InvalidCopy("unsupported depth/stencil conversion format"));
    }
    let planes = converted_depth_stencil_copies(copies);
    for ((copy, depth), stencil) in copies.iter().zip(&planes.depth).zip(&planes.stencil) {
        let row = if copy.buffer_row_length == 0 { copy.image_extent.width } else { copy.buffer_row_length } as usize;
        let rows = if copy.buffer_image_height == 0 { copy.image_extent.height } else { copy.buffer_image_height } as usize;
        if row < copy.image_extent.width as usize || rows < copy.image_extent.height as usize
            || copy.image_subresource.num_layers <= 0
            || depth.buffer_offset.saturating_add(depth.buffer_size) > input.len()
            || stencil.buffer_offset.saturating_add(stencil.buffer_size) > input.len()
            || copy.buffer_offset.saturating_add(depth.buffer_size) > output.len()
        {
            return Err(MetalImageError::InvalidCopy("converted depth/stencil download range"));
        }
        let layers = if copy.image_extent.depth > 1 { copy.image_extent.depth as usize }
            else { copy.image_subresource.num_layers as usize };
        for layer in 0..layers {
            for y in 0..copy.image_extent.height as usize {
                for x in 0..copy.image_extent.width as usize {
                    let index = (layer * rows + y) * row + x;
                    let offset = depth.buffer_offset + index * 4;
                    let value = f32::from_le_bytes(input[offset..offset + 4].try_into().unwrap());
                    let d = depth_float_to_unorm24(value);
                    let s = input[stencil.buffer_offset + index] as u32;
                    let packed = if format == PixelFormat::D24UnormS8Uint { d | (s << 24) } else { (d << 8) | s };
                    let offset = copy.buffer_offset + index * 4;
                    output[offset..offset + 4].copy_from_slice(&packed.to_le_bytes());
                }
            }
        }
    }
    Ok(())
}

fn convert_depth24_stencil8_upload(
    format: PixelFormat,
    input: &[u8],
    output: &mut [u8],
    copies: &[BufferImageCopy],
) -> Result<ConvertedDepthStencilCopies, super::metal_image::MetalImageError> {
    let planes = converted_depth_stencil_copies(copies);
    for ((copy, depth_copy), stencil_copy) in copies.iter().zip(&planes.depth).zip(&planes.stencil) {
        let texels = copy_linear_texel_count(copy);
        let input_size = texels.saturating_mul(4);
        let input_end = copy.buffer_offset.saturating_add(input_size);
        let depth_offset = depth_copy.buffer_offset;
        let stencil_offset = stencil_copy.buffer_offset;
        let output_end = stencil_offset.saturating_add(texels);
        if input_end > input.len() || output_end > output.len() {
            return Err(super::metal_image::MetalImageError::InvalidCopy(
                "converted depth/stencil staging range",
            ));
        }
        for (index, source) in input[copy.buffer_offset..input_end]
            .chunks_exact(4)
            .enumerate()
        {
            let packed = u32::from_le_bytes(source.try_into().unwrap());
            let (depth, stencil) = match format {
                PixelFormat::D24UnormS8Uint => (packed & 0x00ff_ffff, packed >> 24),
                PixelFormat::S8UintD24Unorm => (packed >> 8, packed & 0xff),
                _ => {
                    return Err(super::metal_image::MetalImageError::InvalidCopy(
                        "unsupported depth/stencil conversion format",
                    ));
                }
            };
            let depth_output = depth_offset + index * 4;
            output[depth_output..depth_output + 4]
                .copy_from_slice(&((depth as f32) / 16_777_215.0).to_le_bytes());
            output[stencil_offset + index] = stencil as u8;
        }
    }
    Ok(planes)
}

#[repr(transparent)]
pub struct MetalTextureCache {
    pub base: CommonTextureCache<MetalTextureCacheParams>,
}

impl MetalTextureCache {
    pub fn new(
        device: MetalDevice,
        device_memory: Arc<MaxwellDeviceMemoryManager>,
        scheduler: &mut MetalScheduler,
        staging_buffer_pool: &mut MetalStagingBufferPool,
        blit_image_helper: &mut MetalBlitHelper,
    ) -> Self {
        let mut base = CommonTextureCache::<MetalTextureCacheParams>::new_with_caps_for_backend(
            device_memory,
            false,
            true,
        );
        let runtime = Box::new(MetalTextureCacheRuntime::new(
            device,
            scheduler,
            staging_buffer_pool,
            blit_image_helper,
        ));
        let null_view_base = NonNull::from(base.slot_image_views[NULL_IMAGE_VIEW_ID].base.as_mut());
        base.slot_image_views[NULL_IMAGE_VIEW_ID].backend =
            Some(MetalCachedImageView::Null(null_view_base));
        let null_sampler_descriptor = **base.slot_samplers.get(NULL_SAMPLER_ID);
        base.slot_samplers[NULL_SAMPLER_ID].backend = Some(
            MetalSampler::new(runtime.device(), &null_sampler_descriptor)
                .expect("Metal null sampler construction must succeed"),
        );
        base.bind_runtime(runtime);
        Self { base }
    }

    pub fn create_channel(&mut self, channel: &crate::control::channel_state::ChannelState) {
        self.base.create_channel(channel);
    }

    pub fn blit_image(
        &mut self,
        dst: &crate::engines::fermi_2d::Surface,
        src: &crate::engines::fermi_2d::Surface,
        copy: &crate::engines::fermi_2d::Config,
    ) -> bool {
        self.base.blit_image(dst, src, copy)
    }

    pub fn bind_to_channel(&mut self, channel_id: i32) {
        self.base.bind_to_channel(channel_id);
    }

    pub fn erase_channel(&mut self, channel_id: i32) {
        self.base.erase_channel(channel_id);
    }

    pub fn tick_frame(&mut self) {
        if self.base.total_used_memory > self.base.minimum_memory {
            // As in the GL bridge, the runtime lives in a stable Box. The
            // callback borrows only runtime and the detached image backend;
            // common GC owns metadata, guest writeback and eviction ordering.
            let runtime = self.base.runtime_mut() as *mut MetalTextureCacheRuntime;
            self.base.run_garbage_collector_with_downloader(|image_id, base, backend, staging| {
                let Some(image) = backend.as_ref() else {
                    log::error!("Metal GC cannot download image {} without native storage", image_id.index);
                    return false;
                };
                let copies = crate::texture_cache::util::full_download_copies(&base.info);
                // SAFETY: GPU-thread cache work is serialized; this closure
                // does not access the cache through the runtime.
                match unsafe { &mut *runtime }.download_single_sample_memory(image, staging, &copies) {
                    Ok(()) => true,
                    Err(error) => {
                        log::error!("Metal GC retained image {} after download failure: {error}", image_id.index);
                        false
                    }
                }
            });
        }
        // Match TextureCache<P>::TickFrame retirement before runtime/frame
        // advancement. Native command buffers retain encoded resources until
        // completion; these rings release the cache's separate ownership.
        self.base.tick_delayed_destruction_rings();
        self.base.tick_async_decode();
        self.base.tick_async_unswizzle();
        self.base
            .runtime_mut()
            .tick_frame()
            .unwrap_or_else(|error| panic!("Metal texture-cache frame tick failed: {error}"));
        self.base.tick_frame();
        self.profile_texture_memory();
    }

    // Native diagnostic only: slot-owned roots exclude retired/in-flight
    // resources and snapshot heaps. Device totals are not process footprint.
    fn profile_texture_memory(&mut self) {
        let runtime = self.base.runtime_mut();
        let Some(last_report) = runtime.memory_profile_last_report.as_mut() else { return; };
        if last_report.elapsed() < Duration::from_secs(1) { return; }
        *last_report = Instant::now();
        let device_bytes = runtime.device.device().currentAllocatedSize();
        let mut root_bytes = 0usize;
        let mut slice_bytes = 0usize;
        let mut images = 0usize;
        let mut largest = (0usize, 0u32, PixelFormat::Invalid);
        for (id, slot) in self.base.slot_images.iter() {
            let Some(image) = slot.backend.as_ref() else { continue; };
            let root = image.handle().allocatedSize();
            let slices = image.slice_handle().map_or(0, |texture| texture.allocatedSize());
            root_bytes += root;
            slice_bytes += slices;
            images += 1;
            if root + slices > largest.0 {
                largest = (root + slices, id.index, image.guest_format());
            }
        }
        log::info!("[METAL_TEXTURE_MEMORY] images={images} root_bytes={root_bytes} slice_bytes={slice_bytes} largest_bytes={} largest_id={} largest_format={:?} retired_images={} retired_views={} retired_framebuffers={} device_bytes={device_bytes}",
            largest.0, largest.1, largest.2,
            self.base.sentenced_images.retained_len(),
            self.base.sentenced_image_view.retained_len(),
            self.base.sentenced_framebuffers.retained_len());
    }

    pub fn synchronize_graphics_descriptors(&mut self, regs: DescriptorSyncRegs) {
        self.base.synchronize_graphics_descriptors(regs);
    }

    pub fn fill_image_views(
        &mut self,
        views: &mut [ImageViewInOut],
        compute: bool,
        blacklist: bool,
    ) {
        self.base.fill_image_views(views, compute, blacklist);
    }

    pub fn get_sampler_id(&mut self, index: u32, compute: bool) -> SamplerId {
        self.base.get_sampler_id(index, compute)
    }

    pub fn sampler(&self, sampler_id: SamplerId) -> Option<&MetalSampler> {
        if !sampler_id.is_valid() {
            return None;
        }
        self.base.slot_samplers[sampler_id].backend.as_ref()
    }

    pub fn image_view(&self, view_id: ImageViewId) -> Option<&MetalImageView> {
        if !view_id.is_valid() || view_id == NULL_IMAGE_VIEW_ID {
            return None;
        }
        self.base.slot_image_views[view_id]
            .backend
            .as_ref()
            .and_then(MetalCachedImageView::image)
    }

    pub fn retained_image_view(
        &mut self,
        view_id: ImageViewId,
        texture_type: TextureType,
    ) -> Option<objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_metal::MTLTexture>>>
    {
        self.prepare_retained_image_view(view_id, texture_type, false)
    }

    /// Construct a sampled view over independent storage using the original
    /// guest aspect, swizzle and subresource range. Caller holds the cache lock.
    pub fn retained_sampling_snapshot_view(
        &mut self,
        view_id: ImageViewId,
        texture_type: TextureType,
    ) -> Result<Option<Retained<ProtocolObject<dyn MTLTexture>>>, MetalTextureCacheError> {
        if !view_id.is_valid() || view_id == NULL_IMAGE_VIEW_ID
            || !self.base.slot_image_views.contains(view_id) {
            return Ok(None);
        }
        let view = &self.base.slot_image_views[view_id];
        if view.is_buffer() || view.image_id == NULL_IMAGE_ID
            || !self.base.slot_images.contains(view.image_id) {
            return Ok(None);
        }
        let Some(image) = self.base.slot_images[view.image_id].backend.as_ref() else {
            return Ok(None);
        };
        let Some(runtime) = self.base.runtime.as_mut() else {
            return Ok(None);
        };
        let Some(snapshot) = runtime.sampling_snapshot(image)? else {
            return Ok(None);
        };
        let original = view.backend.as_ref().and_then(MetalCachedImageView::image)
            .and_then(|native| native.retained_handle(texture_type));
        let key = original.as_deref().map(|original| (
            original as *const _ as usize, snapshot.handle() as *const _ as usize,
        ));
        if let Some(cached) = key.and_then(|key| runtime.sampling_snapshots.views.get(&key)) {
            return Ok(Some(cached.1.clone()));
        }
        // This temporary wrapper cannot outlive the borrowed base. Only its
        // retained native handle escapes; it contains no Rust metadata pointer.
        let snapshot_view = MetalImageView::new(
            NonNull::from(view.base.as_ref()), &view.info, &snapshot,
        )?;
        let handle = snapshot_view.retained_handle(texture_type);
        if let (Some(key), Some(original), Some(handle)) = (key, original, handle.as_ref()) {
            if runtime.sampling_snapshots.views.len() >= SAMPLING_SNAPSHOT_VIEW_LIMIT {
                runtime.sampling_snapshots.views.clear();
            }
            runtime.sampling_snapshots.views.insert(key, (original, handle.clone()));
        }
        Ok(handle)
    }

    /// Called at native write recording sites, not by UpdateRenderTargets'
    /// conservative guest modification tracking. Whole-image invalidation is safe
    /// for partial clears/copies; it may forgo reuse of disjoint subresources.
    pub fn mark_render_target_contents_modified(&self, color_mask: u32, depth_stencil: bool) {
        for (index, view_id) in self.base.render_targets.color_buffer_ids.iter().enumerate() {
            if color_mask & (1 << index) != 0 { self.mark_view_contents_modified(*view_id); }
        }
        if depth_stencil {
            self.mark_view_contents_modified(self.base.render_targets.depth_buffer_id);
        }
    }

    fn mark_view_contents_modified(&self, view_id: ImageViewId) {
        if !view_id.is_valid() || view_id == NULL_IMAGE_VIEW_ID
            || !self.base.slot_image_views.contains(view_id) { return; }
        let image_id = self.base.slot_image_views[view_id].image_id;
        if !image_id.is_valid() || image_id == NULL_IMAGE_ID
            || !self.base.slot_images.contains(image_id) { return; }
        if let Some(image) = self.base.slot_images[image_id].backend.as_ref() {
            image.mark_contents_modified();
        }
    }

    pub fn prepare_retained_image_view(
        &mut self,
        view_id: ImageViewId,
        texture_type: TextureType,
        is_modification: bool,
    ) -> Option<objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_metal::MTLTexture>>>
    {
        if !view_id.is_valid()
            || view_id == NULL_IMAGE_VIEW_ID
            || !self.base.slot_image_views.contains(view_id)
        {
            return None;
        }
        let view = &self.base.slot_image_views[view_id];
        if view.is_buffer() {
            return self.image_view(view_id)?.retained_handle(texture_type);
        }
        let image_id = view.image_id;
        if is_modification {
            self.base.mark_modification_by_id(image_id);
            self.mark_view_contents_modified(view_id);
        }
        synchronize_image_storage(&mut self.base, image_id, texture_type, is_modification)
            .then(|| self.image_view(view_id)?.retained_handle(texture_type))?
    }

    pub fn framebuffer_image_view(
        &mut self,
        config: &crate::framebuffer_config::FramebufferConfig,
        cpu_addr: u64,
    ) -> Option<(
        objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_metal::MTLTexture>>,
        u32,
        u32,
        ImageViewId,
    )> {
        let framebuffer = self
            .base
            .try_find_framebuffer_image_view(config, cpu_addr)?;
        <MetalTextureCacheParams as TextureCacheParams>::prepare_image_view(
            &mut self.base,
            framebuffer.view_id,
            false,
            false,
        );
        let view = self.image_view(framebuffer.view_id)?;
        Some((
            view.retained_handle(TextureType::Color2D)?,
            framebuffer.view.size.width,
            framebuffer.view.size.height,
            framebuffer.view_id,
        ))
    }

    pub fn image_view_buffer_info(
        &self,
        view_id: ImageViewId,
    ) -> Option<(u64, u32, crate::surface::PixelFormat)> {
        if !view_id.is_valid() || view_id == NULL_IMAGE_VIEW_ID {
            return None;
        }
        let view = self.base.slot_image_views[view_id].backend.as_ref()?;
        let base = view.base();
        base.is_buffer().then(|| {
            (
                base.gpu_addr,
                base.size
                    .width
                    .wrapping_mul(crate::surface::bytes_per_block(base.format)),
                base.format,
            )
        })
    }

    /// Metal specialization of upstream `TextureCache::GetImageView(index)`
    /// used by Maxwell's DrawTexture path.
    pub fn draw_texture_source(
        &mut self,
        index: u32,
    ) -> Option<(
        objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_metal::MTLTexture>>,
        u32,
        u32,
        bool,
    )> {
        let mut selected = [ImageViewInOut {
            index,
            blacklist: false,
            id: NULL_IMAGE_VIEW_ID,
        }];
        self.fill_image_views(&mut selected, false, false);
        let view_id = selected[0].id;
        let texture = self.prepare_retained_image_view(view_id, TextureType::Color2D, false)?;
        let view = self.image_view(view_id)?;
        Some((
            texture,
            view.base().size.width,
            view.base().size.height,
            false,
        ))
    }
}

#[derive(Clone, Copy)]
struct NativeImageCopy {
    source_slice: usize,
    source_level: usize,
    source_origin: MTLOrigin,
    source_size: MTLSize,
    destination_slice: usize,
    destination_level: usize,
    destination_origin: MTLOrigin,
    destination_size: MTLSize,
}

struct NativeBufferCopy {
    image: NativeImageCopy,
    buffer_offset: usize,
    bytes_per_row: usize,
    bytes_per_image: usize,
}

fn align_up(value: usize, alignment: usize) -> usize {
    value.div_ceil(alignment).saturating_mul(alignment)
}

fn make_native_image_copies(
    source: &MetalImage,
    destination: &MetalImage,
    copy: &ImageCopy,
) -> Result<Vec<NativeImageCopy>, MetalTextureCacheError> {
    if source.image_type() != destination.image_type() {
        return Err(MetalTextureCacheError::InvalidCopy("image type mismatch"));
    }
    let source_level = usize::try_from(copy.src_subresource.base_level)
        .map_err(|_| MetalTextureCacheError::InvalidCopy("negative source mip"))?;
    let destination_level = usize::try_from(copy.dst_subresource.base_level)
        .map_err(|_| MetalTextureCacheError::InvalidCopy("negative destination mip"))?;
    let source_layer = usize::try_from(copy.src_subresource.base_layer)
        .map_err(|_| MetalTextureCacheError::InvalidCopy("negative source layer"))?;
    let destination_layer = usize::try_from(copy.dst_subresource.base_layer)
        .map_err(|_| MetalTextureCacheError::InvalidCopy("negative destination layer"))?;
    let source_layers = usize::try_from(copy.src_subresource.num_layers)
        .map_err(|_| MetalTextureCacheError::InvalidCopy("negative source layer count"))?;
    let destination_layers = usize::try_from(copy.dst_subresource.num_layers)
        .map_err(|_| MetalTextureCacheError::InvalidCopy("negative destination layer count"))?;
    if source_level >= source.levels() as usize
        || destination_level >= destination.levels() as usize
        || source_layers == 0
        || source_layers != destination_layers
    {
        return Err(MetalTextureCacheError::InvalidCopy("subresource range"));
    }
    let source_origin = checked_origin(copy.src_offset.x, copy.src_offset.y, copy.src_offset.z)?;
    let destination_origin =
        checked_origin(copy.dst_offset.x, copy.dst_offset.y, copy.dst_offset.z)?;
    let source_extent = MTLSize {
        width: copy.extent.width as usize,
        height: copy.extent.height as usize,
        depth: copy.extent.depth as usize,
    };
    if source_extent.width == 0 || source_extent.height == 0 || source_extent.depth == 0 {
        return Err(MetalTextureCacheError::InvalidCopy("zero extent"));
    }
    let source_block_width =
        crate::surface::default_block_width(source.guest_format()).max(1) as usize;
    let source_block_height =
        crate::surface::default_block_height(source.guest_format()).max(1) as usize;
    let destination_block_width =
        crate::surface::default_block_width(destination.guest_format()).max(1) as usize;
    let destination_block_height =
        crate::surface::default_block_height(destination.guest_format()).max(1) as usize;
    let destination_mip_size = mip_size(destination, destination_level);
    let destination_remaining = MTLSize {
        width: destination_mip_size
            .width
            .saturating_sub(destination_origin.x),
        height: destination_mip_size
            .height
            .saturating_sub(destination_origin.y),
        depth: destination_mip_size
            .depth
            .saturating_sub(destination_origin.z),
    };
    let destination_extent = MTLSize {
        width: source_extent
            .width
            .div_ceil(source_block_width)
            .saturating_mul(destination_block_width)
            .min(destination_remaining.width),
        height: source_extent
            .height
            .div_ceil(source_block_height)
            .saturating_mul(destination_block_height)
            .min(destination_remaining.height),
        depth: source_extent.depth.min(destination_remaining.depth),
    };
    if destination_extent.width == 0
        || destination_extent.height == 0
        || destination_extent.depth == 0
    {
        return Err(MetalTextureCacheError::InvalidCopy(
            "destination origin exceeds mip bounds",
        ));
    }
    validate_mip_bounds(source, source_level, source_origin, source_extent)?;
    validate_mip_bounds(
        destination,
        destination_level,
        destination_origin,
        destination_extent,
    )?;

    if source.image_type() == ImageType::E3D {
        if source_layers != 1 {
            return Err(MetalTextureCacheError::InvalidCopy(
                "3D copies require one subresource layer",
            ));
        }
        return Ok(vec![NativeImageCopy {
            source_slice: 0,
            source_level,
            source_origin,
            source_size: source_extent,
            destination_slice: 0,
            destination_level,
            destination_origin,
            destination_size: destination_extent,
        }]);
    }
    if source_origin.z != 0 || destination_origin.z != 0 || source_extent.depth != 1 {
        return Err(MetalTextureCacheError::InvalidCopy(
            "non-3D copies require z=0 and depth=1",
        ));
    }
    if source_layer + source_layers > source.layers() as usize
        || destination_layer + destination_layers > destination.layers() as usize
    {
        return Err(MetalTextureCacheError::InvalidCopy("array layer range"));
    }
    Ok((0..source_layers)
        .map(|layer| NativeImageCopy {
            source_slice: source_layer + layer,
            source_level,
            source_origin,
            source_size: source_extent,
            destination_slice: destination_layer + layer,
            destination_level,
            destination_origin,
            destination_size: destination_extent,
        })
        .collect())
}

fn checked_origin(x: i32, y: i32, z: i32) -> Result<MTLOrigin, MetalTextureCacheError> {
    Ok(MTLOrigin {
        x: usize::try_from(x).map_err(|_| MetalTextureCacheError::InvalidCopy("negative x"))?,
        y: usize::try_from(y).map_err(|_| MetalTextureCacheError::InvalidCopy("negative y"))?,
        z: usize::try_from(z).map_err(|_| MetalTextureCacheError::InvalidCopy("negative z"))?,
    })
}

fn validate_mip_bounds(
    image: &MetalImage,
    level: usize,
    origin: MTLOrigin,
    extent: MTLSize,
) -> Result<(), MetalTextureCacheError> {
    let size = image.size();
    let width = (size.0 as usize >> level).max(1);
    let height = (size.1 as usize >> level).max(1);
    let depth = (size.2 as usize >> level).max(1);
    if origin.x.saturating_add(extent.width) > width
        || origin.y.saturating_add(extent.height) > height
        || (image.image_type() == ImageType::E3D && origin.z.saturating_add(extent.depth) > depth)
    {
        return Err(MetalTextureCacheError::InvalidCopy(
            "copy exceeds mip bounds",
        ));
    }
    Ok(())
}

fn mip_size(image: &MetalImage, level: usize) -> MTLSize {
    let size = image.size();
    MTLSize {
        width: (size.0 as usize >> level).max(1),
        height: (size.1 as usize >> level).max(1),
        depth: (size.2 as usize >> level).max(1),
    }
}

fn guest_image_info(image: &MetalImage) -> ImageInfo {
    let (samples_x, samples_y) =
        crate::texture_cache::samples_helper::samples_log2(image.guest_samples().max(1) as i32);
    ImageInfo {
        format: image.guest_format(),
        image_type: image.image_type(),
        size: Extent3D {
            width: image.size().0 << samples_x.max(0) as u32,
            height: image.size().1 << samples_y.max(0) as u32,
            depth: image.size().2,
        },
        resources: SubresourceExtent {
            levels: image.levels() as i32,
            layers: image.layers() as i32,
        },
        num_samples: image.guest_samples(),
        ..ImageInfo::default()
    }
}

fn msaa_scratch_info(info: &ImageInfo) -> ImageInfo {
    let mut scratch = info.clone();
    scratch.num_samples = 1;
    scratch
}

/// Aspect/format rules of Metal `CopyMSAA` downloads. Native sample count
/// must match the guest count so the expanded grid stays bit-identical.
fn can_download_msaa_info(info: &ImageInfo, native_samples: u32) -> bool {
    if !matches!(info.num_samples, 2 | 4 | 8 | 16) || native_samples != info.num_samples {
        return false;
    }
    if !matches!(info.image_type, ImageType::E2D | ImageType::Linear) {
        return false;
    }
    if crate::surface::default_block_width(info.format) > 1
        || crate::surface::default_block_height(info.format) > 1
    {
        return false;
    }
    if get_format_type(info.format) != SurfaceType::ColorTexture
        || crate::surface::is_pixel_format_integer(info.format)
    {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer_metal::metal_buffer::MetalBuffer;
    use crate::surface::PixelFormat;
    use crate::texture_cache::image_info::ImageInfo;
    use crate::texture_cache::types::{
        BufferImageCopy, Extent3D, SubresourceExtent, SubresourceLayers,
    };

    fn image_with_format(device: &MetalDevice, format: PixelFormat) -> MetalImage {
        image_with_format_and_size(device, format, 4, 4)
    }

    fn image_with_format_and_size(
        device: &MetalDevice,
        format: PixelFormat,
        width: u32,
        height: u32,
    ) -> MetalImage {
        image_with_format_size_and_levels(device, format, width, height, 1)
    }

    fn image_with_format_size_and_levels(
        device: &MetalDevice,
        format: PixelFormat,
        width: u32,
        height: u32,
        levels: i32,
    ) -> MetalImage {
        MetalImage::new(
            device,
            &ImageInfo {
                format,
                image_type: ImageType::E2D,
                resources: SubresourceExtent { levels, layers: 1 },
                size: Extent3D {
                    width,
                    height,
                    depth: 1,
                },
                num_samples: 1,
                ..ImageInfo::default()
            },
        )
        .unwrap()
    }

    fn image(device: &MetalDevice) -> MetalImage {
        image_with_format(device, PixelFormat::A8B8G8R8Unorm)
    }

    fn multisample_image(device: &MetalDevice) -> MetalImage {
        MetalImage::new(
            device,
            &ImageInfo {
                format: PixelFormat::A8B8G8R8Unorm,
                image_type: ImageType::E2D,
                resources: SubresourceExtent {
                    levels: 1,
                    layers: 1,
                },
                // Maxwell stores the sample-expanded dimensions. MetalImage
                // converts these back to the logical 4x4 attachment extent.
                size: Extent3D {
                    width: 8,
                    height: 8,
                    depth: 1,
                },
                num_samples: 4,
                ..ImageInfo::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn frame_gc_writes_gpu_pixels_before_eviction_and_retains_failed_downloads() {
        use crate::texture_cache::image_base::ImageFlagBits;
        use crate::texture_cache::image_info::TilingMode;
        for fail_download in [false, true] {
            let device = MetalDevice::new().unwrap();
            let mut scheduler = MetalScheduler::new(&device);
            let mut staging = MetalStagingBufferPool::new(&device).unwrap();
            let mut blit = MetalBlitHelper::new(&device).unwrap();
            let mut cache = MetalTextureCache::new(
                device.clone(), Arc::new(MaxwellDeviceMemoryManager::default()),
                &mut scheduler, &mut staging, &mut blit,
            );
            let info = ImageInfo {
                format: PixelFormat::A8B8G8R8Unorm, image_type: ImageType::Linear,
                tiling: TilingMode::PitchLinear(16),
                size: Extent3D { width: 4, height: 4, depth: 1 },
                resources: SubresourceExtent { levels: 1, layers: 1 }, num_samples: 1,
                ..ImageInfo::default()
            };
            let memory = Arc::new(parking_lot::Mutex::new(
                crate::memory_manager::MemoryManager::new(17),
            ));
            memory.lock().map(0x10000, 0x100000, 0x10000, 0, true);
            memory.lock().map(0x20000, 0x200000, 0x10000, 0, true);
            cache.base.set_channel_gpu_memory(memory);
            let id = cache.base.insert_image(&info, 0x10000);
            let recent = cache.base.insert_image(&info, 0x20000);
            // Observe the common CPU-address writeback adapter below. Image
            // registration has already retained its GPU page-table owner.
            cache.base.channel_gpu_memory = None;
            cache.base.channel_gpu_memory_handle = None;
            let image = MetalImage::new(&device, &info).unwrap();
            let pass = MTLRenderPassDescriptor::new();
            let color = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
            color.setTexture(Some(image.handle()));
            color.setLoadAction(MTLLoadAction::Clear);
            color.setStoreAction(MTLStoreAction::Store);
            color.setClearColor(objc2_metal::MTLClearColor { red: 1.0, green: 0.0, blue: 0.0, alpha: 1.0 });
            pass.setRenderTargetWidth(4);
            pass.setRenderTargetHeight(4);
            pass.setDefaultRasterSampleCount(1);
            scheduler.begin_render_pass(&pass).unwrap();
            scheduler.end_render_pass();
            cache.base.slot_images[id].backend = Some(image);
            cache.base.slot_images[id].flags.remove(ImageFlagBits::CPU_MODIFIED);
            cache.base.mark_modification_by_id(id);
            let address = cache.base.slot_images[id].cpu_addr;
            if fail_download {
                // A real native validation failure, not a stub downloader.
                cache.base.slot_images[id].unswizzled_size_bytes = 1;
            }
            let writes = Arc::new(parking_lot::Mutex::new(Vec::<(u64, Vec<u8>)>::new()));
            let captured = writes.clone();
            cache.base.set_guest_memory_writer(Arc::new(move |addr, bytes| {
                captured.lock().push((addr, bytes.to_vec()));
            }));
            cache.base.frame_tick = 100;
            cache.base.touch_image(recent);
            cache.base.minimum_memory = 0;
            cache.base.expected_memory = 0;
            cache.base.critical_memory = u64::MAX;
            let before = cache.base.total_used_memory;
            cache.tick_frame();
            assert!(cache.base.slot_images.contains(recent), "recently used images must survive GC");
            assert_eq!(cache.base.slot_images.contains(id), fail_download);
            if fail_download {
                assert!(writes.lock().is_empty());
                assert!(cache.base.slot_images[id].flags.contains(ImageFlagBits::GPU_MODIFIED));
                assert!(cache.base.slot_images[id].backend.is_some());
                assert_eq!(cache.base.total_used_memory, before);
            } else {
                let writes = writes.lock();
                assert!(!writes.is_empty());
                let mut pixels = [0xcd; 64];
                for (addr, bytes) in writes.iter() {
                    let offset = (*addr - address) as usize;
                    pixels[offset..offset + bytes.len()].copy_from_slice(bytes);
                }
                assert!(pixels.chunks_exact(4).all(|pixel| pixel == [255, 0, 0, 255]));
                assert!(cache.base.total_used_memory < before);
                assert_eq!(cache.base.sentenced_images.retained_len(), 1);
            }
            scheduler.finish_all().unwrap();
        }
    }

    #[test]
    fn frame_tick_retires_native_images_views_and_framebuffers_without_losing_commands() {
        use crate::texture_cache::image_base::ImageBase;
        use crate::texture_cache::texture_cache_base::{ImageSlot, ImageViewSlot, TICKS_TO_DESTROY};
        use crate::texture_cache::render_targets::RenderTargets;
        use common::slot_vector::SlotId;

        let device = MetalDevice::new().unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let mut staging = MetalStagingBufferPool::new(&device).unwrap();
        let mut blit = MetalBlitHelper::new(&device).unwrap();
        let mut cache = MetalTextureCache::new(
            device.clone(), Arc::new(MaxwellDeviceMemoryManager::default()),
            &mut scheduler, &mut staging, &mut blit,
        );
        let info = ImageInfo {
            format: PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            size: Extent3D { width: 4, height: 4, depth: 1 },
            resources: SubresourceExtent { levels: 1, layers: 1 },
            num_samples: 1,
            ..ImageInfo::default()
        };
        let native = MetalImage::new(&device, &info).unwrap();
        let upload = MetalBuffer::new(&device, 64).unwrap();
        let download = MetalBuffer::new(&device, 64).unwrap();
        let pixels: Vec<u8> = (0..64).collect();
        upload.write(0, &pixels).unwrap();
        let copy = BufferImageCopy {
            buffer_size: 64,
            image_extent: info.size,
            ..BufferImageCopy::default()
        };
        native.upload_memory(&mut scheduler, &upload, 0, &[copy]).unwrap();
        native.download_memory(&mut scheduler, &download, 0, &[copy]).unwrap();
        let view_info = ImageViewInfo {
            view_type: ImageViewType::E2D,
            format: info.format,
            ..ImageViewInfo::default()
        };
        let mut view = ImageViewSlot::pending(
            view_info.clone(),
            ImageViewBase::new(&view_info, &info, SlotId { index: 1 }, 0x10000),
        );
        let native_view = MetalImageView::new(NonNull::from(view.base.as_mut()), &view_info, &native).unwrap();
        let mut colors = [None; crate::texture_cache::types::NUM_RT];
        colors[0] = Some(&native_view);
        let framebuffer = MetalFramebuffer::new(colors, None, &RenderTargets::default()).unwrap();
        view.backend = Some(MetalCachedImageView::Image(native_view));
        let mut image = ImageSlot::pending(ImageBase::new(info, 0x10000, 0x20000));
        image.backend = Some(native);
        cache.base.sentenced_images.push(image);
        cache.base.sentenced_image_view.push(view);
        cache.base.sentenced_framebuffers.push(Box::new(framebuffer));
        for _ in 1..TICKS_TO_DESTROY {
            cache.tick_frame();
            assert_eq!(cache.base.sentenced_images.retained_len(), 1);
            assert_eq!(cache.base.sentenced_image_view.retained_len(), 1);
            assert_eq!(cache.base.sentenced_framebuffers.retained_len(), 1);
        }
        cache.tick_frame();
        assert_eq!(cache.base.frame_tick, TICKS_TO_DESTROY as u64);
        assert_eq!(cache.base.sentenced_images.retained_len(), 0);
        assert_eq!(cache.base.sentenced_image_view.retained_len(), 0);
        assert_eq!(cache.base.sentenced_framebuffers.retained_len(), 0);
        // The still-unsubmitted command buffer must own its image references.
        scheduler.finish_all().unwrap();
        let mut actual = [0; 64];
        download.read(0, &mut actual).unwrap();
        assert_eq!(actual.as_slice(), pixels);
    }

    #[test]
    fn render_target_content_revision_changes_only_for_selected_writers() {
        use crate::texture_cache::image_base::ImageBase;
        use crate::texture_cache::texture_cache_base::{ImageSlot, ImageViewSlot};
        let device = MetalDevice::new().unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let mut staging = MetalStagingBufferPool::new(&device).unwrap();
        let mut blit = MetalBlitHelper::new(&device).unwrap();
        let mut cache = MetalTextureCache::new(device.clone(), Arc::new(MaxwellDeviceMemoryManager::default()),
            &mut scheduler, &mut staging, &mut blit);
        let info = ImageInfo {
            format: PixelFormat::D32Float, image_type: ImageType::E2D,
            size: Extent3D { width: 4, height: 4, depth: 1 },
            resources: SubresourceExtent { levels: 1, layers: 1 }, num_samples: 1,
            ..ImageInfo::default()
        };
        let native = MetalImage::new(&device, &info).unwrap();
        let mut image = ImageSlot::pending(ImageBase::new(info.clone(), 0x10000, 0x20000));
        image.backend = Some(native);
        let id = cache.base.slot_images.insert(image);
        let view_info = ImageViewInfo { view_type: ImageViewType::E2D, format: info.format, ..ImageViewInfo::default() };
        let view = ImageViewSlot::pending(view_info.clone(), ImageViewBase::new(&view_info, &info, id, 0x10000));
        let view_id = cache.base.slot_image_views.insert(view);
        cache.base.render_targets.depth_buffer_id = view_id;
        cache.mark_render_target_contents_modified(u32::MAX, false);
        assert_eq!(cache.base.slot_images[id].backend.as_ref().unwrap().content_revision(), Some(0));
        cache.mark_render_target_contents_modified(0, true);
        assert_eq!(cache.base.slot_images[id].backend.as_ref().unwrap().content_revision(), Some(1));
        cache.base.render_targets.depth_buffer_id = NULL_IMAGE_VIEW_ID;
        cache.base.render_targets.color_buffer_ids[2] = view_id;
        cache.mark_render_target_contents_modified(1, false);
        assert_eq!(cache.base.slot_images[id].backend.as_ref().unwrap().content_revision(), Some(1));
        cache.mark_render_target_contents_modified(1 << 2, false);
        assert_eq!(cache.base.slot_images[id].backend.as_ref().unwrap().content_revision(), Some(2));
    }

    #[test]
    fn g4r4_native_roundtrip_covers_every_packed_value_and_padding() {
        let device = MetalDevice::new().unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let mut pool = MetalStagingBufferPool::new(&device).unwrap();
        let mut blit = MetalBlitHelper::new(&device).unwrap();
        let mut runtime = MetalTextureCacheRuntime::new(device.clone(), &mut scheduler, &mut pool, &mut blit);
        let image = MetalImage::new(&device, &ImageInfo {
            format: PixelFormat::G4R4Unorm, image_type: ImageType::E2D,
            size: Extent3D { width: 32, height: 32, depth: 1 },
            resources: SubresourceExtent { levels: 2, layers: 3 }, num_samples: 1,
            ..ImageInfo::default()
        }).unwrap();
        let copy = BufferImageCopy {
            buffer_offset: 16, buffer_size: 322, buffer_row_length: 18, buffer_image_height: 10,
            image_subresource: SubresourceLayers { base_level: 1, base_layer: 1, num_layers: 2 },
            image_extent: Extent3D { width: 16, height: 8, depth: 1 },
            ..BufferImageCopy::default()
        };
        let mut guest = [0xcd; 400];
        for layer in 0..2 {
            for y in 0..8 {
                for x in 0..16 {
                    guest[16 + (layer * 10 + y) * 18 + x] = (layer * 128 + y * 16 + x) as u8;
                }
            }
        }
        let (native_copies, size) = converted_copy_layout(&[copy], 2).unwrap();
        let mut expanded = vec![0xa5; size];
        convert_uncompressed_memory(PixelFormat::G4R4Unorm, &guest, &mut expanded, &[copy], true).unwrap();
        let upload = MetalBuffer::new(&device, size + 32).unwrap();
        upload.write(32, &expanded).unwrap();
        let download = MetalBuffer::new(&device, size + 32).unwrap();
        image.upload_converted_memory(runtime.scheduler(), &upload, 32, &native_copies, 2).unwrap();
        image.download_converted_memory(runtime.scheduler(), &download, 32, &native_copies).unwrap();
        let mut output = [0xcd; 400];
        runtime.download_single_sample_memory(&image, &mut output, &[copy]).unwrap();
        assert_eq!(output, guest);
        let mut native = vec![0; size];
        download.read(32, &mut native).unwrap();
        for layer in 0..2 {
            for y in 0..8 {
                for x in 0..16 {
                    let offset = ((layer * 10 + y) * 18 + x) * 2;
                    assert_eq!(&native[offset..offset + 2], &[x as u8 * 17, (layer * 8 + y) as u8 * 17]);
                }
            }
        }
    }

    #[test]
    fn g4r4_download_quantizes_all_native_byte_pairs_to_nearest_unorm4() {
        let copy = BufferImageCopy {
            buffer_size: 65536,
            image_subresource: SubresourceLayers { base_level: 0, base_layer: 0, num_layers: 1 },
            image_extent: Extent3D { width: 65536, height: 1, depth: 1 },
            ..BufferImageCopy::default()
        };
        let native: Vec<u8> = (0..=u16::MAX).flat_map(u16::to_le_bytes).collect();
        let mut guest = vec![0; 65536];
        convert_uncompressed_memory(PixelFormat::G4R4Unorm, &native, &mut guest, &[copy], false).unwrap();
        for (i, value) in guest.into_iter().enumerate() {
            let r = (((i & 255) as f64 / 255.0) * 15.0).round() as u8;
            let g = (((i >> 8) as f64 / 255.0) * 15.0).round() as u8;
            assert_eq!(value, r | (g << 4));
        }
    }

    #[test]
    fn rgb32_transfer_preserves_float_bits_layers_and_padding() {
        let device = MetalDevice::new().unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let mut pool = MetalStagingBufferPool::new(&device).unwrap();
        let mut blit = MetalBlitHelper::new(&device).unwrap();
        let mut runtime = MetalTextureCacheRuntime::new(device.clone(), &mut scheduler, &mut pool, &mut blit);
        let image = MetalImage::new(&device, &ImageInfo {
            format: PixelFormat::R32G32B32Float, image_type: ImageType::E2D,
            size: Extent3D { width: 8, height: 8, depth: 1 },
            resources: SubresourceExtent { levels: 2, layers: 3 }, num_samples: 1,
            ..ImageInfo::default()
        }).unwrap();
        let copy = BufferImageCopy {
            buffer_offset: 16, buffer_size: 168, buffer_row_length: 3, buffer_image_height: 3,
            image_subresource: SubresourceLayers { base_level: 1, base_layer: 1, num_layers: 2 },
            image_extent: Extent3D { width: 2, height: 2, depth: 1 },
            ..BufferImageCopy::default()
        };
        let mut guest = [0xcd; 256];
        for layer in 0..2 {
            for y in 0..2 {
                for x in 0..2 {
                    let offset = 16 + ((layer * 3 + y) * 3 + x) * 12;
                    let words = [0x80000000u32, 0x7fc12345 + layer as u32, 0x3f800000 + (x + y) as u32];
                    for (i, word) in words.into_iter().enumerate() {
                        guest[offset + i * 4..offset + (i + 1) * 4].copy_from_slice(&word.to_le_bytes());
                    }
                }
            }
        }
        let (native_copies, size) = converted_copy_layout(&[copy], 16).unwrap();
        let mut expanded = vec![0xa5; size];
        convert_uncompressed_memory(PixelFormat::R32G32B32Float, &guest, &mut expanded, &[copy], true).unwrap();
        let upload = MetalBuffer::new(&device, size + 32).unwrap();
        upload.write(32, &expanded).unwrap();
        let tick = runtime.scheduler().current_tick();
        image.upload_converted_memory(runtime.scheduler(), &upload, 32, &native_copies, 16).unwrap();
        let download = MetalBuffer::new(&device, size + 32).unwrap();
        image.download_converted_memory(runtime.scheduler(), &download, 32, &native_copies).unwrap();
        assert_eq!(runtime.scheduler().current_tick(), tick);
        let mut actual = [0xcd; 256];
        runtime.download_single_sample_memory(&image, &mut actual, &[copy]).unwrap();
        assert_eq!(actual, guest);
        let mut native = vec![0; size];
        download.read(32, &mut native).unwrap();
        for layer in 0..2 {
            for y in 0..2 {
                for x in 0..2 {
                    let offset = ((layer * 3 + y) * 3 + x) * 16;
                    assert_eq!(&native[offset..offset + 16], &expanded[offset..offset + 16]);
                    assert_eq!(&native[offset + 12..offset + 16], &1.0f32.to_le_bytes());
                }
            }
        }
    }

    #[test]
    fn rgb32_conversion_rejects_bad_copies_without_partial_writes() {
        let copy = BufferImageCopy {
            buffer_size: 12,
            image_subresource: SubresourceLayers { base_level: 0, base_layer: 0, num_layers: 1 },
            image_extent: Extent3D { width: 1, height: 1, depth: 1 },
            ..BufferImageCopy::default()
        };
        for upload in [false, true] {
            for bad in [
                BufferImageCopy { buffer_offset: usize::MAX, ..copy },
                BufferImageCopy { buffer_size: 11, ..copy },
                BufferImageCopy { image_extent: Extent3D { width: 2, height: 1, depth: 1 }, buffer_row_length: 1, ..copy },
                BufferImageCopy { image_subresource: SubresourceLayers { num_layers: 0, ..copy.image_subresource }, ..copy },
            ] {
                let mut output = [0xcd; 64];
                assert!(convert_uncompressed_memory(PixelFormat::R32G32B32Float, &[0; 64], &mut output, &[copy, bad], upload).is_err());
                assert_eq!(output, [0xcd; 64]);
            }
        }
        let mut output = [0xcd; 16];
        assert!(convert_uncompressed_memory(PixelFormat::R32G32B32Float, &[0; 11], &mut output, &[copy], true).is_err());
        assert_eq!(output, [0xcd; 16]);
    }

    #[test]
    fn x8d24_native_transfer_preserves_depth_layers_and_padding() {
        let device = MetalDevice::new().unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let mut pool = MetalStagingBufferPool::new(&device).unwrap();
        let mut blit = MetalBlitHelper::new(&device).unwrap();
        let mut runtime = MetalTextureCacheRuntime::new(device.clone(), &mut scheduler, &mut pool, &mut blit);
        let image = MetalImage::new(&device, &ImageInfo {
            format: PixelFormat::X8D24Unorm, image_type: ImageType::E2D,
            size: Extent3D { width: 8, height: 8, depth: 1 },
            resources: SubresourceExtent { levels: 2, layers: 3 }, num_samples: 1,
            ..ImageInfo::default()
        }).unwrap();
        let copy = BufferImageCopy {
            buffer_offset: 16, buffer_size: 76, buffer_row_length: 3, buffer_image_height: 3,
            image_subresource: SubresourceLayers { base_level: 1, base_layer: 1, num_layers: 2 },
            image_extent: Extent3D { width: 2, height: 3, depth: 1 },
            ..BufferImageCopy::default()
        };
        let depths = [0u32, 1, 0x7fffff, 0x800000, 0xfffffe, 0xffffff];
        let mut guest = [0xcd; 128];
        let mut expected = guest;
        for layer in 0..2 {
            for y in 0..3 {
                for x in 0..2 {
                    let offset = 16 + ((layer * 3 + y) * 3 + x) * 4;
                    let depth = depths[(y * 2 + x + layer) % depths.len()];
                    guest[offset..offset + 4].copy_from_slice(&(depth | 0xab000000).to_le_bytes());
                    expected[offset..offset + 4].copy_from_slice(&depth.to_le_bytes());
                }
            }
        }
        let (copies, size) = converted_copy_layout(&[copy], 4).unwrap();
        let mut native = vec![0; size];
        convert_uncompressed_memory(PixelFormat::X8D24Unorm, &guest, &mut native, &[copy], true).unwrap();
        let upload = MetalBuffer::new(&device, size + 32).unwrap();
        upload.write(32, &native).unwrap();
        let readback = MetalBuffer::new(&device, size + 32).unwrap();
        let tick = runtime.scheduler().current_tick();
        image.upload_converted_memory(runtime.scheduler(), &upload, 32, &copies, 4).unwrap();
        image.download_converted_memory(runtime.scheduler(), &readback, 32, &copies).unwrap();
        assert_eq!(runtime.scheduler().current_tick(), tick);
        let mut actual = [0xcd; 128];
        runtime.download_single_sample_memory(&image, &mut actual, &[copy]).unwrap();
        assert_eq!(actual, expected);
        readback.read(32, &mut native).unwrap();
        for layer in 0..2 {
            for y in 0..3 {
                for x in 0..2 {
                    let offset = ((layer * 3 + y) * 3 + x) * 4;
                    let value = f32::from_le_bytes(native[offset..offset + 4].try_into().unwrap());
                    let expected = (depths[(y * 2 + x + layer) % depths.len()] as f64 / 16777215.0) as f32;
                    assert_eq!(value, expected);
                }
            }
        }
    }

    #[test]
    fn x8d24_quantization_roundtrips_every_depth_value() {
        for value in 0..=0xffffffu32 {
            let depth = value as f32 / 16777215.0;
            assert_eq!(depth_float_to_unorm24(depth), value);
        }
        let copy = BufferImageCopy {
            buffer_size: 4,
            image_subresource: SubresourceLayers { base_level: 0, base_layer: 0, num_layers: 1 },
            image_extent: Extent3D { width: 1, height: 1, depth: 1 },
            ..BufferImageCopy::default()
        };
        for upload in [true, false] {
            let mut output = [0xcd; 16];
            let invalid = BufferImageCopy { buffer_offset: usize::MAX, ..copy };
            assert!(convert_uncompressed_memory(PixelFormat::X8D24Unorm, &[0; 16], &mut output, &[copy, invalid], upload).is_err());
            assert_eq!(output, [0xcd; 16]);
        }
    }

    #[test]
    fn packed_1555_and_5551_gpu_reads_match_all_guest_words() {
        use objc2_foundation::NSString;
        use objc2_metal::{MTLDevice, MTLLibrary, MTLComputeCommandEncoder};
        use crate::texture_cache::image_view_info::SwizzleSource;
        let device = MetalDevice::new().unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let mut pool = MetalStagingBufferPool::new(&device).unwrap();
        let mut blit = MetalBlitHelper::new(&device).unwrap();
        let mut runtime = MetalTextureCacheRuntime::new(device.clone(), &mut scheduler, &mut pool, &mut blit);
        let library = device.device().newLibraryWithSource_options_error(&NSString::from_str(r#"
#include <metal_stdlib>
using namespace metal;
kernel void inspect_packed(texture2d<float, access::read> image [[texture(0)]],
                           device uint4* result [[buffer(0)]], constant float4& scale [[buffer(1)]],
                           uint i [[thread_position_in_grid]]) {
    result[i] = uint4(round(image.read(uint2(i % 256, i / 256)) * scale));
}
"#), None).unwrap();
        let function = library.newFunctionWithName(&NSString::from_str("inspect_packed")).unwrap();
        let pipeline = device.device().newComputePipelineStateWithFunction_error(&function).unwrap();
        let copy = BufferImageCopy {
            buffer_size: 65536 * 2,
            image_subresource: SubresourceLayers { base_level: 0, base_layer: 0, num_layers: 1 },
            image_extent: Extent3D { width: 256, height: 256, depth: 1 },
            ..BufferImageCopy::default()
        };
        let guest: Vec<u8> = (0..=u16::MAX).flat_map(u16::to_le_bytes).collect();
        for format in [PixelFormat::A1B5G5R5Unorm, PixelFormat::A1R5G5B5Unorm, PixelFormat::A5B5G5R1Unorm] {
            let image_info = ImageInfo {
                format, image_type: ImageType::E2D, size: copy.image_extent,
                resources: SubresourceExtent { levels: 1, layers: 1 }, num_samples: 1,
                ..ImageInfo::default()
            };
            let image = MetalImage::new(&device, &image_info).unwrap();
            let view_info = ImageViewInfo {
                format, view_type: ImageViewType::E2D,
                x_source: SwizzleSource::R as u8, y_source: SwizzleSource::G as u8,
                z_source: SwizzleSource::B as u8, w_source: SwizzleSource::A as u8,
                ..ImageViewInfo::default()
            };
            let mut view_base = Box::new(ImageViewBase::new(&view_info, &image_info, ImageId { index: 1 }, 0x1000));
            let view = MetalImageView::new(NonNull::from(view_base.as_mut()), &view_info, &image).unwrap();
            let upload = MetalBuffer::new(&device, guest.len() + 32).unwrap();
            if format == PixelFormat::A1B5G5R5Unorm {
                let mut native = vec![0; guest.len()];
                let copies = convert_packed16_upload(format, &guest, &mut native, &[copy]).unwrap();
                upload.write(32, &native).unwrap();
                image.upload_converted_memory(runtime.scheduler(), &upload, 32, &copies, 2).unwrap();
            } else {
                upload.write(32, &guest).unwrap();
                image.upload_memory(runtime.scheduler(), &upload, 32, &[copy]).unwrap();
            }
            let result = MetalBuffer::new(&device, 65536 * 16).unwrap();
            let scale = MetalBuffer::new(&device, 16).unwrap();
            let scales = if format == PixelFormat::A5B5G5R1Unorm {
                [1.0f32, 31.0, 31.0, 31.0]
            } else { [31.0f32, 31.0, 31.0, 1.0] };
            scale.write(0, &scales.into_iter().flat_map(f32::to_le_bytes).collect::<Vec<_>>()).unwrap();
            runtime.scheduler().with_compute_encoder(|encoder| unsafe {
                encoder.setComputePipelineState(&pipeline);
                encoder.setTexture_atIndex(view.handle(TextureType::Color2D), 0);
                encoder.setBuffer_offset_atIndex(Some(result.handle()), 0, 0);
                encoder.setBuffer_offset_atIndex(Some(scale.handle()), 0, 1);
                encoder.dispatchThreads_threadsPerThreadgroup(
                    MTLSize { width: 65536, height: 1, depth: 1 },
                    MTLSize { width: 32, height: 1, depth: 1 },
                );
            }).unwrap();
            let mut restored = vec![0xcd; guest.len()];
            runtime.download_single_sample_memory(&image, &mut restored, &[copy]).unwrap();
            assert_eq!(restored, guest);
            let mut pixels = vec![0; 65536 * 16];
            result.read(0, &mut pixels).unwrap();
            for (word, pixel) in pixels.chunks_exact(16).enumerate() {
                let (red, blue) = if format == PixelFormat::A1B5G5R5Unorm {
                    (word % 32, word / 1024 % 32)
                } else { (word / 1024 % 32, word % 32) };
                let expected = if format == PixelFormat::A5B5G5R1Unorm {
                    [word % 2, word / 2 % 32, word / 64 % 32, word / 2048]
                } else { [red, word / 32 % 32, blue, word / 32768] };
                for (component, expected) in pixel.chunks_exact(4).zip(expected) {
                    assert_eq!(u32::from_le_bytes(component.try_into().unwrap()), expected as u32,
                        "format={format:?} guest={word:#06x}");
                }
            }
        }
    }

    #[test]
    fn downloads_b5g6r5_mip_layers_without_overwriting_padding() {
        let device = MetalDevice::new().unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let mut pool = MetalStagingBufferPool::new(&device).unwrap();
        let mut blit = MetalBlitHelper::new(&device).unwrap();
        let mut runtime = MetalTextureCacheRuntime::new(device.clone(), &mut scheduler, &mut pool, &mut blit);
        let image = MetalImage::new(&device, &ImageInfo {
            format: PixelFormat::B5G6R5Unorm,
            image_type: ImageType::E2D,
            size: Extent3D { width: 8, height: 8, depth: 1 },
            resources: SubresourceExtent { levels: 2, layers: 3 },
            num_samples: 1,
            ..ImageInfo::default()
        }).unwrap();
        let copy = BufferImageCopy {
            buffer_offset: 16,
            buffer_size: 104,
            buffer_row_length: 6,
            buffer_image_height: 5,
            image_subresource: SubresourceLayers { base_level: 1, base_layer: 1, num_layers: 2 },
            image_extent: Extent3D { width: 4, height: 4, depth: 1 },
            ..BufferImageCopy::default()
        };
        let source = MetalBuffer::new(&device, 192).unwrap();
        let destination = MetalBuffer::new(&device, 192).unwrap();
        let mut native = [0xa5; 144];
        let mut expected = [0xcd; 144];
        for layer in 0..2 {
            for y in 0..4 {
                for x in 0..4 {
                    let offset = 16 + ((layer * 5 + y) * 6 + x) * 2;
                    let index = (x + y + layer) % 4;
                    let word = [0x001fu16, 0x07e0, 0xf800, 0xffff][index];
                    native[offset..offset + 2].copy_from_slice(&word.to_le_bytes());
                    let word = [0xf800u16, 0x07e0, 0x001f, 0xffff][index];
                    expected[offset..offset + 2].copy_from_slice(&word.to_le_bytes());
                }
            }
        }
        source.write(32, &native).unwrap();
        destination.write(0, &[0xcd; 192]).unwrap();
        let tick = runtime.scheduler().current_tick();
        image.upload_converted_memory(runtime.scheduler(), &source, 32, &[copy], 2).unwrap();
        image.download_packed16_memory(runtime.scheduler(), &destination, 32, &[copy]).unwrap();
        assert_eq!(runtime.scheduler().current_tick(), tick, "native transfers must not submit or wait");
        let mut actual = [0xcd; 144];
        runtime.download_single_sample_memory(&image, &mut actual, &[copy]).unwrap();
        assert_eq!(actual, expected);
        let mut transferred = [0; 192];
        destination.read(0, &mut transferred).unwrap();
        assert_eq!(&transferred[..32], &[0xcd; 32]);
        assert_eq!(&transferred[176..], &[0xcd; 16]);
        let mut unpacked = [0xcd; 144];
        convert_packed16_download(PixelFormat::B5G6R5Unorm, &transferred[32..176], &mut unpacked, &[copy]).unwrap();
        assert_eq!(unpacked, expected);
    }

    #[test]
    fn b5g6r5_download_rejects_bad_ranges_before_writing() {
        let copy = BufferImageCopy {
            buffer_offset: 2,
            buffer_size: 2,
            image_subresource: SubresourceLayers { base_level: 0, base_layer: 0, num_layers: 1 },
            image_extent: Extent3D { width: 1, height: 1, depth: 1 },
            ..BufferImageCopy::default()
        };
        let mut output = [0xcd; 8];
        for bad in [
            BufferImageCopy { buffer_offset: usize::MAX, ..copy },
            BufferImageCopy { buffer_size: 1, ..copy },
            BufferImageCopy { image_extent: Extent3D { width: 0, height: 1, depth: 1 }, ..copy },
            BufferImageCopy { image_extent: Extent3D { width: 2, height: 1, depth: 1 }, buffer_row_length: 1, ..copy },
            BufferImageCopy { image_subresource: SubresourceLayers { num_layers: 0, ..copy.image_subresource }, ..copy },
        ] {
            assert!(convert_packed16_download(PixelFormat::B5G6R5Unorm, &[0; 8], &mut output, &[copy, bad]).is_err());
            assert_eq!(output, [0xcd; 8]);
        }
        assert!(convert_packed16_download(PixelFormat::B5G6R5Unorm, &[0; 3], &mut output, &[copy]).is_err());
        assert_eq!(output, [0xcd; 8]);
    }

    #[test]
    fn downloads_converted_depth_stencil_planes_with_mips_layers_and_staging_offset() {
        let device = MetalDevice::new().unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let mut pool = MetalStagingBufferPool::new(&device).unwrap();
        let mut blit = MetalBlitHelper::new(&device).unwrap();
        let mut runtime = MetalTextureCacheRuntime::new(device.clone(), &mut scheduler, &mut pool, &mut blit);
        for format in [PixelFormat::D24UnormS8Uint, PixelFormat::S8UintD24Unorm] {
            let image = MetalImage::new(&device, &ImageInfo {
                format,
                image_type: ImageType::E2D,
                size: Extent3D { width: 8, height: 8, depth: 1 },
                resources: SubresourceExtent { levels: 2, layers: 2 },
                num_samples: 1,
                ..ImageInfo::default()
            }).unwrap();
            let copy = BufferImageCopy {
                buffer_offset: 16,
                buffer_size: 128,
                image_subresource: SubresourceLayers { base_level: 1, base_layer: 0, num_layers: 2 },
                image_extent: Extent3D { width: 4, height: 4, depth: 1 },
                ..BufferImageCopy::default()
            };
            let mut guest = vec![0; 144];
            for i in 0..32usize {
                let depth = [0, 1, 0x7fffff, 0xffffff][i % 4];
                let stencil = (255 - i) as u32;
                let word: u32 = if format == PixelFormat::D24UnormS8Uint {
                    depth | (stencil << 24)
                } else { (depth << 8) | stencil };
                guest[16 + i * 4..20 + i * 4].copy_from_slice(&word.to_le_bytes());
            }
            let size = converted_depth_stencil_linear_size(&[copy]);
            let mut upload = runtime.upload_staging_buffer(size, false).unwrap();
            let planes = convert_depth24_stencil8_upload(format, &guest, upload.mapped_span_mut(), &[copy]).unwrap();
            image.upload_depth_stencil_memory(runtime.scheduler(), &upload.buffer, upload.offset, &planes.depth, &planes.stencil).unwrap();
            let mut download = runtime.download_staging_buffer(size + 64, true).unwrap();
            download.mapped_span_mut().fill(0xcd);
            image.download_depth_stencil_memory(runtime.scheduler(), &download.buffer, download.offset + 32, &planes.depth, &planes.stencil).unwrap();
            runtime.finish().unwrap();
            let actual = download.mapped_span();
            assert_eq!(&actual[32..32 + size], upload.mapped_span());
            assert!(actual[..32].iter().all(|&byte| byte == 0xcd));
            assert!(actual[32 + size..].iter().all(|&byte| byte == 0xcd));
            runtime.free_deferred_staging_buffer(&mut download).unwrap();
            let mut packed = vec![0xcc; guest.len() + 16];
            runtime.download_single_sample_memory(&image, &mut packed, &[copy]).unwrap();
            assert_eq!(&packed[16..guest.len()], &guest[16..]);
            assert!(packed[..16].iter().all(|&byte| byte == 0xcc));
            assert!(packed[guest.len()..].iter().all(|&byte| byte == 0xcc));

            // Snapshot and overwrite are recorded in order without an intervening
            // CPU wait. The snapshot must retain both original native aspects.
            let snapshot = runtime.sampling_snapshot(&image).unwrap().unwrap();
            let reused = runtime.sampling_snapshot(&image).unwrap().unwrap();
            assert!(Arc::ptr_eq(&snapshot, &reused));
            let revision = image.content_revision().unwrap();
            assert_ne!(snapshot.handle() as *const _, image.handle() as *const _);
            let mut replacement = runtime.upload_staging_buffer(size, false).unwrap();
            replacement.mapped_span_mut().fill(0);
            image.upload_depth_stencil_memory(runtime.scheduler(), &replacement.buffer,
                replacement.offset, &planes.depth, &planes.stencil).unwrap();
            assert_ne!(image.content_revision(), Some(revision));
            let refreshed = runtime.sampling_snapshot(&image).unwrap().unwrap();
            assert!(!Arc::ptr_eq(&snapshot, &refreshed));
            let mut overwritten = vec![0xcc; guest.len()];
            runtime.download_depth24_stencil8_memory(&image, &mut overwritten, &[copy]).unwrap();
            assert!(overwritten[16..].iter().all(|&byte| byte == 0));
            let mut refreshed_bytes = vec![0xcc; guest.len()];
            runtime.download_depth24_stencil8_memory(&refreshed, &mut refreshed_bytes, &[copy]).unwrap();
            assert!(refreshed_bytes[16..].iter().all(|&byte| byte == 0));
            let mut preserved = vec![0xcc; guest.len()];
            runtime.download_depth24_stencil8_memory(&snapshot, &mut preserved, &[copy]).unwrap();
            assert_eq!(&preserved[16..], &guest[16..]);
            assert!(preserved[..16].iter().all(|&byte| byte == 0xcc));
        }
    }

    #[test]
    fn sampling_snapshot_views_preserve_aspects_swizzles_and_subresources() {
        use crate::texture_cache::texture_cache_base::{ImageSlot, ImageViewSlot};
        use crate::texture_cache::types::{SubresourceRange, SubresourceBase};
        use crate::texture_cache::image_view_info::SwizzleSource;
        let device = MetalDevice::new().unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let mut staging = MetalStagingBufferPool::new(&device).unwrap();
        let mut blit = MetalBlitHelper::new(&device).unwrap();
        let mut cache = MetalTextureCache::new(device.clone(), Arc::new(MaxwellDeviceMemoryManager::default()),
            &mut scheduler, &mut staging, &mut blit);
        let info = ImageInfo {
            format: PixelFormat::D32FloatS8Uint, image_type: ImageType::E2D,
            size: Extent3D { width: 8, height: 8, depth: 1 },
            resources: SubresourceExtent { levels: 2, layers: 2 }, num_samples: 1,
            ..ImageInfo::default()
        };
        let mut image = ImageSlot::pending(ImageBase::new(info.clone(), 0x10000, 0x20000));
        image.backend = Some(MetalImage::new(&device, &info).unwrap());
        let image_id = cache.base.slot_images.insert(image);
        for component in [SwizzleSource::R, SwizzleSource::G] {
            let view_info = ImageViewInfo {
                view_type: ImageViewType::E2DArray, format: info.format,
                range: SubresourceRange {
                    base: SubresourceBase { level: 1, layer: 1 },
                    extent: SubresourceExtent { levels: 1, layers: 1 },
                },
                x_source: component as u8, y_source: component as u8,
                z_source: SwizzleSource::Zero as u8, w_source: SwizzleSource::OneFloat as u8,
            };
            let mut view = ImageViewSlot::pending(view_info.clone(),
                ImageViewBase::new(&view_info, &info, image_id, 0x10000));
            let original = MetalImageView::new(NonNull::from(view.base.as_mut()), &view_info,
                cache.base.slot_images[image_id].backend.as_ref().unwrap()).unwrap();
            let original_handle = original.retained_handle(TextureType::ColorArray2D).unwrap();
            view.backend = Some(MetalCachedImageView::Image(original));
            let original = original_handle;
            let view_id = cache.base.slot_image_views.insert(view);
            let snapshot = cache.retained_sampling_snapshot_view(view_id, TextureType::ColorArray2D)
                .unwrap().unwrap();
            let reused = cache.retained_sampling_snapshot_view(view_id, TextureType::ColorArray2D)
                .unwrap().unwrap();
            assert_eq!(&*snapshot as *const _, &*reused as *const _);
            use super::super::metal_graphics_pipeline::{MetalPreparedGraphics, MetalStageTextureBinding, MetalTextureBindingSource};
            cache.base.render_targets.depth_buffer_id = view_id;
            let mut prepared = MetalPreparedGraphics::default();
            prepared.fragment.textures.push(MetalStageTextureBinding {
                index: 0, texture: Some(original.clone()),
                source: MetalTextureBindingSource::Sampled { view_id, texture_type: TextureType::ColorArray2D },
            });
            assert!(!prepared.snapshot_read_only_depth_feedback(&mut cache, true).unwrap());
            assert_eq!(prepared.fragment.textures[0].texture.as_deref().map(|t| t as *const _), Some(&*original as *const _));
            prepared.vertex.textures.push(MetalStageTextureBinding {
                index: 1, texture: Some(original.clone()),
                source: MetalTextureBindingSource::Storage {
                    view_id, texture_type: TextureType::ColorArray2D, is_written: false,
                },
            });
            assert!(!prepared.snapshot_read_only_depth_feedback(&mut cache, false).unwrap());
            assert_eq!(prepared.fragment.textures[0].texture.as_deref().map(|t| t as *const _), Some(&*original as *const _));
            prepared.vertex.textures.clear();
            assert!(prepared.snapshot_read_only_depth_feedback(&mut cache, false).unwrap());
            assert_eq!(prepared.fragment.textures[0].texture.as_deref().map(|t| t as *const _), Some(&*snapshot as *const _));
            assert_eq!(snapshot.pixelFormat(), original.pixelFormat());
            assert_eq!(snapshot.textureType(), original.textureType());
            assert_eq!(snapshot.swizzle(), original.swizzle());
            assert_eq!(snapshot.parentRelativeLevel(), 1);
            assert_eq!(snapshot.parentRelativeSlice(), 1);
            assert_eq!(snapshot.mipmapLevelCount(), 1);
            assert_eq!(snapshot.arrayLength(), 1);
            assert_eq!(snapshot.width(), 4);
            assert_eq!(snapshot.height(), 4);
            assert_ne!(snapshot.parentTexture().as_deref().map(|p| p as *const _),
                original.parentTexture().as_deref().map(|p| p as *const _));
            drop(cache.base.slot_image_views.take(view_id));
            assert!(cache.retained_sampling_snapshot_view(view_id, TextureType::ColorArray2D)
                .unwrap().is_none());
            // Escaped handles remain native-only after the base slot is gone.
            assert_eq!(snapshot.width(), 4);
        }
        scheduler.finish_all().unwrap();
    }

    #[test]
    fn sampling_snapshot_heap_full_falls_back_without_submission() {
        let device = MetalDevice::new().unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let descriptor = MTLHeapDescriptor::new();
        descriptor.setSize(4096);
        descriptor.setStorageMode(MTLStorageMode::Private);
        descriptor.setHazardTrackingMode(MTLHazardTrackingMode::Tracked);
        let heap = device.device().newHeapWithDescriptor(&descriptor).unwrap();
        let mut cache = SamplingSnapshotCache {
            heap: Some(heap), attempted_allocation: true, ..SamplingSnapshotCache::default()
        };
        let source = image_with_format_and_size(&device, PixelFormat::D32Float, 1024, 1024);
        let tick = scheduler.current_tick();
        assert!(cache.get(&device, &mut scheduler, &source).unwrap().is_none());
        assert!(cache.entries.is_empty());
        assert_eq!(scheduler.current_tick(), tick);
        assert!(scheduler.flush().unwrap().is_none());
    }

    #[test]
    fn sampling_snapshot_rejects_color_without_allocating_heap() {
        let device = MetalDevice::new().unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let mut cache = SamplingSnapshotCache::default();
        let source = image_with_format(&device, PixelFormat::A8B8G8R8Unorm);
        assert!(cache.get(&device, &mut scheduler, &source).unwrap().is_none());
        assert!(!cache.attempted_allocation);
        assert!(scheduler.flush().unwrap().is_none());
    }

    #[test]
    fn sampling_snapshot_eviction_cannot_recycle_in_flight_heap_storage() {
        let device = MetalDevice::new().unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let descriptor = MTLHeapDescriptor::new();
        descriptor.setSize(256 * 1024);
        descriptor.setStorageMode(MTLStorageMode::Private);
        descriptor.setHazardTrackingMode(MTLHazardTrackingMode::Tracked);
        let heap = device.device().newHeapWithDescriptor(&descriptor).unwrap();
        let mut cache = SamplingSnapshotCache {
            heap: Some(heap.clone()), attempted_allocation: true, ..SamplingSnapshotCache::default()
        };
        let source = image_with_format_and_size(&device, PixelFormat::D32Float, 128, 128);
        let mut allocated = 0;
        for _ in 0..32 {
            source.mark_contents_modified();
            let Some(snapshot) = cache.get(&device, &mut scheduler, &source).unwrap() else {
                break;
            };
            allocated += 1;
            drop(snapshot);
            cache.entries.clear();
            assert!(heap.currentAllocatedSize() <= heap.size());
        }
        assert!(allocated > 0);
        assert!(allocated < 32, "unsubmitted copies must retain their heap textures");
        scheduler.finish_all().unwrap();
    }

    #[test]
    fn depth24_quantization_preserves_every_guest_depth_value() {
        for value in 0..=0x00ff_ffffu32 {
            assert_eq!(depth_float_to_unorm24(value as f32 / 16_777_215.0), value);
        }
        assert_eq!(depth_float_to_unorm24(0.5), 0x800000);
        assert_eq!(depth_float_to_unorm24(-1.0), 0);
        assert_eq!(depth_float_to_unorm24(2.0), 0xffffff);
        assert_eq!(depth_float_to_unorm24(f32::NAN), 0);
    }

    #[test]
    fn depth24_download_preserves_row_and_layer_padding_and_checks_bounds() {
        let copy = BufferImageCopy {
            buffer_offset: 12,
            buffer_size: 96,
            buffer_row_length: 4,
            buffer_image_height: 3,
            image_subresource: SubresourceLayers { base_level: 0, base_layer: 0, num_layers: 2 },
            image_extent: Extent3D { width: 2, height: 2, depth: 1 },
            ..BufferImageCopy::default()
        };
        for format in [PixelFormat::D24UnormS8Uint, PixelFormat::S8UintD24Unorm] {
            let mut guest = vec![0; 108];
            for (i, word) in guest[12..].chunks_exact_mut(4).enumerate() {
                word.copy_from_slice(&(0x7fff0000u32 + i as u32).to_le_bytes());
            }
            let mut native = vec![0; converted_depth_stencil_linear_size(&[copy])];
            convert_depth24_stencil8_upload(format, &guest, &mut native, &[copy]).unwrap();
            let mut output = vec![0xcc; 120];
            convert_depth24_stencil8_download(format, &native, &mut output, &[copy]).unwrap();
            let mut expected = vec![0xcc; 120];
            for layer in 0..2 {
                for row in 0..2 {
                    let offset = 12 + (layer * 12 + row * 4) * 4;
                    expected[offset..offset + 8].copy_from_slice(&guest[offset..offset + 8]);
                }
            }
            assert_eq!(output, expected);
            let mut untouched = vec![0xcc; 120];
            assert!(convert_depth24_stencil8_download(format, &native[..native.len() - 1], &mut untouched, &[copy]).is_err());
            assert_eq!(untouched, vec![0xcc; 120]);
            assert!(convert_depth24_stencil8_download(format, &native, &mut untouched[..107], &[copy]).is_err());
            assert_eq!(untouched, vec![0xcc; 120]);
        }
    }

    #[test]
    fn policy_matches_native_metal_cache_contract() {
        assert!(MetalTextureCacheParams::ENABLE_VALIDATION);
        assert!(MetalTextureCacheParams::FRAMEBUFFER_BLITS);
        assert!(!MetalTextureCacheParams::HAS_EMULATED_COPIES);
        assert!(!MetalTextureCacheParams::HAS_DEVICE_MEMORY_INFO);
        assert!(!MetalTextureCacheParams::IMPLEMENTS_ASYNC_DOWNLOADS);
        assert!(MetalTextureCacheParams::HAS_MSAA_DOWNLOADS);
    }

    #[test]
    fn runtime_uses_rasterizer_owned_scheduler_and_staging_pool() {
        let device = MetalDevice::new().unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let mut staging_buffer_pool = MetalStagingBufferPool::new(&device).unwrap();
        let scheduler_address = std::ptr::from_ref(&scheduler);
        let staging_address = std::ptr::from_ref(&staging_buffer_pool);
        let mut blit_helper = MetalBlitHelper::new(&device).unwrap();
        let mut runtime = MetalTextureCacheRuntime::new(
            device,
            &mut scheduler,
            &mut staging_buffer_pool,
            &mut blit_helper,
        );
        assert_eq!(std::ptr::from_ref(runtime.scheduler()), scheduler_address);
        assert_eq!(
            std::ptr::from_ref(runtime.staging_buffer_pool()),
            staging_address
        );
    }

    #[test]
    fn common_fermi_blit_uses_native_cache_images_without_cpu_roundtrip() {
        use crate::engines::fermi_2d::{Config, MemoryLayout, Surface};
        use crate::texture_cache::image_base::ImageFlagBits;
        let device = MetalDevice::new().unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let mut staging = MetalStagingBufferPool::new(&device).unwrap();
        let mut blit = MetalBlitHelper::new(&device).unwrap();
        let mut cache = MetalTextureCache::new(
            device.clone(),
            Arc::new(MaxwellDeviceMemoryManager::default()),
            &mut scheduler,
            &mut staging,
            &mut blit,
        );
        let memory = Arc::new(parking_lot::Mutex::new(
            crate::memory_manager::MemoryManager::new(17),
        ));
        memory.lock().map(0x10000, 0x100000, 0x10000, 0, true);
        memory.lock().map(0x20000, 0x200000, 0x10000, 0, true);
        cache.base.set_channel_gpu_memory(memory);
        let surface = |address| Surface {
            format: crate::gpu::RenderTargetFormat::A8B8G8R8Unorm as u32,
            linear: MemoryLayout::Pitch as u32,
            block_dimensions: 0,
            depth: 1,
            layer: 0,
            pitch: 16,
            width: 4,
            height: 4,
            addr_upper: 0,
            addr_lower: address,
        };
        let source = surface(0x10000);
        let destination = surface(0x20000);
        let config = Config {
            operation: Operation::SrcCopy,
            filter: Filter::Point,
            must_accelerate: true,
            src_x0: 0,
            src_y0: 0,
            src_x1: 4,
            src_y1: 4,
            dst_x0: 0,
            dst_y0: 0,
            dst_x1: 4,
            dst_y1: 4,
        };
        let images = cache
            .base
            .get_blit_images(&destination, &source, &config)
            .unwrap();
        let input = MetalBuffer::new(&device, 64).unwrap();
        let output = MetalBuffer::new(&device, 64).unwrap();
        let zero = MetalBuffer::new(&device, 64).unwrap();
        zero.write(0, &[0; 64]).unwrap();
        let pixels: Vec<u8> = (0..64).map(|i| (i * 3) as u8).collect();
        input.write(0, &pixels).unwrap();
        let copy = BufferImageCopy {
            buffer_size: 64,
            image_extent: Extent3D {
                width: 4,
                height: 4,
                depth: 1,
            },
            ..BufferImageCopy::default()
        };
        for id in [images.src_id, images.dst_id] {
            let image = &mut cache.base.slot_images[id];
            // GPU-owned input has no CPU backing. Any accidental refresh would
            // replace the nonzero pixels and fail the comparison below.
            image.flags.remove(ImageFlagBits::CPU_MODIFIED);
            image.flags.insert(ImageFlagBits::GPU_MODIFIED);
            image
                .backend
                .as_ref()
                .unwrap()
                .upload_memory(
                    &mut scheduler,
                    if id == images.src_id { &input } else { &zero },
                    0,
                    &[copy],
                )
                .unwrap();
        }
        assert!(cache.blit_image(&destination, &source, &config));
        assert!(cache.base.slot_images[images.dst_id]
            .flags
            .contains(ImageFlagBits::GPU_MODIFIED));
        cache.base.slot_images[images.dst_id]
            .backend
            .as_ref()
            .unwrap()
            .download_memory(&mut scheduler, &output, 0, &[copy])
            .unwrap();
        scheduler.finish_all().unwrap();
        let mut actual = [0; 64];
        output.read(0, &mut actual).unwrap();
        assert_eq!(actual.as_slice(), pixels);
    }

    #[test]
    fn copies_native_images_in_guest_order() {
        let device = MetalDevice::new().unwrap();
        let source_image = image(&device);
        let destination_image = image(&device);
        let upload = MetalBuffer::new(&device, 64).unwrap();
        let download = MetalBuffer::new(&device, 64).unwrap();
        let pixels = (0..64).map(|value| 255 - value as u8).collect::<Vec<_>>();
        upload.write(0, &pixels).unwrap();
        let buffer_copy = BufferImageCopy {
            buffer_size: 64,
            image_extent: Extent3D {
                width: 4,
                height: 4,
                depth: 1,
            },
            ..BufferImageCopy::default()
        };
        let image_copy = ImageCopy {
            src_subresource: SubresourceLayers::default(),
            dst_subresource: SubresourceLayers::default(),
            extent: buffer_copy.image_extent,
            ..ImageCopy::default()
        };
        let mut scheduler = MetalScheduler::new(&device);
        let mut staging_buffer_pool = MetalStagingBufferPool::new(&device).unwrap();
        let mut blit_helper = MetalBlitHelper::new(&device).unwrap();
        let mut runtime = MetalTextureCacheRuntime::new(
            device,
            &mut scheduler,
            &mut staging_buffer_pool,
            &mut blit_helper,
        );
        source_image
            .upload_memory(runtime.scheduler(), &upload, 0, &[buffer_copy])
            .unwrap();
        runtime
            .copy_image(&destination_image, &source_image, &[image_copy])
            .unwrap();
        destination_image
            .download_memory(runtime.scheduler(), &download, 0, &[buffer_copy])
            .unwrap();
        runtime.scheduler().finish_all().unwrap();
        let mut result = vec![0; 64];
        download.read(0, &mut result).unwrap();
        assert_eq!(result, pixels);
    }

    #[test]
    fn reinterprets_size_compatible_formats_through_a_buffer() {
        let device = MetalDevice::new().unwrap();
        let source_image = image_with_format(&device, PixelFormat::A8B8G8R8Unorm);
        let destination_image = image_with_format(&device, PixelFormat::A8B8G8R8Uint);
        let upload = MetalBuffer::new(&device, 64).unwrap();
        let download = MetalBuffer::new(&device, 64).unwrap();
        let pixels = (0..64).map(|value| value as u8).collect::<Vec<_>>();
        upload.write(0, &pixels).unwrap();
        let buffer_copy = BufferImageCopy {
            buffer_size: 64,
            image_extent: Extent3D {
                width: 4,
                height: 4,
                depth: 1,
            },
            ..BufferImageCopy::default()
        };
        let image_copy = ImageCopy {
            src_subresource: SubresourceLayers::default(),
            dst_subresource: SubresourceLayers::default(),
            extent: buffer_copy.image_extent,
            ..ImageCopy::default()
        };
        let mut scheduler = MetalScheduler::new(&device);
        let mut staging_buffer_pool = MetalStagingBufferPool::new(&device).unwrap();
        let mut blit_helper = MetalBlitHelper::new(&device).unwrap();
        let mut runtime = MetalTextureCacheRuntime::new(
            device,
            &mut scheduler,
            &mut staging_buffer_pool,
            &mut blit_helper,
        );

        source_image
            .upload_memory(runtime.scheduler(), &upload, 0, &[buffer_copy])
            .unwrap();
        runtime
            .copy_image(&destination_image, &source_image, &[image_copy])
            .unwrap();
        destination_image
            .download_memory(runtime.scheduler(), &download, 0, &[buffer_copy])
            .unwrap();
        runtime.scheduler().finish_all().unwrap();

        let mut result = vec![0; 64];
        download.read(0, &mut result).unwrap();
        assert_eq!(result, pixels);
        let mut cpu_output = vec![0xcd; 64];
        runtime.download_single_sample_memory(&destination_image, &mut cpu_output, &[buffer_copy]).unwrap();
        assert_eq!(cpu_output, pixels);
    }

    #[test]
    fn reinterprets_d32s8_rg32_without_float_conversion() {
        test_depth_stencil_reinterpretation(false);
    }

    #[test]
    fn reinterprets_d32s8_rg32_partial_mip_and_array_layers() {
        test_depth_stencil_reinterpretation(true);
    }

    #[test]
    fn transfers_packed_d32s8_memory_with_pitched_rows() {
        let device = MetalDevice::new().unwrap();
        let depth = image_with_format(&device, PixelFormat::D32FloatS8Uint);
        let upload = MetalBuffer::new(&device, 512).unwrap();
        let download = MetalBuffer::new(&device, 512).unwrap();
        let input = (0..512).map(|index| (index * 37) as u8).collect::<Vec<_>>();
        upload.write(0, &input).unwrap();
        download.write(0, &[0x55; 512]).unwrap();
        let copy = BufferImageCopy {
            buffer_offset: 16,
            buffer_size: 192,
            buffer_row_length: 6,
            buffer_image_height: 4,
            image_extent: Extent3D {
                width: 4,
                height: 4,
                depth: 1,
            },
            ..BufferImageCopy::default()
        };
        let mut scheduler = MetalScheduler::new(&device);
        let mut staging_pool = MetalStagingBufferPool::new(&device).unwrap();
        let mut blit_helper = MetalBlitHelper::new(&device).unwrap();
        let mut runtime = MetalTextureCacheRuntime::new(
            device,
            &mut scheduler,
            &mut staging_pool,
            &mut blit_helper,
        );
        runtime
            .transfer_depth32_stencil8_memory(&depth, &upload, 256, &[copy], true)
            .unwrap();
        runtime
            .transfer_depth32_stencil8_memory(&depth, &download, 256, &[copy], false)
            .unwrap();
        runtime.finish().unwrap();
        let mut result = vec![0; 512];
        download.read(0, &mut result).unwrap();
        let mut expected = vec![0x55; 512];
        for y in 0..4 {
            for x in 0..4 {
                let offset = 272 + y * 48 + x * 8;
                expected[offset..offset + 5].copy_from_slice(&input[offset..offset + 5]);
                expected[offset + 5..offset + 8].fill(0);
            }
        }
        assert_eq!(result, expected);
        let mut cpu_output = vec![0x55; 256];
        runtime.download_single_sample_memory(&depth, &mut cpu_output, &[copy]).unwrap();
        assert_eq!(cpu_output, expected[256..]);
    }

    fn test_depth_stencil_reinterpretation(partial: bool) {
        let device = MetalDevice::new().unwrap();
        let make_image = |format| {
            MetalImage::new(
                &device,
                &ImageInfo {
                    format,
                    image_type: ImageType::E2D,
                    resources: SubresourceExtent {
                        levels: 3,
                        layers: 3,
                    },
                    size: Extent3D {
                        width: 8,
                        height: 8,
                        depth: 1,
                    },
                    num_samples: 1,
                    ..ImageInfo::default()
                },
            )
            .unwrap()
        };
        let source = make_image(PixelFormat::R32G32Float);
        let depth = make_image(PixelFormat::D32FloatS8Uint);
        let destination = make_image(PixelFormat::R32G32Float);
        let upload = MetalBuffer::new(&device, 256).unwrap();
        let initial = MetalBuffer::new(&device, 256).unwrap();
        let download = MetalBuffer::new(&device, 256).unwrap();
        // Include signed zero, denormals, infinities and NaN payloads. None
        // may be sampled or converted as float during a bit reinterpretation.
        let depths = [
            0,
            0x8000_0000,
            1,
            0x007f_ffff,
            0x3f00_0000,
            0x3f80_0000,
            0x7f80_0000,
            0xff80_0000,
            0x7fc0_1234,
            0xffc0_5678,
            0x7f80_0001,
        ];
        let mut input = Vec::new();
        for index in 0..32 {
            input.extend_from_slice(&u32::to_le_bytes(depths[index % depths.len()]));
            input.extend_from_slice(&(0xabcd_0000u32 | ((index as u32 * 37) & 255)).to_le_bytes());
        }
        upload.write(0, &input).unwrap();
        initial.write(0, &[0x55; 256]).unwrap();
        let buffer_copy = BufferImageCopy {
            buffer_size: 256,
            image_subresource: SubresourceLayers {
                base_level: 1,
                base_layer: 1,
                num_layers: 2,
            },
            image_extent: Extent3D {
                width: 4,
                height: 4,
                depth: 1,
            },
            ..BufferImageCopy::default()
        };
        let extent = if partial { 2 } else { 4 };
        let origin = if partial { 1 } else { 0 };
        let to_depth = ImageCopy {
            src_subresource: buffer_copy.image_subresource,
            dst_subresource: buffer_copy.image_subresource,
            src_offset: crate::texture_cache::types::Offset3D {
                x: origin,
                y: origin,
                z: 0,
            },
            dst_offset: crate::texture_cache::types::Offset3D {
                x: 0,
                y: origin,
                z: 0,
            },
            extent: Extent3D {
                width: extent,
                height: extent,
                depth: 1,
            },
        };
        let from_depth = ImageCopy {
            src_offset: to_depth.dst_offset,
            dst_offset: to_depth.src_offset,
            ..to_depth
        };
        let mut scheduler = MetalScheduler::new(&device);
        let mut staging_pool = MetalStagingBufferPool::new(&device).unwrap();
        let mut blit_helper = MetalBlitHelper::new(&device).unwrap();
        let mut runtime = MetalTextureCacheRuntime::new(
            device,
            &mut scheduler,
            &mut staging_pool,
            &mut blit_helper,
        );
        source
            .upload_memory(runtime.scheduler(), &upload, 0, &[buffer_copy])
            .unwrap();
        destination
            .upload_memory(runtime.scheduler(), &initial, 0, &[buffer_copy])
            .unwrap();
        runtime.copy_image(&depth, &source, &[to_depth]).unwrap();
        runtime
            .copy_image(&destination, &depth, &[from_depth])
            .unwrap();
        destination
            .download_memory(runtime.scheduler(), &download, 0, &[buffer_copy])
            .unwrap();
        // There is no finish/readback between the two conversion directions.
        runtime.finish().unwrap();
        let mut result = vec![0; 256];
        download.read(0, &mut result).unwrap();
        let mut expected = vec![0x55; 256];
        for layer in 0..2 {
            for y in origin as usize..origin as usize + extent as usize {
                for x in origin as usize..origin as usize + extent as usize {
                    let offset = (layer * 16 + y * 4 + x) * 8;
                    expected[offset..offset + 5].copy_from_slice(&input[offset..offset + 5]);
                    expected[offset + 5..offset + 8].fill(0);
                }
            }
        }
        assert_eq!(result, expected);
    }

    #[test]
    fn reinterprets_uncompressed_and_bc3_block_extents() {
        let device = MetalDevice::new().unwrap();
        let uncompressed = image_with_format_and_size(&device, PixelFormat::R32G32B32A32Uint, 4, 4);
        let compressed = image_with_format_and_size(&device, PixelFormat::Bc3Unorm, 16, 16);
        let round_trip = image_with_format_and_size(&device, PixelFormat::R32G32B32A32Uint, 4, 4);
        let upload = MetalBuffer::new(&device, 256).unwrap();
        let download = MetalBuffer::new(&device, 256).unwrap();
        let bytes = (0..256).map(|value| value as u8).collect::<Vec<_>>();
        upload.write(0, &bytes).unwrap();
        let buffer_copy = BufferImageCopy {
            buffer_size: bytes.len(),
            image_extent: Extent3D {
                width: 4,
                height: 4,
                depth: 1,
            },
            ..BufferImageCopy::default()
        };
        let to_compressed = ImageCopy {
            src_subresource: SubresourceLayers::default(),
            dst_subresource: SubresourceLayers::default(),
            extent: buffer_copy.image_extent,
            ..ImageCopy::default()
        };
        let to_uncompressed = ImageCopy {
            src_subresource: SubresourceLayers::default(),
            dst_subresource: SubresourceLayers::default(),
            extent: Extent3D {
                width: 16,
                height: 16,
                depth: 1,
            },
            ..ImageCopy::default()
        };
        let mut scheduler = MetalScheduler::new(&device);
        let mut staging_buffer_pool = MetalStagingBufferPool::new(&device).unwrap();
        let mut blit_helper = MetalBlitHelper::new(&device).unwrap();
        let mut runtime = MetalTextureCacheRuntime::new(
            device,
            &mut scheduler,
            &mut staging_buffer_pool,
            &mut blit_helper,
        );

        uncompressed
            .upload_memory(runtime.scheduler(), &upload, 0, &[buffer_copy])
            .unwrap();
        runtime
            .copy_image(&compressed, &uncompressed, &[to_compressed])
            .unwrap();
        runtime
            .copy_image(&round_trip, &compressed, &[to_uncompressed])
            .unwrap();
        round_trip
            .download_memory(runtime.scheduler(), &download, 0, &[buffer_copy])
            .unwrap();
        runtime.scheduler().finish_all().unwrap();

        let mut result = vec![0; bytes.len()];
        download.read(0, &mut result).unwrap();
        assert_eq!(result, bytes);
    }

    #[test]
    fn clamps_reinterpreted_copy_to_compressed_mip_edge() {
        let device = MetalDevice::new().unwrap();
        let source = image_with_format_and_size(&device, PixelFormat::R32G32B32A32Uint, 4, 1);
        let destination =
            image_with_format_size_and_levels(&device, PixelFormat::Bc3Unorm, 128, 128, 8);
        let copy = ImageCopy {
            src_subresource: SubresourceLayers::default(),
            dst_subresource: SubresourceLayers {
                base_level: 6,
                ..SubresourceLayers::default()
            },
            extent: Extent3D {
                width: 1,
                height: 1,
                depth: 1,
            },
            ..ImageCopy::default()
        };

        let native = make_native_image_copies(&source, &destination, &copy).unwrap();

        assert_eq!(native.len(), 1);
        assert_eq!(
            native[0].source_size,
            MTLSize {
                width: 1,
                height: 1,
                depth: 1,
            }
        );
        assert_eq!(
            native[0].destination_size,
            MTLSize {
                width: 2,
                height: 2,
                depth: 1,
            }
        );
    }

    #[test]
    fn resolves_multisample_color_in_guest_order() {
        let device = MetalDevice::new().unwrap();
        let source_image = multisample_image(&device);
        let destination_image = image(&device);
        let download = MetalBuffer::new(&device, 64).unwrap();
        let buffer_copy = BufferImageCopy {
            buffer_size: 64,
            image_extent: Extent3D {
                width: 4,
                height: 4,
                depth: 1,
            },
            ..BufferImageCopy::default()
        };
        let image_copy = ImageCopy {
            src_subresource: SubresourceLayers::default(),
            dst_subresource: SubresourceLayers::default(),
            extent: buffer_copy.image_extent,
            ..ImageCopy::default()
        };
        let mut scheduler = MetalScheduler::new(&device);
        let mut staging_buffer_pool = MetalStagingBufferPool::new(&device).unwrap();
        let mut blit_helper = MetalBlitHelper::new(&device).unwrap();
        let mut runtime = MetalTextureCacheRuntime::new(
            device,
            &mut scheduler,
            &mut staging_buffer_pool,
            &mut blit_helper,
        );

        let clear_pass = MTLRenderPassDescriptor::renderPassDescriptor();
        let attachment = unsafe { clear_pass.colorAttachments().objectAtIndexedSubscript(0) };
        attachment.setTexture(Some(source_image.handle()));
        attachment.setLoadAction(MTLLoadAction::Clear);
        attachment.setStoreAction(MTLStoreAction::Store);
        attachment.setClearColor(objc2_metal::MTLClearColor {
            red: 1.0,
            green: 0.0,
            blue: 0.0,
            alpha: 1.0,
        });
        clear_pass.setRenderTargetWidth(4);
        clear_pass.setRenderTargetHeight(4);
        clear_pass.setDefaultRasterSampleCount(source_image.samples() as usize);
        runtime.scheduler().begin_render_pass(&clear_pass).unwrap();
        runtime.scheduler().end_render_pass();
        runtime
            .resolve_image_msaa(&destination_image, &source_image, &[image_copy])
            .unwrap();
        destination_image
            .download_memory(runtime.scheduler(), &download, 0, &[buffer_copy])
            .unwrap();
        runtime.scheduler().finish_all().unwrap();

        let mut result = vec![0; 64];
        download.read(0, &mut result).unwrap();
        assert!(result
            .chunks_exact(4)
            .all(|pixel| pixel == [255, 0, 0, 255]));
    }

    #[test]
    fn can_download_msaa_follows_copy_msaa_color_rules() {
        let color = |samples, format| ImageInfo {
            format,
            image_type: ImageType::E2D,
            size: Extent3D {
                width: 8,
                height: 8,
                depth: 1,
            },
            resources: SubresourceExtent { levels: 1, layers: 1 },
            num_samples: samples,
            ..ImageInfo::default()
        };
        assert!(can_download_msaa_info(&color(4, PixelFormat::A8B8G8R8Unorm), 4));
        assert!(!can_download_msaa_info(&color(1, PixelFormat::A8B8G8R8Unorm), 1));
        assert!(!can_download_msaa_info(&color(4, PixelFormat::A8B8G8R8Unorm), 2));
        assert!(!can_download_msaa_info(&color(4, PixelFormat::A8B8G8R8Uint), 4));
        assert!(!can_download_msaa_info(&color(4, PixelFormat::D32Float), 4));
        assert!(!can_download_msaa_info(&color(4, PixelFormat::D32FloatS8Uint), 4));
        assert!(!can_download_msaa_info(
            &ImageInfo {
                format: PixelFormat::A8B8G8R8Unorm,
                image_type: ImageType::E3D,
                size: Extent3D {
                    width: 8,
                    height: 8,
                    depth: 4,
                },
                resources: SubresourceExtent { levels: 1, layers: 1 },
                num_samples: 4,
                ..ImageInfo::default()
            },
            4
        ));
        assert!(!can_download_msaa_info(&color(4, PixelFormat::Bc3Unorm), 4));
    }

    #[test]
    fn downloads_msaa_color_through_scratch_copy_without_averaging_samples() {
        use objc2_foundation::NSString;
        use objc2_metal::{
            MTLCompileOptions, MTLCullMode, MTLLanguageVersion, MTLLibrary,
            MTLPrimitiveType, MTLRenderCommandEncoder, MTLRenderPipelineDescriptor, MTLViewport,
        };

        let device = MetalDevice::new().unwrap();
        let source_image = multisample_image(&device);
        let mut scheduler = MetalScheduler::new(&device);
        let mut staging_buffer_pool = MetalStagingBufferPool::new(&device).unwrap();
        let mut blit_helper = MetalBlitHelper::new(&device).unwrap();
        let mut runtime = MetalTextureCacheRuntime::new(
            device.clone(),
            &mut scheduler,
            &mut staging_buffer_pool,
            &mut blit_helper,
        );
        let info = guest_image_info(&source_image);
        assert!(runtime.can_download_msaa(&info));

        let shader = NSString::from_str(
            r#"
#include <metal_stdlib>
using namespace metal;
struct VOut { float4 position [[position]]; };
vertex VOut v(uint vid [[vertex_id]]) {
    float2 p = float2(vid & 1u, vid >> 1u);
    return { float4(p * 2.0f - 1.0f, 0.0f, 1.0f) };
}
fragment float4 f(VOut input [[stage_in]], uint sample [[sample_id]]) {
    return float4(float(sample), floor(input.position.x), floor(input.position.y), 3.0f) / 3.0f;
}
"#,
        );
        let options = MTLCompileOptions::new();
        options.setLanguageVersion(MTLLanguageVersion::Version2_3);
        let library = device
            .device()
            .newLibraryWithSource_options_error(&shader, Some(&options))
            .unwrap();
        let vertex = library
            .newFunctionWithName(&NSString::from_str("v"))
            .unwrap();
        let fragment = library
            .newFunctionWithName(&NSString::from_str("f"))
            .unwrap();
        let descriptor = MTLRenderPipelineDescriptor::new();
        descriptor.setVertexFunction(Some(&vertex));
        descriptor.setFragmentFunction(Some(&fragment));
        descriptor.setRasterSampleCount(source_image.samples() as usize);
        unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) }
            .setPixelFormat(source_image.handle().pixelFormat());
        let pipeline = device
            .device()
            .newRenderPipelineStateWithDescriptor_error(&descriptor)
            .unwrap();
        let pass = MTLRenderPassDescriptor::renderPassDescriptor();
        let attachment = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        attachment.setTexture(Some(source_image.handle()));
        attachment.setLoadAction(MTLLoadAction::DontCare);
        attachment.setStoreAction(MTLStoreAction::Store);
        pass.setRenderTargetWidth(4);
        pass.setRenderTargetHeight(4);
        pass.setDefaultRasterSampleCount(source_image.samples() as usize);
        runtime.scheduler().begin_render_pass(&pass).unwrap();
        runtime
            .scheduler()
            .with_render_encoder(|encoder| unsafe {
                encoder.setRenderPipelineState(&pipeline);
                encoder.setCullMode(MTLCullMode::None);
                encoder.setViewport(MTLViewport {
                    originX: 0.0,
                    originY: 0.0,
                    width: 4.0,
                    height: 4.0,
                    znear: 0.0,
                    zfar: 1.0,
                });
                encoder.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::TriangleStrip, 0, 4);
            })
            .unwrap();
        runtime.scheduler().end_render_pass();

        let copy = BufferImageCopy {
            buffer_size: 256,
            image_extent: Extent3D {
                width: 8,
                height: 8,
                depth: 1,
            },
            ..BufferImageCopy::default()
        };
        let mut output = vec![0xcd; 256];
        runtime
            .download_single_sample_memory(&source_image, &mut output, &[copy])
            .unwrap();
        let mut expected = vec![0u8; 256];
        for y in 0..8usize {
            for x in 0..8usize {
                let sample = x % 2 + 2 * (y % 2);
                let offset = (y * 8 + x) * 4;
                expected[offset..offset + 4].copy_from_slice(&[
                    (sample * 85) as u8,
                    ((x / 2) * 85) as u8,
                    ((y / 2) * 85) as u8,
                    255,
                ]);
            }
        }
        assert_eq!(output, expected, "MSAA downloads must expand samples, not resolve");
        assert_eq!(runtime.msaa_scratch_images.len(), 1);

        let mut again = vec![0xcd; 256];
        runtime
            .download_single_sample_memory(&source_image, &mut again, &[copy])
            .unwrap();
        assert_eq!(again, expected);
        assert_eq!(
            runtime.msaa_scratch_images.len(),
            1,
            "matching scratch images must be reused"
        );

        let depth = MetalImage::new(
            &device,
            &ImageInfo {
                format: PixelFormat::D32Float,
                image_type: ImageType::E2D,
                resources: SubresourceExtent { levels: 1, layers: 1 },
                size: Extent3D {
                    width: 8,
                    height: 8,
                    depth: 1,
                },
                num_samples: 4,
                ..ImageInfo::default()
            },
        )
        .unwrap();
        assert!(!runtime.can_download_msaa(&guest_image_info(&depth)));
        assert!(runtime
            .download_single_sample_memory(&depth, &mut [0; 4], &[copy])
            .is_err());
    }

    fn depth_stencil_copy() -> BufferImageCopy {
        BufferImageCopy {
            buffer_size: 8,
            buffer_row_length: 2,
            buffer_image_height: 1,
            image_extent: Extent3D {
                width: 2,
                height: 1,
                depth: 1,
            },
            ..BufferImageCopy::default()
        }
    }

    fn assert_converted_depth_stencil(format: PixelFormat, packed: [u32; 2]) {
        let input = packed
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>();
        let mut output = vec![0; converted_depth_stencil_linear_size(&[depth_stencil_copy()])];
        let copies =
            convert_depth24_stencil8_upload(format, &input, &mut output, &[depth_stencil_copy()])
                .unwrap();
        assert_eq!(copies.depth.len(), 1);
        assert_eq!(copies.stencil.len(), 1);
        assert_eq!(copies.depth[0].buffer_offset, 0);
        assert_eq!(copies.depth[0].buffer_size, 8);
        assert_eq!(copies.stencil[0].buffer_offset, 8);
        assert_eq!(copies.stencil[0].buffer_size, 2);
        let first_depth = f32::from_le_bytes(output[0..4].try_into().unwrap());
        let second_depth = f32::from_le_bytes(output[4..8].try_into().unwrap());
        assert_eq!(first_depth, 0.0);
        assert_eq!(second_depth, 1.0);
        assert_eq!(output[8], 0x12);
        assert_eq!(output[9], 0xab);
    }

    #[test]
    fn converts_d24s8_guest_words_to_metal_depth32_stencil8() {
        assert_converted_depth_stencil(PixelFormat::D24UnormS8Uint, [0x1200_0000, 0xabff_ffff]);
    }

    #[test]
    fn converts_s8d24_guest_words_to_metal_depth32_stencil8() {
        assert_converted_depth_stencil(PixelFormat::S8UintD24Unorm, [0x0000_0012, 0xffff_ffab]);
    }

    #[test]
    fn converts_b5g6r5_guest_words_to_metal_b5g6r5_storage() {
        let input = [0x001fu16, 0x07e0, 0xf800]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        let mut output = vec![0; input.len()];
        let copy = BufferImageCopy {
            buffer_size: input.len(),
            buffer_row_length: 3,
            buffer_image_height: 1,
            image_extent: Extent3D {
                width: 3,
                height: 1,
                depth: 1,
            },
            ..BufferImageCopy::default()
        };

        let copies = convert_packed16_upload(PixelFormat::B5G6R5Unorm, &input, &mut output, &[copy]).unwrap();

        assert_eq!(copies[0].buffer_size, input.len());
        assert_eq!(u16::from_le_bytes(output[0..2].try_into().unwrap()), 0xf800);
        assert_eq!(u16::from_le_bytes(output[2..4].try_into().unwrap()), 0x07e0);
        assert_eq!(u16::from_le_bytes(output[4..6].try_into().unwrap()), 0x001f);
    }
}
