// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/texture_cache/util.h and util.cpp
//!
//! Utility functions for the texture cache: size/offset calculations,
//! swizzle/unswizzle, copy generation, subresource lookup, and more.
//!
//! The helper and public-function boundaries mirror the upstream owner so the
//! dense layout and overlap calculations remain directly reviewable.

use crate::textures::texture::TicEntry;

use super::format_lookup_table::{pixel_format_from_texture_info_raw, PixelFormat};
use super::image_base::{GPUVAddr, ImageBase, VAddr};
use super::image_info::ImageInfo;
use super::types::*;

use crate::surface;
use crate::textures::decoders::{
    GOB_SIZE_SHIFT, GOB_SIZE_X, GOB_SIZE_X_SHIFT, GOB_SIZE_Y, GOB_SIZE_Y_SHIFT, GOB_SIZE_Z,
    GOB_SIZE_Z_SHIFT,
};
use common::scratch_buffer::ScratchBuffer;
use smallvec::{smallvec, SmallVec};

fn assert_fail_soft(condition: bool, message: impl FnOnce() -> String) {
    if condition {
        return;
    }
    let message = message();
    log::error!("{message}");
    if *common::settings::values().use_debug_asserts.get_value() {
        panic!("{message}");
    }
}

// ── Alignment helpers ─────────────────────────────────────────────────

fn align_up_log2(value: u32, alignment_log2: u32) -> u32 {
    let mask = (1u32 << alignment_log2) - 1;
    value.wrapping_add(mask) & !mask
}

fn div_ceil(a: u32, b: u32) -> u32 {
    a.wrapping_add(b).wrapping_sub(1) / b
}

fn div_ceil_log2(value: u32, shift: u32) -> u32 {
    let mask = (1u32 << shift) - 1;
    value.wrapping_add(mask) >> shift
}

fn align_up(value: u32, alignment: u32) -> u32 {
    if alignment == 0 {
        return value;
    }
    let remainder = value % alignment;
    if remainder == 0 {
        value
    } else {
        value.wrapping_sub(remainder).wrapping_add(alignment)
    }
}

// ── Type aliases matching upstream ─────────────────────────────────────

pub type LevelArray = [u32; MAX_MIP_LEVELS];

// ── LevelInfo (internal) ──────────────────────────────────────────────

/// Internal level info struct for size calculations.
///
/// Port of anonymous `LevelInfo` from `util.cpp`.
struct LevelInfo {
    size: Extent3D,
    block: Extent3D,
    tile_size: Extent2D,
    bpp_log2: u32,
    tile_width_spacing: u32,
    num_levels: u32,
}

// ── OverlapResult ──────────────────────────────────────────────────────

/// Result of resolving an overlap between two images.
///
/// Port of `VideoCommon::OverlapResult`.
#[derive(Debug, Clone, Copy)]
pub struct OverlapResult {
    pub gpu_addr: GPUVAddr,
    pub cpu_addr: VAddr,
    pub resources: SubresourceExtent,
}

// ── Internal helper functions (port of anonymous namespace) ────────────

/// Port of `AdjustTileSize(u32, u32, u32)`.
fn adjust_tile_size_scalar(shift: u32, unit_factor: u32, dimension: u32) -> u32 {
    if shift == 0 {
        return 0;
    }
    let mut s = shift;
    let mut x = unit_factor.wrapping_shl(s - 1);
    if x >= dimension {
        while s > 0 {
            s -= 1;
            x >>= 1;
            if x < dimension {
                break;
            }
        }
    }
    s
}

/// Port of `AdjustMipSize(u32, u32)`.
fn adjust_mip_size(size: u32, level: u32) -> u32 {
    (size >> level).max(1)
}

/// Port of `AdjustMipBlockSize<GOB_EXTENT>(num_tiles, block_size, level)`.
fn adjust_mip_block_size_impl(
    gob_extent: u32,
    num_tiles: u32,
    mut block_size: u32,
    mut level: u32,
) -> u32 {
    loop {
        while block_size > 0
            && num_tiles <= 1u32.wrapping_shl(block_size - 1).wrapping_mul(gob_extent)
        {
            block_size -= 1;
        }
        if level == 0 {
            break;
        }
        level -= 1;
    }
    block_size
}

/// Port of `AdjustMipSize(Extent3D, s32)` — 3D overload.
fn adjust_mip_size_3d(size: Extent3D, level: u32) -> Extent3D {
    Extent3D {
        width: adjust_mip_size(size.width, level),
        height: adjust_mip_size(size.height, level),
        depth: adjust_mip_size(size.depth, level),
    }
}

/// Port of `AdjustSamplesSize`.
fn adjust_samples_size(size: Extent3D, num_samples: u32) -> Extent3D {
    let (samples_x, samples_y) = super::samples_helper::samples_log2(num_samples as i32);
    Extent3D {
        width: size.width >> samples_x as u32,
        height: size.height >> samples_y as u32,
        depth: size.depth,
    }
}

/// Port of `AdjustTileSize(Extent3D, Extent2D)`.
fn adjust_tile_size_3d(size: Extent3D, tile_size: Extent2D) -> Extent3D {
    Extent3D {
        width: div_ceil(size.width, tile_size.width),
        height: div_ceil(size.height, tile_size.height),
        depth: size.depth,
    }
}

/// Port of `NumBlocks`.
fn num_blocks(size: Extent3D, tile_size: Extent2D) -> u32 {
    let num_blocks = adjust_tile_size_3d(size, tile_size);
    num_blocks
        .width
        .wrapping_mul(num_blocks.height)
        .wrapping_mul(num_blocks.depth)
}

/// Port of `AdjustSize`.
fn adjust_size(size: u32, level: u32, block_size: u32) -> u32 {
    div_ceil(adjust_mip_size(size, level), block_size)
}

/// Port of `AdjustMipBlockSize(Extent3D, Extent3D, u32, u32)`.
fn adjust_mip_block_size_3d(
    num_tiles: Extent3D,
    block_size: Extent3D,
    level: u32,
    num_levels: u32,
) -> Extent3D {
    Extent3D {
        width: adjust_mip_block_size_impl(GOB_SIZE_X, num_tiles.width, block_size.width, level),
        height: adjust_mip_block_size_impl(GOB_SIZE_Y, num_tiles.height, block_size.height, level),
        depth: if level == 0 && num_levels == 1 {
            block_size.depth
        } else {
            adjust_mip_block_size_impl(GOB_SIZE_Z, num_tiles.depth, block_size.depth, level)
        },
    }
}

/// Port of `StrideAlignment(Extent3D, Extent3D, Extent2D, u32)`.
fn stride_alignment_gob(num_tiles: Extent3D, block: Extent3D, gob: Extent2D, bpp_log2: u32) -> u32 {
    if is_smaller_than_gob_size(num_tiles, gob, block.depth) {
        GOB_SIZE_X_SHIFT - bpp_log2
    } else {
        gob.width
    }
}

/// Port of `StrideAlignment(Extent3D, Extent3D, u32, u32)`.
fn stride_alignment(
    num_tiles: Extent3D,
    block: Extent3D,
    bpp_log2: u32,
    tile_width_spacing: u32,
) -> u32 {
    let g = gob_size(bpp_log2, block.height, tile_width_spacing);
    stride_alignment_gob(num_tiles, block, g, bpp_log2)
}

/// Port of `PitchLinearAlignedSize`.
fn pitch_linear_aligned_size(info: &ImageInfo) -> Extent2D {
    const STRIDE_ALIGNMENT: u32 = 32;
    assert_fail_soft(info.image_type == ImageType::Linear, || {
        "PitchLinearAlignedSize requires a linear image".to_owned()
    });
    let num_tiles = Extent2D {
        width: div_ceil(info.size.width, surface::default_block_width(info.format)),
        height: div_ceil(info.size.height, surface::default_block_height(info.format)),
    };
    let width_alignment = STRIDE_ALIGNMENT / surface::bytes_per_block(info.format);
    Extent2D {
        width: align_up(num_tiles.width, width_alignment),
        height: num_tiles.height,
    }
}

/// Port of `BlockLinearAlignedSize`.
fn block_linear_aligned_size(info: &ImageInfo, level: u32) -> Extent3D {
    assert_fail_soft(info.image_type != ImageType::Linear, || {
        "BlockLinearAlignedSize requires a block-linear image".to_owned()
    });
    let size = adjust_mip_size_3d(info.size, level);
    let num_tiles = Extent3D {
        width: div_ceil(size.width, surface::default_block_width(info.format)),
        height: div_ceil(size.height, surface::default_block_height(info.format)),
        depth: size.depth,
    };
    let bpp_log2 = bytes_per_block_log2_format(info.format);
    let alignment = stride_alignment(num_tiles, info.block(), bpp_log2, info.tile_width_spacing);
    let mip_block =
        adjust_mip_block_size_3d(num_tiles, info.block(), 0, info.resources.levels as u32);
    Extent3D {
        width: align_up_log2(num_tiles.width, alignment),
        height: align_up_log2(num_tiles.height, GOB_SIZE_Y_SHIFT + mip_block.height),
        depth: align_up_log2(num_tiles.depth, GOB_SIZE_Z_SHIFT + mip_block.depth),
    }
}

/// Port of `NumBlocksPerLayer`.
fn num_blocks_per_layer(info: &ImageInfo, tile_size: Extent2D) -> u32 {
    let mut num_blocks = 0u32;
    for level in 0..info.resources.levels as u32 {
        let mip_size = adjust_mip_size_3d(info.size, level);
        num_blocks = num_blocks.wrapping_add(self::num_blocks(mip_size, tile_size));
    }
    num_blocks
}

/// Port of `NumSlices`.
fn num_slices(info: &ImageInfo) -> u32 {
    assert_fail_soft(info.image_type == ImageType::E3D, || {
        "NumSlices requires a 3D image".to_owned()
    });
    let mut slices = 0u32;
    for level in 0..info.resources.levels {
        slices = slices.wrapping_add(adjust_mip_size(info.size.depth, level as u32));
    }
    slices
}

/// Port of `BytesPerBlockLog2(u32)`.
fn bytes_per_block_log2(bytes_per_block: u32) -> u32 {
    bytes_per_block.leading_zeros() ^ 0x1F
}

/// Port of `BytesPerBlockLog2(PixelFormat)`.
fn bytes_per_block_log2_format(format: PixelFormat) -> u32 {
    bytes_per_block_log2(surface::bytes_per_block(format))
}

/// Port of `DefaultBlockSize(PixelFormat)`.
fn default_block_size(format: PixelFormat) -> Extent2D {
    Extent2D {
        width: surface::default_block_width(format),
        height: surface::default_block_height(format),
    }
}

/// Port of `NumLevelBlocks(info, level)`.
fn num_level_blocks(info: &LevelInfo, level: u32) -> Extent3D {
    Extent3D {
        width: adjust_size(info.size.width, level, info.tile_size.width)
            .wrapping_shl(info.bpp_log2),
        height: adjust_size(info.size.height, level, info.tile_size.height),
        depth: adjust_mip_size(info.size.depth, level),
    }
}

/// Port of `TileShift(info, level)`.
fn tile_shift(info: &LevelInfo, level: u32) -> Extent3D {
    if level == 0 && info.num_levels == 1 {
        return info.block;
    }
    let blocks = num_level_blocks(info, level);
    Extent3D {
        width: adjust_tile_size_scalar(info.block.width, GOB_SIZE_X, blocks.width),
        height: adjust_tile_size_scalar(info.block.height, GOB_SIZE_Y, blocks.height),
        depth: adjust_tile_size_scalar(info.block.depth, GOB_SIZE_Z, blocks.depth),
    }
}

/// Port of `GobSize(bpp_log2, block_height, tile_width_spacing)`.
fn gob_size(bpp_log2: u32, block_height: u32, tile_width_spacing: u32) -> Extent2D {
    Extent2D {
        width: GOB_SIZE_X_SHIFT
            .wrapping_sub(bpp_log2)
            .wrapping_add(tile_width_spacing),
        height: GOB_SIZE_Y_SHIFT.wrapping_add(block_height),
    }
}

