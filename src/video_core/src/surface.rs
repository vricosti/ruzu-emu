// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `video_core/surface.h` and `video_core/surface.cpp`.
//!
//! Pixel format utilities, surface target helpers, block dimension tables,
//! and format classification functions.
//!
//! As in upstream `surface.h`, this module owns the canonical `PixelFormat`
//! enum together with its indexed property tables and utility functions.

// Upstream defines this enum in `video_core/surface.h`; keep it here so the
// ordering that every `PixelFormat`-indexed table depends on lives with those
// tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u32)]
#[allow(non_camel_case_types)]
pub enum PixelFormat {
    A8B8G8R8Unorm = 0,
    A8B8G8R8Snorm,
    A8B8G8R8Sint,
    A8B8G8R8Uint,
    R5G6B5Unorm,
    B5G6R5Unorm,
    A1R5G5B5Unorm,
    A2B10G10R10Unorm,
    A2B10G10R10Uint,
    A2R10G10B10Unorm,
    A1B5G5R5Unorm,
    A5B5G5R1Unorm,
    R8Unorm,
    R8Snorm,
    R8Sint,
    R8Uint,
    R16G16B16A16Float,
    R16G16B16A16Unorm,
    R16G16B16A16Snorm,
    R16G16B16A16Sint,
    R16G16B16A16Uint,
    B10G11R11Float,
    R32G32B32A32Uint,
    Bc1RgbaUnorm,
    Bc2Unorm,
    Bc3Unorm,
    Bc4Unorm,
    Bc4Snorm,
    Bc5Unorm,
    Bc5Snorm,
    Bc7Unorm,
    Bc6hUfloat,
    Bc6hSfloat,
    Astc2d4x4Unorm,
    B8G8R8A8Unorm,
    R32G32B32A32Float,
    R32G32B32A32Sint,
    R32G32Float,
    R32G32Sint,
    R32Float,
    R16Float,
    R16Unorm,
    R16Snorm,
    R16Uint,
    R16Sint,
    R16G16Unorm,
    R16G16Float,
    R16G16Uint,
    R16G16Sint,
    R16G16Snorm,
    R32G32B32Float,
    A8B8G8R8Srgb,
    R8G8Unorm,
    R8G8Snorm,
    R8G8Sint,
    R8G8Uint,
    R32G32Uint,
    R16G16B16X16Float,
    R32Uint,
    R32Sint,
    Astc2d8x8Unorm,
    Astc2d8x5Unorm,
    Astc2d5x4Unorm,
    B8G8R8A8Srgb,
    Bc1RgbaSrgb,
    Bc2Srgb,
    Bc3Srgb,
    Bc7Srgb,
    A4B4G4R4Unorm,
    G4R4Unorm,
    Astc2d4x4Srgb,
    Astc2d8x8Srgb,
    Astc2d8x5Srgb,
    Astc2d5x4Srgb,
    Astc2d5x5Unorm,
    Astc2d5x5Srgb,
    Astc2d10x8Unorm,
    Astc2d10x8Srgb,
    Astc2d6x6Unorm,
    Astc2d6x6Srgb,
    Astc2d10x6Unorm,
    Astc2d10x6Srgb,
    Astc2d10x5Unorm,
    Astc2d10x5Srgb,
    Astc2d10x10Unorm,
    Astc2d10x10Srgb,
    Astc2d12x10Unorm,
    Astc2d12x10Srgb,
    Astc2d12x12Unorm,
    Astc2d12x12Srgb,
    Astc2d8x6Unorm,
    Astc2d8x6Srgb,
    Astc2d6x5Unorm,
    Astc2d6x5Srgb,
    E5B9G9R9Float,

    // ETC2 / EAC formats. Upstream orders these between `E5B9G9R9_FLOAT` and
    // `D32_FLOAT` in `PIXEL_FORMAT_LIST`; keep that position so every
    // `PixelFormat`-indexed table stays aligned with upstream.
    Etc2RgbUnorm,
    Etc2RgbaUnorm,
    Etc2RgbPtaUnorm,
    Etc2RgbSrgb,
    Etc2RgbaSrgb,
    Etc2RgbPtaSrgb,
    EacR11Unorm,
    EacR11Snorm,
    EacR11G11Unorm,
    EacR11G11Snorm,

    // Depth formats
    D32Float,
    D16Unorm,
    X8D24Unorm,
    S8Uint,
    D24UnormS8Uint,
    S8UintD24Unorm,
    D32FloatS8Uint,

    MaxDepthStencilFormat,

    #[default]
    Invalid = 255,
}

// ---------------------------------------------------------------------------
// SurfaceType
// ---------------------------------------------------------------------------

/// Surface type classification.
///
/// Port of `VideoCore::Surface::SurfaceType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum SurfaceType {
    ColorTexture = 0,
    Depth = 1,
    Stencil = 2,
    DepthStencil = 3,
    Invalid = 4,
}

// ---------------------------------------------------------------------------
// SurfaceTarget
// ---------------------------------------------------------------------------

/// Surface target (texture dimensionality).
///
/// Port of `VideoCore::Surface::SurfaceTarget`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum SurfaceTarget {
    Texture1D,
    TextureBuffer,
    Texture2D,
    Texture3D,
    Texture1DArray,
    Texture2DArray,
    TextureCubemap,
    TextureCubeArray,
}

// ---------------------------------------------------------------------------
// PixelFormat boundary constants
// ---------------------------------------------------------------------------

// These constants mark the boundaries between format categories in the
// PixelFormat enum. They match the upstream sentinel values.

/// Number of color formats (up to and not including depth formats).
///
/// Port of `PixelFormat::MaxColorFormat`.
pub const MAX_COLOR_FORMAT: u32 = PixelFormat::D32Float as u32;

/// Number of color + depth formats.
///
/// Port of `PixelFormat::MaxDepthFormat`.
pub const MAX_DEPTH_FORMAT: u32 = PixelFormat::S8Uint as u32;

/// Number of color + depth + stencil formats.
///
/// Port of `PixelFormat::MaxStencilFormat`.
pub const MAX_STENCIL_FORMAT: u32 = PixelFormat::D24UnormS8Uint as u32;

/// Number of all formats (color + depth + stencil + depth-stencil).
///
/// Port of `PixelFormat::MaxDepthStencilFormat`.
pub const MAX_DEPTH_STENCIL_FORMAT: u32 = PixelFormat::MaxDepthStencilFormat as u32;

/// Total number of pixel formats (for table sizing).
pub const MAX_PIXEL_FORMAT: usize = MAX_DEPTH_STENCIL_FORMAT as usize;

// ---------------------------------------------------------------------------
// Render-target format conversion
// ---------------------------------------------------------------------------

fn unimplemented_surface_format(kind: &str, format: u32, fallback: PixelFormat) -> PixelFormat {
    // Eden's `UNIMPLEMENTED_MSG` is an always-on, fail-soft assertion. It logs
    // and returns the switch's fallback unless the user enables debug asserts.
    log::error!("Surface::{kind} unimplemented format=0x{format:X}");
    if *common::settings::values().use_debug_asserts.get_value() {
        panic!("Surface::{kind} unimplemented format=0x{format:X}");
    }
    fallback
}

