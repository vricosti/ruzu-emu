// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Software rasterizer for draw call rendering.
//!
//! Implements vertex fetching from GPU memory, viewport transform, triangle
//! rasterization with barycentric coordinate interpolation, depth testing,
//! alpha blending, scissor clipping, color write masks, and back-face culling.

use crate::engines::maxwell_3d::{
    BlendColorInfo, BlendEquation, BlendFactor, BlendInfo, ColorMaskInfo, ComparisonOp,
    ComponentType, CullFace, DepthStencilInfo, DrawCall, FrontFace, IndexFormat, PrimitiveTopology,
    RasterizerInfo, SamplerDescriptor, ScissorInfo, ShaderStageType, StencilOp, SwizzleSource,
    TextureDescriptor, TextureFilter, TextureFormat, TextureType, TicHeaderVersion,
    VertexAttribSize, VertexAttribType, WrapMode,
};
use crate::engines::Framebuffer;
use crate::shader;

/// Pre-decoded RGBA8 data for a single mip level.
struct MipLevel {
    data: Vec<u8>,
    width: u32,
    height: u32,
}

/// Cached pre-decoded RGBA8 texture data, loaded once per draw call.
struct SampledTexture {
    data: Vec<u8>,
    width: u32,
    height: u32,
    wrap_u: WrapMode,
    wrap_v: WrapMode,
    normalized_coords: bool,
    mag_filter: TextureFilter,
    /// Additional mip levels (level 1+). Level 0 is `data`/`width`/`height`.
    mip_levels: Vec<MipLevel>,
}

/// Return bytes per texel for supported uncompressed formats, or `None` for
/// compressed / unsupported formats.
fn bytes_per_texel(fmt: TextureFormat) -> Option<u32> {
    match fmt {
        TextureFormat::A8B8G8R8 | TextureFormat::R8G8B8A8 | TextureFormat::A2B10G10R10 => Some(4),
        TextureFormat::B5G6R5 => Some(2),
        TextureFormat::R8G8 => Some(2),
        TextureFormat::R8 => Some(1),
        TextureFormat::R16 => Some(2),
        TextureFormat::R16G16 => Some(4),
        TextureFormat::R16G16B16A16 => Some(8),
        TextureFormat::R32 => Some(4),
        TextureFormat::R32G32 => Some(8),
        TextureFormat::R32G32B32A32 => Some(16),
        TextureFormat::B10G11R11 => Some(4),
        TextureFormat::R16G16B16X16 => Some(8),
        TextureFormat::R32G32B32 => Some(12),
        _ => None,
    }
}

/// Convert a half-precision f16 (u16) to f32.
fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) & 1) as u32;
    let exp = ((h >> 10) & 0x1F) as u32;
    let mant = (h & 0x3FF) as u32;
    if exp == 0 {
        if mant == 0 {
            f32::from_bits(sign << 31)
        } else {
            // Denormalized
            let mut m = mant;
            let mut e = 0i32;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            m &= 0x3FF;
            let f_exp = (127 - 15 + 1 + e) as u32;
            f32::from_bits((sign << 31) | (f_exp << 23) | (m << 13))
        }
    } else if exp == 31 {
        if mant == 0 {
            f32::from_bits((sign << 31) | (0xFF << 23))
        } else {
            f32::NAN
        }
    } else {
        let f_exp = exp + 127 - 15;
        f32::from_bits((sign << 31) | (f_exp << 23) | (mant << 13))
    }
}

/// Decode a R11G11B10 packed float value to [f32; 3].
fn decode_r11g11b10_float(val: u32) -> [f32; 3] {
    // R11: bits[10:0] — 5-bit exp, 6-bit mant
    let r_bits = val & 0x7FF;
    let r_exp = (r_bits >> 6) & 0x1F;
    let r_mant = r_bits & 0x3F;
    let r = if r_exp == 0 {
        (r_mant as f32) / 64.0 / 16384.0
    } else if r_exp == 31 {
        f32::INFINITY
    } else {
        (1.0 + r_mant as f32 / 64.0) * 2.0f32.powi(r_exp as i32 - 15)
    };
    // G11: bits[21:11]
    let g_bits = (val >> 11) & 0x7FF;
    let g_exp = (g_bits >> 6) & 0x1F;
    let g_mant = g_bits & 0x3F;
    let g = if g_exp == 0 {
        (g_mant as f32) / 64.0 / 16384.0
    } else if g_exp == 31 {
        f32::INFINITY
    } else {
        (1.0 + g_mant as f32 / 64.0) * 2.0f32.powi(g_exp as i32 - 15)
    };
    // B10: bits[31:22] — 5-bit exp, 5-bit mant
    let b_bits = (val >> 22) & 0x3FF;
    let b_exp = (b_bits >> 5) & 0x1F;
    let b_mant = b_bits & 0x1F;
    let b = if b_exp == 0 {
        (b_mant as f32) / 32.0 / 16384.0
    } else if b_exp == 31 {
        f32::INFINITY
    } else {
        (1.0 + b_mant as f32 / 32.0) * 2.0f32.powi(b_exp as i32 - 15)
    };
    [r, g, b]
}

/// Decode raw bytes of a single texel into RGBA8.
fn decode_texel(bytes: &[u8], fmt: TextureFormat, comp_type: ComponentType) -> Option<[u8; 4]> {
    match fmt {
        TextureFormat::A8B8G8R8 => {
            if comp_type == ComponentType::SNorm {
                // SNorm: interpret as signed and map [-128..127] to [0..255]
                let r = ((bytes[0] as i8 as f32 / 127.0).clamp(-1.0, 1.0) * 0.5 + 0.5) * 255.0;
                let g = ((bytes[1] as i8 as f32 / 127.0).clamp(-1.0, 1.0) * 0.5 + 0.5) * 255.0;
                let b = ((bytes[2] as i8 as f32 / 127.0).clamp(-1.0, 1.0) * 0.5 + 0.5) * 255.0;
                let a = ((bytes[3] as i8 as f32 / 127.0).clamp(-1.0, 1.0) * 0.5 + 0.5) * 255.0;
                Some([r as u8, g as u8, b as u8, a as u8])
            } else {
                Some([bytes[0], bytes[1], bytes[2], bytes[3]])
            }
        }
        TextureFormat::R8G8B8A8 => Some([bytes[3], bytes[2], bytes[1], bytes[0]]),
        TextureFormat::B5G6R5 => {
            let val = u16::from_le_bytes([bytes[0], bytes[1]]);
            let b = (val & 0x1F) as u8;
            let g = ((val >> 5) & 0x3F) as u8;
            let r = ((val >> 11) & 0x1F) as u8;
            Some([
                (r << 3) | (r >> 2),
                (g << 2) | (g >> 4),
                (b << 3) | (b >> 2),
                255,
            ])
        }
        TextureFormat::R8G8 => Some([bytes[0], bytes[1], 0, 255]),
        TextureFormat::R8 => Some([bytes[0], 0, 0, 255]),
        TextureFormat::R16 => {
            let val = u16::from_le_bytes([bytes[0], bytes[1]]);
            match comp_type {
                ComponentType::Float => {
                    let f = f16_to_f32(val);
                    Some([(f.clamp(0.0, 1.0) * 255.0).round() as u8, 0, 0, 255])
                }
                _ => {
                    // UNorm: 16-bit → 8-bit
                    Some([(val >> 8) as u8, 0, 0, 255])
                }
            }
        }
        TextureFormat::R16G16 => {
            let r = u16::from_le_bytes([bytes[0], bytes[1]]);
            let g = u16::from_le_bytes([bytes[2], bytes[3]]);
            match comp_type {
                ComponentType::Float => {
                    let rf = f16_to_f32(r);
                    let gf = f16_to_f32(g);
                    Some([
                        (rf.clamp(0.0, 1.0) * 255.0).round() as u8,
                        (gf.clamp(0.0, 1.0) * 255.0).round() as u8,
                        0,
                        255,
                    ])
                }
                _ => Some([(r >> 8) as u8, (g >> 8) as u8, 0, 255]),
            }
        }
        TextureFormat::R16G16B16A16 | TextureFormat::R16G16B16X16 => {
            let r = u16::from_le_bytes([bytes[0], bytes[1]]);
            let g = u16::from_le_bytes([bytes[2], bytes[3]]);
            let b = u16::from_le_bytes([bytes[4], bytes[5]]);
            let a = if bytes.len() >= 8 {
                u16::from_le_bytes([bytes[6], bytes[7]])
            } else {
                0xFFFF
            };
            match comp_type {
                ComponentType::Float => {
                    let rf = f16_to_f32(r);
                    let gf = f16_to_f32(g);
                    let bf = f16_to_f32(b);
                    let af = f16_to_f32(a);
                    Some([
                        (rf.clamp(0.0, 1.0) * 255.0).round() as u8,
                        (gf.clamp(0.0, 1.0) * 255.0).round() as u8,
                        (bf.clamp(0.0, 1.0) * 255.0).round() as u8,
                        (af.clamp(0.0, 1.0) * 255.0).round() as u8,
                    ])
                }
                _ => Some([
                    (r >> 8) as u8,
                    (g >> 8) as u8,
                    (b >> 8) as u8,
                    (a >> 8) as u8,
                ]),
            }
        }
        TextureFormat::R32 => {
            let val = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            Some([(val.clamp(0.0, 1.0) * 255.0).round() as u8, 0, 0, 255])
        }
        TextureFormat::R32G32 => {
            let r = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            let g = f32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
            Some([
                (r.clamp(0.0, 1.0) * 255.0).round() as u8,
                (g.clamp(0.0, 1.0) * 255.0).round() as u8,
                0,
                255,
            ])
        }
        TextureFormat::R32G32B32 => {
            let r = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            let g = f32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
            let b = f32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
            Some([
                (r.clamp(0.0, 1.0) * 255.0).round() as u8,
                (g.clamp(0.0, 1.0) * 255.0).round() as u8,
                (b.clamp(0.0, 1.0) * 255.0).round() as u8,
                255,
            ])
        }
        TextureFormat::R32G32B32A32 => {
            let r = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            let g = f32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
            let b = f32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
            let a = f32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
            Some([
                (r.clamp(0.0, 1.0) * 255.0).round() as u8,
                (g.clamp(0.0, 1.0) * 255.0).round() as u8,
                (b.clamp(0.0, 1.0) * 255.0).round() as u8,
                (a.clamp(0.0, 1.0) * 255.0).round() as u8,
            ])
        }
        TextureFormat::A2B10G10R10 => {
            let val = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            let a = ((val >> 30) & 0x3) as u8;
            // Map 10-bit to 8-bit, 2-bit to 8-bit
            Some([
                ((val & 0x3FF) >> 2) as u8,
                (((val >> 10) & 0x3FF) >> 2) as u8,
                (((val >> 20) & 0x3FF) >> 2) as u8,
                (a << 6) | (a << 4) | (a << 2) | a,
            ])
        }
        TextureFormat::B10G11R11 => {
            let val = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            let rgb = decode_r11g11b10_float(val);
            Some([
                (rgb[0].clamp(0.0, 1.0) * 255.0).round() as u8,
                (rgb[1].clamp(0.0, 1.0) * 255.0).round() as u8,
                (rgb[2].clamp(0.0, 1.0) * 255.0).round() as u8,
                255,
            ])
        }
        _ => None,
    }
}

/// Apply TIC component swizzle to an RGBA8 texel.
fn apply_swizzle(rgba: [u8; 4], swizzle: &[SwizzleSource; 4]) -> [u8; 4] {
    let mut out = [0u8; 4];
    for (i, src) in swizzle.iter().enumerate() {
        out[i] = match src {
            SwizzleSource::Zero => 0,
            SwizzleSource::R => rgba[0],
            SwizzleSource::G => rgba[1],
            SwizzleSource::B => rgba[2],
            SwizzleSource::A => rgba[3],
            SwizzleSource::OneInt | SwizzleSource::OneFloat => 255,
            SwizzleSource::Invalid => 0,
        };
    }
    out
}

