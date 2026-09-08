// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native counterpart of present/layer.{h,cpp}. Texture resolution stays in
//! MetalTextureCache; this snapshot retains identity until command encoding.
use crate::framebuffer_config::{normalize_crop, BlendMode, FramebufferConfig};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_metal::MTLTexture;

pub struct Layer {
    pub texture: Retained<ProtocolObject<dyn MTLTexture>>,
    pub crop: [f32; 4],
    pub blending: BlendMode,
}

impl Layer {
    pub fn configure_draw(
        texture: Retained<ProtocolObject<dyn MTLTexture>>,
        config: &FramebufferConfig,
        width: u32,
        height: u32,
    ) -> Self {
        let crop = normalize_crop(config, width, height);
        Self {
            texture,
            crop: [crop.left, crop.top, crop.right, crop.bottom],
            blending: config.blending,
        }
    }

    pub fn opaque(texture: Retained<ProtocolObject<dyn MTLTexture>>) -> Self {
        Self {
            texture,
            crop: [0.0, 0.0, 1.0, 1.0],
            blending: BlendMode::Opaque,
        }
    }
}
