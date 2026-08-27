// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `video_core/framebuffer_config.h` and `framebuffer_config.cpp`.

use common::math_util::Rectangle;
use ruzu_core::hle::service::nvnflinger::buffer_transform_flags::BufferTransformFlags;
use ruzu_core::hle::service::nvnflinger::pixel_format::PixelFormat;

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
#[derive(Debug, Clone, Default)]
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