/// Port of `PixelFormatFromRenderTargetFormat` from `surface.cpp`.
pub fn pixel_format_from_render_target_format(format: u32) -> PixelFormat {
    match format {
        0xC0 | 0xC3 => PixelFormat::R32G32B32A32Float,
        0xC1 | 0xC4 => PixelFormat::R32G32B32A32Sint,
        0xC2 | 0xC5 => PixelFormat::R32G32B32A32Uint,
        0xC6 => PixelFormat::R16G16B16A16Unorm,
        0xC7 => PixelFormat::R16G16B16A16Snorm,
        0xC8 => PixelFormat::R16G16B16A16Sint,
        0xC9 => PixelFormat::R16G16B16A16Uint,
        0xCA => PixelFormat::R16G16B16A16Float,
        0xCB => PixelFormat::R32G32Float,
        0xCC => PixelFormat::R32G32Sint,
        0xCD => PixelFormat::R32G32Uint,
        0xCE => PixelFormat::R16G16B16X16Float,
        0xCF | 0xE6 => PixelFormat::B8G8R8A8Unorm,
        0xD0 | 0xE7 => PixelFormat::B8G8R8A8Srgb,
        0xD1 => PixelFormat::A2B10G10R10Unorm,
        0xD2 => PixelFormat::A2B10G10R10Uint,
        0xDF => PixelFormat::A2R10G10B10Unorm,
        0xD5 | 0xF9 => PixelFormat::A8B8G8R8Unorm,
        0xD6 | 0xFA => PixelFormat::A8B8G8R8Srgb,
        0xD7 => PixelFormat::A8B8G8R8Snorm,
        0xD8 => PixelFormat::A8B8G8R8Sint,
        0xD9 => PixelFormat::A8B8G8R8Uint,
        0xDA => PixelFormat::R16G16Unorm,
        0xDB => PixelFormat::R16G16Snorm,
        0xDC => PixelFormat::R16G16Sint,
        0xDD => PixelFormat::R16G16Uint,
        0xDE => PixelFormat::R16G16Float,
        0xE0 => PixelFormat::B10G11R11Float,
        0xE3 => PixelFormat::R32Sint,
        0xE4 => PixelFormat::R32Uint,
        0xE5 => PixelFormat::R32Float,
        0xE8 => PixelFormat::R5G6B5Unorm,
        0xE9 | 0xF8 => PixelFormat::A1R5G5B5Unorm,
        0xEA => PixelFormat::R8G8Unorm,
        0xEB => PixelFormat::R8G8Snorm,
        0xEC => PixelFormat::R8G8Sint,
        0xED => PixelFormat::R8G8Uint,
        0xEE => PixelFormat::R16Unorm,
        0xEF => PixelFormat::R16Snorm,
        0xF0 => PixelFormat::R16Sint,
        0xF1 => PixelFormat::R16Uint,
        0xF2 => PixelFormat::R16Float,
        0xF3 => PixelFormat::R8Unorm,
        0xF4 => PixelFormat::R8Snorm,
        0xF5 => PixelFormat::R8Sint,
        0xF6 => PixelFormat::R8Uint,
        _ => unimplemented_surface_format(
            "PixelFormatFromRenderTargetFormat",
            format,
            PixelFormat::A8B8G8R8Unorm,
        ),
    }
}

/// Port of `PixelFormatFromDepthFormat` from `surface.cpp`.
pub fn pixel_format_from_depth_format(format: u32) -> PixelFormat {
    match format {
        0x14 => PixelFormat::S8UintD24Unorm,
        0x16 => PixelFormat::D24UnormS8Uint,
        0x0A => PixelFormat::D32Float,
        0x13 => PixelFormat::D16Unorm,
        0x17 => PixelFormat::S8Uint,
        0x19 => PixelFormat::D32FloatS8Uint,
        0x15 => PixelFormat::X8D24Unorm,
        _ => unimplemented_surface_format(
            "PixelFormatFromDepthFormat",
            format,
            PixelFormat::S8UintD24Unorm,
        ),
    }
}

/// Port of `PixelFormatFromGPUPixelFormat` from `surface.cpp`.
pub fn pixel_format_from_gpu_pixel_format(format: u32) -> PixelFormat {
    match format {
        1 | 2 => PixelFormat::A8B8G8R8Unorm,
        4 => PixelFormat::R5G6B5Unorm,
        5 => PixelFormat::B8G8R8A8Unorm,
        _ => unimplemented_surface_format(
            "PixelFormatFromGPUPixelFormat",
            format,
            PixelFormat::A8B8G8R8Unorm,
        ),
    }
}

// ---------------------------------------------------------------------------
// Block width table
// ---------------------------------------------------------------------------

/// Default block width for each pixel format.
///
/// Port of `BLOCK_WIDTH_TABLE` from `surface.h`.
pub const BLOCK_WIDTH_TABLE: [u8; MAX_PIXEL_FORMAT] = [
    1,  // A8B8G8R8_UNORM
    1,  // A8B8G8R8_SNORM
    1,  // A8B8G8R8_SINT
    1,  // A8B8G8R8_UINT
    1,  // R5G6B5_UNORM
    1,  // B5G6R5_UNORM
    1,  // A1R5G5B5_UNORM
    1,  // A2B10G10R10_UNORM
    1,  // A2B10G10R10_UINT
    1,  // A2R10G10B10_UNORM
    1,  // A1B5G5R5_UNORM
    1,  // A5B5G5R1_UNORM
    1,  // R8_UNORM
    1,  // R8_SNORM
    1,  // R8_SINT
    1,  // R8_UINT
    1,  // R16G16B16A16_FLOAT
    1,  // R16G16B16A16_UNORM
    1,  // R16G16B16A16_SNORM
    1,  // R16G16B16A16_SINT
    1,  // R16G16B16A16_UINT
    1,  // B10G11R11_FLOAT
    1,  // R32G32B32A32_UINT
    4,  // BC1_RGBA_UNORM
    4,  // BC2_UNORM
    4,  // BC3_UNORM
    4,  // BC4_UNORM
    4,  // BC4_SNORM
    4,  // BC5_UNORM
    4,  // BC5_SNORM
    4,  // BC7_UNORM
    4,  // BC6H_UFLOAT
    4,  // BC6H_SFLOAT
    4,  // ASTC_2D_4X4_UNORM
    1,  // B8G8R8A8_UNORM
    1,  // R32G32B32A32_FLOAT
    1,  // R32G32B32A32_SINT
    1,  // R32G32_FLOAT
    1,  // R32G32_SINT
    1,  // R32_FLOAT
    1,  // R16_FLOAT
    1,  // R16_UNORM
    1,  // R16_SNORM
    1,  // R16_UINT
    1,  // R16_SINT
    1,  // R16G16_UNORM
    1,  // R16G16_FLOAT
    1,  // R16G16_UINT
    1,  // R16G16_SINT
    1,  // R16G16_SNORM
    1,  // R32G32B32_FLOAT
    1,  // A8B8G8R8_SRGB
    1,  // R8G8_UNORM
    1,  // R8G8_SNORM
    1,  // R8G8_SINT
    1,  // R8G8_UINT
    1,  // R32G32_UINT
    1,  // R16G16B16X16_FLOAT
    1,  // R32_UINT
    1,  // R32_SINT
    8,  // ASTC_2D_8X8_UNORM
    8,  // ASTC_2D_8X5_UNORM
    5,  // ASTC_2D_5X4_UNORM
    1,  // B8G8R8A8_SRGB
    4,  // BC1_RGBA_SRGB
    4,  // BC2_SRGB
    4,  // BC3_SRGB
    4,  // BC7_SRGB
    1,  // A4B4G4R4_UNORM
    1,  // G4R4_UNORM
    4,  // ASTC_2D_4X4_SRGB
    8,  // ASTC_2D_8X8_SRGB
    8,  // ASTC_2D_8X5_SRGB
    5,  // ASTC_2D_5X4_SRGB
    5,  // ASTC_2D_5X5_UNORM
    5,  // ASTC_2D_5X5_SRGB
    10, // ASTC_2D_10X8_UNORM
    10, // ASTC_2D_10X8_SRGB
    6,  // ASTC_2D_6X6_UNORM
    6,  // ASTC_2D_6X6_SRGB
    10, // ASTC_2D_10X6_UNORM
    10, // ASTC_2D_10X6_SRGB
    10, // ASTC_2D_10X5_UNORM
    10, // ASTC_2D_10X5_SRGB
    10, // ASTC_2D_10X10_UNORM
    10, // ASTC_2D_10X10_SRGB
    12, // ASTC_2D_12X10_UNORM
    12, // ASTC_2D_12X10_SRGB
    12, // ASTC_2D_12X12_UNORM
    12, // ASTC_2D_12X12_SRGB
    8,  // ASTC_2D_8X6_UNORM
    8,  // ASTC_2D_8X6_SRGB
    6,  // ASTC_2D_6X5_UNORM
    6,  // ASTC_2D_6X5_SRGB
    1,  // E5B9G9R9_FLOAT
    4,  // ETC2_RGB_UNORM
    4,  // ETC2_RGBA_UNORM
    4,  // ETC2_RGB_PTA_UNORM
    4,  // ETC2_RGB_SRGB
    4,  // ETC2_RGBA_SRGB
    4,  // ETC2_RGB_PTA_SRGB
    4,  // EAC_R11_UNORM
    4,  // EAC_R11_SNORM
    4,  // EAC_R11G11_UNORM
    4,  // EAC_R11G11_SNORM
    1,  // D32_FLOAT
    1,  // D16_UNORM
    1,  // X8_D24_UNORM
    1,  // S8_UINT
    1,  // D24_UNORM_S8_UINT
    1,  // S8_UINT_D24_UNORM
    1,  // D32_FLOAT_S8_UINT
];

