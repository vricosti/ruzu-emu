// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Metal framebuffer attachment ownership.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLClearColor, MTLLoadAction, MTLPixelFormat, MTLRenderPassDescriptor, MTLStoreAction,
    MTLTexture,
};
use thiserror::Error;

use crate::surface::{get_format_type, SurfaceType};
use crate::texture_cache::render_targets::RenderTargets;
use crate::texture_cache::types::NUM_RT;

use super::metal_image_view::MetalImageView;

#[derive(Debug, Error)]
pub enum MetalFramebufferError {
    #[error("Metal framebuffer attachments use different sample counts")]
    SampleCountMismatch,
}

/// Metal counterpart of Eden's `Framebuffer`.
///
/// Metal has no persistent framebuffer object. This owner retains the exact
/// image views selected by the common texture cache and materializes a render
/// pass descriptor only when the scheduler begins a pass.
pub struct MetalFramebuffer {
    color_attachments: [Option<Retained<ProtocolObject<dyn MTLTexture>>>; NUM_RT],
    depth_attachment: Option<Retained<ProtocolObject<dyn MTLTexture>>>,
    stencil_attachment: Option<Retained<ProtocolObject<dyn MTLTexture>>>,
    render_area: (u32, u32),
    samples: u32,
    layers: usize,
    num_color_buffers: u32,
    has_depth: bool,
    has_stencil: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MetalFramebufferSignature {
    pub color_formats: [MTLPixelFormat; NUM_RT],
    pub depth_format: MTLPixelFormat,
    pub stencil_format: MTLPixelFormat,
    pub samples: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MetalFramebufferClear {
    pub color: Option<(usize, [f32; 4])>,
    pub depth: Option<f32>,
    pub stencil: Option<u32>,
    pub base_layer: u32,
    pub layer_count: u32,
}

impl MetalFramebuffer {
    /// Port of Eden `Framebuffer::CreateFramebuffer`.
    pub fn new(
        color_buffers: [Option<&MetalImageView>; NUM_RT],
        depth_buffer: Option<&MetalImageView>,
        key: &RenderTargets,
    ) -> Result<Self, MetalFramebufferError> {
        let resolution = common::settings::values().resolution_info.clone();
        let attachment_extent = |value: u32| {
            if key.is_rescaled {
                resolution.scale_up_u32(value)
            } else {
                value
            }
        };
        let mut width = key.size.width;
        let mut height = key.size.height;
        let mut samples = None;
        let mut layers = 1usize;
        let mut num_color_buffers = 0;
        let color_attachments = std::array::from_fn(|index| {
            color_buffers[index].map(|view| {
                width = width.min(attachment_extent(view.base().size.width));
                height = height.min(attachment_extent(view.base().size.height));
                layers = layers.max(view.base().range.extent.layers.max(1) as usize);
                num_color_buffers += 1;
                samples.get_or_insert(view.samples());
                view.retained_render_target()
            })
        });

        let mut depth_attachment = None;
        let mut stencil_attachment = None;
        let mut has_depth = false;
        let mut has_stencil = false;
        if let Some(view) = depth_buffer {
            width = width.min(attachment_extent(view.base().size.width));
            height = height.min(attachment_extent(view.base().size.height));
            layers = layers.max(view.base().range.extent.layers.max(1) as usize);
            if samples.is_some_and(|value| value != view.samples()) {
                return Err(MetalFramebufferError::SampleCountMismatch);
            }
            samples.get_or_insert(view.samples());
            match get_format_type(view.base().format) {
                SurfaceType::Depth => {
                    has_depth = true;
                    depth_attachment = view.retained_depth_view();
                }
                SurfaceType::Stencil => {
                    has_stencil = true;
                    stencil_attachment = view.retained_stencil_view();
                }
                SurfaceType::DepthStencil => {
                    has_depth = true;
                    has_stencil = true;
                    depth_attachment = view.retained_depth_view();
                    // Metal requires both attachments to use the combined
                    // depth/stencil format while rendering. The stencil-only
                    // aspect view is reserved for sampling.
                    stencil_attachment = Some(view.retained_render_target());
                }
                _ => {}
            }
        }
        let samples = samples.unwrap_or(1);
        if color_buffers
            .iter()
            .flatten()
            .any(|view| view.samples() != samples)
        {
            return Err(MetalFramebufferError::SampleCountMismatch);
        }

        Ok(Self {
            color_attachments,
            depth_attachment,
            stencil_attachment,
            render_area: (width, height),
            samples,
            layers,
            num_color_buffers,
            has_depth,
            has_stencil,
        })
    }

    /// Build the LOAD/STORE pass used by ordinary draws. Guest clears are
    /// encoded explicitly, matching Eden's attachment persistence model.
    pub fn render_pass_descriptor(&self) -> Retained<MTLRenderPassDescriptor> {
        let descriptor = MTLRenderPassDescriptor::renderPassDescriptor();
        let attachments = descriptor.colorAttachments();
        for (index, texture) in self.color_attachments.iter().enumerate() {
            let Some(texture) = texture else {
                continue;
            };
            let attachment = unsafe { attachments.objectAtIndexedSubscript(index) };
            attachment.setTexture(Some(texture));
            attachment.setLoadAction(MTLLoadAction::Load);
            attachment.setStoreAction(MTLStoreAction::Store);
        }
        if let Some(texture) = self.depth_attachment.as_ref() {
            let attachment = descriptor.depthAttachment();
            attachment.setTexture(Some(texture));
            attachment.setLoadAction(MTLLoadAction::Load);
            attachment.setStoreAction(MTLStoreAction::Store);
        }
        if let Some(texture) = self.stencil_attachment.as_ref() {
            let attachment = descriptor.stencilAttachment();
            attachment.setTexture(Some(texture));
            attachment.setLoadAction(MTLLoadAction::Load);
            attachment.setStoreAction(MTLStoreAction::Store);
        }
        unsafe {
            descriptor.setRenderTargetWidth(self.render_area.0 as usize);
            descriptor.setRenderTargetHeight(self.render_area.1 as usize);
            descriptor.setRenderTargetArrayLength(self.layers);
            descriptor.setDefaultRasterSampleCount(self.samples as usize);
        }
        descriptor
    }

    pub fn render_pass_descriptor_for_layer(
        &self,
        layer: u32,
    ) -> Retained<MTLRenderPassDescriptor> {
        let descriptor = self.render_pass_descriptor();
        for index in 0..NUM_RT {
            let attachment = unsafe {
                descriptor
                    .colorAttachments()
                    .objectAtIndexedSubscript(index)
            };
            if attachment.texture().is_some() {
                unsafe { attachment.setSlice(layer as usize) };
            }
        }
        if descriptor.depthAttachment().texture().is_some() {
            unsafe { descriptor.depthAttachment().setSlice(layer as usize) };
        }
        if descriptor.stencilAttachment().texture().is_some() {
            unsafe { descriptor.stencilAttachment().setSlice(layer as usize) };
        }
        unsafe { descriptor.setRenderTargetArrayLength(1) };
        descriptor
    }

    /// Build a targeted attachment clear. Only attachments selected by the
    /// Maxwell clear flags are present, so unrelated render targets retain
    /// their contents without relying on a shader write mask.
    pub fn clear_render_pass_descriptor(
        &self,
        clear: MetalFramebufferClear,
    ) -> Retained<MTLRenderPassDescriptor> {
        let descriptor = MTLRenderPassDescriptor::renderPassDescriptor();
        if let Some((index, value)) = clear.color {
            if let Some(texture) = self.color_attachments.get(index).and_then(Option::as_ref) {
                let attachment = unsafe {
                    descriptor
                        .colorAttachments()
                        .objectAtIndexedSubscript(index)
                };
                attachment.setTexture(Some(texture));
                attachment.setLoadAction(MTLLoadAction::Clear);
                attachment.setStoreAction(MTLStoreAction::Store);
                attachment.setClearColor(MTLClearColor {
                    red: value[0] as f64,
                    green: value[1] as f64,
                    blue: value[2] as f64,
                    alpha: value[3] as f64,
                });
                unsafe { attachment.setSlice(clear.base_layer as usize) };
            }
        }
        if let Some(value) = clear.depth {
            if let Some(texture) = self.depth_attachment.as_ref() {
                let attachment = descriptor.depthAttachment();
                attachment.setTexture(Some(texture));
                attachment.setLoadAction(MTLLoadAction::Clear);
                attachment.setStoreAction(MTLStoreAction::Store);
                attachment.setClearDepth(value as f64);
                unsafe { attachment.setSlice(clear.base_layer as usize) };
            }
        }
        if let Some(value) = clear.stencil {
            if let Some(texture) = self.stencil_attachment.as_ref() {
                let attachment = descriptor.stencilAttachment();
                attachment.setTexture(Some(texture));
                attachment.setLoadAction(MTLLoadAction::Clear);
                attachment.setStoreAction(MTLStoreAction::Store);
                attachment.setClearStencil(value);
                unsafe { attachment.setSlice(clear.base_layer as usize) };
            }
        }
        unsafe {
            descriptor.setRenderTargetWidth(self.render_area.0 as usize);
            descriptor.setRenderTargetHeight(self.render_area.1 as usize);
            descriptor.setRenderTargetArrayLength(clear.layer_count.max(1) as usize);
            descriptor.setDefaultRasterSampleCount(self.samples as usize);
        }
        descriptor
    }

    pub fn render_area(&self) -> (u32, u32) {
        self.render_area
    }

    pub fn samples(&self) -> u32 {
        self.samples
    }

    pub fn num_color_buffers(&self) -> u32 {
        self.num_color_buffers
    }

    pub fn color_formats(&self) -> [MTLPixelFormat; NUM_RT] {
        std::array::from_fn(|index| {
            self.color_attachments[index]
                .as_ref()
                .map_or(MTLPixelFormat::Invalid, |texture| texture.pixelFormat())
        })
    }

    pub fn depth_format(&self) -> MTLPixelFormat {
        self.depth_attachment
            .as_ref()
            .map_or(MTLPixelFormat::Invalid, |texture| texture.pixelFormat())
    }

    pub fn stencil_format(&self) -> MTLPixelFormat {
        self.stencil_attachment
            .as_ref()
            .map_or(MTLPixelFormat::Invalid, |texture| texture.pixelFormat())
    }

    pub fn has_depth(&self) -> bool {
        self.has_depth
    }

    pub fn has_stencil(&self) -> bool {
        self.has_stencil
    }

    pub fn signature(&self) -> MetalFramebufferSignature {
        MetalFramebufferSignature {
            color_formats: self.color_formats(),
            depth_format: self.depth_format(),
            stencil_format: self.stencil_format(),
            samples: self.samples,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ptr::NonNull;

    use common::slot_vector::SlotId;

    use super::*;
    use crate::renderer_metal::metal_device::MetalDevice;
    use crate::renderer_metal::metal_image::MetalImage;
    use crate::surface::PixelFormat;
    use crate::texture_cache::image_info::ImageInfo;
    use crate::texture_cache::image_view_base::ImageViewBase;
    use crate::texture_cache::image_view_info::ImageViewInfo;
    use crate::texture_cache::types::{
        Extent2D, Extent3D, ImageType, ImageViewType, SubresourceExtent,
    };

    #[test]
    fn retains_common_cache_views_and_preserves_sparse_rt_slots() {
        let device = MetalDevice::new().unwrap();
        let info = ImageInfo {
            format: PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            size: Extent3D {
                width: 640,
                height: 360,
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
        let mut colors = [None; NUM_RT];
        colors[2] = Some(&view);
        let framebuffer = MetalFramebuffer::new(
            colors,
            None,
            &RenderTargets {
                size: Extent2D {
                    width: 1280,
                    height: 720,
                },
                ..RenderTargets::default()
            },
        )
        .unwrap();
        assert_eq!(framebuffer.render_area(), (640, 360));
        assert_eq!(framebuffer.num_color_buffers(), 1);
        assert_eq!(framebuffer.samples(), 1);
        let descriptor = framebuffer.render_pass_descriptor();
        let attachment = unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(2) };
        assert!(attachment.texture().is_some());
    }

    #[test]
    fn combined_depth_stencil_uses_one_render_attachment_format() {
        let device = MetalDevice::new().unwrap();
        let info = ImageInfo {
            format: PixelFormat::D32FloatS8Uint,
            image_type: ImageType::E2D,
            size: Extent3D {
                width: 64,
                height: 64,
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
            SlotId { index: 2 },
            0x2000,
        ));
        let view = MetalImageView::new(NonNull::from(base.as_mut()), &image).unwrap();
        let framebuffer = MetalFramebuffer::new(
            [None; NUM_RT],
            Some(&view),
            &RenderTargets {
                size: Extent2D {
                    width: 64,
                    height: 64,
                },
                ..RenderTargets::default()
            },
        )
        .unwrap();

        assert_eq!(
            framebuffer.depth_format(),
            MTLPixelFormat::Depth32Float_Stencil8
        );
        assert_eq!(framebuffer.stencil_format(), framebuffer.depth_format());
        let descriptor = framebuffer.render_pass_descriptor();
        assert_eq!(
            descriptor
                .depthAttachment()
                .texture()
                .unwrap()
                .pixelFormat(),
            descriptor
                .stencilAttachment()
                .texture()
                .unwrap()
                .pixelFormat()
        );
    }
}