/// Port of `IsSmallerThanGobSize`.
fn is_smaller_than_gob_size(num_tiles: Extent3D, gob: Extent2D, block_depth: u32) -> bool {
    num_tiles.width <= 1u32.wrapping_shl(gob.width)
        || num_tiles.height <= 1u32.wrapping_shl(gob.height)
        || num_tiles.depth < 1u32.wrapping_shl(block_depth)
}

/// Port of `NumGobs(info, level)`.
fn num_gobs(info: &LevelInfo, level: u32) -> Extent2D {
    let blocks = num_level_blocks(info, level);
    let gobs = Extent2D {
        width: div_ceil_log2(blocks.width, GOB_SIZE_X_SHIFT),
        height: div_ceil_log2(blocks.height, GOB_SIZE_Y_SHIFT),
    };
    let gob = gob_size(info.bpp_log2, info.block.height, info.tile_width_spacing);
    let is_small = is_smaller_than_gob_size(blocks, gob, info.block.depth);
    let alignment = if is_small { 0 } else { info.tile_width_spacing };
    Extent2D {
        width: align_up_log2(gobs.width, alignment),
        height: gobs.height,
    }
}

/// Port of `LevelTiles(info, level)`.
fn level_tiles(info: &LevelInfo, level: u32) -> Extent3D {
    let blocks = num_level_blocks(info, level);
    let ts = tile_shift(info, level);
    let gobs = num_gobs(info, level);
    Extent3D {
        width: div_ceil_log2(gobs.width, ts.width),
        height: div_ceil_log2(gobs.height, ts.height),
        depth: div_ceil_log2(blocks.depth, ts.depth),
    }
}

/// Port of `CalculateLevelSize(info, level)`.
fn calculate_level_size(info: &LevelInfo, level: u32) -> u32 {
    let ts = tile_shift(info, level);
    let tiles = level_tiles(info, level);
    let num_tiles = tiles
        .width
        .wrapping_mul(tiles.height)
        .wrapping_mul(tiles.depth);
    let shift = GOB_SIZE_SHIFT + ts.width + ts.height + ts.depth;
    num_tiles.wrapping_shl(shift)
}

/// Port of `CalculateLevelSizes(info, num_levels)`.
fn calculate_level_sizes(info: &LevelInfo, num_levels: u32) -> LevelArray {
    assert!((num_levels as usize) <= MAX_MIP_LEVELS);
    let mut sizes = [0u32; MAX_MIP_LEVELS];
    for level in 0..num_levels {
        sizes[level as usize] = calculate_level_size(info, level);
    }
    sizes
}

/// Port of `CalculateLevelBytes(sizes, num_levels)`.
fn calculate_level_bytes(sizes: &LevelArray, num_levels: u32) -> u32 {
    sizes[..num_levels as usize]
        .iter()
        .fold(0, |total, size| total.wrapping_add(*size))
}

/// Port of `MakeLevelInfo(format, size, block, tile_width_spacing, num_levels)`.
fn make_level_info(
    format: PixelFormat,
    size: Extent3D,
    block: Extent3D,
    tile_width_spacing: u32,
    num_levels: u32,
) -> LevelInfo {
    let bpb = surface::bytes_per_block(format);
    LevelInfo {
        size,
        block,
        tile_size: default_block_size(format),
        bpp_log2: bytes_per_block_log2(bpb),
        tile_width_spacing,
        num_levels,
    }
}

/// Port of `MakeLevelInfo(const ImageInfo&)`.
fn make_level_info_from_image(info: &ImageInfo) -> LevelInfo {
    make_level_info(
        info.format,
        info.size,
        info.block(),
        info.tile_width_spacing,
        info.resources.levels as u32,
    )
}

/// Port of `CalculateLevelOffset`.
fn calculate_level_offset(
    format: PixelFormat,
    size: Extent3D,
    block: Extent3D,
    tile_width_spacing: u32,
    level: u32,
) -> u32 {
    let info = make_level_info(format, size, block, tile_width_spacing, level);
    let mut offset = 0u32;
    for current_level in 0..level {
        offset = offset.wrapping_add(calculate_level_size(&info, current_level));
    }
    offset
}

/// Port of `AlignLayerSize`.
fn align_layer_size(
    size_bytes: u32,
    size: Extent3D,
    mut block: Extent3D,
    tile_size_y: u32,
    tile_width_spacing: u32,
) -> u32 {
    if tile_width_spacing > 0 {
        let alignment_log2 = GOB_SIZE_SHIFT + tile_width_spacing + block.height + block.depth;
        return align_up_log2(size_bytes, alignment_log2);
    }
    let aligned_height = align_up(size.height, tile_size_y);
    while block.height != 0
        && aligned_height <= 1u32.wrapping_shl(block.height - 1).wrapping_mul(GOB_SIZE_Y)
    {
        block.height -= 1;
    }
    while block.depth != 0 && size.depth <= 1u32.wrapping_shl(block.depth - 1) {
        block.depth -= 1;
    }
    let block_shift = GOB_SIZE_SHIFT + block.height + block.depth;
    let num_blocks = size_bytes >> block_shift;
    if size_bytes != num_blocks << block_shift {
        num_blocks.wrapping_add(1).wrapping_shl(block_shift)
    } else {
        size_bytes
    }
}

// ── Size / offset calculation ─────────────────────────────────────────

/// Port of `CalculateGuestSizeInBytes`.
pub fn calculate_guest_size_in_bytes(info: &ImageInfo) -> u32 {
    if info.image_type == ImageType::Buffer {
        return info
            .size
            .width
            .wrapping_mul(surface::bytes_per_block(info.format));
    }
    if info.image_type == ImageType::Linear {
        return info.pitch().wrapping_mul(div_ceil(
            info.size.height,
            surface::default_block_height(info.format),
        ));
    }
    if info.resources.layers > 1 {
        assert_fail_soft(info.layer_stride != 0, || {
            "CalculateGuestSizeInBytes requires layer_stride for layered images".to_owned()
        });
        return info.layer_stride.wrapping_mul(info.resources.layers as u32);
    }
    calculate_layer_size(info)
}

/// Port of `CalculateUnswizzledSizeBytes`.
pub fn calculate_unswizzled_size_bytes(info: &ImageInfo) -> u32 {
    if info.image_type == ImageType::Buffer {
        return info
            .size
            .width
            .wrapping_mul(surface::bytes_per_block(info.format));
    }
    if info.image_type == ImageType::Linear {
        return info.pitch().wrapping_mul(div_ceil(
            info.size.height,
            surface::default_block_height(info.format),
        ));
    }
    let tile_size = default_block_size(info.format);
    num_blocks_per_layer(info, tile_size)
        .wrapping_mul(info.resources.layers as u32)
        .wrapping_mul(surface::bytes_per_block(info.format))
}

/// Port of `CalculateConvertedSizeBytes`.
pub fn calculate_converted_size_bytes(info: &ImageInfo) -> u32 {
    if info.image_type == ImageType::Buffer {
        return info
            .size
            .width
            .wrapping_mul(surface::bytes_per_block(info.format));
    }

    let recompression = *common::settings::values().astc_recompression.get_value();
    if surface::is_pixel_format_astc(info.format)
        && recompression != common::settings_enums::AstcRecompression::Uncompressed
    {
        let bpp_div = if recompression == common::settings_enums::AstcRecompression::Bc1 {
            2
        } else {
            1
        };
        let mut output_size = 0u32;
        for level in 0..info.resources.levels as u32 {
            let mip_size = adjust_mip_size_3d(info.size, level);
            let plane_dim = align_up(mip_size.width, 4).wrapping_mul(align_up(mip_size.height, 4));
            let level_size = plane_dim
                .wrapping_mul(info.size.depth)
                .wrapping_mul(info.resources.layers as u32)
                / bpp_div;
            output_size = output_size.wrapping_add(level_size);
        }
        return output_size;
    }

    num_blocks_per_layer(
        info,
        Extent2D {
            width: 1,
            height: 1,
        },
    )
    .wrapping_mul(info.resources.layers as u32)
    .wrapping_mul(crate::texture_cache::decode_bc::converted_bytes_per_block(
        info.format,
    ))
}

/// Port of `CalculateLayerStride`.
pub fn calculate_layer_stride(info: &ImageInfo) -> u32 {
    assert_fail_soft(info.image_type != ImageType::Linear, || {
        "CalculateLayerStride requires a block-linear image".to_owned()
    });
    let level_bytes = calculate_layer_size(info);
    align_layer_size(
        level_bytes,
        info.size,
        info.block(),
        surface::default_block_height(info.format),
        info.tile_width_spacing,
    )
}

/// Port of `CalculateLayerSize`.
pub fn calculate_layer_size(info: &ImageInfo) -> u32 {
    assert_fail_soft(info.image_type != ImageType::Linear, || {
        "CalculateLayerSize requires a block-linear image".to_owned()
    });
    calculate_level_offset(
        info.format,
        info.size,
        info.block(),
        info.tile_width_spacing,
        info.resources.levels as u32,
    )
}

/// Port of `CalculateMipLevelOffsets`.
pub fn calculate_mip_level_offsets(info: &ImageInfo) -> LevelArray {
    if info.image_type == ImageType::Linear {
        return [0u32; MAX_MIP_LEVELS];
    }
    if info.resources.levels > MAX_MIP_LEVELS as i32 {
        log::error!(
            "Image has too many mip levels={}, maximum supported is={}",
            info.resources.levels,
            MAX_MIP_LEVELS
        );
        return [0u32; MAX_MIP_LEVELS];
    }
    let level_info = make_level_info_from_image(info);
    let mut offsets = [0u32; MAX_MIP_LEVELS];
    let mut offset = 0u32;
    for level in 0..info.resources.levels {
        offsets[level as usize] = offset;
        offset = offset.wrapping_add(calculate_level_size(&level_info, level as u32));
    }
    offsets
}

/// Port of `CalculateMipLevelSizes`.
pub fn calculate_mip_level_sizes(info: &ImageInfo) -> LevelArray {
    let level_info = make_level_info_from_image(info);
    calculate_level_sizes(&level_info, info.resources.levels as u32)
}

/// Port of `CalculateSliceOffsets`.
pub fn calculate_slice_offsets(info: &ImageInfo) -> SmallVec<[u32; 16]> {
    assert_fail_soft(info.image_type == ImageType::E3D, || {
        "CalculateSliceOffsets requires a 3D image".to_owned()
    });
    let level_info = make_level_info_from_image(info);
    let mut offsets = SmallVec::with_capacity(num_slices(info) as usize);
    let mut mip_offset = 0u32;
    for level in 0..info.resources.levels as u32 {
        let ts = tile_shift(&level_info, level);
        let tiles = level_tiles(&level_info, level);
        let gob_size_shift = ts.height + GOB_SIZE_SHIFT;
        let slice_size = tiles
            .width
            .wrapping_mul(tiles.height)
            .wrapping_shl(gob_size_shift);
        let z_mask = (1u32 << ts.depth) - 1;
        let depth = adjust_mip_size(info.size.depth, level);
        for slice in 0..depth {
            let z_low = slice & z_mask;
            let z_high = slice & !z_mask;
            offsets.push(
                mip_offset
                    .wrapping_add(z_low.wrapping_shl(gob_size_shift))
                    .wrapping_add(z_high.wrapping_mul(slice_size)),
            );
        }
        mip_offset = mip_offset.wrapping_add(calculate_level_size(&level_info, level));
    }
    offsets
}

/// Port of `CalculateSliceSubresources`.
pub fn calculate_slice_subresources(info: &ImageInfo) -> SmallVec<[SubresourceBase; 16]> {
    assert_fail_soft(info.image_type == ImageType::E3D, || {
        "CalculateSliceSubresources requires a 3D image".to_owned()
    });
    let mut subresources = SmallVec::with_capacity(num_slices(info) as usize);
    for level in 0..info.resources.levels {
        let depth = adjust_mip_size(info.size.depth, level as u32) as i32;
        for slice in 0..depth {
            subresources.push(SubresourceBase {
                level,
                layer: slice,
            });
        }
    }
    subresources
}