/// Convert an sRGB 8-bit value to linear (approximate). Alpha is never converted.
fn srgb_to_linear(val: u8) -> u8 {
    let f = val as f32 / 255.0;
    let linear = if f <= 0.04045 {
        f / 12.92
    } else {
        ((f + 0.055) / 1.055).powf(2.4)
    };
    (linear * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Map a floating-point UV coordinate to a texel index given a wrap mode.
fn apply_wrap_mode(coord: f32, size: u32, mode: WrapMode) -> u32 {
    if size == 0 {
        return 0;
    }
    let s = size as i32;
    match mode {
        WrapMode::Wrap => {
            let i = coord.floor() as i32;
            ((i % s + s) % s) as u32
        }
        WrapMode::ClampToEdge | WrapMode::Clamp => {
            let i = coord.floor() as i32;
            i.clamp(0, s - 1) as u32
        }
        WrapMode::Mirror => {
            let period = s * 2;
            let i = coord.floor() as i32;
            let m = ((i % period) + period) % period;
            if m < s {
                m as u32
            } else {
                (period - 1 - m) as u32
            }
        }
        _ => {
            // Border, MirrorOnce variants — fall back to clamp.
            let i = coord.floor() as i32;
            i.clamp(0, s - 1) as u32
        }
    }
}

// ── BlockLinear (GOB) deswizzle constants and functions ──────────────────────

/// GOB width in bytes.
const GOB_SIZE_X: u32 = 64;
/// GOB height in pixels.
const GOB_SIZE_Y: u32 = 8;
/// Total bytes per GOB (64 × 8 = 512).
const GOB_SIZE: u32 = 512;

/// Compute intra-GOB byte offset using the Tegra X1 swizzle formula.
/// `x` is the byte offset within the GOB row (0..63), `y` is the row (0..7).
fn gob_offset(x: u32, y: u32) -> u32 {
    ((x % 64) / 32) * 256 + ((y % 8) / 2) * 64 + ((x % 32) / 16) * 32 + (y % 2) * 16 + (x % 16)
}

/// Compute the memory size (in bytes) of a BlockLinear texture, aligned to
/// block boundaries.
fn block_linear_size(width: u32, height: u32, bpt: u32, block_height: u32) -> usize {
    let aligned_width = align_up(width * bpt, GOB_SIZE_X);
    let block_height_pixels = GOB_SIZE_Y << block_height;
    let aligned_height = align_up(height, block_height_pixels);
    (aligned_width * aligned_height) as usize
}

/// Convert BlockLinear-tiled data to a linear row-major pixel buffer.
fn deswizzle_block_linear(
    raw: &[u8],
    width: u32,
    height: u32,
    bpt: u32,
    block_height: u32,
) -> Vec<u8> {
    let linear_size = (width * height * bpt) as usize;
    let mut output = vec![0u8; linear_size];

    let gobs_in_x = align_up(width * bpt, GOB_SIZE_X) / GOB_SIZE_X;
    let block_height_gobs = 1u32 << block_height;
    let block_size_bytes = gobs_in_x * GOB_SIZE * block_height_gobs;

    for y in 0..height {
        for x in 0..width {
            let x_bytes = x * bpt;
            let gob_x = x_bytes / GOB_SIZE_X;
            let gob_y = y / GOB_SIZE_Y;
            let block_y = gob_y / block_height_gobs;
            let gob_in_block = gob_y % block_height_gobs;

            let block_offset = block_y * block_size_bytes
                + gob_x * GOB_SIZE * block_height_gobs
                + gob_in_block * GOB_SIZE;
            let intra = gob_offset(x_bytes % GOB_SIZE_X, y % GOB_SIZE_Y);

            let src = (block_offset + intra) as usize;
            let dst = (y * width * bpt + x * bpt) as usize;

            if src + bpt as usize <= raw.len() && dst + bpt as usize <= output.len() {
                output[dst..dst + bpt as usize].copy_from_slice(&raw[src..src + bpt as usize]);
            }
        }
    }

    output
}

/// Round `value` up to the next multiple of `align`.
fn align_up(value: u32, align: u32) -> u32 {
    (value + align - 1) / align * align
}

// ── Texture compression decompression ────────────────────────────────────────

/// Return (block_width, block_height, bytes_per_block) for compressed formats.
fn block_format_info(fmt: TextureFormat) -> Option<(u32, u32, u32)> {
    match fmt {
        TextureFormat::Bc1Rgba => Some((4, 4, 8)),
        TextureFormat::Bc2 => Some((4, 4, 16)),
        TextureFormat::Bc3 => Some((4, 4, 16)),
        TextureFormat::Bc4 => Some((4, 4, 8)),
        TextureFormat::Bc5 => Some((4, 4, 16)),
        TextureFormat::Bc7 => Some((4, 4, 16)),
        TextureFormat::Bc6HSf16 | TextureFormat::Bc6HUf16 => Some((4, 4, 16)),
        TextureFormat::Astc2d4x4 => Some((4, 4, 16)),
        TextureFormat::Astc2d5x4 => Some((5, 4, 16)),
        TextureFormat::Astc2d5x5 => Some((5, 5, 16)),
        TextureFormat::Astc2d6x5 => Some((6, 5, 16)),
        TextureFormat::Astc2d6x6 => Some((6, 6, 16)),
        TextureFormat::Astc2d8x5 => Some((8, 5, 16)),
        TextureFormat::Astc2d8x6 => Some((8, 6, 16)),
        TextureFormat::Astc2d8x8 => Some((8, 8, 16)),
        TextureFormat::Astc2d10x5 => Some((10, 5, 16)),
        TextureFormat::Astc2d10x6 => Some((10, 6, 16)),
        TextureFormat::Astc2d10x8 => Some((10, 8, 16)),
        TextureFormat::Astc2d10x10 => Some((10, 10, 16)),
        TextureFormat::Astc2d12x10 => Some((12, 10, 16)),
        TextureFormat::Astc2d12x12 => Some((12, 12, 16)),
        _ => None,
    }
}

/// Expand RGB565 to RGB888.
fn rgb565_to_rgb888(c: u16) -> [u8; 3] {
    let r5 = ((c >> 11) & 0x1F) as u8;
    let g6 = ((c >> 5) & 0x3F) as u8;
    let b5 = (c & 0x1F) as u8;
    [
        (r5 << 3) | (r5 >> 2),
        (g6 << 2) | (g6 >> 4),
        (b5 << 3) | (b5 >> 2),
    ]
}

/// Decompress a single BC1 (DXT1) 8-byte block into 16 RGBA8 texels (4x4).
fn decompress_bc1_block(block: &[u8; 8]) -> [[u8; 4]; 16] {
    let c0_raw = u16::from_le_bytes([block[0], block[1]]);
    let c1_raw = u16::from_le_bytes([block[2], block[3]]);
    let c0 = rgb565_to_rgb888(c0_raw);
    let c1 = rgb565_to_rgb888(c1_raw);

    let mut palette = [[0u8; 4]; 4];
    palette[0] = [c0[0], c0[1], c0[2], 255];
    palette[1] = [c1[0], c1[1], c1[2], 255];

    if c0_raw > c1_raw {
        // 4-color mode: 1/3, 2/3 interpolation.
        for i in 0..3 {
            palette[2][i] = ((2 * c0[i] as u16 + c1[i] as u16 + 1) / 3) as u8;
            palette[3][i] = ((c0[i] as u16 + 2 * c1[i] as u16 + 1) / 3) as u8;
        }
        palette[2][3] = 255;
        palette[3][3] = 255;
    } else {
        // 3-color + transparent mode.
        for i in 0..3 {
            palette[2][i] = ((c0[i] as u16 + c1[i] as u16) / 2) as u8;
        }
        palette[2][3] = 255;
        palette[3] = [0, 0, 0, 0]; // transparent black
    }

    let mut result = [[0u8; 4]; 16];
    for row in 0..4u32 {
        let bits = block[4 + row as usize];
        for col in 0..4u32 {
            let idx = ((bits >> (col * 2)) & 0x3) as usize;
            result[(row * 4 + col) as usize] = palette[idx];
        }
    }
    result
}

/// Decode BC3/BC4-style interpolated alpha block (8 bytes) into 16 alpha values.
fn decode_alpha_block(block: &[u8; 8]) -> [u8; 16] {
    let a0 = block[0];
    let a1 = block[1];

    let mut palette = [0u8; 8];
    palette[0] = a0;
    palette[1] = a1;

    if a0 > a1 {
        for i in 1..7u16 {
            palette[(i + 1) as usize] = (((7 - i) * a0 as u16 + i * a1 as u16 + 3) / 7) as u8;
        }
    } else {
        for i in 1..5u16 {
            palette[(i + 1) as usize] = (((5 - i) * a0 as u16 + i * a1 as u16 + 2) / 5) as u8;
        }
        palette[6] = 0;
        palette[7] = 255;
    }

    // Decode 48 bits of 3-bit indices from bytes 2..8.
    let mut bits: u64 = 0;
    for i in 0..6 {
        bits |= (block[2 + i] as u64) << (i * 8);
    }

    let mut result = [0u8; 16];
    for i in 0..16 {
        let idx = ((bits >> (i * 3)) & 0x7) as usize;
        result[i] = palette[idx];
    }
    result
}

/// Decompress a single BC2 (DXT3) 16-byte block into 16 RGBA8 texels.
fn decompress_bc2_block(block: &[u8; 16]) -> [[u8; 4]; 16] {
    // First 8 bytes: explicit 4-bit alpha per texel.
    // Last 8 bytes: BC1 color block.
    let color_block: [u8; 8] = block[8..16].try_into().unwrap();
    let mut result = decompress_bc1_block(&color_block);

    // Override alpha with explicit 4-bit values.
    for i in 0..16 {
        let byte_idx = i / 2;
        let nibble = if i % 2 == 0 {
            block[byte_idx] & 0x0F
        } else {
            (block[byte_idx] >> 4) & 0x0F
        };
        result[i][3] = nibble | (nibble << 4);
    }
    result
}

/// Decompress a single BC3 (DXT5) 16-byte block into 16 RGBA8 texels.
fn decompress_bc3_block(block: &[u8; 16]) -> [[u8; 4]; 16] {
    let alpha_block: [u8; 8] = block[0..8].try_into().unwrap();
    let color_block: [u8; 8] = block[8..16].try_into().unwrap();
    let alphas = decode_alpha_block(&alpha_block);
    let mut result = decompress_bc1_block(&color_block);
    for i in 0..16 {
        result[i][3] = alphas[i];
    }
    result
}

/// Decompress a single BC4 (single-channel) 8-byte block into 16 RGBA8 texels.
fn decompress_bc4_block(block: &[u8; 8]) -> [[u8; 4]; 16] {
    let alpha_block: [u8; 8] = *block;
    let values = decode_alpha_block(&alpha_block);
    let mut result = [[0u8; 4]; 16];
    for i in 0..16 {
        result[i] = [values[i], 0, 0, 255];
    }
    result
}

/// Decompress a single BC5 (two-channel) 16-byte block into 16 RGBA8 texels.
fn decompress_bc5_block(block: &[u8; 16]) -> [[u8; 4]; 16] {
    let r_block: [u8; 8] = block[0..8].try_into().unwrap();
    let g_block: [u8; 8] = block[8..16].try_into().unwrap();
    let r_values = decode_alpha_block(&r_block);
    let g_values = decode_alpha_block(&g_block);
    let mut result = [[0u8; 4]; 16];
    for i in 0..16 {
        result[i] = [r_values[i], g_values[i], 0, 255];
    }
    result
}

/// Decompress a single BC7 16-byte block into 16 RGBA8 texels.
/// Supports modes 1, 3, and 6. Other modes return magenta error color.
fn decompress_bc7_block(block: &[u8; 16]) -> [[u8; 4]; 16] {
    let magenta = [[255u8, 0, 255, 255]; 16];

    // Read bits from block as a 128-bit value (little-endian).
    let mut bits: u128 = 0;
    for i in 0..16 {
        bits |= (block[i] as u128) << (i * 8);
    }

    // Determine mode from leading bits.
    let mode;
    let mut bit_pos: u32;
    if bits & 1 != 0 {
        mode = 0;
        bit_pos = 1;
    } else if bits & 2 != 0 {
        mode = 1;
        bit_pos = 2;
    } else if bits & 4 != 0 {
        mode = 2;
        bit_pos = 3;
    } else if bits & 8 != 0 {
        mode = 3;
        bit_pos = 4;
    } else if bits & 16 != 0 {
        mode = 4;
        bit_pos = 5;
    } else if bits & 32 != 0 {
        mode = 5;
        bit_pos = 6;
    } else if bits & 64 != 0 {
        mode = 6;
        bit_pos = 7;
    } else if bits & 128 != 0 {
        mode = 7;
        bit_pos = 8;
    } else {
        return magenta; // reserved (void) block
    }

    let read_bits = |pos: &mut u32, count: u32| -> u32 {
        if count == 0 {
            return 0;
        }
        let val = ((bits >> *pos) & ((1u128 << count) - 1)) as u32;
        *pos += count;
        val
    };

    match mode {
        1 => {
            // Mode 1: 2 subsets, 6-bit color, 6-bit alpha, 1-bit partition
            let partition = read_bits(&mut bit_pos, 6);

            // 6 color endpoints (3 pairs for 2 subsets, but mode 1 has 2 subsets × 2 endpoints × RGB)
            let mut ep_r = [0u32; 4];
            let mut ep_g = [0u32; 4];
            let mut ep_b = [0u32; 4];
            for i in 0..4 {
                ep_r[i] = read_bits(&mut bit_pos, 6);
            }
            for i in 0..4 {
                ep_g[i] = read_bits(&mut bit_pos, 6);
            }
            for i in 0..4 {
                ep_b[i] = read_bits(&mut bit_pos, 6);
            }
            // Shared P-bit per subset.
            let p0 = read_bits(&mut bit_pos, 1);
            let p1 = read_bits(&mut bit_pos, 1);

            // Expand endpoints: 6 bits + p-bit = 7 bits, then replicate top bit.
            let expand7 = |v: u32, p: u32| -> u8 {
                let v7 = (v << 1) | p;
                ((v7 << 1) | (v7 >> 6)) as u8
            };

            let mut endpoints = [[[0u8; 4]; 2]; 2];
            for e in 0..2 {
                let p = if e < 2 { [p0, p0] } else { [p1, p1] };
                let _ = p; // both endpoints in a subset share the same p-bit
            }
            let ps = [p0, p0, p1, p1];
            for i in 0..4 {
                endpoints[i / 2][i % 2] = [
                    expand7(ep_r[i], ps[i]),
                    expand7(ep_g[i], ps[i]),
                    expand7(ep_b[i], ps[i]),
                    255,
                ];
            }

            // Read 3-bit indices (46 bits total: anchor texels get 2 bits).
            let partition_table = bc7_partition_table_2(partition as usize);
            let anchors = bc7_anchor_indices_2(partition as usize);

            let mut result = [[0u8; 4]; 16];
            for i in 0..16 {
                let subset = partition_table[i] as usize;
                let is_anchor = i == 0 || i == anchors[subset];
                let idx_bits = if is_anchor { 2 } else { 3 };
                let idx = read_bits(&mut bit_pos, idx_bits);
                let w = bc7_interpolation_weight(3, idx);
                let ep0 = endpoints[subset][0];
                let ep1 = endpoints[subset][1];
                for c in 0..3 {
                    result[i][c] = (((64 - w) * ep0[c] as u32 + w * ep1[c] as u32 + 32) / 64) as u8;
                }
                result[i][3] = 255;
            }
            result
        }
        3 => {
            // Mode 3: 2 subsets, 7-bit color, no alpha, unique p-bits
            let partition = read_bits(&mut bit_pos, 6);
            let mut ep_r = [0u32; 4];
            let mut ep_g = [0u32; 4];
            let mut ep_b = [0u32; 4];
            for i in 0..4 {
                ep_r[i] = read_bits(&mut bit_pos, 7);
            }
            for i in 0..4 {
                ep_g[i] = read_bits(&mut bit_pos, 7);
            }
            for i in 0..4 {
                ep_b[i] = read_bits(&mut bit_pos, 7);
            }
            // 4 unique p-bits (one per endpoint).
            let mut p_bits = [0u32; 4];
            for i in 0..4 {
                p_bits[i] = read_bits(&mut bit_pos, 1);
            }

            let expand8 = |v: u32, p: u32| -> u8 { ((v << 1) | p) as u8 };

            let mut endpoints = [[[0u8; 4]; 2]; 2];
            for i in 0..4 {
                endpoints[i / 2][i % 2] = [
                    expand8(ep_r[i], p_bits[i]),
                    expand8(ep_g[i], p_bits[i]),
                    expand8(ep_b[i], p_bits[i]),
                    255,
                ];
            }

            let partition_table = bc7_partition_table_2(partition as usize);
            let anchors = bc7_anchor_indices_2(partition as usize);

            let mut result = [[0u8; 4]; 16];
            for i in 0..16 {
                let subset = partition_table[i] as usize;
                let is_anchor = i == 0 || i == anchors[subset];
                let idx_bits = if is_anchor { 1 } else { 2 };
                let idx = read_bits(&mut bit_pos, idx_bits);
                let w = bc7_interpolation_weight(2, idx);
                let ep0 = endpoints[subset][0];
                let ep1 = endpoints[subset][1];
                for c in 0..3 {
                    result[i][c] = (((64 - w) * ep0[c] as u32 + w * ep1[c] as u32 + 32) / 64) as u8;
                }
                result[i][3] = 255;
            }
            result
        }
        6 => {
            // Mode 6: 1 subset, 7+1 color bits, 7+1 alpha bits, 4-bit indices
            // No partition bits.
            let mut ep_r = [0u32; 2];
            let mut ep_g = [0u32; 2];
            let mut ep_b = [0u32; 2];
            let mut ep_a = [0u32; 2];
            for i in 0..2 {
                ep_r[i] = read_bits(&mut bit_pos, 7);
            }
            for i in 0..2 {
                ep_g[i] = read_bits(&mut bit_pos, 7);
            }
            for i in 0..2 {
                ep_b[i] = read_bits(&mut bit_pos, 7);
            }
            for i in 0..2 {
                ep_a[i] = read_bits(&mut bit_pos, 7);
            }
            // Unique p-bits per endpoint.
            let p0 = read_bits(&mut bit_pos, 1);
            let p1 = read_bits(&mut bit_pos, 1);

            let expand8 = |v: u32, p: u32| -> u8 { ((v << 1) | p) as u8 };

            let endpoints = [
                [
                    expand8(ep_r[0], p0),
                    expand8(ep_g[0], p0),
                    expand8(ep_b[0], p0),
                    expand8(ep_a[0], p0),
                ],
                [
                    expand8(ep_r[1], p1),
                    expand8(ep_g[1], p1),
                    expand8(ep_b[1], p1),
                    expand8(ep_a[1], p1),
                ],
            ];

            let mut result = [[0u8; 4]; 16];
            for i in 0..16 {
                let idx_bits = if i == 0 { 3 } else { 4 };
                let idx = read_bits(&mut bit_pos, idx_bits);
                let w = bc7_interpolation_weight(4, idx);
                for c in 0..4 {
                    result[i][c] = (((64 - w) * endpoints[0][c] as u32
                        + w * endpoints[1][c] as u32
                        + 32)
                        / 64) as u8;
                }
            }
            result
        }
        _ => magenta, // Unsupported mode
    }
}

/// BC7 interpolation weight table.
fn bc7_interpolation_weight(index_bits: u32, index: u32) -> u32 {
    match index_bits {
        2 => [0, 21, 43, 64][index as usize],
        3 => [0, 9, 18, 27, 37, 46, 55, 64][index as usize],
        4 => [0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64][index as usize],
        _ => 0,
    }
}

/// BC7 2-subset partition table (64 partitions × 16 texels).
/// Returns subset index (0 or 1) for each texel.
fn bc7_partition_table_2(partition: usize) -> [u8; 16] {
    // Standard BC7 2-subset partition table.
    const TABLE: [[u8; 16]; 64] = [
        [0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1],
        [0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1],
        [0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1],
        [0, 0, 0, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 1, 1, 1],
        [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 1, 1],
        [0, 0, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 1, 1, 1, 1],
        [0, 0, 0, 1, 0, 0, 1, 1, 0, 1, 1, 1, 1, 1, 1, 1],
        [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 1, 1, 0, 1, 1, 1],
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 1, 1],
        [0, 0, 1, 1, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1],
        [0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 1, 1, 1, 1, 1, 1],
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 1, 1],
        [0, 0, 0, 1, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1],
        [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1],
        [0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1],
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1],
        [0, 0, 0, 0, 1, 0, 0, 0, 1, 1, 1, 0, 1, 1, 1, 1],
        [0, 1, 1, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0],
        [0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 1, 1, 0],
        [0, 1, 1, 1, 0, 0, 1, 1, 0, 0, 0, 1, 0, 0, 0, 0],
        [0, 0, 1, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0],
        [0, 0, 0, 0, 1, 0, 0, 0, 1, 1, 0, 0, 1, 1, 1, 0],
        [0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 1, 0, 0],
        [0, 1, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 0, 1],
        [0, 0, 1, 1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0],
        [0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 1, 0, 0],
        [0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0],
        [0, 0, 1, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 1, 0, 0],
        [0, 0, 0, 1, 0, 1, 1, 1, 1, 1, 1, 0, 1, 0, 0, 0],
        [0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0],
        [0, 1, 1, 1, 0, 0, 0, 1, 1, 0, 0, 0, 1, 1, 1, 0],
        [0, 0, 1, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 1, 0, 0],
        [0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1],
        [0, 0, 0, 0, 1, 1, 1, 1, 0, 0, 0, 0, 1, 1, 1, 1],
        [0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0],
        [0, 0, 1, 1, 0, 0, 1, 1, 1, 1, 0, 0, 1, 1, 0, 0],
        [0, 0, 1, 1, 1, 1, 0, 0, 0, 0, 1, 1, 1, 1, 0, 0],
        [0, 1, 0, 1, 0, 1, 0, 1, 1, 0, 1, 0, 1, 0, 1, 0],
        [0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1],
        [0, 1, 0, 1, 1, 0, 1, 0, 1, 0, 1, 0, 0, 1, 0, 1],
        [0, 1, 1, 1, 0, 0, 1, 1, 1, 1, 0, 0, 1, 1, 1, 0],
        [0, 0, 0, 1, 0, 0, 1, 1, 1, 1, 0, 0, 1, 0, 0, 0],
        [0, 0, 1, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1, 1, 0, 0],
        [0, 0, 1, 1, 1, 0, 1, 1, 1, 1, 0, 1, 1, 1, 0, 0],
        [0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0],
        [0, 0, 1, 1, 1, 1, 0, 0, 1, 1, 0, 0, 0, 0, 1, 1],
        [0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1],
        [0, 0, 0, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 0, 0, 0],
        [0, 1, 0, 0, 1, 1, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0],
        [0, 0, 1, 0, 0, 1, 1, 1, 0, 0, 1, 0, 0, 0, 0, 0],
        [0, 0, 0, 0, 0, 0, 1, 0, 0, 1, 1, 1, 0, 0, 1, 0],
        [0, 0, 0, 0, 0, 1, 0, 0, 1, 1, 1, 0, 0, 1, 0, 0],
        [0, 1, 1, 0, 1, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1, 1],
        [0, 0, 1, 1, 0, 1, 1, 0, 1, 1, 0, 0, 1, 0, 0, 1],
        [0, 1, 1, 0, 0, 0, 1, 1, 1, 0, 0, 1, 1, 1, 0, 0],
        [0, 0, 1, 1, 1, 0, 0, 1, 1, 1, 0, 0, 0, 1, 1, 0],
        [0, 1, 1, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 0, 0, 1],
        [0, 1, 1, 0, 0, 0, 1, 1, 0, 0, 1, 1, 1, 0, 0, 1],
        [0, 1, 1, 1, 1, 1, 1, 0, 1, 0, 0, 0, 0, 0, 0, 1],
        [0, 0, 0, 1, 1, 0, 0, 0, 1, 1, 1, 0, 0, 1, 1, 1],
        [0, 0, 0, 0, 1, 1, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1],
        [0, 0, 1, 1, 0, 0, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0],
        [0, 0, 1, 0, 0, 0, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0],
        [0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 1, 1, 0, 1, 1, 1],
    ];
    TABLE[partition % 64]
}

/// Return second anchor index for 2-subset BC7 partitions.
fn bc7_anchor_indices_2(partition: usize) -> [usize; 2] {
    // First subset anchor is always 0. Second subset anchor:
    const ANCHOR2: [usize; 64] = [
        15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 2, 8, 2, 2, 8, 8, 15,
        2, 8, 2, 2, 8, 8, 2, 2, 15, 15, 6, 8, 2, 8, 15, 15, 2, 8, 2, 2, 2, 15, 15, 6, 6, 2, 6, 8,
        15, 15, 2, 2, 15, 15, 15, 15, 15, 2, 2, 15,
    ];
    [0, ANCHOR2[partition % 64]]
}

/// Decompress ASTC block (void-extent only, other blocks return magenta).
fn decompress_astc_block(block: &[u8; 16], block_w: u32, block_h: u32) -> Vec<[u8; 4]> {
    let count = (block_w * block_h) as usize;

    // Check for void-extent block: first 9 bits == 0x1FC (bits[8:0] = 111111100).
    let first_bits = u16::from_le_bytes([block[0], block[1]]);
    if (first_bits & 0x1FF) == 0x1FC {
        // Void-extent: solid color stored at bytes 8..16 as RGBA16 (we take high bytes).
        let r = block[9]; // high byte of R16
        let g = block[11];
        let b = block[13];
        let a = block[15];
        return vec![[r, g, b, a]; count];
    }

    // Non-void-extent: return magenta for now.
    vec![[255, 0, 255, 255]; count]
}

/// Get the RGBA8 data, width, and height for a specific mip level.
/// Level 0 returns the base texture; higher levels return from `mip_levels`.
fn mip_data(tex: &SampledTexture, level: u32) -> (&[u8], u32, u32) {
    if level == 0 || tex.mip_levels.is_empty() {
        (&tex.data, tex.width, tex.height)
    } else {
        let idx = (level as usize - 1).min(tex.mip_levels.len() - 1);
        let m = &tex.mip_levels[idx];
        (&m.data, m.width, m.height)
    }
}

/// Nearest-neighbor sample from a pre-decoded texture at a given mip level.
/// Returns normalised RGBA.
fn sample_texture_nearest_lod(tex: &SampledTexture, u: f32, v: f32, lod: u32) -> [f32; 4] {
    let (data, width, height) = mip_data(tex, lod);
    let (su, sv) = if tex.normalized_coords {
        (u * width as f32, v * height as f32)
    } else {
        (u, v)
    };
    let tx = apply_wrap_mode(su, width, tex.wrap_u);
    let ty = apply_wrap_mode(sv, height, tex.wrap_v);
    let idx = (ty * width + tx) as usize * 4;
    if idx + 4 > data.len() {
        return [1.0, 1.0, 1.0, 1.0];
    }
    [
        data[idx] as f32 / 255.0,
        data[idx + 1] as f32 / 255.0,
        data[idx + 2] as f32 / 255.0,
        data[idx + 3] as f32 / 255.0,
    ]
}

/// Read a single texel from pre-decoded RGBA8 data at a given mip level.
fn read_texel_lod(tex: &SampledTexture, tx: u32, ty: u32, lod: u32) -> [f32; 4] {
    let (data, width, _height) = mip_data(tex, lod);
    let idx = (ty * width + tx) as usize * 4;
    if idx + 4 > data.len() {
        return [1.0, 1.0, 1.0, 1.0];
    }
    [
        data[idx] as f32 / 255.0,
        data[idx + 1] as f32 / 255.0,
        data[idx + 2] as f32 / 255.0,
        data[idx + 3] as f32 / 255.0,
    ]
}

/// Bilinear sample from a pre-decoded texture at a given mip level.
fn sample_texture_bilinear_lod(tex: &SampledTexture, u: f32, v: f32, lod: u32) -> [f32; 4] {
    let (_data, width, height) = mip_data(tex, lod);
    let (su, sv) = if tex.normalized_coords {
        (u * width as f32, v * height as f32)
    } else {
        (u, v)
    };
    let fx = su - 0.5;
    let fy = sv - 0.5;
    let x0 = apply_wrap_mode(fx, width, tex.wrap_u);
    let y0 = apply_wrap_mode(fy, height, tex.wrap_v);
    let x1 = apply_wrap_mode(fx + 1.0, width, tex.wrap_u);
    let y1 = apply_wrap_mode(fy + 1.0, height, tex.wrap_v);

    let t00 = read_texel_lod(tex, x0, y0, lod);
    let t10 = read_texel_lod(tex, x1, y0, lod);
    let t01 = read_texel_lod(tex, x0, y1, lod);
    let t11 = read_texel_lod(tex, x1, y1, lod);

    let frac_x = fx.fract();
    let frac_y = fy.fract();
    // Handle negative fractions.
    let frac_x = if frac_x < 0.0 { frac_x + 1.0 } else { frac_x };
    let frac_y = if frac_y < 0.0 { frac_y + 1.0 } else { frac_y };

    let mut result = [0.0f32; 4];
    for i in 0..4 {
        let top = t00[i] * (1.0 - frac_x) + t10[i] * frac_x;
        let bot = t01[i] * (1.0 - frac_x) + t11[i] * frac_x;
        result[i] = top * (1.0 - frac_y) + bot * frac_y;
    }
    result
}

/// Bilinear sample from a pre-decoded texture (level 0). Returns normalised RGBA.
#[cfg(test)]
fn sample_texture_bilinear(tex: &SampledTexture, u: f32, v: f32) -> [f32; 4] {
    sample_texture_bilinear_lod(tex, u, v, 0)
}

/// Sample from a pre-decoded texture using the configured filter mode and LOD.
fn sample_texture_lod(tex: &SampledTexture, u: f32, v: f32, lod: u32) -> [f32; 4] {
    match tex.mag_filter {
        TextureFilter::Linear => sample_texture_bilinear_lod(tex, u, v, lod),
        _ => sample_texture_nearest_lod(tex, u, v, lod),
    }
}

/// Sample from a pre-decoded texture using the configured filter mode.
fn sample_texture(tex: &SampledTexture, u: f32, v: f32) -> [f32; 4] {
    sample_texture_lod(tex, u, v, 0)
}

/// Compute the maximum number of mip levels for a texture of given dimensions.
fn max_mip_count(width: u32, height: u32) -> u32 {
    if width == 0 || height == 0 {
        return 1;
    }
    (width.max(height) as f32).log2().floor() as u32 + 1
}

/// Read UV (texture coordinate) attribute for all vertices of a draw call.
fn fetch_texture_coords(
    draw: &DrawCall,
    read_gpu: &dyn Fn(u64, &mut [u8]),
    vertex_count: usize,
) -> Option<Vec<[f32; 2]>> {
    // Heuristic: find a Float R32G32 attrib that is NOT the position attribute.
    let uv_attrib =
        draw.vertex_attribs
            .iter()
            .find(|a| {
                a.attrib_type == VertexAttribType::Float
                    && a.size == VertexAttribSize::R32G32
                    && a.buffer_index != 0
            })
            .or_else(|| {
                // Fallback: any Float attrib with 2 components, not the first attribute.
                draw.vertex_attribs.iter().skip(1).find(|a| {
                    a.attrib_type == VertexAttribType::Float && a.size.component_count() == 2
                })
            })?;

    let stream = draw
        .vertex_streams
        .iter()
        .find(|s| s.index == uv_attrib.buffer_index)?;

    if stream.stride == 0 || stream.address == 0 {
        return None;
    }

    // Re-derive vertex indices (same as fetch_positions).
    let indices: Vec<u32> = if draw.indexed {
        fetch_indices(draw, read_gpu)
    } else {
        let first = draw.vertex_first;
        let count = draw.vertex_count;
        (first..first + count).collect()
    };

    let mut uvs = Vec::with_capacity(vertex_count);
    for idx in indices.iter().take(vertex_count) {
        let addr =
            stream.address + (*idx as u64) * (stream.stride as u64) + (uv_attrib.offset as u64);
        let mut buf = [0u8; 8];
        read_gpu(addr, &mut buf);
        let u = f32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let v = f32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        uvs.push([u, v]);
    }

    // Pad if needed.
    while uvs.len() < vertex_count {
        uvs.push([0.0, 0.0]);
    }

    Some(uvs)
}

