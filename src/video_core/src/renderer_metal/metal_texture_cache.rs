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
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use crate::texture_cache::image_base::ImageBase;
use crate::texture_cache::image_view_base::ImageViewBase;
use crate::texture_cache::render_targets::RenderTargets;
use crate::texture_cache::texture_cache_base::{
    TextureCacheBase as CommonTextureCache, TextureCacheParams,
};
use crate::texture_cache::types::{
    BufferImageCopy, ImageCopy, ImageId, ImageType, ImageViewId, NULL_IMAGE_VIEW_ID,
    NULL_SAMPLER_ID, NUM_RT,
};

use super::metal_buffer::MetalBuffer;
use super::metal_device::MetalDevice;
use super::metal_framebuffer::{MetalFramebuffer, MetalFramebufferError};
use super::metal_image::MetalImage;
use super::metal_image_view::MetalImageView;
use super::metal_sampler::MetalSampler;
use super::metal_scheduler::{MetalScheduler, MetalSchedulerError};
use super::metal_staging_buffer_pool::{
    MetalStagingBufferError, MetalStagingBufferPool, StagingBufferRef,
};

#[derive(Debug, Error)]
pub enum MetalTextureCacheError {
    #[error(transparent)]
    Scheduler(#[from] MetalSchedulerError),
    #[error(transparent)]
    Staging(#[from] MetalStagingBufferError),
    #[error("native Metal image copy requires byte-compatible equal formats")]
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
}

impl MetalTextureCacheRuntime {
    pub fn new(
        device: MetalDevice,
        scheduler: &mut MetalScheduler,
        staging_buffer_pool: &mut MetalStagingBufferPool,
    ) -> Self {
        Self {
            device,
            scheduler: NonNull::from(scheduler),
            staging_buffer_pool: NonNull::from(staging_buffer_pool),
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

    /// Port of Eden `TextureCacheRuntime::CopyImage` for native Metal copies.
    pub fn copy_image(
        &mut self,
        destination: &MetalImage,
        source: &MetalImage,
        copies: &[ImageCopy],
    ) -> Result<(), MetalTextureCacheError> {
        if source.format().requires_conversion
            || destination.format().requires_conversion
            || source.format().pixel_format != destination.format().pixel_format
        {
            return Err(MetalTextureCacheError::IncompatibleFormats);
        }
        if source.samples() != 1 || destination.samples() != 1 {
            return Err(MetalTextureCacheError::MultisampleCopyRequiresShader);
        }
        let native_copies = copies
            .iter()
            .map(|copy| make_native_image_copies(source, destination, copy))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
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
                        copy.size,
                        destination.handle(),
                        copy.destination_slice,
                        copy.destination_level,
                        copy.destination_origin,
                    );
                }
            }
        })?;
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
                || copy.size != source_size
                || copy.size != destination_size
                || copy.source_level != 0
            {
                return Err(MetalTextureCacheError::MultisampleCopyRequiresShader);
            }

            let descriptor = MTLRenderPassDescriptor::renderPassDescriptor();
            let attachment = unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) };
            attachment.setTexture(Some(source.handle()));
            attachment.setResolveTexture(Some(destination.handle()));
            unsafe {
                attachment.setSlice(copy.source_slice);
                attachment.setResolveSlice(copy.destination_slice);
                attachment.setResolveLevel(copy.destination_level);
                descriptor.setRenderTargetWidth(copy.size.width);
                descriptor.setRenderTargetHeight(copy.size.height);
                descriptor.setRenderTargetArrayLength(1);
                descriptor.setDefaultRasterSampleCount(source.samples() as usize);
            }
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
    const FRAMEBUFFER_BLITS: bool = false;
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

    fn set_image_allocation_tick(image: &mut Self::Image, allocation_tick: u64) {
        image.set_allocation_tick(allocation_tick);
    }

    fn create_image_view(
        _runtime: Option<&mut Self::Runtime>,
        view_id: ImageViewId,
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
        cache.prepare_image(view.image_id, is_modification, invalidate);
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
        let result = image.upload_memory(
            cache.runtime_mut().scheduler(),
            &staging.buffer,
            staging.offset,
            copies,
        );
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
        cache.slot_images[src_id].backend = Some(source);
        cache.slot_images[dst_id].backend = Some(destination);
        if let Err(error) = result {
            log::error!(
                "Metal image copy failed: dst={} src={}: {error}",
                dst_id.index,
                src_id.index
            );
        }
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
}

struct NativeImageCopy {
    source_slice: usize,
    source_level: usize,
    source_origin: MTLOrigin,
    size: MTLSize,
    destination_slice: usize,
    destination_level: usize,
    destination_origin: MTLOrigin,
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
    let extent = MTLSize {
        width: copy.extent.width as usize,
        height: copy.extent.height as usize,
        depth: copy.extent.depth as usize,
    };
    if extent.width == 0 || extent.height == 0 || extent.depth == 0 {
        return Err(MetalTextureCacheError::InvalidCopy("zero extent"));
    }
    validate_mip_bounds(source, source_level, source_origin, extent)?;
    validate_mip_bounds(destination, destination_level, destination_origin, extent)?;

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
            size: extent,
            destination_slice: 0,
            destination_level,
            destination_origin,
        }]);
    }
    if source_origin.z != 0 || destination_origin.z != 0 || extent.depth != 1 {
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
            size: extent,
            destination_slice: destination_layer + layer,
            destination_level,
            destination_origin,
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

    fn image(device: &MetalDevice) -> MetalImage {
        MetalImage::new(
            device,
            &ImageInfo {
                format: PixelFormat::A8B8G8R8Unorm,
                image_type: ImageType::E2D,
                resources: SubresourceExtent {
                    levels: 1,
                    layers: 1,
                },
                size: Extent3D {
                    width: 4,
                    height: 4,
                    depth: 1,
                },
                num_samples: 1,
                ..ImageInfo::default()
            },
        )
        .unwrap()
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
        assert!(!MetalTextureCacheParams::FRAMEBUFFER_BLITS);
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
        let mut runtime =
            MetalTextureCacheRuntime::new(device, &mut scheduler, &mut staging_buffer_pool);
        assert_eq!(std::ptr::from_ref(runtime.scheduler()), scheduler_address);
        assert_eq!(
            std::ptr::from_ref(runtime.staging_buffer_pool()),
            staging_address
        );
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
        let mut runtime =
            MetalTextureCacheRuntime::new(device, &mut scheduler, &mut staging_buffer_pool);
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
        let mut runtime =
            MetalTextureCacheRuntime::new(device, &mut scheduler, &mut staging_buffer_pool);

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
        unsafe {
            clear_pass.setRenderTargetWidth(4);
            clear_pass.setRenderTargetHeight(4);
            clear_pass.setDefaultRasterSampleCount(source_image.samples() as usize);
        }
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
}
