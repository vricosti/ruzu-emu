// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Metal ownership counterpart of Eden's `vk_texture_cache.cpp` runtime.

use objc2_metal::{MTLBlitCommandEncoder, MTLOrigin, MTLSize};
use thiserror::Error;

use crate::texture_cache::types::{ImageCopy, ImageType};

use super::metal_device::MetalDevice;
use super::metal_image::MetalImage;
use super::metal_scheduler::{MetalScheduler, MetalSchedulerError};
use super::metal_staging_buffer_pool::{MetalStagingBufferError, MetalStagingBufferPool};

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
    #[error("invalid Metal image copy: {0}")]
    InvalidCopy(&'static str),
}

pub struct MetalTextureCacheRuntime {
    device: MetalDevice,
    scheduler: MetalScheduler,
    staging_buffer_pool: MetalStagingBufferPool,
}

impl MetalTextureCacheRuntime {
    pub fn new(device: MetalDevice) -> Result<Self, MetalTextureCacheError> {
        let scheduler = MetalScheduler::new(&device);
        let staging_buffer_pool = MetalStagingBufferPool::new(&device)?;
        Ok(Self {
            device,
            scheduler,
            staging_buffer_pool,
        })
    }

    pub fn device(&self) -> &MetalDevice {
        &self.device
    }

    pub fn scheduler(&mut self) -> &mut MetalScheduler {
        &mut self.scheduler
    }

    pub fn staging_buffer_pool(&mut self) -> &mut MetalStagingBufferPool {
        &mut self.staging_buffer_pool
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
        self.scheduler
            .request_outside_render_pass_operation_context();
        self.scheduler.with_blit_encoder(|encoder| {
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
        let mut runtime = MetalTextureCacheRuntime::new(device).unwrap();
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
}