/// Load TIC/TSC entry 0, bulk-decode the texture to RGBA8.
fn load_texture(draw: &DrawCall, read_gpu: &dyn Fn(u64, &mut [u8])) -> Option<SampledTexture> {
    if draw.tex_header_pool_addr == 0 {
        return None;
    }

    // Read TIC entry 0 (32 bytes = 8 × u32).
    let mut tic_buf = [0u8; 32];
    read_gpu(draw.tex_header_pool_addr, &mut tic_buf);
    let mut tic_words = [0u32; 8];
    for (i, chunk) in tic_buf.chunks_exact(4).enumerate() {
        tic_words[i] = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    let tex_desc = TextureDescriptor::from_words(&tic_words);

    // Only support 2D textures with Pitch or BlockLinear layout.
    match tex_desc.texture_type {
        TextureType::Texture2D | TextureType::Texture2DNoMip => {}
        _ => return None,
    }

    if tex_desc.width == 0 || tex_desc.height == 0 || tex_desc.address == 0 {
        return None;
    }

    // Read TSC entry 0 (32 bytes) if sampler pool is available; otherwise use defaults.
    let (wrap_u, wrap_v, mag_filter) = if draw.tex_sampler_pool_addr != 0 {
        let mut tsc_buf = [0u8; 32];
        read_gpu(draw.tex_sampler_pool_addr, &mut tsc_buf);
        let mut tsc_words = [0u32; 8];
        for (i, chunk) in tsc_buf.chunks_exact(4).enumerate() {
            tsc_words[i] = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        let sam_desc = SamplerDescriptor::from_words(&tsc_words);
        (sam_desc.wrap_u, sam_desc.wrap_v, sam_desc.mag_filter)
    } else {
        (
            WrapMode::ClampToEdge,
            WrapMode::ClampToEdge,
            TextureFilter::Nearest,
        )
    };

    let texel_count = (tex_desc.width * tex_desc.height) as usize;
    let mut data = vec![0u8; texel_count * 4];
    let swizzle = [
        tex_desc.x_source,
        tex_desc.y_source,
        tex_desc.z_source,
        tex_desc.w_source,
    ];

    if let Some((bw, bh, bpb)) = block_format_info(tex_desc.format) {
        // ── Compressed texture path ──
        let blocks_x = (tex_desc.width + bw - 1) / bw;
        let blocks_y = (tex_desc.height + bh - 1) / bh;
        let total_block_bytes = (blocks_x * blocks_y * bpb) as usize;

        let raw = match tex_desc.header_version {
            TicHeaderVersion::Pitch => {
                let mut buf = vec![0u8; total_block_bytes];
                read_gpu(tex_desc.address, &mut buf);
                buf
            }
            TicHeaderVersion::BlockLinear => {
                // For BlockLinear compressed, treat each block as an element:
                // effective width = blocks_x, effective height = blocks_y, bpt = bpb.
                let tiled_size = block_linear_size(blocks_x, blocks_y, bpb, tex_desc.block_height);
                let mut tiled = vec![0u8; tiled_size];
                read_gpu(tex_desc.address, &mut tiled);
                deswizzle_block_linear(&tiled, blocks_x, blocks_y, bpb, tex_desc.block_height)
            }
            _ => return None,
        };

        // Decompress each block into the RGBA8 output.
        for by in 0..blocks_y {
            for bx in 0..blocks_x {
                let block_idx = (by * blocks_x + bx) as usize;
                let block_off = block_idx * bpb as usize;
                if block_off + bpb as usize > raw.len() {
                    continue;
                }
                let block_data = &raw[block_off..block_off + bpb as usize];

                let texels: Vec<[u8; 4]> = match tex_desc.format {
                    TextureFormat::Bc1Rgba => {
                        let b: [u8; 8] = block_data.try_into().unwrap();
                        decompress_bc1_block(&b).to_vec()
                    }
                    TextureFormat::Bc2 => {
                        let b: [u8; 16] = block_data.try_into().unwrap();
                        decompress_bc2_block(&b).to_vec()
                    }
                    TextureFormat::Bc3 => {
                        let b: [u8; 16] = block_data.try_into().unwrap();
                        decompress_bc3_block(&b).to_vec()
                    }
                    TextureFormat::Bc4 => {
                        let b: [u8; 8] = block_data.try_into().unwrap();
                        decompress_bc4_block(&b).to_vec()
                    }
                    TextureFormat::Bc5 => {
                        let b: [u8; 16] = block_data.try_into().unwrap();
                        decompress_bc5_block(&b).to_vec()
                    }
                    TextureFormat::Bc7 => {
                        let b: [u8; 16] = block_data.try_into().unwrap();
                        decompress_bc7_block(&b).to_vec()
                    }
                    TextureFormat::Astc2d4x4
                    | TextureFormat::Astc2d5x4
                    | TextureFormat::Astc2d5x5
                    | TextureFormat::Astc2d6x5
                    | TextureFormat::Astc2d6x6
                    | TextureFormat::Astc2d8x5
                    | TextureFormat::Astc2d8x6
                    | TextureFormat::Astc2d8x8
                    | TextureFormat::Astc2d10x5
                    | TextureFormat::Astc2d10x6
                    | TextureFormat::Astc2d10x8
                    | TextureFormat::Astc2d10x10
                    | TextureFormat::Astc2d12x10
                    | TextureFormat::Astc2d12x12 => {
                        let b: [u8; 16] = block_data.try_into().unwrap();
                        decompress_astc_block(&b, bw, bh)
                    }
                    _ => {
                        // BC6H and other unsupported → magenta.
                        vec![[255, 0, 255, 255]; (bw * bh) as usize]
                    }
                };

                // Write decompressed texels into output, clipping to actual dimensions.
                for ty in 0..bh {
                    for tx in 0..bw {
                        let px = bx * bw + tx;
                        let py = by * bh + ty;
                        if px >= tex_desc.width || py >= tex_desc.height {
                            continue;
                        }
                        let texel_idx = (ty * bw + tx) as usize;
                        if texel_idx >= texels.len() {
                            continue;
                        }
                        let rgba = texels[texel_idx];
                        let swizzled = apply_swizzle(rgba, &swizzle);
                        let final_rgba = if tex_desc.srgb_conversion {
                            [
                                srgb_to_linear(swizzled[0]),
                                srgb_to_linear(swizzled[1]),
                                srgb_to_linear(swizzled[2]),
                                swizzled[3],
                            ]
                        } else {
                            swizzled
                        };
                        let out_idx = (py * tex_desc.width + px) as usize;
                        data[out_idx * 4] = final_rgba[0];
                        data[out_idx * 4 + 1] = final_rgba[1];
                        data[out_idx * 4 + 2] = final_rgba[2];
                        data[out_idx * 4 + 3] = final_rgba[3];
                    }
                }
            }
        }
    } else {
        // ── Uncompressed texture path ──
        let bpt = match bytes_per_texel(tex_desc.format) {
            Some(b) => b,
            None => return None,
        };

        let raw = match tex_desc.header_version {
            TicHeaderVersion::Pitch => {
                let total_bytes = (tex_desc.width * tex_desc.height * bpt) as usize;
                let mut buf = vec![0u8; total_bytes];
                read_gpu(tex_desc.address, &mut buf);
                buf
            }
            TicHeaderVersion::BlockLinear => {
                let tiled_size =
                    block_linear_size(tex_desc.width, tex_desc.height, bpt, tex_desc.block_height);
                let mut tiled = vec![0u8; tiled_size];
                read_gpu(tex_desc.address, &mut tiled);
                deswizzle_block_linear(
                    &tiled,
                    tex_desc.width,
                    tex_desc.height,
                    bpt,
                    tex_desc.block_height,
                )
            }
            _ => return None,
        };

        for i in 0..texel_count {
            let byte_off = i * bpt as usize;
            let end = byte_off + bpt as usize;
            if end > raw.len() {
                break;
            }
            let texel_bytes = &raw[byte_off..end];
            let rgba = match decode_texel(texel_bytes, tex_desc.format, tex_desc.r_type) {
                Some(c) => c,
                None => [255, 255, 255, 255],
            };
            let swizzled = apply_swizzle(rgba, &swizzle);
            let final_rgba = if tex_desc.srgb_conversion {
                [
                    srgb_to_linear(swizzled[0]),
                    srgb_to_linear(swizzled[1]),
                    srgb_to_linear(swizzled[2]),
                    swizzled[3],
                ]
            } else {
                swizzled
            };
            data[i * 4] = final_rgba[0];
            data[i * 4 + 1] = final_rgba[1];
            data[i * 4 + 2] = final_rgba[2];
            data[i * 4 + 3] = final_rgba[3];
        }
    }

    // Load additional mip levels (up to 8) for uncompressed pitch textures.
    let mut mip_levels = Vec::new();
    if tex_desc.max_mip_level > 0 && tex_desc.header_version == TicHeaderVersion::Pitch {
        if let Some(bpt) = bytes_per_texel(tex_desc.format) {
            let level_count = tex_desc
                .max_mip_level
                .min(7)
                .min(max_mip_count(tex_desc.width, tex_desc.height).saturating_sub(1));
            // Compute base level byte size to find the start of level 1.
            let mut mip_addr = tex_desc.address + (tex_desc.width * tex_desc.height * bpt) as u64;
            let mut mip_w = tex_desc.width / 2;
            let mut mip_h = tex_desc.height / 2;
            for _ in 0..level_count {
                if mip_w == 0 {
                    mip_w = 1;
                }
                if mip_h == 0 {
                    mip_h = 1;
                }
                let texel_count = (mip_w * mip_h) as usize;
                let byte_count = texel_count * bpt as usize;
                let mut raw = vec![0u8; byte_count];
                read_gpu(mip_addr, &mut raw);
                let mut mip_data = vec![0u8; texel_count * 4];
                for i in 0..texel_count {
                    let off = i * bpt as usize;
                    let end = off + bpt as usize;
                    if end > raw.len() {
                        break;
                    }
                    let rgba = match decode_texel(&raw[off..end], tex_desc.format, tex_desc.r_type)
                    {
                        Some(c) => c,
                        None => [255, 255, 255, 255],
                    };
                    let swizzled = apply_swizzle(rgba, &swizzle);
                    let final_rgba = if tex_desc.srgb_conversion {
                        [
                            srgb_to_linear(swizzled[0]),
                            srgb_to_linear(swizzled[1]),
                            srgb_to_linear(swizzled[2]),
                            swizzled[3],
                        ]
                    } else {
                        swizzled
                    };
                    mip_data[i * 4] = final_rgba[0];
                    mip_data[i * 4 + 1] = final_rgba[1];
                    mip_data[i * 4 + 2] = final_rgba[2];
                    mip_data[i * 4 + 3] = final_rgba[3];
                }
                mip_levels.push(MipLevel {
                    data: mip_data,
                    width: mip_w,
                    height: mip_h,
                });
                mip_addr += byte_count as u64;
                mip_w /= 2;
                mip_h /= 2;
            }
        }
    }

    Some(SampledTexture {
        data,
        width: tex_desc.width,
        height: tex_desc.height,
        wrap_u,
        wrap_v,
        normalized_coords: tex_desc.normalized_coords,
        mag_filter,
        mip_levels,
    })
}

/// Stateless software rasterizer. All state comes from `DrawCall` parameters.
pub struct SoftwareRasterizer;

impl SoftwareRasterizer {
    /// Render draw calls onto a framebuffer.
    ///
    /// If `base_framebuffer` is provided, draws on top of it. Otherwise creates
    /// a blank framebuffer from the first draw call's render target dimensions.
    /// Returns `None` only if there are no draw calls and no base framebuffer.
    pub fn render_draw_calls(
        draws: &[DrawCall],
        read_gpu: &dyn Fn(u64, &mut [u8]),
        write_gpu: &dyn Fn(u64, &[u8]),
        base_framebuffer: Option<Framebuffer>,
    ) -> Option<Framebuffer> {
        if draws.is_empty() {
            return base_framebuffer;
        }

        // Determine framebuffer dimensions.
        let (fb_width, fb_height, gpu_va, mut pixels) = if let Some(fb) = base_framebuffer {
            let w = fb.width;
            let h = fb.height;
            let va = fb.gpu_va;
            (w, h, va, fb.pixels)
        } else {
            // Use first draw call's render target 0.
            let rt = &draws[0].render_targets[0];
            let w = if rt.width > 0 { rt.width } else { 1280 };
            let h = if rt.height > 0 { rt.height } else { 720 };
            let size = (w * h * 4) as usize;
            (w, h, rt.address, vec![0u8; size])
        };

        if fb_width == 0 || fb_height == 0 {
            return None;
        }

        let mut depth_buf = vec![1.0f32; (fb_width * fb_height) as usize];
        let mut stencil_buf = vec![0u8; (fb_width * fb_height) as usize];

        for draw in draws {
            // Check if vertex shader (slot 1 = VertexB) is enabled.
            let has_vs = draw.shader_stages[1].enabled
                && draw.shader_stages[1].program_type == ShaderStageType::VertexB;
            let has_fs = draw.shader_stages[5].enabled
                && draw.shader_stages[5].program_type == ShaderStageType::Fragment;

            if has_vs {
                // ── Shader-driven rendering path ─────────────────────
                render_draw_shader(
                    draw,
                    read_gpu,
                    has_fs,
                    &mut pixels,
                    &mut depth_buf,
                    &mut stencil_buf,
                    fb_width,
                    fb_height,
                );
            } else {
                // ── Fixed-function fallback path ─────────────────────
                render_draw_fixed_function(
                    draw,
                    read_gpu,
                    &mut pixels,
                    &mut depth_buf,
                    &mut stencil_buf,
                    fb_width,
                    fb_height,
                );
            }
        }

        // Write rendered pixels back to GPU memory.
        if gpu_va != 0 {
            write_gpu(gpu_va, &pixels);
        }

        Some(Framebuffer {
            gpu_va,
            width: fb_width,
            height: fb_height,
            pixels,
        })
    }
}

/// Shader-driven rendering for a single draw call.
///
/// Runs the vertex shader for each vertex, then rasterizes triangles and runs
/// the fragment shader per pixel. Falls back to fixed-function fragment shading
/// if no fragment shader is enabled.
#[allow(clippy::too_many_arguments)]
fn render_draw_shader(
    draw: &DrawCall,
    read_gpu: &dyn Fn(u64, &mut [u8]),
    has_fs: bool,
    pixels: &mut [u8],
    depth_buf: &mut [f32],
    stencil_buf: &mut [u8],
    fb_width: u32,
    fb_height: u32,
) {
    // Load vertex shader program.
    let vs_stage = &draw.shader_stages[1];
    let vs_addr = draw.program_base_address + vs_stage.offset as u64;
    let vs_program = shader::load_shader_program(vs_addr, read_gpu, 4096);

    if vs_program.instructions.is_empty() {
        return;
    }

    // Optionally load fragment shader program.
    let fs_program = if has_fs {
        let fs_stage = &draw.shader_stages[5];
        let fs_addr = draw.program_base_address + fs_stage.offset as u64;
        let prog = shader::load_shader_program(fs_addr, read_gpu, 4096);
        if prog.instructions.is_empty() {
            None
        } else {
            Some(prog)
        }
    } else {
        None
    };

    // Load texture for fragment shader sampling.
    let texture = load_texture(draw, read_gpu);

    // Fetch all vertex attributes for all vertices.
    let all_attribs = fetch_all_vertex_attribs(draw, read_gpu);
    if all_attribs.is_empty() {
        return;
    }

    // Prepare constant buffer bindings for vertex shader stage (stage index 1).
    let vs_cb = build_cb_bindings(draw, 1);

    // Texture sampler closure.
    let tex_ref = &texture;
    let sample_tex = |_idx: u32, u: f32, v: f32| -> shader::interpreter::TexSample {
        if let Some(tex) = tex_ref {
            let rgba = sample_texture(tex, u, v);
            shader::interpreter::TexSample {
                r: rgba[0],
                g: rgba[1],
                b: rgba[2],
                a: rgba[3],
            }
        } else {
            shader::interpreter::TexSample::default()
        }
    };

    // Run vertex shader for each vertex.
    let vert_count = all_attribs.len();
    let mut vs_outputs: Vec<shader::VertexShaderOutput> = Vec::with_capacity(vert_count);

    for (vi, attribs) in all_attribs.iter().enumerate() {
        let mut ctx = shader::interpreter::ShaderExecContext::new(read_gpu, &sample_tex);
        ctx.cb_bindings = vs_cb.clone();
        ctx.input_attribs = attribs.clone();
        ctx.vertex_id = vi as u32;
        ctx.instance_id = 0;

        ctx.run(&vs_program);
        vs_outputs.push(ctx.collect_vertex_output());
    }

    // Extract positions from vertex shader outputs.
    let positions: Vec<[f32; 4]> = vs_outputs.iter().map(|o| o.position).collect();
    if positions.is_empty() {
        return;
    }

    // Process topology.
    let triangles = process_topology(&positions, draw.topology);
    let viewport = &draw.viewports[0];
    let vp_width = if viewport.width > 0.0 {
        viewport.width
    } else {
        fb_width as f32
    };
    let vp_height = if viewport.height > 0.0 {
        viewport.height
    } else {
        fb_height as f32
    };
    let vp_x = viewport.x;
    let vp_y = viewport.y;

    // Prepare fragment shader constant buffer bindings (stage index 4 for fragment).
    let fs_cb = if fs_program.is_some() {
        build_cb_bindings(draw, 4)
    } else {
        Vec::new()
    };

    for tri in &triangles {
        // Viewport transform on positions.
        let p0 = viewport_transform(positions[tri[0]], vp_x, vp_y, vp_width, vp_height);
        let p1 = viewport_transform(positions[tri[1]], vp_x, vp_y, vp_width, vp_height);
        let p2 = viewport_transform(positions[tri[2]], vp_x, vp_y, vp_width, vp_height);

        let area = signed_area(p0, p1, p2);
        if should_cull(area, &draw.rasterizer) {
            continue;
        }
        if area.abs() < 0.5 {
            continue; // Degenerate triangle.
        }

        // Collect all generic varyings from vertex shader outputs for this triangle.
        let out0 = &vs_outputs[tri[0]];
        let out1 = &vs_outputs[tri[1]];
        let out2 = &vs_outputs[tri[2]];

        if let Some(ref fs_prog) = fs_program {
            // Shader-driven fragment processing.
            rasterize_triangle_shader(
                p0,
                p1,
                p2,
                out0,
                out1,
                out2,
                fs_prog,
                &fs_cb,
                read_gpu,
                &sample_tex,
                pixels,
                depth_buf,
                stencil_buf,
                fb_width,
                fb_height,
                &draw.scissors[0],
                &draw.depth_stencil,
                &draw.blend[0],
                &draw.blend_color,
                &draw.color_masks[0],
                &draw.rasterizer,
            );
        } else {
            // Fall back to fixed-function fragment shading with vertex shader output.
            // Use generic 0 as color if present, else white.
            let c0 = out0
                .generics
                .get(&0)
                .copied()
                .unwrap_or([1.0, 1.0, 1.0, 1.0]);
            let c1 = out1
                .generics
                .get(&0)
                .copied()
                .unwrap_or([1.0, 1.0, 1.0, 1.0]);
            let c2 = out2
                .generics
                .get(&0)
                .copied()
                .unwrap_or([1.0, 1.0, 1.0, 1.0]);
            let uv0 = extract_uv_from_generics(out0);
            let uv1 = extract_uv_from_generics(out1);
            let uv2 = extract_uv_from_generics(out2);

            rasterize_triangle(
                p0,
                p1,
                p2,
                c0,
                c1,
                c2,
                uv0,
                uv1,
                uv2,
                texture.as_ref(),
                pixels,
                depth_buf,
                stencil_buf,
                fb_width,
                fb_height,
                &draw.scissors[0],
                &draw.depth_stencil,
                &draw.blend[0],
                &draw.blend_color,
                &draw.color_masks[0],
            );
        }
    }
}

/// Fixed-function rendering for a single draw call (pre-shader fallback).
#[allow(clippy::too_many_arguments)]
fn render_draw_fixed_function(
    draw: &DrawCall,
    read_gpu: &dyn Fn(u64, &mut [u8]),
    pixels: &mut [u8],
    depth_buf: &mut [f32],
    stencil_buf: &mut [u8],
    fb_width: u32,
    fb_height: u32,
) {
    let positions = fetch_positions(draw, read_gpu);
    if positions.is_empty() {
        return;
    }

    let vert_count = positions.len();
    let colors = fetch_vertex_colors(draw, read_gpu, vert_count);
    let texture = load_texture(draw, read_gpu);
    let uvs = if texture.is_some() {
        fetch_texture_coords(draw, read_gpu, vert_count)
    } else {
        None
    };
    let default_uv = [0.0f32, 0.0];

    let triangles = process_topology(&positions, draw.topology);
    let viewport = &draw.viewports[0];
    let vp_width = if viewport.width > 0.0 {
        viewport.width
    } else {
        fb_width as f32
    };
    let vp_height = if viewport.height > 0.0 {
        viewport.height
    } else {
        fb_height as f32
    };
    let vp_x = viewport.x;
    let vp_y = viewport.y;

    for tri in &triangles {
        let p0 = viewport_transform(positions[tri[0]], vp_x, vp_y, vp_width, vp_height);
        let p1 = viewport_transform(positions[tri[1]], vp_x, vp_y, vp_width, vp_height);
        let p2 = viewport_transform(positions[tri[2]], vp_x, vp_y, vp_width, vp_height);

        let area = signed_area(p0, p1, p2);
        if should_cull(area, &draw.rasterizer) {
            continue;
        }

        let c0 = colors[tri[0]];
        let c1 = colors[tri[1]];
        let c2 = colors[tri[2]];

        let uv0 = uvs.as_ref().map_or(default_uv, |u| u[tri[0]]);
        let uv1 = uvs.as_ref().map_or(default_uv, |u| u[tri[1]]);
        let uv2 = uvs.as_ref().map_or(default_uv, |u| u[tri[2]]);

        rasterize_triangle(
            p0,
            p1,
            p2,
            c0,
            c1,
            c2,
            uv0,
            uv1,
            uv2,
            texture.as_ref(),
            pixels,
            depth_buf,
            stencil_buf,
            fb_width,
            fb_height,
            &draw.scissors[0],
            &draw.depth_stencil,
            &draw.blend[0],
            &draw.blend_color,
            &draw.color_masks[0],
        );
    }
}

/// Build constant buffer bindings vector for a shader stage from a draw call.
fn build_cb_bindings(draw: &DrawCall, stage_idx: usize) -> Vec<shader::interpreter::CbBinding> {
    let cb_row = &draw.cb_bindings[stage_idx.min(draw.cb_bindings.len().saturating_sub(1))];
    cb_row
        .iter()
        .map(|cb| shader::interpreter::CbBinding {
            address: cb.address,
            size: cb.size,
        })
        .collect()
}

/// Extract UV coordinates from vertex shader generic outputs.
/// Looks for generic 1 (typical texcoord slot) x,y components.
fn extract_uv_from_generics(output: &shader::VertexShaderOutput) -> [f32; 2] {
    // Try generic 1 first (common texcoord slot), then generic 0.
    if let Some(g) = output.generics.get(&1) {
        [g[0], g[1]]
    } else if let Some(g) = output.generics.get(&0) {
        [g[0], g[1]]
    } else {
        [0.0, 0.0]
    }
}

/// Rasterize a triangle with per-pixel fragment shader execution.
#[allow(clippy::too_many_arguments)]
fn rasterize_triangle_shader(
    v0: [f32; 4],
    v1: [f32; 4],
    v2: [f32; 4],
    vs_out0: &shader::VertexShaderOutput,
    vs_out1: &shader::VertexShaderOutput,
    vs_out2: &shader::VertexShaderOutput,
    fs_program: &shader::ShaderProgram,
    fs_cb: &[shader::interpreter::CbBinding],
    read_gpu: &dyn Fn(u64, &mut [u8]),
    sample_tex: &dyn Fn(u32, f32, f32) -> shader::interpreter::TexSample,
    pixels: &mut [u8],
    depth_buf: &mut [f32],
    _stencil_buf: &mut [u8],
    fb_width: u32,
    fb_height: u32,
    scissor: &ScissorInfo,
    depth_stencil: &DepthStencilInfo,
    blend: &BlendInfo,
    blend_color: &BlendColorInfo,
    color_mask: &ColorMaskInfo,
    rasterizer_info: &RasterizerInfo,
) {
    // Compute bounding box.
    let mut min_x = v0[0].min(v1[0]).min(v2[0]).max(0.0) as u32;
    let mut max_x = (v0[0].max(v1[0]).max(v2[0]).ceil() as u32).min(fb_width.saturating_sub(1));
    let mut min_y = v0[1].min(v1[1]).min(v2[1]).max(0.0) as u32;
    let mut max_y = (v0[1].max(v1[1]).max(v2[1]).ceil() as u32).min(fb_height.saturating_sub(1));

    if scissor.enabled {
        min_x = min_x.max(scissor.min_x);
        max_x = max_x.min(scissor.max_x.saturating_sub(1));
        min_y = min_y.max(scissor.min_y);
        max_y = max_y.min(scissor.max_y.saturating_sub(1));
    }

    if min_x > max_x || min_y > max_y {
        return;
    }

    let area = signed_area(v0, v1, v2);
    if area.abs() < 0.5 {
        return;
    }
    let inv_area = 1.0 / area;

    // Collect all generic attribute indices present across the three vertices.
    let mut generic_keys: Vec<u32> = Vec::new();
    for key in vs_out0.generics.keys() {
        if !generic_keys.contains(key) {
            generic_keys.push(*key);
        }
    }
    for key in vs_out1.generics.keys() {
        if !generic_keys.contains(key) {
            generic_keys.push(*key);
        }
    }
    for key in vs_out2.generics.keys() {
        if !generic_keys.contains(key) {
            generic_keys.push(*key);
        }
    }
    generic_keys.sort_unstable();

    let default_generic = [0.0f32; 4];

    for py in min_y..=max_y {
        for px in min_x..=max_x {
            let fx = px as f32 + 0.5;
            let fy = py as f32 + 0.5;

            // Barycentric coordinates.
            let w0 = ((v1[0] - v2[0]) * (fy - v2[1]) - (v1[1] - v2[1]) * (fx - v2[0])) * inv_area;
            let w1 = ((v2[0] - v0[0]) * (fy - v0[1]) - (v2[1] - v0[1]) * (fx - v0[0])) * inv_area;
            let w2 = 1.0 - w0 - w1;

            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }

            // Perspective-correct interpolation weights.
            let inv_w0 = if v0[3] != 0.0 { 1.0 / v0[3] } else { 1.0 };
            let inv_w1 = if v1[3] != 0.0 { 1.0 / v1[3] } else { 1.0 };
            let inv_w2 = if v2[3] != 0.0 { 1.0 / v2[3] } else { 1.0 };
            let denom = w0 * inv_w0 + w1 * inv_w1 + w2 * inv_w2;
            let (pb0, pb1, pb2) = if denom != 0.0 {
                (
                    w0 * inv_w0 / denom,
                    w1 * inv_w1 / denom,
                    w2 * inv_w2 / denom,
                )
            } else {
                (w0, w1, w2)
            };

            // Interpolate depth and apply depth bias.
            let mut z = v0[2] * w0 + v1[2] * w1 + v2[2] * w2;
            if rasterizer_info.depth_bias != 0.0 || rasterizer_info.slope_scale_depth_bias != 0.0 {
                // Compute depth gradients (triangle is planar, so constant per-triangle).
                let dzdx = (v1[2] - v0[2]) * (v2[1] - v0[1]) - (v2[2] - v0[2]) * (v1[1] - v0[1]);
                let dzdy = (v2[2] - v0[2]) * (v1[0] - v0[0]) - (v1[2] - v0[2]) * (v2[0] - v0[0]);
                let max_slope = dzdx.abs().max(dzdy.abs()) / area.abs();
                let min_resolvable = 1.0 / (1u32 << 24) as f32;
                let bias = rasterizer_info.slope_scale_depth_bias * max_slope
                    + rasterizer_info.depth_bias * min_resolvable;
                let bias = if rasterizer_info.depth_bias_clamp != 0.0 {
                    bias.clamp(
                        -rasterizer_info.depth_bias_clamp.abs(),
                        rasterizer_info.depth_bias_clamp.abs(),
                    )
                } else {
                    bias
                };
                z += bias;
            }

            let fb_idx = (py * fb_width + px) as usize;

            // Depth test.
            if depth_stencil.depth_test_enable {
                if !depth_test_passes(depth_stencil.depth_func, z, depth_buf[fb_idx]) {
                    continue;
                }
            }

            // Interpolate all generic varyings for fragment shader.
            let mut varyings: Vec<[f32; 4]> = Vec::new();
            let max_generic = generic_keys.last().map_or(0, |k| *k as usize + 1);
            varyings.resize(max_generic, [0.0; 4]);
            for &key in &generic_keys {
                let g0 = vs_out0.generics.get(&key).unwrap_or(&default_generic);
                let g1 = vs_out1.generics.get(&key).unwrap_or(&default_generic);
                let g2 = vs_out2.generics.get(&key).unwrap_or(&default_generic);
                let idx = key as usize;
                for c in 0..4 {
                    varyings[idx][c] = g0[c] * pb0 + g1[c] * pb1 + g2[c] * pb2;
                }
            }

            // Run fragment shader.
            let mut fs_ctx = shader::interpreter::ShaderExecContext::new(read_gpu, sample_tex);
            fs_ctx.cb_bindings = fs_cb.to_vec();
            fs_ctx.input_varyings = varyings;

            fs_ctx.run(fs_program);
            let frag_out = fs_ctx.collect_fragment_output();

            if frag_out.killed {
                continue;
            }

            let src_color = frag_out.color;

            // Depth write.
            if depth_stencil.depth_test_enable && depth_stencil.depth_write_enable {
                depth_buf[fb_idx] = frag_out.depth.unwrap_or(z);
            }

            // Read existing pixel for blending.
            let pixel_offset = fb_idx * 4;
            let dst_color = [
                pixels[pixel_offset] as f32 / 255.0,
                pixels[pixel_offset + 1] as f32 / 255.0,
                pixels[pixel_offset + 2] as f32 / 255.0,
                pixels[pixel_offset + 3] as f32 / 255.0,
            ];

            // Blend.
            let final_color = if blend.enabled {
                blend_colors(src_color, dst_color, blend, blend_color)
            } else {
                src_color
            };

            // Write pixel with color mask.
            if color_mask.r {
                pixels[pixel_offset] = (final_color[0].clamp(0.0, 1.0) * 255.0) as u8;
            }
            if color_mask.g {
                pixels[pixel_offset + 1] = (final_color[1].clamp(0.0, 1.0) * 255.0) as u8;
            }
            if color_mask.b {
                pixels[pixel_offset + 2] = (final_color[2].clamp(0.0, 1.0) * 255.0) as u8;
            }
            if color_mask.a {
                pixels[pixel_offset + 3] = (final_color[3].clamp(0.0, 1.0) * 255.0) as u8;
            }
        }
    }
}

/// Fetch vertex positions from GPU memory for a draw call.
fn fetch_positions(draw: &DrawCall, read_gpu: &dyn Fn(u64, &mut [u8])) -> Vec<[f32; 4]> {
    // Find the position attribute (index 0 by convention, or first Float with >= 2 components).
    let pos_attrib = draw.vertex_attribs.iter().find(|a| {
        a.buffer_index == 0
            || (a.attrib_type == VertexAttribType::Float && a.size.component_count() >= 2)
    });

    let attrib = match pos_attrib {
        Some(a) => a,
        None => {
            if draw.vertex_attribs.is_empty() {
                return vec![];
            }
            // Fall back to first attribute with >= 2 components.
            match draw
                .vertex_attribs
                .iter()
                .find(|a| a.size.component_count() >= 2)
            {
                Some(a) => a,
                None => return vec![],
            }
        }
    };

    // Find the vertex stream this attribute references.
    let stream = draw
        .vertex_streams
        .iter()
        .find(|s| s.index == attrib.buffer_index);
    let stream = match stream {
        Some(s) => s,
        None => return vec![],
    };

    if stream.stride == 0 || stream.address == 0 {
        return vec![];
    }

    // Determine vertex indices.
    let indices = if draw.indexed {
        fetch_indices(draw, read_gpu)
    } else {
        let first = draw.vertex_first;
        let count = draw.vertex_count;
        (first..first + count).collect()
    };

    // Fetch position data for each vertex.
    let comp_count = attrib.size.component_count() as usize;
    let mut positions = Vec::with_capacity(indices.len());

    for idx in &indices {
        let vertex_addr =
            stream.address + (*idx as u64) * (stream.stride as u64) + (attrib.offset as u64);

        let mut pos = [0.0f32; 4];
        pos[3] = 1.0; // default w

        match attrib.size {
            VertexAttribSize::R32G32
            | VertexAttribSize::R32G32B32
            | VertexAttribSize::R32G32B32A32
            | VertexAttribSize::R32 => {
                let byte_count = comp_count * 4;
                let mut buf = vec![0u8; byte_count];
                read_gpu(vertex_addr, &mut buf);
                for (i, chunk) in buf.chunks_exact(4).enumerate().take(comp_count) {
                    pos[i] = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                }
            }
            _ => {
                // For non-float32 formats, skip (unsupported without shader).
                continue;
            }
        }

        positions.push(pos);
    }

    positions
}

/// Fetch ALL vertex attributes for each vertex, returning a Vec (per-vertex) of
/// Vec<[f32; 4]> indexed by hardware attribute slot (0..31). Used by the shader
/// execution path to provide input attributes to the vertex shader.
fn fetch_all_vertex_attribs(
    draw: &DrawCall,
    read_gpu: &dyn Fn(u64, &mut [u8]),
) -> Vec<Vec<[f32; 4]>> {
    // Determine vertex indices.
    let indices: Vec<u32> = if draw.indexed {
        fetch_indices(draw, read_gpu)
    } else {
        let first = draw.vertex_first;
        let count = draw.vertex_count;
        (first..first + count).collect()
    };

    let num_verts = indices.len();
    // Each vertex gets up to 32 attribute slots, initially zero.
    let mut result: Vec<Vec<[f32; 4]>> = Vec::with_capacity(num_verts);
    for _ in 0..num_verts {
        result.push(vec![[0.0, 0.0, 0.0, 0.0]; 32]);
    }

    for attrib in &draw.vertex_attribs {
        let stream = draw
            .vertex_streams
            .iter()
            .find(|s| s.index == attrib.buffer_index);
        let stream = match stream {
            Some(s) if s.stride > 0 && s.address > 0 => s,
            _ => continue,
        };

        // Determine the hardware attribute slot. The slot is the index of this
        // attribute in the vertex_attribs array. Maxwell uses explicit slot
        // assignment via the attrib index register, but since we store them in
        // order from the engine, use the position in the list.
        let slot = draw
            .vertex_attribs
            .iter()
            .position(|a| std::ptr::eq(a, attrib))
            .unwrap_or(0);
        if slot >= 32 {
            continue;
        }

        let comp_count = attrib.size.component_count() as usize;
        if comp_count == 0 {
            continue;
        }

        for (vi, idx) in indices.iter().enumerate() {
            let addr =
                stream.address + (*idx as u64) * (stream.stride as u64) + (attrib.offset as u64);

            let mut val = [0.0f32; 4];
            val[3] = if comp_count < 4 { 1.0 } else { 0.0 }; // Default w=1 for 3-comp

            match (attrib.size, attrib.attrib_type) {
                // Float formats
                (
                    VertexAttribSize::R32
                    | VertexAttribSize::R32G32
                    | VertexAttribSize::R32G32B32
                    | VertexAttribSize::R32G32B32A32,
                    VertexAttribType::Float,
                ) => {
                    let byte_count = comp_count * 4;
                    let mut buf = vec![0u8; byte_count];
                    read_gpu(addr, &mut buf);
                    for (i, chunk) in buf.chunks_exact(4).enumerate().take(comp_count) {
                        val[i] = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                    }
                }
                // UNorm R8G8B8A8 / R8G8B8
                (
                    VertexAttribSize::R8G8B8A8 | VertexAttribSize::R8G8B8,
                    VertexAttribType::UNorm,
                ) => {
                    let byte_count = comp_count;
                    let mut buf = vec![0u8; byte_count];
                    read_gpu(addr, &mut buf);
                    for (i, &b) in buf.iter().enumerate().take(comp_count.min(4)) {
                        val[i] = b as f32 / 255.0;
                    }
                }
                // UNorm R16G16 / R16G16B16A16
                (
                    VertexAttribSize::R16G16 | VertexAttribSize::R16G16B16A16,
                    VertexAttribType::UNorm,
                ) => {
                    let byte_count = comp_count * 2;
                    let mut buf = vec![0u8; byte_count];
                    read_gpu(addr, &mut buf);
                    for (i, chunk) in buf.chunks_exact(2).enumerate().take(comp_count) {
                        let v = u16::from_le_bytes([chunk[0], chunk[1]]);
                        val[i] = v as f32 / 65535.0;
                    }
                }
                // SNorm R16G16
                (VertexAttribSize::R16G16, VertexAttribType::SNorm) => {
                    let mut buf = [0u8; 4];
                    read_gpu(addr, &mut buf);
                    for i in 0..2 {
                        let v = i16::from_le_bytes([buf[i * 2], buf[i * 2 + 1]]);
                        val[i] = (v as f32 / 32767.0).clamp(-1.0, 1.0);
                    }
                }
                _ => {
                    // Unsupported format — leave as zeros.
                }
            }

            result[vi][slot] = val;
        }
    }

    result
}

/// Fetch per-vertex RGBA colors from GPU memory.
///
/// Heuristic: finds the color attribute as (1) first attrib with `buffer_index == 1`,
/// or (2) first `UNorm` attrib with >= 3 components, or (3) second attrib overall.
/// Returns `[1.0, 1.0, 1.0, 1.0]` (white) per vertex if no color attribute found.
fn fetch_vertex_colors(
    draw: &DrawCall,
    read_gpu: &dyn Fn(u64, &mut [u8]),
    vertex_count: usize,
) -> Vec<[f32; 4]> {
    let white = [1.0f32, 1.0, 1.0, 1.0];

    // Find a color attribute using heuristics.
    let color_attrib = draw
        .vertex_attribs
        .iter()
        .find(|a| a.buffer_index == 1)
        .or_else(|| {
            draw.vertex_attribs
                .iter()
                .find(|a| a.attrib_type == VertexAttribType::UNorm && a.size.component_count() >= 3)
        })
        .or_else(|| {
            if draw.vertex_attribs.len() >= 2 {
                Some(&draw.vertex_attribs[1])
            } else {
                None
            }
        });

    let attrib = match color_attrib {
        Some(a) => a,
        None => return vec![white; vertex_count],
    };

    // Find the vertex stream for this attribute.
    let stream = draw
        .vertex_streams
        .iter()
        .find(|s| s.index == attrib.buffer_index);
    let stream = match stream {
        Some(s) => s,
        None => return vec![white; vertex_count],
    };

    if stream.stride == 0 || stream.address == 0 {
        return vec![white; vertex_count];
    }

    // Re-derive vertex indices (same as fetch_positions).
    let indices: Vec<u32> = if draw.indexed {
        fetch_indices(draw, read_gpu)
    } else {
        let first = draw.vertex_first;
        let count = draw.vertex_count;
        (first..first + count).collect()
    };

    let comp_count = attrib.size.component_count() as usize;
    let mut colors = Vec::with_capacity(vertex_count);

    for idx in indices.iter().take(vertex_count) {
        let addr = stream.address + (*idx as u64) * (stream.stride as u64) + (attrib.offset as u64);

        let mut color = white;

        match (attrib.size, attrib.attrib_type) {
            (VertexAttribSize::R8G8B8A8, VertexAttribType::UNorm)
            | (VertexAttribSize::R8G8B8, VertexAttribType::UNorm) => {
                let byte_count = comp_count;
                let mut buf = vec![0u8; byte_count];
                read_gpu(addr, &mut buf);
                for (i, &b) in buf.iter().enumerate().take(comp_count.min(4)) {
                    color[i] = b as f32 / 255.0;
                }
            }
            (
                VertexAttribSize::R32G32B32A32
                | VertexAttribSize::R32G32B32
                | VertexAttribSize::R32G32
                | VertexAttribSize::R32,
                VertexAttribType::Float,
            ) => {
                let byte_count = comp_count * 4;
                let mut buf = vec![0u8; byte_count];
                read_gpu(addr, &mut buf);
                for (i, chunk) in buf.chunks_exact(4).enumerate().take(comp_count.min(4)) {
                    color[i] = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                }
            }
            _ => {
                // Unsupported color format — default white.
            }
        }

        colors.push(color);
    }

    // Pad with white if indices produced fewer than vertex_count.
    while colors.len() < vertex_count {
        colors.push(white);
    }

    colors
}

/// Fetch index buffer entries as u32 indices.
fn fetch_indices(draw: &DrawCall, read_gpu: &dyn Fn(u64, &mut [u8])) -> Vec<u32> {
    // If inline index data is available, use it.
    if !draw.inline_index_data.is_empty() {
        return parse_indices(&draw.inline_index_data, draw.index_format);
    }

    if draw.index_buffer_addr == 0 || draw.index_buffer_count == 0 {
        return vec![];
    }

    let count = draw.index_buffer_count as usize;
    let elem_size = match draw.index_format {
        IndexFormat::UnsignedByte => 1,
        IndexFormat::UnsignedShort => 2,
        IndexFormat::UnsignedInt => 4,
    };

    let first = draw.index_buffer_first as usize;
    let byte_offset = first * elem_size;
    let byte_count = count * elem_size;
    let mut buf = vec![0u8; byte_count];
    read_gpu(draw.index_buffer_addr + byte_offset as u64, &mut buf);

    parse_indices(&buf, draw.index_format)
}

/// Parse raw bytes into u32 index values.
fn parse_indices(data: &[u8], format: IndexFormat) -> Vec<u32> {
    match format {
        IndexFormat::UnsignedByte => data.iter().map(|&b| b as u32).collect(),
        IndexFormat::UnsignedShort => data
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]) as u32)
            .collect(),
        IndexFormat::UnsignedInt => data
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    }
}

