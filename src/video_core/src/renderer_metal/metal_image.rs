// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Metal image allocation from common texture-cache metadata.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLBlitCommandEncoder, MTLDevice, MTLOrigin, MTLSize, MTLStorageMode, MTLTexture,
    MTLTextureDescriptor, MTLTextureType,
};
use thiserror::Error;

use crate::surface::PixelFormat;
use crate::texture_cache::image_info::ImageInfo;
use crate::texture_cache::types::{BufferImageCopy, ImageType};

use super::metal_buffer::MetalBuffer;
use super::metal_device::MetalDevice;
use super::metal_format::{is_format_supported, surface_format, texture_usage, MetalFormat};
use super::metal_scheduler::{MetalScheduler, MetalSchedulerError};

#[derive(Debug, Error)]
pub enum MetalImageError {
    #[error("guest image format {0:?} has no Metal representation")]
    UnsupportedFormat(crate::surface::PixelFormat),
    #[error("buffer images must be created as views over a Metal buffer")]
    BufferImageRequiresBuffer,
    #[error("Metal failed to allocate {width}x{height}x{depth} texture {format:?}")]
    AllocationFailed {
        width: u32,
        height: u32,
        depth: u32,
        format: crate::surface::PixelFormat,
    },
    #[error("guest format {0:?} requires conversion before a native Metal transfer")]
    ConversionRequired(PixelFormat),
    #[error("invalid Metal image copy: {0}")]
    InvalidCopy(&'static str),
    #[error("Metal image copy exceeds the {length}-byte staging buffer")]
    BufferRangeOutOfBounds { length: usize },
    #[error(transparent)]
    Scheduler(#[from] MetalSchedulerError),
}

pub struct MetalImage {
    texture: Retained<ProtocolObject<dyn MTLTexture>>,
    guest_format: PixelFormat,
    image_type: ImageType,
    format: MetalFormat,
    size: (u32, u32, u32),
    levels: u32,
    layers: u32,
    samples: u32,
    guest_samples: u32,
}

impl MetalImage {
    pub fn new(device: &MetalDevice, info: &ImageInfo) -> Result<Self, MetalImageError> {
        if info.image_type == ImageType::Buffer {
            return Err(MetalImageError::BufferImageRequiresBuffer);
        }
        if !is_format_supported(device.profile(), info.format) {
            return Err(MetalImageError::UnsupportedFormat(info.format));
        }
        let format =
            surface_format(info.format).ok_or(MetalImageError::UnsupportedFormat(info.format))?;
        let samples = device
            .profile()
            .best_supported_sample_count(info.num_samples.max(1));

        let layers = info.resources.layers.max(1) as u32;
        let levels = if info.num_samples > 1 {
            1
        } else {
            info.resources.levels.max(1) as u32
        };
        let (samples_x, samples_y) =
            crate::texture_cache::samples_helper::samples_log2(info.num_samples as i32);
        let width = (info.size.width >> samples_x.max(0) as u32).max(1);
        let height = (info.size.height >> samples_y.max(0) as u32).max(1);
        let depth = info.size.depth.max(1);
        let texture_type = metal_texture_type(info.image_type, layers, samples);

        let descriptor = MTLTextureDescriptor::new();
        descriptor.setTextureType(texture_type);
        descriptor.setPixelFormat(format.pixel_format);
        unsafe {
            descriptor.setWidth(width as usize);
            descriptor.setHeight(height as usize);
            descriptor.setDepth(depth as usize);
            descriptor.setMipmapLevelCount(levels as usize);
            descriptor.setSampleCount(samples as usize);
            descriptor.setArrayLength(layers as usize);
        }
        descriptor.setStorageMode(MTLStorageMode::Private);
        descriptor.setUsage(texture_usage(device.profile(), info.format));

        let texture = device
            .device()
            .newTextureWithDescriptor(&descriptor)
            .ok_or(MetalImageError::AllocationFailed {
                width,
                height,
                depth,
                format: info.format,
            })?;
        Ok(Self {
            texture,
            guest_format: info.format,
            image_type: info.image_type,
            format,
            size: (width, height, depth),
            levels,
            layers,
            samples,
            guest_samples: info.num_samples,
        })
    }

    pub fn handle(&self) -> &ProtocolObject<dyn MTLTexture> {
        &self.texture
    }

    pub(crate) fn retained_handle(&self) -> Retained<ProtocolObject<dyn MTLTexture>> {
        self.texture.clone()
    }

    pub fn format(&self) -> MetalFormat {
        self.format
    }

    pub fn size(&self) -> (u32, u32, u32) {
        self.size
    }

    pub fn levels(&self) -> u32 {
        self.levels
    }

    pub fn layers(&self) -> u32 {
        self.layers
    }

    pub fn samples(&self) -> u32 {
        self.samples
    }

    pub fn guest_samples(&self) -> u32 {
        self.guest_samples
    }

    pub fn guest_format(&self) -> PixelFormat {
        self.guest_format
    }

    pub fn image_type(&self) -> ImageType {
        self.image_type
    }

    /// Port of Eden `Image::UploadMemory` for byte-compatible Metal formats.
    pub fn upload_memory(
        &self,
        scheduler: &mut MetalScheduler,
        source: &MetalBuffer,
        base_offset: usize,
        copies: &[BufferImageCopy],
    ) -> Result<(), MetalImageError> {
        let native_copies = self.native_buffer_copies(source, base_offset, copies)?;
        scheduler.request_outside_render_pass_operation_context();
        scheduler.with_blit_encoder(|encoder| {
            for copy in native_copies {
                unsafe {
                    encoder.copyFromBuffer_sourceOffset_sourceBytesPerRow_sourceBytesPerImage_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                        source.handle(),
                        copy.buffer_offset,
                        copy.bytes_per_row,
                        copy.bytes_per_image,
                        copy.size,
                        self.handle(),
                        copy.slice,
                        copy.level,
                        copy.origin,
                    );
                }
            }
        })?;
        Ok(())
    }

    /// Port of Eden `Image::DownloadMemory` for byte-compatible Metal formats.
    pub fn download_memory(
        &self,
        scheduler: &mut MetalScheduler,
        destination: &MetalBuffer,
        base_offset: usize,
        copies: &[BufferImageCopy],
    ) -> Result<(), MetalImageError> {
        let native_copies = self.native_buffer_copies(destination, base_offset, copies)?;
        scheduler.request_outside_render_pass_operation_context();
        scheduler.with_blit_encoder(|encoder| {
            for copy in native_copies {
                unsafe {
                    encoder.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage(
                        self.handle(),
                        copy.slice,
                        copy.level,
                        copy.origin,
                        copy.size,
                        destination.handle(),
                        copy.buffer_offset,
                        copy.bytes_per_row,
                        copy.bytes_per_image,
                    );
                }
            }
        })?;
        Ok(())
    }

    fn native_buffer_copies(
        &self,
        buffer: &MetalBuffer,
        base_offset: usize,
        copies: &[BufferImageCopy],
    ) -> Result<Vec<NativeBufferImageCopy>, MetalImageError> {
        if self.format.requires_conversion {
            return Err(MetalImageError::ConversionRequired(self.guest_format));
        }
        let block_width = crate::surface::default_block_width(self.guest_format).max(1) as usize;
        let block_height = crate::surface::default_block_height(self.guest_format).max(1) as usize;
        let bytes_per_block = crate::surface::bytes_per_block(self.guest_format).max(1) as usize;
        let mut result = Vec::new();
        for copy in copies {
            let level = usize::try_from(copy.image_subresource.base_level)
                .map_err(|_| MetalImageError::InvalidCopy("negative mip level"))?;
            let base_layer = usize::try_from(copy.image_subresource.base_layer)
                .map_err(|_| MetalImageError::InvalidCopy("negative array layer"))?;
            let num_layers = usize::try_from(copy.image_subresource.num_layers)
                .map_err(|_| MetalImageError::InvalidCopy("negative layer count"))?;
            if level >= self.levels as usize || num_layers == 0 {
                return Err(MetalImageError::InvalidCopy("subresource range"));
            }
            let origin = checked_origin(copy)?;
            let extent = checked_size(copy)?;
            let mip_width = (self.size.0 as usize >> level).max(1);
            let mip_height = (self.size.1 as usize >> level).max(1);
            let mip_depth = (self.size.2 as usize >> level).max(1);
            if origin.x.saturating_add(extent.width) > mip_width
                || origin.y.saturating_add(extent.height) > mip_height
                || (self.image_type == ImageType::E3D
                    && origin.z.saturating_add(extent.depth) > mip_depth)
                || (self.image_type != ImageType::E3D && origin.z != 0)
            {
                return Err(MetalImageError::InvalidCopy(
                    "image extent exceeds mip bounds",
                ));
            }
            let row_texels = if copy.buffer_row_length == 0 {
                extent.width
            } else {
                copy.buffer_row_length as usize
            };
            let image_texel_rows = if copy.buffer_image_height == 0 {
                extent.height
            } else {
                copy.buffer_image_height as usize
            };
            if row_texels < extent.width || image_texel_rows < extent.height {
                return Err(MetalImageError::InvalidCopy(
                    "buffer pitch is smaller than the image",
                ));
            }
            let bytes_per_row = row_texels.div_ceil(block_width) * bytes_per_block;
            let copied_block_rows = extent.height.div_ceil(block_height);
            let bytes_per_image = bytes_per_row * image_texel_rows.div_ceil(block_height);
            let copied_row_bytes = extent.width.div_ceil(block_width) * bytes_per_block;
            let depth_or_layers = if self.image_type == ImageType::E3D {
                extent.depth
            } else {
                num_layers
            };
            let first_byte = base_offset.checked_add(copy.buffer_offset).ok_or(
                MetalImageError::BufferRangeOutOfBounds {
                    length: buffer.length(),
                },
            )?;
            let last_byte = first_byte
                .checked_add(bytes_per_image.saturating_mul(depth_or_layers - 1))
                .and_then(|offset| {
                    offset.checked_add(bytes_per_row.saturating_mul(copied_block_rows - 1))
                })
                .and_then(|offset| offset.checked_add(copied_row_bytes))
                .ok_or(MetalImageError::BufferRangeOutOfBounds {
                    length: buffer.length(),
                })?;
            if last_byte > buffer.length() {
                return Err(MetalImageError::BufferRangeOutOfBounds {
                    length: buffer.length(),
                });
            }
            if copy.buffer_size != 0 && last_byte - first_byte > copy.buffer_size {
                return Err(MetalImageError::InvalidCopy(
                    "copy exceeds declared buffer size",
                ));
            }
            if self.image_type == ImageType::E3D {
                result.push(NativeBufferImageCopy {
                    buffer_offset: base_offset + copy.buffer_offset,
                    bytes_per_row,
                    bytes_per_image,
                    size: MTLSize {
                        width: extent.width,
                        height: extent.height,
                        depth: extent.depth,
                    },
                    slice: 0,
                    level,
                    origin,
                });
            } else {
                if base_layer + num_layers > self.layers as usize {
                    return Err(MetalImageError::InvalidCopy("array layer range"));
                }
                for layer in 0..num_layers {
                    result.push(NativeBufferImageCopy {
                        buffer_offset: base_offset + copy.buffer_offset + layer * bytes_per_image,
                        bytes_per_row,
                        bytes_per_image,
                        size: MTLSize {
                            width: extent.width,
                            height: extent.height,
                            depth: 1,
                        },
                        slice: base_layer + layer,
                        level,
                        origin: MTLOrigin {
                            x: origin.x,
                            y: origin.y,
                            z: 0,
                        },
                    });
                }
            }
        }
        Ok(result)
    }
}

#[derive(Clone, Copy)]
struct NativeBufferImageCopy {
    buffer_offset: usize,
    bytes_per_row: usize,
    bytes_per_image: usize,
    size: MTLSize,
    slice: usize,
    level: usize,
    origin: MTLOrigin,
}

fn checked_origin(copy: &BufferImageCopy) -> Result<MTLOrigin, MetalImageError> {
    Ok(MTLOrigin {
        x: usize::try_from(copy.image_offset.x)
            .map_err(|_| MetalImageError::InvalidCopy("negative x origin"))?,
        y: usize::try_from(copy.image_offset.y)
            .map_err(|_| MetalImageError::InvalidCopy("negative y origin"))?,
        z: usize::try_from(copy.image_offset.z)
            .map_err(|_| MetalImageError::InvalidCopy("negative z origin"))?,
    })
}

fn checked_size(copy: &BufferImageCopy) -> Result<MTLSize, MetalImageError> {
    let size = MTLSize {
        width: copy.image_extent.width as usize,
        height: copy.image_extent.height as usize,
        depth: copy.image_extent.depth as usize,
    };
    if size.width == 0 || size.height == 0 || size.depth == 0 {
        return Err(MetalImageError::InvalidCopy("zero image extent"));
    }
    Ok(size)
}

fn metal_texture_type(image_type: ImageType, layers: u32, samples: u32) -> MTLTextureType {
    match (image_type, layers > 1, samples > 1) {
        (ImageType::E1D, false, _) => MTLTextureType::Type1D,
        (ImageType::E1D, true, _) => MTLTextureType::Type1DArray,
        (ImageType::E2D | ImageType::Linear, false, false) => MTLTextureType::Type2D,
        (ImageType::E2D | ImageType::Linear, true, false) => MTLTextureType::Type2DArray,
        (ImageType::E2D | ImageType::Linear, false, true) => MTLTextureType::Type2DMultisample,
        (ImageType::E2D | ImageType::Linear, true, true) => MTLTextureType::Type2DMultisampleArray,
        (ImageType::E3D, _, _) => MTLTextureType::Type3D,
        (ImageType::Buffer, _, _) => MTLTextureType::TypeTextureBuffer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::PixelFormat;
    use crate::texture_cache::types::{Extent3D, SubresourceExtent};

    fn image_info(format: PixelFormat, layers: i32, samples: u32) -> ImageInfo {
        ImageInfo {
            format,
            image_type: ImageType::E2D,
            resources: SubresourceExtent { levels: 1, layers },
            size: Extent3D {
                width: 128,
                height: 64,
                depth: 1,
            },
            num_samples: samples,
            ..ImageInfo::default()
        }
    }

    #[test]
    fn allocates_native_color_and_depth_textures() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let color = MetalImage::new(&device, &image_info(PixelFormat::A8B8G8R8Unorm, 1, 1))
            .expect("color image allocation");
        assert_eq!(color.size(), (128, 64, 1));
        assert_eq!(color.handle().width(), 128);

        let depth = MetalImage::new(&device, &image_info(PixelFormat::D32Float, 1, 1))
            .expect("depth image allocation");
        assert_eq!(
            depth.format().pixel_format,
            objc2_metal::MTLPixelFormat::Depth32Float
        );
    }

    #[test]
    fn creates_array_storage_for_layered_guest_images() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let image = MetalImage::new(&device, &image_info(PixelFormat::A8B8G8R8Unorm, 6, 1))
            .expect("array image allocation");
        assert_eq!(image.layers(), 6);
        assert_eq!(image.handle().textureType(), MTLTextureType::Type2DArray);
    }

