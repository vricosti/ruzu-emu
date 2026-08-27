// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/texture_cache/formatter.h and formatter.cpp
//!
//! `Display` / debug-format implementations for texture-cache types, plus
//! `name()` helpers for `ImageBase`, `ImageViewBase`, and `RenderTargets`.

use std::fmt;

use super::format_lookup_table::PixelFormat;
use super::image_base::{GPUVAddr, ImageBase};
use super::image_view_base::ImageViewBase;
use super::render_targets::RenderTargets;
use super::samples_helper::samples_log2;
use super::types::*;

// ── Display for PixelFormat ────────────────────────────────────────────

impl fmt::Display for PixelFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PixelFormat::A8B8G8R8Unorm => "A8B8G8R8_UNORM",
            PixelFormat::A8B8G8R8Snorm => "A8B8G8R8_SNORM",
            PixelFormat::A8B8G8R8Sint => "A8B8G8R8_SINT",
            PixelFormat::A8B8G8R8Uint => "A8B8G8R8_UINT",
            PixelFormat::R5G6B5Unorm => "R5G6B5_UNORM",
            PixelFormat::B5G6R5Unorm => "B5G6R5_UNORM",
            PixelFormat::A1R5G5B5Unorm => "A1R5G5B5_UNORM",
            PixelFormat::A2B10G10R10Unorm => "A2B10G10R10_UNORM",
            PixelFormat::A2B10G10R10Uint => "A2B10G10R10_UINT",
            PixelFormat::A2R10G10B10Unorm => "A2R10G10B10_UNORM",
            PixelFormat::A1B5G5R5Unorm => "A1B5G5R5_UNORM",
            PixelFormat::A5B5G5R1Unorm => "A5B5G5R1_UNORM",
            PixelFormat::R8Unorm => "R8_UNORM",
            PixelFormat::R8Snorm => "R8_SNORM",
            PixelFormat::R8Sint => "R8_SINT",
            PixelFormat::R8Uint => "R8_UINT",
            PixelFormat::R16G16B16A16Float => "R16G16B16A16_FLOAT",
            PixelFormat::R16G16B16A16Unorm => "R16G16B16A16_UNORM",
            PixelFormat::R16G16B16A16Snorm => "R16G16B16A16_SNORM",
            PixelFormat::R16G16B16A16Sint => "R16G16B16A16_SINT",
            PixelFormat::R16G16B16A16Uint => "R16G16B16A16_UINT",
            PixelFormat::B10G11R11Float => "B10G11R11_FLOAT",
            PixelFormat::R32G32B32A32Uint => "R32G32B32A32_UINT",
            PixelFormat::Bc1RgbaUnorm => "BC1_RGBA_UNORM",
            PixelFormat::Bc2Unorm => "BC2_UNORM",
            PixelFormat::Bc3Unorm => "BC3_UNORM",
            PixelFormat::Bc4Unorm => "BC4_UNORM",
            PixelFormat::Bc4Snorm => "BC4_SNORM",
            PixelFormat::Bc5Unorm => "BC5_UNORM",
            PixelFormat::Bc5Snorm => "BC5_SNORM",
            PixelFormat::Bc7Unorm => "BC7_UNORM",
            PixelFormat::Bc6hUfloat => "BC6H_UFLOAT",
            PixelFormat::Bc6hSfloat => "BC6H_SFLOAT",
            PixelFormat::Astc2d4x4Unorm => "ASTC_2D_4X4_UNORM",
            PixelFormat::B8G8R8A8Unorm => "B8G8R8A8_UNORM",
            PixelFormat::R32G32B32A32Float => "R32G32B32A32_FLOAT",
            PixelFormat::R32G32B32A32Sint => "R32G32B32A32_SINT",
            PixelFormat::R32G32Float => "R32G32_FLOAT",
            PixelFormat::R32G32Sint => "R32G32_SINT",
            PixelFormat::R32Float => "R32_FLOAT",
            PixelFormat::R16Float => "R16_FLOAT",
            PixelFormat::R16Unorm => "R16_UNORM",
            PixelFormat::R16Snorm => "R16_SNORM",
            PixelFormat::R16Uint => "R16_UINT",
            PixelFormat::R16Sint => "R16_SINT",
            PixelFormat::R16G16Unorm => "R16G16_UNORM",
            PixelFormat::R16G16Float => "R16G16_FLOAT",
            PixelFormat::R16G16Uint => "R16G16_UINT",
            PixelFormat::R16G16Sint => "R16G16_SINT",
            PixelFormat::R16G16Snorm => "R16G16_SNORM",
            PixelFormat::R32G32B32Float => "R32G32B32_FLOAT",
            PixelFormat::A8B8G8R8Srgb => "A8B8G8R8_SRGB",
            PixelFormat::R8G8Unorm => "R8G8_UNORM",
            PixelFormat::R8G8Snorm => "R8G8_SNORM",
            PixelFormat::R8G8Sint => "R8G8_SINT",
            PixelFormat::R8G8Uint => "R8G8_UINT",
            PixelFormat::R32G32Uint => "R32G32_UINT",
            PixelFormat::R16G16B16X16Float => "R16G16B16X16_FLOAT",
            PixelFormat::R32Uint => "R32_UINT",
            PixelFormat::R32Sint => "R32_SINT",
            PixelFormat::Astc2d8x8Unorm => "ASTC_2D_8X8_UNORM",
            PixelFormat::Astc2d8x5Unorm => "ASTC_2D_8X5_UNORM",
            PixelFormat::Astc2d5x4Unorm => "ASTC_2D_5X4_UNORM",
            PixelFormat::B8G8R8A8Srgb => "B8G8R8A8_SRGB",
            PixelFormat::Bc1RgbaSrgb => "BC1_RGBA_SRGB",
            PixelFormat::Bc2Srgb => "BC2_SRGB",
            PixelFormat::Bc3Srgb => "BC3_SRGB",
            PixelFormat::Bc7Srgb => "BC7_SRGB",
            PixelFormat::A4B4G4R4Unorm => "A4B4G4R4_UNORM",
            PixelFormat::G4R4Unorm => "G4R4_UNORM",
            PixelFormat::Astc2d4x4Srgb => "ASTC_2D_4X4_SRGB",
            PixelFormat::Astc2d8x8Srgb => "ASTC_2D_8X8_SRGB",
            PixelFormat::Astc2d8x5Srgb => "ASTC_2D_8X5_SRGB",
            PixelFormat::Astc2d5x4Srgb => "ASTC_2D_5X4_SRGB",
            PixelFormat::Astc2d5x5Unorm => "ASTC_2D_5X5_UNORM",
            PixelFormat::Astc2d5x5Srgb => "ASTC_2D_5X5_SRGB",
            PixelFormat::Astc2d10x8Unorm => "ASTC_2D_10X8_UNORM",
            PixelFormat::Astc2d10x8Srgb => "ASTC_2D_10X8_SRGB",
            PixelFormat::Astc2d6x6Unorm => "ASTC_2D_6X6_UNORM",
            PixelFormat::Astc2d6x6Srgb => "ASTC_2D_6X6_SRGB",
            PixelFormat::Astc2d10x6Unorm => "ASTC_2D_10X6_UNORM",
            PixelFormat::Astc2d10x6Srgb => "ASTC_2D_10X6_SRGB",
            PixelFormat::Astc2d10x5Unorm => "ASTC_2D_10X5_UNORM",
            PixelFormat::Astc2d10x5Srgb => "ASTC_2D_10X5_SRGB",
            PixelFormat::Astc2d10x10Unorm => "ASTC_2D_10X10_UNORM",
            PixelFormat::Astc2d10x10Srgb => "ASTC_2D_10X10_SRGB",
            PixelFormat::Astc2d12x10Unorm => "ASTC_2D_12X10_UNORM",
            PixelFormat::Astc2d12x10Srgb => "ASTC_2D_12X10_SRGB",
            PixelFormat::Astc2d12x12Unorm => "ASTC_2D_12X12_UNORM",
            PixelFormat::Astc2d12x12Srgb => "ASTC_2D_12X12_SRGB",
            PixelFormat::Astc2d8x6Unorm => "ASTC_2D_8X6_UNORM",
            PixelFormat::Astc2d8x6Srgb => "ASTC_2D_8X6_SRGB",
            PixelFormat::Astc2d6x5Unorm => "ASTC_2D_6X5_UNORM",
            PixelFormat::Astc2d6x5Srgb => "ASTC_2D_6X5_SRGB",
            PixelFormat::Etc2RgbUnorm => "ETC2_RGB_UNORM",
            PixelFormat::Etc2RgbaUnorm => "ETC2_RGBA_UNORM",
            PixelFormat::Etc2RgbPtaUnorm => "ETC2_RGB_PTA_UNORM",
            PixelFormat::Etc2RgbSrgb => "ETC2_RGB_SRGB",
            PixelFormat::Etc2RgbaSrgb => "ETC2_RGBA_SRGB",
            PixelFormat::Etc2RgbPtaSrgb => "ETC2_RGB_PTA_SRGB",
            PixelFormat::EacR11Unorm => "EAC_R11_UNORM",
            PixelFormat::EacR11Snorm => "EAC_R11_SNORM",
            PixelFormat::EacR11G11Unorm => "EAC_R11G11_UNORM",
            PixelFormat::EacR11G11Snorm => "EAC_R11G11_SNORM",
            PixelFormat::D32Float => "D32_FLOAT",
            PixelFormat::D16Unorm => "D16_UNORM",
            PixelFormat::X8D24Unorm => "X8_D24_UNORM",
            PixelFormat::S8Uint => "S8_UINT",
            PixelFormat::D24UnormS8Uint => "D24_UNORM_S8_UINT",
            PixelFormat::S8UintD24Unorm => "S8_UINT_D24_UNORM",
            PixelFormat::D32FloatS8Uint => "D32_FLOAT_S8_UINT",
            PixelFormat::E5B9G9R9Float => "E5B9G9R9_FLOAT",
            PixelFormat::MaxDepthStencilFormat | PixelFormat::Invalid => "Invalid",
        })
    }
}

