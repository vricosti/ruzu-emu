// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden `video_core/renderer_opengl/util_shaders.{h,cpp}`.
//!
//! Utility compute shaders for texture swizzling, ASTC decoding, format conversion, and MSAA.

use super::gl_resource_manager::OGLProgram;
use super::gl_staging_buffer_pool::StagingBufferMap;
use super::gl_texture_cache::Image;
use crate::host_shaders::compute_shaders::{
    ASTC_DECODER_COMP, BLOCK_LINEAR_UNSWIZZLE_2D_COMP, BLOCK_LINEAR_UNSWIZZLE_3D_COMP,
    CONVERT_MSAA_TO_NON_MSAA_COMP, CONVERT_NON_MSAA_TO_MSAA_COMP, OPENGL_CONVERT_S8D24_COMP,
    OPENGL_COPY_BC4_COMP, PITCH_UNSWIZZLE_COMP,
};
use crate::renderer_opengl::gl_shader_manager::ProgramManagerHandle;
use crate::renderer_opengl::gl_shader_util::create_program_from_source;
use crate::surface::{bytes_per_block, default_block_height, default_block_width};
use crate::texture_cache::accelerated_swizzle::{
    make_block_linear_swizzle_2d_params, make_block_linear_swizzle_3d_params,
};
use crate::texture_cache::image_info::TilingMode;
use crate::texture_cache::types::{Extent3D, ImageCopy, SwizzleParameters};

macro_rules! assert_fail_soft {
    ($condition:expr, $($message:tt)*) => {
        if !$condition {
            let message = format!($($message)*);
            log::error!("{message}");
            if *common::settings::values().use_debug_asserts.get_value() {
                panic!("{message}");
            }
        }
    };
}

fn make_program(source: &str) -> OGLProgram {
    create_program_from_source(source, gl::COMPUTE_SHADER)
}

/// Utility shaders collection.
///
/// Corresponds to `OpenGL::UtilShaders`.
pub struct UtilShaders {
    // Rust drops fields in declaration order. Program fields are declared in
    // Eden's effective reverse-member destruction order.
    convert_nonms_to_ms_program: OGLProgram,
    convert_ms_to_nonms_program: OGLProgram,
    convert_s8d24_program: OGLProgram,
    copy_bc4_program: OGLProgram,
    pitch_unswizzle_program: OGLProgram,
    block_linear_unswizzle_3d_program: OGLProgram,
    block_linear_unswizzle_2d_program: OGLProgram,
    astc_decoder_program: OGLProgram,
    program_manager: ProgramManagerHandle,
}

