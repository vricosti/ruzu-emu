// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden `video_core/renderer_opengl/maxwell_to_gl.h`.
//!
//! Maxwell GPU register values to OpenGL enum translation tables.

use std::ffi::CStr;
use std::sync::OnceLock;

use crate::engines::maxwell_3d::PrimitiveTopology;

const GL_QUAD_STRIP: u32 = 0x0008;
const GL_POLYGON: u32 = 0x0009;

/// A GL format tuple (internal_format, format, type).
///
/// Corresponds to `OpenGL::MaxwellToGL::FormatTuple`.
#[derive(Clone, Copy, Debug)]
pub struct FormatTuple {
    pub internal_format: u32,
    pub format: u32,
    pub gl_type: u32,
}

impl FormatTuple {
    pub const fn new(internal_format: u32, format: u32, gl_type: u32) -> Self {
        Self {
            internal_format,
            format,
            gl_type,
        }
    }

    /// A slot with no OpenGL representation. Upstream's `GetFormatTuple` switch
    /// simply has no `case` for these formats and hits `UNREACHABLE()`; here the
    /// table is indexed, so the slot exists and is flagged instead.
    pub const fn unsupported() -> Self {
        Self {
            internal_format: gl::NONE,
            format: gl::NONE,
            gl_type: gl::NONE,
        }
    }

    /// True for a slot with no OpenGL representation.
    pub const fn is_unsupported(&self) -> bool {
        self.internal_format == gl::NONE
    }

    pub const fn compressed(internal_format: u32) -> Self {
        Self {
            internal_format,
            format: gl::NONE,
            gl_type: gl::NONE,
        }
    }
}

// Extension constants used by upstream's `FORMAT_TABLE`. The `gl` crate does
// not expose all KHR/EXT names on every generated profile, so keep the numeric
// values next to the upstream table they serve.
const GL_COMPRESSED_RGBA_S3TC_DXT1_EXT: u32 = 0x83F1;
const GL_COMPRESSED_RGBA_S3TC_DXT3_EXT: u32 = 0x83F2;
const GL_COMPRESSED_RGBA_S3TC_DXT5_EXT: u32 = 0x83F3;
const GL_COMPRESSED_SRGB_ALPHA_S3TC_DXT1_EXT: u32 = 0x8C4D;
const GL_COMPRESSED_SRGB_ALPHA_S3TC_DXT3_EXT: u32 = 0x8C4E;
const GL_COMPRESSED_SRGB_ALPHA_S3TC_DXT5_EXT: u32 = 0x8C4F;
const GL_COMPRESSED_RGBA_ASTC_4X4_KHR: u32 = 0x93B0;
const GL_COMPRESSED_RGBA_ASTC_5X4_KHR: u32 = 0x93B1;
const GL_COMPRESSED_RGBA_ASTC_5X5_KHR: u32 = 0x93B2;
const GL_COMPRESSED_RGBA_ASTC_6X5_KHR: u32 = 0x93B3;
const GL_COMPRESSED_RGBA_ASTC_6X6_KHR: u32 = 0x93B4;
const GL_COMPRESSED_RGBA_ASTC_8X5_KHR: u32 = 0x93B5;
const GL_COMPRESSED_RGBA_ASTC_8X6_KHR: u32 = 0x93B6;
const GL_COMPRESSED_RGBA_ASTC_8X8_KHR: u32 = 0x93B7;
const GL_COMPRESSED_RGBA_ASTC_10X5_KHR: u32 = 0x93B8;
const GL_COMPRESSED_RGBA_ASTC_10X6_KHR: u32 = 0x93B9;
const GL_COMPRESSED_RGBA_ASTC_10X8_KHR: u32 = 0x93BA;
const GL_COMPRESSED_RGBA_ASTC_10X10_KHR: u32 = 0x93BB;
const GL_COMPRESSED_RGBA_ASTC_12X10_KHR: u32 = 0x93BC;
const GL_COMPRESSED_RGBA_ASTC_12X12_KHR: u32 = 0x93BD;
const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_4X4_KHR: u32 = 0x93D0;
const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_5X4_KHR: u32 = 0x93D1;
const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_5X5_KHR: u32 = 0x93D2;
const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_6X5_KHR: u32 = 0x93D3;
const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_6X6_KHR: u32 = 0x93D4;
const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_8X5_KHR: u32 = 0x93D5;
const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_8X6_KHR: u32 = 0x93D6;
const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_8X8_KHR: u32 = 0x93D7;
const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_10X5_KHR: u32 = 0x93D8;
const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_10X6_KHR: u32 = 0x93D9;
const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_10X8_KHR: u32 = 0x93DA;
const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_10X10_KHR: u32 = 0x93DB;
const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_12X10_KHR: u32 = 0x93DC;
const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_12X12_KHR: u32 = 0x93DD;

