// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Metal texture views for common texture-cache image views.

use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSRange;
use objc2_metal::{
    MTLPixelFormat, MTLTexture, MTLTextureSwizzle, MTLTextureSwizzleChannels, MTLTextureType,
};
use shader_recompiler::shader_info::{TextureType, NUM_TEXTURE_TYPES};
use thiserror::Error;

use crate::surface::{get_format_type, PixelFormat, SurfaceType};
use crate::texture_cache::image_view_base::ImageViewBase;
use crate::texture_cache::image_view_info::SwizzleSource;
use crate::texture_cache::types::ImageViewType;

use super::metal_format::surface_format;
use super::metal_image::MetalImage;

#[derive(Debug, Error)]
pub enum MetalImageViewError {
    #[error("guest image-view format {0:?} has no Metal representation")]
    UnsupportedFormat(crate::surface::PixelFormat),
    #[error("Metal failed to create a {texture_type:?} view for {format:?}")]
    CreationFailed {
        texture_type: TextureType,
        format: crate::surface::PixelFormat,
    },
    #[error("buffer image views must be materialized by the Metal buffer cache")]
    BufferViewRequiresBuffer,
}

/// Backend payload corresponding to Eden's `Vulkan::ImageView`.
pub struct MetalImageView {
    base: NonNull<ImageViewBase>,
    image: Retained<ProtocolObject<dyn MTLTexture>>,
    image_views: [Option<Retained<ProtocolObject<dyn MTLTexture>>>; NUM_TEXTURE_TYPES as usize],
    render_target: Retained<ProtocolObject<dyn MTLTexture>>,
    depth_view: Option<Retained<ProtocolObject<dyn MTLTexture>>>,
    stencil_view: Option<Retained<ProtocolObject<dyn MTLTexture>>>,
    samples: u32,
}