    #[test]
    fn unsupported_guest_msaa_is_clamped_to_a_native_sample_count() {
        let device = MetalDevice::new().unwrap();
        let image =
            MetalImage::new(&device, &image_info(PixelFormat::A8B8G8R8Unorm, 1, 16)).unwrap();
        assert_eq!(image.guest_samples(), 16);
        assert!(image.samples() <= 16);
        assert!(device
            .device()
            .supportsTextureSampleCount(image.samples() as usize));
    }

    #[test]
    fn uploads_and_downloads_a_native_texture_through_the_scheduler() {
        let device = MetalDevice::new().unwrap();
        let image = MetalImage::new(
            &device,
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
        .unwrap();
        let source = MetalBuffer::new(&device, 64).unwrap();
        let destination = MetalBuffer::new(&device, 64).unwrap();
        let pixels = (0..64).map(|value| value as u8).collect::<Vec<_>>();
        source.write(0, &pixels).unwrap();
        let copy = BufferImageCopy {
            buffer_size: 64,
            image_extent: Extent3D {
                width: 4,
                height: 4,
                depth: 1,
            },
            ..BufferImageCopy::default()
        };
        let mut scheduler = MetalScheduler::new(&device);
        image
            .upload_memory(&mut scheduler, &source, 0, &[copy])
            .unwrap();
        image
            .download_memory(&mut scheduler, &destination, 0, &[copy])
            .unwrap();
        scheduler.finish_all().unwrap();
        let mut downloaded = vec![0; 64];
        destination.read(0, &mut downloaded).unwrap();
        assert_eq!(downloaded, pixels);
    }
}