/// Format table mapping PixelFormat enum to GL format tuples.
///
/// Corresponds to `OpenGL::MaxwellToGL::FORMAT_TABLE`.
pub(crate) static FORMAT_TABLE: &[FormatTuple] = &[
    FormatTuple::new(gl::RGBA8, gl::RGBA, gl::UNSIGNED_INT_8_8_8_8_REV), // A8B8G8R8_UNORM
    FormatTuple::new(gl::RGBA8_SNORM, gl::RGBA, gl::BYTE),               // A8B8G8R8_SNORM
    FormatTuple::new(gl::RGBA8I, gl::RGBA_INTEGER, gl::BYTE),            // A8B8G8R8_SINT
    FormatTuple::new(gl::RGBA8UI, gl::RGBA_INTEGER, gl::UNSIGNED_BYTE),  // A8B8G8R8_UINT
    FormatTuple::new(gl::RGB565, gl::RGB, gl::UNSIGNED_SHORT_5_6_5),     // R5G6B5_UNORM
    FormatTuple::new(gl::RGB565, gl::RGB, gl::UNSIGNED_SHORT_5_6_5_REV), // B5G6R5_UNORM
    FormatTuple::new(gl::RGB5_A1, gl::BGRA, gl::UNSIGNED_SHORT_1_5_5_5_REV), // A1R5G5B5_UNORM
    FormatTuple::new(gl::RGB10_A2, gl::RGBA, gl::UNSIGNED_INT_2_10_10_10_REV), // A2B10G10R10_UNORM
    FormatTuple::new(
        gl::RGB10_A2UI,
        gl::RGBA_INTEGER,
        gl::UNSIGNED_INT_2_10_10_10_REV,
    ), // A2B10G10R10_UINT
    FormatTuple::new(gl::RGB10_A2, gl::BGRA, gl::UNSIGNED_INT_2_10_10_10_REV), // A2R10G10B10_UNORM
    FormatTuple::new(gl::RGB5_A1, gl::RGBA, gl::UNSIGNED_SHORT_1_5_5_5_REV), // A1B5G5R5_UNORM
    FormatTuple::new(gl::RGB5_A1, gl::RGBA, gl::UNSIGNED_SHORT_5_5_5_1), // A5B5G5R1_UNORM
    FormatTuple::new(gl::R8, gl::RED, gl::UNSIGNED_BYTE),                // R8_UNORM
    FormatTuple::new(gl::R8_SNORM, gl::RED, gl::BYTE),                   // R8_SNORM
    FormatTuple::new(gl::R8I, gl::RED_INTEGER, gl::BYTE),                // R8_SINT
    FormatTuple::new(gl::R8UI, gl::RED_INTEGER, gl::UNSIGNED_BYTE),      // R8_UINT
    FormatTuple::new(gl::RGBA16F, gl::RGBA, gl::HALF_FLOAT),             // R16G16B16A16_FLOAT
    FormatTuple::new(gl::RGBA16, gl::RGBA, gl::UNSIGNED_SHORT),          // R16G16B16A16_UNORM
    FormatTuple::new(gl::RGBA16_SNORM, gl::RGBA, gl::SHORT),             // R16G16B16A16_SNORM
    FormatTuple::new(gl::RGBA16I, gl::RGBA_INTEGER, gl::SHORT),          // R16G16B16A16_SINT
    FormatTuple::new(gl::RGBA16UI, gl::RGBA_INTEGER, gl::UNSIGNED_SHORT), // R16G16B16A16_UINT
    FormatTuple::new(
        gl::R11F_G11F_B10F,
        gl::RGB,
        gl::UNSIGNED_INT_10F_11F_11F_REV,
    ), // B10G11R11_FLOAT
    FormatTuple::new(gl::RGBA32UI, gl::RGBA_INTEGER, gl::UNSIGNED_INT),  // R32G32B32A32_UINT
    FormatTuple::compressed(GL_COMPRESSED_RGBA_S3TC_DXT1_EXT),           // BC1_RGBA_UNORM
    FormatTuple::compressed(GL_COMPRESSED_RGBA_S3TC_DXT3_EXT),           // BC2_UNORM
    FormatTuple::compressed(GL_COMPRESSED_RGBA_S3TC_DXT5_EXT),           // BC3_UNORM
    FormatTuple::compressed(gl::COMPRESSED_RED_RGTC1),                   // BC4_UNORM
    FormatTuple::compressed(gl::COMPRESSED_SIGNED_RED_RGTC1),            // BC4_SNORM
    FormatTuple::compressed(gl::COMPRESSED_RG_RGTC2),                    // BC5_UNORM
    FormatTuple::compressed(gl::COMPRESSED_SIGNED_RG_RGTC2),             // BC5_SNORM
    FormatTuple::compressed(gl::COMPRESSED_RGBA_BPTC_UNORM),             // BC7_UNORM
    FormatTuple::compressed(gl::COMPRESSED_RGB_BPTC_UNSIGNED_FLOAT),     // BC6H_UFLOAT
    FormatTuple::compressed(gl::COMPRESSED_RGB_BPTC_SIGNED_FLOAT),       // BC6H_SFLOAT
    FormatTuple::compressed(GL_COMPRESSED_RGBA_ASTC_4X4_KHR),            // ASTC_2D_4X4_UNORM
    FormatTuple::new(gl::RGBA8, gl::BGRA, gl::UNSIGNED_INT_8_8_8_8_REV), // B8G8R8A8_UNORM
    FormatTuple::new(gl::RGBA32F, gl::RGBA, gl::FLOAT),                  // R32G32B32A32_FLOAT
    FormatTuple::new(gl::RGBA32I, gl::RGBA_INTEGER, gl::INT),            // R32G32B32A32_SINT
    FormatTuple::new(gl::RG32F, gl::RG, gl::FLOAT),                      // R32G32_FLOAT
    FormatTuple::new(gl::RG32I, gl::RG_INTEGER, gl::INT),                // R32G32_SINT
    FormatTuple::new(gl::R32F, gl::RED, gl::FLOAT),                      // R32_FLOAT
    FormatTuple::new(gl::R16F, gl::RED, gl::HALF_FLOAT),                 // R16_FLOAT
    FormatTuple::new(gl::R16, gl::RED, gl::UNSIGNED_SHORT),              // R16_UNORM
    FormatTuple::new(gl::R16_SNORM, gl::RED, gl::SHORT),                 // R16_SNORM
    FormatTuple::new(gl::R16UI, gl::RED_INTEGER, gl::UNSIGNED_SHORT),    // R16_UINT
    FormatTuple::new(gl::R16I, gl::RED_INTEGER, gl::SHORT),              // R16_SINT
    FormatTuple::new(gl::RG16, gl::RG, gl::UNSIGNED_SHORT),              // R16G16_UNORM
    FormatTuple::new(gl::RG16F, gl::RG, gl::HALF_FLOAT),                 // R16G16_FLOAT
    FormatTuple::new(gl::RG16UI, gl::RG_INTEGER, gl::UNSIGNED_SHORT),    // R16G16_UINT
    FormatTuple::new(gl::RG16I, gl::RG_INTEGER, gl::SHORT),              // R16G16_SINT
    FormatTuple::new(gl::RG16_SNORM, gl::RG, gl::SHORT),                 // R16G16_SNORM
    FormatTuple::new(gl::RGB32F, gl::RGB, gl::FLOAT),                    // R32G32B32_FLOAT
    FormatTuple::new(gl::SRGB8_ALPHA8, gl::RGBA, gl::UNSIGNED_INT_8_8_8_8_REV), // A8B8G8R8_SRGB
    FormatTuple::new(gl::RG8, gl::RG, gl::UNSIGNED_BYTE),                // R8G8_UNORM
    FormatTuple::new(gl::RG8_SNORM, gl::RG, gl::BYTE),                   // R8G8_SNORM
    FormatTuple::new(gl::RG8I, gl::RG_INTEGER, gl::BYTE),                // R8G8_SINT
    FormatTuple::new(gl::RG8UI, gl::RG_INTEGER, gl::UNSIGNED_BYTE),      // R8G8_UINT
    FormatTuple::new(gl::RG32UI, gl::RG_INTEGER, gl::UNSIGNED_INT),      // R32G32_UINT
    FormatTuple::new(gl::RGB16F, gl::RGBA, gl::HALF_FLOAT),              // R16G16B16X16_FLOAT
    FormatTuple::new(gl::R32UI, gl::RED_INTEGER, gl::UNSIGNED_INT),      // R32_UINT
    FormatTuple::new(gl::R32I, gl::RED_INTEGER, gl::INT),                // R32_SINT
    FormatTuple::compressed(GL_COMPRESSED_RGBA_ASTC_8X8_KHR),            // ASTC_2D_8X8_UNORM
    FormatTuple::compressed(GL_COMPRESSED_RGBA_ASTC_8X5_KHR),            // ASTC_2D_8X5_UNORM
    FormatTuple::compressed(GL_COMPRESSED_RGBA_ASTC_5X4_KHR),            // ASTC_2D_5X4_UNORM
    FormatTuple::new(gl::SRGB8_ALPHA8, gl::BGRA, gl::UNSIGNED_INT_8_8_8_8_REV), // B8G8R8A8_SRGB
    FormatTuple::compressed(GL_COMPRESSED_SRGB_ALPHA_S3TC_DXT1_EXT),     // BC1_RGBA_SRGB
    FormatTuple::compressed(GL_COMPRESSED_SRGB_ALPHA_S3TC_DXT3_EXT),     // BC2_SRGB
    FormatTuple::compressed(GL_COMPRESSED_SRGB_ALPHA_S3TC_DXT5_EXT),     // BC3_SRGB
    FormatTuple::compressed(gl::COMPRESSED_SRGB_ALPHA_BPTC_UNORM),       // BC7_SRGB
    FormatTuple::new(gl::RGBA4, gl::RGBA, gl::UNSIGNED_SHORT_4_4_4_4_REV), // A4B4G4R4_UNORM
    FormatTuple::new(gl::R8, gl::RED, gl::UNSIGNED_BYTE),                // G4R4_UNORM
    FormatTuple::compressed(GL_COMPRESSED_SRGB8_ALPHA8_ASTC_4X4_KHR),    // ASTC_2D_4X4_SRGB
    FormatTuple::compressed(GL_COMPRESSED_SRGB8_ALPHA8_ASTC_8X8_KHR),    // ASTC_2D_8X8_SRGB
    FormatTuple::compressed(GL_COMPRESSED_SRGB8_ALPHA8_ASTC_8X5_KHR),    // ASTC_2D_8X5_SRGB
    FormatTuple::compressed(GL_COMPRESSED_SRGB8_ALPHA8_ASTC_5X4_KHR),    // ASTC_2D_5X4_SRGB
    FormatTuple::compressed(GL_COMPRESSED_RGBA_ASTC_5X5_KHR),            // ASTC_2D_5X5_UNORM
    FormatTuple::compressed(GL_COMPRESSED_SRGB8_ALPHA8_ASTC_5X5_KHR),    // ASTC_2D_5X5_SRGB
    FormatTuple::compressed(GL_COMPRESSED_RGBA_ASTC_10X8_KHR),           // ASTC_2D_10X8_UNORM
    FormatTuple::compressed(GL_COMPRESSED_SRGB8_ALPHA8_ASTC_10X8_KHR),   // ASTC_2D_10X8_SRGB
    FormatTuple::compressed(GL_COMPRESSED_RGBA_ASTC_6X6_KHR),            // ASTC_2D_6X6_UNORM
    FormatTuple::compressed(GL_COMPRESSED_SRGB8_ALPHA8_ASTC_6X6_KHR),    // ASTC_2D_6X6_SRGB
    FormatTuple::compressed(GL_COMPRESSED_RGBA_ASTC_10X6_KHR),           // ASTC_2D_10X6_UNORM
    FormatTuple::compressed(GL_COMPRESSED_SRGB8_ALPHA8_ASTC_10X6_KHR),   // ASTC_2D_10X6_SRGB
    FormatTuple::compressed(GL_COMPRESSED_RGBA_ASTC_10X5_KHR),           // ASTC_2D_10X5_UNORM
    FormatTuple::compressed(GL_COMPRESSED_SRGB8_ALPHA8_ASTC_10X5_KHR),   // ASTC_2D_10X5_SRGB
    FormatTuple::compressed(GL_COMPRESSED_RGBA_ASTC_10X10_KHR),          // ASTC_2D_10X10_UNORM
    FormatTuple::compressed(GL_COMPRESSED_SRGB8_ALPHA8_ASTC_10X10_KHR),  // ASTC_2D_10X10_SRGB
    FormatTuple::compressed(GL_COMPRESSED_RGBA_ASTC_12X10_KHR),          // ASTC_2D_12X10_UNORM
    FormatTuple::compressed(GL_COMPRESSED_SRGB8_ALPHA8_ASTC_12X10_KHR),  // ASTC_2D_12X10_SRGB
    FormatTuple::compressed(GL_COMPRESSED_RGBA_ASTC_12X12_KHR),          // ASTC_2D_12X12_UNORM
    FormatTuple::compressed(GL_COMPRESSED_SRGB8_ALPHA8_ASTC_12X12_KHR),  // ASTC_2D_12X12_SRGB
    FormatTuple::compressed(GL_COMPRESSED_RGBA_ASTC_8X6_KHR),            // ASTC_2D_8X6_UNORM
    FormatTuple::compressed(GL_COMPRESSED_SRGB8_ALPHA8_ASTC_8X6_KHR),    // ASTC_2D_8X6_SRGB
    FormatTuple::compressed(GL_COMPRESSED_RGBA_ASTC_6X5_KHR),            // ASTC_2D_6X5_UNORM
    FormatTuple::compressed(GL_COMPRESSED_SRGB8_ALPHA8_ASTC_6X5_KHR),    // ASTC_2D_6X5_SRGB
    FormatTuple::new(gl::RGB9_E5, gl::RGB, gl::UNSIGNED_INT_5_9_9_9_REV), // E5B9G9R9_FLOAT
    // `SURFACE_FORMAT_LIST` omits ETC2/EAC and continues directly with the
    // depth/stencil tuples. Keep that compact initializer order: the fixed-size
    // upstream array zero-initializes its ten unused trailing elements.
    FormatTuple::new(gl::DEPTH_COMPONENT32F, gl::DEPTH_COMPONENT, gl::FLOAT), // D32_FLOAT
    FormatTuple::new(
        gl::DEPTH_COMPONENT16,
        gl::DEPTH_COMPONENT,
        gl::UNSIGNED_SHORT,
    ), // D16_UNORM
    FormatTuple::new(
        gl::DEPTH_COMPONENT24,
        gl::DEPTH_COMPONENT,
        gl::UNSIGNED_INT_24_8,
    ), // X8_D24_UNORM
    FormatTuple::new(gl::STENCIL_INDEX8, gl::STENCIL, gl::UNSIGNED_BYTE),     // S8_UINT
    FormatTuple::new(
        gl::DEPTH24_STENCIL8,
        gl::DEPTH_STENCIL,
        gl::UNSIGNED_INT_24_8,
    ), // D24_UNORM_S8_UINT
    FormatTuple::new(
        gl::DEPTH24_STENCIL8,
        gl::DEPTH_STENCIL,
        gl::UNSIGNED_INT_24_8,
    ), // S8_UINT_D24_UNORM
    FormatTuple::new(
        gl::DEPTH32F_STENCIL8,
        gl::DEPTH_STENCIL,
        gl::FLOAT_32_UNSIGNED_INT_24_8_REV,
    ), // D32_FLOAT_S8_UINT
    FormatTuple::unsupported(),
    FormatTuple::unsupported(),
    FormatTuple::unsupported(),
    FormatTuple::unsupported(),
    FormatTuple::unsupported(),
    FormatTuple::unsupported(),
    FormatTuple::unsupported(),
    FormatTuple::unsupported(),
    FormatTuple::unsupported(),
    FormatTuple::unsupported(),
];

