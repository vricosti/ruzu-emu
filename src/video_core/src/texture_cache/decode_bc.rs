// SPDX-FileCopyrightText: Copyright 2020 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `video_core/texture_cache/decode_bc.h` and `decode_bc.cpp`.

use super::format_lookup_table::PixelFormat;
use super::types::BufferImageCopy;

const BLOCK_SIZE: u32 = 4;

type BcnDecode = unsafe extern "C" fn(*const u8, *mut u8, usize, usize, usize, usize);
type BcnDecodeSigned = unsafe extern "C" fn(*const u8, *mut u8, usize, usize, usize, usize, bool);

extern "C" {
    fn ruzu_decode_bc1_block(
        src: *const u8,
        dst: *mut u8,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
    );
    fn ruzu_decode_bc2_block(
        src: *const u8,
        dst: *mut u8,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
    );
    fn ruzu_decode_bc3_block(
        src: *const u8,
        dst: *mut u8,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
    );
    fn ruzu_decode_bc4_block(
        src: *const u8,
        dst: *mut u8,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        is_signed: bool,
    );
    fn ruzu_decode_bc5_block(
        src: *const u8,
        dst: *mut u8,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        is_signed: bool,
    );
    fn ruzu_decode_bc6_block(
        src: *const u8,
        dst: *mut u8,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        is_signed: bool,
    );
    fn ruzu_decode_bc7_block(
        src: *const u8,
        dst: *mut u8,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
    );
}

const fn is_signed(pixel_format: PixelFormat) -> bool {
    matches!(
        pixel_format,
        PixelFormat::Bc4Snorm
            | PixelFormat::Bc4Unorm
            | PixelFormat::Bc5Snorm
            | PixelFormat::Bc5Unorm
            | PixelFormat::Bc6hSfloat
            | PixelFormat::Bc6hUfloat
    )
}

pub fn converted_bytes_per_block(pixel_format: PixelFormat) -> u32 {
    match pixel_format {
        PixelFormat::Bc4Snorm | PixelFormat::Bc4Unorm => 1,
        PixelFormat::Bc5Snorm | PixelFormat::Bc5Unorm => 2,
        PixelFormat::Bc6hSfloat | PixelFormat::Bc6hUfloat => 8,
        _ => 4,
    }
}

const fn block_size(pixel_format: PixelFormat) -> u32 {
    match pixel_format {
        PixelFormat::Bc1RgbaSrgb
        | PixelFormat::Bc1RgbaUnorm
        | PixelFormat::Bc4Snorm
        | PixelFormat::Bc4Unorm => 8,
        _ => 16,
    }
}

fn decompress_blocks(
    input: &[u8],
    output: &mut [u8],
    copy: &BufferImageCopy,
    pixel_format: PixelFormat,
    decompress: BcnDecode,
) {
    debug_assert!(!is_signed(pixel_format));
    decompress_blocks_impl(
        input,
        output,
        copy,
        pixel_format,
        |src, dst, x, y, width, height| unsafe {
            decompress(src, dst, x, y, width, height);
        },
    );
}

fn decompress_signed_blocks(
    input: &[u8],
    output: &mut [u8],
    copy: &BufferImageCopy,
    pixel_format: PixelFormat,
    decompress: BcnDecodeSigned,
    signed: bool,
) {
    debug_assert!(is_signed(pixel_format));
    decompress_blocks_impl(
        input,
        output,
        copy,
        pixel_format,
        |src, dst, x, y, width, height| unsafe {
            decompress(src, dst, x, y, width, height, signed);
        },
    );
}

