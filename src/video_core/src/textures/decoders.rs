// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `video_core/textures/decoders.h` and `decoders.cpp`.
//!
//! Tegra block-linear (GOB-based) texture swizzle/unswizzle routines.

use common::alignment::align_up_log2;
use common::div_ceil::div_ceil_log2_u32;

// ── GOB Constants ────────────────────────────────────────────────────────────

/// GOB (Graphics Operation Block) X dimension in bytes.
pub const GOB_SIZE_X: u32 = 64;
/// GOB Y dimension in rows.
pub const GOB_SIZE_Y: u32 = 8;
/// GOB Z dimension in slices.
pub const GOB_SIZE_Z: u32 = 1;
/// Total GOB size in bytes.
pub const GOB_SIZE: u32 = GOB_SIZE_X * GOB_SIZE_Y * GOB_SIZE_Z;

pub const GOB_SIZE_X_SHIFT: u32 = 6;
pub const GOB_SIZE_Y_SHIFT: u32 = 3;
pub const GOB_SIZE_Z_SHIFT: u32 = 0;
pub const GOB_SIZE_SHIFT: u32 = GOB_SIZE_X_SHIFT + GOB_SIZE_Y_SHIFT + GOB_SIZE_Z_SHIFT;

const SWIZZLE_X_BITS: u32 = 0b100101111;
const SWIZZLE_Y_BITS: u32 = 0b011010000;

// ── Helper: pdep (parallel bit deposit) ──────────────────────────────────────

/// Parallel bit deposit — deposits bits of `value` at positions specified by `mask`.
///
/// Port of the `pdep<mask>(value)` template from `decoders.cpp`.
const fn pdep<const MASK: u32>(value: u32) -> u32 {
    let mut result = 0u32;
    let mut m = MASK;
    let mut bit = 1u32;
    while m != 0 {
        if value & bit != 0 {
            result |= m & m.wrapping_neg(); // m & (~m + 1)
        }
        m &= m.wrapping_sub(1);
        bit = bit.wrapping_add(bit);
    }
    result
}

/// Increment a pdep-encoded value by `incr_amount` within the given `mask`.
///
/// Port of `incrpdep<mask, incr_amount>(value)` from `decoders.cpp`.
fn incrpdep<const MASK: u32, const INCR_AMOUNT: u32>(value: &mut u32) {
    let swizzled_incr = pdep::<MASK>(INCR_AMOUNT);
    *value = ((*value | !MASK).wrapping_add(swizzled_incr)) & MASK;
}

// ── Swizzle implementation ───────────────────────────────────────────────────

