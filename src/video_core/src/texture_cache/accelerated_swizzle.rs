// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/texture_cache/accelerated_swizzle.h and accelerated_swizzle.cpp
//!
//! GPU-accelerated block-linear swizzle parameter generation for 2D and 3D
//! textures.

use common::alignment::align_up_log2;
use common::div_ceil::div_ceil_log2_u32;

use crate::surface::bytes_per_block;
use crate::textures::decoders::{GOB_SIZE_SHIFT, GOB_SIZE_X, GOB_SIZE_X_SHIFT, GOB_SIZE_Y_SHIFT};

use super::image_info::ImageInfo;
use super::types::*;

// ── Parameter structs ──────────────────────────────────────────────────

/// Parameters for a 2D block-linear swizzle compute dispatch.
///
/// Port of `VideoCommon::Accelerated::BlockLinearSwizzle2DParams`.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C, align(16))]
pub struct BlockLinearSwizzle2DParams {
    pub origin: [u32; 3],
    pub _pad0: u32,
    pub destination: [i32; 3],
    pub _pad1: i32,
    pub bytes_per_block_log2: u32,
    pub layer_stride: u32,
    pub block_size: u32,
    pub x_shift: u32,
    pub block_height: u32,
    pub block_height_mask: u32,
}

/// Parameters for a 3D block-linear swizzle compute dispatch.
///
/// Port of `VideoCommon::Accelerated::BlockLinearSwizzle3DParams`.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct BlockLinearSwizzle3DParams {
    pub origin: [u32; 3],
    pub destination: [i32; 3],
    pub bytes_per_block_log2: u32,
    pub slice_size: u32,
    pub block_size: u32,
    pub x_shift: u32,
    pub block_height: u32,
    pub block_height_mask: u32,
    pub block_depth: u32,
    pub block_depth_mask: u32,
}

// ── Public functions ───────────────────────────────────────────────────

/// Build parameters for a 2D block-linear swizzle.
///
/// Port of `VideoCommon::Accelerated::MakeBlockLinearSwizzle2DParams`.
pub fn make_block_linear_swizzle_2d_params(
    swizzle: &SwizzleParameters,
    info: &ImageInfo,
) -> BlockLinearSwizzle2DParams {
    let block = swizzle.block;
    let num_tiles = swizzle.num_tiles;
    let bytes_per_block = bytes_per_block(info.format);
    let stride_alignment =
        super::util::calculate_level_stride_alignment(info, swizzle.level as u32);
    let stride = align_up_log2(num_tiles.width.into(), stride_alignment) as u32 * bytes_per_block;
    let gobs_in_x = div_ceil_log2_u32(stride, GOB_SIZE_X_SHIFT);
    BlockLinearSwizzle2DParams {
        origin: [0, 0, 0],
        _pad0: 0,
        destination: [0, 0, 0],
        _pad1: 0,
        bytes_per_block_log2: bytes_per_block.trailing_zeros(),
        layer_stride: info.layer_stride,
        block_size: gobs_in_x << (GOB_SIZE_SHIFT + block.height + block.depth),
        x_shift: GOB_SIZE_SHIFT + block.height + block.depth,
        block_height: block.height,
        block_height_mask: (1u32 << block.height) - 1,
    }
}

/// Build parameters for a 3D block-linear swizzle.
///
/// Port of `VideoCommon::Accelerated::MakeBlockLinearSwizzle3DParams`.
pub fn make_block_linear_swizzle_3d_params(
    swizzle: &SwizzleParameters,
    info: &ImageInfo,
) -> BlockLinearSwizzle3DParams {
    let block = swizzle.block;
    let num_tiles = swizzle.num_tiles;
    let bytes_per_block = bytes_per_block(info.format);
    let stride_alignment =
        super::util::calculate_level_stride_alignment(info, swizzle.level as u32);
    let stride = align_up_log2(num_tiles.width.into(), stride_alignment) as u32 * bytes_per_block;

    let gobs_in_x = (stride + GOB_SIZE_X - 1) >> GOB_SIZE_X_SHIFT;
    let block_size = gobs_in_x << (GOB_SIZE_SHIFT + block.height + block.depth);
    let slice_size =
        div_ceil_log2_u32(num_tiles.height, block.height + GOB_SIZE_Y_SHIFT) * block_size;

    BlockLinearSwizzle3DParams {
        origin: [0, 0, 0],
        destination: [0, 0, 0],
        bytes_per_block_log2: bytes_per_block.trailing_zeros(),
        slice_size,
        block_size,
        x_shift: GOB_SIZE_SHIFT + block.height + block.depth,
        block_height: block.height,
        block_height_mask: (1u32 << block.height) - 1,
        block_depth: block.depth,
        block_depth_mask: (1u32 << block.depth) - 1,
    }
}