/// Port of `CalculateLevelStrideAlignment`.
pub fn calculate_level_stride_alignment(info: &ImageInfo, level: u32) -> u32 {
    let tile_size = default_block_size(info.format);
    let level_size = adjust_mip_size_3d(info.size, level);
    let num_tiles = adjust_tile_size_3d(level_size, tile_size);
    let block =
        adjust_mip_block_size_3d(num_tiles, info.block(), level, info.resources.levels as u32);
    let bpp_log2 = bytes_per_block_log2_format(info.format);
    stride_alignment(num_tiles, block, bpp_log2, info.tile_width_spacing)
}

// ── Format helpers ─────────────────────────────────────────────────────

/// Port of `PixelFormatFromTIC`.
pub fn pixel_format_from_tic(config: &TicEntry) -> PixelFormat {
    pixel_format_from_texture_info_raw(
        config.format(),
        config.r_type(),
        config.g_type(),
        config.b_type(),
        config.a_type(),
        config.srgb_conversion() != 0,
    )
}

/// Port of `RenderTargetImageViewType`.
pub fn render_target_image_view_type(info: &ImageInfo) -> ImageViewType {
    match info.image_type {
        ImageType::E2D => {
            if info.resources.layers > 1 {
                ImageViewType::E2DArray
            } else {
                ImageViewType::E2D
            }
        }
        ImageType::E3D => ImageViewType::E2DArray,
        ImageType::Linear => ImageViewType::E2D,
        _ => {
            assert_fail_soft(false, || {
                format!("Unimplemented image type {:?}", info.image_type)
            });
            ImageViewType::E1D
        }
    }
}

// ── Copy generation ────────────────────────────────────────────────────

/// Port of `MakeShrinkImageCopies`.
pub fn make_shrink_image_copies(
    dst: &ImageInfo,
    src: &ImageInfo,
    base: SubresourceBase,
    up_scale: u32,
    down_shift: u32,
) -> SmallVec<[ImageCopy; 16]> {
    assert_fail_soft(dst.resources.levels >= src.resources.levels, || {
        "MakeShrinkImageCopies destination has fewer levels than the source".to_owned()
    });
    let is_dst_3d = dst.image_type == ImageType::E3D;
    if is_dst_3d {
        assert_fail_soft(src.image_type == ImageType::E3D, || {
            "MakeShrinkImageCopies requires a 3D source for a 3D destination".to_owned()
        });
        assert_fail_soft(src.resources.levels == 1, || {
            "MakeShrinkImageCopies requires one source level for a 3D destination".to_owned()
        });
    }
    let both_2d = src.image_type == ImageType::E2D && dst.image_type == ImageType::E2D;
    let mut copies = SmallVec::with_capacity(src.resources.levels as usize);
    for level in 0..src.resources.levels {
        let src_subresource = SubresourceLayers {
            base_level: level,
            base_layer: 0,
            num_layers: src.resources.layers,
        };
        let dst_subresource = SubresourceLayers {
            base_level: base.level + level,
            base_layer: if is_dst_3d { 0 } else { base.layer },
            num_layers: if is_dst_3d { 1 } else { src.resources.layers },
        };
        let src_offset = Offset3D { x: 0, y: 0, z: 0 };
        let dst_offset = Offset3D {
            x: 0,
            y: 0,
            z: if is_dst_3d { base.layer } else { 0 },
        };
        let mip = adjust_mip_size_3d(dst.size, (base.level + level) as u32);
        let mut extent = adjust_samples_size(mip, dst.num_samples);
        if is_dst_3d {
            extent.depth = src.size.depth;
        }
        extent.width = (extent.width.wrapping_mul(up_scale) >> down_shift).max(1);
        if both_2d {
            extent.height = (extent.height.wrapping_mul(up_scale) >> down_shift).max(1);
        }
        copies.push(ImageCopy {
            src_subresource,
            dst_subresource,
            src_offset,
            dst_offset,
            extent,
        });
    }
    copies
}

/// Port of `MakeReinterpretImageCopies`.
pub fn make_reinterpret_image_copies(
    src: &ImageInfo,
    up_scale: u32,
    down_shift: u32,
) -> SmallVec<[ImageCopy; 16]> {
    let is_3d = src.image_type == ImageType::E3D;
    let mut copies = SmallVec::with_capacity(src.resources.levels as usize);
    for level in 0..src.resources.levels {
        let subresource = SubresourceLayers {
            base_level: level,
            base_layer: 0,
            num_layers: src.resources.layers,
        };
        let offset = Offset3D { x: 0, y: 0, z: 0 };
        let mip = adjust_mip_size_3d(src.size, level as u32);
        let mut extent = adjust_samples_size(mip, src.num_samples);
        if is_3d {
            extent.depth = src.size.depth;
        }
        extent.width = (extent.width.wrapping_mul(up_scale) >> down_shift).max(1);
        extent.height = (extent.height.wrapping_mul(up_scale) >> down_shift).max(1);
        copies.push(ImageCopy {
            src_subresource: subresource,
            dst_subresource: subresource,
            src_offset: offset,
            dst_offset: offset,
            extent,
        });
    }
    copies
}

// ── Validation ─────────────────────────────────────────────────────────

/// Port of upstream `VideoCommon::IsValidEntry` (util.cpp:818-832).
///
/// ```cpp
/// const GPUVAddr address = config.Address();
/// if (address == 0) return false;
/// if (address >= (1ULL << 40)) return false;
/// if (gpu_memory.GpuToCpuAddress(address).has_value()) return true;
/// const ImageInfo info{config};
/// const size_t guest_size_bytes = CalculateGuestSizeInBytes(info);
/// return gpu_memory.GpuToCpuAddress(address, guest_size_bytes).has_value();
/// ```
///
pub fn is_valid_entry(
    gpu_memory: &dyn super::descriptor_table::GpuMemoryReader,
    config: &crate::textures::texture::TicEntry,
) -> bool {
    is_valid_entry_with_range_valid(config, |address, size| {
        if size <= 1 {
            gpu_memory.addr_valid(address)
        } else {
            gpu_memory.range_valid(address, size)
        }
    })
}

/// Callback-shaped adaptation of `IsValidEntry` for texture-cache call sites
/// that already hold the memory-manager lock.
pub fn is_valid_entry_with_range_valid(
    config: &crate::textures::texture::TicEntry,
    mut range_valid: impl FnMut(GPUVAddr, u64) -> bool,
) -> bool {
    let address = config.address();
    if address == 0 {
        return false;
    }
    if address >= 1u64 << 40 {
        return false;
    }
    if range_valid(address, 1) {
        return true;
    }
    let info = super::image_info::ImageInfo::from_tic_entry(config);
    let guest_size_bytes = calculate_guest_size_in_bytes(&info) as u64;
    range_valid(address, guest_size_bytes)
}

// ── Swizzle / unswizzle ────────────────────────────────────────────────

/// Port of `UnswizzleImage`.
///
/// Converts guest texture memory into the linear upload buffer consumed by the
/// backend `Image::UploadMemory` path and returns the matching
/// `BufferImageCopy` list. The caller owns the GPU-memory read; `input` starts
/// at `gpu_addr` and contains the image's guest-layout bytes.
pub fn unswizzle_image(
    _gpu_memory: &(),
    _gpu_addr: GPUVAddr,
    info: &ImageInfo,
    input: &[u8],
    output: &mut [u8],
) -> SmallVec<[BufferImageCopy; 16]> {
    let bytes_per_block = surface::bytes_per_block(info.format);
    let tile_size = default_block_size(info.format);

    if info.image_type == ImageType::Linear {
        assert!(
            output.len() >= input.len(),
            "UnswizzleImage linear output is too small: output={} input={}",
            output.len(),
            input.len()
        );
        output[..input.len()].copy_from_slice(input);
        let bpp_log2 = bytes_per_block_log2(bytes_per_block);
        assert_fail_soft(
            (info.pitch() >> bpp_log2) << bpp_log2 == info.pitch(),
            || "UnswizzleImage pitch is not aligned to the bytes per block".to_owned(),
        );
        return smallvec![BufferImageCopy {
            buffer_offset: 0,
            buffer_size: input.len(),
            buffer_row_length: info.pitch().wrapping_mul(tile_size.width) >> bpp_log2,
            buffer_image_height: info.size.height,
            image_subresource: SubresourceLayers {
                base_level: 0,
                base_layer: 0,
                num_layers: 1,
            },
            image_offset: Offset3D { x: 0, y: 0, z: 0 },
            image_extent: info.size,
        }];
    }

    let bpp_log2 = bytes_per_block_log2_format(info.format);
    let level_info = make_level_info_from_image(info);
    let num_layers = info.resources.layers;
    let num_levels = info.resources.levels;
    let level_sizes = calculate_level_sizes(&level_info, num_levels as u32);
    let gob = gob_size(bpp_log2, info.block().height, info.tile_width_spacing);
    let layer_size = calculate_level_bytes(&level_sizes, num_levels as u32);
    let layer_stride = align_layer_size(
        layer_size,
        info.size,
        level_info.block,
        tile_size.height,
        info.tile_width_spacing,
    );
    let mut guest_offset = 0usize;
    let mut host_offset = 0u32;
    let mut copies = SmallVec::with_capacity(num_levels as usize);

    for level in 0..num_levels {
        let level_size = adjust_mip_size_3d(info.size, level as u32);
        let num_tiles = adjust_tile_size_3d(level_size, tile_size);
        let num_blocks_per_layer = num_tiles
            .width
            .wrapping_mul(num_tiles.height)
            .wrapping_mul(num_tiles.depth);
        let host_bytes_per_layer = num_blocks_per_layer.wrapping_shl(bpp_log2);
        copies.push(BufferImageCopy {
            buffer_offset: host_offset as usize,
            buffer_size: (host_bytes_per_layer as usize).wrapping_mul(num_layers as usize),
            buffer_row_length: align_up(level_size.width, tile_size.width),
            buffer_image_height: align_up(level_size.height, tile_size.height),
            image_subresource: SubresourceLayers {
                base_level: level,
                base_layer: 0,
                num_layers,
            },
            image_offset: Offset3D { x: 0, y: 0, z: 0 },
            image_extent: level_size,
        });

        let block = adjust_mip_block_size_3d(
            num_tiles,
            level_info.block,
            level as u32,
            level_info.num_levels,
        );
        let stride_alignment = stride_alignment_gob(num_tiles, info.block(), gob, bpp_log2);
        let mut guest_layer_offset = 0usize;

        for _layer in 0..num_layers {
            let dst_offset = host_offset as usize;
            let src_offset = guest_offset + guest_layer_offset;
            let dst = &mut output[dst_offset..];
            let src = &input[src_offset..];
            crate::textures::decoders::unswizzle_texture(
                dst,
                src,
                1u32 << bpp_log2,
                num_tiles.width,
                num_tiles.height,
                num_tiles.depth,
                block.height,
                block.depth,
                stride_alignment,
            );
            guest_layer_offset += layer_stride as usize;
            host_offset = host_offset.wrapping_add(host_bytes_per_layer);
        }
        guest_offset += level_sizes[level as usize] as usize;
    }

    copies
}

