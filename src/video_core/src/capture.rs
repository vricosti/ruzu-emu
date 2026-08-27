// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/capture.h
//!
//! Constants for frame capture configuration.

/// Block height for capture tiling.
pub const BLOCK_HEIGHT: u32 = 4;

/// Block depth for capture tiling.
pub const BLOCK_DEPTH: u32 = 0;

/// Log2 of bytes per pixel.
pub const BPP_LOG2: u32 = 2;

/// Applet-capture pixel format.
pub const PIXEL_FORMAT: crate::surface::PixelFormat = crate::surface::PixelFormat::B8G8R8A8Unorm;

/// Linear width derived from the undocked screen layout.
pub const LINEAR_WIDTH: u32 = ruzu_core::frontend::framebuffer_layout::ScreenUndocked::WIDTH;

/// Linear height derived from the undocked screen layout.
pub const LINEAR_HEIGHT: u32 = ruzu_core::frontend::framebuffer_layout::ScreenUndocked::HEIGHT;

/// Linear depth.
pub const LINEAR_DEPTH: u32 = 1;

/// Bytes per pixel.
pub const BYTES_PER_PIXEL: u32 = 4;

/// Tiled width matches linear width.
pub const TILED_WIDTH: u32 = LINEAR_WIDTH;

/// Tiled height aligned to block parameters.
pub const TILED_HEIGHT: u32 =
    common::alignment::align_up_log2(LINEAR_HEIGHT as u64, BLOCK_HEIGHT + BLOCK_DEPTH + BPP_LOG2)
        as u32;

/// Total tiled capture size in bytes.
pub const TILED_SIZE: u32 = TILED_WIDTH * TILED_HEIGHT * (1 << BPP_LOG2);

/// Upstream `VideoCore::Capture::Layout`.
pub const LAYOUT: ruzu_core::frontend::framebuffer_layout::FramebufferLayout =
    ruzu_core::frontend::framebuffer_layout::FramebufferLayout {
        width: LINEAR_WIDTH,
        height: LINEAR_HEIGHT,
        screen: ruzu_core::frontend::framebuffer_layout::Rectangle {
            left: 0,
            top: 0,
            right: LINEAR_WIDTH,
            bottom: LINEAR_HEIGHT,
        },
        is_srgb: false,
    };

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_constants_match_upstream_layout_and_tiling() {
        assert_eq!(PIXEL_FORMAT, crate::surface::PixelFormat::B8G8R8A8Unorm);
        assert_eq!((LAYOUT.width, LAYOUT.height), (LINEAR_WIDTH, LINEAR_HEIGHT));
        assert_eq!(
            (
                LAYOUT.screen.left,
                LAYOUT.screen.top,
                LAYOUT.screen.right,
                LAYOUT.screen.bottom
            ),
            (0, 0, LINEAR_WIDTH, LINEAR_HEIGHT)
        );
        assert!(!LAYOUT.is_srgb);
        assert_eq!(TILED_HEIGHT, 768);
        assert_eq!(TILED_SIZE, TILED_WIDTH * TILED_HEIGHT * (1 << BPP_LOG2));
    }
}
