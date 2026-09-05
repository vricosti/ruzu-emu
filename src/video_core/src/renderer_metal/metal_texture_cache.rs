// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Metal ownership counterpart of Eden's `vk_texture_cache.cpp` runtime.

use std::ptr::NonNull;
use std::sync::Arc;

use objc2_metal::{
    MTLBlitCommandEncoder, MTLLoadAction, MTLOrigin, MTLRenderPassDescriptor, MTLSize,
    MTLStoreAction,
};
use thiserror::Error;

use crate::buffer_cache::buffer_cache_base::BufferCacheAsyncBuffer;
use crate::engines::fermi_2d::{Filter, Operation};
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use crate::surface::{get_format_type, PixelFormat, SurfaceType};
use crate::texture_cache::image_base::ImageBase;
use crate::texture_cache::image_view_base::ImageViewBase;
use crate::texture_cache::image_view_info::ImageViewInfo;
use crate::texture_cache::render_targets::RenderTargets;
use crate::texture_cache::texture_cache_base::{
    DescriptorSyncRegs, ImageViewInOut, TextureCacheBase as CommonTextureCache, TextureCacheParams,
};
use crate::texture_cache::types::{
    BufferImageCopy, FramebufferId, ImageCopy, ImageId, ImageType, ImageViewId, ImageViewType,
    Region2D, SamplerId, NULL_IMAGE_ID, NULL_IMAGE_VIEW_ID, NULL_SAMPLER_ID, NUM_RT,
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
use super::metal_image_view::MetalImageView;
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
    #[error("Metal image copy requires byte-compatible formats")]
    IncompatibleFormats,
    #[error("native Metal image copy does not support multisample textures")]
    MultisampleCopyRequiresShader,
    #[error("native Metal multisample resolve requires a color MSAA source and single-sample destination")]
    InvalidMultisampleResolve,
    #[error("invalid Metal image copy: {0}")]
    InvalidCopy(&'static str),
}

pub struct MetalTextureCacheRuntime {
    device: MetalDevice,
    scheduler: NonNull<MetalScheduler>,
    staging_buffer_pool: NonNull<MetalStagingBufferPool>,
    blit_image_helper: NonNull<MetalBlitHelper>,
    depth_stencil_copy: Option<MetalDepthStencilCopy>,
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
        }
    }

    pub fn device(&self) -> &MetalDevice {
        &self.device
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

    pub fn tick_frame(&mut self) -> Result<(), MetalTextureCacheError> {
        let scheduler = unsafe { self.scheduler.as_mut() };
        let pool = unsafe { self.staging_buffer_pool.as_mut() };
        pool.tick_frame(scheduler)?;
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
            PixelFormat::B5G6R5Unorm => {
                let converted_size = converted_linear_size(copies, 2);
                let mut converted = cache
                    .runtime_mut()
                    .upload_staging_buffer(converted_size, false);
                match converted.as_mut() {
                    Ok(converted) => {
                        let converted_copies = convert_b5g6r5_upload(
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
                        log::error!("Metal B5G6R5 staging allocation failed: {error}");
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

fn convert_b5g6r5_upload(
    input: &[u8],
    output: &mut [u8],
    copies: &[BufferImageCopy],
) -> Result<Vec<BufferImageCopy>, super::metal_image::MetalImageError> {
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
                ((packed & 0x001f) << 11) | (packed & 0x07e0) | ((packed & 0xf800) >> 11);
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

struct ConvertedDepthStencilCopies {
    depth: Vec<BufferImageCopy>,
    stencil: Vec<BufferImageCopy>,
}

fn convert_depth24_stencil8_upload(
    format: PixelFormat,
    input: &[u8],
    output: &mut [u8],
    copies: &[BufferImageCopy],
) -> Result<ConvertedDepthStencilCopies, super::metal_image::MetalImageError> {
    let mut output_offset = 0usize;
    let mut depth_copies = Vec::with_capacity(copies.len());
    let mut stencil_copies = Vec::with_capacity(copies.len());
    for copy in copies {
        let texels = copy_linear_texel_count(copy);
        let input_size = texels.saturating_mul(4);
        let input_end = copy.buffer_offset.saturating_add(input_size);
        let depth_offset = align_up(output_offset, 8);
        let depth_end = depth_offset.saturating_add(texels.saturating_mul(4));
        let stencil_offset = align_up(depth_end, 8);
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
        let mut depth_copy = *copy;
        depth_copy.buffer_offset = depth_offset;
        depth_copy.buffer_size = texels.saturating_mul(4);
        depth_copies.push(depth_copy);
        let mut stencil_copy = *copy;
        stencil_copy.buffer_offset = stencil_offset;
        stencil_copy.buffer_size = texels;
        stencil_copies.push(stencil_copy);
        output_offset = output_end;
    }
    Ok(ConvertedDepthStencilCopies {
        depth: depth_copies,
        stencil: stencil_copies,
    })
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
        self.base.tick_frame();
        self.base
            .runtime_mut()
            .tick_frame()
            .unwrap_or_else(|error| panic!("Metal texture-cache frame tick failed: {error}"));
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
    fn policy_matches_native_metal_cache_contract() {
        assert!(MetalTextureCacheParams::ENABLE_VALIDATION);
        assert!(MetalTextureCacheParams::FRAMEBUFFER_BLITS);
        assert!(!MetalTextureCacheParams::HAS_EMULATED_COPIES);
        assert!(!MetalTextureCacheParams::HAS_DEVICE_MEMORY_INFO);
        assert!(!MetalTextureCacheParams::IMPLEMENTS_ASYNC_DOWNLOADS);
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

        let copies = convert_b5g6r5_upload(&input, &mut output, &[copy]).unwrap();

        assert_eq!(copies[0].buffer_size, input.len());
        assert_eq!(u16::from_le_bytes(output[0..2].try_into().unwrap()), 0xf800);
        assert_eq!(u16::from_le_bytes(output[2..4].try_into().unwrap()), 0x07e0);
        assert_eq!(u16::from_le_bytes(output[4..6].try_into().unwrap()), 0x001f);
    }
}