/// Port of `ConvertImage`.
///
/// Decompresses ASTC or BCn compressed images into the host output format.
pub fn convert_image(
    input: &[u8],
    info: &ImageInfo,
    output: &mut [u8],
    copies: &mut [BufferImageCopy],
) {
    let mut output_offset = 0u32;
    let mut decode_scratch = ScratchBuffer::<u8>::new();
    let tile_size = default_block_size(info.format);

    for copy in copies.iter_mut() {
        let level = copy.image_subresource.base_level;
        let mip_size = adjust_mip_size_3d(info.size, level as u32);
        assert_fail_soft(copy.image_offset == Offset3D::default(), || {
            "ConvertImage requires a zero image offset".to_owned()
        });
        assert_fail_soft(copy.image_subresource.base_layer == 0, || {
            "ConvertImage requires base layer zero".to_owned()
        });
        assert_fail_soft(copy.image_extent == mip_size, || {
            "ConvertImage copy extent does not match its mip size".to_owned()
        });
        assert_fail_soft(
            copy.buffer_row_length == align_up(mip_size.width, tile_size.width),
            || "ConvertImage row length does not match the aligned mip width".to_owned(),
        );
        assert_fail_soft(
            copy.buffer_image_height == align_up(mip_size.height, tile_size.height),
            || "ConvertImage image height does not match the aligned mip height".to_owned(),
        );

        let input_offset = copy.buffer_offset;
        copy.buffer_offset = output_offset as usize;

        let astc = surface::is_pixel_format_astc(info.format);

        let recompression = *common::settings::values().astc_recompression.get_value();

        if astc && recompression == common::settings_enums::AstcRecompression::Uncompressed {
            let input_slice = &input[input_offset..];
            let depth_layers =
                (copy.image_subresource.num_layers as u32).wrapping_mul(copy.image_extent.depth);
            crate::textures::astc::decompress(
                input_slice,
                copy.image_extent.width,
                copy.image_extent.height,
                depth_layers,
                tile_size.width,
                tile_size.height,
                &mut output[output_offset as usize..],
            );

            output_offset = output_offset.wrapping_add(
                copy.image_extent
                    .width
                    .wrapping_mul(copy.image_extent.height)
                    .wrapping_mul(copy.image_subresource.num_layers as u32)
                    .wrapping_mul(surface::bytes_per_block(
                        surface::PixelFormat::A8B8G8R8Unorm,
                    )),
            );
        } else if astc {
            let bpp_div = if recompression == common::settings_enums::AstcRecompression::Bc1 {
                2
            } else {
                1
            };
            let plane_dim = copy
                .image_extent
                .width
                .wrapping_mul(copy.image_extent.height);
            let depth_layers =
                (copy.image_subresource.num_layers as u32).wrapping_mul(copy.image_extent.depth);
            let level_size = plane_dim
                .wrapping_mul(copy.image_extent.depth)
                .wrapping_mul(copy.image_subresource.num_layers as u32)
                .wrapping_mul(surface::bytes_per_block(
                    surface::PixelFormat::A8B8G8R8Unorm,
                ));
            decode_scratch.resize_destructive(level_size as usize);

            crate::textures::astc::decompress(
                &input[input_offset..],
                copy.image_extent.width,
                copy.image_extent.height,
                depth_layers,
                tile_size.width,
                tile_size.height,
                &mut decode_scratch,
            );

            if recompression == common::settings_enums::AstcRecompression::Bc1 {
                crate::textures::bcn::compress_bc1(
                    &decode_scratch,
                    copy.image_extent.width,
                    copy.image_extent.height,
                    depth_layers,
                    &mut output[output_offset as usize..],
                );
            } else {
                crate::textures::bcn::compress_bc3(
                    &decode_scratch,
                    copy.image_extent.width,
                    copy.image_extent.height,
                    depth_layers,
                    &mut output[output_offset as usize..],
                );
            }

            let aligned_plane_dim = align_up(copy.image_extent.width, 4)
                .wrapping_mul(align_up(copy.image_extent.height, 4));
            copy.buffer_size = (aligned_plane_dim
                .wrapping_mul(copy.image_extent.depth)
                .wrapping_mul(copy.image_subresource.num_layers as u32)
                / bpp_div) as usize;
            output_offset = output_offset.wrapping_add(copy.buffer_size as u32);
        } else {
            crate::texture_cache::decode_bc::decompress_bcn(
                &input[input_offset..],
                &mut output[output_offset as usize..],
                copy,
                info.format,
            );
            let bytes = copy
                .image_extent
                .width
                .wrapping_mul(copy.image_extent.height)
                .wrapping_mul(copy.image_subresource.num_layers as u32)
                .wrapping_mul(crate::texture_cache::decode_bc::converted_bytes_per_block(
                    info.format,
                ));
            output_offset = output_offset.wrapping_add(bytes);
        }

        copy.buffer_row_length = mip_size.width;
        copy.buffer_image_height = mip_size.height;
    }
}

/// Port of `FullDownloadCopies`.
pub fn full_download_copies(info: &ImageInfo) -> SmallVec<[BufferImageCopy; 16]> {
    let size = info.size;
    let bpb = surface::bytes_per_block(info.format);
    if info.image_type == ImageType::Linear {
        assert_fail_soft(info.pitch() % bpb == 0, || {
            "FullDownloadCopies pitch is not divisible by bytes per block".to_owned()
        });
        return smallvec![BufferImageCopy {
            buffer_offset: 0,
            buffer_size: (info.pitch() as usize).wrapping_mul(size.height as usize),
            buffer_row_length: info.pitch() / bpb,
            buffer_image_height: size.height,
            image_subresource: SubresourceLayers {
                base_level: 0,
                base_layer: 0,
                num_layers: 1,
            },
            image_offset: Offset3D { x: 0, y: 0, z: 0 },
            image_extent: size,
        }];
    }
    if info.tile_width_spacing > 0 {
        assert_fail_soft(false, || {
            format!(
                "FullDownloadCopies: tile_width_spacing={} is unimplemented",
                info.tile_width_spacing
            )
        });
    }
    let num_layers = info.resources.layers;
    let num_levels = info.resources.levels;
    let tile_size = default_block_size(info.format);
    let mut host_offset = 0u32;
    let mut copies = SmallVec::with_capacity(num_levels as usize);
    for level in 0..num_levels {
        let level_size = adjust_mip_size_3d(size, level as u32);
        let adj = adjust_tile_size_3d(level_size, tile_size);
        let num_blocks_per_layer = adj.width.wrapping_mul(adj.height).wrapping_mul(adj.depth);
        let host_bytes_per_level = num_blocks_per_layer
            .wrapping_mul(bpb)
            .wrapping_mul(num_layers as u32);
        copies.push(BufferImageCopy {
            buffer_offset: host_offset as usize,
            buffer_size: host_bytes_per_level as usize,
            buffer_row_length: level_size.width,
            buffer_image_height: level_size.height,
            image_subresource: SubresourceLayers {
                base_level: level,
                base_layer: 0,
                num_layers: info.resources.layers,
            },
            image_offset: Offset3D { x: 0, y: 0, z: 0 },
            image_extent: level_size,
        });
        host_offset = host_offset.wrapping_add(host_bytes_per_level);
    }
    copies
}

/// Port of `FullUploadSwizzles`.
pub fn full_upload_swizzles(info: &ImageInfo) -> SmallVec<[SwizzleParameters; 16]> {
    let tile_size = default_block_size(info.format);
    if info.image_type == ImageType::Linear {
        return smallvec![SwizzleParameters {
            num_tiles: adjust_tile_size_3d(info.size, tile_size),
            block: Extent3D {
                width: 0,
                height: 0,
                depth: 0,
            },
            buffer_offset: 0,
            level: 0,
        }];
    }
    let level_info = make_level_info_from_image(info);
    let size = info.size;
    let num_levels = info.resources.levels;
    let mut guest_offset = 0u32;
    let mut params = SmallVec::with_capacity(num_levels as usize);
    for level in 0..num_levels {
        let level_size = adjust_mip_size_3d(size, level as u32);
        let num_tiles = adjust_tile_size_3d(level_size, tile_size);
        let block = adjust_mip_block_size_3d(
            num_tiles,
            level_info.block,
            level as u32,
            level_info.num_levels,
        );
        params.push(SwizzleParameters {
            num_tiles,
            block,
            buffer_offset: guest_offset as usize,
            level,
        });
        guest_offset = guest_offset.wrapping_add(calculate_level_size(&level_info, level as u32));
    }
    params
}

/// Port of `SwizzlePitchLinearImage`.
fn swizzle_pitch_linear_image(
    guest_memory_writer: &dyn Fn(u64, &[u8]),
    gpu_addr: GPUVAddr,
    info: &ImageInfo,
    copy: &BufferImageCopy,
    memory: &[u8],
) {
    assert_fail_soft(copy.image_offset.z == 0, || {
        "SwizzlePitchLinearImage requires z offset zero".to_owned()
    });
    assert_fail_soft(copy.image_extent.depth == 1, || {
        "SwizzlePitchLinearImage requires depth one".to_owned()
    });
    assert_fail_soft(copy.image_subresource.base_level == 0, || {
        "SwizzlePitchLinearImage requires base level zero".to_owned()
    });
    assert_fail_soft(copy.image_subresource.base_layer == 0, || {
        "SwizzlePitchLinearImage requires base layer zero".to_owned()
    });
    assert_fail_soft(copy.image_subresource.num_layers == 1, || {
        "SwizzlePitchLinearImage requires one layer".to_owned()
    });

    let bytes_per_block = surface::bytes_per_block(info.format);
    let row_length = copy.image_extent.width.wrapping_mul(bytes_per_block) as usize;
    let guest_offset_x = (copy.image_offset.x as u32).wrapping_mul(bytes_per_block) as u64;

    for line in 0..copy.image_extent.height {
        let host_offset_y = line.wrapping_mul(info.pitch()) as usize;
        let guest_offset_y = (copy.image_offset.y as u32)
            .wrapping_add(line)
            .wrapping_mul(info.pitch()) as u64;
        let guest_offset = guest_offset_x.wrapping_add(guest_offset_y);
        guest_memory_writer(
            gpu_addr.wrapping_add(guest_offset),
            &memory[host_offset_y..host_offset_y + row_length],
        );
    }
}

/// Port of `SwizzleBlockLinearImage`.
fn swizzle_block_linear_image(
    guest_memory_reader: &dyn Fn(u64, &mut [u8]),
    guest_memory_writer: &dyn Fn(u64, &[u8]),
    gpu_addr: GPUVAddr,
    info: &ImageInfo,
    copy: &BufferImageCopy,
    memory: &[u8],
    tmp_buffer: &mut ScratchBuffer<u8>,
) {
    let size = info.size;
    let level_info = make_level_info_from_image(info);
    let tile_size = default_block_size(info.format);
    let bytes_per_block = surface::bytes_per_block(info.format);

    let level = copy.image_subresource.base_level as u32;
    let level_size = adjust_mip_size_3d(size, level);
    let host_bytes_per_layer = num_blocks(level_size, tile_size).wrapping_mul(bytes_per_block);

    assert_fail_soft(copy.image_offset.x == 0, || {
        "SwizzleBlockLinearImage does not implement a nonzero x offset".to_owned()
    });
    assert_fail_soft(copy.image_offset.y == 0, || {
        "SwizzleBlockLinearImage does not implement a nonzero y offset".to_owned()
    });
    assert_fail_soft(copy.image_offset.z == 0, || {
        "SwizzleBlockLinearImage does not implement a nonzero z offset".to_owned()
    });
    assert_fail_soft(copy.image_extent == level_size, || {
        "SwizzleBlockLinearImage does not implement partial mip extents".to_owned()
    });

    let num_tiles = adjust_tile_size_3d(level_size, tile_size);
    let block = adjust_mip_block_size_3d(num_tiles, level_info.block, level, level_info.num_levels);
    let mut host_offset = copy.buffer_offset;

    let num_levels = info.resources.levels as u32;
    let sizes = calculate_level_sizes(&level_info, num_levels);
    let mut guest_offset = calculate_level_bytes(&sizes, level) as u64;
    let layer_stride = align_layer_size(
        calculate_level_bytes(&sizes, num_levels),
        size,
        level_info.block,
        tile_size.height,
        info.tile_width_spacing,
    ) as u64;
    let subresource_size = sizes[level as usize] as usize;

    for _layer in 0..info.resources.layers {
        tmp_buffer.resize_destructive(subresource_size);
        guest_memory_reader(gpu_addr.wrapping_add(guest_offset), tmp_buffer);
        crate::textures::decoders::swizzle_texture(
            tmp_buffer,
            &memory[host_offset..],
            bytes_per_block,
            num_tiles.width,
            num_tiles.height,
            num_tiles.depth,
            block.height,
            block.depth,
            1,
        );
        guest_memory_writer(gpu_addr.wrapping_add(guest_offset), tmp_buffer);

        host_offset += host_bytes_per_layer as usize;
        guest_offset = guest_offset.wrapping_add(layer_stride);
    }
    assert_fail_soft(host_offset - copy.buffer_offset == copy.buffer_size, || {
        "SwizzleBlockLinearImage consumed a different byte count than the copy declares".to_owned()
    });
}

