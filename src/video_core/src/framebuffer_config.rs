// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `video_core/framebuffer_config.h` and `framebuffer_config.cpp`.

use common::math_util::Rectangle;
use ruzu_core::hle::service::nvnflinger::buffer_transform_flags::BufferTransformFlags;
use ruzu_core::hle::service::nvnflinger::pixel_format::PixelFormat;
use ruzu_core::hle::service::nvnflinger::hwc_layer::{
    layer_stack_bit, LayerStackId, DEFAULT_LAYER_STACK_MASK,
};

/// Represents a pointer in the device-specific virtual address space.
pub type DAddr = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlendMode {
    #[default]
    Opaque,
    Premultiplied,
    Coverage,
}

/// Port of `Tegra::FramebufferConfig`.
#[derive(Debug, Clone)]
pub struct FramebufferConfig {
    pub address: DAddr,
    pub offset: u32,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub pixel_format: PixelFormat,
    pub transform_flags: BufferTransformFlags,
    pub crop_rect: Rectangle<i32>,
    pub blending: BlendMode,
    pub layer_stack_mask: u32,
}

impl Default for FramebufferConfig {
    fn default() -> Self {
        Self {
            address: 0,
            offset: 0,
            width: 0,
            height: 0,
            stride: 0,
            pixel_format: PixelFormat::default(),
            transform_flags: BufferTransformFlags::default(),
            crop_rect: Rectangle::default(),
            blending: BlendMode::default(),
            layer_stack_mask: DEFAULT_LAYER_STACK_MASK,
        }
    }
}

/// Port of Tegra::FilterLayerStack. The all-matching path borrows the input
/// without touching scratch; otherwise the returned slice borrows scratch.
pub fn filter_layer_stack<'a>(
    layers: &'a [FramebufferConfig],
    stack: LayerStackId,
    scratch: &'a mut Vec<FramebufferConfig>,
) -> &'a [FramebufferConfig] {
    let bit = layer_stack_bit(stack);
    if layers.iter().all(|layer| layer.layer_stack_mask & bit != 0) {
        return layers;
    }
    scratch.clear();
    for layer in layers {
        if layer.layer_stack_mask & bit != 0 {
            scratch.push(layer.clone());
        }
    }
    scratch
}

/// Port of `Tegra::NormalizeCrop`.
pub fn normalize_crop(
    framebuffer: &FramebufferConfig,
    texture_width: u32,
    texture_height: u32,
) -> Rectangle<f32> {
    let (mut left, mut top, mut right, mut bottom);

    if !framebuffer.crop_rect.is_empty() {
        left = framebuffer.crop_rect.left as f32;
        top = framebuffer.crop_rect.top as f32;
        right = framebuffer.crop_rect.right as f32;
        bottom = framebuffer.crop_rect.bottom as f32;
    } else {
        left = 0.0;
        top = 0.0;
        right = framebuffer.width as f32;
        bottom = framebuffer.height as f32;
    }

    let mut framebuffer_transform_flags = framebuffer.transform_flags;

    if framebuffer_transform_flags.contains(BufferTransformFlags::FLIP_H) {
        std::mem::swap(&mut left, &mut right);
    }
    if framebuffer_transform_flags.contains(BufferTransformFlags::FLIP_V) {
        std::mem::swap(&mut top, &mut bottom);
    }

    framebuffer_transform_flags.remove(BufferTransformFlags::FLIP_H);
    framebuffer_transform_flags.remove(BufferTransformFlags::FLIP_V);
    if !framebuffer_transform_flags.is_empty() {
        log::warn!(
            "Unsupported framebuffer_transform_flags={}",
            framebuffer_transform_flags.bits()
        );
    }

    left /= texture_width as f32;
    top /= texture_height as f32;
    right /= texture_width as f32;
    bottom /= texture_height as f32;

    Rectangle {
        left,
        top,
        right,
        bottom,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_filter_preserves_order_and_borrows_when_all_match() {
        let layers = vec![
            FramebufferConfig { address: 1, ..Default::default() },
            FramebufferConfig { address: 2, layer_stack_mask: 1, ..Default::default() },
            FramebufferConfig { address: 3, ..Default::default() },
        ];
        let mut scratch = vec![FramebufferConfig::default()];
        assert_eq!(FramebufferConfig::default().layer_stack_mask, 0x1d);
        let all = filter_layer_stack(&layers, LayerStackId::Default, &mut scratch);
        assert_eq!(all.as_ptr(), layers.as_ptr());
        assert_eq!(scratch.len(), 1);
        let filtered = filter_layer_stack(&layers, LayerStackId::LastFrame, &mut scratch);
        assert_eq!(filtered.iter().map(|layer| layer.address).collect::<Vec<_>>(), [1, 3]);
        assert!(filter_layer_stack(&layers, LayerStackId::Null, &mut scratch).is_empty());
        scratch.push(FramebufferConfig::default());
        assert!(filter_layer_stack(&[], LayerStackId::Default, &mut scratch).is_empty());
        assert_eq!(scratch.len(), 1);
    }

    #[test]
    fn zero_width_crop_uses_framebuffer_dimensions() {
        let framebuffer = FramebufferConfig {
            width: 640,
            height: 360,
            crop_rect: Rectangle::new(12, 24, 12, 96),
            ..Default::default()
        };

        assert_eq!(
            normalize_crop(&framebuffer, 640, 360),
            Rectangle::new(0.0, 0.0, 1.0, 1.0)
        );
    }

    #[test]
    fn crop_and_flip_order_matches_upstream() {
        let framebuffer = FramebufferConfig {
            crop_rect: Rectangle::new(16, 8, 80, 40),
            transform_flags: BufferTransformFlags::FLIP_H | BufferTransformFlags::FLIP_V,
            ..Default::default()
        };

        assert_eq!(
            normalize_crop(&framebuffer, 128, 64),
            Rectangle::new(0.625, 0.625, 0.125, 0.125)
        );
    }

    #[test]
    fn framebuffer_uses_canonical_android_types() {
        let framebuffer = FramebufferConfig::default();
        let _: PixelFormat = framebuffer.pixel_format;
        let _: BufferTransformFlags = framebuffer.transform_flags;
        let _: Rectangle<i32> = framebuffer.crop_rect;
    }
}
