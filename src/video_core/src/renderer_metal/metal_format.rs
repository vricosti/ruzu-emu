// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Maxwell surface format to native Metal pixel format conversion.

use objc2_metal::{MTLPixelFormat, MTLTextureUsage};

use crate::surface::PixelFormat;

use super::metal_device::MetalDeviceProfile;

/// Native storage format plus whether guest bytes require conversion before
/// upload (and the inverse conversion on download).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetalFormat {
    pub pixel_format: MTLPixelFormat,
    pub requires_conversion: bool,
}

impl MetalFormat {
    const fn native(pixel_format: MTLPixelFormat) -> Self {
        Self {
            pixel_format,
            requires_conversion: false,
        }
    }

    const fn converted(pixel_format: MTLPixelFormat) -> Self {
        Self {
            pixel_format,
            requires_conversion: true,
        }
    }
}

/// Resolve a guest format without routing through Vulkan's format table.
///
/// Formats with no byte-compatible Metal representation use a native expanded
/// format and explicitly request conversion. Sentinel values return `None`.
pub fn surface_format(format: PixelFormat) -> Option<MetalFormat> {
    use PixelFormat::*;
    let resolved = match format {
        A8B8G8R8Unorm => MetalFormat::native(MTLPixelFormat::RGBA8Unorm),
        A8B8G8R8Snorm => MetalFormat::native(MTLPixelFormat::RGBA8Snorm),
        A8B8G8R8Sint => MetalFormat::native(MTLPixelFormat::RGBA8Sint),
        A8B8G8R8Uint => MetalFormat::native(MTLPixelFormat::RGBA8Uint),
        R5G6B5Unorm => MetalFormat::native(MTLPixelFormat::B5G6R5Unorm),
        B5G6R5Unorm => MetalFormat::converted(MTLPixelFormat::B5G6R5Unorm),
        A1R5G5B5Unorm => MetalFormat::native(MTLPixelFormat::A1BGR5Unorm),
        A2B10G10R10Unorm => MetalFormat::native(MTLPixelFormat::RGB10A2Unorm),
        A2B10G10R10Uint => MetalFormat::native(MTLPixelFormat::RGB10A2Uint),
        A2R10G10B10Unorm => MetalFormat::native(MTLPixelFormat::BGR10A2Unorm),
        A1B5G5R5Unorm => MetalFormat::converted(MTLPixelFormat::A1BGR5Unorm),
        A5B5G5R1Unorm => MetalFormat::converted(MTLPixelFormat::A1BGR5Unorm),
        R8Unorm => MetalFormat::native(MTLPixelFormat::R8Unorm),
        R8Snorm => MetalFormat::native(MTLPixelFormat::R8Snorm),
        R8Sint => MetalFormat::native(MTLPixelFormat::R8Sint),
        R8Uint => MetalFormat::native(MTLPixelFormat::R8Uint),
        R16G16B16A16Float | R16G16B16X16Float => MetalFormat::native(MTLPixelFormat::RGBA16Float),
        R16G16B16A16Unorm => MetalFormat::native(MTLPixelFormat::RGBA16Unorm),
        R16G16B16A16Snorm => MetalFormat::native(MTLPixelFormat::RGBA16Snorm),
        R16G16B16A16Sint => MetalFormat::native(MTLPixelFormat::RGBA16Sint),
        R16G16B16A16Uint => MetalFormat::native(MTLPixelFormat::RGBA16Uint),
        B10G11R11Float => MetalFormat::native(MTLPixelFormat::RG11B10Float),
        R32G32B32A32Uint => MetalFormat::native(MTLPixelFormat::RGBA32Uint),
        Bc1RgbaUnorm => MetalFormat::native(MTLPixelFormat::BC1_RGBA),
        Bc2Unorm => MetalFormat::native(MTLPixelFormat::BC2_RGBA),
        Bc3Unorm => MetalFormat::native(MTLPixelFormat::BC3_RGBA),
        Bc4Unorm => MetalFormat::native(MTLPixelFormat::BC4_RUnorm),
        Bc4Snorm => MetalFormat::native(MTLPixelFormat::BC4_RSnorm),
        Bc5Unorm => MetalFormat::native(MTLPixelFormat::BC5_RGUnorm),
        Bc5Snorm => MetalFormat::native(MTLPixelFormat::BC5_RGSnorm),
        Bc7Unorm => MetalFormat::native(MTLPixelFormat::BC7_RGBAUnorm),
        Bc6hUfloat => MetalFormat::native(MTLPixelFormat::BC6H_RGBUfloat),
        Bc6hSfloat => MetalFormat::native(MTLPixelFormat::BC6H_RGBFloat),
        Astc2d4x4Unorm => MetalFormat::native(MTLPixelFormat::ASTC_4x4_LDR),
        B8G8R8A8Unorm => MetalFormat::native(MTLPixelFormat::BGRA8Unorm),
        R32G32B32A32Float => MetalFormat::native(MTLPixelFormat::RGBA32Float),
        R32G32B32A32Sint => MetalFormat::native(MTLPixelFormat::RGBA32Sint),
        R32G32Float => MetalFormat::native(MTLPixelFormat::RG32Float),
        R32G32Sint => MetalFormat::native(MTLPixelFormat::RG32Sint),
        R32Float => MetalFormat::native(MTLPixelFormat::R32Float),
        R16Float => MetalFormat::native(MTLPixelFormat::R16Float),
        R16Unorm => MetalFormat::native(MTLPixelFormat::R16Unorm),
        R16Snorm => MetalFormat::native(MTLPixelFormat::R16Snorm),
        R16Uint => MetalFormat::native(MTLPixelFormat::R16Uint),
        R16Sint => MetalFormat::native(MTLPixelFormat::R16Sint),
        R16G16Unorm => MetalFormat::native(MTLPixelFormat::RG16Unorm),
        R16G16Float => MetalFormat::native(MTLPixelFormat::RG16Float),
        R16G16Uint => MetalFormat::native(MTLPixelFormat::RG16Uint),
        R16G16Sint => MetalFormat::native(MTLPixelFormat::RG16Sint),
        R16G16Snorm => MetalFormat::native(MTLPixelFormat::RG16Snorm),
        R32G32B32Float => MetalFormat::converted(MTLPixelFormat::RGBA32Float),
        A8B8G8R8Srgb => MetalFormat::native(MTLPixelFormat::RGBA8Unorm_sRGB),
        R8G8Unorm => MetalFormat::native(MTLPixelFormat::RG8Unorm),
        R8G8Snorm => MetalFormat::native(MTLPixelFormat::RG8Snorm),
        R8G8Sint => MetalFormat::native(MTLPixelFormat::RG8Sint),
        R8G8Uint => MetalFormat::native(MTLPixelFormat::RG8Uint),
        R32G32Uint => MetalFormat::native(MTLPixelFormat::RG32Uint),
        R32Uint => MetalFormat::native(MTLPixelFormat::R32Uint),
        R32Sint => MetalFormat::native(MTLPixelFormat::R32Sint),
        Astc2d8x8Unorm => MetalFormat::native(MTLPixelFormat::ASTC_8x8_LDR),
        Astc2d8x5Unorm => MetalFormat::native(MTLPixelFormat::ASTC_8x5_LDR),
        Astc2d5x4Unorm => MetalFormat::native(MTLPixelFormat::ASTC_5x4_LDR),
        B8G8R8A8Srgb => MetalFormat::native(MTLPixelFormat::BGRA8Unorm_sRGB),
        Bc1RgbaSrgb => MetalFormat::native(MTLPixelFormat::BC1_RGBA_sRGB),
        Bc2Srgb => MetalFormat::native(MTLPixelFormat::BC2_RGBA_sRGB),
        Bc3Srgb => MetalFormat::native(MTLPixelFormat::BC3_RGBA_sRGB),
        Bc7Srgb => MetalFormat::native(MTLPixelFormat::BC7_RGBAUnorm_sRGB),
        A4B4G4R4Unorm => MetalFormat::native(MTLPixelFormat::ABGR4Unorm),
        G4R4Unorm => MetalFormat::converted(MTLPixelFormat::RG8Unorm),
        Astc2d4x4Srgb => MetalFormat::native(MTLPixelFormat::ASTC_4x4_sRGB),
        Astc2d8x8Srgb => MetalFormat::native(MTLPixelFormat::ASTC_8x8_sRGB),
        Astc2d8x5Srgb => MetalFormat::native(MTLPixelFormat::ASTC_8x5_sRGB),
        Astc2d5x4Srgb => MetalFormat::native(MTLPixelFormat::ASTC_5x4_sRGB),
        Astc2d5x5Unorm => MetalFormat::native(MTLPixelFormat::ASTC_5x5_LDR),
        Astc2d5x5Srgb => MetalFormat::native(MTLPixelFormat::ASTC_5x5_sRGB),
        Astc2d10x8Unorm => MetalFormat::native(MTLPixelFormat::ASTC_10x8_LDR),
        Astc2d10x8Srgb => MetalFormat::native(MTLPixelFormat::ASTC_10x8_sRGB),
        Astc2d6x6Unorm => MetalFormat::native(MTLPixelFormat::ASTC_6x6_LDR),
        Astc2d6x6Srgb => MetalFormat::native(MTLPixelFormat::ASTC_6x6_sRGB),
        Astc2d10x6Unorm => MetalFormat::native(MTLPixelFormat::ASTC_10x6_LDR),
        Astc2d10x6Srgb => MetalFormat::native(MTLPixelFormat::ASTC_10x6_sRGB),
        Astc2d10x5Unorm => MetalFormat::native(MTLPixelFormat::ASTC_10x5_LDR),
        Astc2d10x5Srgb => MetalFormat::native(MTLPixelFormat::ASTC_10x5_sRGB),
        Astc2d10x10Unorm => MetalFormat::native(MTLPixelFormat::ASTC_10x10_LDR),
        Astc2d10x10Srgb => MetalFormat::native(MTLPixelFormat::ASTC_10x10_sRGB),
        Astc2d12x10Unorm => MetalFormat::native(MTLPixelFormat::ASTC_12x10_LDR),
        Astc2d12x10Srgb => MetalFormat::native(MTLPixelFormat::ASTC_12x10_sRGB),
        Astc2d12x12Unorm => MetalFormat::native(MTLPixelFormat::ASTC_12x12_LDR),
        Astc2d12x12Srgb => MetalFormat::native(MTLPixelFormat::ASTC_12x12_sRGB),
        Astc2d8x6Unorm => MetalFormat::native(MTLPixelFormat::ASTC_8x6_LDR),
        Astc2d8x6Srgb => MetalFormat::native(MTLPixelFormat::ASTC_8x6_sRGB),
        Astc2d6x5Unorm => MetalFormat::native(MTLPixelFormat::ASTC_6x5_LDR),
        Astc2d6x5Srgb => MetalFormat::native(MTLPixelFormat::ASTC_6x5_sRGB),
        E5B9G9R9Float => MetalFormat::native(MTLPixelFormat::RGB9E5Float),
        Etc2RgbUnorm => MetalFormat::native(MTLPixelFormat::ETC2_RGB8),
        Etc2RgbaUnorm => MetalFormat::native(MTLPixelFormat::EAC_RGBA8),
        Etc2RgbPtaUnorm => MetalFormat::native(MTLPixelFormat::ETC2_RGB8A1),
        Etc2RgbSrgb => MetalFormat::native(MTLPixelFormat::ETC2_RGB8_sRGB),
        Etc2RgbaSrgb => MetalFormat::native(MTLPixelFormat::EAC_RGBA8_sRGB),
        Etc2RgbPtaSrgb => MetalFormat::native(MTLPixelFormat::ETC2_RGB8A1_sRGB),
        EacR11Unorm => MetalFormat::native(MTLPixelFormat::EAC_R11Unorm),
        EacR11Snorm => MetalFormat::native(MTLPixelFormat::EAC_R11Snorm),
        EacR11G11Unorm => MetalFormat::native(MTLPixelFormat::EAC_RG11Unorm),
        EacR11G11Snorm => MetalFormat::native(MTLPixelFormat::EAC_RG11Snorm),
        D32Float => MetalFormat::native(MTLPixelFormat::Depth32Float),
        D16Unorm => MetalFormat::native(MTLPixelFormat::Depth16Unorm),
        X8D24Unorm => MetalFormat::converted(MTLPixelFormat::Depth32Float),
        S8Uint => MetalFormat::native(MTLPixelFormat::Stencil8),
        D24UnormS8Uint | S8UintD24Unorm => {
            MetalFormat::converted(MTLPixelFormat::Depth32Float_Stencil8)
        }
        D32FloatS8Uint => MetalFormat::native(MTLPixelFormat::Depth32Float_Stencil8),
        MaxDepthStencilFormat | Invalid => return None,
    };
    Some(resolved)
}