/// Look up the format tuple for a pixel format.
///
/// Corresponds to `OpenGL::MaxwellToGL::GetFormatTuple()`.
pub fn get_format_tuple(pixel_format: crate::surface::PixelFormat) -> FormatTuple {
    use crate::surface::PixelFormat;

    match pixel_format {
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
        | PixelFormat::MaxDepthStencilFormat
        | PixelFormat::Invalid => {
            panic!("GetFormatTuple: pixel format {pixel_format:?} has no OpenGL representation")
        }
        // Upstream `GetFormatTuple` is a switch generated from
        // `SURFACE_FORMAT_LIST`; it does not use the compact `FORMAT_TABLE` to
        // translate enum ordinals. Keep these post-ETC2/EAC cases explicit so
        // their mapping cannot depend on a magic enum/table offset.
        PixelFormat::D32Float => {
            FormatTuple::new(gl::DEPTH_COMPONENT32F, gl::DEPTH_COMPONENT, gl::FLOAT)
        }
        PixelFormat::D16Unorm => FormatTuple::new(
            gl::DEPTH_COMPONENT16,
            gl::DEPTH_COMPONENT,
            gl::UNSIGNED_SHORT,
        ),
        PixelFormat::X8D24Unorm => FormatTuple::new(
            gl::DEPTH_COMPONENT24,
            gl::DEPTH_COMPONENT,
            gl::UNSIGNED_INT_24_8,
        ),
        PixelFormat::S8Uint => FormatTuple::new(gl::STENCIL_INDEX8, gl::STENCIL, gl::UNSIGNED_BYTE),
        PixelFormat::D24UnormS8Uint | PixelFormat::S8UintD24Unorm => FormatTuple::new(
            gl::DEPTH24_STENCIL8,
            gl::DEPTH_STENCIL,
            gl::UNSIGNED_INT_24_8,
        ),
        PixelFormat::D32FloatS8Uint => FormatTuple::new(
            gl::DEPTH32F_STENCIL8,
            gl::DEPTH_STENCIL,
            gl::FLOAT_32_UNSIGNED_INT_24_8_REV,
        ),
        _ => FORMAT_TABLE[pixel_format as usize],
    }
}

