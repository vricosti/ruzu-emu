// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/texture_cache/util.h and util.cpp
//!
//! Utility functions for the texture cache: size/offset calculations,
//! swizzle/unswizzle, copy generation, subresource lookup, and more.
//!
//! util.cpp is ~1 500 lines of dense GPU-texture math.  Method signatures
//! and constant definitions are ported in full; complex bodies are stubbed
//! with `todo!()` and will be filled as dependent types are completed.

use crate::textures::texture::TicEntry;

use super::format_lookup_table::{
    pixel_format_from_texture_info, ComponentType, PixelFormat, TextureFormat,
};
use super::image_base::{GPUVAddr, ImageBase, VAddr};
use super::image_info::ImageInfo;
use super::types::*;

use crate::surface;
use crate::textures::decoders::{
    GOB_SIZE_SHIFT, GOB_SIZE_X, GOB_SIZE_X_SHIFT, GOB_SIZE_Y, GOB_SIZE_Y_SHIFT, GOB_SIZE_Z,
    GOB_SIZE_Z_SHIFT,
};

// ── Alignment helpers ─────────────────────────────────────────────────

fn align_up_log2(value: u32, alignment_log2: u32) -> u32 {
    let mask = (1u32 << alignment_log2) - 1;
    (value + mask) & !mask
}

fn div_ceil(a: u32, b: u32) -> u32 {
    (a + b - 1) / b
}

fn div_ceil_log2(value: u32, shift: u32) -> u32 {
    let mask = (1u32 << shift) - 1;
    (value + mask) >> shift
}

fn align_up(value: u32, alignment: u32) -> u32 {
    if alignment == 0 {
        return value;
    }
    let mask = alignment - 1;
    (value + mask) & !mask
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
    let mut x = unit_factor << (s - 1);
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
        while block_size > 0 && num_tiles <= (1u32 << (block_size - 1)) * gob_extent {
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
    debug_assert!(info.image_type == ImageType::Linear);
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
    debug_assert!(info.image_type != ImageType::Linear);
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
    let mut num_blocks = 0;
    for level in 0..info.resources.levels as u32 {
        let mip_size = adjust_mip_size_3d(info.size, level);
        num_blocks += div_ceil(mip_size.width, tile_size.width)
            * div_ceil(mip_size.height, tile_size.height)
            * mip_size.depth;
    }
    num_blocks
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
        width: div_ceil(
            adjust_mip_size(info.size.width, level),
            info.tile_size.width,
        ) << info.bpp_log2,
        height: div_ceil(
            adjust_mip_size(info.size.height, level),
            info.tile_size.height,
        ),
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
        width: GOB_SIZE_X_SHIFT - bpp_log2 + tile_width_spacing,
        height: GOB_SIZE_Y_SHIFT + block_height,
    }
}

/// Port of `IsSmallerThanGobSize`.
fn is_smaller_than_gob_size(num_tiles: Extent3D, gob: Extent2D, block_depth: u32) -> bool {
    num_tiles.width <= (1u32 << gob.width)
        || num_tiles.height <= (1u32 << gob.height)
        || num_tiles.depth < (1u32 << block_depth)
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
    let num_tiles = tiles.width * tiles.height * tiles.depth;
    let shift = GOB_SIZE_SHIFT + ts.width + ts.height + ts.depth;
    num_tiles << shift
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
    sizes[..num_levels as usize].iter().sum()
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
        info.resources.levels as u32 as u32,
    )
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
    while block.height != 0 && aligned_height <= (1u32 << (block.height - 1)) * GOB_SIZE_Y {
        block.height -= 1;
    }
    while block.depth != 0 && size.depth <= (1u32 << (block.depth - 1)) {
        block.depth -= 1;
    }
    let block_shift = GOB_SIZE_SHIFT + block.height + block.depth;
    let num_blocks = size_bytes >> block_shift;
    if size_bytes != num_blocks << block_shift {
        (num_blocks + 1) << block_shift
    } else {
        size_bytes
    }
}

// ── Size / offset calculation ─────────────────────────────────────────

/// Port of `CalculateGuestSizeInBytes`.
pub fn calculate_guest_size_in_bytes(info: &ImageInfo) -> u32 {
    if info.image_type == ImageType::Buffer {
        return surface::bytes_per_block(info.format) * info.size.width;
    }
    if info.image_type == ImageType::Linear {
        return info.pitch()
            * div_ceil(info.size.height, surface::default_block_height(info.format));
    }
    if info.resources.layers > 1 {
        assert_ne!(
            info.layer_stride, 0,
            "CalculateGuestSizeInBytes requires layer_stride for layered images"
        );
        return info.layer_stride * info.resources.layers as u32;
    }
    calculate_layer_size(info)
}