/// Returns the default block width for a pixel format.
///
/// Port of `DefaultBlockWidth` from `surface.h`.
pub fn default_block_width(format: PixelFormat) -> u32 {
    let idx = format as usize;
    assert!(idx < BLOCK_WIDTH_TABLE.len());
    BLOCK_WIDTH_TABLE[idx] as u32
}

// ---------------------------------------------------------------------------
// Block height table
// ---------------------------------------------------------------------------

/// Default block height for each pixel format.
///
/// Port of `BLOCK_HEIGHT_TABLE` from `surface.h`.
pub const BLOCK_HEIGHT_TABLE: [u8; MAX_PIXEL_FORMAT] = [
    1,  // A8B8G8R8_UNORM
    1,  // A8B8G8R8_SNORM
    1,  // A8B8G8R8_SINT
    1,  // A8B8G8R8_UINT
    1,  // R5G6B5_UNORM
    1,  // B5G6R5_UNORM
    1,  // A1R5G5B5_UNORM
    1,  // A2B10G10R10_UNORM
    1,  // A2B10G10R10_UINT
    1,  // A2R10G10B10_UNORM
    1,  // A1B5G5R5_UNORM
    1,  // A5B5G5R1_UNORM
    1,  // R8_UNORM
    1,  // R8_SNORM
    1,  // R8_SINT
    1,  // R8_UINT
    1,  // R16G16B16A16_FLOAT
    1,  // R16G16B16A16_UNORM
    1,  // R16G16B16A16_SNORM
    1,  // R16G16B16A16_SINT
    1,  // R16G16B16A16_UINT
    1,  // B10G11R11_FLOAT
    1,  // R32G32B32A32_UINT
    4,  // BC1_RGBA_UNORM
    4,  // BC2_UNORM
    4,  // BC3_UNORM
    4,  // BC4_UNORM
    4,  // BC4_SNORM
    4,  // BC5_UNORM
    4,  // BC5_SNORM
    4,  // BC7_UNORM
    4,  // BC6H_UFLOAT
    4,  // BC6H_SFLOAT
    4,  // ASTC_2D_4X4_UNORM
    1,  // B8G8R8A8_UNORM
    1,  // R32G32B32A32_FLOAT
    1,  // R32G32B32A32_SINT
    1,  // R32G32_FLOAT
    1,  // R32G32_SINT
    1,  // R32_FLOAT
    1,  // R16_FLOAT
    1,  // R16_UNORM
    1,  // R16_SNORM
    1,  // R16_UINT
    1,  // R16_SINT
    1,  // R16G16_UNORM
    1,  // R16G16_FLOAT
    1,  // R16G16_UINT
    1,  // R16G16_SINT
    1,  // R16G16_SNORM
    1,  // R32G32B32_FLOAT
    1,  // A8B8G8R8_SRGB
    1,  // R8G8_UNORM
    1,  // R8G8_SNORM
    1,  // R8G8_SINT
    1,  // R8G8_UINT
    1,  // R32G32_UINT
    1,  // R16G16B16X16_FLOAT
    1,  // R32_UINT
    1,  // R32_SINT
    8,  // ASTC_2D_8X8_UNORM
    5,  // ASTC_2D_8X5_UNORM
    4,  // ASTC_2D_5X4_UNORM
    1,  // B8G8R8A8_SRGB
    4,  // BC1_RGBA_SRGB
    4,  // BC2_SRGB
    4,  // BC3_SRGB
    4,  // BC7_SRGB
    1,  // A4B4G4R4_UNORM
    1,  // G4R4_UNORM
    4,  // ASTC_2D_4X4_SRGB
    8,  // ASTC_2D_8X8_SRGB
    5,  // ASTC_2D_8X5_SRGB
    4,  // ASTC_2D_5X4_SRGB
    5,  // ASTC_2D_5X5_UNORM
    5,  // ASTC_2D_5X5_SRGB
    8,  // ASTC_2D_10X8_UNORM
    8,  // ASTC_2D_10X8_SRGB
    6,  // ASTC_2D_6X6_UNORM
    6,  // ASTC_2D_6X6_SRGB
    6,  // ASTC_2D_10X6_UNORM
    6,  // ASTC_2D_10X6_SRGB
    5,  // ASTC_2D_10X5_UNORM
    5,  // ASTC_2D_10X5_SRGB
    10, // ASTC_2D_10X10_UNORM
    10, // ASTC_2D_10X10_SRGB
    10, // ASTC_2D_12X10_UNORM
    10, // ASTC_2D_12X10_SRGB
    12, // ASTC_2D_12X12_UNORM
    12, // ASTC_2D_12X12_SRGB
    6,  // ASTC_2D_8X6_UNORM
    6,  // ASTC_2D_8X6_SRGB
    5,  // ASTC_2D_6X5_UNORM
    5,  // ASTC_2D_6X5_SRGB
    1,  // E5B9G9R9_FLOAT
    4,  // ETC2_RGB_UNORM
    4,  // ETC2_RGBA_UNORM
    4,  // ETC2_RGB_PTA_UNORM
    4,  // ETC2_RGB_SRGB
    4,  // ETC2_RGBA_SRGB
    4,  // ETC2_RGB_PTA_SRGB
    4,  // EAC_R11_UNORM
    4,  // EAC_R11_SNORM
    4,  // EAC_R11G11_UNORM
    4,  // EAC_R11G11_SNORM
    1,  // D32_FLOAT
    1,  // D16_UNORM
    1,  // X8_D24_UNORM
    1,  // S8_UINT
    1,  // D24_UNORM_S8_UINT
    1,  // S8_UINT_D24_UNORM
    1,  // D32_FLOAT_S8_UINT
];