/// Map a Maxwell index format to GL index type.
///
/// Corresponds to `OpenGL::MaxwellToGL::IndexFormat()`.
pub fn index_format(format: crate::engines::maxwell_3d::IndexFormat) -> u32 {
    use crate::engines::maxwell_3d::IndexFormat;
    match format {
        IndexFormat::UnsignedByte => gl::UNSIGNED_BYTE,
        IndexFormat::UnsignedShort => gl::UNSIGNED_SHORT,
        IndexFormat::UnsignedInt => gl::UNSIGNED_INT,
    }
}

/// Map a Maxwell primitive topology to GL primitive mode.
///
/// Corresponds to `OpenGL::MaxwellToGL::PrimitiveTopology()`.
pub fn primitive_topology(topology: PrimitiveTopology) -> u32 {
    use PrimitiveTopology::*;
    match topology {
        Points => gl::POINTS,
        Lines => gl::LINES,
        LineLoop => gl::LINE_LOOP,
        LineStrip => gl::LINE_STRIP,
        Triangles => gl::TRIANGLES,
        TriangleStrip => gl::TRIANGLE_STRIP,
        TriangleFan => gl::TRIANGLE_FAN,
        Quads => gl::QUADS,
        QuadStrip => GL_QUAD_STRIP,
        Polygon => GL_POLYGON,
        LinesAdjacency => gl::LINES_ADJACENCY,
        LineStripAdjacency => gl::LINE_STRIP_ADJACENCY,
        TrianglesAdjacency => gl::TRIANGLES_ADJACENCY,
        TriangleStripAdjacency => gl::TRIANGLE_STRIP_ADJACENCY,
        Patches => gl::PATCHES,
        invalid => {
            debug_assert!(false, "Invalid topology={invalid:?}");
            gl::POINTS
        }
    }
}