#[cfg(test)]
mod tests {
    use std::mem::{align_of, offset_of, size_of};

    use super::*;
    use crate::surface::PixelFormat;
    use crate::texture_cache::image_info::TilingMode;

    fn image_info(block: Extent3D, size: Extent3D, layer_stride: u32) -> ImageInfo {
        ImageInfo {
            format: PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size,
            tiling: TilingMode::BlockLinear(block),
            layer_stride,
            ..ImageInfo::default()
        }
    }

    #[test]
    fn block_linear_parameter_layouts_match_upstream() {
        assert_eq!(size_of::<BlockLinearSwizzle2DParams>(), 64);
        assert_eq!(align_of::<BlockLinearSwizzle2DParams>(), 16);
        assert_eq!(offset_of!(BlockLinearSwizzle2DParams, origin), 0);
        assert_eq!(offset_of!(BlockLinearSwizzle2DParams, destination), 16);
        assert_eq!(
            offset_of!(BlockLinearSwizzle2DParams, bytes_per_block_log2),
            32
        );
        assert_eq!(offset_of!(BlockLinearSwizzle2DParams, layer_stride), 36);
        assert_eq!(offset_of!(BlockLinearSwizzle2DParams, block_size), 40);
        assert_eq!(offset_of!(BlockLinearSwizzle2DParams, x_shift), 44);
        assert_eq!(offset_of!(BlockLinearSwizzle2DParams, block_height), 48);
        assert_eq!(
            offset_of!(BlockLinearSwizzle2DParams, block_height_mask),
            52
        );

        assert_eq!(size_of::<BlockLinearSwizzle3DParams>(), 56);
        assert_eq!(align_of::<BlockLinearSwizzle3DParams>(), 4);
        assert_eq!(offset_of!(BlockLinearSwizzle3DParams, origin), 0);
        assert_eq!(offset_of!(BlockLinearSwizzle3DParams, destination), 12);
        assert_eq!(
            offset_of!(BlockLinearSwizzle3DParams, bytes_per_block_log2),
            24
        );
        assert_eq!(offset_of!(BlockLinearSwizzle3DParams, slice_size), 28);
        assert_eq!(offset_of!(BlockLinearSwizzle3DParams, block_size), 32);
        assert_eq!(offset_of!(BlockLinearSwizzle3DParams, x_shift), 36);
        assert_eq!(offset_of!(BlockLinearSwizzle3DParams, block_height), 40);
        assert_eq!(
            offset_of!(BlockLinearSwizzle3DParams, block_height_mask),
            44
        );
        assert_eq!(offset_of!(BlockLinearSwizzle3DParams, block_depth), 48);
        assert_eq!(offset_of!(BlockLinearSwizzle3DParams, block_depth_mask), 52);
    }

    #[test]
    fn two_dimensional_parameters_match_upstream_formula() {
        let block = Extent3D {
            width: 0,
            height: 1,
            depth: 0,
        };
        let num_tiles = Extent3D {
            width: 64,
            height: 32,
            depth: 1,
        };
        let info = image_info(block, num_tiles, 0x20_000);
        let params = make_block_linear_swizzle_2d_params(
            &SwizzleParameters {
                num_tiles,
                block,
                buffer_offset: 0,
                level: 0,
            },
            &info,
        );

        assert_eq!(params.origin, [0, 0, 0]);
        assert_eq!(params.destination, [0, 0, 0]);
        assert_eq!(params.bytes_per_block_log2, 2);
        assert_eq!(params.layer_stride, 0x20_000);
        assert_eq!(params.block_size, 4096);
        assert_eq!(params.x_shift, 10);
        assert_eq!(params.block_height, 1);
        assert_eq!(params.block_height_mask, 1);
    }

    #[test]
    fn three_dimensional_parameters_match_upstream_formula() {
        let block = Extent3D {
            width: 0,
            height: 1,
            depth: 1,
        };
        let num_tiles = Extent3D {
            width: 64,
            height: 32,
            depth: 4,
        };
        let mut info = image_info(block, num_tiles, 0);
        info.image_type = ImageType::E3D;
        let params = make_block_linear_swizzle_3d_params(
            &SwizzleParameters {
                num_tiles,
                block,
                buffer_offset: 0,
                level: 0,
            },
            &info,
        );

        assert_eq!(params.origin, [0, 0, 0]);
        assert_eq!(params.destination, [0, 0, 0]);
        assert_eq!(params.bytes_per_block_log2, 2);
        assert_eq!(params.slice_size, 16_384);
        assert_eq!(params.block_size, 8192);
        assert_eq!(params.x_shift, 11);
        assert_eq!(params.block_height, 1);
        assert_eq!(params.block_height_mask, 1);
        assert_eq!(params.block_depth, 1);
        assert_eq!(params.block_depth_mask, 1);
    }
}