impl MetalImageView {
    pub fn new(
        base: NonNull<ImageViewBase>,
        image: &MetalImage,
    ) -> Result<Self, MetalImageViewError> {
        let view = unsafe { base.as_ref() };
        if view.is_buffer() {
            return Err(MetalImageViewError::BufferViewRequiresBuffer);
        }
        let render_format = surface_format(view.format)
            .ok_or(MetalImageViewError::UnsupportedFormat(view.format))?
            .pixel_format;
        let aspect = image_view_aspect(view);
        let format = aspect_format(view.format, aspect, render_format);
        let levels = ns_range(view.range.base.level, view.range.extent.levels);
        let slices = ns_range(view.range.base.layer, view.range.extent.layers);
        let mut sampled_swizzle = if view.is_render_target() {
            identity_swizzle()
        } else {
            swizzle_channels(view.swizzle)
        };
        if aspect != MetalImageAspect::Color && !view.is_render_target() {
            let mut sources = view.swizzle;
            sources.iter_mut().for_each(|source| {
                if *source == SwizzleSource::G as u8 {
                    *source = SwizzleSource::R as u8;
                }
            });
            sampled_swizzle = swizzle_channels(sources);
        }
        let mut image_views: [Option<Retained<ProtocolObject<dyn MTLTexture>>>;
            NUM_TEXTURE_TYPES as usize] = std::array::from_fn(|_| None);

        let create =
            |texture_type: TextureType,
             layer_count: Option<usize>|
             -> Result<Retained<ProtocolObject<dyn MTLTexture>>, MetalImageViewError> {
                let metal_type = metal_texture_type(texture_type, image.samples());
                let slices =
                    layer_count.map_or(slices, |count| NSRange::new(slices.location, count));
                unsafe {
                    image
                        .handle()
                        .newTextureViewWithPixelFormat_textureType_levels_slices_swizzle(
                            format,
                            metal_type,
                            levels,
                            slices,
                            sampled_swizzle,
                        )
                }
                .ok_or(MetalImageViewError::CreationFailed {
                    texture_type,
                    format: view.format,
                })
            };

        let sampled_render_target = match view.view_type {
            ImageViewType::E1D | ImageViewType::E1DArray => {
                image_views[TextureType::Color1D as usize] =
                    Some(create(TextureType::Color1D, Some(1))?);
                image_views[TextureType::ColorArray1D as usize] =
                    Some(create(TextureType::ColorArray1D, None)?);
                image_views[TextureType::ColorArray1D as usize]
                    .as_ref()
                    .unwrap()
                    .clone()
            }
            ImageViewType::E2D | ImageViewType::E2DArray | ImageViewType::Rect => {
                image_views[TextureType::Color2D as usize] =
                    Some(create(TextureType::Color2D, Some(1))?);
                image_views[TextureType::Color2DRect as usize] =
                    image_views[TextureType::Color2D as usize].clone();
                image_views[TextureType::ColorArray2D as usize] =
                    Some(create(TextureType::ColorArray2D, None)?);
                image_views[TextureType::ColorArray2D as usize]
                    .as_ref()
                    .unwrap()
                    .clone()
            }
            ImageViewType::E3D => {
                image_views[TextureType::Color3D as usize] =
                    Some(create(TextureType::Color3D, None)?);
                image_views[TextureType::Color3D as usize]
                    .as_ref()
                    .unwrap()
                    .clone()
            }
            ImageViewType::Cube | ImageViewType::CubeArray => {
                image_views[TextureType::ColorCube as usize] =
                    Some(create(TextureType::ColorCube, Some(6))?);
                image_views[TextureType::ColorArrayCube as usize] =
                    Some(create(TextureType::ColorArrayCube, None)?);
                image_views[TextureType::ColorArrayCube as usize]
                    .as_ref()
                    .unwrap()
                    .clone()
            }
            ImageViewType::Buffer => return Err(MetalImageViewError::BufferViewRequiresBuffer),
        };

        let render_target = if view.is_render_target() && render_format != format {
            unsafe {
                image
                    .handle()
                    .newTextureViewWithPixelFormat_textureType_levels_slices_swizzle(
                        render_format,
                        sampled_render_target.textureType(),
                        levels,
                        slices,
                        identity_swizzle(),
                    )
            }
            .ok_or(MetalImageViewError::CreationFailed {
                texture_type: TextureType::Color2D,
                format: view.format,
            })?
        } else {
            sampled_render_target
        };

        let depth_view = if matches!(
            get_format_type(view.format),
            SurfaceType::Depth | SurfaceType::DepthStencil
        ) {
            // Metal samples depth directly from a combined depth/stencil
            // texture. Creating a Depth32Float view of Depth32Float_Stencil8
            // is rejected by Apple GPUs.
            Some(render_target.clone())
        } else {
            None
        };
        let stencil_view = if get_format_type(view.format) == SurfaceType::DepthStencil {
            Some(make_aspect_view(
                image,
                MTLPixelFormat::X32_Stencil8,
                &render_target,
                levels,
                slices,
            )?)
        } else if get_format_type(view.format) == SurfaceType::Stencil {
            Some(render_target.clone())
        } else {
            None
        };

        Ok(Self {
            base,
            image: image.retained_handle(),
            image_views,
            render_target,
            depth_view,
            stencil_view,
            samples: image.samples(),
        })
    }

    pub fn base(&self) -> &ImageViewBase {
        unsafe { self.base.as_ref() }
    }

    pub fn handle(&self, texture_type: TextureType) -> Option<&ProtocolObject<dyn MTLTexture>> {
        self.image_views[texture_type as usize].as_deref()
    }

    pub(crate) fn retained_handle(
        &self,
        texture_type: TextureType,
    ) -> Option<Retained<ProtocolObject<dyn MTLTexture>>> {
        self.image_views[texture_type as usize].clone()
    }

    pub fn render_target(&self) -> &ProtocolObject<dyn MTLTexture> {
        &self.render_target
    }

    pub(crate) fn retained_render_target(&self) -> Retained<ProtocolObject<dyn MTLTexture>> {
        self.render_target.clone()
    }

    pub fn image_handle(&self) -> &ProtocolObject<dyn MTLTexture> {
        &self.image
    }

    pub fn depth_view(&self) -> Option<&ProtocolObject<dyn MTLTexture>> {
        self.depth_view.as_deref()
    }