fn decompress_blocks_impl(
    input: &[u8],
    output: &mut [u8],
    copy: &BufferImageCopy,
    pixel_format: PixelFormat,
    mut decompress: impl FnMut(*const u8, *mut u8, usize, usize, usize, usize),
) {
    let out_bpp = converted_bytes_per_block(pixel_format);
    let compressed_block_size = block_size(pixel_format);
    let width = copy.image_extent.width;
    let height = copy
        .image_extent
        .height
        .wrapping_mul(copy.image_subresource.num_layers as u32);
    let depth = copy.image_extent.depth;
    if width == 0 || height == 0 || depth == 0 {
        return;
    }
    let block_width = width.min(BLOCK_SIZE);
    let block_height = height.min(BLOCK_SIZE);
    let pitch = width.wrapping_mul(out_bpp);
    let mut input_offset = 0usize;
    let mut output_offset = 0usize;
    for _slice in 0..depth {
        let mut y = 0u32;
        while y < height {
            let mut src_offset = input_offset;
            let mut dst_offset = output_offset;
            let mut x = 0u32;
            while x < width {
                let Some(src) =
                    input.get(src_offset..src_offset.wrapping_add(compressed_block_size as usize))
                else {
                    return;
                };
                let decoded_width = (width - x).min(BLOCK_SIZE) as usize;
                let decoded_height = (height - y).min(BLOCK_SIZE) as usize;
                let Some(required_output) = (decoded_height - 1)
                    .checked_mul(pitch as usize)
                    .and_then(|rows| {
                        decoded_width
                            .checked_mul(out_bpp as usize)
                            .and_then(|last_row| rows.checked_add(last_row))
                    })
                else {
                    return;
                };
                let Some(dst_end) = dst_offset.checked_add(required_output) else {
                    return;
                };
                let Some(dst) = output.get_mut(dst_offset..dst_end) else {
                    return;
                };
                decompress(
                    src.as_ptr(),
                    dst.as_mut_ptr(),
                    x as usize,
                    y as usize,
                    width as usize,
                    height as usize,
                );
                src_offset = src_offset.wrapping_add(compressed_block_size as usize);
                dst_offset = dst_offset.wrapping_add(block_width.wrapping_mul(out_bpp) as usize);
                x = x.wrapping_add(block_width);
            }
            input_offset = input_offset.wrapping_add(
                (copy.buffer_row_length.wrapping_mul(compressed_block_size) / block_width) as usize,
            );
            output_offset = output_offset.wrapping_add(block_height.wrapping_mul(pitch) as usize);
            y = y.wrapping_add(block_height);
        }
    }
}