/// Returns the default block height for a pixel format.
///
/// Port of `DefaultBlockHeight` from `surface.h`.
pub fn default_block_height(format: PixelFormat) -> u32 {
    let idx = format as usize;
    assert!(idx < BLOCK_HEIGHT_TABLE.len());
    BLOCK_HEIGHT_TABLE[idx] as u32
}

// ---------------------------------------------------------------------------
// Bits per block table
// ---------------------------------------------------------------------------

/// Bits per block for each pixel format.
///
/// Port of `BITS_PER_BLOCK_TABLE` from `surface.h`.
pub const BITS_PER_BLOCK_TABLE: [u16; MAX_PIXEL_FORMAT] = [
    32,  // A8B8G8R8_UNORM
    32,  // A8B8G8R8_SNORM
    32,  // A8B8G8R8_SINT
    32,  // A8B8G8R8_UINT
    16,  // R5G6B5_UNORM
    16,  // B5G6R5_UNORM
    16,  // A1R5G5B5_UNORM
    32,  // A2B10G10R10_UNORM
    32,  // A2B10G10R10_UINT
    32,  // A2R10G10B10_UNORM
    16,  // A1B5G5R5_UNORM
    16,  // A5B5G5R1_UNORM
    8,   // R8_UNORM
    8,   // R8_SNORM
    8,   // R8_SINT
    8,   // R8_UINT
    64,  // R16G16B16A16_FLOAT
    64,  // R16G16B16A16_UNORM
    64,  // R16G16B16A16_SNORM
    64,  // R16G16B16A16_SINT
    64,  // R16G16B16A16_UINT
    32,  // B10G11R11_FLOAT
    128, // R32G32B32A32_UINT
    64,  // BC1_RGBA_UNORM
    128, // BC2_UNORM
    128, // BC3_UNORM
    64,  // BC4_UNORM
    64,  // BC4_SNORM
    128, // BC5_UNORM
    128, // BC5_SNORM
    128, // BC7_UNORM
    128, // BC6H_UFLOAT
    128, // BC6H_SFLOAT
    128, // ASTC_2D_4X4_UNORM
    32,  // B8G8R8A8_UNORM
    128, // R32G32B32A32_FLOAT
    128, // R32G32B32A32_SINT
    64,  // R32G32_FLOAT
    64,  // R32G32_SINT
    32,  // R32_FLOAT
    16,  // R16_FLOAT
    16,  // R16_UNORM
    16,  // R16_SNORM
    16,  // R16_UINT
    16,  // R16_SINT
    32,  // R16G16_UNORM
    32,  // R16G16_FLOAT
    32,  // R16G16_UINT
    32,  // R16G16_SINT
    32,  // R16G16_SNORM
    96,  // R32G32B32_FLOAT
    32,  // A8B8G8R8_SRGB
    16,  // R8G8_UNORM
    16,  // R8G8_SNORM
    16,  // R8G8_SINT
    16,  // R8G8_UINT
    64,  // R32G32_UINT
    64,  // R16G16B16X16_FLOAT
    32,  // R32_UINT
    32,  // R32_SINT
    128, // ASTC_2D_8X8_UNORM
    128, // ASTC_2D_8X5_UNORM
    128, // ASTC_2D_5X4_UNORM
    32,  // B8G8R8A8_SRGB
    64,  // BC1_RGBA_SRGB
    128, // BC2_SRGB
    128, // BC3_SRGB
    128, // BC7_SRGB
    16,  // A4B4G4R4_UNORM
    8,   // G4R4_UNORM
    128, // ASTC_2D_4X4_SRGB
    128, // ASTC_2D_8X8_SRGB
    128, // ASTC_2D_8X5_SRGB
    128, // ASTC_2D_5X4_SRGB
    128, // ASTC_2D_5X5_UNORM
    128, // ASTC_2D_5X5_SRGB
    128, // ASTC_2D_10X8_UNORM
    128, // ASTC_2D_10X8_SRGB
    128, // ASTC_2D_6X6_UNORM
    128, // ASTC_2D_6X6_SRGB
    128, // ASTC_2D_10X6_UNORM
    128, // ASTC_2D_10X6_SRGB
    128, // ASTC_2D_10X5_UNORM
    128, // ASTC_2D_10X5_SRGB
    128, // ASTC_2D_10X10_UNORM
    128, // ASTC_2D_10X10_SRGB
    128, // ASTC_2D_12X10_UNORM
    128, // ASTC_2D_12X10_SRGB
    128, // ASTC_2D_12X12_UNORM
    128, // ASTC_2D_12X12_SRGB
    128, // ASTC_2D_8X6_UNORM
    128, // ASTC_2D_8X6_SRGB
    128, // ASTC_2D_6X5_UNORM
    128, // ASTC_2D_6X5_SRGB
    32,  // E5B9G9R9_FLOAT
    64,  // ETC2_RGB_UNORM
    128, // ETC2_RGBA_UNORM
    64,  // ETC2_RGB_PTA_UNORM
    64,  // ETC2_RGB_SRGB
    128, // ETC2_RGBA_SRGB
    64,  // ETC2_RGB_PTA_SRGB
    64,  // EAC_R11_UNORM
    64,  // EAC_R11_SNORM
    128, // EAC_R11G11_UNORM
    128, // EAC_R11G11_SNORM
    32,  // D32_FLOAT
    16,  // D16_UNORM
    32,  // X8_D24_UNORM
    8,   // S8_UINT
    32,  // D24_UNORM_S8_UINT
    32,  // S8_UINT_D24_UNORM
    64,  // D32_FLOAT_S8_UINT
];

/// Returns bits per block for a pixel format.
///
/// Port of `BitsPerBlock` from `surface.h`.
pub fn bits_per_block(format: PixelFormat) -> u32 {
    let idx = format as usize;
    assert!(idx < BITS_PER_BLOCK_TABLE.len());
    BITS_PER_BLOCK_TABLE[idx] as u32
}

/// Returns bytes per block for a pixel format.
///
/// Port of `BytesPerBlock` from `surface.h`.
pub fn bytes_per_block(format: PixelFormat) -> u32 {
    bits_per_block(format) / 8
}

// ---------------------------------------------------------------------------
// SurfaceTarget helpers
// ---------------------------------------------------------------------------

/// Port of `SurfaceTargetFromTextureType` from `surface.cpp`.
pub fn surface_target_from_texture_type(
    texture_type: crate::textures::texture::TextureType,
) -> SurfaceTarget {
    use crate::textures::texture::TextureType;

    match texture_type {
        TextureType::Texture1D => SurfaceTarget::Texture1D,
        TextureType::Texture1DBuffer => SurfaceTarget::TextureBuffer,
        TextureType::Texture2D | TextureType::Texture2DNoMipmap => SurfaceTarget::Texture2D,
        TextureType::Texture3D => SurfaceTarget::Texture3D,
        TextureType::TextureCubemap => SurfaceTarget::TextureCubemap,
        TextureType::TextureCubeArray => SurfaceTarget::TextureCubeArray,
        TextureType::Texture1DArray => SurfaceTarget::Texture1DArray,
        TextureType::Texture2DArray => SurfaceTarget::Texture2DArray,
    }
}

/// Returns whether a surface target is layered (array or cubemap).
///
/// Port of `SurfaceTargetIsLayered` from `surface.cpp`.
pub fn surface_target_is_layered(target: SurfaceTarget) -> bool {
    matches!(
        target,
        SurfaceTarget::Texture1DArray
            | SurfaceTarget::Texture2DArray
            | SurfaceTarget::TextureCubemap
            | SurfaceTarget::TextureCubeArray
    )
}