/// Conservative native usage declaration for a Maxwell image allocation.
///
/// Maxwell permits sampled, attachment and storage aliases of one allocation.
/// Metal requires those intended uses at allocation time. Compressed,
/// depth/stencil, sRGB and packed formats are never exposed as writable storage
/// textures; their storage-image paths require a converted helper image.
pub fn texture_usage(profile: &MetalDeviceProfile, format: PixelFormat) -> MTLTextureUsage {
    let mut usage = MTLTextureUsage::ShaderRead | MTLTextureUsage::PixelFormatView;
    let format_type = crate::surface::get_format_type(format);
    if !matches!(format_type, crate::surface::SurfaceType::Invalid)
        && !crate::surface::is_pixel_format_astc(format)
        && !crate::surface::is_pixel_format_bcn(format)
        && !crate::surface::is_pixel_format_etc2(format)
    {
        usage |= MTLTextureUsage::RenderTarget;
    }
    if matches!(format_type, crate::surface::SurfaceType::ColorTexture)
        && !crate::surface::is_pixel_format_srgb(format)
        && profile.supports_read_write_textures()
        && metal_storage_write_supported(format)
    {
        usage |= MTLTextureUsage::ShaderWrite;
    }
    usage
}

pub fn is_format_supported(profile: &MetalDeviceProfile, format: PixelFormat) -> bool {
    if crate::surface::is_pixel_format_bcn(format) {
        return profile.supports_bc_texture_compression;
    }
    if crate::surface::is_pixel_format_astc(format) {
        return profile.supports_astc_texture_compression;
    }
    if crate::surface::is_pixel_format_etc2(format) {
        return profile.supports_etc2_texture_compression;
    }
    surface_format(format).is_some()
}