impl UtilShaders {
    /// Create the upstream utility compute programs.
    pub fn new(program_manager: ProgramManagerHandle) -> Self {
        let astc_decoder_program = make_program(ASTC_DECODER_COMP);
        let block_linear_unswizzle_2d_program = make_program(BLOCK_LINEAR_UNSWIZZLE_2D_COMP);
        let block_linear_unswizzle_3d_program = make_program(BLOCK_LINEAR_UNSWIZZLE_3D_COMP);
        let pitch_unswizzle_program = make_program(PITCH_UNSWIZZLE_COMP);
        let copy_bc4_program = make_program(OPENGL_COPY_BC4_COMP);
        let convert_s8d24_program = make_program(OPENGL_CONVERT_S8D24_COMP);
        let convert_ms_to_nonms_program = make_program(CONVERT_MSAA_TO_NON_MSAA_COMP);
        let convert_nonms_to_ms_program = make_program(CONVERT_NON_MSAA_TO_MSAA_COMP);
        Self {
            convert_nonms_to_ms_program,
            convert_ms_to_nonms_program,
            convert_s8d24_program,
            copy_bc4_program,
            pitch_unswizzle_program,
            block_linear_unswizzle_3d_program,
            block_linear_unswizzle_2d_program,
            astc_decoder_program,
            program_manager,
        }
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(program_manager: ProgramManagerHandle) -> Self {
        Self {
            convert_nonms_to_ms_program: OGLProgram::new(),
            convert_ms_to_nonms_program: OGLProgram::new(),
            convert_s8d24_program: OGLProgram::new(),
            copy_bc4_program: OGLProgram::new(),
            pitch_unswizzle_program: OGLProgram::new(),
            block_linear_unswizzle_3d_program: OGLProgram::new(),
            block_linear_unswizzle_2d_program: OGLProgram::new(),
            astc_decoder_program: OGLProgram::new(),
            program_manager,
        }
    }

    /// Port of `UtilShaders::ASTCDecode`.
    pub fn astc_decode(
        &mut self,
        image: &mut Image,
        map: &StagingBufferMap,
        swizzles: &[SwizzleParameters],
    ) {
        const BINDING_INPUT_BUFFER: u32 = 0;
        const BINDING_OUTPUT_IMAGE: u32 = 0;
        let mut program_manager = self.program_manager.lock();
        program_manager.local_memory_warmup();
        let info = image.base().info.clone();
        let guest_size_bytes = image.base().guest_size_bytes as usize;
        let tile_width = default_block_width(info.format);
        let tile_height = default_block_height(info.format);
        program_manager.bind_compute_program(self.astc_decoder_program.handle);
        unsafe {
            gl::FlushMappedNamedBufferRange(
                map.buffer,
                map.offset as isize,
                guest_size_bytes as isize,
            );
            gl::Uniform2ui(1, tile_width, tile_height);
            gl::Flush();
            for swizzle in swizzles {
                let input_offset = map.offset.wrapping_add(swizzle.buffer_offset);
                let range_size = guest_size_bytes.wrapping_sub(swizzle.buffer_offset);
                let params = make_block_linear_swizzle_2d_params(swizzle, &info);
                assert_fail_soft!(
                    params.origin == [0, 0, 0],
                    "ASTC decode origin differs from upstream invariant: {:?}",
                    params.origin
                );
                assert_fail_soft!(
                    params.destination == [0, 0, 0],
                    "ASTC decode destination differs from upstream invariant: {:?}",
                    params.destination
                );
                assert_fail_soft!(
                    params.bytes_per_block_log2 == 4,
                    "ASTC decode bytes_per_block_log2 differs from upstream invariant: {}",
                    params.bytes_per_block_log2
                );
                gl::Uniform1ui(2, params.layer_stride);
                gl::Uniform1ui(3, params.block_size);
                gl::Uniform1ui(4, params.x_shift);
                gl::Uniform1ui(5, params.block_height);
                gl::Uniform1ui(6, params.block_height_mask);
                gl::BindBufferRange(
                    gl::SHADER_STORAGE_BUFFER,
                    BINDING_INPUT_BUFFER,
                    map.buffer,
                    input_offset as isize,
                    range_size as isize,
                );
                gl::BindImageTexture(
                    BINDING_OUTPUT_IMAGE,
                    image.storage_handle(),
                    swizzle.level,
                    gl::TRUE,
                    0,
                    gl::WRITE_ONLY,
                    gl::RGBA8,
                );
                gl::DispatchCompute(
                    swizzle.num_tiles.width.div_ceil(8),
                    swizzle.num_tiles.height.div_ceil(8),
                    info.resources.layers as u32,
                );
            }
            gl::MemoryBarrier(
                gl::UNIFORM_BARRIER_BIT
                    | gl::COMMAND_BARRIER_BIT
                    | gl::PIXEL_BUFFER_BARRIER_BIT
                    | gl::TEXTURE_UPDATE_BARRIER_BIT
                    | gl::BUFFER_UPDATE_BARRIER_BIT
                    | gl::SHADER_STORAGE_BARRIER_BIT
                    | gl::CLIENT_MAPPED_BUFFER_BARRIER_BIT,
            );
        }
        program_manager.restore_guest_compute();
    }

    /// Port of `UtilShaders::BlockLinearUpload2D`.
    pub fn block_linear_upload_2d(
        &mut self,
        image: &mut Image,
        map: &StagingBufferMap,
        swizzles: &[SwizzleParameters],
    ) {
        const WORKGROUP_SIZE: Extent3D = Extent3D {
            width: 32,
            height: 32,
            depth: 1,
        };
        const BINDING_INPUT_BUFFER: u32 = 0;
        const BINDING_OUTPUT_IMAGE: u32 = 0;
        let info = image.base().info.clone();
        let guest_size_bytes = image.base().guest_size_bytes as usize;
        let mut program_manager = self.program_manager.lock();
        program_manager.bind_compute_program(self.block_linear_unswizzle_2d_program.handle);
        unsafe {
            gl::FlushMappedNamedBufferRange(
                map.buffer,
                map.offset as isize,
                guest_size_bytes as isize,
            );
            let store_fmt = store_format(bytes_per_block(info.format));
            for swizzle in swizzles {
                let input_offset = map.offset.wrapping_add(swizzle.buffer_offset);
                let range_size = guest_size_bytes.wrapping_sub(swizzle.buffer_offset);
                let params = make_block_linear_swizzle_2d_params(swizzle, &info);
                gl::Uniform3uiv(0, 1, params.origin.as_ptr());
                gl::Uniform3iv(1, 1, params.destination.as_ptr());
                gl::Uniform1ui(2, params.bytes_per_block_log2);
                gl::Uniform1ui(3, params.layer_stride);
                gl::Uniform1ui(4, params.block_size);
                gl::Uniform1ui(5, params.x_shift);
                gl::Uniform1ui(6, params.block_height);
                gl::Uniform1ui(7, params.block_height_mask);
                gl::BindBufferRange(
                    gl::SHADER_STORAGE_BUFFER,
                    BINDING_INPUT_BUFFER,
                    map.buffer,
                    input_offset as isize,
                    range_size as isize,
                );
                gl::BindImageTexture(
                    BINDING_OUTPUT_IMAGE,
                    image.storage_handle(),
                    swizzle.level,
                    gl::TRUE,
                    0,
                    gl::WRITE_ONLY,
                    store_fmt,
                );
                gl::DispatchCompute(
                    swizzle.num_tiles.width.div_ceil(WORKGROUP_SIZE.width),
                    swizzle.num_tiles.height.div_ceil(WORKGROUP_SIZE.height),
                    info.resources.layers as u32,
                );
            }
        }
        program_manager.restore_guest_compute();
    }

    /// Port of `UtilShaders::BlockLinearUpload3D`.
    pub fn block_linear_upload_3d(
        &mut self,
        image: &mut Image,
        map: &StagingBufferMap,
        swizzles: &[SwizzleParameters],
    ) {
        const WORKGROUP_SIZE: Extent3D = Extent3D {
            width: 16,
            height: 8,
            depth: 8,
        };
        const BINDING_INPUT_BUFFER: u32 = 0;
        const BINDING_OUTPUT_IMAGE: u32 = 0;
        let info = image.base().info.clone();
        let guest_size_bytes = image.base().guest_size_bytes as usize;
        let mut program_manager = self.program_manager.lock();
        unsafe {
            gl::FlushMappedNamedBufferRange(
                map.buffer,
                map.offset as isize,
                guest_size_bytes as isize,
            );
            program_manager.bind_compute_program(self.block_linear_unswizzle_3d_program.handle);
            let store_fmt = store_format(bytes_per_block(info.format));
            for swizzle in swizzles {
                let input_offset = map.offset.wrapping_add(swizzle.buffer_offset);
                let range_size = guest_size_bytes.wrapping_sub(swizzle.buffer_offset);
                let params = make_block_linear_swizzle_3d_params(swizzle, &info);
                gl::Uniform3uiv(0, 1, params.origin.as_ptr());
                gl::Uniform3iv(1, 1, params.destination.as_ptr());
                gl::Uniform1ui(2, params.bytes_per_block_log2);
                gl::Uniform1ui(3, params.slice_size);
                gl::Uniform1ui(4, params.block_size);
                gl::Uniform1ui(5, params.x_shift);
                gl::Uniform1ui(6, params.block_height);
                gl::Uniform1ui(7, params.block_height_mask);
                gl::Uniform1ui(8, params.block_depth);
                gl::Uniform1ui(9, params.block_depth_mask);
                gl::BindBufferRange(
                    gl::SHADER_STORAGE_BUFFER,
                    BINDING_INPUT_BUFFER,
                    map.buffer,
                    input_offset as isize,
                    range_size as isize,
                );
                gl::BindImageTexture(
                    BINDING_OUTPUT_IMAGE,
                    image.storage_handle(),
                    swizzle.level,
                    gl::TRUE,
                    0,
                    gl::WRITE_ONLY,
                    store_fmt,
                );
                gl::DispatchCompute(
                    swizzle.num_tiles.width.div_ceil(WORKGROUP_SIZE.width),
                    swizzle.num_tiles.height.div_ceil(WORKGROUP_SIZE.height),
                    swizzle.num_tiles.depth.div_ceil(WORKGROUP_SIZE.depth),
                );
            }
        }
        program_manager.restore_guest_compute();
    }

    /// Port of `UtilShaders::PitchUpload`.
    pub fn pitch_upload(
        &mut self,
        image: &mut Image,
        map: &StagingBufferMap,
        swizzles: &[SwizzleParameters],
    ) {
        const WORKGROUP_SIZE: Extent3D = Extent3D {
            width: 32,
            height: 32,
            depth: 1,
        };
        const BINDING_INPUT_BUFFER: u32 = 0;
        const BINDING_OUTPUT_IMAGE: u32 = 0;
        const LOC_ORIGIN: i32 = 0;
        const LOC_DESTINATION: i32 = 1;
        const LOC_BYTES_PER_BLOCK: i32 = 2;
        const LOC_PITCH: i32 = 3;
        let info = image.base().info.clone();
        let guest_size_bytes = image.base().guest_size_bytes as usize;
        let bytes_per_block = bytes_per_block(info.format);
        let store_fmt = store_format(bytes_per_block);
        let pitch = match info.tiling {
            TilingMode::PitchLinear(pitch) => pitch,
            // C++ reads the first u32 of the anonymous block/pitch union.
            TilingMode::BlockLinear(block) => block.width,
        };
        assert_fail_soft!(
            bytes_per_block.is_power_of_two(),
            "Non-power of two images are not implemented"
        );
        let mut program_manager = self.program_manager.lock();
        program_manager.bind_compute_program(self.pitch_unswizzle_program.handle);
        unsafe {
            gl::FlushMappedNamedBufferRange(
                map.buffer,
                map.offset as isize,
                guest_size_bytes as isize,
            );
            gl::Uniform2ui(LOC_ORIGIN, 0, 0);
            gl::Uniform2i(LOC_DESTINATION, 0, 0);
            gl::Uniform1ui(LOC_BYTES_PER_BLOCK, bytes_per_block);
            gl::Uniform1ui(LOC_PITCH, pitch);
            gl::BindImageTexture(
                BINDING_OUTPUT_IMAGE,
                image.storage_handle(),
                0,
                gl::FALSE,
                0,
                gl::WRITE_ONLY,
                store_fmt,
            );
            for swizzle in swizzles {
                let input_offset = map.offset.wrapping_add(swizzle.buffer_offset);
                let range_size = guest_size_bytes.wrapping_sub(swizzle.buffer_offset);
                gl::BindBufferRange(
                    gl::SHADER_STORAGE_BUFFER,
                    BINDING_INPUT_BUFFER,
                    map.buffer,
                    input_offset as isize,
                    range_size as isize,
                );
                gl::DispatchCompute(
                    swizzle.num_tiles.width.div_ceil(WORKGROUP_SIZE.width),
                    swizzle.num_tiles.height.div_ceil(WORKGROUP_SIZE.height),
                    1,
                );
            }
        }
        program_manager.restore_guest_compute();
    }

    /// Port of `UtilShaders::CopyBC4`.
    pub fn copy_bc4(&mut self, dst_image: &mut Image, src_image: &mut Image, copies: &[ImageCopy]) {
        const BINDING_INPUT_IMAGE: u32 = 0;
        const BINDING_OUTPUT_IMAGE: u32 = 1;
        const LOC_SRC_OFFSET: i32 = 0;
        const LOC_DST_OFFSET: i32 = 1;
        let mut program_manager = self.program_manager.lock();
        program_manager.bind_compute_program(self.copy_bc4_program.handle);
        unsafe {
            for copy in copies {
                assert_fail_soft!(
                    copy.src_subresource.base_layer == 0,
                    "CopyBC4 source base layer must be zero"
                );
                assert_fail_soft!(
                    copy.src_subresource.num_layers == 1,
                    "CopyBC4 source layer count must be one"
                );
                assert_fail_soft!(
                    copy.dst_subresource.base_layer == 0,
                    "CopyBC4 destination base layer must be zero"
                );
                assert_fail_soft!(
                    copy.dst_subresource.num_layers == 1,
                    "CopyBC4 destination layer count must be one"
                );

                gl::Uniform3ui(
                    LOC_SRC_OFFSET,
                    copy.src_offset.x as u32,
                    copy.src_offset.y as u32,
                    copy.src_offset.z as u32,
                );
                gl::Uniform3ui(
                    LOC_DST_OFFSET,
                    copy.dst_offset.x as u32,
                    copy.dst_offset.y as u32,
                    copy.dst_offset.z as u32,
                );
                gl::BindImageTexture(
                    BINDING_INPUT_IMAGE,
                    src_image.storage_handle(),
                    copy.src_subresource.base_level as i32,
                    gl::TRUE,
                    0,
                    gl::READ_ONLY,
                    gl::RG32UI,
                );
                gl::BindImageTexture(
                    BINDING_OUTPUT_IMAGE,
                    dst_image.storage_handle(),
                    copy.dst_subresource.base_level as i32,
                    gl::TRUE,
                    0,
                    gl::WRITE_ONLY,
                    gl::RGBA8UI,
                );
                gl::DispatchCompute(copy.extent.width, copy.extent.height, copy.extent.depth);
            }
        }
        program_manager.restore_guest_compute();
    }

    /// Port of `UtilShaders::ConvertS8D24`.
    pub fn convert_s8d24(&mut self, dst_image: &mut Image, copies: &[ImageCopy]) {
        const BINDING_DESTINATION: u32 = 0;
        const LOC_SIZE: i32 = 0;
        let mut program_manager = self.program_manager.lock();
        program_manager.bind_compute_program(self.convert_s8d24_program.handle);
        unsafe {
            for copy in copies {
                assert_fail_soft!(
                    copy.src_subresource.base_layer == 0,
                    "ConvertS8D24 source base layer must be zero"
                );
                assert_fail_soft!(
                    copy.src_subresource.num_layers == 1,
                    "ConvertS8D24 source layer count must be one"
                );
                assert_fail_soft!(
                    copy.dst_subresource.base_layer == 0,
                    "ConvertS8D24 destination base layer must be zero"
                );
                assert_fail_soft!(
                    copy.dst_subresource.num_layers == 1,
                    "ConvertS8D24 destination layer count must be one"
                );

                gl::Uniform3ui(
                    LOC_SIZE,
                    copy.extent.width,
                    copy.extent.height,
                    copy.extent.depth,
                );
                gl::BindImageTexture(
                    BINDING_DESTINATION,
                    dst_image.storage_handle(),
                    copy.dst_subresource.base_level as i32,
                    gl::TRUE,
                    0,
                    gl::READ_WRITE,
                    gl::RGBA8UI,
                );
                gl::DispatchCompute(
                    copy.extent.width.div_ceil(16),
                    copy.extent.height.div_ceil(8),
                    copy.extent.depth,
                );
            }
        }
        program_manager.restore_guest_compute();
    }

    /// Copy between MSAA and non-MSAA textures.
    ///
    /// Port of `UtilShaders::CopyMSAA`.
    pub fn copy_msaa(
        &mut self,
        dst_image: &mut Image,
        src_image: &mut Image,
        copies: &[ImageCopy],
    ) {
        let ms_to_nonms =
            src_image.base().info.num_samples > 1 && dst_image.base().info.num_samples == 1;
        let program = if ms_to_nonms {
            self.convert_ms_to_nonms_program.handle
        } else {
            self.convert_nonms_to_ms_program.handle
        };
        let mut program_manager = self.program_manager.lock();
        program_manager.bind_compute_program(program);
        unsafe {
            for copy in copies {
                assert_fail_soft!(
                    copy.src_subresource.base_layer == 0,
                    "CopyMSAA source base layer must be zero"
                );
                assert_fail_soft!(
                    copy.src_subresource.num_layers == 1,
                    "CopyMSAA source layer count must be one"
                );
                assert_fail_soft!(
                    copy.dst_subresource.base_layer == 0,
                    "CopyMSAA destination base layer must be zero"
                );
                assert_fail_soft!(
                    copy.dst_subresource.num_layers == 1,
                    "CopyMSAA destination layer count must be one"
                );

                gl::BindImageTexture(
                    0,
                    src_image.storage_handle(),
                    copy.src_subresource.base_level as i32,
                    gl::TRUE,
                    0,
                    gl::READ_ONLY,
                    gl::RGBA8,
                );
                gl::BindImageTexture(
                    1,
                    dst_image.storage_handle(),
                    copy.dst_subresource.base_level as i32,
                    gl::TRUE,
                    0,
                    gl::WRITE_ONLY,
                    gl::RGBA8,
                );

                let groups_x = copy.extent.width.div_ceil(8);
                let groups_y = copy.extent.height.div_ceil(8);
                gl::DispatchCompute(groups_x, groups_y, copy.extent.depth);
            }
        }
        program_manager.restore_guest_compute();
    }
}

/// Map bytes-per-block to the appropriate GL store format for compute shader image access.
///
/// Corresponds to `OpenGL::StoreFormat()`.
pub fn store_format(bytes_per_block: u32) -> u32 {
    match bytes_per_block {
        1 => gl::R8UI,
        2 => gl::R16UI,
        4 => gl::R32UI,
        8 => gl::RG32UI,
        16 => gl::RGBA32UI,
        _ => {
            assert_fail_soft!(false, "Invalid bytes_per_block: {bytes_per_block}");
            gl::R8UI
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_format_mapping() {
        assert_eq!(store_format(1), gl::R8UI);
        assert_eq!(store_format(2), gl::R16UI);
        assert_eq!(store_format(4), gl::R32UI);
        assert_eq!(store_format(8), gl::RG32UI);
        assert_eq!(store_format(16), gl::RGBA32UI);
        assert_eq!(store_format(3), gl::R8UI);
    }

    #[test]
    fn utility_programs_use_the_upstream_raii_owners() {
        let shaders = UtilShaders::new_for_test(
            crate::renderer_opengl::gl_shader_manager::ProgramManager::new_shared_for_test(),
        );
        assert_eq!(shaders.astc_decoder_program.handle, 0);
        assert_eq!(shaders.block_linear_unswizzle_2d_program.handle, 0);
        assert_eq!(shaders.block_linear_unswizzle_3d_program.handle, 0);
        assert_eq!(shaders.pitch_unswizzle_program.handle, 0);
        assert_eq!(shaders.copy_bc4_program.handle, 0);
        assert_eq!(shaders.convert_s8d24_program.handle, 0);
        assert_eq!(shaders.convert_ms_to_nonms_program.handle, 0);
        assert_eq!(shaders.convert_nonms_to_ms_program.handle, 0);
    }
}