/// Returns whether a surface target is an array type.
///
/// Port of `SurfaceTargetIsArray` from `surface.cpp`.
pub fn surface_target_is_array(target: SurfaceTarget) -> bool {
    matches!(
        target,
        SurfaceTarget::Texture1DArray
            | SurfaceTarget::Texture2DArray
            | SurfaceTarget::TextureCubeArray
    )
}

// ---------------------------------------------------------------------------
// Format type classification
// ---------------------------------------------------------------------------

/// Returns the surface type (color, depth, stencil, depth-stencil) for a format.
///
/// Port of `GetFormatType` from `surface.cpp`.
pub fn get_format_type(pixel_format: PixelFormat) -> SurfaceType {
    let idx = pixel_format as u32;
    if idx < MAX_COLOR_FORMAT {
        return SurfaceType::ColorTexture;
    }
    if idx < MAX_DEPTH_FORMAT {
        return SurfaceType::Depth;
    }
    if idx < MAX_STENCIL_FORMAT {
        return SurfaceType::Stencil;
    }
    if idx < MAX_DEPTH_STENCIL_FORMAT {
        return SurfaceType::DepthStencil;
    }

    // Upstream ASSERT is fail-soft unless debug asserts are enabled.
    log::error!("surface.cpp: assert false for pixel_format={pixel_format:?}");
    if *common::settings::values().use_debug_asserts.get_value() {
        panic!("assertion failed: unsupported pixel format {pixel_format:?}");
    }
    SurfaceType::Invalid
}

// ---------------------------------------------------------------------------
// Format classification functions
// ---------------------------------------------------------------------------

/// Returns whether the format has an alpha component.
///
/// Port of `HasAlpha` from `surface.cpp`.
pub fn has_alpha(pixel_format: PixelFormat) -> bool {
    matches!(
        pixel_format,
        PixelFormat::A8B8G8R8Unorm
            | PixelFormat::A8B8G8R8Snorm
            | PixelFormat::A8B8G8R8Sint
            | PixelFormat::A8B8G8R8Uint
            | PixelFormat::A1R5G5B5Unorm
            | PixelFormat::A2B10G10R10Unorm
            | PixelFormat::A2B10G10R10Uint
            | PixelFormat::A2R10G10B10Unorm
            | PixelFormat::A1B5G5R5Unorm
            | PixelFormat::A5B5G5R1Unorm
            | PixelFormat::R16G16B16A16Float
            | PixelFormat::R16G16B16A16Unorm
            | PixelFormat::R16G16B16A16Snorm
            | PixelFormat::R16G16B16A16Sint
            | PixelFormat::R16G16B16A16Uint
            | PixelFormat::R32G32B32A32Uint
            | PixelFormat::Bc1RgbaUnorm
            | PixelFormat::B8G8R8A8Unorm
            | PixelFormat::R32G32B32A32Float
            | PixelFormat::R32G32B32A32Sint
            | PixelFormat::A8B8G8R8Srgb
            | PixelFormat::B8G8R8A8Srgb
            | PixelFormat::Bc1RgbaSrgb
            | PixelFormat::A4B4G4R4Unorm
            | PixelFormat::Bc2Srgb
            | PixelFormat::Bc2Unorm
            | PixelFormat::Bc3Srgb
            | PixelFormat::Bc3Unorm
            | PixelFormat::Bc7Srgb
            | PixelFormat::Bc7Unorm
    )
}

/// Returns true if the format is an ASTC compressed format.
///
/// Port of `IsPixelFormatASTC` from `surface.cpp`.
pub fn is_pixel_format_astc(format: PixelFormat) -> bool {
    matches!(
        format,
        PixelFormat::Astc2d4x4Unorm
            | PixelFormat::Astc2d5x4Unorm
            | PixelFormat::Astc2d5x5Unorm
            | PixelFormat::Astc2d8x8Unorm
            | PixelFormat::Astc2d8x5Unorm
            | PixelFormat::Astc2d4x4Srgb
            | PixelFormat::Astc2d5x4Srgb
            | PixelFormat::Astc2d5x5Srgb
            | PixelFormat::Astc2d8x8Srgb
            | PixelFormat::Astc2d8x5Srgb
            | PixelFormat::Astc2d10x8Unorm
            | PixelFormat::Astc2d10x8Srgb
            | PixelFormat::Astc2d6x6Unorm
            | PixelFormat::Astc2d6x6Srgb
            | PixelFormat::Astc2d10x6Unorm
            | PixelFormat::Astc2d10x6Srgb
            | PixelFormat::Astc2d10x5Unorm
            | PixelFormat::Astc2d10x5Srgb
            | PixelFormat::Astc2d10x10Unorm
            | PixelFormat::Astc2d10x10Srgb
            | PixelFormat::Astc2d12x10Unorm
            | PixelFormat::Astc2d12x10Srgb
            | PixelFormat::Astc2d12x12Unorm
            | PixelFormat::Astc2d12x12Srgb
            | PixelFormat::Astc2d8x6Unorm
            | PixelFormat::Astc2d8x6Srgb
            | PixelFormat::Astc2d6x5Unorm
            | PixelFormat::Astc2d6x5Srgb
    )
}

/// Returns true if the format is a BCn compressed format.
///
/// Port of `IsPixelFormatBCn` from `surface.cpp`.
pub fn is_pixel_format_bcn(format: PixelFormat) -> bool {
    matches!(
        format,
        PixelFormat::Bc1RgbaUnorm
            | PixelFormat::Bc2Unorm
            | PixelFormat::Bc3Unorm
            | PixelFormat::Bc4Unorm
            | PixelFormat::Bc4Snorm
            | PixelFormat::Bc5Unorm
            | PixelFormat::Bc5Snorm
            | PixelFormat::Bc1RgbaSrgb
            | PixelFormat::Bc2Srgb
            | PixelFormat::Bc3Srgb
            | PixelFormat::Bc7Unorm
            | PixelFormat::Bc6hUfloat
            | PixelFormat::Bc6hSfloat
            | PixelFormat::Bc7Srgb
    )
}

/// Returns true if the format is an sRGB format.
///
/// Port of `IsPixelFormatSRGB` from `surface.cpp`.
pub fn is_pixel_format_srgb(format: PixelFormat) -> bool {
    matches!(
        format,
        PixelFormat::A8B8G8R8Srgb
            | PixelFormat::B8G8R8A8Srgb
            | PixelFormat::Bc1RgbaSrgb
            | PixelFormat::Bc2Srgb
            | PixelFormat::Bc3Srgb
            | PixelFormat::Bc7Srgb
            | PixelFormat::Astc2d4x4Srgb
            | PixelFormat::Astc2d8x8Srgb
            | PixelFormat::Astc2d8x5Srgb
            | PixelFormat::Astc2d5x4Srgb
            | PixelFormat::Astc2d5x5Srgb
            | PixelFormat::Astc2d10x6Srgb
            | PixelFormat::Astc2d10x8Srgb
            | PixelFormat::Astc2d6x6Srgb
            | PixelFormat::Astc2d10x5Srgb
            | PixelFormat::Astc2d10x10Srgb
            | PixelFormat::Astc2d12x12Srgb
            | PixelFormat::Astc2d12x10Srgb
            | PixelFormat::Astc2d8x6Srgb
            | PixelFormat::Astc2d6x5Srgb
            | PixelFormat::Etc2RgbSrgb
            | PixelFormat::Etc2RgbaSrgb
            | PixelFormat::Etc2RgbPtaSrgb
    )
}