/// Core swizzle/unswizzle implementation for a given bytes-per-pixel.
///
/// Port of `SwizzleImpl<TO_LINEAR, BYTES_PER_PIXEL>` from `decoders.cpp`.
fn swizzle_impl<const TO_LINEAR: bool, const BYTES_PER_PIXEL: u32>(
    output: &mut [u8],
    input: &[u8],
    width: u32,
    height: u32,
    depth: u32,
    block_height: u32,
    block_depth: u32,
    stride: u32,
) {
    let origin_x: u32 = 0;
    let origin_y: u32 = 0;
    let origin_z: u32 = 0;

    let pitch = width.wrapping_mul(BYTES_PER_PIXEL);
    let gobs_in_x = div_ceil_log2_u32(stride, GOB_SIZE_X_SHIFT);
    let block_size = gobs_in_x.wrapping_shl(GOB_SIZE_SHIFT + block_height + block_depth);
    let slice_size =
        div_ceil_log2_u32(height, block_height + GOB_SIZE_Y_SHIFT).wrapping_mul(block_size);

    let block_height_mask = 1u32.wrapping_shl(block_height).wrapping_sub(1);
    let block_depth_mask = 1u32.wrapping_shl(block_depth).wrapping_sub(1);
    let x_shift = GOB_SIZE_SHIFT + block_height + block_depth;

    for slice in 0..depth {
        let z = slice.wrapping_add(origin_z);
        let offset_z = (z >> block_depth)
            .wrapping_mul(slice_size)
            .wrapping_add((z & block_depth_mask).wrapping_shl(GOB_SIZE_SHIFT + block_height));

        for line in 0..height {
            let y = line.wrapping_add(origin_y);
            let swizzled_y = pdep::<SWIZZLE_Y_BITS>(y);
            let block_y = y >> GOB_SIZE_Y_SHIFT;
            let offset_y = (block_y >> block_height)
                .wrapping_mul(block_size)
                .wrapping_add((block_y & block_height_mask).wrapping_shl(GOB_SIZE_SHIFT));

            let mut swizzled_x = pdep::<SWIZZLE_X_BITS>(origin_x.wrapping_mul(BYTES_PER_PIXEL));

            for column in 0..width {
                let x = column.wrapping_add(origin_x).wrapping_mul(BYTES_PER_PIXEL);
                let offset_x = (x >> GOB_SIZE_X_SHIFT).wrapping_shl(x_shift);

                let base_swizzled_offset = offset_z.wrapping_add(offset_y).wrapping_add(offset_x);
                let swizzled_offset =
                    base_swizzled_offset.wrapping_add(swizzled_x | swizzled_y) as usize;
                let unswizzled_offset = slice
                    .wrapping_mul(pitch)
                    .wrapping_mul(height)
                    .wrapping_add(line.wrapping_mul(pitch))
                    .wrapping_add(column.wrapping_mul(BYTES_PER_PIXEL))
                    as usize;

                let bpp = BYTES_PER_PIXEL as usize;
                if TO_LINEAR {
                    if let (Some(dst_end), Some(src_end)) = (
                        swizzled_offset.checked_add(bpp),
                        unswizzled_offset.checked_add(bpp),
                    ) {
                        if dst_end <= output.len() && src_end <= input.len() {
                            output[swizzled_offset..dst_end]
                                .copy_from_slice(&input[unswizzled_offset..src_end]);
                        }
                    }
                } else {
                    if let (Some(dst_end), Some(src_end)) = (
                        unswizzled_offset.checked_add(bpp),
                        swizzled_offset.checked_add(bpp),
                    ) {
                        if dst_end <= output.len() && src_end <= input.len() {
                            output[unswizzled_offset..dst_end]
                                .copy_from_slice(&input[swizzled_offset..src_end]);
                        }
                    }
                }

                incrpdep::<SWIZZLE_X_BITS, BYTES_PER_PIXEL>(&mut swizzled_x);
            }
        }
    }
}