/// Port of `SwizzleImage`.
///
/// Upstream reads and writes through `Tegra::MemoryManager& gpu_memory`.
/// The paired callbacks preserve the same `UnsafeReadWrite` contract when a
/// block-linear subresource has padding that must survive the writeback.
pub fn swizzle_image(
    guest_memory_reader: &dyn Fn(u64, &mut [u8]),
    guest_memory_writer: &dyn Fn(u64, &[u8]),
    gpu_addr: GPUVAddr,
    info: &ImageInfo,
    copies: &[BufferImageCopy],
    memory: &[u8],
    tmp_buffer: &mut ScratchBuffer<u8>,
) {
    if surface::bytes_per_block(info.format) == 0 {
        return;
    }

    let is_pitch_linear = info.image_type == ImageType::Linear;
    for copy in copies {
        if is_pitch_linear {
            swizzle_pitch_linear_image(guest_memory_writer, gpu_addr, info, copy, memory);
        } else {
            swizzle_block_linear_image(
                guest_memory_reader,
                guest_memory_writer,
                gpu_addr,
                info,
                copy,
                memory,
                tmp_buffer,
            );
        }
    }
}

// ── Mip helpers ────────────────────────────────────────────────────────

/// Compute the size at a given mip level.
///
/// Port of `MipSize`.
pub fn mip_size(size: Extent3D, level: u32) -> Extent3D {
    adjust_mip_size_3d(size, level)
}

/// Compute the block size at a given mip level.
///
/// Port of `MipBlockSize`.
pub fn mip_block_size(info: &ImageInfo, level: u32) -> Extent3D {
    let level_info = make_level_info_from_image(info);
    let tile_size = default_block_size(info.format);
    let level_size = adjust_mip_size_3d(info.size, level);
    let num_tiles = adjust_tile_size_3d(level_size, tile_size);
    adjust_mip_block_size_3d(num_tiles, level_info.block, level, level_info.num_levels)
}

// ── Compatibility checks ───────────────────────────────────────────────

/// Port of `IsBlockLinearSizeCompatible`.
pub fn is_block_linear_size_compatible(
    lhs: &ImageInfo,
    rhs: &ImageInfo,
    lhs_level: u32,
    rhs_level: u32,
    strict_size: bool,
) -> bool {
    assert_fail_soft(lhs.image_type != ImageType::Linear, || {
        "IsBlockLinearSizeCompatible requires a block-linear lhs".to_owned()
    });
    assert_fail_soft(rhs.image_type != ImageType::Linear, || {
        "IsBlockLinearSizeCompatible requires a block-linear rhs".to_owned()
    });
    if strict_size {
        let lhs_size = adjust_mip_size_3d(lhs.size, lhs_level);
        let rhs_size = adjust_mip_size_3d(rhs.size, rhs_level);
        lhs_size.width == rhs_size.width && lhs_size.height == rhs_size.height
    } else {
        let lhs_size = block_linear_aligned_size(lhs, lhs_level);
        let rhs_size = block_linear_aligned_size(rhs, rhs_level);
        lhs_size.width == rhs_size.width && lhs_size.height == rhs_size.height
    }
}

/// Port of `IsPitchLinearSameSize`.
pub fn is_pitch_linear_same_size(lhs: &ImageInfo, rhs: &ImageInfo, strict_size: bool) -> bool {
    assert_fail_soft(lhs.image_type == ImageType::Linear, || {
        "IsPitchLinearSameSize requires a linear lhs".to_owned()
    });
    assert_fail_soft(rhs.image_type == ImageType::Linear, || {
        "IsPitchLinearSameSize requires a linear rhs".to_owned()
    });
    if strict_size {
        lhs.size.width == rhs.size.width && lhs.size.height == rhs.size.height
    } else {
        pitch_linear_aligned_size(lhs) == pitch_linear_aligned_size(rhs)
    }
}

/// Port of `IsBlockLinearSizeCompatibleBPPRelaxed`.
pub fn is_block_linear_size_compatible_bpp_relaxed(
    lhs: &ImageInfo,
    rhs: &ImageInfo,
    lhs_level: u32,
    rhs_level: u32,
) -> bool {
    assert_fail_soft(lhs.image_type != ImageType::Linear, || {
        "IsBlockLinearSizeCompatibleBPPRelaxed requires a block-linear lhs".to_owned()
    });
    assert_fail_soft(rhs.image_type != ImageType::Linear, || {
        "IsBlockLinearSizeCompatibleBPPRelaxed requires a block-linear rhs".to_owned()
    });
    let lhs_bpp = surface::bytes_per_block(lhs.format);
    let rhs_bpp = surface::bytes_per_block(rhs.format);
    let lhs_size = adjust_mip_size_3d(lhs.size, lhs_level);
    let rhs_size = adjust_mip_size_3d(rhs.size, rhs_level);
    align_up_log2(lhs_size.width.wrapping_mul(lhs_bpp), GOB_SIZE_X_SHIFT)
        == align_up_log2(rhs_size.width.wrapping_mul(rhs_bpp), GOB_SIZE_X_SHIFT)
        && align_up_log2(lhs_size.height, GOB_SIZE_Y_SHIFT)
            == align_up_log2(rhs_size.height, GOB_SIZE_Y_SHIFT)
}

/// Port of `ResolveOverlapEqualAddress`.
fn resolve_overlap_equal_address(
    new_info: &ImageInfo,
    overlap: &ImageBase,
    strict_size: bool,
) -> Option<SubresourceExtent> {
    let info = &overlap.info;
    if !is_block_linear_size_compatible(new_info, info, 0, 0, strict_size) {
        return None;
    }
    if new_info.block() != info.block() {
        return None;
    }
    let resources = new_info.resources;
    Some(SubresourceExtent {
        levels: resources.levels.max(info.resources.levels),
        layers: resources.layers.max(info.resources.layers),
    })
}

/// Port of `ResolveOverlapRightAddress3D`.
fn resolve_overlap_right_address_3d(
    new_info: &ImageInfo,
    gpu_addr: GPUVAddr,
    overlap: &ImageBase,
    strict_size: bool,
) -> Option<SubresourceExtent> {
    let slice_offsets = calculate_slice_offsets(new_info);
    let diff = (overlap.gpu_addr - gpu_addr) as u32;
    let it = slice_offsets.iter().position(|&o| o == diff);
    let idx = it?;
    let subresources = calculate_slice_subresources(new_info);
    let base = subresources[idx];
    let info = &overlap.info;
    if !is_block_linear_size_compatible(new_info, info, base.level as u32, 0, strict_size) {
        return None;
    }
    let mip_depth = adjust_mip_size(new_info.size.depth, base.level as u32);
    if mip_depth < info.size.depth.wrapping_add(base.layer as u32) {
        return None;
    }
    if mip_block_size(new_info, base.level as u32) != info.block() {
        return None;
    }
    Some(SubresourceExtent {
        levels: new_info
            .resources
            .levels
            .max(info.resources.levels + base.level),
        layers: 1,
    })
}

/// Port of `ResolveOverlapRightAddress2D`.
fn resolve_overlap_right_address_2d(
    new_info: &ImageInfo,
    gpu_addr: GPUVAddr,
    overlap: &ImageBase,
    strict_size: bool,
) -> Option<SubresourceExtent> {
    let layer_stride = new_info.layer_stride as u64;
    let new_size = layer_stride.wrapping_mul(new_info.resources.layers as u64);
    let diff = overlap.gpu_addr - gpu_addr;
    if diff > new_size {
        return None;
    }
    let base_layer = (diff / layer_stride) as i32;
    let mip_offset = (diff % layer_stride) as u32;
    let offsets = calculate_mip_level_offsets(new_info);
    let levels = new_info.resources.levels as usize;
    let it = offsets[..levels].iter().position(|&o| o == mip_offset);
    let level = it? as i32;
    let base = SubresourceBase {
        level,
        layer: base_layer,
    };
    let info = &overlap.info;
    if !is_block_linear_size_compatible(new_info, info, base.level as u32, 0, strict_size) {
        return None;
    }
    if mip_block_size(new_info, base.level as u32) != info.block() {
        return None;
    }
    Some(SubresourceExtent {
        levels: new_info
            .resources
            .levels
            .max(info.resources.levels + base.level),
        layers: new_info
            .resources
            .layers
            .max(info.resources.layers + base.layer),
    })
}

/// Port of `ResolveOverlapRightAddress`.
fn resolve_overlap_right_address(
    new_info: &ImageInfo,
    gpu_addr: GPUVAddr,
    cpu_addr: VAddr,
    overlap: &ImageBase,
    strict_size: bool,
) -> Option<OverlapResult> {
    let resources = if new_info.image_type != ImageType::E3D {
        resolve_overlap_right_address_2d(new_info, gpu_addr, overlap, strict_size)?
    } else {
        resolve_overlap_right_address_3d(new_info, gpu_addr, overlap, strict_size)?
    };
    Some(OverlapResult {
        gpu_addr,
        cpu_addr,
        resources,
    })
}

/// Port of `ResolveOverlapLeftAddress`.
fn resolve_overlap_left_address(
    new_info: &ImageInfo,
    gpu_addr: GPUVAddr,
    _cpu_addr: VAddr,
    overlap: &ImageBase,
    strict_size: bool,
) -> Option<OverlapResult> {
    let base = overlap.try_find_base(gpu_addr)?;
    let info = &overlap.info;
    if !is_block_linear_size_compatible(new_info, info, base.level as u32, 0, strict_size) {
        return None;
    }
    if new_info.block() != mip_block_size(info, base.level as u32) {
        return None;
    }
    let resources = new_info.resources;
    let layers = if info.image_type == ImageType::E3D {
        let mip_depth = adjust_mip_size(info.size.depth, base.level as u32);
        if mip_depth < new_info.size.depth.wrapping_add(base.layer as u32) {
            return None;
        }
        1
    } else {
        resources.layers.max(info.resources.layers + base.layer)
    };
    Some(OverlapResult {
        gpu_addr: overlap.gpu_addr,
        cpu_addr: overlap.cpu_addr,
        resources: SubresourceExtent {
            levels: (resources.levels + base.level).max(info.resources.levels),
            layers,
        },
    })
}

/// Port of `ResolveOverlap`.
pub fn resolve_overlap(
    new_info: &ImageInfo,
    gpu_addr: GPUVAddr,
    cpu_addr: VAddr,
    overlap: &ImageBase,
    strict_size: bool,
    broken_views: bool,
    native_bgr: bool,
) -> Option<OverlapResult> {
    assert_fail_soft(new_info.image_type != ImageType::Linear, || {
        "ResolveOverlap requires a block-linear new image".to_owned()
    });
    assert_fail_soft(overlap.info.image_type != ImageType::Linear, || {
        "ResolveOverlap requires a block-linear overlapping image".to_owned()
    });
    if !is_layer_stride_compatible(new_info, &overlap.info) {
        return None;
    }
    if !crate::compatible_formats::is_view_compatible(
        overlap.info.format,
        new_info.format,
        broken_views,
        native_bgr,
    ) {
        return None;
    }
    if gpu_addr == overlap.gpu_addr {
        let solution = resolve_overlap_equal_address(new_info, overlap, strict_size)?;
        return Some(OverlapResult {
            gpu_addr,
            cpu_addr,
            resources: solution,
        });
    }
    if overlap.gpu_addr > gpu_addr {
        return resolve_overlap_right_address(new_info, gpu_addr, cpu_addr, overlap, strict_size);
    }
    resolve_overlap_left_address(new_info, gpu_addr, cpu_addr, overlap, strict_size)
}

/// Port of `IsLayerStrideCompatible`.
pub fn is_layer_stride_compatible(lhs: &ImageInfo, rhs: &ImageInfo) -> bool {
    if lhs.layer_stride == 0 {
        return true;
    }
    if rhs.layer_stride == 0 {
        return true;
    }
    if lhs.layer_stride == rhs.layer_stride {
        return true;
    }
    if lhs.maybe_unaligned_layer_stride == rhs.maybe_unaligned_layer_stride {
        return true;
    }
    false
}