/// Map clip-space position to screen pixel coordinates, preserving Z and W.
fn viewport_transform(
    pos: [f32; 4],
    vp_x: f32,
    vp_y: f32,
    vp_width: f32,
    vp_height: f32,
) -> [f32; 4] {
    let w = if pos[3] != 0.0 { pos[3] } else { 1.0 };
    let x = (pos[0] * 0.5 + 0.5) * vp_width + vp_x;
    let y = (pos[1] * 0.5 + 0.5) * vp_height + vp_y;
    [x, y, pos[2], w]
}

/// Convert a list of vertex positions into triangle index triples based on topology.
fn process_topology(positions: &[[f32; 4]], topology: PrimitiveTopology) -> Vec<[usize; 3]> {
    let n = positions.len();
    let mut tris = Vec::new();

    match topology {
        PrimitiveTopology::Triangles => {
            let count = n / 3;
            for i in 0..count {
                tris.push([i * 3, i * 3 + 1, i * 3 + 2]);
            }
        }
        PrimitiveTopology::TriangleStrip => {
            if n < 3 {
                return tris;
            }
            for i in 0..n - 2 {
                if i % 2 == 0 {
                    tris.push([i, i + 1, i + 2]);
                } else {
                    tris.push([i, i + 2, i + 1]); // swap winding
                }
            }
        }
        PrimitiveTopology::TriangleFan => {
            if n < 3 {
                return tris;
            }
            for i in 1..n - 1 {
                tris.push([0, i, i + 1]);
            }
        }
        PrimitiveTopology::Quads => {
            let count = n / 4;
            for i in 0..count {
                let base = i * 4;
                tris.push([base, base + 1, base + 2]);
                tris.push([base, base + 2, base + 3]);
            }
        }
        _ => {
            log::warn!(
                "SoftwareRasterizer: unsupported topology {:?}, skipping",
                topology
            );
        }
    }

    tris
}

/// Compute signed area of a triangle from screen-space vertices.
/// Positive = CCW winding, negative = CW winding. Uses only X,Y components.
fn signed_area(v0: [f32; 4], v1: [f32; 4], v2: [f32; 4]) -> f32 {
    (v1[0] - v0[0]) * (v2[1] - v0[1]) - (v2[0] - v0[0]) * (v1[1] - v0[1])
}

/// Determine whether a triangle should be culled based on winding and rasterizer state.
fn should_cull(area: f32, rasterizer: &RasterizerInfo) -> bool {
    if !rasterizer.cull_enable {
        return false;
    }

    // Determine if the triangle is front-facing.
    let is_front = match rasterizer.front_face {
        FrontFace::CCW => area > 0.0,
        FrontFace::CW => area < 0.0,
    };

    match rasterizer.cull_face {
        CullFace::Front => is_front,
        CullFace::Back => !is_front,
        CullFace::FrontAndBack => true,
    }
}

/// Evaluate a depth comparison function.
fn depth_test_passes(func: ComparisonOp, frag_z: f32, stored_z: f32) -> bool {
    match func {
        ComparisonOp::Never => false,
        ComparisonOp::Less => frag_z < stored_z,
        ComparisonOp::Equal => (frag_z - stored_z).abs() < f32::EPSILON,
        ComparisonOp::LessEqual => frag_z <= stored_z,
        ComparisonOp::Greater => frag_z > stored_z,
        ComparisonOp::NotEqual => (frag_z - stored_z).abs() >= f32::EPSILON,
        ComparisonOp::GreaterEqual => frag_z >= stored_z,
        ComparisonOp::Always => true,
    }
}

/// Evaluate a stencil comparison function on masked values.
fn stencil_test_passes(func: ComparisonOp, ref_val: u32, mask: u32, stored: u8) -> bool {
    let masked_ref = (ref_val & mask) as u8;
    let masked_stored = stored & (mask as u8);
    match func {
        ComparisonOp::Never => false,
        ComparisonOp::Less => masked_ref < masked_stored,
        ComparisonOp::Equal => masked_ref == masked_stored,
        ComparisonOp::LessEqual => masked_ref <= masked_stored,
        ComparisonOp::Greater => masked_ref > masked_stored,
        ComparisonOp::NotEqual => masked_ref != masked_stored,
        ComparisonOp::GreaterEqual => masked_ref >= masked_stored,
        ComparisonOp::Always => true,
    }
}

/// Apply a stencil operation to produce a new stencil value.
fn apply_stencil_op(op: StencilOp, ref_val: u32, current: u8) -> u8 {
    match op {
        StencilOp::Keep => current,
        StencilOp::Zero => 0,
        StencilOp::Replace => ref_val as u8,
        StencilOp::IncrSat => current.saturating_add(1),
        StencilOp::DecrSat => current.saturating_sub(1),
        StencilOp::Invert => !current,
        StencilOp::Incr => current.wrapping_add(1),
        StencilOp::Decr => current.wrapping_sub(1),
    }
}