// ── Display for ImageType ──────────────────────────────────────────────

impl fmt::Display for ImageType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ImageType::E1D => "1D",
            ImageType::E2D => "2D",
            ImageType::E3D => "3D",
            ImageType::Linear => "Linear",
            ImageType::Buffer => "Buffer",
        })
    }
}

// ── Display for Extent3D ───────────────────────────────────────────────

impl fmt::Display for Extent3D {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{{{}, {}, {}}}", self.width, self.height, self.depth)
    }
}

// ── Name helpers ───────────────────────────────────────────────────────

/// Human-readable name for an image.
///
/// Port of `VideoCommon::Name(const ImageBase&)`.
pub fn image_name(image: &ImageBase) -> String {
    let gpu_addr = image.gpu_addr;
    let info = &image.info;
    let mut width = info.size.width;
    let mut height = info.size.height;
    let depth = info.size.depth;
    let num_layers = info.resources.layers as u32;
    let num_levels = info.resources.levels as u32;
    let mut resource = String::new();
    if info.num_samples > 1 {
        let (sx, sy) = samples_log2(info.num_samples as i32);
        width >>= sx;
        height >>= sy;
        resource += &format!(":{}xMSAA", info.num_samples);
    }
    if num_layers > 1 {
        resource += &format!(":L{}", num_layers);
    }
    if num_levels > 1 {
        resource += &format!(":M{}", num_levels);
    }
    match info.image_type {
        ImageType::E1D => format!("Image 1D 0x{:x} {}{}", gpu_addr, width, resource),
        ImageType::E2D => {
            format!("Image 2D 0x{:x} {}x{}{}", gpu_addr, width, height, resource)
        }
        ImageType::E3D => format!(
            "Image 2D 0x{:x} {}x{}x{}{}",
            gpu_addr, width, height, depth, resource
        ),
        ImageType::Linear => format!("Image Linear 0x{:x} {}x{}", gpu_addr, width, height),
        ImageType::Buffer => format!("Buffer 0x{:x} {}", gpu_addr, width),
    }
}