/// Map a Maxwell blend equation to GL blend equation.
///
/// Corresponds to `OpenGL::MaxwellToGL::BlendEquation()`.
pub fn blend_equation(equation: u32) -> u32 {
    match equation {
        1 | 0x8006 => gl::FUNC_ADD,
        2 | 0x800A => gl::FUNC_SUBTRACT,
        3 | 0x800B => gl::FUNC_REVERSE_SUBTRACT,
        4 | 0x8007 => gl::MIN,
        5 | 0x8008 => gl::MAX,
        _ => {
            log::warn!("Unimplemented blend equation: {}", equation);
            gl::FUNC_ADD
        }
    }
}

/// Map a Maxwell comparison op to GL comparison function.
///
/// Corresponds to `OpenGL::MaxwellToGL::ComparisonOp()`.
pub fn comparison_op(comparison: u32) -> u32 {
    match comparison {
        1 | 0x0200 => gl::NEVER,
        2 | 0x0201 => gl::LESS,
        3 | 0x0202 => gl::EQUAL,
        4 | 0x0203 => gl::LEQUAL,
        5 | 0x0204 => gl::GREATER,
        6 | 0x0205 => gl::NOTEQUAL,
        7 | 0x0206 => gl::GEQUAL,
        8 | 0x0207 => gl::ALWAYS,
        _ => {
            log::warn!("Unimplemented comparison op: {}", comparison);
            gl::ALWAYS
        }
    }
}

/// Map a Maxwell front face to GL front face.
///
/// Corresponds to `OpenGL::MaxwellToGL::FrontFace()`.
pub fn front_face(face: u32) -> u32 {
    match face {
        0x900 => gl::CW,
        0x901 => gl::CCW,
        _ => {
            log::warn!("Unimplemented front face: {}", face);
            gl::CCW
        }
    }
}

/// Map a Maxwell cull face to GL cull face.
///
/// Corresponds to `OpenGL::MaxwellToGL::CullFace()`.
pub fn cull_face(face: u32) -> u32 {
    match face {
        0x404 => gl::FRONT,
        0x405 => gl::BACK,
        0x408 => gl::FRONT_AND_BACK,
        _ => {
            log::warn!("Unimplemented cull face: {}", face);
            gl::BACK
        }
    }
}

/// Map a Maxwell polygon mode to GL polygon mode.
///
/// Corresponds to `OpenGL::MaxwellToGL::PolygonMode()`.
pub fn polygon_mode(mode: u32) -> u32 {
    match mode {
        0x1B00 => gl::POINT,
        0x1B01 => gl::LINE,
        0x1B02 => gl::FILL,
        _ => {
            log::warn!("Invalid polygon mode: {:#x}", mode);
            gl::FILL
        }
    }
}

/// Map a Maxwell stencil operation to GL stencil operation.
///
/// Corresponds to `OpenGL::MaxwellToGL::StencilOp()`.
pub fn stencil_op(op: u32) -> u32 {
    match op {
        // D3D / GL pairs
        1 | 0x1E00 => gl::KEEP,
        2 | 0x0000 => gl::ZERO,
        3 | 0x1E01 => gl::REPLACE,
        4 | 0x1E02 => gl::INCR,
        5 | 0x1E03 => gl::DECR,
        6 | 0x150A => gl::INVERT,
        7 | 0x8507 => gl::INCR_WRAP,
        8 | 0x8508 => gl::DECR_WRAP,
        _ => {
            log::warn!("Unimplemented stencil op: {:#x}", op);
            gl::KEEP
        }
    }
}