/// Write a new stencil value applying the write mask.
fn write_stencil(stencil_buf: &mut [u8], idx: usize, new_val: u8, write_mask: u32, old: u8) {
    let wm = write_mask as u8;
    stencil_buf[idx] = (new_val & wm) | (old & !wm);
}

/// Apply a blend factor to produce per-channel multipliers.
fn apply_blend_factor(
    factor: BlendFactor,
    src: [f32; 4],
    dst: [f32; 4],
    constant: [f32; 4],
) -> [f32; 4] {
    match factor {
        BlendFactor::Zero => [0.0, 0.0, 0.0, 0.0],
        BlendFactor::One => [1.0, 1.0, 1.0, 1.0],
        BlendFactor::SrcColor => src,
        BlendFactor::OneMinusSrcColor => [1.0 - src[0], 1.0 - src[1], 1.0 - src[2], 1.0 - src[3]],
        BlendFactor::SrcAlpha => [src[3], src[3], src[3], src[3]],
        BlendFactor::OneMinusSrcAlpha => {
            let a = 1.0 - src[3];
            [a, a, a, a]
        }
        BlendFactor::DstAlpha => [dst[3], dst[3], dst[3], dst[3]],
        BlendFactor::OneMinusDstAlpha => {
            let a = 1.0 - dst[3];
            [a, a, a, a]
        }
        BlendFactor::DstColor => dst,
        BlendFactor::OneMinusDstColor => [1.0 - dst[0], 1.0 - dst[1], 1.0 - dst[2], 1.0 - dst[3]],
        BlendFactor::SrcAlphaSaturate => {
            let f = src[3].min(1.0 - dst[3]);
            [f, f, f, 1.0]
        }
        BlendFactor::ConstantColor => constant,
        BlendFactor::OneMinusConstantColor => [
            1.0 - constant[0],
            1.0 - constant[1],
            1.0 - constant[2],
            1.0 - constant[3],
        ],
        BlendFactor::ConstantAlpha => [constant[3], constant[3], constant[3], constant[3]],
        BlendFactor::OneMinusConstantAlpha => {
            let a = 1.0 - constant[3];
            [a, a, a, a]
        }
        // Dual-source blending — treat as One for now (no second color source in software).
        BlendFactor::Src1Color
        | BlendFactor::OneMinusSrc1Color
        | BlendFactor::Src1Alpha
        | BlendFactor::OneMinusSrc1Alpha => [1.0, 1.0, 1.0, 1.0],
    }
}

/// Apply a blend equation to combine source and destination terms.
fn blend_equation(eq: BlendEquation, sf: f32, df: f32) -> f32 {
    match eq {
        BlendEquation::Add => sf + df,
        BlendEquation::Subtract => sf - df,
        BlendEquation::ReverseSubtract => df - sf,
        BlendEquation::Min => sf.min(df),
        BlendEquation::Max => sf.max(df),
    }
}

/// Full blend pipeline: combine source and destination colors.
fn blend_colors(
    src: [f32; 4],
    dst: [f32; 4],
    blend: &BlendInfo,
    blend_color: &BlendColorInfo,
) -> [f32; 4] {
    if !blend.enabled {
        return src;
    }

    let constant = [blend_color.r, blend_color.g, blend_color.b, blend_color.a];
    let sf = apply_blend_factor(blend.color_src, src, dst, constant);
    let df = apply_blend_factor(blend.color_dst, src, dst, constant);

    let mut result = [0.0f32; 4];

    // Color channels (RGB).
    for i in 0..3 {
        result[i] = blend_equation(blend.color_op, src[i] * sf[i], dst[i] * df[i]).clamp(0.0, 1.0);
    }

    // Alpha channel.
    if blend.separate_alpha {
        let sa = apply_blend_factor(blend.alpha_src, src, dst, constant);
        let da = apply_blend_factor(blend.alpha_dst, src, dst, constant);
        result[3] = blend_equation(blend.alpha_op, src[3] * sa[3], dst[3] * da[3]).clamp(0.0, 1.0);
    } else {
        result[3] = blend_equation(blend.color_op, src[3] * sf[3], dst[3] * df[3]).clamp(0.0, 1.0);
    }

    result
}