/// BPP dispatch for swizzle operations.
///
/// Port of the `Swizzle<TO_LINEAR>` function from `decoders.cpp`.
fn swizzle<const TO_LINEAR: bool>(
    output: &mut [u8],
    input: &[u8],
    bytes_per_pixel: u32,
    width: u32,
    height: u32,
    depth: u32,
    block_height: u32,
    block_depth: u32,
    stride_alignment: u32,
) {
    macro_rules! bpp_case {
        ($bpp:literal) => {
            swizzle_impl::<TO_LINEAR, $bpp>(
                output,
                input,
                width,
                height,
                depth,
                block_height,
                block_depth,
                stride_alignment,
            )
        };
    }
    match bytes_per_pixel {
        1 => bpp_case!(1),
        2 => bpp_case!(2),
        3 => bpp_case!(3),
        4 => bpp_case!(4),
        6 => bpp_case!(6),
        8 => bpp_case!(8),
        12 => bpp_case!(12),
        16 => bpp_case!(16),
        _ => panic!("Invalid bytes_per_pixel={}", bytes_per_pixel),
    }
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Unswizzles a block linear texture into linear memory.
///
/// Port of `Tegra::Texture::UnswizzleTexture`.
pub fn unswizzle_texture(
    output: &mut [u8],
    input: &[u8],
    mut bytes_per_pixel: u32,
    mut width: u32,
    height: u32,
    depth: u32,
    block_height: u32,
    block_depth: u32,
    stride_alignment: u32,
) {
    let stride =
        (align_up_log2(width as u64, stride_alignment) as u32).wrapping_mul(bytes_per_pixel);
    let width_bytes = width.wrapping_mul(bytes_per_pixel);
    let new_bpp = std::cmp::min(4, width_bytes.trailing_zeros());
    width = width_bytes >> new_bpp;
    bytes_per_pixel = 1u32.wrapping_shl(new_bpp);
    swizzle::<false>(
        output,
        input,
        bytes_per_pixel,
        width,
        height,
        depth,
        block_height,
        block_depth,
        stride,
    );
}

/// Swizzles linear memory into a block linear texture.
///
/// Port of `Tegra::Texture::SwizzleTexture`.
pub fn swizzle_texture(
    output: &mut [u8],
    input: &[u8],
    mut bytes_per_pixel: u32,
    mut width: u32,
    height: u32,
    depth: u32,
    block_height: u32,
    block_depth: u32,
    stride_alignment: u32,
) {
    let stride =
        (align_up_log2(width as u64, stride_alignment) as u32).wrapping_mul(bytes_per_pixel);
    let width_bytes = width.wrapping_mul(bytes_per_pixel);
    let new_bpp = std::cmp::min(4, width_bytes.trailing_zeros());
    width = width_bytes >> new_bpp;
    bytes_per_pixel = 1u32.wrapping_shl(new_bpp);
    swizzle::<true>(
        output,
        input,
        bytes_per_pixel,
        width,
        height,
        depth,
        block_height,
        block_depth,
        stride,
    );
}

/// Core subrect swizzle/unswizzle implementation for a given bytes-per-pixel.
///
/// Port of `SwizzleSubrectImpl<TO_LINEAR, BYTES_PER_PIXEL>` from `decoders.cpp`.
fn swizzle_subrect_impl<const TO_LINEAR: bool, const BYTES_PER_PIXEL: u32>(
    output: &mut [u8],
    input: &[u8],
    width: u32,
    height: u32,
    depth: u32,
    origin_x: u32,
    origin_y: u32,
    extent_x: u32,
    num_lines: u32,
    block_height: u32,
    block_depth: u32,
    pitch_linear: u32,
) {
    let origin_z: u32 = 0;
    let pitch = pitch_linear;
    let stride = align_up_log2(width.wrapping_mul(BYTES_PER_PIXEL) as u64, GOB_SIZE_X_SHIFT) as u32;

    let gobs_in_x = div_ceil_log2_u32(stride, GOB_SIZE_X_SHIFT);
    let block_size = gobs_in_x.wrapping_shl(GOB_SIZE_SHIFT + block_height + block_depth);
    let slice_size =
        div_ceil_log2_u32(height, block_height + GOB_SIZE_Y_SHIFT).wrapping_mul(block_size);

    let block_height_mask = 1u32.wrapping_shl(block_height).wrapping_sub(1);
    let block_depth_mask = 1u32.wrapping_shl(block_depth).wrapping_sub(1);
    let x_shift = GOB_SIZE_SHIFT + block_height + block_depth;

    let mut unprocessed_lines = num_lines;
    let extent_y = std::cmp::min(num_lines, height.wrapping_sub(origin_y));

    for slice in 0..depth {
        let z = slice.wrapping_add(origin_z);
        let offset_z = (z >> block_depth)
            .wrapping_mul(slice_size)
            .wrapping_add((z & block_depth_mask).wrapping_shl(GOB_SIZE_SHIFT + block_height));
        let lines_in_y = std::cmp::min(unprocessed_lines, extent_y);

        for line in 0..lines_in_y {
            let y = line.wrapping_add(origin_y);
            let swizzled_y = pdep::<SWIZZLE_Y_BITS>(y);
            let block_y = y >> GOB_SIZE_Y_SHIFT;
            let offset_y = (block_y >> block_height)
                .wrapping_mul(block_size)
                .wrapping_add((block_y & block_height_mask).wrapping_shl(GOB_SIZE_SHIFT));

            let mut swizzled_x = pdep::<SWIZZLE_X_BITS>(origin_x.wrapping_mul(BYTES_PER_PIXEL));

            for column in 0..extent_x {
                let x = column.wrapping_add(origin_x).wrapping_mul(BYTES_PER_PIXEL);
                let offset_x = (x >> GOB_SIZE_X_SHIFT).wrapping_shl(x_shift);

                let base_swizzled_offset = offset_z.wrapping_add(offset_y).wrapping_add(offset_x);
                let swizzled_offset =
                    base_swizzled_offset.wrapping_add(swizzled_x | swizzled_y) as usize;
                let unswizzled_offset = slice
                    .wrapping_mul(pitch)
                    .wrapping_mul(height)
                    .wrapping_add(line.wrapping_mul(pitch))
                    .wrapping_add(column.wrapping_mul(BYTES_PER_PIXEL))
                    as usize;

                let bpp = BYTES_PER_PIXEL as usize;
                if TO_LINEAR {
                    if let (Some(dst_end), Some(src_end)) = (
                        swizzled_offset.checked_add(bpp),
                        unswizzled_offset.checked_add(bpp),
                    ) {
                        if dst_end <= output.len() && src_end <= input.len() {
                            output[swizzled_offset..dst_end]
                                .copy_from_slice(&input[unswizzled_offset..src_end]);
                        }
                    }
                } else {
                    if let (Some(dst_end), Some(src_end)) = (
                        unswizzled_offset.checked_add(bpp),
                        swizzled_offset.checked_add(bpp),
                    ) {
                        if dst_end <= output.len() && src_end <= input.len() {
                            output[unswizzled_offset..dst_end]
                                .copy_from_slice(&input[swizzled_offset..src_end]);
                        }
                    }
                }

                incrpdep::<SWIZZLE_X_BITS, BYTES_PER_PIXEL>(&mut swizzled_x);
            }
        }
        unprocessed_lines = unprocessed_lines.wrapping_sub(lines_in_y);
        if unprocessed_lines == 0 {
            return;
        }
    }
}

/// Copies an untiled subrectangle into a tiled surface.
///
/// Port of `Tegra::Texture::SwizzleSubrect`.
pub fn swizzle_subrect(
    output: &mut [u8],
    input: &[u8],
    bytes_per_pixel: u32,
    width: u32,
    height: u32,
    depth: u32,
    origin_x: u32,
    origin_y: u32,
    extent_x: u32,
    extent_y: u32,
    block_height: u32,
    block_depth: u32,
    pitch_linear: u32,
) {
    macro_rules! bpp_case {
        ($bpp:literal) => {
            swizzle_subrect_impl::<true, $bpp>(
                output,
                input,
                width,
                height,
                depth,
                origin_x,
                origin_y,
                extent_x,
                extent_y,
                block_height,
                block_depth,
                pitch_linear,
            )
        };
    }
    match bytes_per_pixel {
        1 => bpp_case!(1),
        2 => bpp_case!(2),
        3 => bpp_case!(3),
        4 => bpp_case!(4),
        6 => bpp_case!(6),
        8 => bpp_case!(8),
        12 => bpp_case!(12),
        16 => bpp_case!(16),
        _ => panic!("Invalid bytes_per_pixel={}", bytes_per_pixel),
    }
}

/// Copies a tiled subrectangle into a linear surface.
///
/// Port of `Tegra::Texture::UnswizzleSubrect`.
pub fn unswizzle_subrect(
    output: &mut [u8],
    input: &[u8],
    bytes_per_pixel: u32,
    width: u32,
    height: u32,
    depth: u32,
    origin_x: u32,
    origin_y: u32,
    extent_x: u32,
    extent_y: u32,
    block_height: u32,
    block_depth: u32,
    pitch_linear: u32,
) {
    macro_rules! bpp_case {
        ($bpp:literal) => {
            swizzle_subrect_impl::<false, $bpp>(
                output,
                input,
                width,
                height,
                depth,
                origin_x,
                origin_y,
                extent_x,
                extent_y,
                block_height,
                block_depth,
                pitch_linear,
            )
        };
    }
    match bytes_per_pixel {
        1 => bpp_case!(1),
        2 => bpp_case!(2),
        3 => bpp_case!(3),
        4 => bpp_case!(4),
        6 => bpp_case!(6),
        8 => bpp_case!(8),
        12 => bpp_case!(12),
        16 => bpp_case!(16),
        _ => panic!("Invalid bytes_per_pixel={}", bytes_per_pixel),
    }
}

/// Calculates the correct size of a texture depending on whether it's tiled or not.
///
/// Port of `Tegra::Texture::CalculateSize`.
pub fn calculate_size(
    tiled: bool,
    bytes_per_pixel: u32,
    width: u32,
    height: u32,
    depth: u32,
    block_height: u32,
    block_depth: u32,
) -> usize {
    if tiled {
        let aligned_width =
            align_up_log2(width.wrapping_mul(bytes_per_pixel) as u64, GOB_SIZE_X_SHIFT) as u32;
        let aligned_height = align_up_log2(height as u64, GOB_SIZE_Y_SHIFT + block_height) as u32;
        let aligned_depth = align_up_log2(depth as u64, GOB_SIZE_Z_SHIFT + block_depth) as u32;
        aligned_width
            .wrapping_mul(aligned_height)
            .wrapping_mul(aligned_depth) as usize
    } else {
        width
            .wrapping_mul(height)
            .wrapping_mul(depth)
            .wrapping_mul(bytes_per_pixel) as usize
    }
}

/// Obtains the offset of the GOB for positions `dst_x` & `dst_y`.
///
/// Port of `Tegra::Texture::GetGOBOffset`.
pub fn get_gob_offset(
    width: u32,
    _height: u32,
    dst_x: u32,
    dst_y: u32,
    block_height: u32,
    bytes_per_pixel: u32,
) -> u64 {
    let div_ceil = |x: u32, y: u32| x.wrapping_add(y).wrapping_sub(1) / y;
    let gobs_in_block = 1u32.wrapping_shl(block_height);
    let y_blocks = GOB_SIZE_Y.wrapping_shl(block_height);
    let x_per_gob = GOB_SIZE_X / bytes_per_pixel;
    let x_blocks = div_ceil(width, x_per_gob);
    let block_size = GOB_SIZE.wrapping_mul(gobs_in_block);
    let stride = block_size.wrapping_mul(x_blocks);
    let base = (dst_y / y_blocks)
        .wrapping_mul(stride)
        .wrapping_add((dst_x / x_per_gob).wrapping_mul(block_size));
    let relative_y = dst_y % y_blocks;
    base.wrapping_add((relative_y / GOB_SIZE_Y).wrapping_mul(GOB_SIZE)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gob_constants() {
        assert_eq!(GOB_SIZE, 512);
        assert_eq!(GOB_SIZE_X, 64);
        assert_eq!(GOB_SIZE_Y, 8);
        assert_eq!(GOB_SIZE_Z, 1);
    }

    #[test]
    fn calculate_size_linear() {
        let size = calculate_size(false, 4, 64, 64, 1, 0, 0);
        assert_eq!(size, 64 * 64 * 4);
    }

    #[test]
    fn calculate_size_tiled() {
        let size = calculate_size(true, 4, 64, 64, 1, 0, 0);
        assert_eq!(size, 64 * 64 * 4);
    }

    #[test]
    fn calculate_size_preserves_upstream_u32_overflow() {
        assert_eq!(
            calculate_size(false, 1, u32::MAX, 2, 1, 0, 0),
            u32::MAX.wrapping_mul(2) as usize
        );
    }

    #[test]
    fn pdep_basic() {
        // pdep with mask 0b1111 and value 0b1010 = deposit into first 4 bit positions
        assert_eq!(pdep::<0b1111>(0b1010), 0b1010);
        // pdep with mask 0b10101 and value 0b111 = deposit bits at positions 0, 2, 4
        assert_eq!(pdep::<0b10101>(0b111), 0b10101);
    }

    #[test]
    fn every_upstream_bpp_specialization_round_trips() {
        for bytes_per_pixel in [1, 2, 3, 4, 6, 8, 12, 16] {
            let width = 13;
            let height = 9;
            let depth = 2;
            let linear_size = (width * height * depth * bytes_per_pixel) as usize;
            let input = (0..linear_size)
                .map(|index| index.wrapping_mul(37) as u8)
                .collect::<Vec<_>>();
            let mut tiled =
                vec![0u8; calculate_size(true, bytes_per_pixel, width, height, depth, 1, 1)];
            let mut output = vec![0u8; linear_size];

            swizzle_texture(
                &mut tiled,
                &input,
                bytes_per_pixel,
                width,
                height,
                depth,
                1,
                1,
                1,
            );
            unswizzle_texture(
                &mut output,
                &tiled,
                bytes_per_pixel,
                width,
                height,
                depth,
                1,
                1,
                1,
            );

            assert_eq!(output, input, "bytes_per_pixel={bytes_per_pixel}");
        }
    }

    #[test]
    fn subrect_non_overlapping_specializations_copy_the_selected_rectangle() {
        for bytes_per_pixel in [1, 2, 3, 4, 8, 16] {
            let width = 16;
            let height = 16;
            let extent_x = 4;
            let extent_y = 4;
            let pitch = extent_x * bytes_per_pixel;
            let linear_size = (pitch * extent_y) as usize;
            let input = (0..linear_size)
                .map(|index| index.wrapping_mul(19) as u8)
                .collect::<Vec<_>>();
            let mut tiled =
                vec![0u8; calculate_size(true, bytes_per_pixel, width, height, 1, 0, 0)];
            let mut output = vec![0u8; linear_size];

            swizzle_subrect(
                &mut tiled,
                &input,
                bytes_per_pixel,
                width,
                height,
                1,
                0,
                0,
                extent_x,
                extent_y,
                0,
                0,
                pitch,
            );
            unswizzle_subrect(
                &mut output,
                &tiled,
                bytes_per_pixel,
                width,
                height,
                1,
                0,
                0,
                extent_x,
                extent_y,
                0,
                0,
                pitch,
            );

            assert_eq!(output, input, "bytes_per_pixel={bytes_per_pixel}");
        }
    }

    #[test]
    fn subrect_non_power_of_two_specializations_preserve_upstream_overlap() {
        for bytes_per_pixel in [6, 12] {
            let width = 16;
            let height = 16;
            let extent_x = 4;
            let extent_y = 4;
            let pitch = extent_x * bytes_per_pixel;
            let linear_size = (pitch * extent_y) as usize;
            let input = (0..linear_size)
                .map(|index| index.wrapping_mul(19) as u8)
                .collect::<Vec<_>>();
            let mut tiled =
                vec![0u8; calculate_size(true, bytes_per_pixel, width, height, 1, 0, 0)];
            let mut output = vec![0u8; linear_size];

            swizzle_subrect(
                &mut tiled,
                &input,
                bytes_per_pixel,
                width,
                height,
                1,
                0,
                0,
                extent_x,
                extent_y,
                0,
                0,
                pitch,
            );
            unswizzle_subrect(
                &mut output,
                &tiled,
                bytes_per_pixel,
                width,
                height,
                1,
                0,
                0,
                extent_x,
                extent_y,
                0,
                0,
                pitch,
            );

            assert_ne!(output, input, "bytes_per_pixel={bytes_per_pixel}");
            assert!(output.iter().any(|&value| value != 0));
        }

        let bytes_per_pixel = 6;
        let pitch = 4 * bytes_per_pixel;
        let input = (0..(pitch * 4) as usize)
            .map(|index| index.wrapping_mul(19) as u8)
            .collect::<Vec<_>>();
        let mut tiled = vec![0u8; calculate_size(true, bytes_per_pixel, 16, 16, 1, 0, 0)];
        let mut output = vec![0u8; input.len()];
        swizzle_subrect(
            &mut tiled,
            &input,
            bytes_per_pixel,
            16,
            16,
            1,
            0,
            0,
            4,
            4,
            0,
            0,
            pitch,
        );
        unswizzle_subrect(
            &mut output,
            &tiled,
            bytes_per_pixel,
            16,
            16,
            1,
            0,
            0,
            4,
            4,
            0,
            0,
            pitch,
        );
        assert_eq!(&output[16..18], &input[24..26]);
        assert_eq!(&output[64..66], &input[72..74]);
    }

    #[test]
    fn get_gob_offset_basic() {
        assert_eq!(get_gob_offset(64, 8, 0, 0, 0, 4), 0);
        assert_eq!(get_gob_offset(130, 64, 33, 41, 2, 4), 23_040);
    }
}
