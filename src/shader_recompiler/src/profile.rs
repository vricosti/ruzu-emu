// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `shader_recompiler/profile.h`
//!
//! GPU/driver capability profile used during shader compilation.

use crate::stage::Stage;

/// Full GPU/driver capability profile matching upstream `Profile` struct.
#[derive(Debug, Clone)]
pub struct Profile {
    pub supported_spirv: u32,
    pub unified_descriptor_binding: bool,
    pub support_descriptor_aliasing: bool,
    pub support_int8: bool,
    pub support_uniform_and_storage_buffer_8bit: bool,
    pub support_storage_buffer_8bit: bool,
    pub support_int16: bool,
    pub support_uniform_and_storage_buffer_16bit: bool,
    pub support_storage_buffer_16bit: bool,
    pub support_int64: bool,
    pub support_vertex_instance_id: bool,
    pub support_float_controls: bool,
    pub support_separate_denorm_behavior: bool,
    pub support_separate_rounding_mode: bool,
    pub support_fp16_denorm_preserve: bool,
    pub support_fp32_denorm_preserve: bool,
    pub support_fp16_denorm_flush: bool,
    pub support_fp32_denorm_flush: bool,
    pub support_fp16_signed_zero_nan_preserve: bool,
    pub support_fp32_signed_zero_nan_preserve: bool,
    pub support_fp64_signed_zero_nan_preserve: bool,
    pub support_explicit_workgroup_layout: bool,
    pub support_workgroup_layout_8bit_access: bool,
    pub support_workgroup_layout_16bit_access: bool,
    pub support_vote: bool,
    pub supported_subgroup_stages: u32,
    pub support_viewport_index_layer_non_geometry: bool,
    pub support_viewport_mask: bool,
    pub support_typeless_image_loads: bool,
    pub support_demote_to_helper_invocation: bool,
    pub support_int64_atomics: bool,
    pub support_shared_int64_atomics: bool,
    pub support_derivative_control: bool,
    pub support_geometry_shader_passthrough: bool,
    pub support_native_ndc: bool,
    pub support_gl_nv_gpu_shader_5: bool,
    pub support_gl_amd_gpu_shader_half_float: bool,
    pub support_gl_texture_shadow_lod: bool,
    pub support_gl_warp_intrinsics: bool,
    pub support_gl_variable_aoffi: bool,
    pub support_gl_sparse_textures: bool,
    pub support_gl_derivative_control: bool,
    pub support_scaled_attributes: bool,
    pub support_multi_viewport: bool,
    pub support_geometry_streams: bool,
    pub support_sampled_image_array_nonuniform_indexing: bool,
    pub support_storage_image_array_nonuniform_indexing: bool,
    pub support_uniform_texel_buffer_array_nonuniform_indexing: bool,
    pub support_storage_texel_buffer_array_nonuniform_indexing: bool,

    pub warp_size_potentially_larger_than_guest: bool,

    pub lower_left_origin_mode: bool,
    /// Fragment outputs have to be declared even if they are not written to avoid undefined values.
    pub need_declared_frag_colors: bool,
    /// Prevents fast math optimizations that may cause inaccuracies.
    pub need_fastmath_off: bool,
    /// Some GPU vendors use a different rounding precision when calculating texture pixel
    /// coordinates with the 16.8 format in the ImageGather instruction.
    pub need_gather_subpixel_offset: bool,

    /// OpFClamp is broken and OpFMax + OpFMin should be used instead.
    pub has_broken_spirv_clamp: bool,
    /// The Position builtin needs to be wrapped in a struct when used as an input.
    pub has_broken_spirv_position_input: bool,
    /// Offset image operands with an unsigned type do not work.
    pub has_broken_unsigned_image_offsets: bool,
    /// Signed instructions with unsigned data types are misinterpreted.
    pub has_broken_signed_operations: bool,
    /// Float controls break when fp16 is enabled.
    pub has_broken_fp16_float_controls: bool,
    /// Declaring fp32 denorm flush to zero miscompiles on some drivers
    pub has_broken_fp32_denorm_flush: bool,
    /// Dynamic vec4 indexing is broken on some OpenGL drivers.
    pub has_gl_component_indexing_bug: bool,
    /// The precise type qualifier is broken in the fragment stage of some drivers.
    pub has_gl_precise_bug: bool,
    /// Some drivers do not properly support floatBitsToUint when used on cbufs.
    pub has_gl_cbuf_ftou_bug: bool,
    /// Some drivers poorly optimize boolean variable references.
    pub has_gl_bool_ref_bug: bool,
    /// Ignores SPIR-V ordered vs unordered using GLSL semantics.
    pub ignore_nan_fp_comparisons: bool,
    /// Some drivers have broken support for OpVectorExtractDynamic on subgroup mask inputs.
    pub has_broken_spirv_subgroup_mask_vector_extract_dynamic: bool,