/// Rasterize a single triangle with full per-pixel pipeline.
fn rasterize_triangle(
    v0: [f32; 4],
    v1: [f32; 4],
    v2: [f32; 4],
    c0: [f32; 4],
    c1: [f32; 4],
    c2: [f32; 4],
    uv0: [f32; 2],
    uv1: [f32; 2],
    uv2: [f32; 2],
    texture: Option<&SampledTexture>,
    pixels: &mut [u8],
    depth_buf: &mut [f32],
    stencil_buf: &mut [u8],
    fb_width: u32,
    fb_height: u32,
    scissor: &ScissorInfo,
    depth_stencil: &DepthStencilInfo,
    blend: &BlendInfo,
    blend_color: &BlendColorInfo,
    color_mask: &ColorMaskInfo,
) {
    // Compute bounding box.
    let mut min_x = v0[0].min(v1[0]).min(v2[0]).max(0.0) as u32;
    let mut max_x = (v0[0].max(v1[0]).max(v2[0]).ceil() as u32).min(fb_width.saturating_sub(1));
    let mut min_y = v0[1].min(v1[1]).min(v2[1]).max(0.0) as u32;
    let mut max_y = (v0[1].max(v1[1]).max(v2[1]).ceil() as u32).min(fb_height.saturating_sub(1));

    // Apply scissor clipping to bounding box.
    if scissor.enabled {
        min_x = min_x.max(scissor.min_x);
        max_x = max_x.min(scissor.max_x.saturating_sub(1));
        min_y = min_y.max(scissor.min_y);
        max_y = max_y.min(scissor.max_y.saturating_sub(1));
    }

    if min_x > max_x || min_y > max_y {
        return;
    }

    // Edge function: positive when point is on the left side of the edge.
    let edge = |a: [f32; 4], b: [f32; 4], p: [f32; 2]| -> f32 {
        (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
    };

    // Perspective-correct interpolation weights (1/w per vertex).
    let inv_w0 = 1.0 / v0[3];
    let inv_w1 = 1.0 / v1[3];
    let inv_w2 = 1.0 / v2[3];

    // Compute per-triangle mipmap LOD from screen-space vs texture-space area ratio.
    let tri_lod = if let Some(tex) = texture {
        if !tex.mip_levels.is_empty() && tex.normalized_coords {
            let screen_area =
                0.5 * ((v1[0] - v0[0]) * (v2[1] - v0[1]) - (v2[0] - v0[0]) * (v1[1] - v0[1])).abs();
            let tex_area_uv = 0.5
                * ((uv1[0] - uv0[0]) * (uv2[1] - uv0[1]) - (uv2[0] - uv0[0]) * (uv1[1] - uv0[1]))
                    .abs();
            let texel_area = tex_area_uv * tex.width as f32 * tex.height as f32;
            if screen_area > 0.0 && texel_area > 0.0 {
                let lod = 0.5 * (texel_area / screen_area).log2();
                let max_level = tex.mip_levels.len() as u32;
                (lod.round() as u32).min(max_level)
            } else {
                0
            }
        } else {
            0
        }
    } else {
        0
    };

    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let p = [x as f32 + 0.5, y as f32 + 0.5];
            let w0 = edge(v1, v2, p);
            let w1 = edge(v2, v0, p);
            let w2 = edge(v0, v1, p);

            if w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0 {
                let w_sum = w0 + w1 + w2;
                if w_sum < f32::EPSILON {
                    continue; // degenerate triangle
                }

                let inv_wsum = 1.0 / w_sum;
                let b0 = w0 * inv_wsum;
                let b1 = w1 * inv_wsum;
                let b2 = w2 * inv_wsum;

                let pixel_idx = (y * fb_width + x) as usize;
                let offset = pixel_idx * 4;

                if offset + 4 > pixels.len() {
                    continue;
                }

                // Stencil test (before depth test).
                if depth_stencil.stencil_enable {
                    let face = &depth_stencil.front;
                    let stored = stencil_buf[pixel_idx];
                    if !stencil_test_passes(face.func, face.ref_value, face.func_mask, stored) {
                        let new_val = apply_stencil_op(face.fail_op, face.ref_value, stored);
                        write_stencil(stencil_buf, pixel_idx, new_val, face.write_mask, stored);
                        continue;
                    }
                }

                // Depth test.
                let depth_passed = if depth_stencil.depth_test_enable {
                    let frag_z = v0[2] * b0 + v1[2] * b1 + v2[2] * b2;
                    let passed =
                        depth_test_passes(depth_stencil.depth_func, frag_z, depth_buf[pixel_idx]);
                    if passed && depth_stencil.depth_write_enable {
                        depth_buf[pixel_idx] = frag_z;
                    }
                    passed
                } else {
                    true
                };

                if !depth_passed {
                    // Stencil zfail_op.
                    if depth_stencil.stencil_enable {
                        let face = &depth_stencil.front;
                        let stored = stencil_buf[pixel_idx];
                        let new_val = apply_stencil_op(face.zfail_op, face.ref_value, stored);
                        write_stencil(stencil_buf, pixel_idx, new_val, face.write_mask, stored);
                    }
                    continue;
                }

                // Stencil zpass_op.
                if depth_stencil.stencil_enable {
                    let face = &depth_stencil.front;
                    let stored = stencil_buf[pixel_idx];
                    let new_val = apply_stencil_op(face.zpass_op, face.ref_value, stored);
                    write_stencil(stencil_buf, pixel_idx, new_val, face.write_mask, stored);
                }

                // Perspective-correct interpolation weights for attributes.
                let interp_inv_w = inv_w0 * b0 + inv_w1 * b1 + inv_w2 * b2;
                let persp_w = if interp_inv_w.abs() > f32::EPSILON {
                    1.0 / interp_inv_w
                } else {
                    1.0
                };
                let pb0 = b0 * inv_w0 * persp_w;
                let pb1 = b1 * inv_w1 * persp_w;
                let pb2 = b2 * inv_w2 * persp_w;

                // Interpolate vertex color using perspective-correct weights.
                let mut src = [
                    c0[0] * pb0 + c1[0] * pb1 + c2[0] * pb2,
                    c0[1] * pb0 + c1[1] * pb1 + c2[1] * pb2,
                    c0[2] * pb0 + c1[2] * pb1 + c2[2] * pb2,
                    c0[3] * pb0 + c1[3] * pb1 + c2[3] * pb2,
                ];

                // Texture modulation using perspective-correct UVs.
                if let Some(tex) = texture {
                    let u = uv0[0] * pb0 + uv1[0] * pb1 + uv2[0] * pb2;
                    let v = uv0[1] * pb0 + uv1[1] * pb1 + uv2[1] * pb2;
                    let tex_color = sample_texture_lod(tex, u, v, tri_lod);
                    src[0] *= tex_color[0];
                    src[1] *= tex_color[1];
                    src[2] *= tex_color[2];
                    src[3] *= tex_color[3];
                }

                // Read destination color for blending.
                let dst = [
                    pixels[offset] as f32 / 255.0,
                    pixels[offset + 1] as f32 / 255.0,
                    pixels[offset + 2] as f32 / 255.0,
                    pixels[offset + 3] as f32 / 255.0,
                ];

                let blended = blend_colors(src, dst, blend, blend_color);

                // Apply color write mask.
                if color_mask.r {
                    pixels[offset] = (blended[0] * 255.0).round().clamp(0.0, 255.0) as u8;
                }
                if color_mask.g {
                    pixels[offset + 1] = (blended[1] * 255.0).round().clamp(0.0, 255.0) as u8;
                }
                if color_mask.b {
                    pixels[offset + 2] = (blended[2] * 255.0).round().clamp(0.0, 255.0) as u8;
                }
                if color_mask.a {
                    pixels[offset + 3] = (blended[3] * 255.0).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::maxwell_3d::*;

    fn default_draw_call() -> DrawCall {
        DrawCall {
            topology: PrimitiveTopology::Triangles,
            vertex_first: 0,
            vertex_count: 0,
            indexed: false,
            index_buffer_addr: 0,
            index_buffer_addr_end: 0,
            index_buffer_count: 0,
            index_buffer_first: 0,
            index_format: IndexFormat::UnsignedByte,
            vertex_streams: Default::default(),
            vertex_stream_instances: Default::default(),
            vertex_stream_limits: Default::default(),
            viewports: [ViewportInfo::default(); NUM_VIEWPORTS],
            viewport_transforms: Default::default(),
            scissors: [ScissorInfo::default(); NUM_VIEWPORTS],
            viewport_scale_offset_enabled: false,
            window_origin_lower_left: false,
            window_origin_flip_y: false,
            surface_clip: SurfaceClipInfo::default(),
            blend: [BlendInfo::default(); 8],
            blend_per_target_enabled: false,
            global_blend: BlendInfo::default(),
            iterated_blend_enabled: false,
            blend_color: BlendColorInfo {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            },
            depth_stencil: DepthStencilInfo {
                depth_test_enable: false,
                depth_write_enable: false,
                depth_func: ComparisonOp::Always,
                depth_mode: DepthMode::ZeroToOne,
                stencil_enable: false,
                stencil_two_side: false,
                front: StencilFaceInfo::default(),
                back: StencilFaceInfo::default(),
            },
            rasterizer: RasterizerInfo {
                cull_enable: false,
                front_face: FrontFace::CCW,
                cull_face: CullFace::Back,
                polygon_mode_front: PolygonMode::Fill,
                polygon_mode_back: PolygonMode::Fill,
                line_width_smooth: 1.0,
                line_width_aliased: 1.0,
                depth_bias: 0.0,
                slope_scale_depth_bias: 0.0,
                depth_bias_clamp: 0.0,
                ..RasterizerInfo::default()
            },
            rasterize_enable: true,
            primitive_restart: PrimitiveRestartInfo::default(),
            logic_op: LogicOpInfo::default(),
            depth_clamp_enabled: true,
            conservative_raster_enable: false,
            engine_state: EngineHint::None,
            provoking_vertex_last: false,
            depth_bounds_enable: false,
            depth_bounds: [0.0, 1.0],
            mandated_early_z: false,
            alpha_test_enabled: false,
            alpha_test_func: ComparisonOp::Always,
            alpha_test_ref: 0.0,
            point_size: 1.0,
            tessellation_primitive: 0,
            tessellation_spacing: 0,
            tessellation_clockwise: false,
            patch_vertices: 1,
            anti_alias_samples_mode: 0,
            anti_alias_alpha_control: AntiAliasAlphaControlInfo::default(),
            line_anti_alias_enable: false,
            line_stipple: Default::default(),
            program_base_address: 0,
            cb_bindings: [[ConstBufferBinding::default(); 18]; 5],
            vertex_attribs: Default::default(),
            shader_stages: [ShaderStageInfo::default(); 6],
            color_masks: [ColorMaskInfo::default(); 8],
            rt_control: RtControlInfo::default(),
            tex_header_pool_addr: 0,
            tex_header_pool_limit: 0,
            tex_sampler_pool_addr: 0,
            tex_sampler_pool_limit: 0,
            instance_count: 1,
            base_instance: 0,
            base_vertex: 0,
            inline_index_data: vec![],
            sampler_binding: SamplerBinding::Independently,
            render_targets: [RenderTargetInfo::default(); 8],
            zeta: ZetaInfo::default(),
            transform_feedback_enabled: false,
            transform_feedback_state: Default::default(),
            dirty_flags: [false; 256],
        }
    }

    /// Helper: default scissor (disabled).
    fn default_scissor() -> ScissorInfo {
        ScissorInfo {
            enabled: false,
            min_x: 0,
            max_x: 0,
            min_y: 0,
            max_y: 0,
        }
    }

    /// Helper: default depth/stencil (disabled).
    fn default_depth_stencil() -> DepthStencilInfo {
        DepthStencilInfo {
            depth_test_enable: false,
            depth_write_enable: false,
            depth_func: ComparisonOp::Always,
            depth_mode: DepthMode::ZeroToOne,
            stencil_enable: false,
            stencil_two_side: false,
            front: StencilFaceInfo::default(),
            back: StencilFaceInfo::default(),
        }
    }

    /// Helper: default blend (disabled).
    fn default_blend() -> BlendInfo {
        BlendInfo::default()
    }

    /// Helper: default blend color.
    fn default_blend_color() -> BlendColorInfo {
        BlendColorInfo {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        }
    }

    /// Helper: default color mask (all enabled).
    fn default_color_mask() -> ColorMaskInfo {
        ColorMaskInfo::default()
    }

    #[test]
    fn test_topology_triangles() {
        let positions: Vec<[f32; 4]> = (0..6).map(|i| [i as f32, 0.0, 0.0, 1.0]).collect();
        let tris = process_topology(&positions, PrimitiveTopology::Triangles);
        assert_eq!(tris.len(), 2);
        assert_eq!(tris[0], [0, 1, 2]);
        assert_eq!(tris[1], [3, 4, 5]);
    }

    #[test]
    fn test_topology_triangle_strip() {
        let positions: Vec<[f32; 4]> = (0..5).map(|i| [i as f32, 0.0, 0.0, 1.0]).collect();
        let tris = process_topology(&positions, PrimitiveTopology::TriangleStrip);
        assert_eq!(tris.len(), 3);
        assert_eq!(tris[0], [0, 1, 2]);
        assert_eq!(tris[1], [1, 3, 2]); // winding swap
        assert_eq!(tris[2], [2, 3, 4]);
    }

    #[test]
    fn test_topology_triangle_fan() {
        let positions: Vec<[f32; 4]> = (0..4).map(|i| [i as f32, 0.0, 0.0, 1.0]).collect();
        let tris = process_topology(&positions, PrimitiveTopology::TriangleFan);
        assert_eq!(tris.len(), 2);
        assert_eq!(tris[0], [0, 1, 2]);
        assert_eq!(tris[1], [0, 2, 3]);
    }

    #[test]
    fn test_topology_quads() {
        let positions: Vec<[f32; 4]> = (0..8).map(|i| [i as f32, 0.0, 0.0, 1.0]).collect();
        let tris = process_topology(&positions, PrimitiveTopology::Quads);
        assert_eq!(tris.len(), 4);
        assert_eq!(tris[0], [0, 1, 2]);
        assert_eq!(tris[1], [0, 2, 3]);
        assert_eq!(tris[2], [4, 5, 6]);
        assert_eq!(tris[3], [4, 6, 7]);
    }

    #[test]
    fn test_viewport_transform() {
        // NDC (0,0) should map to viewport center.
        let result = viewport_transform([0.0, 0.0, 0.0, 1.0], 0.0, 0.0, 1280.0, 720.0);
        assert!((result[0] - 640.0).abs() < 0.01);
        assert!((result[1] - 360.0).abs() < 0.01);

        // NDC (-1,-1) should map to top-left.
        let result = viewport_transform([-1.0, -1.0, 0.0, 1.0], 0.0, 0.0, 1280.0, 720.0);
        assert!((result[0] - 0.0).abs() < 0.01);
        assert!((result[1] - 0.0).abs() < 0.01);

        // NDC (1,1) should map to bottom-right.
        let result = viewport_transform([1.0, 1.0, 0.0, 1.0], 0.0, 0.0, 1280.0, 720.0);
        assert!((result[0] - 1280.0).abs() < 0.01);
        assert!((result[1] - 720.0).abs() < 0.01);
    }

    #[test]
    fn test_viewport_transform_preserves_z() {
        let result = viewport_transform([0.0, 0.0, 0.75, 1.0], 0.0, 0.0, 100.0, 100.0);
        assert!((result[2] - 0.75).abs() < f32::EPSILON);
    }

    #[test]
    fn test_viewport_transform_preserves_w() {
        let result = viewport_transform([0.0, 0.0, 0.5, 2.0], 0.0, 0.0, 100.0, 100.0);
        assert!((result[3] - 2.0).abs() < f32::EPSILON);
        // W=0 should default to 1.
        let result = viewport_transform([0.0, 0.0, 0.5, 0.0], 0.0, 0.0, 100.0, 100.0);
        assert!((result[3] - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_rasterize_triangle_fills_pixels() {
        let width = 10u32;
        let height = 10u32;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let mut depth_buf = vec![1.0f32; (width * height) as usize];
        let mut stencil_buf = vec![0u8; (width * height) as usize];
        let white = [1.0f32, 1.0, 1.0, 1.0];

        // A triangle covering the upper-left quadrant.
        let v0 = [0.0, 0.0, 0.0, 1.0];
        let v1 = [5.0, 0.0, 0.0, 1.0];
        let v2 = [0.0, 5.0, 0.0, 1.0];
        rasterize_triangle(
            v0,
            v1,
            v2,
            white,
            white,
            white,
            [0.0, 0.0],
            [0.0, 0.0],
            [0.0, 0.0],
            None,
            &mut pixels,
            &mut depth_buf,
            &mut stencil_buf,
            width,
            height,
            &default_scissor(),
            &default_depth_stencil(),
            &default_blend(),
            &default_blend_color(),
            &default_color_mask(),
        );

        // Pixel (1, 1) should be inside the triangle.
        let offset = ((1 * width + 1) * 4) as usize;
        assert_eq!(&pixels[offset..offset + 4], &[255, 255, 255, 255]);

        // Pixel (9, 9) should NOT be filled.
        let offset = ((9 * width + 9) * 4) as usize;
        assert_eq!(&pixels[offset..offset + 4], &[0, 0, 0, 0]);
    }

    #[test]
    fn test_rasterize_triangle_clips_to_bounds() {
        let width = 4u32;
        let height = 4u32;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let mut depth_buf = vec![1.0f32; (width * height) as usize];
        let mut stencil_buf = vec![0u8; (width * height) as usize];
        let white = [1.0f32, 1.0, 1.0, 1.0];

        // Triangle extending way past framebuffer edges.
        let v0 = [-10.0, -10.0, 0.0, 1.0];
        let v1 = [100.0, -10.0, 0.0, 1.0];
        let v2 = [-10.0, 100.0, 0.0, 1.0];
        rasterize_triangle(
            v0,
            v1,
            v2,
            white,
            white,
            white,
            [0.0, 0.0],
            [0.0, 0.0],
            [0.0, 0.0],
            None,
            &mut pixels,
            &mut depth_buf,
            &mut stencil_buf,
            width,
            height,
            &default_scissor(),
            &default_depth_stencil(),
            &default_blend(),
            &default_blend_color(),
            &default_color_mask(),
        );

        // All pixels should be filled (triangle covers everything).
        for y in 0..height {
            for x in 0..width {
                let offset = ((y * width + x) * 4) as usize;
                assert_eq!(
                    &pixels[offset..offset + 4],
                    &[255, 255, 255, 255],
                    "pixel ({}, {}) should be filled",
                    x,
                    y
                );
            }
        }
    }

    #[test]
    fn test_render_draw_calls_no_draws() {
        let base_fb = Framebuffer {
            gpu_va: 0x1000,
            width: 4,
            height: 4,
            pixels: vec![42u8; 64],
        };
        let result =
            SoftwareRasterizer::render_draw_calls(&[], &|_, _| {}, &|_, _| {}, Some(base_fb));
        let fb = result.unwrap();
        assert_eq!(fb.width, 4);
        assert_eq!(fb.height, 4);
        // Pixels should be unchanged.
        assert!(fb.pixels.iter().all(|&p| p == 42));
    }

    #[test]
    fn test_render_draw_calls_with_base_fb() {
        // Create vertex data: a triangle at NDC positions covering the framebuffer.
        // 3 vertices, R32G32 format = 2 floats * 4 bytes = 8 bytes each
        let mut vertex_data = vec![0u8; 24];
        // v0: (-1, -1)
        vertex_data[0..4].copy_from_slice(&(-1.0f32).to_le_bytes());
        vertex_data[4..8].copy_from_slice(&(-1.0f32).to_le_bytes());
        // v1: (1, -1)
        vertex_data[8..12].copy_from_slice(&(1.0f32).to_le_bytes());
        vertex_data[12..16].copy_from_slice(&(-1.0f32).to_le_bytes());
        // v2: (-1, 1)
        vertex_data[16..20].copy_from_slice(&(-1.0f32).to_le_bytes());
        vertex_data[20..24].copy_from_slice(&(1.0f32).to_le_bytes());

        let vertex_data_clone = vertex_data.clone();
        let read_gpu = move |addr: u64, buf: &mut [u8]| {
            if addr >= 0x5000 && addr < 0x5000 + vertex_data_clone.len() as u64 {
                let off = (addr - 0x5000) as usize;
                let len = buf.len().min(vertex_data_clone.len() - off);
                buf[..len].copy_from_slice(&vertex_data_clone[off..off + len]);
            }
        };

        let mut draw = default_draw_call();
        draw.vertex_count = 3;
        draw.vertex_streams[0] = VertexStreamInfo {
            index: 0,
            address: 0x5000,
            stride: 8,
            frequency: 0,
            enabled: true,
        };
        draw.vertex_attribs[0] = VertexAttribInfo {
            buffer_index: 0,
            constant: false,
            offset: 0,
            size: VertexAttribSize::R32G32,
            attrib_type: VertexAttribType::Float,
            bgra: false,
        };
        draw.viewports[0] = ViewportInfo {
            x: 0.0,
            y: 0.0,
            width: 4.0,
            height: 4.0,
            depth_near: 0.0,
            depth_far: 1.0,
        };

        let base_fb = Framebuffer {
            gpu_va: 0x1000,
            width: 4,
            height: 4,
            pixels: vec![0u8; 64], // black
        };

        let result =
            SoftwareRasterizer::render_draw_calls(&[draw], &read_gpu, &|_, _| {}, Some(base_fb));
        let fb = result.unwrap();
        assert_eq!(fb.width, 4);
        assert_eq!(fb.height, 4);
        // At least some pixels should be white (255).
        assert!(fb.pixels.iter().any(|&p| p == 255));
    }

    #[test]
    fn test_parse_indices_unsigned_byte() {
        let data = vec![0u8, 1, 2, 3];
        let indices = parse_indices(&data, IndexFormat::UnsignedByte);
        assert_eq!(indices, vec![0, 1, 2, 3]);
    }

    #[test]
    fn test_parse_indices_unsigned_short() {
        let mut data = vec![0u8; 6];
        data[0..2].copy_from_slice(&0u16.to_le_bytes());
        data[2..4].copy_from_slice(&100u16.to_le_bytes());
        data[4..6].copy_from_slice(&200u16.to_le_bytes());
        let indices = parse_indices(&data, IndexFormat::UnsignedShort);
        assert_eq!(indices, vec![0, 100, 200]);
    }

    #[test]
    fn test_parse_indices_unsigned_int() {
        let mut data = vec![0u8; 8];
        data[0..4].copy_from_slice(&42u32.to_le_bytes());
        data[4..8].copy_from_slice(&99u32.to_le_bytes());
        let indices = parse_indices(&data, IndexFormat::UnsignedInt);
        assert_eq!(indices, vec![42, 99]);
    }

    // --- Phase 25 new tests ---

    #[test]
    fn test_depth_test_passes_helper() {
        assert!(!depth_test_passes(ComparisonOp::Never, 0.5, 1.0));
        assert!(depth_test_passes(ComparisonOp::Always, 0.5, 1.0));
        assert!(depth_test_passes(ComparisonOp::Less, 0.3, 0.5));
        assert!(!depth_test_passes(ComparisonOp::Less, 0.5, 0.3));
        assert!(!depth_test_passes(ComparisonOp::Less, 0.5, 0.5));
        assert!(depth_test_passes(ComparisonOp::Equal, 0.5, 0.5));
        assert!(!depth_test_passes(ComparisonOp::Equal, 0.5, 0.6));
        assert!(depth_test_passes(ComparisonOp::LessEqual, 0.5, 0.5));
        assert!(depth_test_passes(ComparisonOp::LessEqual, 0.3, 0.5));
        assert!(!depth_test_passes(ComparisonOp::LessEqual, 0.6, 0.5));
        assert!(depth_test_passes(ComparisonOp::Greater, 0.6, 0.5));
        assert!(!depth_test_passes(ComparisonOp::Greater, 0.3, 0.5));
        assert!(depth_test_passes(ComparisonOp::NotEqual, 0.3, 0.5));
        assert!(!depth_test_passes(ComparisonOp::NotEqual, 0.5, 0.5));
        assert!(depth_test_passes(ComparisonOp::GreaterEqual, 0.5, 0.5));
        assert!(depth_test_passes(ComparisonOp::GreaterEqual, 0.6, 0.5));
        assert!(!depth_test_passes(ComparisonOp::GreaterEqual, 0.3, 0.5));
    }

    #[test]
    fn test_blend_equation_variants() {
        assert!((blend_equation(BlendEquation::Add, 0.3, 0.5) - 0.8).abs() < 0.001);
        assert!((blend_equation(BlendEquation::Subtract, 0.5, 0.3) - 0.2).abs() < 0.001);
        assert!((blend_equation(BlendEquation::ReverseSubtract, 0.3, 0.5) - 0.2).abs() < 0.001);
        assert!((blend_equation(BlendEquation::Min, 0.3, 0.5) - 0.3).abs() < 0.001);
        assert!((blend_equation(BlendEquation::Max, 0.3, 0.5) - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_vertex_color_interpolation() {
        // Triangle at screen coords with red, green, blue vertices.
        let width = 10u32;
        let height = 10u32;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let mut depth_buf = vec![1.0f32; (width * height) as usize];
        let mut stencil_buf = vec![0u8; (width * height) as usize];

        let v0 = [1.0, 1.0, 0.0, 1.0];
        let v1 = [9.0, 1.0, 0.0, 1.0];
        let v2 = [5.0, 9.0, 0.0, 1.0];

        let c0 = [1.0, 0.0, 0.0, 1.0]; // red
        let c1 = [0.0, 1.0, 0.0, 1.0]; // green
        let c2 = [0.0, 0.0, 1.0, 1.0]; // blue

        rasterize_triangle(
            v0,
            v1,
            v2,
            c0,
            c1,
            c2,
            [0.0, 0.0],
            [0.0, 0.0],
            [0.0, 0.0],
            None,
            &mut pixels,
            &mut depth_buf,
            &mut stencil_buf,
            width,
            height,
            &default_scissor(),
            &default_depth_stencil(),
            &default_blend(),
            &default_blend_color(),
            &default_color_mask(),
        );

        // Center pixel (~5, ~4) should have a mix of colors (not pure white or black).
        let cx = 5u32;
        let cy = 4u32;
        let off = ((cy * width + cx) * 4) as usize;
        let r = pixels[off];
        let g = pixels[off + 1];
        let b = pixels[off + 2];
        // Should have contributions from all three vertices.
        assert!(r > 0, "red channel should be > 0");
        assert!(g > 0, "green channel should be > 0");
        assert!(b > 0, "blue channel should be > 0");
        // No single channel should be 255 at the centroid.
        assert!(
            r < 255 || g < 255 || b < 255,
            "should be a mix, not pure white"
        );
    }

    #[test]
    fn test_depth_test_less() {
        let width = 4u32;
        let height = 4u32;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let mut depth_buf = vec![1.0f32; (width * height) as usize];
        let mut stencil_buf = vec![0u8; (width * height) as usize];
        let ds = DepthStencilInfo {
            depth_test_enable: true,
            depth_write_enable: true,
            depth_func: ComparisonOp::Less,
            depth_mode: DepthMode::ZeroToOne,
            stencil_enable: false,
            stencil_two_side: false,
            front: StencilFaceInfo::default(),
            back: StencilFaceInfo::default(),
        };

        // First triangle at z=0.5, red.
        let v0 = [-1.0, -1.0, 0.5, 1.0];
        let v1 = [20.0, -1.0, 0.5, 1.0];
        let v2 = [-1.0, 20.0, 0.5, 1.0];
        let red = [1.0, 0.0, 0.0, 1.0];
        rasterize_triangle(
            v0,
            v1,
            v2,
            red,
            red,
            red,
            [0.0, 0.0],
            [0.0, 0.0],
            [0.0, 0.0],
            None,
            &mut pixels,
            &mut depth_buf,
            &mut stencil_buf,
            width,
            height,
            &default_scissor(),
            &ds,
            &default_blend(),
            &default_blend_color(),
            &default_color_mask(),
        );

        let off = ((1 * width + 1) * 4) as usize;
        assert_eq!(pixels[off], 255); // red
        assert_eq!(pixels[off + 1], 0);

        // Second triangle at z=0.3 (nearer), green — should win.
        let green = [0.0, 1.0, 0.0, 1.0];
        let v0b = [-1.0, -1.0, 0.3, 1.0];
        let v1b = [20.0, -1.0, 0.3, 1.0];
        let v2b = [-1.0, 20.0, 0.3, 1.0];
        rasterize_triangle(
            v0b,
            v1b,
            v2b,
            green,
            green,
            green,
            [0.0, 0.0],
            [0.0, 0.0],
            [0.0, 0.0],
            None,
            &mut pixels,
            &mut depth_buf,
            &mut stencil_buf,
            width,
            height,
            &default_scissor(),
            &ds,
            &default_blend(),
            &default_blend_color(),
            &default_color_mask(),
        );

        assert_eq!(pixels[off], 0);
        assert_eq!(pixels[off + 1], 255); // green overwrote

        // Third triangle at z=0.8 (farther), blue — should be rejected.
        let blue = [0.0, 0.0, 1.0, 1.0];
        let v0c = [-1.0, -1.0, 0.8, 1.0];
        let v1c = [20.0, -1.0, 0.8, 1.0];
        let v2c = [-1.0, 20.0, 0.8, 1.0];
        rasterize_triangle(
            v0c,
            v1c,
            v2c,
            blue,
            blue,
            blue,
            [0.0, 0.0],
            [0.0, 0.0],
            [0.0, 0.0],
            None,
            &mut pixels,
            &mut depth_buf,
            &mut stencil_buf,
            width,
            height,
            &default_scissor(),
            &ds,
            &default_blend(),
            &default_blend_color(),
            &default_color_mask(),
        );

        assert_eq!(pixels[off], 0);
        assert_eq!(pixels[off + 1], 255); // still green
        assert_eq!(pixels[off + 2], 0); // blue rejected
    }

    #[test]
    fn test_depth_test_disabled() {
        let width = 4u32;
        let height = 4u32;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let mut depth_buf = vec![1.0f32; (width * height) as usize];
        let mut stencil_buf = vec![0u8; (width * height) as usize];
        let ds = default_depth_stencil(); // disabled

        // First triangle: red at z=0.3.
        let red = [1.0, 0.0, 0.0, 1.0];
        rasterize_triangle(
            [-1.0, -1.0, 0.3, 1.0],
            [20.0, -1.0, 0.3, 1.0],
            [-1.0, 20.0, 0.3, 1.0],
            red,
            red,
            red,
            [0.0, 0.0],
            [0.0, 0.0],
            [0.0, 0.0],
            None,
            &mut pixels,
            &mut depth_buf,
            &mut stencil_buf,
            width,
            height,
            &default_scissor(),
            &ds,
            &default_blend(),
            &default_blend_color(),
            &default_color_mask(),
        );

        // Second triangle: blue at z=0.8 (farther). Should still overwrite because depth disabled.
        let blue = [0.0, 0.0, 1.0, 1.0];
        rasterize_triangle(
            [-1.0, -1.0, 0.8, 1.0],
            [20.0, -1.0, 0.8, 1.0],
            [-1.0, 20.0, 0.8, 1.0],
            blue,
            blue,
            blue,
            [0.0, 0.0],
            [0.0, 0.0],
            [0.0, 0.0],
            None,
            &mut pixels,
            &mut depth_buf,
            &mut stencil_buf,
            width,
            height,
            &default_scissor(),
            &ds,
            &default_blend(),
            &default_blend_color(),
            &default_color_mask(),
        );

        let off = ((1 * width + 1) * 4) as usize;
        assert_eq!(pixels[off], 0);
        assert_eq!(pixels[off + 2], 255); // blue overwrote
    }

    #[test]
    fn test_alpha_blend_src_alpha() {
        let width = 4u32;
        let height = 4u32;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let mut depth_buf = vec![1.0f32; (width * height) as usize];
        let mut stencil_buf = vec![0u8; (width * height) as usize];

        // Fill background with solid red.
        let red = [1.0, 0.0, 0.0, 1.0];
        rasterize_triangle(
            [-1.0, -1.0, 0.0, 1.0],
            [20.0, -1.0, 0.0, 1.0],
            [-1.0, 20.0, 0.0, 1.0],
            red,
            red,
            red,
            [0.0, 0.0],
            [0.0, 0.0],
            [0.0, 0.0],
            None,
            &mut pixels,
            &mut depth_buf,
            &mut stencil_buf,
            width,
            height,
            &default_scissor(),
            &default_depth_stencil(),
            &default_blend(),
            &default_blend_color(),
            &default_color_mask(),
        );

        // Overlay with 50% transparent green.
        let blend = BlendInfo {
            enabled: true,
            separate_alpha: false,
            color_op: BlendEquation::Add,
            color_src: BlendFactor::SrcAlpha,
            color_dst: BlendFactor::OneMinusSrcAlpha,
            alpha_op: BlendEquation::Add,
            alpha_src: BlendFactor::One,
            alpha_dst: BlendFactor::Zero,
        };
        let green_half = [0.0, 1.0, 0.0, 0.5];
        rasterize_triangle(
            [-1.0, -1.0, 0.0, 1.0],
            [20.0, -1.0, 0.0, 1.0],
            [-1.0, 20.0, 0.0, 1.0],
            green_half,
            green_half,
            green_half,
            [0.0, 0.0],
            [0.0, 0.0],
            [0.0, 0.0],
            None,
            &mut pixels,
            &mut depth_buf,
            &mut stencil_buf,
            width,
            height,
            &default_scissor(),
            &default_depth_stencil(),
            &blend,
            &default_blend_color(),
            &default_color_mask(),
        );

        let off = ((1 * width + 1) * 4) as usize;
        let r = pixels[off];
        let g = pixels[off + 1];
        // Red should be about 127 (0.5 * 255), green about 127.
        assert!(r > 100 && r < 155, "red={}, expected ~127", r);
        assert!(g > 100 && g < 155, "green={}, expected ~127", g);
    }

    #[test]
    fn test_scissor_clipping() {
        let width = 10u32;
        let height = 10u32;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let mut depth_buf = vec![1.0f32; (width * height) as usize];
        let mut stencil_buf = vec![0u8; (width * height) as usize];
        let white = [1.0f32, 1.0, 1.0, 1.0];

        let scissor = ScissorInfo {
            enabled: true,
            min_x: 3,
            max_x: 7, // exclusive upper bound
            min_y: 3,
            max_y: 7,
        };

        // Big triangle covering entire framebuffer.
        rasterize_triangle(
            [-1.0, -1.0, 0.0, 1.0],
            [20.0, -1.0, 0.0, 1.0],
            [-1.0, 20.0, 0.0, 1.0],
            white,
            white,
            white,
            [0.0, 0.0],
            [0.0, 0.0],
            [0.0, 0.0],
            None,
            &mut pixels,
            &mut depth_buf,
            &mut stencil_buf,
            width,
            height,
            &scissor,
            &default_depth_stencil(),
            &default_blend(),
            &default_blend_color(),
            &default_color_mask(),
        );

        // Pixel (1, 1) should be empty (outside scissor).
        let off_outside = ((1 * width + 1) * 4) as usize;
        assert_eq!(pixels[off_outside], 0);

        // Pixel (4, 4) should be filled (inside scissor).
        let off_inside = ((4 * width + 4) * 4) as usize;
        assert_eq!(pixels[off_inside], 255);

        // Pixel (8, 8) should be empty (outside scissor).
        let off_far = ((8 * width + 8) * 4) as usize;
        assert_eq!(pixels[off_far], 0);
    }

    #[test]
    fn test_color_write_mask() {
        let width = 4u32;
        let height = 4u32;
        // Fill with green (0, 128, 0, 255).
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        for i in 0..(width * height) as usize {
            pixels[i * 4] = 0;
            pixels[i * 4 + 1] = 128;
            pixels[i * 4 + 2] = 0;
            pixels[i * 4 + 3] = 255;
        }
        let mut depth_buf = vec![1.0f32; (width * height) as usize];
        let mut stencil_buf = vec![0u8; (width * height) as usize];

        // Write white but mask out green and alpha channels.
        let mask = ColorMaskInfo {
            r: true,
            g: false,
            b: true,
            a: false,
        };
        let white = [1.0f32, 1.0, 1.0, 1.0];

        rasterize_triangle(
            [-1.0, -1.0, 0.0, 1.0],
            [20.0, -1.0, 0.0, 1.0],
            [-1.0, 20.0, 0.0, 1.0],
            white,
            white,
            white,
            [0.0, 0.0],
            [0.0, 0.0],
            [0.0, 0.0],
            None,
            &mut pixels,
            &mut depth_buf,
            &mut stencil_buf,
            width,
            height,
            &default_scissor(),
            &default_depth_stencil(),
            &default_blend(),
            &default_blend_color(),
            &mask,
        );

        let off = ((1 * width + 1) * 4) as usize;
        assert_eq!(pixels[off], 255); // R written
        assert_eq!(pixels[off + 1], 128); // G preserved
        assert_eq!(pixels[off + 2], 255); // B written
        assert_eq!(pixels[off + 3], 255); // A preserved
    }

    #[test]
    fn test_backface_culling() {
        // signed_area([0,0,0,1], [1,0,0,1], [0,1,0,1]) = (1)*(1) - (0)*(0) = 1.0 (positive = CCW).
        let area = signed_area(
            [0.0, 0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, 1.0],
        );
        assert!(area > 0.0); // Positive = CCW winding

        // CCW front_face + Back culling: positive area = CCW = front → NOT culled.
        let rast_ccw_back = RasterizerInfo {
            cull_enable: true,
            front_face: FrontFace::CCW,
            cull_face: CullFace::Back,
            polygon_mode_front: PolygonMode::Fill,
            polygon_mode_back: PolygonMode::Fill,
            line_width_smooth: 1.0,
            line_width_aliased: 1.0,
            depth_bias: 0.0,
            slope_scale_depth_bias: 0.0,
            depth_bias_clamp: 0.0,
            ..RasterizerInfo::default()
        };
        assert!(!should_cull(area, &rast_ccw_back));

        // CW front_face + Back culling: positive area = CCW ≠ CW → back face → IS culled.
        let rast_cw_back = RasterizerInfo {
            cull_enable: true,
            front_face: FrontFace::CW,
            cull_face: CullFace::Back,
            polygon_mode_front: PolygonMode::Fill,
            polygon_mode_back: PolygonMode::Fill,
            line_width_smooth: 1.0,
            line_width_aliased: 1.0,
            depth_bias: 0.0,
            slope_scale_depth_bias: 0.0,
            depth_bias_clamp: 0.0,
            ..RasterizerInfo::default()
        };
        assert!(should_cull(area, &rast_cw_back));

        // CCW front_face + Front culling: positive area = CCW = front → IS culled.
        let rast_ccw_front = RasterizerInfo {
            cull_enable: true,
            front_face: FrontFace::CCW,
            cull_face: CullFace::Front,
            polygon_mode_front: PolygonMode::Fill,
            polygon_mode_back: PolygonMode::Fill,
            line_width_smooth: 1.0,
            line_width_aliased: 1.0,
            depth_bias: 0.0,
            slope_scale_depth_bias: 0.0,
            depth_bias_clamp: 0.0,
            ..RasterizerInfo::default()
        };
        assert!(should_cull(area, &rast_ccw_front));

        // Cull disabled — never culled.
        let rast_off = RasterizerInfo {
            cull_enable: false,
            front_face: FrontFace::CCW,
            cull_face: CullFace::Back,
            polygon_mode_front: PolygonMode::Fill,
            polygon_mode_back: PolygonMode::Fill,
            line_width_smooth: 1.0,
            line_width_aliased: 1.0,
            depth_bias: 0.0,
            slope_scale_depth_bias: 0.0,
            depth_bias_clamp: 0.0,
            ..RasterizerInfo::default()
        };
        assert!(!should_cull(area, &rast_off));

        // FrontAndBack culls everything.
        let rast_both = RasterizerInfo {
            cull_enable: true,
            front_face: FrontFace::CCW,
            cull_face: CullFace::FrontAndBack,
            polygon_mode_front: PolygonMode::Fill,
            polygon_mode_back: PolygonMode::Fill,
            line_width_smooth: 1.0,
            line_width_aliased: 1.0,
            depth_bias: 0.0,
            slope_scale_depth_bias: 0.0,
            depth_bias_clamp: 0.0,
            ..RasterizerInfo::default()
        };
        assert!(should_cull(area, &rast_both));
    }

    #[test]
    fn test_no_color_attrib_defaults_white() {
        // Draw call with only a position attribute — no color.
        let mut vertex_data = vec![0u8; 24];
        vertex_data[0..4].copy_from_slice(&(-1.0f32).to_le_bytes());
        vertex_data[4..8].copy_from_slice(&(-1.0f32).to_le_bytes());
        vertex_data[8..12].copy_from_slice(&(1.0f32).to_le_bytes());
        vertex_data[12..16].copy_from_slice(&(-1.0f32).to_le_bytes());
        vertex_data[16..20].copy_from_slice(&(-1.0f32).to_le_bytes());
        vertex_data[20..24].copy_from_slice(&(1.0f32).to_le_bytes());

        let vertex_data_clone = vertex_data.clone();
        let read_gpu = move |addr: u64, buf: &mut [u8]| {
            if addr >= 0x5000 && addr < 0x5000 + vertex_data_clone.len() as u64 {
                let off = (addr - 0x5000) as usize;
                let len = buf.len().min(vertex_data_clone.len() - off);
                buf[..len].copy_from_slice(&vertex_data_clone[off..off + len]);
            }
        };

        let mut draw = default_draw_call();
        draw.vertex_count = 3;
        draw.vertex_streams[0] = VertexStreamInfo {
            index: 0,
            address: 0x5000,
            stride: 8,
            frequency: 0,
            enabled: true,
        };
        draw.vertex_attribs[0] = VertexAttribInfo {
            buffer_index: 0,
            constant: false,
            offset: 0,
            size: VertexAttribSize::R32G32,
            attrib_type: VertexAttribType::Float,
            bgra: false,
        };
        draw.viewports[0] = ViewportInfo {
            x: 0.0,
            y: 0.0,
            width: 4.0,
            height: 4.0,
            depth_near: 0.0,
            depth_far: 1.0,
        };

        let base_fb = Framebuffer {
            gpu_va: 0x1000,
            width: 4,
            height: 4,
            pixels: vec![0u8; 64],
        };

        let result =
            SoftwareRasterizer::render_draw_calls(&[draw], &read_gpu, &|_, _| {}, Some(base_fb));
        let fb = result.unwrap();

        // Filled pixels should be white (255, 255, 255, 255).
        let has_white = fb
            .pixels
            .chunks_exact(4)
            .any(|px| px[0] == 255 && px[1] == 255 && px[2] == 255 && px[3] == 255);
        assert!(has_white, "with no color attribute, pixels should be white");
    }

    #[test]
    fn test_signed_area_winding() {
        // CCW in math coords, but we're in screen space.
        let area = signed_area(
            [0.0, 0.0, 0.0, 1.0],
            [10.0, 0.0, 0.0, 1.0],
            [0.0, 10.0, 0.0, 1.0],
        );
        // (10-0)*(10-0) - (0-0)*(0-0) = 100 > 0
        assert!(area > 0.0);

        // Reversed winding.
        let area_rev = signed_area(
            [0.0, 0.0, 0.0, 1.0],
            [0.0, 10.0, 0.0, 1.0],
            [10.0, 0.0, 0.0, 1.0],
        );
        assert!(area_rev < 0.0);
    }

    // --- Phase 26: Texture Sampling tests ---

    #[test]
    fn test_bytes_per_texel() {
        assert_eq!(bytes_per_texel(TextureFormat::A8B8G8R8), Some(4));
        assert_eq!(bytes_per_texel(TextureFormat::R8G8B8A8), Some(4));
        assert_eq!(bytes_per_texel(TextureFormat::B5G6R5), Some(2));
        assert_eq!(bytes_per_texel(TextureFormat::R8G8), Some(2));
        assert_eq!(bytes_per_texel(TextureFormat::R8), Some(1));
        assert_eq!(bytes_per_texel(TextureFormat::R32G32B32A32), Some(16));
        // Compressed formats return None.
        assert_eq!(bytes_per_texel(TextureFormat::Bc1Rgba), None);
        assert_eq!(bytes_per_texel(TextureFormat::Astc2d4x4), None);
    }

    #[test]
    fn test_decode_texel_a8b8g8r8() {
        // Bytes [R, G, B, A] in LE layout.
        let bytes = [100u8, 150, 200, 255];
        let rgba = decode_texel(&bytes, TextureFormat::A8B8G8R8, ComponentType::UNorm).unwrap();
        assert_eq!(rgba, [100, 150, 200, 255]);
    }

    #[test]
    fn test_decode_texel_b5g6r5() {
        // Pure red: R=31, G=0, B=0 → u16 = 31 << 11 = 0xF800.
        let val: u16 = 0xF800;
        let bytes = val.to_le_bytes();
        let rgba = decode_texel(&bytes, TextureFormat::B5G6R5, ComponentType::UNorm).unwrap();
        assert!(rgba[0] >= 248, "red={}, expected >=248", rgba[0]);
        assert_eq!(rgba[1], 0);
        assert_eq!(rgba[2], 0);
        assert_eq!(rgba[3], 255);
    }

    #[test]
    fn test_decode_texel_r8() {
        let bytes = [42u8];
        let rgba = decode_texel(&bytes, TextureFormat::R8, ComponentType::UNorm).unwrap();
        assert_eq!(rgba, [42, 0, 0, 255]);
    }

    #[test]
    fn test_decode_texel_float() {
        let mut bytes = [0u8; 16];
        bytes[0..4].copy_from_slice(&(0.5f32).to_le_bytes());
        bytes[4..8].copy_from_slice(&(0.25f32).to_le_bytes());
        bytes[8..12].copy_from_slice(&(1.0f32).to_le_bytes());
        bytes[12..16].copy_from_slice(&(0.75f32).to_le_bytes());
        let rgba = decode_texel(&bytes, TextureFormat::R32G32B32A32, ComponentType::Float).unwrap();
        assert_eq!(rgba[0], 128); // 0.5 * 255 = 127.5 → 128
        assert_eq!(rgba[1], 64); // 0.25 * 255 = 63.75 → 64
        assert_eq!(rgba[2], 255); // 1.0 * 255 = 255
        assert_eq!(rgba[3], 191); // 0.75 * 255 = 191.25 → 191
    }

    #[test]
    fn test_apply_swizzle() {
        let rgba = [10, 20, 30, 40];
        // Identity: RGBA → RGBA.
        let identity = [
            SwizzleSource::R,
            SwizzleSource::G,
            SwizzleSource::B,
            SwizzleSource::A,
        ];
        assert_eq!(apply_swizzle(rgba, &identity), [10, 20, 30, 40]);

        // Swap R and B.
        let swap_rb = [
            SwizzleSource::B,
            SwizzleSource::G,
            SwizzleSource::R,
            SwizzleSource::A,
        ];
        assert_eq!(apply_swizzle(rgba, &swap_rb), [30, 20, 10, 40]);

        // Zero and One.
        let special = [
            SwizzleSource::Zero,
            SwizzleSource::OneFloat,
            SwizzleSource::R,
            SwizzleSource::Zero,
        ];
        assert_eq!(apply_swizzle(rgba, &special), [0, 255, 10, 0]);
    }

    #[test]
    fn test_apply_wrap_mode_repeat() {
        // Positive wrap.
        assert_eq!(apply_wrap_mode(4.5, 4, WrapMode::Wrap), 0);
        assert_eq!(apply_wrap_mode(5.0, 4, WrapMode::Wrap), 1);
        // Negative wrap.
        assert_eq!(apply_wrap_mode(-1.0, 4, WrapMode::Wrap), 3);
        assert_eq!(apply_wrap_mode(-0.5, 4, WrapMode::Wrap), 3);
    }

    #[test]
    fn test_apply_wrap_mode_clamp() {
        assert_eq!(apply_wrap_mode(-1.0, 4, WrapMode::ClampToEdge), 0);
        assert_eq!(apply_wrap_mode(0.5, 4, WrapMode::ClampToEdge), 0);
        assert_eq!(apply_wrap_mode(3.5, 4, WrapMode::ClampToEdge), 3);
        assert_eq!(apply_wrap_mode(10.0, 4, WrapMode::ClampToEdge), 3);
    }

    #[test]
    fn test_apply_wrap_mode_mirror() {
        // Within first period (0..4): direct mapping.
        assert_eq!(apply_wrap_mode(1.5, 4, WrapMode::Mirror), 1);
        // In reflected region (4..8): mirrors back.
        assert_eq!(apply_wrap_mode(5.0, 4, WrapMode::Mirror), 2);
        // Negative: mirrors.
        let result = apply_wrap_mode(-1.0, 4, WrapMode::Mirror);
        assert!(result < 4, "mirrored result {} should be < 4", result);
    }

    #[test]
    fn test_sample_texture_basic() {
        // 2x2 texture: red, green, blue, white.
        let data = vec![
            255, 0, 0, 255, // (0,0) red
            0, 255, 0, 255, // (1,0) green
            0, 0, 255, 255, // (0,1) blue
            255, 255, 255, 255, // (1,1) white
        ];
        let tex = SampledTexture {
            data,
            width: 2,
            height: 2,
            wrap_u: WrapMode::ClampToEdge,
            wrap_v: WrapMode::ClampToEdge,
            normalized_coords: false,
            mag_filter: TextureFilter::Nearest,
            mip_levels: Vec::new(),
        };

        // Sample at (0.5, 0.5) → texel (0, 0) = red.
        let c = sample_texture(&tex, 0.5, 0.5);
        assert!((c[0] - 1.0).abs() < 0.01, "R={}", c[0]);
        assert!(c[1] < 0.01);
        assert!(c[2] < 0.01);

        // Sample at (1.5, 0.5) → texel (1, 0) = green.
        let c = sample_texture(&tex, 1.5, 0.5);
        assert!(c[0] < 0.01);
        assert!((c[1] - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_textured_triangle() {
        // Full pipeline: white vertices × solid red 2x2 texture → red pixels.
        let width = 8u32;
        let height = 8u32;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let mut depth_buf = vec![1.0f32; (width * height) as usize];
        let mut stencil_buf = vec![0u8; (width * height) as usize];
        let white = [1.0f32, 1.0, 1.0, 1.0];

        // Solid red 2x2 texture.
        let tex = SampledTexture {
            data: vec![
                255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
            ],
            width: 2,
            height: 2,
            wrap_u: WrapMode::ClampToEdge,
            wrap_v: WrapMode::ClampToEdge,
            normalized_coords: true,
            mag_filter: TextureFilter::Nearest,
            mip_levels: Vec::new(),
        };

        // Large triangle covering the framebuffer, UVs spanning [0,1].
        rasterize_triangle(
            [-1.0, -1.0, 0.0, 1.0],
            [20.0, -1.0, 0.0, 1.0],
            [-1.0, 20.0, 0.0, 1.0],
            white,
            white,
            white,
            [0.0, 0.0],
            [1.0, 0.0],
            [0.0, 1.0],
            Some(&tex),
            &mut pixels,
            &mut depth_buf,
            &mut stencil_buf,
            width,
            height,
            &default_scissor(),
            &default_depth_stencil(),
            &default_blend(),
            &default_blend_color(),
            &default_color_mask(),
        );

        // Center pixel should be red.
        let off = ((4 * width + 4) * 4) as usize;
        assert_eq!(pixels[off], 255, "R should be 255");
        assert_eq!(pixels[off + 1], 0, "G should be 0");
        assert_eq!(pixels[off + 2], 0, "B should be 0");
        assert_eq!(pixels[off + 3], 255, "A should be 255");
    }

    #[test]
    fn test_no_texture_backwards_compat() {
        // No TIC pool → load_texture returns None → identical to Phase 25.
        let draw = default_draw_call();
        assert_eq!(draw.tex_header_pool_addr, 0);
        let tex = load_texture(&draw, &|_, _| {});
        assert!(tex.is_none());
    }

    // --- Phase 27: BlockLinear Deswizzle + Framebuffer Writeback tests ---

    #[test]
    fn test_gob_offset_origin() {
        assert_eq!(gob_offset(0, 0), 0);
    }

    #[test]
    fn test_gob_offset_known_positions() {
        // (16, 0): first 16-byte group in second half of first row pair → 32
        assert_eq!(gob_offset(16, 0), 32);
        // (0, 1): second row of first row-pair → 16
        assert_eq!(gob_offset(0, 1), 16);
        // (0, 2): second row-pair → 64
        assert_eq!(gob_offset(0, 2), 64);
        // (32, 0): second 32-byte half → 256
        assert_eq!(gob_offset(32, 0), 256);
    }

    #[test]
    fn test_gob_offset_range() {
        // All 512 byte positions in a 64×8 GOB must be unique and < 512.
        let mut seen = vec![false; 512];
        for y in 0..8u32 {
            for x in 0..64u32 {
                let off = gob_offset(x, y) as usize;
                assert!(off < 512, "gob_offset({}, {}) = {} >= 512", x, y, off);
                assert!(!seen[off], "duplicate offset {} at ({}, {})", off, x, y);
                seen[off] = true;
            }
        }
        // All 512 positions should be covered.
        assert!(seen.iter().all(|&s| s), "not all GOB offsets covered");
    }

    #[test]
    fn test_align_up() {
        assert_eq!(align_up(0, 64), 0);
        assert_eq!(align_up(1, 64), 64);
        assert_eq!(align_up(64, 64), 64);
        assert_eq!(align_up(65, 64), 128);
        assert_eq!(align_up(100, 8), 104);
    }

    #[test]
    fn test_block_linear_size() {
        // 16×8 RGBA8 (bpt=4): width_bytes=64=1 GOB wide, height=8=1 GOB tall
        // block_height=0 → 1 GOB per block → 64×8 = 512 bytes
        assert_eq!(block_linear_size(16, 8, 4, 0), 512);

        // 32×8 RGBA8: width_bytes=128=2 GOBs wide, height=8=1 GOB tall
        assert_eq!(block_linear_size(32, 8, 4, 0), 1024);

        // 16×16 RGBA8, block_height=1 (2 GOBs/block): block_height_pixels=16
        // aligned_width=64, aligned_height=16 → 64×16=1024
        assert_eq!(block_linear_size(16, 16, 4, 1), 1024);

        // 16×9 RGBA8, block_height=0: aligned_height=16 (rounds 9 up to 8-multiple=16)
        assert_eq!(block_linear_size(16, 9, 4, 0), 1024);
    }

    #[test]
    fn test_deswizzle_block_linear_1x1_gob() {
        // 16×8 RGBA8 texture = exactly 1 GOB. Fill tiled buffer via gob_offset,
        // then deswizzle should produce linear row-major order.
        let width = 16u32;
        let height = 8u32;
        let bpt = 4u32;
        let block_height = 0u32;
        let mut tiled = vec![0u8; 512];

        // Write a unique byte pattern for each pixel using the GOB formula.
        for y in 0..height {
            for x in 0..width {
                let x_bytes = x * bpt;
                let off = gob_offset(x_bytes, y) as usize;
                // Store pixel ID = y * width + x as RGBA.
                let id = (y * width + x) as u8;
                tiled[off] = id;
                tiled[off + 1] = id;
                tiled[off + 2] = id;
                tiled[off + 3] = 255;
            }
        }

        let linear = deswizzle_block_linear(&tiled, width, height, bpt, block_height);
        assert_eq!(linear.len(), (width * height * bpt) as usize);

        // Verify each pixel in linear layout.
        for y in 0..height {
            for x in 0..width {
                let lin_off = ((y * width + x) * bpt) as usize;
                let id = (y * width + x) as u8;
                assert_eq!(linear[lin_off], id, "pixel ({}, {}) R mismatch", x, y);
            }
        }
    }

    #[test]
    fn test_deswizzle_block_linear_multi_gob() {
        // 32×8 RGBA8 = 2 GOB columns, 1 GOB row, block_height=0.
        let width = 32u32;
        let height = 8u32;
        let bpt = 4u32;
        let block_height = 0u32;
        let size = block_linear_size(width, height, bpt, block_height);
        let mut tiled = vec![0u8; size];

        // Write via the full deswizzle address formula.
        let gobs_in_x = align_up(width * bpt, GOB_SIZE_X) / GOB_SIZE_X;
        let bh_gobs = 1u32 << block_height;
        let block_size = gobs_in_x * GOB_SIZE * bh_gobs;

        for y in 0..height {
            for x in 0..width {
                let x_bytes = x * bpt;
                let gob_x = x_bytes / GOB_SIZE_X;
                let gob_y = y / GOB_SIZE_Y;
                let block_y = gob_y / bh_gobs;
                let gob_in_block = gob_y % bh_gobs;
                let base =
                    block_y * block_size + gob_x * GOB_SIZE * bh_gobs + gob_in_block * GOB_SIZE;
                let intra = gob_offset(x_bytes % GOB_SIZE_X, y % GOB_SIZE_Y);
                let off = (base + intra) as usize;
                let id = ((y * width + x) & 0xFF) as u8;
                tiled[off] = id;
                tiled[off + 1] = id;
                tiled[off + 2] = id;
                tiled[off + 3] = 255;
            }
        }

        let linear = deswizzle_block_linear(&tiled, width, height, bpt, block_height);
        for y in 0..height {
            for x in 0..width {
                let lin_off = ((y * width + x) * bpt) as usize;
                let id = ((y * width + x) & 0xFF) as u8;
                assert_eq!(linear[lin_off], id, "pixel ({}, {}) mismatch", x, y);
            }
        }
    }

    #[test]
    fn test_deswizzle_block_linear_block_height_1() {
        // 16×16 RGBA8, block_height=1 (2 GOBs per block = 16-pixel-tall blocks).
        let width = 16u32;
        let height = 16u32;
        let bpt = 4u32;
        let block_height = 1u32;
        let size = block_linear_size(width, height, bpt, block_height);
        let mut tiled = vec![0u8; size];

        let gobs_in_x = align_up(width * bpt, GOB_SIZE_X) / GOB_SIZE_X;
        let bh_gobs = 1u32 << block_height;
        let block_size = gobs_in_x * GOB_SIZE * bh_gobs;

        for y in 0..height {
            for x in 0..width {
                let x_bytes = x * bpt;
                let gob_x = x_bytes / GOB_SIZE_X;
                let gob_y = y / GOB_SIZE_Y;
                let block_y = gob_y / bh_gobs;
                let gob_in_block = gob_y % bh_gobs;
                let base =
                    block_y * block_size + gob_x * GOB_SIZE * bh_gobs + gob_in_block * GOB_SIZE;
                let intra = gob_offset(x_bytes % GOB_SIZE_X, y % GOB_SIZE_Y);
                let off = (base + intra) as usize;
                let id = ((y * width + x) & 0xFF) as u8;
                tiled[off] = id;
                tiled[off + 1] = !id;
                tiled[off + 2] = 0;
                tiled[off + 3] = 255;
            }
        }

        let linear = deswizzle_block_linear(&tiled, width, height, bpt, block_height);
        for y in 0..height {
            for x in 0..width {
                let lin_off = ((y * width + x) * bpt) as usize;
                let id = ((y * width + x) & 0xFF) as u8;
                assert_eq!(linear[lin_off], id, "R mismatch at ({}, {})", x, y);
                assert_eq!(linear[lin_off + 1], !id, "G mismatch at ({}, {})", x, y);
            }
        }
    }

    #[test]
    fn test_load_texture_block_linear() {
        // Build a BlockLinear 16×8 RGBA8 texture and verify load_texture deswizzles it.
        let width = 16u32;
        let height = 8u32;
        let bpt = 4u32;
        let block_height = 0u32;
        let mut tiled = vec![0u8; 512];

        // Fill tiled buffer: all pixels = solid green (0, 255, 0, 255).
        for y in 0..height {
            for x in 0..width {
                let x_bytes = x * bpt;
                let off = gob_offset(x_bytes, y) as usize;
                tiled[off] = 0;
                tiled[off + 1] = 255;
                tiled[off + 2] = 0;
                tiled[off + 3] = 255;
            }
        }

        // Build TIC words for BlockLinear A8B8G8R8 16×8 texture.
        let mut tic_words = [0u32; 8];
        // word0: format=A8B8G8R8(0x1D), types=UNorm(2), swizzle=RGBA(2,3,4,5)
        tic_words[0] = 0x1D
            | (2 << 7)
            | (2 << 10)
            | (2 << 13)
            | (2 << 16)
            | (2 << 19)
            | (3 << 22)
            | (4 << 25)
            | (5 << 28);
        // word1: addr_low = 0x2000
        tic_words[1] = 0x2000;
        // word2: header_version = BlockLinear(3) at [23:21]
        tic_words[2] = 3 << 21;
        // word3: block_height at bits [5:3], block_depth=0
        tic_words[3] = block_height << 3;
        // word4: width=15(+1=16), texture_type=Texture2D(1) at [26:23]
        tic_words[4] = 15 | (1 << 23);
        // word5: height=7(+1=8), normalized=1
        tic_words[5] = 7 | (1 << 31);

        let mut tic_bytes = [0u8; 32];
        for (i, &w) in tic_words.iter().enumerate() {
            tic_bytes[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
        }

        let tiled_clone = tiled.clone();
        let tic_clone = tic_bytes;
        let read_gpu = move |addr: u64, buf: &mut [u8]| {
            if addr == 0x1000 {
                // TIC read
                let len = buf.len().min(32);
                buf[..len].copy_from_slice(&tic_clone[..len]);
            } else if addr >= 0x2000 && addr < 0x2000 + tiled_clone.len() as u64 {
                let off = (addr - 0x2000) as usize;
                let len = buf.len().min(tiled_clone.len() - off);
                buf[..len].copy_from_slice(&tiled_clone[off..off + len]);
            }
        };

        let mut draw = default_draw_call();
        draw.tex_header_pool_addr = 0x1000;

        let tex = load_texture(&draw, &read_gpu).expect("should load BlockLinear texture");
        assert_eq!(tex.width, 16);
        assert_eq!(tex.height, 8);
        // All pixels should be green.
        for i in 0..(width * height) as usize {
            assert_eq!(tex.data[i * 4], 0, "R at pixel {}", i);
            assert_eq!(tex.data[i * 4 + 1], 255, "G at pixel {}", i);
            assert_eq!(tex.data[i * 4 + 2], 0, "B at pixel {}", i);
            assert_eq!(tex.data[i * 4 + 3], 255, "A at pixel {}", i);
        }
    }

    #[test]
    fn test_framebuffer_writeback() {
        // render_draw_calls should call write_gpu with framebuffer pixels when gpu_va != 0.
        let base_fb = Framebuffer {
            gpu_va: 0x1000,
            width: 2,
            height: 2,
            pixels: vec![42u8; 16],
        };

        // Need at least one draw call (even with no vertices) to reach writeback.
        let draw = default_draw_call();

        let written = std::sync::Mutex::new(Vec::new());
        let write_gpu = |addr: u64, data: &[u8]| {
            written.lock().unwrap().push((addr, data.to_vec()));
        };

        let result =
            SoftwareRasterizer::render_draw_calls(&[draw], &|_, _| {}, &write_gpu, Some(base_fb));
        let fb = result.unwrap();
        let writes = written.into_inner().unwrap();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].0, 0x1000);
        assert_eq!(writes[0].1, fb.pixels);
    }

    #[test]
    fn test_framebuffer_writeback_no_va() {
        // gpu_va == 0 → no writeback.
        let base_fb = Framebuffer {
            gpu_va: 0,
            width: 2,
            height: 2,
            pixels: vec![0u8; 16],
        };

        let written = std::sync::Mutex::new(Vec::new());
        let write_gpu = |addr: u64, data: &[u8]| {
            written.lock().unwrap().push((addr, data.to_vec()));
        };

        SoftwareRasterizer::render_draw_calls(&[], &|_, _| {}, &write_gpu, Some(base_fb));
        let writes = written.into_inner().unwrap();
        assert!(writes.is_empty(), "no writeback should occur when gpu_va=0");
    }

    // --- Phase 28 new tests ---

    // Part A: Bilinear Filtering

    #[test]
    fn test_bilinear_center() {
        // Sampling at exact texel center should match nearest.
        let data = vec![
            255, 0, 0, 255, // (0,0) red
            0, 255, 0, 255, // (1,0) green
            0, 0, 255, 255, // (0,1) blue
            255, 255, 0, 255, // (1,1) yellow
        ];
        let tex = SampledTexture {
            data: data.clone(),
            width: 2,
            height: 2,
            wrap_u: WrapMode::ClampToEdge,
            wrap_v: WrapMode::ClampToEdge,
            normalized_coords: false,
            mag_filter: TextureFilter::Linear,
            mip_levels: Vec::new(),
        };
        // Center of texel (0,0) is at (0.5, 0.5).
        let c = sample_texture_bilinear(&tex, 0.5, 0.5);
        // Should be close to red.
        assert!((c[0] - 1.0).abs() < 0.01, "R={}", c[0]);
        assert!(c[1] < 0.01, "G={}", c[1]);
        assert!(c[2] < 0.01, "B={}", c[2]);
    }

    #[test]
    fn test_bilinear_midpoint() {
        // Sampling between (0,0) red and (1,0) green → average.
        let data = vec![
            255, 0, 0, 255, // (0,0) red
            0, 255, 0, 255, // (1,0) green
            255, 0, 0, 255, // (0,1) red
            0, 255, 0, 255, // (1,1) green
        ];
        let tex = SampledTexture {
            data,
            width: 2,
            height: 2,
            wrap_u: WrapMode::ClampToEdge,
            wrap_v: WrapMode::ClampToEdge,
            normalized_coords: false,
            mag_filter: TextureFilter::Linear,
            mip_levels: Vec::new(),
        };
        // Sample at x=1.0 (between texels 0 and 1), y=0.5 (center of row 0).
        let c = sample_texture_bilinear(&tex, 1.0, 0.5);
        // Should be ~50% red + ~50% green.
        assert!((c[0] - 0.5).abs() < 0.15, "R={}", c[0]);
        assert!((c[1] - 0.5).abs() < 0.15, "G={}", c[1]);
    }

    #[test]
    fn test_bilinear_filter_mode_dispatch() {
        let data = vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
        ];
        let tex_nearest = SampledTexture {
            data: data.clone(),
            width: 2,
            height: 2,
            wrap_u: WrapMode::ClampToEdge,
            wrap_v: WrapMode::ClampToEdge,
            normalized_coords: false,
            mag_filter: TextureFilter::Nearest,
            mip_levels: Vec::new(),
        };
        let tex_linear = SampledTexture {
            data,
            width: 2,
            height: 2,
            wrap_u: WrapMode::ClampToEdge,
            wrap_v: WrapMode::ClampToEdge,
            normalized_coords: false,
            mag_filter: TextureFilter::Linear,
            mip_levels: Vec::new(),
        };
        // At texel center, both should give similar results.
        let cn = sample_texture(&tex_nearest, 0.5, 0.5);
        let cl = sample_texture(&tex_linear, 0.5, 0.5);
        assert!((cn[0] - cl[0]).abs() < 0.02);
    }

    // Part B: Texture Compression

    #[test]
    fn test_block_format_info_values() {
        assert_eq!(block_format_info(TextureFormat::Bc1Rgba), Some((4, 4, 8)));
        assert_eq!(block_format_info(TextureFormat::Bc2), Some((4, 4, 16)));
        assert_eq!(block_format_info(TextureFormat::Bc3), Some((4, 4, 16)));
        assert_eq!(block_format_info(TextureFormat::Bc4), Some((4, 4, 8)));
        assert_eq!(block_format_info(TextureFormat::Bc5), Some((4, 4, 16)));
        assert_eq!(block_format_info(TextureFormat::Bc7), Some((4, 4, 16)));
        assert_eq!(
            block_format_info(TextureFormat::Astc2d4x4),
            Some((4, 4, 16))
        );
        assert_eq!(
            block_format_info(TextureFormat::Astc2d8x8),
            Some((8, 8, 16))
        );
        assert_eq!(
            block_format_info(TextureFormat::Astc2d12x12),
            Some((12, 12, 16))
        );
        // Uncompressed → None.
        assert_eq!(block_format_info(TextureFormat::A8B8G8R8), None);
    }

    #[test]
    fn test_decompress_bc1_solid() {
        // BC1 block where both colors are the same → solid color.
        let mut block = [0u8; 8];
        // Both endpoints = RGB565 pure red (0xF800).
        block[0..2].copy_from_slice(&0xF800u16.to_le_bytes());
        block[2..4].copy_from_slice(&0xF800u16.to_le_bytes());
        // All indices = 0 (use color 0).
        block[4] = 0;
        block[5] = 0;
        block[6] = 0;
        block[7] = 0;

        let result = decompress_bc1_block(&block);
        for texel in &result {
            assert!(texel[0] >= 248, "R={}", texel[0]);
            assert_eq!(texel[1], 0);
            assert_eq!(texel[2], 0);
            assert_eq!(texel[3], 255);
        }
    }

    #[test]
    fn test_decompress_bc1_gradient() {
        // BC1 with c0 > c1 (4-color mode), different colors.
        let mut block = [0u8; 8];
        // c0 = pure white (0xFFFF), c1 = pure black (0x0000).
        block[0..2].copy_from_slice(&0xFFFFu16.to_le_bytes());
        block[2..4].copy_from_slice(&0x0000u16.to_le_bytes());
        // First row indices: 0,1,2,3 = 0b11_10_01_00 = 0xE4.
        block[4] = 0xE4;
        block[5] = 0;
        block[6] = 0;
        block[7] = 0;

        let result = decompress_bc1_block(&block);
        // Index 0 = c0 (white).
        assert!(result[0][0] >= 248);
        // Index 1 = c1 (black).
        assert!(result[1][0] <= 7);
        // Index 2 = 2/3 c0 + 1/3 c1 (light).
        assert!(result[2][0] > 100);
        // Index 3 = 1/3 c0 + 2/3 c1 (dark).
        assert!(result[3][0] < 150);
    }

    #[test]
    fn test_decompress_bc3_alpha() {
        // BC3: first 8 bytes = alpha block, last 8 = BC1 color block.
        let mut block = [0u8; 16];
        // Alpha: a0=255, a1=0, all indices=0 (use a0=255).
        block[0] = 255;
        block[1] = 0;
        // 48 bits of indices, all 0.
        block[2..8].fill(0);
        // Color block: solid white.
        block[8..10].copy_from_slice(&0xFFFFu16.to_le_bytes());
        block[10..12].copy_from_slice(&0xFFFFu16.to_le_bytes());
        block[12..16].fill(0);

        let result = decompress_bc3_block(&block);
        for texel in &result {
            assert_eq!(texel[3], 255, "alpha should be 255");
            assert!(texel[0] >= 248, "should be white-ish");
        }
    }

    #[test]
    fn test_decompress_bc4_single_channel() {
        // BC4: single-channel alpha block.
        let mut block = [0u8; 8];
        block[0] = 200; // a0
        block[1] = 100; // a1
                        // All indices = 0 → use a0=200.
        block[2..8].fill(0);

        let result = decompress_bc4_block(&block);
        for texel in &result {
            assert_eq!(texel[0], 200, "R should be a0=200");
            assert_eq!(texel[1], 0);
            assert_eq!(texel[2], 0);
            assert_eq!(texel[3], 255);
        }
    }

    #[test]
    fn test_decompress_bc5_two_channel() {
        // BC5: two BC4 blocks concatenated.
        let mut block = [0u8; 16];
        // R block: a0=128, a1=64, all indices=0.
        block[0] = 128;
        block[1] = 64;
        block[2..8].fill(0);
        // G block: a0=200, a1=100, all indices=0.
        block[8] = 200;
        block[9] = 100;
        block[10..16].fill(0);

        let result = decompress_bc5_block(&block);
        for texel in &result {
            assert_eq!(texel[0], 128, "R should be 128");
            assert_eq!(texel[1], 200, "G should be 200");
            assert_eq!(texel[2], 0);
            assert_eq!(texel[3], 255);
        }
    }

    #[test]
    fn test_load_texture_compressed() {
        // Build a TIC with BC1 format, 4×4 texture (1 block).
        let mut tic_words = [0u32; 8];
        // word0: format=Bc1Rgba(0x25), types=UNorm(2), swizzle=RGBA(2,3,4,5)
        tic_words[0] = 0x25
            | (2 << 7)
            | (2 << 10)
            | (2 << 13)
            | (2 << 16)
            | (2 << 19)
            | (3 << 22)
            | (4 << 25)
            | (5 << 28);
        // word1: addr_low = 0x2000
        tic_words[1] = 0x2000;
        // word2: header_version = Pitch(2) at [23:21]
        tic_words[2] = 2 << 21;
        // word4: width=3(+1=4), texture_type=Texture2D(1)
        tic_words[4] = 3 | (1 << 23);
        // word5: height=3(+1=4), normalized=1
        tic_words[5] = 3 | (1 << 31);

        let mut tic_bytes = [0u8; 32];
        for (i, &w) in tic_words.iter().enumerate() {
            tic_bytes[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
        }

        // Build a BC1 block: solid red.
        let mut bc1_block = [0u8; 8];
        bc1_block[0..2].copy_from_slice(&0xF800u16.to_le_bytes());
        bc1_block[2..4].copy_from_slice(&0xF800u16.to_le_bytes());

        let tic_clone = tic_bytes;
        let bc1_clone = bc1_block;
        let read_gpu = move |addr: u64, buf: &mut [u8]| {
            if addr == 0x1000 {
                let len = buf.len().min(32);
                buf[..len].copy_from_slice(&tic_clone[..len]);
            } else if addr >= 0x2000 && addr < 0x2000 + 8 {
                let off = (addr - 0x2000) as usize;
                let len = buf.len().min(8 - off);
                buf[..len].copy_from_slice(&bc1_clone[off..off + len]);
            }
        };

        let mut draw = default_draw_call();
        draw.tex_header_pool_addr = 0x1000;

        let tex = load_texture(&draw, &read_gpu).expect("should load BC1 texture");
        assert_eq!(tex.width, 4);
        assert_eq!(tex.height, 4);
        // All pixels should be red.
        for i in 0..16 {
            assert!(
                tex.data[i * 4] >= 248,
                "R at pixel {}: {}",
                i,
                tex.data[i * 4]
            );
            assert_eq!(tex.data[i * 4 + 1], 0, "G at pixel {}", i);
            assert_eq!(tex.data[i * 4 + 2], 0, "B at pixel {}", i);
            assert_eq!(tex.data[i * 4 + 3], 255, "A at pixel {}", i);
        }
    }

    // Part C: Stencil Testing

    #[test]
    fn test_stencil_test_passes_all_ops() {
        // Always / Never.
        assert!(stencil_test_passes(ComparisonOp::Always, 0, 0xFF, 0));
        assert!(!stencil_test_passes(ComparisonOp::Never, 0, 0xFF, 0));
        // Less: ref < stored.
        assert!(stencil_test_passes(ComparisonOp::Less, 5, 0xFF, 10));
        assert!(!stencil_test_passes(ComparisonOp::Less, 10, 0xFF, 5));
        assert!(!stencil_test_passes(ComparisonOp::Less, 5, 0xFF, 5));
        // Equal.
        assert!(stencil_test_passes(ComparisonOp::Equal, 42, 0xFF, 42));
        assert!(!stencil_test_passes(ComparisonOp::Equal, 42, 0xFF, 43));
        // LessEqual.
        assert!(stencil_test_passes(ComparisonOp::LessEqual, 5, 0xFF, 5));
        assert!(stencil_test_passes(ComparisonOp::LessEqual, 4, 0xFF, 5));
        assert!(!stencil_test_passes(ComparisonOp::LessEqual, 6, 0xFF, 5));
        // Greater.
        assert!(stencil_test_passes(ComparisonOp::Greater, 10, 0xFF, 5));
        assert!(!stencil_test_passes(ComparisonOp::Greater, 5, 0xFF, 10));
        // NotEqual.
        assert!(stencil_test_passes(ComparisonOp::NotEqual, 5, 0xFF, 6));
        assert!(!stencil_test_passes(ComparisonOp::NotEqual, 5, 0xFF, 5));
        // GreaterEqual.
        assert!(stencil_test_passes(ComparisonOp::GreaterEqual, 5, 0xFF, 5));
        assert!(stencil_test_passes(ComparisonOp::GreaterEqual, 6, 0xFF, 5));
        assert!(!stencil_test_passes(ComparisonOp::GreaterEqual, 4, 0xFF, 5));
        // Mask: only compare lower 4 bits.
        assert!(stencil_test_passes(ComparisonOp::Equal, 0x15, 0x0F, 0x25));
    }

    #[test]
    fn test_apply_stencil_op_all() {
        assert_eq!(apply_stencil_op(StencilOp::Keep, 0, 42), 42);
        assert_eq!(apply_stencil_op(StencilOp::Zero, 0, 42), 0);
        assert_eq!(apply_stencil_op(StencilOp::Replace, 100, 42), 100);
        assert_eq!(apply_stencil_op(StencilOp::IncrSat, 0, 254), 255);
        assert_eq!(apply_stencil_op(StencilOp::IncrSat, 0, 255), 255); // saturates
        assert_eq!(apply_stencil_op(StencilOp::DecrSat, 0, 1), 0);
        assert_eq!(apply_stencil_op(StencilOp::DecrSat, 0, 0), 0); // saturates
        assert_eq!(apply_stencil_op(StencilOp::Invert, 0, 0x0F), 0xF0);
        assert_eq!(apply_stencil_op(StencilOp::Incr, 0, 255), 0); // wraps
        assert_eq!(apply_stencil_op(StencilOp::Decr, 0, 0), 255); // wraps
    }

    #[test]
    fn test_stencil_reject_pixel() {
        // Stencil test fails → pixel NOT written, fail_op applied.
        let width = 4u32;
        let height = 4u32;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let mut depth_buf = vec![1.0f32; (width * height) as usize];
        let mut stencil_buf = vec![0u8; (width * height) as usize];

        let ds = DepthStencilInfo {
            depth_test_enable: false,
            depth_write_enable: false,
            depth_func: ComparisonOp::Always,
            depth_mode: DepthMode::ZeroToOne,
            stencil_enable: true,
            stencil_two_side: false,
            front: StencilFaceInfo {
                func: ComparisonOp::Never, // always fails
                ref_value: 1,
                func_mask: 0xFF,
                write_mask: 0xFF,
                fail_op: StencilOp::Replace, // write ref on fail
                zfail_op: StencilOp::Keep,
                zpass_op: StencilOp::Keep,
            },
            back: StencilFaceInfo::default(),
        };

        let white = [1.0f32, 1.0, 1.0, 1.0];
        rasterize_triangle(
            [-1.0, -1.0, 0.0, 1.0],
            [20.0, -1.0, 0.0, 1.0],
            [-1.0, 20.0, 0.0, 1.0],
            white,
            white,
            white,
            [0.0, 0.0],
            [0.0, 0.0],
            [0.0, 0.0],
            None,
            &mut pixels,
            &mut depth_buf,
            &mut stencil_buf,
            width,
            height,
            &default_scissor(),
            &ds,
            &default_blend(),
            &default_blend_color(),
            &default_color_mask(),
        );

        // Pixels should remain black (stencil rejected all).
        let off = ((1 * width + 1) * 4) as usize;
        assert_eq!(pixels[off], 0);
        // But stencil buffer should have ref_value=1 (fail_op = Replace).
        let stencil_idx = (1 * width + 1) as usize;
        assert_eq!(
            stencil_buf[stencil_idx], 1,
            "stencil should be written on fail"
        );
    }

    #[test]
    fn test_stencil_depth_fail() {
        // Stencil passes, depth fails → zfail_op applied.
        let width = 4u32;
        let height = 4u32;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let mut depth_buf = vec![0.0f32; (width * height) as usize]; // all z=0
        let mut stencil_buf = vec![0u8; (width * height) as usize];

        let ds = DepthStencilInfo {
            depth_test_enable: true,
            depth_write_enable: false,
            depth_func: ComparisonOp::Less, // frag z=0.5 > stored z=0.0 → fails
            depth_mode: DepthMode::ZeroToOne,
            stencil_enable: true,
            stencil_two_side: false,
            front: StencilFaceInfo {
                func: ComparisonOp::Always, // stencil passes
                ref_value: 42,
                func_mask: 0xFF,
                write_mask: 0xFF,
                fail_op: StencilOp::Keep,
                zfail_op: StencilOp::Replace, // write ref on depth fail
                zpass_op: StencilOp::Keep,
            },
            back: StencilFaceInfo::default(),
        };

        let white = [1.0f32, 1.0, 1.0, 1.0];
        rasterize_triangle(
            [-1.0, -1.0, 0.5, 1.0],
            [20.0, -1.0, 0.5, 1.0],
            [-1.0, 20.0, 0.5, 1.0],
            white,
            white,
            white,
            [0.0, 0.0],
            [0.0, 0.0],
            [0.0, 0.0],
            None,
            &mut pixels,
            &mut depth_buf,
            &mut stencil_buf,
            width,
            height,
            &default_scissor(),
            &ds,
            &default_blend(),
            &default_blend_color(),
            &default_color_mask(),
        );

        // Pixels should remain black (depth test failed).
        let off = ((1 * width + 1) * 4) as usize;
        assert_eq!(pixels[off], 0);
        // Stencil should have zfail_op=Replace → ref_value=42.
        let stencil_idx = (1 * width + 1) as usize;
        assert_eq!(
            stencil_buf[stencil_idx], 42,
            "stencil should have zfail_op applied"
        );
    }

    // Part D: Perspective Correction

    #[test]
    fn test_perspective_correct_uv() {
        // A triangle with varying W should produce different UV interpolation
        // than one with uniform W=1.0.
        let width = 20u32;
        let height = 20u32;
        let mut pixels_uniform = vec![0u8; (width * height * 4) as usize];
        let mut depth_uniform = vec![1.0f32; (width * height) as usize];
        let mut stencil_uniform = vec![0u8; (width * height) as usize];
        let mut pixels_persp = vec![0u8; (width * height * 4) as usize];
        let mut depth_persp = vec![1.0f32; (width * height) as usize];
        let mut stencil_persp = vec![0u8; (width * height) as usize];

        // Gradient texture: left=black, right=white.
        let tex = SampledTexture {
            data: vec![
                0, 0, 0, 255, // (0,0) black
                255, 255, 255, 255, // (1,0) white
                0, 0, 0, 255, // (0,1) black
                255, 255, 255, 255, // (1,1) white
            ],
            width: 2,
            height: 2,
            wrap_u: WrapMode::ClampToEdge,
            wrap_v: WrapMode::ClampToEdge,
            normalized_coords: true,
            mag_filter: TextureFilter::Nearest,
            mip_levels: Vec::new(),
        };

        let white = [1.0f32, 1.0, 1.0, 1.0];

        // Uniform W=1.0: standard screen-space interpolation.
        rasterize_triangle(
            [1.0, 1.0, 0.0, 1.0],
            [18.0, 1.0, 0.0, 1.0],
            [10.0, 18.0, 0.0, 1.0],
            white,
            white,
            white,
            [0.0, 0.5],
            [1.0, 0.5],
            [0.5, 0.5],
            Some(&tex),
            &mut pixels_uniform,
            &mut depth_uniform,
            &mut stencil_uniform,
            width,
            height,
            &default_scissor(),
            &default_depth_stencil(),
            &default_blend(),
            &default_blend_color(),
            &default_color_mask(),
        );

        // Varying W: v0 closer (W=1), v1 farther (W=4).
        rasterize_triangle(
            [1.0, 1.0, 0.0, 1.0],
            [18.0, 1.0, 0.0, 4.0],
            [10.0, 18.0, 0.0, 2.0],
            white,
            white,
            white,
            [0.0, 0.5],
            [1.0, 0.5],
            [0.5, 0.5],
            Some(&tex),
            &mut pixels_persp,
            &mut depth_persp,
            &mut stencil_persp,
            width,
            height,
            &default_scissor(),
            &default_depth_stencil(),
            &default_blend(),
            &default_blend_color(),
            &default_color_mask(),
        );

        // The two renderings should differ at some interior pixel due to perspective correction.
        let mut differs = false;
        for i in 0..pixels_uniform.len() {
            if pixels_uniform[i] != pixels_persp[i] {
                differs = true;
                break;
            }
        }
        assert!(
            differs,
            "perspective correction should produce different results with varying W"
        );
    }

    #[test]
    fn test_mipmap_lod_selection() {
        // Level 0: 4×4 red texture.
        let data_l0 = vec![[255u8, 0, 0, 255]; 16]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        // Level 1: 2×2 green texture.
        let data_l1 = vec![[0u8, 255, 0, 255]; 4]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        // Level 2: 1×1 blue texture.
        let data_l2 = vec![0u8, 0, 255, 255];

        let tex = SampledTexture {
            data: data_l0,
            width: 4,
            height: 4,
            wrap_u: WrapMode::ClampToEdge,
            wrap_v: WrapMode::ClampToEdge,
            normalized_coords: true,
            mag_filter: TextureFilter::Nearest,
            mip_levels: vec![
                MipLevel {
                    data: data_l1,
                    width: 2,
                    height: 2,
                },
                MipLevel {
                    data: data_l2,
                    width: 1,
                    height: 1,
                },
            ],
        };

        // LOD 0 → red (level 0).
        let c0 = sample_texture_lod(&tex, 0.5, 0.5, 0);
        assert!(
            c0[0] > 0.9 && c0[1] < 0.1 && c0[2] < 0.1,
            "LOD 0 should be red"
        );

        // LOD 1 → green (level 1).
        let c1 = sample_texture_lod(&tex, 0.5, 0.5, 1);
        assert!(
            c1[0] < 0.1 && c1[1] > 0.9 && c1[2] < 0.1,
            "LOD 1 should be green"
        );

        // LOD 2 → blue (level 2).
        let c2 = sample_texture_lod(&tex, 0.5, 0.5, 2);
        assert!(
            c2[0] < 0.1 && c2[1] < 0.1 && c2[2] > 0.9,
            "LOD 2 should be blue"
        );

        // LOD beyond max → clamped to highest level (blue).
        let c3 = sample_texture_lod(&tex, 0.5, 0.5, 5);
        assert!(
            c3[0] < 0.1 && c3[1] < 0.1 && c3[2] > 0.9,
            "LOD >max should clamp to last level"
        );
    }

    #[test]
    fn test_max_mip_count() {
        assert_eq!(max_mip_count(1, 1), 1);
        assert_eq!(max_mip_count(2, 2), 2);
        assert_eq!(max_mip_count(4, 4), 3);
        assert_eq!(max_mip_count(256, 256), 9);
        assert_eq!(max_mip_count(1024, 1024), 11);
        assert_eq!(max_mip_count(1, 512), 10);
    }
}