fn metal_storage_write_supported(format: PixelFormat) -> bool {
    use PixelFormat::*;
    matches!(
        format,
        A8B8G8R8Unorm
            | A8B8G8R8Snorm
            | A8B8G8R8Sint
            | A8B8G8R8Uint
            | R8Unorm
            | R8Snorm
            | R8Sint
            | R8Uint
            | R16G16B16A16Float
            | R16G16B16A16Unorm
            | R16G16B16A16Snorm
            | R16G16B16A16Sint
            | R16G16B16A16Uint
            | R32G32B32A32Uint
            | B8G8R8A8Unorm
            | R32G32B32A32Float
            | R32G32B32A32Sint
            | R32G32Float
            | R32G32Sint
            | R32Float
            | R16Float
            | R16Unorm
            | R16Snorm
            | R16Uint
            | R16Sint
            | R16G16Unorm
            | R16G16Float
            | R16G16Uint
            | R16G16Sint
            | R16G16Snorm
            | R8G8Unorm
            | R8G8Snorm
            | R8G8Sint
            | R8G8Uint
            | R32G32Uint
            | R32Uint
            | R32Sint
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_common_color_and_depth_formats_byte_compatible() {
        assert_eq!(
            surface_format(PixelFormat::A8B8G8R8Unorm),
            Some(MetalFormat::native(MTLPixelFormat::RGBA8Unorm))
        );
        assert_eq!(
            surface_format(PixelFormat::B8G8R8A8Srgb),
            Some(MetalFormat::native(MTLPixelFormat::BGRA8Unorm_sRGB))
        );
        assert_eq!(
            surface_format(PixelFormat::D32Float),
            Some(MetalFormat::native(MTLPixelFormat::Depth32Float))
        );
    }

    #[test]
    fn marks_non_native_guest_layouts_for_conversion() {
        assert!(
            surface_format(PixelFormat::R32G32B32Float)
                .unwrap()
                .requires_conversion
        );
        assert!(
            surface_format(PixelFormat::D24UnormS8Uint)
                .unwrap()
                .requires_conversion
        );
        assert_eq!(surface_format(PixelFormat::Invalid), None);
    }

    #[test]
    fn usage_declares_views_without_claiming_compressed_storage() {
        let device = super::super::metal_device::MetalDevice::new().unwrap();
        let color = texture_usage(device.profile(), PixelFormat::A8B8G8R8Unorm);
        assert!(color.contains(MTLTextureUsage::PixelFormatView));
        assert!(color.contains(MTLTextureUsage::RenderTarget));
        assert!(color.contains(MTLTextureUsage::ShaderWrite));

        let compressed = texture_usage(device.profile(), PixelFormat::Bc3Unorm);
        assert!(compressed.contains(MTLTextureUsage::ShaderRead));
        assert!(!compressed.contains(MTLTextureUsage::ShaderWrite));
        assert!(!compressed.contains(MTLTextureUsage::RenderTarget));
    }
}