/// Map a Maxwell blend factor to GL blend factor.
///
/// Corresponds to `OpenGL::MaxwellToGL::BlendFunc()`.
pub fn blend_func(factor: u32) -> u32 {
    match factor {
        0x01 | 0x4000 => gl::ZERO,
        0x02 | 0x4001 => gl::ONE,
        0x03 | 0x4300 => gl::SRC_COLOR,
        0x04 | 0x4301 => gl::ONE_MINUS_SRC_COLOR,
        0x05 | 0x4302 => gl::SRC_ALPHA,
        0x06 | 0x4303 => gl::ONE_MINUS_SRC_ALPHA,
        0x07 | 0x4304 => gl::DST_ALPHA,
        0x08 | 0x4305 => gl::ONE_MINUS_DST_ALPHA,
        0x09 | 0x4306 => gl::DST_COLOR,
        0x0A | 0x4307 => gl::ONE_MINUS_DST_COLOR,
        0x0B | 0x4308 => gl::SRC_ALPHA_SATURATE,
        // The D3D half of upstream's `Blend::Factor` runs
        // BothSourceAlpha(0xC), OneMinusBothSourceAlpha(0xD),
        // BlendFactor(0xE), OneMinusBlendFactor(0xF), then Source1*(0x10..0x13)
        // — and each pairs with the GL-style value that maps to the same GL
        // enum, not with the one that happens to follow it numerically.
        0x0C | 0xC003 => gl::CONSTANT_ALPHA,
        0x0D | 0xC004 => gl::ONE_MINUS_CONSTANT_ALPHA,
        0x0E | 0xC001 => gl::CONSTANT_COLOR,
        0x0F | 0xC002 => gl::ONE_MINUS_CONSTANT_COLOR,
        0x10 | 0xC900 => gl::SRC1_COLOR,
        0x11 | 0xC901 => gl::ONE_MINUS_SRC1_COLOR,
        0x12 | 0xC902 => gl::SRC1_ALPHA,
        0x13 | 0xC903 => gl::ONE_MINUS_SRC1_ALPHA,
        _ => {
            log::warn!("Unimplemented blend factor: {:#x}", factor);
            gl::ZERO
        }
    }
}

/// Map texture filter + mipmap filter to a combined GL filter mode.
///
/// Corresponds to `OpenGL::MaxwellToGL::TextureFilterMode()`.
pub fn texture_filter_mode(filter: u32, mipmap_filter: u32) -> u32 {
    match filter {
        // Nearest
        1 => match mipmap_filter {
            1 => gl::NEAREST, // None
            2 => gl::NEAREST_MIPMAP_NEAREST,
            3 => gl::NEAREST_MIPMAP_LINEAR,
            _ => {
                log::warn!("Invalid mipmap filter mode: {}", mipmap_filter);
                gl::NEAREST
            }
        },
        // Linear
        2 => match mipmap_filter {
            1 => gl::LINEAR, // None
            2 => gl::LINEAR_MIPMAP_NEAREST,
            3 => gl::LINEAR_MIPMAP_LINEAR,
            _ => {
                log::warn!("Invalid mipmap filter mode: {}", mipmap_filter);
                // Eden falls through the outer switch and returns GL_NEAREST.
                gl::NEAREST
            }
        },
        _ => {
            log::warn!("Invalid texture filter mode: {}", filter);
            gl::NEAREST
        }
    }
}

/// Map a Maxwell wrap mode to GL wrap mode.
///
/// Corresponds to `OpenGL::MaxwellToGL::WrapMode()`.
pub fn wrap_mode(mode: u32) -> u32 {
    wrap_mode_with_texture_mirror_clamp(mode, has_texture_mirror_clamp())
}

const GL_MIRROR_CLAMP_EXT: u32 = 0x8742;
const GL_MIRROR_CLAMP_TO_BORDER_EXT: u32 = 0x8912;

fn wrap_mode_with_texture_mirror_clamp(mode: u32, has_texture_mirror_clamp: bool) -> u32 {
    match mode {
        0 => gl::REPEAT,
        1 => gl::MIRRORED_REPEAT,
        2 => gl::CLAMP_TO_EDGE,
        3 => gl::CLAMP_TO_BORDER,
        4 => gl::CLAMP_TO_EDGE, // GL_CLAMP (deprecated) — fallback
        5 => gl::MIRROR_CLAMP_TO_EDGE,
        6 if has_texture_mirror_clamp => GL_MIRROR_CLAMP_TO_BORDER_EXT,
        7 if has_texture_mirror_clamp => GL_MIRROR_CLAMP_EXT,
        6 | 7 => gl::MIRROR_CLAMP_TO_EDGE,
        _ => {
            log::warn!("Unimplemented texture wrap mode: {}", mode);
            gl::REPEAT
        }
    }
}

fn has_texture_mirror_clamp() -> bool {
    static HAS_TEXTURE_MIRROR_CLAMP: OnceLock<bool> = OnceLock::new();
    *HAS_TEXTURE_MIRROR_CLAMP.get_or_init(|| unsafe {
        let mut count = 0;
        gl::GetIntegerv(gl::NUM_EXTENSIONS, &mut count);
        (0..count as u32).any(|index| {
            let extension = gl::GetStringi(gl::EXTENSIONS, index);
            !extension.is_null()
                && CStr::from_ptr(extension.cast()).to_bytes() == b"GL_EXT_texture_mirror_clamp"
        })
    })
}

/// Map a depth compare function to GL compare function.
///
/// Corresponds to `OpenGL::MaxwellToGL::DepthCompareFunc()`.
pub fn depth_compare_func(func: u32) -> u32 {
    match func {
        0 => gl::NEVER,
        1 => gl::LESS,
        2 => gl::EQUAL,
        3 => gl::LEQUAL,
        4 => gl::GREATER,
        5 => gl::NOTEQUAL,
        6 => gl::GEQUAL,
        7 => gl::ALWAYS,
        _ => {
            log::warn!("Unimplemented depth compare func: {}", func);
            gl::GREATER
        }
    }
}