/// Human-readable name for an image view.
///
/// Port of `VideoCommon::Name(const ImageViewBase&, GPUVAddr)`.
pub fn image_view_name(view: &ImageViewBase, addr: GPUVAddr) -> String {
    let w = view.size.width;
    let h = view.size.height;
    let d = view.size.depth;
    let levels = view.range.extent.levels as u32;
    let layers = view.range.extent.layers as u32;
    let level_str = if levels > 1 {
        format!(":{}", levels)
    } else {
        String::new()
    };
    match view.view_type {
        ImageViewType::E1D => format!("ImageView 1D 0x{:x} {}{}", addr, w, level_str),
        ImageViewType::E2D => {
            format!("ImageView 2D 0x{:x} {}x{}{}", addr, w, h, level_str)
        }
        ImageViewType::Cube => {
            format!("ImageView Cube 0x{:x} {}x{}{}", addr, w, h, level_str)
        }
        ImageViewType::E3D => {
            format!("ImageView 3D 0x{:x} {}x{}x{}{}", addr, w, h, d, level_str)
        }
        ImageViewType::E1DArray => {
            format!(
                "ImageView 1DArray 0x{:x} {}{}|{}",
                addr, w, level_str, layers
            )
        }
        ImageViewType::E2DArray => format!(
            "ImageView 2DArray 0x{:x} {}x{}{}|{}",
            addr, w, h, level_str, layers
        ),
        ImageViewType::CubeArray => format!(
            "ImageView CubeArray 0x{:x} {}x{}{}|{}",
            addr, w, h, level_str, layers
        ),
        ImageViewType::Rect => {
            format!("ImageView Rect 0x{:x} {}x{}{}", addr, w, h, level_str)
        }
        ImageViewType::Buffer => format!("BufferView 0x{:x} {}", addr, w),
    }
}