    pub gl_max_compute_smem_size: u32,

    /// Maxwell and earlier nVidia architectures have broken robust support.
    pub has_broken_robust: bool,

    pub min_ssbo_alignment: u64,

    pub max_user_clip_distances: u32,
}

impl Profile {
    /// Port of upstream `Profile::SupportsSubgroupStage`.
    pub fn supports_subgroup_stage(&self, stage: Stage) -> bool {
        (self.supported_subgroup_stages & (1u32 << stage as u32)) != 0
    }
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            supported_spirv: 0x00010000,
            unified_descriptor_binding: false,
            support_descriptor_aliasing: false,
            support_int8: false,
            support_uniform_and_storage_buffer_8bit: false,
            support_storage_buffer_8bit: false,
            support_int16: false,
            support_uniform_and_storage_buffer_16bit: false,
            support_storage_buffer_16bit: false,
            support_int64: false,
            support_vertex_instance_id: false,
            support_float_controls: false,
            support_separate_denorm_behavior: false,
            support_separate_rounding_mode: false,
            support_fp16_denorm_preserve: false,
            support_fp32_denorm_preserve: false,
            support_fp16_denorm_flush: false,
            support_fp32_denorm_flush: false,
            support_fp16_signed_zero_nan_preserve: false,
            support_fp32_signed_zero_nan_preserve: false,
            support_fp64_signed_zero_nan_preserve: false,
            support_explicit_workgroup_layout: false,
            support_workgroup_layout_8bit_access: false,
            support_workgroup_layout_16bit_access: false,
            support_vote: false,
            supported_subgroup_stages: 0x7F,
            support_viewport_index_layer_non_geometry: false,
            support_viewport_mask: false,
            support_typeless_image_loads: false,
            support_demote_to_helper_invocation: false,
            support_int64_atomics: false,
            support_shared_int64_atomics: false,
            support_derivative_control: false,
            support_geometry_shader_passthrough: false,
            support_native_ndc: false,
            support_gl_nv_gpu_shader_5: false,
            support_gl_amd_gpu_shader_half_float: false,
            support_gl_texture_shadow_lod: false,
            support_gl_warp_intrinsics: false,
            support_gl_variable_aoffi: false,
            support_gl_sparse_textures: false,
            support_gl_derivative_control: false,
            support_scaled_attributes: false,
            support_multi_viewport: false,
            support_geometry_streams: false,
            support_sampled_image_array_nonuniform_indexing: false,
            support_storage_image_array_nonuniform_indexing: false,
            support_uniform_texel_buffer_array_nonuniform_indexing: false,
            support_storage_texel_buffer_array_nonuniform_indexing: false,
            warp_size_potentially_larger_than_guest: false,
            lower_left_origin_mode: false,
            need_declared_frag_colors: false,
            need_fastmath_off: false,
            need_gather_subpixel_offset: false,
            has_broken_spirv_clamp: false,
            has_broken_spirv_position_input: false,
            has_broken_unsigned_image_offsets: false,
            has_broken_signed_operations: false,
            has_broken_fp16_float_controls: false,
            has_broken_fp32_denorm_flush: false,
            has_gl_component_indexing_bug: false,
            has_gl_precise_bug: false,
            has_gl_cbuf_ftou_bug: false,
            has_gl_bool_ref_bug: false,
            ignore_nan_fp_comparisons: false,
            has_broken_spirv_subgroup_mask_vector_extract_dynamic: false,
            gl_max_compute_smem_size: 0,
            has_broken_robust: false,
            min_ssbo_alignment: 0,
            max_user_clip_distances: 0,
        }
    }
}