/// Map a Maxwell vertex attribute type + size to GL type.
///
/// Corresponds to `OpenGL::MaxwellToGL::VertexFormat()`.
/// Returns the GL type constant (e.g. GL_FLOAT, GL_UNSIGNED_BYTE).
/// Upstream's `MaxwellToGL::VertexFormat` has a single failure path: it logs
/// `UNIMPLEMENTED_MSG` and returns a value-initialised `GLenum`, i.e. `GL_NONE`.
/// Substituting a plausible-looking type instead would silently reinterpret the
/// stream rather than surfacing the unhandled combination.
fn unimplemented_vertex_format(attrib_type: u32, size: u32) -> u32 {
    log::warn!("Unimplemented vertex format of type={attrib_type} and size={size:#x}");
    gl::NONE
}

pub fn vertex_format(attrib_type: u32, size: u32) -> u32 {
    match attrib_type {
        // UNorm, UScaled, UInt
        2 | 5 | 4 => match size {
            0x0A | 0x13 | 0x18 | 0x1D | 0x32 | 0x33 | 0x34 => gl::UNSIGNED_BYTE,
            0x03 | 0x05 | 0x0F | 0x1B => gl::UNSIGNED_SHORT,
            0x01 | 0x02 | 0x04 | 0x12 => gl::UNSIGNED_INT,
            0x30 => gl::UNSIGNED_INT_2_10_10_10_REV,
            _ => unimplemented_vertex_format(attrib_type, size),
        },
        // SNorm, SScaled, SInt
        1 | 6 | 3 => match size {
            0x0A | 0x13 | 0x18 | 0x1D | 0x32 | 0x33 | 0x34 => gl::BYTE,
            0x03 | 0x05 | 0x0F | 0x1B => gl::SHORT,
            0x01 | 0x02 | 0x04 | 0x12 => gl::INT,
            0x30 => gl::INT_2_10_10_10_REV,
            _ => unimplemented_vertex_format(attrib_type, size),
        },
        // Float
        7 => match size {
            0x03 | 0x05 | 0x0F | 0x1B => gl::HALF_FLOAT,
            0x01 | 0x02 | 0x04 | 0x12 => gl::FLOAT,
            0x31 => gl::UNSIGNED_INT_10F_11F_11F_REV,
            _ => unimplemented_vertex_format(attrib_type, size),
        },
        _ => unimplemented_vertex_format(attrib_type, size),
    }
}

/// Map a reduction filter mode to GL reduction mode.
///
/// Corresponds to `OpenGL::MaxwellToGL::ReductionFilter()`.
const GL_WEIGHTED_AVERAGE_ARB: u32 = 0x9367;

pub fn reduction_filter(filter: u32) -> u32 {
    match filter {
        0 => GL_WEIGHTED_AVERAGE_ARB,
        1 => gl::MIN,
        2 => gl::MAX,
        _ => {
            log::warn!("Invalid reduction filter: {}", filter);
            GL_WEIGHTED_AVERAGE_ARB
        }
    }
}

/// Map a viewport swizzle to GL viewport swizzle (NV extension).
///
/// Corresponds to `OpenGL::MaxwellToGL::ViewportSwizzle()`.
const GL_VIEWPORT_SWIZZLE_POSITIVE_X_NV: u32 = 0x9350;

pub fn viewport_swizzle(swizzle: u32) -> u32 {
    GL_VIEWPORT_SWIZZLE_POSITIVE_X_NV + swizzle
}