pub fn decompress_bcn(
    input: &[u8],
    output: &mut [u8],
    copy: &mut BufferImageCopy,
    pixel_format: PixelFormat,
) {
    match pixel_format {
        PixelFormat::Bc1RgbaUnorm | PixelFormat::Bc1RgbaSrgb => decompress_blocks(
            input,
            output,
            copy,
            PixelFormat::Bc1RgbaUnorm,
            ruzu_decode_bc1_block,
        ),
        PixelFormat::Bc2Unorm | PixelFormat::Bc2Srgb => decompress_blocks(
            input,
            output,
            copy,
            PixelFormat::Bc2Unorm,
            ruzu_decode_bc2_block,
        ),
        PixelFormat::Bc3Unorm | PixelFormat::Bc3Srgb => decompress_blocks(
            input,
            output,
            copy,
            PixelFormat::Bc3Unorm,
            ruzu_decode_bc3_block,
        ),
        PixelFormat::Bc4Snorm | PixelFormat::Bc4Unorm => decompress_signed_blocks(
            input,
            output,
            copy,
            PixelFormat::Bc4Unorm,
            ruzu_decode_bc4_block,
            pixel_format == PixelFormat::Bc4Snorm,
        ),
        PixelFormat::Bc5Snorm | PixelFormat::Bc5Unorm => decompress_signed_blocks(
            input,
            output,
            copy,
            PixelFormat::Bc5Unorm,
            ruzu_decode_bc5_block,
            pixel_format == PixelFormat::Bc5Snorm,
        ),
        PixelFormat::Bc6hSfloat | PixelFormat::Bc6hUfloat => decompress_signed_blocks(
            input,
            output,
            copy,
            PixelFormat::Bc6hUfloat,
            ruzu_decode_bc6_block,
            pixel_format == PixelFormat::Bc6hSfloat,
        ),
        PixelFormat::Bc7Srgb | PixelFormat::Bc7Unorm => decompress_blocks(
            input,
            output,
            copy,
            PixelFormat::Bc7Unorm,
            ruzu_decode_bc7_block,
        ),
        _ => log::warn!("DecompressBCn: unimplemented format {:?}", pixel_format),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::texture_cache::types::{Extent3D, Offset3D, SubresourceLayers};

    fn copy(width: u32, height: u32, row_length: u32, input_size: usize) -> BufferImageCopy {
        BufferImageCopy {
            buffer_offset: 0,
            buffer_size: input_size,
            buffer_row_length: row_length,
            buffer_image_height: height,
            image_subresource: SubresourceLayers {
                base_level: 0,
                base_layer: 0,
                num_layers: 1,
            },
            image_offset: Offset3D { x: 0, y: 0, z: 0 },
            image_extent: Extent3D {
                width,
                height,
                depth: 1,
            },
        }
    }

    #[test]
    fn converted_bytes_per_block_matches_upstream_switch() {
        assert_eq!(converted_bytes_per_block(PixelFormat::Bc1RgbaUnorm), 4);
        assert_eq!(converted_bytes_per_block(PixelFormat::Bc4Unorm), 1);
        assert_eq!(converted_bytes_per_block(PixelFormat::Bc5Unorm), 2);
        assert_eq!(converted_bytes_per_block(PixelFormat::Bc6hUfloat), 8);
        assert_eq!(converted_bytes_per_block(PixelFormat::Bc7Unorm), 4);
    }

    #[test]
    fn bc1_uses_the_upstream_decoder() {
        let c0 = 0xf800u16.to_le_bytes();
        let c1 = 0x07e0u16.to_le_bytes();
        let indices = 0b11_10_01_00u32.to_le_bytes();
        let input = [
            c0[0], c0[1], c1[0], c1[1], indices[0], indices[1], indices[2], indices[3],
        ];
        let mut copy = copy(4, 4, 4, input.len());
        let mut output = [0u8; 64];

        decompress_bcn(&input, &mut output, &mut copy, PixelFormat::Bc1RgbaUnorm);

        assert_eq!(&output[0..4], &[255, 0, 0, 255]);
        assert_eq!(&output[4..8], &[0, 255, 0, 255]);
        assert_eq!(&output[8..12], &[170, 85, 0, 255]);
        assert_eq!(&output[12..16], &[85, 170, 0, 255]);
    }

    #[test]
    fn bc4_signed_decode_preserves_endpoint_bit_patterns() {
        let input = [0x80, 0x7f, 0, 0, 0, 0, 0, 0];
        let mut copy = copy(4, 4, 4, input.len());
        let mut output = [0u8; 16];

        decompress_bcn(&input, &mut output, &mut copy, PixelFormat::Bc4Snorm);

        assert_eq!(output, [0x80; 16]);
    }

    #[test]
    fn bc2_and_bc3_use_the_upstream_decoders() {
        let color0 = 0xf800u16.to_le_bytes();
        let color1 = 0x07e0u16.to_le_bytes();
        let color_indices = 0u32.to_le_bytes();

        let alpha_bc2 = 0xfedc_ba98_7654_3210u64.to_le_bytes();
        let input_bc2 = [
            alpha_bc2[0],
            alpha_bc2[1],
            alpha_bc2[2],
            alpha_bc2[3],
            alpha_bc2[4],
            alpha_bc2[5],
            alpha_bc2[6],
            alpha_bc2[7],
            color0[0],
            color0[1],
            color1[0],
            color1[1],
            color_indices[0],
            color_indices[1],
            color_indices[2],
            color_indices[3],
        ];
        let mut bc2_copy = copy(4, 4, 4, input_bc2.len());
        let mut bc2_output = [0u8; 64];
        decompress_bcn(
            &input_bc2,
            &mut bc2_output,
            &mut bc2_copy,
            PixelFormat::Bc2Unorm,
        );
        assert_eq!(&bc2_output[0..4], &[255, 0, 0, 0x00]);
        assert_eq!(&bc2_output[4..8], &[255, 0, 0, 0x11]);
        assert_eq!(&bc2_output[60..64], &[255, 0, 0, 0xff]);

        let alpha_bc3 = [10, 20, 0b1000_1000, 0b1100_0110, 0b1111_1010, 0, 0, 0];
        let input_bc3 = [
            alpha_bc3[0],
            alpha_bc3[1],
            alpha_bc3[2],
            alpha_bc3[3],
            alpha_bc3[4],
            alpha_bc3[5],
            alpha_bc3[6],
            alpha_bc3[7],
            color0[0],
            color0[1],
            color1[0],
            color1[1],
            color_indices[0],
            color_indices[1],
            color_indices[2],
            color_indices[3],
        ];
        let mut bc3_copy = copy(4, 4, 4, input_bc3.len());
        let mut bc3_output = [0u8; 64];
        decompress_bcn(
            &input_bc3,
            &mut bc3_output,
            &mut bc3_copy,
            PixelFormat::Bc3Unorm,
        );
        assert_eq!(&bc3_output[0..4], &[255, 0, 0, 10]);
        assert_eq!(&bc3_output[4..8], &[255, 0, 0, 20]);
        assert_eq!(&bc3_output[8..12], &[255, 0, 0, 12]);
    }

    #[test]
    fn bc5_uses_the_upstream_decoder() {
        let input = [
            10, 20, 0, 0, 0, 0, 0, 0, // red channel
            30, 40, 0, 0, 0, 0, 0, 0, // green channel
        ];
        let mut copy = copy(4, 4, 4, input.len());
        let mut output = [0u8; 32];

        decompress_bcn(&input, &mut output, &mut copy, PixelFormat::Bc5Unorm);

        assert_eq!(&output[0..8], &[10, 30, 10, 30, 10, 30, 10, 30]);
    }

    #[test]
    fn partial_edge_blocks_follow_the_upstream_traversal() {
        let input = [
            0x11, 0x80, 0, 0, 0, 0, 0, 0, // x=0..3
            0x22, 0x80, 0, 0, 0, 0, 0, 0, // x=4 edge texel only
        ];
        let mut copy = copy(5, 3, 8, input.len());
        let mut output = [0u8; 15];

        decompress_bcn(&input, &mut output, &mut copy, PixelFormat::Bc4Unorm);

        assert_eq!(
            output,
            [
                0x11, 0x11, 0x11, 0x11, 0x22, 0x11, 0x11, 0x11, 0x11, 0x22, 0x11, 0x11, 0x11, 0x11,
                0x22,
            ]
        );
    }

    #[test]
    fn bc6_and_bc7_use_the_upstream_decoder() {
        let input = [0xffu8; 16];

        let mut bc6_copy = copy(4, 4, 4, input.len());
        let mut bc6_output = [0u8; 4 * 4 * 8];
        decompress_bcn(
            &input,
            &mut bc6_output,
            &mut bc6_copy,
            PixelFormat::Bc6hUfloat,
        );
        assert!(bc6_output.iter().any(|&byte| byte != 0));

        let mut bc7_copy = copy(4, 4, 4, input.len());
        let mut bc7_output = [0u8; 4 * 4 * 4];
        decompress_bcn(
            &input,
            &mut bc7_output,
            &mut bc7_copy,
            PixelFormat::Bc7Unorm,
        );
        assert!(bc7_output.iter().any(|&byte| byte != 0));
    }
}