/// Port of `CalculateUnswizzledSizeBytes`.
pub fn calculate_unswizzled_size_bytes(info: &ImageInfo) -> u32 {
    if info.image_type == ImageType::Buffer {
        return surface::bytes_per_block(info.format) * info.size.width;
    }
    if info.image_type == ImageType::Linear {
        return info.pitch()
            * div_ceil(info.size.height, surface::default_block_height(info.format));
    }
    let tile_size = default_block_size(info.format);
    num_blocks_per_layer(info, tile_size)
        * info.resources.layers as u32
        * surface::bytes_per_block(info.format)
}

/// Port of `CalculateConvertedSizeBytes`.
pub fn calculate_converted_size_bytes(info: &ImageInfo) -> u32 {
    if info.image_type == ImageType::Buffer {
        return surface::bytes_per_block(info.format) * info.size.width;
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
        let mut output_size = 0;
        for level in 0..info.resources.levels as u32 {
            let mip_size = adjust_mip_size_3d(info.size, level);
            let plane_dim = align_up(mip_size.width, 4) * align_up(mip_size.height, 4);
            output_size += (plane_dim * info.size.depth * info.resources.layers as u32) / bpp_div;
        }
        return output_size;
    }

    num_blocks_per_layer(
        info,
        Extent2D {
            width: 1,
            height: 1,
        },
    ) * info.resources.layers as u32
        * crate::texture_cache::decode_bc::converted_bytes_per_block(info.format)
}

/// Port of `CalculateLayerStride`.
pub fn calculate_layer_stride(info: &ImageInfo) -> u32 {
    if info.image_type == ImageType::Linear {
        return info.pitch() * info.size.height;
    }
    let level_info = make_level_info_from_image(info);
    let sizes = calculate_level_sizes(&level_info, info.resources.levels as u32);
    let level_bytes = calculate_level_bytes(&sizes, info.resources.levels as u32);
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
    let level_info = make_level_info_from_image(info);
    let sizes = calculate_level_sizes(&level_info, info.resources.levels as u32);
    calculate_level_bytes(&sizes, info.resources.levels as u32)
}

/// Port of `CalculateMipLevelOffsets`.
pub fn calculate_mip_level_offsets(info: &ImageInfo) -> LevelArray {
    if info.image_type == ImageType::Linear {
        return [0u32; MAX_MIP_LEVELS];
    }
    let level_info = make_level_info_from_image(info);
    let sizes = calculate_level_sizes(&level_info, info.resources.levels as u32);
    let mut offsets = [0u32; MAX_MIP_LEVELS];
    let mut offset = 0u32;
    for level in 0..(info.resources.levels as u32 as usize) {
        offsets[level] = offset;
        offset += sizes[level];
    }
    offsets
}

/// Port of `CalculateMipLevelSizes`.
pub fn calculate_mip_level_sizes(info: &ImageInfo) -> LevelArray {
    if info.image_type == ImageType::Linear {
        let mut sizes = [0u32; MAX_MIP_LEVELS];
        sizes[0] = info.pitch() * info.size.height;
        return sizes;
    }
    let level_info = make_level_info_from_image(info);
    calculate_level_sizes(&level_info, info.resources.levels as u32)
}

/// Port of `CalculateSliceOffsets`.
pub fn calculate_slice_offsets(info: &ImageInfo) -> Vec<u32> {
    debug_assert!(info.image_type == ImageType::E3D);
    let level_info = make_level_info_from_image(info);
    let mut offsets = Vec::new();
    let mut mip_offset = 0u32;
    for level in 0..info.resources.levels as u32 {
        let ts = tile_shift(&level_info, level);
        let tiles = level_tiles(&level_info, level);
        let gob_size_shift = ts.height + GOB_SIZE_SHIFT;
        let slice_size = (tiles.width * tiles.height) << gob_size_shift;
        let z_mask = (1u32 << ts.depth) - 1;
        let depth = adjust_mip_size(info.size.depth, level);
        for slice in 0..depth {
            let z_low = slice & z_mask;
            let z_high = slice & !z_mask;
            offsets.push(mip_offset + (z_low << gob_size_shift) + (z_high * slice_size));
        }
        mip_offset += calculate_level_size(&level_info, level);
    }
    offsets
}