/// Returns true if the format is an ETC2 or EAC format.
///
/// Port of `IsPixelFormatETC2` from `surface.cpp`.
pub fn is_pixel_format_etc2(format: PixelFormat) -> bool {
    matches!(
        format,
        PixelFormat::Etc2RgbUnorm
            | PixelFormat::Etc2RgbaUnorm
            | PixelFormat::Etc2RgbPtaUnorm
            | PixelFormat::Etc2RgbSrgb
            | PixelFormat::Etc2RgbaSrgb
            | PixelFormat::Etc2RgbPtaSrgb
            | PixelFormat::EacR11Unorm
            | PixelFormat::EacR11Snorm
            | PixelFormat::EacR11G11Unorm
            | PixelFormat::EacR11G11Snorm
    )
}

/// Returns true if the format is an integer format.
///
/// Port of `IsPixelFormatInteger` from `surface.cpp`.
pub fn is_pixel_format_integer(format: PixelFormat) -> bool {
    matches!(
        format,
        PixelFormat::A8B8G8R8Sint
            | PixelFormat::A8B8G8R8Uint
            | PixelFormat::A2B10G10R10Uint
            | PixelFormat::R8Sint
            | PixelFormat::R8Uint
            | PixelFormat::R16G16B16A16Sint
            | PixelFormat::R16G16B16A16Uint
            | PixelFormat::R32G32B32A32Uint
            | PixelFormat::R32G32B32A32Sint
            | PixelFormat::R32G32Sint
            | PixelFormat::R16Uint
            | PixelFormat::R16Sint
            | PixelFormat::R16G16Uint
            | PixelFormat::R16G16Sint
            | PixelFormat::R8G8Sint
            | PixelFormat::R8G8Uint
            | PixelFormat::R32G32Uint
            | PixelFormat::R32Uint
            | PixelFormat::R32Sint
    )
}

/// Returns true if the format is a signed integer format.
///
/// Port of `IsPixelFormatSignedInteger` from `surface.cpp`.
pub fn is_pixel_format_signed_integer(format: PixelFormat) -> bool {
    matches!(
        format,
        PixelFormat::A8B8G8R8Sint
            | PixelFormat::R8Sint
            | PixelFormat::R16G16B16A16Sint
            | PixelFormat::R32G32B32A32Sint
            | PixelFormat::R32G32Sint
            | PixelFormat::R16Sint
            | PixelFormat::R16G16Sint
            | PixelFormat::R8G8Sint
            | PixelFormat::R32Sint
    )
}

/// Returns the component size in bits for integer formats.
///
/// Port of `PixelComponentSizeBitsInteger` from `surface.cpp`.
pub fn pixel_component_size_bits_integer(format: PixelFormat) -> usize {
    match format {
        PixelFormat::A8B8G8R8Sint
        | PixelFormat::A8B8G8R8Uint
        | PixelFormat::R8Sint
        | PixelFormat::R8Uint
        | PixelFormat::R8G8Sint
        | PixelFormat::R8G8Uint => 8,
        PixelFormat::A2B10G10R10Uint => 10,
        PixelFormat::R16G16B16A16Sint
        | PixelFormat::R16G16B16A16Uint
        | PixelFormat::R16Uint
        | PixelFormat::R16Sint
        | PixelFormat::R16G16Uint
        | PixelFormat::R16G16Sint => 16,
        PixelFormat::R32G32B32A32Uint
        | PixelFormat::R32G32B32A32Sint
        | PixelFormat::R32G32Sint
        | PixelFormat::R32G32Uint
        | PixelFormat::R32Uint
        | PixelFormat::R32Sint => 32,
        _ => 0,
    }
}

/// Returns (block_width, block_height) for an ASTC format.
///
/// Port of `GetASTCBlockSize` from `surface.cpp`.
pub fn get_astc_block_size(format: PixelFormat) -> (u32, u32) {
    (default_block_width(format), default_block_height(format))
}

