// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/core/hle/service/nvnflinger/hwc_layer.h

use common::math_util::Rectangle;

use super::buffer_transform_flags::BufferTransformFlags;
use super::pixel_format::PixelFormat;
use super::ui::fence::Fence;

/// hwc_layer_t::blending values
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerBlending {
    /// No blending
    None = 0x100,
    /// ONE / ONE_MINUS_SRC_ALPHA
    Premultiplied = 0x105,
    /// SRC_ALPHA / ONE_MINUS_SRC_ALPHA
    Coverage = 0x405,
}

impl Default for LayerBlending {
    fn default() -> Self {
        LayerBlending::None
    }
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerStackId {
    Default = 0,
    Lcd = 1,
    Screenshot = 2,
    Recording = 3,
    LastFrame = 4,
    Arbitrary = 5,
    ApplicationForDebug = 6,
    Null = 10,
}

pub const fn layer_stack_bit(id: LayerStackId) -> u32 {
    1u32 << id as u32
}

pub const DEFAULT_LAYER_STACK_MASK: u32 = layer_stack_bit(LayerStackId::Default)
    | layer_stack_bit(LayerStackId::Screenshot)
    | layer_stack_bit(LayerStackId::Recording)
    | layer_stack_bit(LayerStackId::LastFrame);

pub struct HwcLayer {
    pub buffer_handle: u32,
    pub offset: u32,
    pub format: PixelFormat,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub z_index: i32,
    pub blending: LayerBlending,
    pub transform: BufferTransformFlags,
    pub crop_rect: Rectangle<i32>,
    pub acquire_fence: Fence,
    pub layer_stack_mask: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_ids_and_default_mask_match_upstream() {
        for (id, bit) in [
            (LayerStackId::Default, 0x1),
            (LayerStackId::Lcd, 0x2),
            (LayerStackId::Screenshot, 0x4),
            (LayerStackId::Recording, 0x8),
            (LayerStackId::LastFrame, 0x10),
            (LayerStackId::Arbitrary, 0x20),
            (LayerStackId::ApplicationForDebug, 0x40),
            (LayerStackId::Null, 0x400),
        ] {
            assert_eq!(layer_stack_bit(id), bit);
        }
        assert_eq!(DEFAULT_LAYER_STACK_MASK, 0x1d);
    }
}