/// Port of `CalculateSliceSubresources`.
pub fn calculate_slice_subresources(info: &ImageInfo) -> Vec<SubresourceBase> {
    debug_assert!(info.image_type == ImageType::E3D);
    let mut subresources = Vec::new();
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
    if info.image_type == ImageType::Linear {
        return 0;
    }
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
    let Some(format) = TextureFormat::from_raw(config.format()) else {
        return PixelFormat::Invalid;
    };
    let Some(red) = ComponentType::from_raw(config.r_type()) else {
        return PixelFormat::Invalid;
    };
    let Some(green) = ComponentType::from_raw(config.g_type()) else {
        return PixelFormat::Invalid;
    };
    let Some(blue) = ComponentType::from_raw(config.b_type()) else {
        return PixelFormat::Invalid;
    };
    let Some(alpha) = ComponentType::from_raw(config.a_type()) else {
        return PixelFormat::Invalid;
    };

    pixel_format_from_texture_info(
        format,
        red,
        green,
        blue,
        alpha,
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
            log::error!("Unimplemented image type {:?}", info.image_type);
            ImageViewType::E2D
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
) -> Vec<ImageCopy> {
    debug_assert!(dst.resources.levels >= src.resources.levels);
    let is_dst_3d = dst.image_type == ImageType::E3D;
    if is_dst_3d {
        debug_assert!(src.image_type == ImageType::E3D);
        debug_assert!(src.resources.levels == 1);
    }
    let both_2d = src.image_type == ImageType::E2D && dst.image_type == ImageType::E2D;
    let mut copies = Vec::with_capacity(src.resources.levels as usize);
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
        extent.width = ((extent.width * up_scale) >> down_shift).max(1);
        if both_2d {
            extent.height = ((extent.height * up_scale) >> down_shift).max(1);
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
) -> Vec<ImageCopy> {
    let is_3d = src.image_type == ImageType::E3D;
    let mut copies = Vec::with_capacity(src.resources.levels as usize);
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
        extent.width = ((extent.width * up_scale) >> down_shift).max(1);
        extent.height = ((extent.height * up_scale) >> down_shift).max(1);
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

pub fn is_valid_entry_with_addr_valid(
    config: &crate::textures::texture::TicEntry,
    mut addr_valid: impl FnMut(GPUVAddr) -> bool,
) -> bool {
    is_valid_entry_with_range_valid(config, |address, _size| addr_valid(address))
}

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
) -> Vec<BufferImageCopy> {
    let bytes_per_block = surface::bytes_per_block(info.format);
    if bytes_per_block == 0 {
        return Vec::new();
    }
    let tile_size = default_block_size(info.format);

    if info.image_type == ImageType::Linear {
        let copy_size = input.len().min(output.len());
        output[..copy_size].copy_from_slice(&input[..copy_size]);
        return vec![BufferImageCopy {
            buffer_offset: 0,
            buffer_size: input.len(),
            buffer_row_length: info.pitch() * tile_size.width / bytes_per_block,
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
    let mut copies = Vec::with_capacity(num_levels as usize);

    for level in 0..num_levels {
        let level_size = adjust_mip_size_3d(info.size, level as u32);
        let num_tiles = adjust_tile_size_3d(level_size, tile_size);
        let num_blocks_per_layer = num_tiles.width * num_tiles.height * num_tiles.depth;
        let host_bytes_per_layer = num_blocks_per_layer << bpp_log2;
        copies.push(BufferImageCopy {
            buffer_offset: host_offset as usize,
            buffer_size: (host_bytes_per_layer * num_layers as u32) as usize,
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
            let src_offset = guest_offset.saturating_add(guest_layer_offset);
            if dst_offset >= output.len() || src_offset >= input.len() {
                break;
            }
            let dst_size = (host_bytes_per_layer as usize).min(output.len() - dst_offset);
            let dst = &mut output[dst_offset..dst_offset + dst_size];
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
            guest_layer_offset = guest_layer_offset.saturating_add(layer_stride as usize);
            host_offset = host_offset.saturating_add(host_bytes_per_layer);
        }
        guest_offset = guest_offset.saturating_add(level_sizes[level as usize] as usize);
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
    let tile_size = default_block_size(info.format);

    for copy in copies.iter_mut() {
        let level = copy.image_subresource.base_level;
        let _mip_size = adjust_mip_size_3d(info.size, level as u32);

        let input_offset = copy.buffer_offset;
        copy.buffer_offset = output_offset as usize;

        let astc = surface::is_pixel_format_astc(
            // SAFETY: info.format is a valid PixelFormat discriminant from the texture cache
            unsafe { std::mem::transmute::<u32, surface::PixelFormat>(info.format as u32) },
        );

        let recompression = *common::settings::values().astc_recompression.get_value();

        if astc && recompression == common::settings_enums::AstcRecompression::Uncompressed {
            let input_slice = &input[input_offset..];
            let depth_layers = copy.image_subresource.num_layers as u32 * copy.image_extent.depth;
            crate::textures::astc::decompress(
                input_slice,
                copy.image_extent.width,
                copy.image_extent.height,
                depth_layers,
                tile_size.width,
                tile_size.height,
                &mut output[output_offset as usize..],
            );

            output_offset += copy.image_extent.width
                * copy.image_extent.height
                * copy.image_subresource.num_layers as u32
                * surface::bytes_per_block(surface::PixelFormat::A8B8G8R8Unorm);
        } else if astc {
            let bpp_div = if recompression == common::settings_enums::AstcRecompression::Bc1 {
                2
            } else {
                1
            };
            let plane_dim = copy.image_extent.width * copy.image_extent.height;
            let depth_layers = copy.image_subresource.num_layers as u32 * copy.image_extent.depth;
            let level_size = plane_dim
                * copy.image_extent.depth
                * copy.image_subresource.num_layers as u32
                * surface::bytes_per_block(surface::PixelFormat::A8B8G8R8Unorm);
            let mut decode_scratch = vec![0; level_size as usize];

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

            let aligned_plane_dim =
                align_up(copy.image_extent.width, 4) * align_up(copy.image_extent.height, 4);
            copy.buffer_size = ((aligned_plane_dim
                * copy.image_extent.depth
                * copy.image_subresource.num_layers as u32)
                / bpp_div) as usize;
            output_offset += copy.buffer_size as u32;
        } else {
            crate::texture_cache::decode_bc::decompress_bcn(
                &input[input_offset..],
                &mut output[output_offset as usize..],
                copy,
                info.format,
            );
            let bytes = copy.image_extent.width
                * copy.image_extent.height
                * copy.image_subresource.num_layers as u32
                * crate::texture_cache::decode_bc::converted_bytes_per_block(info.format);
            output_offset += bytes;
        }

        copy.buffer_row_length = _mip_size.width;
        copy.buffer_image_height = _mip_size.height;
    }
}

/// Port of `FullDownloadCopies`.
pub fn full_download_copies(info: &ImageInfo) -> Vec<BufferImageCopy> {
    let size = info.size;
    let bpb = surface::bytes_per_block(info.format);
    if info.image_type == ImageType::Linear {
        debug_assert!(info.pitch() % bpb == 0);
        return vec![BufferImageCopy {
            buffer_offset: 0,
            buffer_size: (info.pitch() * size.height) as usize,
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
        log::error!(
            "FullDownloadCopies: tile_width_spacing={} is unimplemented",
            info.tile_width_spacing
        );
    }
    let num_layers = info.resources.layers;
    let num_levels = info.resources.levels;
    let tile_size = default_block_size(info.format);
    let mut host_offset = 0u32;
    let mut copies = Vec::with_capacity(num_levels as usize);
    for level in 0..num_levels {
        let level_size = adjust_mip_size_3d(size, level as u32);
        let adj = adjust_tile_size_3d(level_size, tile_size);
        let num_blocks_per_layer = adj.width * adj.height * adj.depth;
        let host_bytes_per_level = num_blocks_per_layer * bpb * num_layers as u32;
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
        host_offset += host_bytes_per_level;
    }
    copies
}

/// Port of `FullUploadSwizzles`.
pub fn full_upload_swizzles(info: &ImageInfo) -> Vec<SwizzleParameters> {
    let tile_size = default_block_size(info.format);
    if info.image_type == ImageType::Linear {
        return vec![SwizzleParameters {
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
    let mut params = Vec::with_capacity(num_levels as usize);
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
        guest_offset += calculate_level_size(&level_info, level as u32);
    }
    params
}

/// Port of `SwizzleImage`.
///
/// Upstream reads and writes through `Tegra::MemoryManager& gpu_memory`.
/// The paired callbacks preserve the same `UnsafeReadWrite` contract when a
/// block-linear subresource has padding that must survive the writeback.
pub fn swizzle_image(
    guest_memory_reader: &dyn Fn(u64, &mut [u8]),
    guest_memory_writer: &dyn Fn(u64, &[u8]),
    gpu_addr: VAddr,
    info: &ImageInfo,
    copies: &[BufferImageCopy],
    memory: &[u8],
    tmp_buffer: &mut Vec<u8>,
) {
    let bytes_per_block = surface::bytes_per_block(info.format);
    if bytes_per_block == 0 {
        return;
    }

    for copy in copies {
        if info.image_type == ImageType::Linear {
            let pitch = info.pitch();
            if pitch == 0 {
                continue;
            }
            assert_eq!(copy.image_offset.z, 0);
            assert_eq!(copy.image_extent.depth, 1);
            assert_eq!(copy.image_subresource.base_level, 0);
            assert_eq!(copy.image_subresource.base_layer, 0);
            assert_eq!(copy.image_subresource.num_layers, 1);

            let row_length = copy.image_extent.width.saturating_mul(bytes_per_block) as usize;
            let guest_offset_x = (copy.image_offset.x as u32).wrapping_mul(bytes_per_block) as u64;
            for line in 0..copy.image_extent.height {
                let host_offset = copy.buffer_offset + line as usize * pitch as usize;
                let host_end = host_offset.saturating_add(row_length);
                if host_end > memory.len() {
                    break;
                }
                let guest_offset_y = (copy.image_offset.y as u32)
                    .wrapping_add(line)
                    .wrapping_mul(pitch) as u64;
                let guest_offset = guest_offset_x + guest_offset_y;
                guest_memory_writer(gpu_addr + guest_offset, &memory[host_offset..host_end]);
            }
            continue;
        }

        let level = copy.image_subresource.base_level.max(0) as u32;
        let level_info = make_level_info_from_image(info);
        let tile_size = default_block_size(info.format);
        let level_size = adjust_mip_size_3d(info.size, level);

        assert_eq!(
            copy.image_offset,
            (Offset3D { x: 0, y: 0, z: 0 }),
            "Unimplemented code!"
        );
        assert_eq!(copy.image_extent, level_size, "Unimplemented code!");

        let num_tiles = adjust_tile_size_3d(level_size, tile_size);
        let num_blocks_per_layer = num_tiles.width * num_tiles.height * num_tiles.depth;
        let host_bytes_per_layer = num_blocks_per_layer * bytes_per_block;
        if host_bytes_per_layer == 0 {
            continue;
        }

        let block =
            adjust_mip_block_size_3d(num_tiles, level_info.block, level, level_info.num_levels);
        let num_levels = info.resources.levels as u32;
        let sizes = calculate_level_sizes(&level_info, num_levels);
        let mut guest_offset = calculate_level_bytes(&sizes, level) as u64;
        let layer_stride = align_layer_size(
            calculate_level_bytes(&sizes, num_levels),
            info.size,
            level_info.block,
            tile_size.height,
            info.tile_width_spacing,
        ) as u64;
        let subresource_size = sizes[level as usize] as usize;
        if subresource_size == 0 {
            continue;
        }

        let mut host_offset = copy.buffer_offset;
        for _layer in 0..info.resources.layers.max(0) as u32 {
            let src_end = host_offset.saturating_add(host_bytes_per_layer as usize);
            if src_end > memory.len() {
                break;
            }

            tmp_buffer.clear();
            tmp_buffer.resize(subresource_size, 0);
            guest_memory_reader(gpu_addr + guest_offset, tmp_buffer);
            crate::textures::decoders::swizzle_texture(
                tmp_buffer,
                &memory[host_offset..src_end],
                bytes_per_block,
                num_tiles.width,
                num_tiles.height,
                num_tiles.depth,
                block.height,
                block.depth,
                calculate_level_stride_alignment(info, level),
            );

            guest_memory_writer(gpu_addr + guest_offset, tmp_buffer);
            host_offset += host_bytes_per_layer as usize;
            guest_offset += layer_stride;
        }
        debug_assert_eq!(
            host_offset.saturating_sub(copy.buffer_offset),
            copy.buffer_size
        );
    }
}

// ── Mip helpers ────────────────────────────────────────────────────────

/// Compute the size at a given mip level.
///
/// Port of `MipSize`.
pub fn mip_size(size: Extent3D, level: u32) -> Extent3D {
    Extent3D {
        width: (size.width >> level).max(1),
        height: (size.height >> level).max(1),
        depth: (size.depth >> level).max(1),
    }
}

/// Compute the block size at a given mip level.
///
/// Port of `MipBlockSize`.
pub fn mip_block_size(info: &ImageInfo, level: u32) -> Extent3D {
    let level_info = make_level_info_from_image(info);
    let tile_size = default_block_size(info.format);
    let level_size = Extent3D {
        width: adjust_mip_size(info.size.width, level),
        height: adjust_mip_size(info.size.height, level),
        depth: adjust_mip_size(info.size.depth, level),
    };
    let num_tiles = Extent3D {
        width: div_ceil(level_size.width, tile_size.width),
        height: div_ceil(level_size.height, tile_size.height),
        depth: level_size.depth,
    };
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
    debug_assert!(lhs.image_type != ImageType::Linear);
    debug_assert!(rhs.image_type != ImageType::Linear);
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
    debug_assert!(lhs.image_type == ImageType::Linear);
    debug_assert!(rhs.image_type == ImageType::Linear);
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
    debug_assert!(lhs.image_type != ImageType::Linear);
    debug_assert!(rhs.image_type != ImageType::Linear);
    let lhs_bpp = surface::bytes_per_block(lhs.format);
    let rhs_bpp = surface::bytes_per_block(rhs.format);
    let lhs_size = adjust_mip_size_3d(lhs.size, lhs_level);
    let rhs_size = adjust_mip_size_3d(rhs.size, rhs_level);
    align_up_log2(lhs_size.width * lhs_bpp, GOB_SIZE_X_SHIFT)
        == align_up_log2(rhs_size.width * rhs_bpp, GOB_SIZE_X_SHIFT)
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
    if mip_depth < info.size.depth + base.layer as u32 {
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
    let new_size = layer_stride * new_info.resources.layers as u64;
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
    let layers = if info.image_type != ImageType::E3D {
        resources.layers.max(info.resources.layers + base.layer)
    } else {
        1
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
    debug_assert!(new_info.image_type != ImageType::Linear);
    debug_assert!(overlap.info.image_type != ImageType::Linear);
    if !is_layer_stride_compatible(new_info, &overlap.info) {
        return None;
    }
    if !surface::is_view_compatible(
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
        if !surface::is_view_compatible(existing.format, candidate.format, broken_views, native_bgr)
        {
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
        let mip_depth = 1u32.max(existing.size.depth << base.level as u32);
        if mip_depth < candidate.size.depth + base.layer as u32 {
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
        let mip_depth = 1u32.max(existing.size.depth << base.level as u32);
        if mip_depth < candidate.size.depth + base.layer as u32 {
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
    fn right_address_3d_overlap_uses_mip_reduced_depth() {
        let gpu_addr = 0x5000_0000;
        let new_info = ImageInfo {
            format: PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E3D,
            resources: SubresourceExtent {
                levels: 3,
                layers: 1,
            },
            size: Extent3D {
                width: 64,
                height: 64,
                depth: 8,
            },
            tiling: TilingMode::BlockLinear(Extent3D {
                width: 0,
                height: 2,
                depth: 2,
            }),
            ..ImageInfo::default()
        };
        let slice_offsets = calculate_slice_offsets(&new_info);
        let subresources = calculate_slice_subresources(&new_info);
        let level_one_slice_zero = subresources
            .iter()
            .position(|base| base.level == 1 && base.layer == 0)
            .expect("3D mip one must have a first slice");
        let overlap_info = ImageInfo {
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: 32,
                height: 32,
                depth: 5,
            },
            tiling: TilingMode::BlockLinear(mip_block_size(&new_info, 1)),
            ..new_info.clone()
        };
        let overlap = ImageBase::new(
            overlap_info,
            gpu_addr + u64::from(slice_offsets[level_one_slice_zero]),
            0x9000_0000,
        );

        assert_eq!(
            resolve_overlap_right_address_3d(&new_info, gpu_addr, &overlap, true),
            None,
        );
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
    fn is_sub_copy_3d_uses_upstream_mip_depth_expansion() {
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
        let candidate = ImageInfo {
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

        assert!(is_sub_copy(&candidate, &image, candidate_addr));
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
        let mut tmp = Vec::new();

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
        let mut tmp = Vec::new();

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
        let mut tmp = Vec::new();

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
        let mut tmp = Vec::new();

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
    #[should_panic(expected = "Unimplemented code!")]
    fn swizzle_image_block_linear_rejects_offset_rectangles_like_upstream() {
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
        let mut tmp = Vec::new();

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