/// Returns the size of an ASTC texture after transcoding.
///
/// Port of `TranscodedAstcSize` from `surface.cpp`.
pub fn transcoded_astc_size(base_size: u64, format: PixelFormat) -> u64 {
    const RGBA8_PIXEL_SIZE: u64 = 4;
    let base_block_size = (default_block_width(format) as u64)
        * (default_block_height(format) as u64)
        * RGBA8_PIXEL_SIZE;
    let uncompressed_size =
        base_size.wrapping_mul(base_block_size) / bytes_per_block(format) as u64;

    match *common::settings::values().astc_recompression.get_value() {
        common::settings_enums::AstcRecompression::Bc1 => uncompressed_size / 8,
        common::settings_enums::AstcRecompression::Bc3 => uncompressed_size / 4,
        common::settings_enums::AstcRecompression::Uncompressed => uncompressed_size,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::settings_enums::AstcRecompression;
    use std::sync::Mutex;

    static ASTC_RECOMPRESSION_LOCK: Mutex<()> = Mutex::new(());

    struct AstcRecompressionRestore(AstcRecompression);

    impl Drop for AstcRecompressionRestore {
        fn drop(&mut self) {
            common::settings::values_mut()
                .astc_recompression
                .set_value(self.0);
        }
    }

    #[test]
    fn enum_values_and_layout_match_upstream_surface_h() {
        assert_eq!(PixelFormat::MaxDepthStencilFormat as u32, 112);
        assert_eq!(PixelFormat::Invalid as u32, 255);
        assert_eq!(std::mem::size_of::<PixelFormat>(), 4);
        assert_eq!(std::mem::size_of::<SurfaceType>(), 4);
        assert_eq!(std::mem::size_of::<SurfaceTarget>(), 4);
    }

    #[test]
    fn table_sizes() {
        assert_eq!(BLOCK_WIDTH_TABLE.len(), MAX_PIXEL_FORMAT);
        assert_eq!(BLOCK_HEIGHT_TABLE.len(), MAX_PIXEL_FORMAT);
        assert_eq!(BITS_PER_BLOCK_TABLE.len(), MAX_PIXEL_FORMAT);
    }

    #[test]
    fn block_dimensions_basic() {
        // Uncompressed RGBA8 should be 1x1
        assert_eq!(default_block_width(PixelFormat::A8B8G8R8Unorm), 1);
        assert_eq!(default_block_height(PixelFormat::A8B8G8R8Unorm), 1);
        // BC1 should be 4x4
        assert_eq!(default_block_width(PixelFormat::Bc1RgbaUnorm), 4);
        assert_eq!(default_block_height(PixelFormat::Bc1RgbaUnorm), 4);
        // ASTC 8x8 should be 8x8
        assert_eq!(default_block_width(PixelFormat::Astc2d8x8Unorm), 8);
        assert_eq!(default_block_height(PixelFormat::Astc2d8x8Unorm), 8);
    }

    #[test]
    fn bits_per_block_basic() {
        assert_eq!(bits_per_block(PixelFormat::A8B8G8R8Unorm), 32);
        assert_eq!(bits_per_block(PixelFormat::R8Unorm), 8);
        assert_eq!(bits_per_block(PixelFormat::R16G16B16A16Float), 64);
        assert_eq!(bits_per_block(PixelFormat::R32G32B32A32Float), 128);
        assert_eq!(bits_per_block(PixelFormat::Bc1RgbaUnorm), 64);
    }

    #[test]
    fn bytes_per_block_basic() {
        assert_eq!(bytes_per_block(PixelFormat::A8B8G8R8Unorm), 4);
        assert_eq!(bytes_per_block(PixelFormat::R8Unorm), 1);
        assert_eq!(bytes_per_block(PixelFormat::R16Float), 2);
    }

    #[test]
    fn format_type_classification() {
        assert_eq!(
            get_format_type(PixelFormat::A8B8G8R8Unorm),
            SurfaceType::ColorTexture
        );
        assert_eq!(get_format_type(PixelFormat::D32Float), SurfaceType::Depth);
        assert_eq!(get_format_type(PixelFormat::S8Uint), SurfaceType::Stencil);
        assert_eq!(
            get_format_type(PixelFormat::D24UnormS8Uint),
            SurfaceType::DepthStencil
        );
        assert_eq!(get_format_type(PixelFormat::Invalid), SurfaceType::Invalid);
    }

    #[test]
    fn texture_type_to_surface_target_matches_upstream() {
        use crate::textures::texture::TextureType;

        let cases = [
            (TextureType::Texture1D, SurfaceTarget::Texture1D),
            (TextureType::Texture1DBuffer, SurfaceTarget::TextureBuffer),
            (TextureType::Texture2D, SurfaceTarget::Texture2D),
            (TextureType::Texture2DNoMipmap, SurfaceTarget::Texture2D),
            (TextureType::Texture3D, SurfaceTarget::Texture3D),
            (TextureType::TextureCubemap, SurfaceTarget::TextureCubemap),
            (
                TextureType::TextureCubeArray,
                SurfaceTarget::TextureCubeArray,
            ),
            (TextureType::Texture1DArray, SurfaceTarget::Texture1DArray),
            (TextureType::Texture2DArray, SurfaceTarget::Texture2DArray),
        ];

        for (texture_type, expected) in cases {
            assert_eq!(surface_target_from_texture_type(texture_type), expected);
        }
    }

    #[test]
    fn alpha_classification_matches_upstream() {
        for format in [
            PixelFormat::A8B8G8R8Unorm,
            PixelFormat::A2B10G10R10Uint,
            PixelFormat::R16G16B16A16Float,
            PixelFormat::R32G32B32A32Uint,
            PixelFormat::Bc1RgbaUnorm,
            PixelFormat::Bc2Unorm,
            PixelFormat::Bc3Srgb,
            PixelFormat::Bc7Unorm,
            PixelFormat::B8G8R8A8Srgb,
            PixelFormat::A4B4G4R4Unorm,
        ] {
            assert!(has_alpha(format), "{format:?}");
        }

        for format in [
            PixelFormat::R8Unorm,
            PixelFormat::R5G6B5Unorm,
            PixelFormat::Bc4Unorm,
            PixelFormat::Bc5Snorm,
            PixelFormat::Astc2d4x4Unorm,
            PixelFormat::Etc2RgbaUnorm,
            PixelFormat::D32Float,
            PixelFormat::Invalid,
        ] {
            assert!(!has_alpha(format), "{format:?}");
        }
    }

    #[test]
    fn astc_classification() {
        assert!(is_pixel_format_astc(PixelFormat::Astc2d4x4Unorm));
        assert!(is_pixel_format_astc(PixelFormat::Astc2d8x8Srgb));
        assert!(!is_pixel_format_astc(PixelFormat::A8B8G8R8Unorm));
        assert!(!is_pixel_format_astc(PixelFormat::Bc1RgbaUnorm));
    }

    #[test]
    fn bcn_classification() {
        assert!(is_pixel_format_bcn(PixelFormat::Bc1RgbaUnorm));
        assert!(is_pixel_format_bcn(PixelFormat::Bc7Srgb));
        assert!(!is_pixel_format_bcn(PixelFormat::A8B8G8R8Unorm));
        assert!(!is_pixel_format_bcn(PixelFormat::Astc2d4x4Unorm));
    }

    #[test]
    fn srgb_classification() {
        assert!(is_pixel_format_srgb(PixelFormat::A8B8G8R8Srgb));
        assert!(is_pixel_format_srgb(PixelFormat::Bc7Srgb));
        assert!(!is_pixel_format_srgb(PixelFormat::A8B8G8R8Unorm));
    }

    #[test]
    fn integer_classification() {
        assert!(is_pixel_format_integer(PixelFormat::A8B8G8R8Uint));
        assert!(is_pixel_format_integer(PixelFormat::R32Uint));
        assert!(!is_pixel_format_integer(PixelFormat::R32Float));
        assert!(is_pixel_format_signed_integer(PixelFormat::R32Sint));
        assert!(!is_pixel_format_signed_integer(PixelFormat::R32Uint));
    }

    #[test]
    fn component_size_bits() {
        assert_eq!(pixel_component_size_bits_integer(PixelFormat::R8Uint), 8);
        assert_eq!(pixel_component_size_bits_integer(PixelFormat::R16Uint), 16);
        assert_eq!(pixel_component_size_bits_integer(PixelFormat::R32Uint), 32);
        assert_eq!(
            pixel_component_size_bits_integer(PixelFormat::A2B10G10R10Uint),
            10
        );
        assert_eq!(pixel_component_size_bits_integer(PixelFormat::R32Float), 0);
    }

    #[test]
    fn surface_target_layered() {
        assert!(surface_target_is_layered(SurfaceTarget::Texture2DArray));
        assert!(surface_target_is_layered(SurfaceTarget::TextureCubemap));
        assert!(!surface_target_is_layered(SurfaceTarget::Texture2D));
        assert!(!surface_target_is_layered(SurfaceTarget::Texture3D));
    }

    #[test]
    fn surface_target_array() {
        assert!(surface_target_is_array(SurfaceTarget::Texture1DArray));
        assert!(surface_target_is_array(SurfaceTarget::Texture2DArray));
        assert!(!surface_target_is_array(SurfaceTarget::TextureCubemap));
        assert!(!surface_target_is_array(SurfaceTarget::Texture2D));
    }

    #[test]
    fn astc_block_size() {
        assert_eq!(get_astc_block_size(PixelFormat::Astc2d4x4Unorm), (4, 4));
        assert_eq!(get_astc_block_size(PixelFormat::Astc2d8x8Unorm), (8, 8));
        assert_eq!(get_astc_block_size(PixelFormat::Astc2d10x6Unorm), (10, 6));
    }

    #[test]
    fn transcoded_astc_size_follows_recompression_setting() {
        let _lock = ASTC_RECOMPRESSION_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = *common::settings::values().astc_recompression.get_value();
        let _restore = AstcRecompressionRestore(previous);

        // A 128-bit block covering 4x4 pixels -> 4*4*4 = 64 bytes uncompressed
        // base_size = 16 bytes (one ASTC block), format 4x4 (block=16 bytes)
        for (recompression, expected) in [
            (AstcRecompression::Uncompressed, 64),
            (AstcRecompression::Bc1, 8),
            (AstcRecompression::Bc3, 16),
        ] {
            common::settings::values_mut()
                .astc_recompression
                .set_value(recompression);
            assert_eq!(
                transcoded_astc_size(16, PixelFormat::Astc2d4x4Unorm),
                expected
            );
        }
    }

    #[test]
    fn render_target_format_mapping_matches_upstream_surface_cpp() {
        let cases = [
            (0xC0, PixelFormat::R32G32B32A32Float),
            (0xC3, PixelFormat::R32G32B32A32Float),
            (0xC1, PixelFormat::R32G32B32A32Sint),
            (0xC4, PixelFormat::R32G32B32A32Sint),
            (0xC2, PixelFormat::R32G32B32A32Uint),
            (0xC5, PixelFormat::R32G32B32A32Uint),
            (0xC6, PixelFormat::R16G16B16A16Unorm),
            (0xC7, PixelFormat::R16G16B16A16Snorm),
            (0xC8, PixelFormat::R16G16B16A16Sint),
            (0xC9, PixelFormat::R16G16B16A16Uint),
            (0xCA, PixelFormat::R16G16B16A16Float),
            (0xCB, PixelFormat::R32G32Float),
            (0xCC, PixelFormat::R32G32Sint),
            (0xCD, PixelFormat::R32G32Uint),
            (0xCE, PixelFormat::R16G16B16X16Float),
            (0xCF, PixelFormat::B8G8R8A8Unorm),
            (0xE6, PixelFormat::B8G8R8A8Unorm),
            (0xD0, PixelFormat::B8G8R8A8Srgb),
            (0xE7, PixelFormat::B8G8R8A8Srgb),
            (0xD1, PixelFormat::A2B10G10R10Unorm),
            (0xD2, PixelFormat::A2B10G10R10Uint),
            (0xDF, PixelFormat::A2R10G10B10Unorm),
            (0xD5, PixelFormat::A8B8G8R8Unorm),
            (0xF9, PixelFormat::A8B8G8R8Unorm),
            (0xD6, PixelFormat::A8B8G8R8Srgb),
            (0xFA, PixelFormat::A8B8G8R8Srgb),
            (0xD7, PixelFormat::A8B8G8R8Snorm),
            (0xD8, PixelFormat::A8B8G8R8Sint),
            (0xD9, PixelFormat::A8B8G8R8Uint),
            (0xDA, PixelFormat::R16G16Unorm),
            (0xDB, PixelFormat::R16G16Snorm),
            (0xDC, PixelFormat::R16G16Sint),
            (0xDD, PixelFormat::R16G16Uint),
            (0xDE, PixelFormat::R16G16Float),
            (0xE0, PixelFormat::B10G11R11Float),
            (0xE3, PixelFormat::R32Sint),
            (0xE4, PixelFormat::R32Uint),
            (0xE5, PixelFormat::R32Float),
            (0xE8, PixelFormat::R5G6B5Unorm),
            (0xE9, PixelFormat::A1R5G5B5Unorm),
            (0xF8, PixelFormat::A1R5G5B5Unorm),
            (0xEA, PixelFormat::R8G8Unorm),
            (0xEB, PixelFormat::R8G8Snorm),
            (0xEC, PixelFormat::R8G8Sint),
            (0xED, PixelFormat::R8G8Uint),
            (0xEE, PixelFormat::R16Unorm),
            (0xEF, PixelFormat::R16Snorm),
            (0xF0, PixelFormat::R16Sint),
            (0xF1, PixelFormat::R16Uint),
            (0xF2, PixelFormat::R16Float),
            (0xF3, PixelFormat::R8Unorm),
            (0xF4, PixelFormat::R8Snorm),
            (0xF5, PixelFormat::R8Sint),
            (0xF6, PixelFormat::R8Uint),
        ];

        for (format, expected) in cases {
            assert_eq!(
                pixel_format_from_render_target_format(format),
                expected,
                "render-target format 0x{format:X}"
            );
        }
    }

    #[test]
    fn depth_format_mapping_matches_upstream_surface_cpp() {
        let cases = [
            (0x14, PixelFormat::S8UintD24Unorm),
            (0x16, PixelFormat::D24UnormS8Uint),
            (0x0A, PixelFormat::D32Float),
            (0x13, PixelFormat::D16Unorm),
            (0x17, PixelFormat::S8Uint),
            (0x19, PixelFormat::D32FloatS8Uint),
            (0x15, PixelFormat::X8D24Unorm),
        ];

        for (format, expected) in cases {
            assert_eq!(
                pixel_format_from_depth_format(format),
                expected,
                "depth format 0x{format:X}"
            );
        }
    }

    #[test]
    fn gpu_pixel_format_mapping_matches_upstream_surface_cpp() {
        let cases = [
            (1, PixelFormat::A8B8G8R8Unorm),
            (2, PixelFormat::A8B8G8R8Unorm),
            (4, PixelFormat::R5G6B5Unorm),
            (5, PixelFormat::B8G8R8A8Unorm),
        ];

        for (format, expected) in cases {
            assert_eq!(
                pixel_format_from_gpu_pixel_format(format),
                expected,
                "GPU pixel format {format}"
            );
        }
    }

    #[test]
    fn unimplemented_surface_format_conversions_are_fail_soft_like_upstream() {
        assert_eq!(
            pixel_format_from_render_target_format(u32::MAX),
            PixelFormat::A8B8G8R8Unorm
        );
        assert_eq!(
            pixel_format_from_depth_format(u32::MAX),
            PixelFormat::S8UintD24Unorm
        );
        assert_eq!(
            pixel_format_from_gpu_pixel_format(u32::MAX),
            PixelFormat::A8B8G8R8Unorm
        );
    }
    // Ported alongside the ETC2/EAC block. Values come from upstream's
    // `PIXEL_FORMAT_ELEM(name, block_width, block_height, bits_per_block)`
    // entries in `surface.h`.
    #[test]
    fn etc2_and_eac_block_geometry_matches_upstream() {
        for format in [
            PixelFormat::Etc2RgbUnorm,
            PixelFormat::Etc2RgbaUnorm,
            PixelFormat::Etc2RgbPtaUnorm,
            PixelFormat::Etc2RgbSrgb,
            PixelFormat::Etc2RgbaSrgb,
            PixelFormat::Etc2RgbPtaSrgb,
            PixelFormat::EacR11Unorm,
            PixelFormat::EacR11Snorm,
            PixelFormat::EacR11G11Unorm,
            PixelFormat::EacR11G11Snorm,
        ] {
            assert_eq!(default_block_width(format), 4, "{format:?}");
            assert_eq!(default_block_height(format), 4, "{format:?}");
            assert!(is_pixel_format_etc2(format), "{format:?}");
        }

        for (format, bits) in [
            (PixelFormat::Etc2RgbUnorm, 64u16),
            (PixelFormat::Etc2RgbaUnorm, 128),
            (PixelFormat::Etc2RgbPtaUnorm, 64),
            (PixelFormat::Etc2RgbSrgb, 64),
            (PixelFormat::Etc2RgbaSrgb, 128),
            (PixelFormat::Etc2RgbPtaSrgb, 64),
            (PixelFormat::EacR11Unorm, 64),
            (PixelFormat::EacR11Snorm, 64),
            (PixelFormat::EacR11G11Unorm, 128),
            (PixelFormat::EacR11G11Snorm, 128),
        ] {
            assert_eq!(BITS_PER_BLOCK_TABLE[format as usize], bits, "{format:?}");
        }
    }

    // Upstream `IsPixelFormatSRGB` lists exactly the three ETC2 sRGB formats;
    // EAC has no sRGB variant.
    #[test]
    fn only_the_three_etc2_srgb_formats_are_srgb() {
        assert!(is_pixel_format_srgb(PixelFormat::Etc2RgbSrgb));
        assert!(is_pixel_format_srgb(PixelFormat::Etc2RgbaSrgb));
        assert!(is_pixel_format_srgb(PixelFormat::Etc2RgbPtaSrgb));
        assert!(!is_pixel_format_srgb(PixelFormat::Etc2RgbUnorm));
        assert!(!is_pixel_format_srgb(PixelFormat::EacR11Unorm));
        assert!(!is_pixel_format_srgb(PixelFormat::EacR11G11Snorm));
    }

    // Guards the table sizes against a future insertion into `PixelFormat`.
    #[test]
    fn every_pixel_format_indexed_table_covers_the_whole_enum() {
        assert_eq!(MAX_PIXEL_FORMAT, 112);
        assert_eq!(BLOCK_WIDTH_TABLE.len(), MAX_PIXEL_FORMAT);
        assert_eq!(BLOCK_HEIGHT_TABLE.len(), MAX_PIXEL_FORMAT);
        assert_eq!(BITS_PER_BLOCK_TABLE.len(), MAX_PIXEL_FORMAT);
    }
}