/// Port of `FindSubresource`.
pub fn find_subresource(
    candidate: &ImageInfo,
    image: &ImageBase,
    candidate_addr: GPUVAddr,
    options: RelaxedOptions,
    broken_views: bool,
    native_bgr: bool,
) -> Option<SubresourceBase> {
    let base = image.try_find_base(candidate_addr)?;
    let existing = &image.info;

    if options.contains(RelaxedOptions::FORMAT) {
        // Format checking is relaxed, but still check matching bytes per block.
        if surface::bytes_per_block(existing.format) != surface::bytes_per_block(candidate.format) {
            return None;
        }
    } else {
        if !crate::compatible_formats::is_view_compatible(
            existing.format,
            candidate.format,
            broken_views,
            native_bgr,
        ) {
            return None;
        }
    }
    if !is_layer_stride_compatible(existing, candidate) {
        return None;
    }
    if existing.image_type != candidate.image_type {
        return None;
    }
    if !options.contains(RelaxedOptions::SAMPLES) && existing.num_samples != candidate.num_samples {
        return None;
    }
    if existing.resources.levels < candidate.resources.levels + base.level {
        return None;
    }
    if existing.image_type == ImageType::E3D {
        let mip_depth = adjust_mip_size(existing.size.depth, base.level as u32);
        if mip_depth < candidate.size.depth.wrapping_add(base.layer as u32) {
            return None;
        }
    } else if existing.resources.layers < candidate.resources.layers + base.layer {
        return None;
    }
    let strict_size = !options.contains(RelaxedOptions::SIZE);
    if !is_block_linear_size_compatible(existing, candidate, base.level as u32, 0, strict_size) {
        return None;
    }
    Some(base)
}

/// Port of `IsSubresource`.
pub fn is_subresource(
    candidate: &ImageInfo,
    image: &ImageBase,
    candidate_addr: GPUVAddr,
    options: RelaxedOptions,
    broken_views: bool,
    native_bgr: bool,
) -> bool {
    find_subresource(
        candidate,
        image,
        candidate_addr,
        options,
        broken_views,
        native_bgr,
    )
    .is_some()
}

/// Port of `IsSubCopy`.
pub fn is_sub_copy(candidate: &ImageInfo, image: &ImageBase, candidate_addr: GPUVAddr) -> bool {
    let base = match image.try_find_base(candidate_addr) {
        Some(b) => b,
        None => return false,
    };
    let existing = &image.info;
    if existing.resources.levels < candidate.resources.levels + base.level {
        return false;
    }
    if existing.image_type == ImageType::E3D {
        let mip_depth = adjust_mip_size(existing.size.depth, base.level as u32);
        if mip_depth < candidate.size.depth.wrapping_add(base.layer as u32) {
            return false;
        }
    } else if existing.resources.layers < candidate.resources.layers + base.layer {
        return false;
    }
    if !is_block_linear_size_compatible_bpp_relaxed(existing, candidate, base.level as u32, 0) {
        return false;
    }
    true
}

/// Port of `DeduceBlitImages`.
pub fn deduce_blit_images(
    dst_info: &mut ImageInfo,
    src_info: &mut ImageInfo,
    dst: Option<&ImageBase>,
    src: Option<&ImageBase>,
) {
    let original_dst_format = dst_info.format;
    if let Some(s) = src {
        if surface::get_format_type(s.info.format) != surface::SurfaceType::ColorTexture {
            src_info.format = s.info.format;
        }
    }
    if let Some(d) = dst {
        if surface::get_format_type(d.info.format) != surface::SurfaceType::ColorTexture {
            dst_info.format = d.info.format;
        }
    }
    if let Some(s) = src {
        if surface::get_format_type(s.info.format) != surface::SurfaceType::ColorTexture {
            dst_info.format = s.info.format;
        }
    }
    if let Some(d) = dst {
        if surface::get_format_type(d.info.format) != surface::SurfaceType::ColorTexture {
            if let Some(s) = src {
                if surface::get_format_type(s.info.format) == surface::SurfaceType::ColorTexture {
                    dst_info.format = original_dst_format;
                }
            } else {
                src_info.format = d.info.format;
            }
        }
    }
}