    pub(crate) fn retained_depth_view(&self) -> Option<Retained<ProtocolObject<dyn MTLTexture>>> {
        self.depth_view.clone()
    }

    pub fn stencil_view(&self) -> Option<&ProtocolObject<dyn MTLTexture>> {
        self.stencil_view.as_deref()
    }

    pub(crate) fn retained_stencil_view(&self) -> Option<Retained<ProtocolObject<dyn MTLTexture>>> {
        self.stencil_view.clone()
    }

    pub fn samples(&self) -> u32 {
        self.samples
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MetalImageAspect {
    Color,
    Depth,
    Stencil,
    DepthStencil,
}

fn image_view_aspect(view: &ImageViewBase) -> MetalImageAspect {
    if view.is_render_target() {
        return match get_format_type(view.format) {
            SurfaceType::Depth => MetalImageAspect::Depth,
            SurfaceType::Stencil => MetalImageAspect::Stencil,
            SurfaceType::DepthStencil => MetalImageAspect::DepthStencil,
            _ => MetalImageAspect::Color,
        };
    }
    let any_r = view
        .swizzle
        .iter()
        .any(|source| *source == SwizzleSource::R as u8);
    match view.format {
        PixelFormat::D24UnormS8Uint | PixelFormat::D32FloatS8Uint => {
            if any_r {
                MetalImageAspect::Depth
            } else {
                MetalImageAspect::Stencil
            }
        }
        PixelFormat::S8UintD24Unorm => {
            if any_r {
                MetalImageAspect::Stencil
            } else {
                MetalImageAspect::Depth
            }
        }
        PixelFormat::D16Unorm | PixelFormat::D32Float | PixelFormat::X8D24Unorm => {
            MetalImageAspect::Depth
        }
        PixelFormat::S8Uint => MetalImageAspect::Stencil,
        _ => MetalImageAspect::Color,
    }
}

fn aspect_format(
    format: PixelFormat,
    aspect: MetalImageAspect,
    fallback: MTLPixelFormat,
) -> MTLPixelFormat {
    match (get_format_type(format), aspect) {
        (SurfaceType::DepthStencil, MetalImageAspect::Stencil) => MTLPixelFormat::X32_Stencil8,
        _ => fallback,
    }
}

fn make_aspect_view(
    image: &MetalImage,
    format: MTLPixelFormat,
    render_target: &ProtocolObject<dyn MTLTexture>,
    levels: NSRange,
    slices: NSRange,
) -> Result<Retained<ProtocolObject<dyn MTLTexture>>, MetalImageViewError> {
    unsafe {
        image
            .handle()
            .newTextureViewWithPixelFormat_textureType_levels_slices_swizzle(
                format,
                render_target.textureType(),
                levels,
                slices,
                identity_swizzle(),
            )
    }
    .ok_or(MetalImageViewError::CreationFailed {
        texture_type: TextureType::Color2D,
        format: PixelFormat::D32FloatS8Uint,
    })
}

fn ns_range(base: i32, count: i32) -> NSRange {
    NSRange::new(base.max(0) as usize, count.max(1) as usize)
}

fn metal_texture_type(texture_type: TextureType, samples: u32) -> MTLTextureType {
    match texture_type {
        TextureType::Color1D => MTLTextureType::Type1D,
        TextureType::ColorArray1D => MTLTextureType::Type1DArray,
        TextureType::Color2D | TextureType::Color2DRect if samples > 1 => {
            MTLTextureType::Type2DMultisample
        }
        TextureType::ColorArray2D if samples > 1 => MTLTextureType::Type2DMultisampleArray,
        TextureType::Color2D | TextureType::Color2DRect => MTLTextureType::Type2D,
        TextureType::ColorArray2D => MTLTextureType::Type2DArray,
        TextureType::Color3D => MTLTextureType::Type3D,
        TextureType::ColorCube => MTLTextureType::TypeCube,
        TextureType::ColorArrayCube => MTLTextureType::TypeCubeArray,
        TextureType::Buffer => MTLTextureType::TypeTextureBuffer,
    }
}

fn swizzle_channels(swizzle: [u8; 4]) -> MTLTextureSwizzleChannels {
    MTLTextureSwizzleChannels {
        red: metal_swizzle(swizzle[0]),
        green: metal_swizzle(swizzle[1]),
        blue: metal_swizzle(swizzle[2]),
        alpha: metal_swizzle(swizzle[3]),
    }
}

fn identity_swizzle() -> MTLTextureSwizzleChannels {
    swizzle_channels([
        SwizzleSource::R as u8,
        SwizzleSource::G as u8,
        SwizzleSource::B as u8,
        SwizzleSource::A as u8,
    ])
}

fn metal_swizzle(source: u8) -> MTLTextureSwizzle {
    match source {
        value if value == SwizzleSource::Zero as u8 => MTLTextureSwizzle::Zero,
        value if value == SwizzleSource::R as u8 => MTLTextureSwizzle::Red,
        value if value == SwizzleSource::G as u8 => MTLTextureSwizzle::Green,
        value if value == SwizzleSource::B as u8 => MTLTextureSwizzle::Blue,
        value if value == SwizzleSource::A as u8 => MTLTextureSwizzle::Alpha,
        value if value == SwizzleSource::OneInt as u8 || value == SwizzleSource::OneFloat as u8 => {
            MTLTextureSwizzle::One
        }
        _ => MTLTextureSwizzle::Zero,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::slot_vector::SlotId;

    use crate::surface::PixelFormat;
    use crate::texture_cache::image_info::ImageInfo;
    use crate::texture_cache::image_view_info::ImageViewInfo;
    use crate::texture_cache::types::{Extent3D, ImageType, SubresourceExtent};

    #[test]
    fn creates_native_2d_and_array_aliases_with_guest_swizzle() {
        let device = super::super::metal_device::MetalDevice::new().unwrap();
        let info = ImageInfo {
            format: PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            size: Extent3D {
                width: 64,
                height: 32,
                depth: 1,
            },
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            ..ImageInfo::default()
        };
        let image = MetalImage::new(&device, &info).unwrap();
        let view_info = ImageViewInfo {
            view_type: ImageViewType::E2D,
            format: info.format,
            x_source: SwizzleSource::B as u8,
            y_source: SwizzleSource::G as u8,
            z_source: SwizzleSource::R as u8,
            w_source: SwizzleSource::A as u8,
            ..ImageViewInfo::default()
        };
        let mut base = Box::new(ImageViewBase::new(
            &view_info,
            &info,
            SlotId { index: 1 },
            0x1000,
        ));
        let view = MetalImageView::new(NonNull::from(base.as_mut()), &image).unwrap();
        assert!(view.handle(TextureType::Color2D).is_some());
        assert!(view.handle(TextureType::ColorArray2D).is_some());
        assert_eq!(
            view.handle(TextureType::Color2D).unwrap().swizzle().red,
            MTLTextureSwizzle::Blue
        );
    }

    #[test]
    fn separates_depth_and_stencil_sampling_views() {
        let device = super::super::metal_device::MetalDevice::new().unwrap();
        let info = ImageInfo {
            format: PixelFormat::D32FloatS8Uint,
            image_type: ImageType::E2D,
            size: Extent3D {
                width: 64,
                height: 32,
                depth: 1,
            },
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            ..ImageInfo::default()
        };
        let image = MetalImage::new(&device, &info).unwrap();
        let view_info =
            ImageViewInfo::for_render_target(ImageViewType::E2D, info.format, Default::default());
        let mut base = Box::new(ImageViewBase::new(
            &view_info,
            &info,
            SlotId { index: 1 },
            0x1000,
        ));
        let view = MetalImageView::new(NonNull::from(base.as_mut()), &image).unwrap();
        assert_eq!(
            view.depth_view().unwrap().pixelFormat(),
            MTLPixelFormat::Depth32Float_Stencil8
        );
        assert_eq!(
            view.stencil_view().unwrap().pixelFormat(),
            MTLPixelFormat::X32_Stencil8
        );
    }
}