/// Human-readable name for a framebuffer (render target set).
///
/// Port of `VideoCommon::Name(const RenderTargets&)`.
pub fn render_targets_name(rt: &RenderTargets) -> String {
    let num_color = rt
        .color_buffer_ids
        .iter()
        .filter(|id| id.is_valid())
        .count();
    let has_depth = rt.depth_buffer_id.is_valid();
    let prefix = match (has_depth, num_color > 0) {
        (true, true) => "R",
        (true, false) => "Z",
        (false, true) => "C",
        (false, false) => "X",
    };
    let size = rt.size;
    if num_color > 0 {
        format!(
            "Framebuffer {}{} {}x{}",
            prefix, num_color, size.width, size.height
        )
    } else {
        format!("Framebuffer {} {}x{}", prefix, size.width, size.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::texture_cache::image_view_base::{ImageViewFlagBits, NullImageViewParams};

    #[test]
    fn pixel_format_names_cover_compressed_and_srgb_formats() {
        assert_eq!(PixelFormat::Astc2d4x4Unorm.to_string(), "ASTC_2D_4X4_UNORM");
        assert_eq!(PixelFormat::Bc1RgbaSrgb.to_string(), "BC1_RGBA_SRGB");
        assert_eq!(PixelFormat::Etc2RgbPtaSrgb.to_string(), "ETC2_RGB_PTA_SRGB");
        assert_eq!(PixelFormat::EacR11G11Snorm.to_string(), "EAC_R11G11_SNORM");
        assert_eq!(PixelFormat::MaxDepthStencilFormat.to_string(), "Invalid");
        assert_eq!(PixelFormat::Invalid.to_string(), "Invalid");
    }

    #[test]
    fn image_view_name_uses_lowercase_hexadecimal_addresses() {
        let mut view = ImageViewBase::null(NullImageViewParams);
        view.view_type = ImageViewType::E2DArray;
        view.range.extent = SubresourceExtent {
            levels: 3,
            layers: 4,
        };
        view.size = Extent3D {
            width: 128,
            height: 64,
            depth: 1,
        };
        view.flags = ImageViewFlagBits::empty();

        assert_eq!(
            image_view_name(&view, 0xABCD_EF12),
            "ImageView 2DArray 0xabcdef12 128x64:3|4"
        );
    }
}