/// Port of `MapSizeBytes`.
pub fn map_size_bytes(image: &ImageBase) -> u32 {
    use super::image_base::ImageFlagBits;
    if image.flags.contains(ImageFlagBits::ACCELERATED_UPLOAD) {
        image.guest_size_bytes
    } else if image.flags.contains(ImageFlagBits::CONVERTED) {
        image.converted_size_bytes
    } else {
        image.unswizzled_size_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::texture_cache::image_info::TilingMode;
    use crate::textures::texture::{ComponentType, TextureFormat, TextureType, TicEntry};
    use std::sync::{Arc, Mutex};

    fn make_2d_tic(address: GPUVAddr) -> TicEntry {
        let word0 = (TextureFormat::A8B8G8R8 as u32)
            | ((ComponentType::Unorm as u32) << 7)
            | ((ComponentType::Unorm as u32) << 10)
            | ((ComponentType::Unorm as u32) << 13)
            | ((ComponentType::Unorm as u32) << 16);
        let word1 = address as u32;
        let word2 = ((address >> 32) as u32 & 0xFFFF) | (3 << 21);
        let word3 = 1 << 10;
        let word4 = 63 | ((TextureType::Texture2D as u32) << 23);
        let word5 = 31 | (1 << 31);
        TicEntry {
            raw: [
                word0 as u64 | ((word1 as u64) << 32),
                word2 as u64 | ((word3 as u64) << 32),
                word4 as u64 | ((word5 as u64) << 32),
                0,
            ],
        }
    }

    #[test]
    fn pixel_format_from_tic_forwards_force_fp16_components_like_upstream() {
        let component = ComponentType::SnormForceFp16 as u32;
        let word0 = (TextureFormat::A8B8G8R8 as u32)
            | (component << 7)
            | (component << 10)
            | (component << 13)
            | (component << 16);
        let tic = TicEntry {
            raw: [word0 as u64, 0, 0, 0],
        };

        assert_eq!(pixel_format_from_tic(&tic), PixelFormat::A8B8G8R8Unorm);
    }

    #[test]
    fn is_valid_entry_accepts_size_aware_range_like_upstream() {
        let tic = make_2d_tic(0x5000_0000);
        let expected_size = calculate_guest_size_in_bytes(
            &super::super::image_info::ImageInfo::from_tic_entry(&tic),
        ) as u64;
        let mut calls = Vec::new();

        let valid = is_valid_entry_with_range_valid(&tic, |addr, size| {
            calls.push((addr, size));
            size == expected_size
        });

        assert!(valid);
        assert_eq!(calls, vec![(0x5000_0000, 1), (0x5000_0000, expected_size)]);
    }

    #[test]
    fn is_valid_entry_uses_gpu_memory_reader_range_valid_for_guest_size() {
        struct Reader {
            expected_size: u64,
            calls: Arc<Mutex<Vec<(GPUVAddr, u64)>>>,
        }

        impl super::super::descriptor_table::GpuMemoryReader for Reader {
            fn read_block(&self, _d_address: u64, _output: &mut [u8]) -> bool {
                false
            }

            fn addr_valid(&self, d_address: u64) -> bool {
                self.calls.lock().unwrap().push((d_address, 1));
                false
            }

            fn range_valid(&self, d_address: u64, size: u64) -> bool {
                self.calls.lock().unwrap().push((d_address, size));
                size == self.expected_size
            }
        }

        let tic = make_2d_tic(0x5000_0000);
        let expected_size = calculate_guest_size_in_bytes(
            &super::super::image_info::ImageInfo::from_tic_entry(&tic),
        ) as u64;
        let calls = Arc::new(Mutex::new(Vec::new()));
        let reader = Reader {
            expected_size,
            calls: Arc::clone(&calls),
        };

        assert!(is_valid_entry(&reader, &tic));
        assert_eq!(
            *calls.lock().unwrap(),
            vec![(0x5000_0000, 1), (0x5000_0000, expected_size)]
        );
    }

    #[test]
    fn full_download_copies_tile_width_spacing_reports_and_continues_like_upstream() {
        let info = ImageInfo {
            format: PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: 64,
                height: 64,
                depth: 1,
            },
            tiling: TilingMode::BlockLinear(Extent3D {
                width: 0,
                height: 0,
                depth: 0,
            }),
            layer_stride: 0,
            maybe_unaligned_layer_stride: 0,
            num_samples: 1,
            tile_width_spacing: 1,
            rescaleable: false,
            downscaleable: false,
            forced_flushed: false,
            dma_downloaded: false,
            is_sparse: false,
        };

        let copies = full_download_copies(&info);
        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0].buffer_offset, 0);
        assert_eq!(copies[0].buffer_size, 64 * 64 * 4);
        assert_eq!(copies[0].buffer_row_length, 64);
        assert_eq!(copies[0].buffer_image_height, 64);
    }

    #[test]
    fn calculate_guest_size_uses_layer_stride_for_layered_images() {
        let info = ImageInfo {
            format: PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            resources: SubresourceExtent {
                levels: 1,
                layers: 3,
            },
            size: Extent3D {
                width: 16,
                height: 16,
                depth: 1,
            },
            tiling: TilingMode::BlockLinear(Extent3D {
                width: 0,
                height: 0,
                depth: 0,
            }),
            layer_stride: 0x1000,
            maybe_unaligned_layer_stride: 0x400,
            num_samples: 1,
            tile_width_spacing: 0,
            rescaleable: false,
            downscaleable: false,
            forced_flushed: false,
            dma_downloaded: false,
            is_sparse: false,
        };

        assert_eq!(calculate_guest_size_in_bytes(&info), 0x3000);
    }

    #[test]
    fn calculate_guest_size_linear_uses_block_height_rows() {
        let info = ImageInfo {
            format: PixelFormat::Bc1RgbaUnorm,
            image_type: ImageType::Linear,
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: 16,
                height: 8,
                depth: 1,
            },
            tiling: TilingMode::PitchLinear(16),
            layer_stride: 0,
            maybe_unaligned_layer_stride: 0,
            num_samples: 1,
            tile_width_spacing: 0,
            rescaleable: false,
            downscaleable: false,
            forced_flushed: false,
            dma_downloaded: false,
            is_sparse: false,
        };

        assert_eq!(calculate_guest_size_in_bytes(&info), 32);
    }

    #[test]
    fn calculate_unswizzled_size_linear_uses_block_height_rows() {
        let info = ImageInfo {
            format: PixelFormat::Bc1RgbaUnorm,
            image_type: ImageType::Linear,
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: 16,
                height: 8,
                depth: 1,
            },
            tiling: TilingMode::PitchLinear(16),
            layer_stride: 0,
            maybe_unaligned_layer_stride: 0,
            num_samples: 1,
            tile_width_spacing: 0,
            rescaleable: false,
            downscaleable: false,
            forced_flushed: false,
            dma_downloaded: false,
            is_sparse: false,
        };

        assert_eq!(calculate_unswizzled_size_bytes(&info), 32);
    }

    #[test]
    fn calculate_converted_size_uses_converted_bytes_per_texel() {
        let info = ImageInfo {
            format: PixelFormat::Bc5Unorm,
            image_type: ImageType::E2D,
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: 8,
                height: 8,
                depth: 1,
            },
            tiling: TilingMode::BlockLinear(Extent3D {
                width: 0,
                height: 0,
                depth: 0,
            }),
            layer_stride: 0,
            maybe_unaligned_layer_stride: 0,
            num_samples: 1,
            tile_width_spacing: 0,
            rescaleable: false,
            downscaleable: false,
            forced_flushed: false,
            dma_downloaded: false,
            is_sparse: false,
        };

        assert_eq!(calculate_unswizzled_size_bytes(&info), 64);
        assert_eq!(calculate_converted_size_bytes(&info), 128);
    }

    #[test]
    fn is_sub_copy_3d_uses_upstream_mip_depth_reduction() {
        let existing = ImageInfo {
            format: PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E3D,
            resources: SubresourceExtent {
                levels: 2,
                layers: 1,
            },
            size: Extent3D {
                width: 8,
                height: 8,
                depth: 4,
            },
            tiling: TilingMode::BlockLinear(Extent3D {
                width: 0,
                height: 0,
                depth: 0,
            }),
            layer_stride: 0,
            maybe_unaligned_layer_stride: 0,
            num_samples: 1,
            tile_width_spacing: 0,
            rescaleable: false,
            downscaleable: false,
            forced_flushed: false,
            dma_downloaded: false,
            is_sparse: false,
        };
        let mut candidate = ImageInfo {
            format: PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E3D,
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: 4,
                height: 4,
                depth: 2,
            },
            tiling: TilingMode::BlockLinear(Extent3D {
                width: 0,
                height: 0,
                depth: 0,
            }),
            layer_stride: 0,
            maybe_unaligned_layer_stride: 0,
            num_samples: 1,
            tile_width_spacing: 0,
            rescaleable: false,
            downscaleable: false,
            forced_flushed: false,
            dma_downloaded: false,
            is_sparse: false,
        };
        let image = ImageBase::new(existing, 0x8000, 0x4000);
        let subresources = calculate_slice_subresources(&image.info);
        let offsets = calculate_slice_offsets(&image.info);
        let index = subresources
            .iter()
            .position(|base| base.level == 1 && base.layer == 1)
            .expect("test image must expose mip level 1 slice 1");
        let candidate_addr = image.gpu_addr + offsets[index] as u64;

        assert!(!is_sub_copy(&candidate, &image, candidate_addr));
        candidate.size.depth = 1;
        assert!(is_sub_copy(&candidate, &image, candidate_addr));

        let mut left_candidate = candidate.clone();
        left_candidate.size.width = 16;
        left_candidate.size.height = 16;
        left_candidate.size.depth = 2;
        assert!(
            resolve_overlap_left_address(&left_candidate, candidate_addr, 0, &image, true,)
                .is_none()
        );
        left_candidate.size.depth = 1;
        assert!(
            resolve_overlap_left_address(&left_candidate, candidate_addr, 0, &image, true,)
                .is_some()
        );
    }

    #[test]
    fn size_calculations_preserve_upstream_unsigned_overflow() {
        let info = ImageInfo {
            format: PixelFormat::R16Uint,
            image_type: ImageType::Buffer,
            size: Extent3D {
                width: u32::MAX,
                height: 1,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        let expected = u32::MAX.wrapping_mul(2);

        assert_eq!(calculate_guest_size_in_bytes(&info), expected);
        assert_eq!(calculate_unswizzled_size_bytes(&info), expected);
        assert_eq!(calculate_converted_size_bytes(&info), expected);
    }

    #[test]
    fn mip_offsets_accept_all_sixteen_upstream_levels_and_reject_seventeen() {
        let mut info = ImageInfo {
            format: PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            resources: SubresourceExtent {
                levels: MAX_MIP_LEVELS as i32,
                layers: 1,
            },
            size: Extent3D {
                width: 1,
                height: 1,
                depth: 1,
            },
            tiling: TilingMode::BlockLinear(Extent3D::default()),
            ..ImageInfo::default()
        };

        let offsets = calculate_mip_level_offsets(&info);
        assert_ne!(offsets[MAX_MIP_LEVELS - 1], 0);

        info.resources.levels += 1;
        assert_eq!(calculate_mip_level_offsets(&info), [0; MAX_MIP_LEVELS]);
    }

    #[test]
    fn level_offsets_match_upstream_compile_time_oracles() {
        assert_eq!(
            calculate_level_size(
                &LevelInfo {
                    size: Extent3D {
                        width: 1920,
                        height: 1080,
                        depth: 1,
                    },
                    block: Extent3D {
                        width: 0,
                        height: 2,
                        depth: 0,
                    },
                    tile_size: Extent2D {
                        width: 1,
                        height: 1,
                    },
                    bpp_log2: 2,
                    tile_width_spacing: 0,
                    num_levels: 1,
                },
                0,
            ),
            0x7f8000
        );
        assert_eq!(
            calculate_level_size(
                &LevelInfo {
                    size: Extent3D {
                        width: 32,
                        height: 32,
                        depth: 1,
                    },
                    block: Extent3D {
                        width: 0,
                        height: 0,
                        depth: 4,
                    },
                    tile_size: Extent2D {
                        width: 1,
                        height: 1,
                    },
                    bpp_log2: 4,
                    tile_width_spacing: 0,
                    num_levels: 1,
                },
                0,
            ),
            0x40000
        );
        assert_eq!(
            calculate_level_size(
                &LevelInfo {
                    size: Extent3D {
                        width: 128,
                        height: 8,
                        depth: 1,
                    },
                    block: Extent3D {
                        width: 0,
                        height: 4,
                        depth: 0,
                    },
                    tile_size: Extent2D {
                        width: 1,
                        height: 1,
                    },
                    bpp_log2: 4,
                    tile_width_spacing: 0,
                    num_levels: 1,
                },
                0,
            ),
            0x40000
        );

        let rgba_size = Extent3D {
            width: 1024,
            height: 1024,
            depth: 1,
        };
        let rgba_block = Extent3D {
            width: 0,
            height: 4,
            depth: 0,
        };
        let expected = [
            0, 0x400000, 0x500000, 0x540000, 0x550000, 0x554000, 0x555000, 0x555400, 0x555600,
            0x555800,
        ];
        for (level, expected_offset) in expected.into_iter().enumerate() {
            assert_eq!(
                calculate_level_offset(
                    PixelFormat::A8B8G8R8Unorm,
                    rgba_size,
                    rgba_block,
                    0,
                    level as u32,
                ),
                expected_offset
            );
        }

        assert_eq!(
            calculate_level_offset(
                PixelFormat::R8Sint,
                Extent3D {
                    width: 1920,
                    height: 1080,
                    depth: 1,
                },
                Extent3D {
                    width: 0,
                    height: 2,
                    depth: 0,
                },
                0,
                7,
            ),
            0x2afc00
        );
        assert_eq!(
            calculate_level_offset(
                PixelFormat::Astc2d12x12Unorm,
                Extent3D {
                    width: 8192,
                    height: 4096,
                    depth: 1,
                },
                Extent3D {
                    width: 0,
                    height: 2,
                    depth: 0,
                },
                0,
                12,
            ),
            0x50d200
        );
    }

    #[test]
    fn layer_sizes_match_upstream_compile_time_oracles() {
        fn validate_layer_size(
            format: PixelFormat,
            width: u32,
            height: u32,
            block_height: u32,
            tile_width_spacing: u32,
            level: u32,
        ) -> u32 {
            let size = Extent3D {
                width,
                height,
                depth: 1,
            };
            let block = Extent3D {
                width: 0,
                height: block_height,
                depth: 0,
            };
            let offset = calculate_level_offset(format, size, block, tile_width_spacing, level);
            align_layer_size(
                offset,
                size,
                block,
                surface::default_block_height(format),
                tile_width_spacing,
            )
        }

        assert_eq!(
            validate_layer_size(PixelFormat::Astc2d12x12Unorm, 8192, 4096, 2, 0, 12),
            0x50d800
        );
        assert_eq!(
            validate_layer_size(PixelFormat::A8B8G8R8Unorm, 1024, 1024, 2, 0, 10),
            0x556000
        );
        assert_eq!(
            validate_layer_size(PixelFormat::Bc3Unorm, 128, 128, 2, 0, 8),
            0x6000
        );
        assert_eq!(
            validate_layer_size(PixelFormat::A8B8G8R8Unorm, 518, 572, 4, 3, 1),
            0x190000
        );
        assert_eq!(
            validate_layer_size(PixelFormat::Bc5Unorm, 1024, 1024, 3, 4, 11),
            0x160000
        );
    }

    #[test]
    fn invalid_render_target_type_uses_value_initialized_view_type() {
        let info = ImageInfo {
            image_type: ImageType::Buffer,
            ..ImageInfo::default()
        };

        assert_eq!(render_target_image_view_type(&info), ImageViewType::E1D);
    }

    #[test]
    fn sixteen_entry_results_stay_in_upstream_inline_storage() {
        let info = ImageInfo {
            format: PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E3D,
            resources: SubresourceExtent {
                levels: MAX_MIP_LEVELS as i32,
                layers: 1,
            },
            size: Extent3D {
                width: 1,
                height: 1,
                depth: 1,
            },
            tiling: TilingMode::BlockLinear(Extent3D::default()),
            ..ImageInfo::default()
        };

        let copies = full_download_copies(&info);
        let slices = calculate_slice_offsets(&info);
        let subresources = calculate_slice_subresources(&info);
        assert_eq!(copies.len(), MAX_MIP_LEVELS);
        assert_eq!(slices.len(), MAX_MIP_LEVELS);
        assert_eq!(subresources.len(), MAX_MIP_LEVELS);
        assert!(!copies.spilled());
        assert!(!slices.spilled());
        assert!(!subresources.spilled());
    }

    #[test]
    fn swizzle_image_pitch_linear_writes_rows_to_guest() {
        let writes = Arc::new(Mutex::new(Vec::<(u64, Vec<u8>)>::new()));
        let writes_for_callback = Arc::clone(&writes);
        let writer = move |addr: u64, bytes: &[u8]| {
            writes_for_callback
                .lock()
                .unwrap()
                .push((addr, bytes.to_vec()));
        };

        let info = ImageInfo {
            format: PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::Linear,
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: 2,
                height: 2,
                depth: 1,
            },
            tiling: TilingMode::PitchLinear(16),
            layer_stride: 0,
            maybe_unaligned_layer_stride: 0,
            num_samples: 1,
            tile_width_spacing: 0,
            rescaleable: false,
            downscaleable: false,
            forced_flushed: false,
            dma_downloaded: false,
            is_sparse: false,
        };
        let copy = BufferImageCopy {
            buffer_offset: 0,
            buffer_size: 32,
            buffer_row_length: 4,
            buffer_image_height: 2,
            image_subresource: SubresourceLayers {
                base_level: 0,
                base_layer: 0,
                num_layers: 1,
            },
            image_offset: Offset3D { x: 0, y: 0, z: 0 },
            image_extent: Extent3D {
                width: 2,
                height: 2,
                depth: 1,
            },
        };
        let mut memory: Vec<u8> = (0..32).collect();
        memory[8..16].fill(0xAA);
        memory[24..32].fill(0xBB);
        let mut tmp = ScratchBuffer::new();

        swizzle_image(
            &|_, output| output.fill(0),
            &writer,
            0x1000,
            &info,
            &[copy],
            &memory,
            &mut tmp,
        );

        let writes = writes.lock().unwrap();
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0], (0x1000, vec![0, 1, 2, 3, 4, 5, 6, 7]));
        assert_eq!(writes[1], (0x1010, vec![16, 17, 18, 19, 20, 21, 22, 23]));
    }

    #[test]
    fn swizzle_image_pitch_linear_preserves_signed_unsigned_offset_bits() {
        let writes = Arc::new(Mutex::new(Vec::<(u64, Vec<u8>)>::new()));
        let writes_for_callback = Arc::clone(&writes);
        let writer = move |addr: u64, bytes: &[u8]| {
            writes_for_callback
                .lock()
                .unwrap()
                .push((addr, bytes.to_vec()));
        };

        let info = ImageInfo {
            format: PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::Linear,
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: 1,
                height: 1,
                depth: 1,
            },
            tiling: TilingMode::PitchLinear(16),
            layer_stride: 0,
            maybe_unaligned_layer_stride: 0,
            num_samples: 1,
            tile_width_spacing: 0,
            rescaleable: false,
            downscaleable: false,
            forced_flushed: false,
            dma_downloaded: false,
            is_sparse: false,
        };
        let copy = BufferImageCopy {
            buffer_offset: 0,
            buffer_size: 16,
            buffer_row_length: 4,
            buffer_image_height: 1,
            image_subresource: SubresourceLayers {
                base_level: 0,
                base_layer: 0,
                num_layers: 1,
            },
            image_offset: Offset3D { x: -1, y: 0, z: 0 },
            image_extent: Extent3D {
                width: 1,
                height: 1,
                depth: 1,
            },
        };
        let memory: Vec<u8> = (0..16).collect();
        let mut tmp = ScratchBuffer::new();

        swizzle_image(
            &|_, output| output.fill(0),
            &writer,
            0x1000,
            &info,
            &[copy],
            &memory,
            &mut tmp,
        );

        let writes = writes.lock().unwrap();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0], (0x1_0000_0FFC, vec![0, 1, 2, 3]));
    }

    #[test]
    fn swizzle_image_block_linear_uses_guest_level_offsets_and_layer_stride() {
        let writes = Arc::new(Mutex::new(Vec::<(u64, usize)>::new()));
        let writes_for_callback = Arc::clone(&writes);
        let writer = move |addr: u64, bytes: &[u8]| {
            writes_for_callback
                .lock()
                .unwrap()
                .push((addr, bytes.len()));
        };

        let info = ImageInfo {
            format: PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            resources: SubresourceExtent {
                levels: 2,
                layers: 2,
            },
            size: Extent3D {
                width: 64,
                height: 64,
                depth: 1,
            },
            tiling: TilingMode::BlockLinear(Extent3D {
                width: 0,
                height: 1,
                depth: 0,
            }),
            layer_stride: 0,
            maybe_unaligned_layer_stride: 0,
            num_samples: 1,
            tile_width_spacing: 0,
            rescaleable: false,
            downscaleable: false,
            forced_flushed: false,
            dma_downloaded: false,
            is_sparse: false,
        };
        let copies = full_download_copies(&info);
        let memory = vec![0x5a; calculate_unswizzled_size_bytes(&info) as usize];
        let mut tmp = ScratchBuffer::new();

        swizzle_image(
            &|_, output| output.fill(0),
            &writer,
            0x4000,
            &info,
            &copies,
            &memory,
            &mut tmp,
        );

        let level_info = make_level_info_from_image(&info);
        let sizes = calculate_level_sizes(&level_info, info.resources.levels as u32);
        let layer_stride = align_layer_size(
            calculate_level_bytes(&sizes, info.resources.levels as u32),
            info.size,
            info.block(),
            surface::default_block_height(info.format),
            info.tile_width_spacing,
        );
        let writes = writes.lock().unwrap();
        assert_eq!(
            writes.iter().map(|&(addr, _)| addr).collect::<Vec<_>>(),
            vec![
                0x4000,
                0x4000 + layer_stride as u64,
                0x4000 + sizes[0] as u64,
                0x4000 + sizes[0] as u64 + layer_stride as u64,
            ]
        );
        assert_eq!(
            writes.iter().map(|&(_, len)| len).collect::<Vec<_>>(),
            vec![
                sizes[0] as usize,
                sizes[0] as usize,
                sizes[1] as usize,
                sizes[1] as usize,
            ]
        );
        assert_ne!(copies[1].buffer_offset as u32, sizes[0]);
    }

    #[test]
    fn swizzle_image_block_linear_preserves_guest_padding_like_unsafe_read_write() {
        let written = Arc::new(Mutex::new(Vec::<u8>::new()));
        let written_for_callback = Arc::clone(&written);
        let writer = move |_addr: u64, bytes: &[u8]| {
            *written_for_callback.lock().unwrap() = bytes.to_vec();
        };
        let info = ImageInfo {
            format: PixelFormat::R16G16B16A16Uint,
            image_type: ImageType::E3D,
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: 8,
                height: 8,
                depth: 1,
            },
            tiling: TilingMode::BlockLinear(Extent3D {
                width: 0,
                height: 0,
                depth: 4,
            }),
            layer_stride: 0x4000,
            maybe_unaligned_layer_stride: 0x4000,
            num_samples: 1,
            tile_width_spacing: 0,
            rescaleable: false,
            downscaleable: false,
            forced_flushed: false,
            dma_downloaded: false,
            is_sparse: false,
        };
        let copies = full_download_copies(&info);
        let memory = vec![0; calculate_unswizzled_size_bytes(&info) as usize];
        let mut tmp = ScratchBuffer::new();

        swizzle_image(
            &|_, output| output.fill(0xa5),
            &writer,
            0x4000,
            &info,
            &copies,
            &memory,
            &mut tmp,
        );

        let written = written.lock().unwrap();
        assert_eq!(written.len(), 0x2000);
        assert_eq!(written[0], 0);
        assert_eq!(written[0x1fc8], 0xa5);
    }

    #[test]
    fn swizzle_image_uses_upstream_default_stride_alignment() {
        let written = Arc::new(Mutex::new(Vec::<u8>::new()));
        let written_for_callback = Arc::clone(&written);
        let writer = move |_addr: u64, bytes: &[u8]| {
            *written_for_callback.lock().unwrap() = bytes.to_vec();
        };
        let info = ImageInfo {
            format: PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: 70,
                height: 64,
                depth: 1,
            },
            tiling: TilingMode::BlockLinear(Extent3D {
                width: 0,
                height: 2,
                depth: 0,
            }),
            tile_width_spacing: 2,
            ..ImageInfo::default()
        };
        let copies = full_download_copies(&info);
        let memory = (0..copies[0].buffer_size)
            .map(|index| index.wrapping_mul(37) as u8)
            .collect::<Vec<_>>();
        let level_info = make_level_info_from_image(&info);
        let guest_size = calculate_level_size(&level_info, 0) as usize;
        let mut tmp = ScratchBuffer::new();

        swizzle_image(
            &|_, output| output.fill(0xa5),
            &writer,
            0x4000,
            &info,
            &copies,
            &memory,
            &mut tmp,
        );

        let num_tiles = adjust_tile_size_3d(info.size, default_block_size(info.format));
        let block = mip_block_size(&info, 0);
        let mut expected = vec![0xa5; guest_size];
        crate::textures::decoders::swizzle_texture(
            &mut expected,
            &memory,
            surface::bytes_per_block(info.format),
            num_tiles.width,
            num_tiles.height,
            num_tiles.depth,
            block.height,
            block.depth,
            1,
        );
        let mut non_upstream = vec![0xa5; guest_size];
        crate::textures::decoders::swizzle_texture(
            &mut non_upstream,
            &memory,
            surface::bytes_per_block(info.format),
            num_tiles.width,
            num_tiles.height,
            num_tiles.depth,
            block.height,
            block.depth,
            calculate_level_stride_alignment(&info, 0),
        );

        assert_eq!(*written.lock().unwrap(), expected);
        assert_ne!(expected, non_upstream);
    }

    #[test]
    fn swizzle_image_block_linear_reports_offset_rectangles_and_continues_like_upstream() {
        let writer = |_addr: u64, _bytes: &[u8]| {};
        let info = ImageInfo {
            format: PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: 16,
                height: 16,
                depth: 1,
            },
            tiling: TilingMode::BlockLinear(Extent3D {
                width: 0,
                height: 1,
                depth: 0,
            }),
            layer_stride: 0,
            maybe_unaligned_layer_stride: 0,
            num_samples: 1,
            tile_width_spacing: 0,
            rescaleable: false,
            downscaleable: false,
            forced_flushed: false,
            dma_downloaded: false,
            is_sparse: false,
        };
        let mut copy = full_download_copies(&info)[0];
        copy.image_offset.x = 1;
        let memory = vec![0x5a; copy.buffer_size];
        let mut tmp = ScratchBuffer::new();

        swizzle_image(
            &|_, output| output.fill(0),
            &writer,
            0x4000,
            &info,
            &[copy],
            &memory,
            &mut tmp,
        );
    }

    #[test]
    fn unswizzle_image_uses_tile_counts_for_compressed_blocks() {
        let info = ImageInfo {
            format: PixelFormat::Bc5Unorm,
            image_type: ImageType::E2D,
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: 8,
                height: 8,
                depth: 1,
            },
            tiling: TilingMode::BlockLinear(Extent3D {
                width: 0,
                height: 0,
                depth: 0,
            }),
            layer_stride: 0,
            maybe_unaligned_layer_stride: 0,
            num_samples: 1,
            tile_width_spacing: 0,
            rescaleable: false,
            downscaleable: false,
            forced_flushed: false,
            dma_downloaded: false,
            is_sparse: false,
        };
        let input: Vec<u8> = (0..64).map(|x| (x ^ 0x5a) as u8).collect();
        let mut expected = vec![0; 64];
        let params = full_upload_swizzles(&info);
        let alignment = calculate_level_stride_alignment(&info, 0);
        crate::textures::decoders::unswizzle_texture(
            &mut expected,
            &input,
            surface::bytes_per_block(info.format),
            params[0].num_tiles.width,
            params[0].num_tiles.height,
            params[0].num_tiles.depth,
            params[0].block.height,
            params[0].block.depth,
            alignment,
        );

        let mut output = vec![0; 64];
        let copies = unswizzle_image(&(), 0, &info, &input, &mut output);

        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0].buffer_size, 64);
        assert_eq!(
            params[0].num_tiles,
            Extent3D {
                width: 2,
                height: 2,
                depth: 1
            }
        );
        assert_eq!(output, expected);
        assert!(output[32..].iter().any(|&byte| byte != 0));
    }

    #[test]
    fn unswizzle_image_aligns_compressed_upload_rows_per_mip() {
        let info = ImageInfo {
            format: PixelFormat::Bc1RgbaSrgb,
            image_type: ImageType::E2D,
            resources: SubresourceExtent {
                levels: 4,
                layers: 1,
            },
            size: Extent3D {
                width: 8,
                height: 8,
                depth: 1,
            },
            tiling: TilingMode::BlockLinear(Extent3D {
                width: 0,
                height: 0,
                depth: 0,
            }),
            layer_stride: 0,
            maybe_unaligned_layer_stride: 0,
            num_samples: 1,
            tile_width_spacing: 0,
            rescaleable: false,
            downscaleable: false,
            forced_flushed: false,
            dma_downloaded: false,
            is_sparse: false,
        };
        let input = vec![0x5a; calculate_guest_size_in_bytes(&info) as usize];
        let mut output = vec![0; calculate_unswizzled_size_bytes(&info) as usize];
        let copies = unswizzle_image(&(), 0, &info, &input, &mut output);

        assert_eq!(copies.len(), 4);
        assert_eq!(copies[0].image_extent.width, 8);
        assert_eq!(copies[1].image_extent.width, 4);
        assert_eq!(copies[2].image_extent.width, 2);
        assert_eq!(copies[3].image_extent.width, 1);
        for copy in &copies {
            assert_eq!(copy.buffer_row_length % 4, 0);
            assert_eq!(copy.buffer_image_height % 4, 0);
        }
        assert_eq!(copies[2].buffer_row_length, 4);
        assert_eq!(copies[2].buffer_image_height, 4);
        assert_eq!(copies[3].buffer_row_length, 4);
        assert_eq!(copies[3].buffer_image_height, 4);
    }

    #[test]
    fn unswizzle_image_uses_guest_layer_stride_like_upstream() {
        let mut info = ImageInfo {
            format: PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            resources: SubresourceExtent {
                levels: 1,
                layers: 2,
            },
            size: Extent3D {
                width: 16,
                height: 16,
                depth: 1,
            },
            tiling: TilingMode::BlockLinear(Extent3D {
                width: 0,
                height: 1,
                depth: 0,
            }),
            layer_stride: 0,
            maybe_unaligned_layer_stride: 0,
            num_samples: 1,
            tile_width_spacing: 0,
            rescaleable: false,
            downscaleable: false,
            forced_flushed: false,
            dma_downloaded: false,
            is_sparse: false,
        };
        let level_info = make_level_info_from_image(&info);
        let tile_size = default_block_size(info.format);
        let level_sizes = calculate_level_sizes(&level_info, info.resources.levels as u32);
        let layer_stride = align_layer_size(
            calculate_level_bytes(&level_sizes, info.resources.levels as u32),
            info.size,
            level_info.block,
            tile_size.height,
            info.tile_width_spacing,
        );
        info.layer_stride = layer_stride;

        let bytes_per_block = surface::bytes_per_block(info.format);
        let host_bytes_per_layer = info.size.width * info.size.height * bytes_per_block;
        let mut input = vec![0; (layer_stride * info.resources.layers as u32) as usize];
        input[..level_sizes[0] as usize].fill(0x11);
        input[layer_stride as usize..layer_stride as usize + level_sizes[0] as usize].fill(0x22);
        let mut output = vec![0; calculate_unswizzled_size_bytes(&info) as usize];

        let copies = unswizzle_image(&(), 0, &info, &input, &mut output);

        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0].buffer_size, (host_bytes_per_layer * 2) as usize);
        assert!(output[..host_bytes_per_layer as usize]
            .iter()
            .all(|&byte| byte == 0x11));
        assert!(
            output[host_bytes_per_layer as usize..(host_bytes_per_layer * 2) as usize]
                .iter()
                .all(|&byte| byte == 0x22)
        );
    }
}