/// Map a logic operation to GL logic op.
///
/// Corresponds to `OpenGL::MaxwellToGL::LogicOp()`.
pub fn logic_op(op: u32) -> u32 {
    match op {
        0x1500 => gl::CLEAR,
        0x1501 => gl::AND,
        0x1502 => gl::AND_REVERSE,
        0x1503 => gl::COPY,
        0x1504 => gl::AND_INVERTED,
        0x1505 => gl::NOOP,
        0x1506 => gl::XOR,
        0x1507 => gl::OR,
        0x1508 => gl::NOR,
        0x1509 => gl::EQUIV,
        0x150A => gl::INVERT,
        0x150B => gl::OR_REVERSE,
        0x150C => gl::COPY_INVERTED,
        0x150D => gl::OR_INVERTED,
        0x150E => gl::NAND,
        0x150F => gl::SET,
        _ => {
            log::warn!("Unimplemented logic op: {:#x}", op);
            gl::COPY
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::PixelFormat;

    #[test]
    fn format_table_covers_every_upstream_pixel_format() {
        assert_eq!(
            FORMAT_TABLE.len(),
            PixelFormat::MaxDepthStencilFormat as usize
        );
        assert_eq!(
            get_format_tuple(PixelFormat::A8B8G8R8Unorm).internal_format,
            gl::RGBA8
        );
        assert_eq!(
            get_format_tuple(PixelFormat::B8G8R8A8Srgb).internal_format,
            gl::SRGB8_ALPHA8
        );
        assert_eq!(
            get_format_tuple(PixelFormat::A2R10G10B10Unorm).format,
            gl::BGRA
        );
        assert_eq!(
            get_format_tuple(PixelFormat::D32FloatS8Uint).gl_type,
            gl::FLOAT_32_UNSIGNED_INT_24_8_REV
        );
    }

    // Upstream initializes FORMAT_TABLE from SURFACE_FORMAT_LIST, which omits
    // ETC2/EAC. Depth/stencil tuples therefore immediately follow E5B9G9R9,
    // while the ten remaining fixed-array elements are zero-initialized.
    #[test]
    fn format_table_keeps_upstream_compact_initializer_order() {
        assert_eq!(PixelFormat::Etc2RgbUnorm as usize, 95);
        assert_eq!(PixelFormat::EacR11G11Snorm as usize, 104);
        assert_eq!(PixelFormat::D32Float as usize, 105);
        assert_eq!(PixelFormat::D32FloatS8Uint as usize, 111);

        assert_eq!(FORMAT_TABLE[95].internal_format, gl::DEPTH_COMPONENT32F);
        assert_eq!(FORMAT_TABLE[101].internal_format, gl::DEPTH32F_STENCIL8);
        assert!(FORMAT_TABLE[102..].iter().all(FormatTuple::is_unsupported));

        assert_eq!(
            get_format_tuple(PixelFormat::D32Float).internal_format,
            gl::DEPTH_COMPONENT32F
        );
        assert_eq!(
            get_format_tuple(PixelFormat::D16Unorm).internal_format,
            gl::DEPTH_COMPONENT16
        );
        assert_eq!(
            get_format_tuple(PixelFormat::S8Uint).internal_format,
            gl::STENCIL_INDEX8
        );
    }

    // Upstream's OpenGL `SURFACE_FORMAT_LIST` has no ETC2/EAC entries, so
    // `GetFormatTuple` reaches `UNREACHABLE()` for them.
    #[test]
    fn etc2_formats_have_no_opengl_tuple() {
        assert!(FORMAT_TABLE[102..].iter().all(FormatTuple::is_unsupported));
    }

    #[test]
    #[should_panic(expected = "no OpenGL representation")]
    fn get_format_tuple_rejects_etc2() {
        get_format_tuple(PixelFormat::Etc2RgbUnorm);
    }

    // Maxwell exposes every blend factor twice: a D3D-style value (0x1..0x13)
    // and a GL-style one. The two halves are not in the same order, so pairing
    // them by position silently produces a shifted table — which is what this
    // function used to carry.
    #[test]
    fn blend_func_pairs_the_d3d_and_gl_encodings_like_upstream() {
        for (d3d, gl_value, expected) in [
            (0x01u32, 0x4000u32, gl::ZERO),
            (0x02, 0x4001, gl::ONE),
            (0x0B, 0x4308, gl::SRC_ALPHA_SATURATE),
            (0x0C, 0xC003, gl::CONSTANT_ALPHA),
            (0x0D, 0xC004, gl::ONE_MINUS_CONSTANT_ALPHA),
            (0x0E, 0xC001, gl::CONSTANT_COLOR),
            (0x0F, 0xC002, gl::ONE_MINUS_CONSTANT_COLOR),
            (0x10, 0xC900, gl::SRC1_COLOR),
            (0x11, 0xC901, gl::ONE_MINUS_SRC1_COLOR),
            (0x12, 0xC902, gl::SRC1_ALPHA),
            (0x13, 0xC903, gl::ONE_MINUS_SRC1_ALPHA),
        ] {
            assert_eq!(blend_func(d3d), expected, "D3D factor {d3d:#x}");
            assert_eq!(blend_func(gl_value), expected, "GL factor {gl_value:#x}");
        }
        // 0x14 is not a Maxwell blend factor.
        assert_eq!(blend_func(0x14), gl::ZERO);
    }

    #[test]
    fn vertex_format_matches_upstream_bit_depth() {
        assert_eq!(vertex_format(4, 0x01), gl::UNSIGNED_INT);
        assert_eq!(vertex_format(4, 0x03), gl::UNSIGNED_SHORT);
        assert_eq!(vertex_format(4, 0x0A), gl::UNSIGNED_BYTE);
        assert_eq!(vertex_format(3, 0x01), gl::INT);
        assert_eq!(vertex_format(3, 0x03), gl::SHORT);
        assert_eq!(vertex_format(3, 0x0A), gl::BYTE);
        assert_eq!(vertex_format(7, 0x03), gl::HALF_FLOAT);
        assert_eq!(vertex_format(7, 0x05), gl::HALF_FLOAT);
        assert_eq!(vertex_format(7, 0x0F), gl::HALF_FLOAT);
        assert_eq!(vertex_format(7, 0x1B), gl::HALF_FLOAT);
        assert_eq!(vertex_format(7, 0x01), gl::FLOAT);
    }

    #[test]
    fn front_and_cull_face_use_upstream_gl_register_encodings() {
        assert_eq!(front_face(0x900), gl::CW);
        assert_eq!(front_face(0x901), gl::CCW);
        assert_eq!(cull_face(0x404), gl::FRONT);
        assert_eq!(cull_face(0x405), gl::BACK);
        assert_eq!(cull_face(0x408), gl::FRONT_AND_BACK);
    }

    #[test]
    fn primitive_topology_maps_all_maxwell_values() {
        assert_eq!(primitive_topology(PrimitiveTopology::Points), gl::POINTS);
        assert_eq!(primitive_topology(PrimitiveTopology::Quads), gl::QUADS);
        assert_eq!(
            primitive_topology(PrimitiveTopology::QuadStrip),
            GL_QUAD_STRIP
        );
        assert_eq!(primitive_topology(PrimitiveTopology::Polygon), GL_POLYGON);
        assert_eq!(primitive_topology(PrimitiveTopology::Patches), gl::PATCHES);
    }

    #[test]
    fn index_and_invalid_filter_mappings_match_upstream() {
        use crate::engines::maxwell_3d::IndexFormat;

        assert_eq!(index_format(IndexFormat::UnsignedByte), gl::UNSIGNED_BYTE);
        assert_eq!(index_format(IndexFormat::UnsignedShort), gl::UNSIGNED_SHORT);
        assert_eq!(index_format(IndexFormat::UnsignedInt), gl::UNSIGNED_INT);
        assert_eq!(texture_filter_mode(2, 0), gl::NEAREST);
    }

    #[test]
    fn mirror_once_wrap_modes_use_ext_variants_when_available() {
        assert_eq!(
            wrap_mode_with_texture_mirror_clamp(6, true),
            GL_MIRROR_CLAMP_TO_BORDER_EXT
        );
        assert_eq!(
            wrap_mode_with_texture_mirror_clamp(7, true),
            GL_MIRROR_CLAMP_EXT
        );
        assert_eq!(
            wrap_mode_with_texture_mirror_clamp(6, false),
            gl::MIRROR_CLAMP_TO_EDGE
        );
    }
}
