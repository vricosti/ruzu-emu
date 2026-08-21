// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `vk_pipeline_cache.h` / `vk_pipeline_cache.cpp`.
//!
//! Manages compilation and caching of both graphics and compute pipelines,
//! including disk serialization of the Vulkan pipeline cache.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::panic::{catch_unwind, resume_unwind, take_hook, AssertUnwindSafe};
use std::path::PathBuf;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ash::vk;
use common::cityhash::city_hash64;
use common::hash::BuildUnorderedDenseHasher;
use common::thread_worker::ThreadWorker;

use crate::control::channel_state::ChannelState;
use crate::control::channel_state_cache::{ChannelInfo, ChannelSetupCaches};
use crate::engines::draw_manager::Maxwell3DDrawView;
use crate::engines::maxwell_3d::{ComparisonOp, PrimitiveTopology, VertexAttribType};
use crate::gpu_logging::{dump_spirv_shader, get_instance, get_shader_stage_name, is_active};
use crate::rasterizer_interface::{
    DiskResourceLoadCallback, DiskResourceLoadStop, LoadCallbackStage,
};
use crate::shader_cache::{GraphicsEnvironments, ShaderCache as SharedShaderCache, NUM_PROGRAMS};
use crate::shader_environment::{
    load_pipelines, serialize_pipeline, ComputeEnvironment, FileEnvironment,
};
use crate::surface::{
    is_pixel_format_integer, is_pixel_format_signed_integer, pixel_format_from_render_target_format,
};
use crate::vulkan_common::vulkan_device::{Device, DeviceReference, NvidiaArchitecture};
use shader_recompiler::backend::bindings::Bindings;
use shader_recompiler::frontend::control_flow::FlowBlock;
use shader_recompiler::frontend::translate_program::{
    convert_legacy_to_generic, generate_geometry_passthrough, merge_dual_vertex_programs,
};
use shader_recompiler::host_translate_info::HostTranslateInfo;
use shader_recompiler::ir::basic_block::Block;
use shader_recompiler::ir::instruction::Inst;
use shader_recompiler::ir::program::Program;
use shader_recompiler::ir::types::OutputTopology;
use shader_recompiler::object_pool::ObjectPool;
use shader_recompiler::runtime_info::{
    AttributeType, CompareFunction, InputTopology, TessPrimitive, TessSpacing,
};
use shader_recompiler::shader_info::Info as ShaderInfo;
use shader_recompiler::{CompiledShader, Profile, RuntimeInfo, ShaderStage};

use super::buffer_cache::VulkanCommonBufferCache;
use super::compute_pipeline::{ComputePipeline, ComputePipelineRuntime};
use super::descriptor_buffer::DescriptorBufferRing;
use super::descriptor_pool::DescriptorPool;
use super::fixed_pipeline_state::{DynamicFeatures, FixedPipelineState, VertexAttribute};
use super::graphics_pipeline::{GraphicsPipeline, GraphicsPipelineKey, GraphicsPipelineRuntime};
use super::pipeline_statistics::PipelineStatistics;

use super::render_pass_cache::RenderPassCache;
use super::scheduler::Scheduler;
use super::texture_cache::TextureCache;
use super::update_descriptor::UpdateDescriptorQueue;

fn needs_gather_subpixel_offset(driver_id: vk::DriverId) -> bool {
    matches!(
        driver_id,
        vk::DriverId::AMD_PROPRIETARY
            | vk::DriverId::AMD_OPEN_SOURCE
            | vk::DriverId::MESA_RADV
            | vk::DriverId::INTEL_PROPRIETARY_WINDOWS
            | vk::DriverId::INTEL_OPEN_SOURCE_MESA
    )
}

/// Port of the `supported_subgroup_stages` fold in upstream
/// `PipelineCache::PipelineCache`. Each `Shader::Stage` contributes its own bit
/// index, so `VertexA` and `VertexB` both read the Vulkan vertex stage flag.
fn supported_subgroup_stages(device: &Device) -> u32 {
    use shader_recompiler::stage::Stage;

    let subgroup_stages = device.get_subgroup_supported_stages();
    let bit = |flag: vk::ShaderStageFlags, stage: Stage| -> u32 {
        if subgroup_stages.contains(flag) {
            1u32 << stage as u32
        } else {
            0
        }
    };
    bit(vk::ShaderStageFlags::VERTEX, Stage::VertexA)
        | bit(vk::ShaderStageFlags::VERTEX, Stage::VertexB)
        | bit(
            vk::ShaderStageFlags::TESSELLATION_CONTROL,
            Stage::TessellationControl,
        )
        | bit(
            vk::ShaderStageFlags::TESSELLATION_EVALUATION,
            Stage::TessellationEval,
        )
        | bit(vk::ShaderStageFlags::GEOMETRY, Stage::Geometry)
        | bit(vk::ShaderStageFlags::FRAGMENT, Stage::Fragment)
        | bit(vk::ShaderStageFlags::COMPUTE, Stage::Compute)
}

/// Builds the Vulkan shader profile owned by upstream `PipelineCache`.
pub(super) fn make_shader_profile(device: &Device) -> Profile {
    let float_control = device.float_control_properties();
    let driver_id = device.get_driver_id();
    Profile {
        supported_spirv: device.supported_spirv_version(),
        unified_descriptor_binding: true,
        support_descriptor_aliasing: device.is_descriptor_aliasing_supported(),
        support_int8: device.is_int8_supported(),
        support_uniform_and_storage_buffer_8bit: device
            .is_uniform_and_storage_buffer_8bit_access_supported(),
        support_storage_buffer_8bit: device.is_storage_buffer_8bit_access_supported(),
        support_int16: device.is_shader_int16_supported(),
        support_uniform_and_storage_buffer_16bit: device
            .is_uniform_and_storage_buffer_16bit_access_supported(),
        support_storage_buffer_16bit: device.is_storage_buffer_16bit_access_supported(),
        support_int64: device.is_shader_int64_supported(),
        support_vertex_instance_id: false,
        support_float_controls: device.is_khr_shader_float_controls_supported(),
        support_separate_denorm_behavior: float_control.denorm_behavior_independence
            == vk::ShaderFloatControlsIndependence::ALL,
        support_separate_rounding_mode: float_control.rounding_mode_independence
            == vk::ShaderFloatControlsIndependence::ALL,
        support_fp16_denorm_preserve: float_control.shader_denorm_preserve_float16 != 0,
        support_fp32_denorm_preserve: float_control.shader_denorm_preserve_float32 != 0,
        support_fp16_denorm_flush: float_control.shader_denorm_flush_to_zero_float16 != 0,
        support_fp32_denorm_flush: float_control.shader_denorm_flush_to_zero_float32 != 0,
        support_fp16_signed_zero_nan_preserve: float_control
            .shader_signed_zero_inf_nan_preserve_float16
            != 0,
        support_fp32_signed_zero_nan_preserve: float_control
            .shader_signed_zero_inf_nan_preserve_float32
            != 0,
        support_fp64_signed_zero_nan_preserve: float_control
            .shader_signed_zero_inf_nan_preserve_float64
            != 0,
        support_explicit_workgroup_layout: device
            .is_khr_workgroup_memory_explicit_layout_supported(),
        support_workgroup_layout_8bit_access: device
            .is_workgroup_memory_explicit_layout_8bit_access_supported(),
        support_workgroup_layout_16bit_access: device
            .is_workgroup_memory_explicit_layout_16bit_access_supported(),
        support_vote: device.is_subgroup_feature_supported(vk::SubgroupFeatureFlags::VOTE),
        supported_subgroup_stages: supported_subgroup_stages(device),
        support_viewport_index_layer_non_geometry: device
            .is_ext_shader_viewport_index_layer_supported(),
        support_viewport_mask: device.is_nv_viewport_array2_supported(),
        support_typeless_image_loads: device.is_formatless_image_load_supported(),
        support_demote_to_helper_invocation: device
            .is_ext_shader_demote_to_helper_invocation_supported(),
        support_int64_atomics: device.is_ext_shader_atomic_int64_supported(),
        support_shared_int64_atomics: device.is_shared_int64_atomics_supported(),
        support_derivative_control: true,
        support_geometry_shader_passthrough: device.is_nv_geometry_shader_passthrough_supported(),
        support_native_ndc: device.is_ext_depth_clip_control_supported(),
        support_scaled_attributes: !device.must_emulate_scaled_formats(),
        support_multi_viewport: device.supports_multi_viewport(),
        support_geometry_streams: device.are_transform_feedback_geometry_streams_supported(),
        support_sampled_image_array_nonuniform_indexing: device
            .is_sampled_image_array_non_uniform_indexing_supported(),
        support_storage_image_array_nonuniform_indexing: device
            .is_storage_image_array_non_uniform_indexing_supported(),
        support_uniform_texel_buffer_array_nonuniform_indexing: device
            .is_uniform_texel_buffer_array_non_uniform_indexing_supported(),
        support_storage_texel_buffer_array_nonuniform_indexing: device
            .is_storage_texel_buffer_array_non_uniform_indexing_supported(),
        warp_size_potentially_larger_than_guest: device
            .is_warp_size_potentially_bigger_than_guest(),
        lower_left_origin_mode: false,
        need_declared_frag_colors: false,
        need_gather_subpixel_offset: needs_gather_subpixel_offset(driver_id),
        has_broken_spirv_clamp: driver_id == vk::DriverId::INTEL_PROPRIETARY_WINDOWS,
        has_broken_spirv_position_input: false,
        has_broken_unsigned_image_offsets: false,
        has_broken_signed_operations: false,
        has_broken_fp16_float_controls: driver_id == vk::DriverId::NVIDIA_PROPRIETARY,
        ignore_nan_fp_comparisons: false,
        has_broken_spirv_subgroup_mask_vector_extract_dynamic: false,
        has_broken_robust: device.is_nvidia()
            && device.get_nvidia_arch() <= NvidiaArchitecture::Pascal,
        min_ssbo_alignment: device.get_storage_buffer_alignment(),
        max_user_clip_distances: device.get_max_user_clip_distances(),
        ..Profile::default()
    }
}

/// Builds the host translation limits owned by upstream `PipelineCache`.
fn make_host_translate_info(device: &Device) -> HostTranslateInfo {
    let driver_id = device.get_driver_id();
    let mut host_info = HostTranslateInfo {
        min_ssbo_alignment: device.get_storage_buffer_alignment(),
        max_per_stage_descriptor_sampled_images: device
            .get_max_per_stage_descriptor_sampled_images(),
        max_per_stage_resources: device.get_max_per_stage_resources(),
        max_descriptor_set_samplers: device.get_max_descriptor_set_samplers(),
        max_descriptor_set_uniform_buffers: device.get_max_descriptor_set_uniform_buffers(),
        max_descriptor_set_uniform_buffers_dynamic: device
            .get_max_descriptor_set_uniform_buffers_dynamic(),
        max_descriptor_set_storage_buffers: device.get_max_descriptor_set_storage_buffers(),
        max_descriptor_set_storage_buffers_dynamic: device
            .get_max_descriptor_set_storage_buffers_dynamic(),
        max_descriptor_set_sampled_images: device.get_max_descriptor_set_sampled_images(),
        max_descriptor_set_storage_images: device.get_max_descriptor_set_storage_images(),
        max_descriptor_set_input_attachements: device.get_max_descriptor_set_input_attachments(),
        support_float64: device.is_float64_supported(),
        support_float16: device.is_float16_supported(),
        support_int64: device.is_shader_int64_supported(),
        needs_demote_reorder: matches!(
            driver_id,
            vk::DriverId::AMD_PROPRIETARY
                | vk::DriverId::AMD_OPEN_SOURCE
                | vk::DriverId::SAMSUNG_PROPRIETARY
        ),
        support_snorm_render_buffer: true,
        support_viewport_index_layer: device.is_ext_shader_viewport_index_layer_supported(),
        support_geometry_shader_passthrough: device.is_nv_geometry_shader_passthrough_supported(),
        support_conditional_barrier: device.supports_conditional_barriers(),
    };
    host_info.apply_descriptor_limit_policy();
    host_info
}

/// One-time installation of the shader-exception panic-hook filter. The hook
/// remains process-wide, but only typed shader exceptions inside this
/// thread-local scope are silenced; independent panics retain normal output.
static SHADER_EXCEPTION_HOOK_INSTALL: std::sync::Once = std::sync::Once::new();

thread_local! {
    /// True while the current thread is inside the Rust equivalent of an
    /// upstream `catch (const Shader::Exception&)` scope.
    static IN_SHADER_EXCEPTION_SCOPE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn shader_exception_message(payload: &(dyn std::any::Any + Send)) -> Option<String> {
    use shader_recompiler::exception::{
        InvalidArgument, LogicError, NotImplementedException, RuntimeError, ShaderException,
    };

    if let Some(error) = payload.downcast_ref::<ShaderException>() {
        Some(error.to_string())
    } else if let Some(error) = payload.downcast_ref::<LogicError>() {
        Some(error.to_string())
    } else if let Some(error) = payload.downcast_ref::<RuntimeError>() {
        Some(error.to_string())
    } else if let Some(error) = payload.downcast_ref::<NotImplementedException>() {
        Some(error.to_string())
    } else {
        payload
            .downcast_ref::<InvalidArgument>()
            .map(ToString::to_string)
    }
}

/// Rust equivalent of the typed shader-exception catches in
/// `vk_pipeline_cache.cpp`. Non-shader panics are resumed unchanged.
pub(super) fn catch_shader_exception<F, T>(f: F) -> Result<T, String>
where
    F: FnOnce() -> T,
{
    SHADER_EXCEPTION_HOOK_INSTALL.call_once(|| {
        let previous = take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let is_shader_exception = shader_exception_message(info.payload()).is_some();
            if !IN_SHADER_EXCEPTION_SCOPE.with(std::cell::Cell::get) || !is_shader_exception {
                previous(info);
            }
        }));
    });

    IN_SHADER_EXCEPTION_SCOPE.with(|flag| flag.set(true));
    let result = catch_unwind(AssertUnwindSafe(f));
    IN_SHADER_EXCEPTION_SCOPE.with(|flag| flag.set(false));
    match result {
        Ok(value) => Ok(value),
        Err(payload) => match shader_exception_message(payload.as_ref()) {
            Some(message) => Err(message),
            None => resume_unwind(payload),
        },
    }
}

/// Error-path half of upstream `PipelineCache::CreateGraphicsPipeline`.
///
/// Upstream dumps every active environment after a `Shader::Exception`, even
/// when normal shader dumping is disabled. `GraphicsEnvironments::envs` is
/// indexed by the Maxwell program slot, so the direct stage/hash association
/// is preserved here.
fn dump_failed_graphics_environments(
    environments: &mut GraphicsEnvironments,
    key: &GraphicsPipelineKey,
    pipeline_hash: u64,
) {
    for (stage, shader_hash) in key.unique_hashes.iter().copied().enumerate() {
        if shader_hash == 0
            || environments
                .env_ptrs
                .iter()
                .flatten()
                .all(|&index| index != stage)
        {
            continue;
        }
        environments.envs[stage]
            .generic_environment_mut()
            .dump(pipeline_hash, shader_hash);
    }
}

fn maxwell_to_output_topology(topology: PrimitiveTopology) -> OutputTopology {
    match topology {
        PrimitiveTopology::Points => OutputTopology::PointList,
        PrimitiveTopology::LineStrip => OutputTopology::LineStrip,
        _ => OutputTopology::TriangleStrip,
    }
}

fn cast_attribute_type(attribute: VertexAttribute) -> AttributeType {
    if !attribute.is_enabled() {
        return AttributeType::Disabled;
    }
    match VertexAttribType::from_raw(attribute.attrib_type()) {
        VertexAttribType::Invalid => AttributeType::Disabled,
        VertexAttribType::SNorm | VertexAttribType::UNorm | VertexAttribType::Float => {
            AttributeType::Float
        }
        VertexAttribType::SInt => AttributeType::SignedInt,
        VertexAttribType::UInt => AttributeType::UnsignedInt,
        VertexAttribType::UScaled => AttributeType::UnsignedScaled,
        VertexAttribType::SScaled => AttributeType::SignedScaled,
    }
}

fn attribute_type(state: &FixedPipelineState, index: usize) -> AttributeType {
    match state.dynamic_attribute_type(index) {
        0 => AttributeType::Disabled,
        1 => AttributeType::Float,
        2 => AttributeType::SignedInt,
        3 => AttributeType::UnsignedInt,
        _ => AttributeType::Disabled,
    }
}

fn maxwell_to_compare_function(op: ComparisonOp) -> CompareFunction {
    match op {
        ComparisonOp::Never => CompareFunction::Never,
        ComparisonOp::Less => CompareFunction::Less,
        ComparisonOp::Equal => CompareFunction::Equal,
        ComparisonOp::LessEqual => CompareFunction::LessThanEqual,
        ComparisonOp::Greater => CompareFunction::Greater,
        ComparisonOp::NotEqual => CompareFunction::NotEqual,
        ComparisonOp::GreaterEqual => CompareFunction::GreaterThanEqual,
        ComparisonOp::Always => CompareFunction::Always,
    }
}

fn fill_transform_feedback_runtime_info(info: &mut RuntimeInfo, state: &FixedPipelineState) {
    let (varyings, count) =
        crate::transform_feedback::make_transform_feedback_varyings(&state.xfb_state);
    info.xfb_varyings = varyings;
    info.xfb_count = count;
}

/// Port of `MakeRuntimeInfo` in Eden `vk_pipeline_cache.cpp`.
#[derive(Clone, Copy)]
pub(crate) struct RuntimeInfoDeviceFeatures {
    pub(crate) transform_feedback: bool,
    pub(crate) molten_vk: bool,
}

fn make_runtime_info_with_features(
    programs: &[Option<Program>; NUM_PROGRAMS],
    key: &GraphicsPipelineKey,
    program: &Program,
    previous_program: Option<&Program>,
    device_features: RuntimeInfoDeviceFeatures,
) -> RuntimeInfo {
    let state = &key.fixed_state;
    let mut info = RuntimeInfo::default();
    if let Some(previous_program) = previous_program {
        info.previous_stage_stores = previous_program.info.stores.clone();
        info.previous_stage_legacy_stores_mapping =
            previous_program.info.legacy_stores_mapping.clone();
        if previous_program.is_geometry_passthrough {
            for (stores, passthrough) in info
                .previous_stage_stores
                .mask
                .iter_mut()
                .zip(previous_program.info.passthrough.mask)
            {
                *stores |= passthrough;
            }
        }
    } else {
        info.previous_stage_stores.mask.fill(u64::MAX);
    }

    let has_geometry = key.unique_hashes[4] != 0
        && programs[4]
            .as_ref()
            .is_some_and(|geometry| !geometry.is_geometry_passthrough);
    let point_size = f32::from_bits(state.point_size);
    match program.stage {
        ShaderStage::VertexB => {
            if !has_geometry {
                if state.topology() == PrimitiveTopology::Points {
                    info.fixed_state_point_size = Some(point_size);
                }
                if state.xfb_enabled() {
                    if device_features.transform_feedback {
                        fill_transform_feedback_runtime_info(&mut info, state);
                    } else {
                        log::warn!(
                            "XFB requested in pipeline key but device lacks VK_EXT_transform_feedback; ignoring XFB decorations"
                        );
                    }
                }
                info.convert_depth_mode = state.ndc_minus_one_to_one();
            }
            for (index, fixed_attribute) in state.attributes.iter().copied().enumerate() {
                info.generic_input_types[index] = if state.dynamic_vertex_input() {
                    attribute_type(state, index)
                } else {
                    cast_attribute_type(fixed_attribute)
                };
            }
        }
        ShaderStage::TessellationEval => {
            info.tess_clockwise = state.tessellation_clockwise();
            info.tess_primitive = match state.tessellation_primitive() {
                0 => TessPrimitive::Isolines,
                1 => TessPrimitive::Triangles,
                2 => TessPrimitive::Quads,
                _ => TessPrimitive::Triangles,
            };
            info.tess_spacing = match state.tessellation_spacing() {
                0 => TessSpacing::Equal,
                1 => TessSpacing::FractionalOdd,
                2 => TessSpacing::FractionalEven,
                _ => TessSpacing::Equal,
            };
        }
        ShaderStage::Geometry => {
            if program.output_topology == OutputTopology::PointList {
                info.fixed_state_point_size = Some(point_size);
            }
            if state.xfb_enabled() {
                if device_features.transform_feedback {
                    fill_transform_feedback_runtime_info(&mut info, state);
                } else {
                    log::warn!(
                        "XFB requested in pipeline key but device lacks VK_EXT_transform_feedback; ignoring XFB decorations"
                    );
                }
            }
            info.convert_depth_mode = state.ndc_minus_one_to_one();
        }
        ShaderStage::Fragment => {
            info.alpha_test_func = Some(maxwell_to_compare_function(state.alpha_test_func()));
            info.alpha_test_reference = f32::from_bits(state.alpha_test_ref);
            info.dual_source_blend = state.attachment0_dual_source_blend();
            if device_features.molten_vk {
                for (index, &format) in state.color_formats.iter().enumerate() {
                    if format == 0 {
                        info.frag_color_types[index] = AttributeType::Float;
                        continue;
                    }
                    let pixel_format = pixel_format_from_render_target_format(format as u32);
                    info.frag_color_types[index] = if is_pixel_format_signed_integer(pixel_format) {
                        AttributeType::SignedInt
                    } else if is_pixel_format_integer(pixel_format) {
                        AttributeType::UnsignedInt
                    } else {
                        AttributeType::Float
                    };
                }
            }
        }
        _ => {}
    }

    info.input_topology = match state.topology() {
        PrimitiveTopology::Points => InputTopology::Points,
        PrimitiveTopology::Lines | PrimitiveTopology::LineLoop | PrimitiveTopology::LineStrip => {
            InputTopology::Lines
        }
        PrimitiveTopology::LinesAdjacency | PrimitiveTopology::LineStripAdjacency => {
            InputTopology::LinesAdjacency
        }
        PrimitiveTopology::TrianglesAdjacency | PrimitiveTopology::TriangleStripAdjacency => {
            InputTopology::TrianglesAdjacency
        }
        PrimitiveTopology::Triangles
        | PrimitiveTopology::TriangleStrip
        | PrimitiveTopology::TriangleFan
        | PrimitiveTopology::Quads
        | PrimitiveTopology::QuadStrip
        | PrimitiveTopology::Polygon
        | PrimitiveTopology::Patches => InputTopology::Triangles,
        _ => InputTopology::Points,
    };
    info.force_early_z = state.early_z();
    info.y_negate = state.y_negate();
    info
}

fn make_runtime_info(
    programs: &[Option<Program>; NUM_PROGRAMS],
    key: &GraphicsPipelineKey,
    program: &Program,
    previous_program: Option<&Program>,
    device: &Device,
) -> RuntimeInfo {
    make_runtime_info_with_features(
        programs,
        key,
        program,
        previous_program,
        RuntimeInfoDeviceFeatures {
            transform_feedback: device.is_ext_transform_feedback_supported(),
            molten_vk: device.is_molten_vk(),
        },
    )
}

fn graphics_environment_is_present(
    environments: &GraphicsEnvironments,
    program_index: usize,
) -> bool {
    environments
        .env_ptrs
        .iter()
        .flatten()
        .any(|&index| index == program_index)
}

/// Port of the translation and SPIR-V-emission body of
/// `PipelineCache::CreateGraphicsPipeline`.
pub(super) fn compile_graphics_stages_from_environments(
    device: &Device,
    profile: &Profile,
    host_info: &HostTranslateInfo,
    key: &GraphicsPipelineKey,
    environments: &mut GraphicsEnvironments,
) -> Option<[Option<CompiledShader>; 5]> {
    compile_graphics_stages_from_environments_with_features(
        profile,
        host_info,
        key,
        environments,
        RuntimeInfoDeviceFeatures {
            transform_feedback: device.is_ext_transform_feedback_supported(),
            molten_vk: device.is_molten_vk(),
        },
    )
}

pub(crate) fn compile_graphics_stages_from_environments_with_features(
    profile: &Profile,
    host_info: &HostTranslateInfo,
    key: &GraphicsPipelineKey,
    environments: &mut GraphicsEnvironments,
    device_features: RuntimeInfoDeviceFeatures,
) -> Option<[Option<CompiledShader>; 5]> {
    let uses_vertex_a = key.unique_hashes[0] != 0;
    let uses_vertex_b = key.unique_hashes[1] != 0;
    if !uses_vertex_b {
        return None;
    }

    let pipeline_hash = key.hash_value();
    let dump_guest_shaders = *common::settings::values().dump_guest_shaders.get_value();
    let mut programs: [Option<Program>; NUM_PROGRAMS] = std::array::from_fn(|_| None);
    let mut layer_source_program: Option<usize> = None;

    for program_index in 0..NUM_PROGRAMS {
        let is_emulated_stage = layer_source_program.is_some() && program_index == 4;
        if key.unique_hashes[program_index] == 0 && is_emulated_stage {
            let source = programs[layer_source_program?].as_ref()?.clone();
            programs[program_index] = Some(generate_geometry_passthrough(
                host_info,
                &source,
                maxwell_to_output_topology(key.fixed_state.topology()),
            ));
            continue;
        }
        if key.unique_hashes[program_index] == 0
            || !graphics_environment_is_present(environments, program_index)
        {
            continue;
        }

        let env = &mut environments.envs[program_index];
        if env.generic_environment().cached_code_slice().is_empty()
            && env.generic_environment_mut().analyze().is_none()
        {
            return None;
        }
        let code = env
            .generic_environment()
            .cached_instruction_slice()
            .to_vec();
        if code.is_empty() {
            return None;
        }
        let cfg_offset = env.generic_environment().cached_instruction_start();
        let mut program =
            shader_recompiler::pipeline_cache::translate_program_from_env_with_host_info(
                &code, cfg_offset, env, host_info,
            );
        if uses_vertex_a && program_index == 1 {
            let mut vertex_a = programs[0].take()?;
            program = merge_dual_vertex_programs(&mut vertex_a, &mut program, env);
        }
        if dump_guest_shaders {
            env.generic_environment_mut()
                .dump(pipeline_hash, key.unique_hashes[program_index]);
        }
        if program.info.requires_layer_emulation {
            layer_source_program = Some(program_index);
        }
        programs[program_index] = Some(program);
    }

    let mut bindings = Bindings::default();
    let mut compiled_stages: [Option<CompiledShader>; 5] = std::array::from_fn(|_| None);
    let mut previous_stage_index: Option<usize> = None;
    let first_program = if uses_vertex_a && uses_vertex_b { 1 } else { 0 };
    for program_index in first_program..NUM_PROGRAMS {
        let is_emulated_stage = layer_source_program.is_some() && program_index == 4;
        if key.unique_hashes[program_index] == 0 && !is_emulated_stage {
            continue;
        }
        if program_index == 0 {
            return None;
        }

        let runtime_info = {
            let program = programs[program_index].as_ref()?;
            let previous_program = previous_stage_index.and_then(|index| programs[index].as_ref());
            make_runtime_info_with_features(
                &programs,
                key,
                program,
                previous_program,
                device_features,
            )
        };
        let program = programs[program_index].as_mut()?;
        convert_legacy_to_generic(program, &runtime_info);
        let spirv_words = shader_recompiler::backend::emit_spirv_with_bindings(
            program,
            profile,
            &runtime_info,
            &mut bindings,
        );
        compiled_stages[program_index - 1] = Some(CompiledShader {
            spirv_words,
            info: program.info.clone(),
            stage: program.stage,
        });
        previous_stage_index = Some(program_index);
    }
    compiled_stages[0].as_ref()?;
    Some(compiled_stages)
}

fn shader_stage_for_program(program_index: usize) -> Option<ShaderStage> {
    match program_index {
        0 => Some(ShaderStage::VertexA),
        1 => Some(ShaderStage::VertexB),
        2 => Some(ShaderStage::TessellationControl),
        3 => Some(ShaderStage::TessellationEval),
        4 => Some(ShaderStage::Geometry),
        5 => Some(ShaderStage::Fragment),
        _ => None,
    }
}

fn two_mut<T>(slice: &mut [T], lhs: usize, rhs: usize) -> Option<(&mut T, &mut T)> {
    if lhs == rhs || lhs >= slice.len() || rhs >= slice.len() {
        return None;
    }
    if lhs < rhs {
        let (left, right) = slice.split_at_mut(rhs);
        Some((&mut left[lhs], &mut right[0]))
    } else {
        let (left, right) = slice.split_at_mut(lhs);
        Some((&mut right[0], &mut left[rhs]))
    }
}

/// Disk-environment entry point for the same upstream
/// `PipelineCache::CreateGraphicsPipeline` body.
pub(super) fn compile_graphics_stages_from_file_environments(
    device: &Device,
    profile: &Profile,
    host_info: &HostTranslateInfo,
    key: &GraphicsPipelineKey,
    environments: &mut [FileEnvironment],
) -> Option<[Option<CompiledShader>; 5]> {
    let uses_vertex_a = key.unique_hashes[0] != 0;
    let uses_vertex_b = key.unique_hashes[1] != 0;
    if !uses_vertex_b {
        return None;
    }

    let pipeline_hash = key.hash_value();
    let dump_guest_shaders = *common::settings::values().dump_guest_shaders.get_value();
    let mut programs: [Option<Program>; NUM_PROGRAMS] = std::array::from_fn(|_| None);
    let mut layer_source_program: Option<usize> = None;

    for program_index in 0..NUM_PROGRAMS {
        let is_emulated_stage = layer_source_program.is_some() && program_index == 4;
        if key.unique_hashes[program_index] == 0 && is_emulated_stage {
            let source = programs[layer_source_program?].as_ref()?.clone();
            programs[program_index] = Some(generate_geometry_passthrough(
                host_info,
                &source,
                maxwell_to_output_topology(key.fixed_state.topology()),
            ));
            continue;
        }
        if key.unique_hashes[program_index] == 0 {
            continue;
        }
        let stage = shader_stage_for_program(program_index)?;
        let env_index = environments
            .iter()
            .position(|environment| environment.shader_stage() == stage)?;
        let env = &mut environments[env_index];
        let code = env.cached_instruction_slice().to_vec();
        if code.is_empty() {
            return None;
        }
        let cfg_offset = env.cached_instruction_start();
        let mut program =
            shader_recompiler::pipeline_cache::translate_program_from_env_with_host_info(
                &code, cfg_offset, env, host_info,
            );
        if uses_vertex_a && program_index == 1 {
            let vertex_a_index = environments
                .iter()
                .position(|environment| environment.shader_stage() == ShaderStage::VertexA)?;
            let vertex_b_index = env_index;
            let (_, vertex_b) = two_mut(environments, vertex_a_index, vertex_b_index)?;
            let mut vertex_a = programs[0].take()?;
            program = merge_dual_vertex_programs(&mut vertex_a, &mut program, vertex_b);
        }
        if dump_guest_shaders {
            environments[env_index].dump(pipeline_hash, key.unique_hashes[program_index]);
        }
        if program.info.requires_layer_emulation {
            layer_source_program = Some(program_index);
        }
        programs[program_index] = Some(program);
    }

    let mut bindings = Bindings::default();
    let mut compiled_stages: [Option<CompiledShader>; 5] = std::array::from_fn(|_| None);
    let mut previous_stage_index: Option<usize> = None;
    let first_program = if uses_vertex_a && uses_vertex_b { 1 } else { 0 };
    for program_index in first_program..NUM_PROGRAMS {
        let is_emulated_stage = layer_source_program.is_some() && program_index == 4;
        if key.unique_hashes[program_index] == 0 && !is_emulated_stage {
            continue;
        }
        if program_index == 0 {
            return None;
        }
        let runtime_info = {
            let program = programs[program_index].as_ref()?;
            let previous_program = previous_stage_index.and_then(|index| programs[index].as_ref());
            make_runtime_info(&programs, key, program, previous_program, device)
        };
        let program = programs[program_index].as_mut()?;
        convert_legacy_to_generic(program, &runtime_info);
        let spirv_words = shader_recompiler::backend::emit_spirv_with_bindings(
            program,
            profile,
            &runtime_info,
            &mut bindings,
        );
        compiled_stages[program_index - 1] = Some(CompiledShader {
            spirv_words,
            info: program.info.clone(),
            stage: program.stage,
        });
        previous_stage_index = Some(program_index);
    }
    compiled_stages[0].as_ref()?;
    Some(compiled_stages)
}

fn stage_infos_from_compiled(compiled_stages: &[Option<CompiledShader>; 5]) -> [ShaderInfo; 5] {
    std::array::from_fn(|index| {
        compiled_stages[index]
            .as_ref()
            .map(|compiled| compiled.info.clone())
            .unwrap_or_default()
    })
}

/// Rust lifetime adapter for the state captured by Eden
/// `PipelineCache::CreateGraphicsPipeline` worker jobs. Translation and shader
/// module creation remain owned by this file.
struct GraphicsPipelineBuilder {
    device_owner: DeviceReference,
    profile: Profile,
    host_info: HostTranslateInfo,
}

impl GraphicsPipelineBuilder {
    fn new(device: &Device, profile: Profile, host_info: HostTranslateInfo) -> Self {
        Self {
            device_owner: DeviceReference::new(device),
            profile,
            host_info,
        }
    }

    fn clone_for_disk_worker(&self) -> Self {
        Self {
            device_owner: self.device_owner,
            profile: self.profile.clone(),
            host_info: self.host_info.clone(),
        }
    }

    fn build_from_environments(
        &mut self,
        pipeline_cache: vk::PipelineCache,
        shader_notify: crate::shader_notify::ShaderNotifyHandle,
        environments: &mut GraphicsEnvironments,
        key: &GraphicsPipelineKey,
        worker: &ThreadWorker,
        runtime: GraphicsPipelineRuntime,
        pipeline_statistics: Option<Arc<PipelineStatistics>>,
    ) -> Option<GraphicsPipeline> {
        let compiled_stages = compile_graphics_stages_from_environments(
            self.device_owner.get(),
            &self.profile,
            &self.host_info,
            key,
            environments,
        )?;
        self.build_pipeline(
            pipeline_cache,
            shader_notify,
            key,
            compiled_stages,
            Some(worker),
            runtime,
            pipeline_statistics,
        )
    }

    fn build_from_file_environments(
        &mut self,
        pipeline_cache: vk::PipelineCache,
        shader_notify: crate::shader_notify::ShaderNotifyHandle,
        environments: &mut [FileEnvironment],
        key: &GraphicsPipelineKey,
        runtime: GraphicsPipelineRuntime,
        pipeline_statistics: Option<Arc<PipelineStatistics>>,
    ) -> Option<GraphicsPipeline> {
        match catch_shader_exception(|| {
            let compiled_stages = compile_graphics_stages_from_file_environments(
                self.device_owner.get(),
                &self.profile,
                &self.host_info,
                key,
                environments,
            )?;
            self.build_pipeline(
                pipeline_cache,
                shader_notify,
                key,
                compiled_stages,
                None,
                runtime,
                pipeline_statistics,
            )
        }) {
            Ok(pipeline) => pipeline,
            Err(reason) => {
                log::error!(
                    "Skipping cached graphics pipeline 0x{:016X}: {}",
                    key.hash_value(),
                    reason
                );
                None
            }
        }
    }

    fn build_pipeline(
        &self,
        pipeline_cache: vk::PipelineCache,
        shader_notify: crate::shader_notify::ShaderNotifyHandle,
        key: &GraphicsPipelineKey,
        compiled_stages: [Option<CompiledShader>; 5],
        worker: Option<&ThreadWorker>,
        runtime: GraphicsPipelineRuntime,
        pipeline_statistics: Option<Arc<PipelineStatistics>>,
    ) -> Option<GraphicsPipeline> {
        let shader_modules = self.create_shader_modules(key, &compiled_stages)?;
        let pipeline = match GraphicsPipeline::new_unbuilt(
            self.device_owner,
            pipeline_cache,
            shader_notify,
            key,
            stage_infos_from_compiled(&compiled_stages),
            shader_modules,
            false,
            runtime,
        ) {
            Some(pipeline) => pipeline,
            None => {
                self.destroy_shader_modules(shader_modules);
                return None;
            }
        };
        if let Some(worker) = worker {
            pipeline.queue_make_pipeline(worker, runtime, shader_notify, pipeline_statistics);
        } else {
            pipeline.finish_build_sync(
                unsafe { runtime.render_pass_cache() },
                pipeline_statistics.as_deref(),
            );
            shader_notify.mark_shader_complete();
        }
        Some(pipeline)
    }

    fn create_shader_modules(
        &self,
        key: &GraphicsPipelineKey,
        compiled_stages: &[Option<CompiledShader>; 5],
    ) -> Option<[vk::ShaderModule; 5]> {
        let mut modules = [vk::ShaderModule::null(); 5];
        for (index, compiled) in compiled_stages.iter().enumerate() {
            let Some(compiled) = compiled else {
                continue;
            };
            self.device_owner.get().save_shader(&compiled.spirv_words);
            let create_info = vk::ShaderModuleCreateInfo::builder()
                .code(&compiled.spirv_words)
                .build();
            modules[index] = match unsafe {
                self.device_owner
                    .get()
                    .get_logical()
                    .create_shader_module(&create_info, None)
            } {
                Ok(module) => module,
                Err(error) => {
                    log::warn!("Failed to create graphics shader module: {error:?}");
                    self.destroy_shader_modules(modules);
                    return None;
                }
            };
            let unique_hash = key.unique_hashes[index + 1];
            let should_log = is_active();
            let should_dump = *common::settings::values().gpu_log_shader_dumps.get_value();
            if should_log || should_dump {
                let shader_name = shader_log_name(unique_hash, get_shader_stage_name(index));
                if should_log {
                    let shader_info = shader_log_info(unique_hash, compiled.spirv_words.len());
                    get_instance().log_shader_compilation(&shader_name, &shader_info);
                }
                if should_dump {
                    dump_spirv_shader(unique_hash, &compiled.spirv_words);
                }
            }
            if self.device_owner.get().has_debugging_tool_attached() {
                self.device_owner
                    .get()
                    .set_shader_module_name(modules[index], &format!("Shader {unique_hash:016x}"));
            }
        }
        Some(modules)
    }

    fn destroy_shader_modules(&self, modules: [vk::ShaderModule; 5]) {
        for module in modules {
            if module != vk::ShaderModule::null() {
                unsafe {
                    self.device_owner
                        .get()
                        .get_logical()
                        .destroy_shader_module(module, None);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ComputePipelineCacheKey
// ---------------------------------------------------------------------------

/// Port of `ComputePipelineCacheKey`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct ComputePipelineCacheKey {
    pub unique_hash: u64,
    pub shared_memory_size: u32,
    pub workgroup_size: [u32; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurrentComputePipeline {
    pipeline_owner: NonNull<ComputePipeline>,
}

impl CurrentComputePipeline {
    /// Stable cache-owned pipeline counterpart of upstream's returned pointer.
    ///
    /// The cache must not be mutated while this reference is used.
    pub unsafe fn owner_mut(&mut self) -> &mut ComputePipeline {
        self.pipeline_owner.as_mut()
    }
}

enum DiskPipelineBuildResult {
    Compute(ComputePipelineCacheKey, ComputePipeline),
    Graphics(GraphicsPipelineKey, GraphicsPipeline),
}

#[derive(Default)]
struct DiskResourceLoadState {
    total: usize,
    built: usize,
    has_loaded: bool,
}

impl DiskResourceLoadState {
    fn complete_one(&mut self, callback: &DiskResourceLoadCallback) {
        self.built += 1;
        if self.has_loaded {
            callback(LoadCallbackStage::Build, self.built, self.total);
        }
    }
}

impl ComputePipelineCacheKey {
    /// Port of `ComputePipelineCacheKey::Hash`.
    ///
    /// Computes the upstream-style CityHash64 over the raw key bytes.
    pub fn hash_value(&self) -> u64 {
        let bytes = unsafe {
            std::slice::from_raw_parts(
                (self as *const Self).cast::<u8>(),
                std::mem::size_of::<Self>(),
            )
        };
        city_hash64(bytes)
    }

    pub fn read_from_file(file: &mut std::fs::File) -> std::io::Result<Self> {
        use std::io::Read;
        let mut buf8 = [0u8; 8];
        let mut buf4 = [0u8; 4];
        file.read_exact(&mut buf8)?;
        let unique_hash = u64::from_le_bytes(buf8);
        file.read_exact(&mut buf4)?;
        let shared_memory_size = u32::from_le_bytes(buf4);
        let mut workgroup_size = [0u32; 3];
        for value in &mut workgroup_size {
            file.read_exact(&mut buf4)?;
            *value = u32::from_le_bytes(buf4);
        }
        Ok(Self {
            unique_hash,
            shared_memory_size,
            workgroup_size,
        })
    }
}

impl GraphicsPipelineKey {
    /// Port of `GraphicsPipelineCacheKey::Hash` from
    /// `vk_pipeline_cache.cpp`.
    ///
    /// Upstream CityHash64 hashes the six shader hashes followed by exactly
    /// `FixedPipelineState::Size()` bytes. The fixed stack buffer avoids the
    /// two heap allocations previously hidden in `to_cache_bytes()`.
    pub fn hash_value(&self) -> u64 {
        const HASH_BYTES: usize =
            std::mem::size_of::<[u64; NUM_PROGRAMS]>() + FixedPipelineState::FULL_SIZE;
        let mut bytes = [0u8; HASH_BYTES];
        let mut offset = 0usize;
        for unique_hash in self.unique_hashes {
            let end = offset + std::mem::size_of::<u64>();
            bytes[offset..end].copy_from_slice(&unique_hash.to_le_bytes());
            offset = end;
        }
        let (state_bytes, state_size) = self.fixed_state.prefix_bytes();
        bytes[offset..offset + state_size].copy_from_slice(&state_bytes[..state_size]);
        city_hash64(&bytes[..offset + state_size])
    }
}

impl Hash for ComputePipelineCacheKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.hash_value());
    }
}

impl Hash for GraphicsPipelineKey {
    /// Port of `std::hash<Vulkan::GraphicsPipelineCacheKey>` calling
    /// `GraphicsPipelineCacheKey::Hash()`.
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.hash_value());
    }
}

// ---------------------------------------------------------------------------
// ShaderPools
// ---------------------------------------------------------------------------

/// Port of `ShaderPools` struct.
///
/// Object pools for IR instructions, blocks, and flow blocks.
pub struct ShaderPools {
    pub inst: ObjectPool<Inst>,
    pub block: ObjectPool<Block>,
    pub flow_block: ObjectPool<FlowBlock>,
}

impl ShaderPools {
    pub fn new() -> Self {
        Self {
            inst: ObjectPool::new(8192),
            block: ObjectPool::new(32),
            flow_block: ObjectPool::new(32),
        }
    }

    /// Port of `ShaderPools::ReleaseContents`.
    pub fn release_contents(&mut self) {
        self.flow_block.release_contents();
        self.block.release_contents();
        self.inst.release_contents();
    }
}

impl Default for ShaderPools {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// PipelineCache
// ---------------------------------------------------------------------------

/// Vulkan pipeline cache version for disk serialization.
// Version 12: FixedPipelineState vertex-attribute type/size bits switched
// from Rust enum ordinals to raw Maxwell hardware encodings (matching
// upstream); older caches carry corrupted attribute state.
// Version 13: FixedPipelineState::refresh now leaves fields covered by a
// supported dynamic-state extension at zero (upstream semantics). Older
// caches carry per-draw dynamic state baked into keys — thousands of
// duplicate pipelines per logical key that can never match again.
// Version 14: Maxwell sched-control decoding is anchored at the shader code
// start. Version 13 caches may contain environments captured with the old
// absolute sched grid and therefore rebuild invalid or mismatched pipelines.
// Version 15: FixedPipelineState::refresh preserves color write masks even
// when blending is disabled. Older caches reconstruct pipelines with a zero
// colorWriteMask and can render an entirely black frame.
// Version 16: draw snapshots preserve all 32 vertex binding/attribute slots
// and FixedPipelineState records instance divisors. Version 15 entries can
// contain renumbered sparse attributes and zero divisors.
// Version 17: vertex strides are omitted from fixed pipeline state whenever
// extended dynamic state owns them, matching upstream. Version 16 entries
// can contain per-draw strides and therefore produce duplicate pipelines.
const CACHE_VERSION: u32 = 18;
const VULKAN_CACHE_MAGIC_NUMBER: [u8; 8] = *b"yuzuvkch";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VulkanPipelineCacheHeaderError {
    TooSmall,
    InvalidMagic,
    VersionMismatch,
}

fn parse_vulkan_pipeline_cache_blob(
    data: &[u8],
    expected_cache_version: u32,
) -> Result<&[u8], VulkanPipelineCacheHeaderError> {
    let header_size = VULKAN_CACHE_MAGIC_NUMBER.len() + std::mem::size_of::<u32>();
    if data.len() < header_size {
        return Err(VulkanPipelineCacheHeaderError::TooSmall);
    }

    let magic_number = &data[..VULKAN_CACHE_MAGIC_NUMBER.len()];
    if magic_number != VULKAN_CACHE_MAGIC_NUMBER {
        return Err(VulkanPipelineCacheHeaderError::InvalidMagic);
    }

    let version_offset = VULKAN_CACHE_MAGIC_NUMBER.len();
    let version = u32::from_le_bytes([
        data[version_offset],
        data[version_offset + 1],
        data[version_offset + 2],
        data[version_offset + 3],
    ]);
    if version != expected_cache_version {
        return Err(VulkanPipelineCacheHeaderError::VersionMismatch);
    }

    Ok(&data[header_size..])
}

fn should_allow_unbuilt_graphics_pipeline(
    use_asynchronous_shaders: bool,
    index_buffer_count: u32,
    vertex_count: u32,
) -> bool {
    if !use_asynchronous_shaders {
        return true;
    }
    index_buffer_count <= 6 || vertex_count <= 6
}

fn graphics_key_dynamic_features_match(
    key: &GraphicsPipelineKey,
    features: &DynamicFeatures,
) -> bool {
    let dynamic_features_match = key.fixed_state.extended_dynamic_state()
        == features.has_extended_dynamic_state
        && key.fixed_state.extended_dynamic_state_2() == features.has_extended_dynamic_state_2
        && key.fixed_state.extended_dynamic_state_2_logic_op()
            == features.has_extended_dynamic_state_2_logic_op
        && key.fixed_state.extended_dynamic_state_3_blend()
            == features.has_extended_dynamic_state_3_blend
        && key.fixed_state.extended_dynamic_state_3_enables()
            == features.has_extended_dynamic_state_3_enables
        && key.fixed_state.color_write_enable_dynamic() == features.has_color_write_enable
        && key.fixed_state.dynamic_vertex_input() == features.has_dynamic_vertex_input;
    let requests_provoking_last = key.fixed_state.provoking_vertex_last();
    let transform_feedback_preserves_provoking = !key.fixed_state.xfb_enabled()
        || !requests_provoking_last
        || features.has_provoking_vertex_tf_preserve;
    dynamic_features_match
        && (!requests_provoking_last || features.has_provoking_vertex_last_mode)
        && transform_feedback_preserves_provoking
}

fn graphics_key_cache_hash(key: &GraphicsPipelineKey) -> u64 {
    key.hash_value()
}

fn compute_key_to_cache_bytes(key: &ComputePipelineCacheKey) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(std::mem::size_of::<ComputePipelineCacheKey>());
    bytes.extend_from_slice(&key.unique_hash.to_le_bytes());
    bytes.extend_from_slice(&key.shared_memory_size.to_le_bytes());
    for value in key.workgroup_size {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn shader_log_name(unique_hash: u64, stage_name: &str) -> String {
    format!("shader_{unique_hash:016x}_{stage_name}")
}

fn shader_log_info(unique_hash: u64, spirv_word_count: usize) -> String {
    format!(
        "SPIR-V size: {} bytes, hash: {unique_hash:016x}",
        spirv_word_count * std::mem::size_of::<u32>()
    )
}

/// Compute half of upstream `PipelineCache::CreateComputePipeline` between
/// CFG/environment construction and `BuildShader`.
fn compile_compute_program(
    code: &[u64],
    base_offset: u32,
    env: &mut dyn shader_recompiler::environment::Environment,
    profile: &Profile,
    host_info: &HostTranslateInfo,
    driver_id: vk::DriverId,
    max_shared_memory: u32,
    unique_hash: u64,
) -> shader_recompiler::CompiledShader {
    let runtime_info = RuntimeInfo::default();
    let mut program = shader_recompiler::pipeline_cache::translate_program_from_env_with_host_info(
        code,
        base_offset,
        env,
        host_info,
    );
    let needs_shared_memory_clamp = matches!(
        driver_id,
        vk::DriverId::QUALCOMM_PROPRIETARY | vk::DriverId::ARM_PROPRIETARY
    );
    if needs_shared_memory_clamp && program.shared_memory_size > max_shared_memory {
        log::warn!(
            "Compute shader {unique_hash:#016x} requests {}KB shared memory but device max is {}KB - clamping",
            program.shared_memory_size / 1024,
            max_shared_memory / 1024,
        );
        program.shared_memory_size = max_shared_memory;
    }
    let spirv_words = shader_recompiler::backend::emit_spirv(&program, profile, &runtime_info);
    shader_recompiler::CompiledShader {
        spirv_words,
        info: program.info,
        stage: program.stage,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_compute_pipeline<E>(
    device_ref: DeviceReference,
    profile: &Profile,
    host_info: &HostTranslateInfo,
    vulkan_pipeline_cache: vk::PipelineCache,
    shader_notify: crate::shader_notify::ShaderNotifyHandle,
    worker: Option<&ThreadWorker>,
    runtime: ComputePipelineRuntime,
    key: &ComputePipelineCacheKey,
    env: &mut E,
    code: &[u64],
    base_offset: u32,
    pipeline_statistics: Option<Arc<PipelineStatistics>>,
) -> Option<ComputePipeline>
where
    E: shader_recompiler::environment::Environment,
{
    let vulkan_device = device_ref.get();
    let hash = key.hash_value();
    if vulkan_device.has_broken_compute() {
        log::error!("Skipping {hash:#016x}");
        return None;
    }
    log::info!("{hash:#016x}");
    if *common::settings::values().dump_guest_shaders.get_value() {
        env.dump(hash, key.unique_hash);
    }
    let compiled = match catch_shader_exception(|| {
        compile_compute_program(
            code,
            base_offset,
            env,
            profile,
            host_info,
            vulkan_device.get_driver_id(),
            vulkan_device.get_max_compute_shared_memory_size(),
            key.unique_hash,
        )
    }) {
        Ok(compiled) => compiled,
        Err(reason) => {
            log::error!("{reason}");
            return None;
        }
    };
    vulkan_device.save_shader(&compiled.spirv_words);
    let device = vulkan_device.get_logical().clone();
    let create_info = vk::ShaderModuleCreateInfo::builder()
        .code(&compiled.spirv_words)
        .build();
    let spv_module = unsafe { device.create_shader_module(&create_info, None).ok()? };
    let should_log = is_active();
    let should_dump = *common::settings::values().gpu_log_shader_dumps.get_value();
    if should_log || should_dump {
        let shader_name = shader_log_name(key.unique_hash, "compute");
        if should_log {
            let shader_info = shader_log_info(key.unique_hash, compiled.spirv_words.len());
            get_instance().log_shader_compilation(&shader_name, &shader_info);
        }
        if should_dump {
            dump_spirv_shader(key.unique_hash, &compiled.spirv_words);
        }
    }
    if vulkan_device.has_debugging_tool_attached() {
        vulkan_device
            .set_shader_module_name(spv_module, &format!("Shader {:016x}", key.unique_hash));
    }
    ComputePipeline::new(
        device_ref,
        compiled.info,
        spv_module,
        vulkan_pipeline_cache,
        shader_notify,
        worker,
        runtime,
        key.unique_hash,
        pipeline_statistics,
    )
    .or_else(|| {
        log::warn!(
            "Failed to rebuild cached compute pipeline 0x{:016X}",
            key.unique_hash
        );
        None
    })
}

fn build_compute_pipeline_from_file_environment(
    device_ref: DeviceReference,
    profile: Profile,
    host_info: HostTranslateInfo,
    vulkan_pipeline_cache: vk::PipelineCache,
    shader_notify: crate::shader_notify::ShaderNotifyHandle,
    runtime: ComputePipelineRuntime,
    key: &ComputePipelineCacheKey,
    env: &mut FileEnvironment,
    pipeline_statistics: Option<Arc<PipelineStatistics>>,
) -> Option<ComputePipeline> {
    let code = env.cached_instruction_slice().to_vec();
    let base_offset = env.start_address();
    if code.is_empty() {
        return None;
    }
    build_compute_pipeline(
        device_ref,
        &profile,
        &host_info,
        vulkan_pipeline_cache,
        shader_notify,
        None,
        runtime,
        key,
        env,
        &code,
        base_offset,
        pipeline_statistics,
    )
}

fn pipeline_cache_paths(
    shader_cache_dir: &std::path::Path,
    title_id: u64,
) -> Option<(PathBuf, PathBuf)> {
    if title_id == 0 {
        return None;
    }
    let base_dir = shader_cache_dir.join(format!("{:016x}", title_id));
    Some((
        base_dir.join("vulkan.bin"),
        base_dir.join("vulkan_pipelines.bin"),
    ))
}

/// Port of upstream `GetTotalPipelineWorkers`.
fn get_total_pipeline_workers() -> usize {
    let max_core_threads = std::thread::available_parallelism()
        .map(|threads| threads.get())
        .unwrap_or(2)
        .max(2)
        - 1;

    #[cfg(target_os = "android")]
    {
        const FREE_CORES: usize = 3;
        if max_core_threads <= FREE_CORES {
            1
        } else {
            max_core_threads - FREE_CORES
        }
    }

    #[cfg(not(target_os = "android"))]
    max_core_threads
}

fn get_pipeline_worker_count(has_broken_parallel_shader_compiling: bool) -> usize {
    if has_broken_parallel_shader_compiling {
        1
    } else {
        get_total_pipeline_workers()
    }
}

/// Port of `PipelineCache` class.
///
/// Extends `ShaderCache` to manage Vulkan graphics and compute pipeline
/// objects, with disk serialization support.
pub struct PipelineCache {
    /// Stable non-owning counterpart of upstream `const Device& device`.
    /// `RendererVulkan` boxes the owner and drops this cache first.
    device_owner: DeviceReference,
    device: ash::Device,
    /// Upstream reference retained by this owner. Pipeline runtime bridges
    /// copy the same stable pointer for worker-thread construction.
    #[allow(dead_code)]
    descriptor_pool: NonNull<DescriptorPool>,
    shader_notify: crate::shader_notify::ShaderNotifyHandle,
    use_asynchronous_shaders: bool,
    use_vulkan_pipeline_cache: bool,
    channel_caches: ChannelSetupCaches<ChannelInfo>,
    /// Upstream reference retained by this owner. Graphics runtime bridges
    /// copy the same stable pointer for worker-thread construction.
    #[allow(dead_code)]
    render_pass_cache: NonNull<RenderPassCache>,
    graphics_runtime: GraphicsPipelineRuntime,
    compute_runtime: ComputePipelineRuntime,
    profile: Profile,
    host_info: HostTranslateInfo,
    graphics_pipeline_builder: GraphicsPipelineBuilder,
    // Upstream stores nullable unique_ptr values in a node-stable map. `None`
    // is the negative-cache entry left by a failed creation, and `Box` keeps
    // transition/current pointers stable across HashMap growth.
    graphics_cache:
        HashMap<GraphicsPipelineKey, Option<Rc<GraphicsPipeline>>, BuildUnorderedDenseHasher>,
    graphics_key: GraphicsPipelineKey,
    current_pipeline: Option<Rc<GraphicsPipeline>>,
    dynamic_features: DynamicFeatures,

    main_pools: ShaderPools,

    pipeline_cache_filename: PathBuf,
    vulkan_pipeline_cache_filename: PathBuf,
    vulkan_pipeline_cache: vk::PipelineCache,

    // Upstream's node-based cache keeps returned `ComputePipeline*` stable and
    // retains a null unique_ptr after a failed build. `None` preserves that
    // negative-cache entry; `Box` keeps successful pipelines stable across
    // HashMap growth.
    compute_cache:
        HashMap<ComputePipelineCacheKey, Option<Box<ComputePipeline>>, BuildUnorderedDenseHasher>,
    /// Upstream `Common::ThreadWorker workers`, owned by `PipelineCache`.
    ///
    /// This is the required owner for disk-cache rebuild jobs and asynchronous
    /// `GraphicsPipeline` / `ComputePipeline` creation.
    workers: ThreadWorker,
    /// Upstream `Common::ThreadWorker serialization_thread`.
    serialization_thread: ThreadWorker,
}

impl PipelineCache {
    fn create_vulkan_pipeline_cache(
        &self,
        initial_data: &[u8],
    ) -> Result<vk::PipelineCache, vk::Result> {
        let cache_ci = vk::PipelineCacheCreateInfo::builder()
            .initial_data(initial_data)
            .build();
        unsafe { self.device.create_pipeline_cache(&cache_ci, None) }
    }

    fn create_empty_vulkan_pipeline_cache(&self) -> vk::PipelineCache {
        self.create_vulkan_pipeline_cache(&[])
            .expect("failed to create an empty Vulkan pipeline cache")
    }

    /// Port of `PipelineCache::PipelineCache`.
    pub fn new(
        vulkan_device: &Device,
        descriptor_pool: &mut DescriptorPool,
        shader_notify: crate::shader_notify::ShaderNotifyHandle,
        render_pass_cache: &mut RenderPassCache,
        scheduler: &mut Scheduler,
        buffer_cache: &mut VulkanCommonBufferCache,
        texture_cache: &mut TextureCache,
        guest_descriptor_queue: &mut UpdateDescriptorQueue,
        descriptor_buffer_ring: &mut DescriptorBufferRing,
    ) -> Self {
        let device = vulkan_device.get_logical().clone();
        let profile = make_shader_profile(vulkan_device);
        let host_info = make_host_translate_info(vulkan_device);
        let use_asynchronous_shaders = *common::settings::values()
            .use_asynchronous_shaders
            .get_value();
        let use_vulkan_pipeline_cache = *common::settings::values()
            .use_vulkan_driver_pipeline_cache
            .get_value();
        let has_broken_parallel_shader_compiling =
            vulkan_device.has_broken_parallel_shader_compiling();
        let has_extended_dynamic_state_3_enables =
            vulkan_device.is_ext_extended_dynamic_state3_enables_supported();
        let dynamic_features = DynamicFeatures {
            driver_id: vulkan_device.get_driver_id().as_raw() as u32,
            driver_version: vulkan_device.get_driver_version(),
            has_extended_dynamic_state: vulkan_device.is_ext_extended_dynamic_state_supported(),
            has_extended_dynamic_state_2: vulkan_device.is_ext_extended_dynamic_state2_supported(),
            has_extended_dynamic_state_2_logic_op: vulkan_device
                .is_ext_extended_dynamic_state2_extras_supported(),
            has_extended_dynamic_state_2_patch_control_points: false,
            has_extended_dynamic_state_3_blend: vulkan_device
                .is_ext_extended_dynamic_state3_blending_supported(),
            has_extended_dynamic_state_3_enables,
            has_dynamic_state3_depth_clamp_enable: has_extended_dynamic_state_3_enables
                && vulkan_device.supports_dynamic_state3_depth_clamp_enable(),
            has_dynamic_state3_logic_op_enable: has_extended_dynamic_state_3_enables
                && vulkan_device.supports_dynamic_state3_logic_op_enable(),
            has_dynamic_state3_line_stipple_enable: has_extended_dynamic_state_3_enables
                && vulkan_device.supports_dynamic_state3_line_stipple_enable(),
            has_dynamic_vertex_input: vulkan_device.is_ext_vertex_input_dynamic_state_supported()
                && *common::settings::values()
                    .vertex_input_dynamic_state
                    .get_value(),
            has_color_write_enable: vulkan_device.is_ext_color_write_enable_supported(),
            has_provoking_vertex: vulkan_device.is_ext_provoking_vertex_supported(),
            has_provoking_vertex_first_mode: vulkan_device.supports_provoking_vertex_first_mode(),
            has_provoking_vertex_last_mode: vulkan_device.supports_provoking_vertex_last_mode(),
            has_provoking_vertex_tf_preserve: vulkan_device
                .supports_transform_feedback_provoking_vertex_preservation(),
        };
        if vulkan_device.get_max_vertex_input_attributes() < 32 {
            log::warn!(
                "maxVertexInputAttributes is too low: {} < 32",
                vulkan_device.get_max_vertex_input_attributes()
            );
        }
        if vulkan_device.get_max_vertex_input_bindings() < 32 {
            log::warn!(
                "maxVertexInputBindings is too low: {} < 32",
                vulkan_device.get_max_vertex_input_bindings()
            );
        }
        log::info!(
            "DynamicState setting value: {}",
            *common::settings::values().dyna_state.get_value() as u32
        );

        let pipeline_cache = PipelineCache {
            device_owner: DeviceReference::new(vulkan_device),
            device: device.clone(),
            descriptor_pool: NonNull::from(&mut *descriptor_pool),
            shader_notify,
            use_asynchronous_shaders,
            use_vulkan_pipeline_cache,
            channel_caches: ChannelSetupCaches::new(),
            render_pass_cache: NonNull::from(&mut *render_pass_cache),
            graphics_runtime: GraphicsPipelineRuntime::new(
                &mut *scheduler,
                &mut *buffer_cache,
                &mut *texture_cache,
                &mut *guest_descriptor_queue,
                &mut *descriptor_buffer_ring,
                &mut *descriptor_pool,
                render_pass_cache,
            ),
            compute_runtime: ComputePipelineRuntime::new(
                &mut *scheduler,
                &mut *guest_descriptor_queue,
                &mut *descriptor_buffer_ring,
                &mut *descriptor_pool,
            ),
            profile: profile.clone(),
            host_info: host_info.clone(),
            graphics_pipeline_builder: GraphicsPipelineBuilder::new(
                vulkan_device,
                profile,
                host_info,
            ),
            graphics_cache: HashMap::with_hasher(BuildUnorderedDenseHasher),
            graphics_key: GraphicsPipelineKey::default(),
            current_pipeline: None,
            dynamic_features,
            main_pools: ShaderPools::new(),
            pipeline_cache_filename: PathBuf::new(),
            vulkan_pipeline_cache_filename: PathBuf::new(),
            vulkan_pipeline_cache: vk::PipelineCache::null(),
            compute_cache: HashMap::with_hasher(BuildUnorderedDenseHasher),
            workers: ThreadWorker::new_stateless(
                get_pipeline_worker_count(has_broken_parallel_shader_compiling),
                "VkPipelineBuilder".to_string(),
            ),
            serialization_thread: ThreadWorker::new_stateless(
                1,
                "VkPipelineSerialization".to_string(),
            ),
        };
        pipeline_cache
    }

    /// Port of the Vulkan pipeline-cache owner `CreateChannel` edge.
    pub fn create_channel(&mut self, channel: &ChannelState) {
        self.channel_caches.create_channel(channel);
    }

    /// Port of the Vulkan pipeline-cache owner `BindToChannel` edge.
    pub fn bind_to_channel(&mut self, channel_id: i32) {
        self.channel_caches.bind_to_channel(channel_id);
    }

    /// Port of the Vulkan pipeline-cache owner `EraseChannel` edge.
    pub fn erase_channel(&mut self, channel_id: i32) {
        self.channel_caches.erase_channel(channel_id);
    }

    /// Runtime path matching upstream's pipeline-cache ownership:
    /// shader discovery and unique hashes come from `VideoCommon::ShaderCache`,
    /// while this Vulkan owner builds/caches the VkPipeline.
    pub fn current_graphics_pipeline(
        &mut self,
        draw: &mut Maxwell3DDrawView<'_>,
        shared_cache: &mut SharedShaderCache,
    ) -> Option<&GraphicsPipeline> {
        if !shared_cache.refresh_stages(&mut self.graphics_key.unique_hashes) {
            self.current_pipeline = None;
            return None;
        }
        self.graphics_key
            .fixed_state
            .refresh(draw, &self.dynamic_features);

        let next = self
            .current_pipeline
            .as_ref()
            .and_then(|current| GraphicsPipeline::next(current, &self.graphics_key));
        if let Some(next) = next {
            self.current_pipeline = Some(next);
            let pipeline = self
                .current_pipeline
                .as_deref()
                .expect("current graphics pipeline vanished after transition");
            return self.built_pipeline(pipeline, draw);
        }

        let key = self.graphics_key.clone();
        self.current_graphics_pipeline_slow_path(draw, shared_cache, key)
    }

    /// Port of upstream `PipelineCache::CurrentComputePipeline`.
    pub fn current_compute_pipeline(
        &mut self,
        shared_cache: &mut SharedShaderCache,
    ) -> Option<CurrentComputePipeline> {
        let (shader_hash, shader_size) = {
            let shader = shared_cache.compute_shader()?;
            (shader.unique_hash, shader.size_bytes)
        };
        let kepler_compute = shared_cache.current_kepler_compute()?;
        let qmd = kepler_compute.launch_description();
        let key = ComputePipelineCacheKey {
            unique_hash: shader_hash,
            shared_memory_size: qmd.shared_alloc,
            workgroup_size: [qmd.block_dim_x, qmd.block_dim_y, qmd.block_dim_z],
        };
        if !self.compute_cache.contains_key(&key) {
            self.compute_cache.insert(key, None);
            let gpu_memory = shared_cache.current_gpu_memory()?;
            let mut env = ComputeEnvironment::from_kepler_compute(kepler_compute, gpu_memory);
            env.generic_environment_mut().set_cached_size(shader_size);
            self.main_pools.release_contents();
            let pipeline = self.create_compute_pipeline_from_environment(&key, &mut env)?;
            if !self.pipeline_cache_filename.as_os_str().is_empty() {
                let key_bytes = compute_key_to_cache_bytes(&key);
                let filename = self.pipeline_cache_filename.clone();
                let generic_env = env.generic_environment().clone();
                self.serialization_thread.queue_stateless_work(move || {
                    serialize_pipeline(&key_bytes, &[&generic_env], &filename, CACHE_VERSION);
                });
            }
            *self
                .compute_cache
                .get_mut(&key)
                .expect("new compute cache entry disappeared") = Some(Box::new(pipeline));
        }
        self.compute_cache
            .get_mut(&key)
            .and_then(Option::as_deref_mut)
            .map(|pipeline| CurrentComputePipeline {
                pipeline_owner: NonNull::from(pipeline),
            })
    }

    fn create_compute_pipeline_from_environment(
        &mut self,
        key: &ComputePipelineCacheKey,
        env: &mut ComputeEnvironment,
    ) -> Option<ComputePipeline> {
        let code = env
            .generic_environment()
            .cached_instruction_slice()
            .to_vec();
        let base_offset = env.generic_environment().cached_instruction_start();
        if code.is_empty() {
            return None;
        }
        self.create_compute_pipeline_from_code(key, env, &code, base_offset)
    }

    fn create_compute_pipeline_from_code<E>(
        &mut self,
        key: &ComputePipelineCacheKey,
        env: &mut E,
        code: &[u64],
        base_offset: u32,
    ) -> Option<ComputePipeline>
    where
        E: shader_recompiler::environment::Environment,
    {
        // Eden always builds runtime compute pipelines on the pipeline worker.
        // `use_asynchronous_shaders` controls whether callers wait for the
        // result, not where Vulkan performs the build.
        let worker = Some(&self.workers);
        build_compute_pipeline(
            self.device_owner,
            &self.profile,
            &self.host_info,
            self.vulkan_pipeline_cache,
            self.shader_notify,
            worker,
            self.compute_runtime,
            key,
            env,
            code,
            base_offset,
            None,
        )
    }

    /// Port of `PipelineCache::CurrentGraphicsPipelineSlowPath`.
    fn current_graphics_pipeline_slow_path(
        &mut self,
        draw: &Maxwell3DDrawView<'_>,
        shared_cache: &SharedShaderCache,
        key: GraphicsPipelineKey,
    ) -> Option<&GraphicsPipeline> {
        if !self.graphics_cache.contains_key(&key) {
            let pipeline = self
                .create_graphics_pipeline(shared_cache, &key)
                .map(Rc::new);
            self.graphics_cache.insert(key.clone(), pipeline);
        }

        let pipeline = Rc::clone(self.graphics_cache.get(&key)?.as_ref()?);
        if let Some(current_pipeline) = self.current_pipeline.as_ref() {
            current_pipeline.add_transition(&pipeline);
        }

        self.current_pipeline = Some(pipeline);
        let pipeline = self.current_pipeline.as_deref()?;
        self.built_pipeline(pipeline, draw)
    }

    /// Port of `PipelineCache::BuiltPipeline`.
    fn built_pipeline<'a>(
        &self,
        pipeline: &'a GraphicsPipeline,
        draw: &Maxwell3DDrawView<'_>,
    ) -> Option<&'a GraphicsPipeline> {
        if pipeline.is_built() {
            return Some(pipeline);
        }
        let draw_state = draw.draw_state();
        if should_allow_unbuilt_graphics_pipeline(
            self.use_asynchronous_shaders,
            draw_state.index_buffer.count,
            draw_state.vertex_buffer.count,
        ) {
            return Some(pipeline);
        }
        None
    }

    /// Port of `PipelineCache::CreateGraphicsPipeline`.
    fn create_graphics_pipeline(
        &mut self,
        shared_cache: &SharedShaderCache,
        key: &GraphicsPipelineKey,
    ) -> Option<GraphicsPipeline> {
        self.main_pools.release_contents();
        let pipeline_cache = self.vulkan_pipeline_cache;
        let mut environments = GraphicsEnvironments::default();
        shared_cache.get_graphics_environments(&mut environments, &key.unique_hashes);
        let pipeline = match catch_shader_exception(|| {
            self.graphics_pipeline_builder.build_from_environments(
                pipeline_cache,
                self.shader_notify,
                &mut environments,
                key,
                &self.workers,
                self.graphics_runtime,
                None,
            )
        }) {
            Ok(Some(pipeline)) => pipeline,
            Ok(None) => return None,
            Err(reason) => {
                let pipeline_hash = graphics_key_cache_hash(key);
                dump_failed_graphics_environments(&mut environments, key, pipeline_hash);
                log::error!("{reason}");
                return None;
            }
        };
        if !self.pipeline_cache_filename.as_os_str().is_empty() {
            let key_bytes = key.to_cache_bytes();
            let filename = self.pipeline_cache_filename.clone();
            let envs: Vec<_> = environments.span().into_iter().cloned().collect();
            self.serialization_thread.queue_stateless_work(move || {
                let env_refs: Vec<_> = envs.iter().collect();
                serialize_pipeline(&key_bytes, &env_refs, &filename, CACHE_VERSION);
            });
        }
        Some(pipeline)
    }

    /// Port of `PipelineCache::LoadDiskResources`.
    ///
    /// Loads previously compiled pipelines from disk for the given title.
    pub fn load_disk_resources(
        &mut self,
        title_id: u64,
        pipeline_cache_dir: &std::path::Path,
        stop_loading: DiskResourceLoadStop,
        callback: DiskResourceLoadCallback,
    ) {
        let Some((pipeline_cache_filename, vulkan_pipeline_cache_filename)) =
            pipeline_cache_paths(pipeline_cache_dir, title_id)
        else {
            log::warn!("Skipping Vulkan disk pipeline cache load for title_id=0");
            return;
        };

        let base_dir = pipeline_cache_filename
            .parent()
            .expect("pipeline cache path should have a parent directory");
        if let Err(err) = std::fs::create_dir_all(base_dir) {
            log::error!("Failed to create pipeline cache directories: {}", err);
            return;
        }

        self.pipeline_cache_filename = pipeline_cache_filename;
        self.vulkan_pipeline_cache_filename = vulkan_pipeline_cache_filename;
        log::info!(
            "Loading Vulkan disk pipeline cache title_id={:016X} file={}",
            title_id,
            self.pipeline_cache_filename.display()
        );

        // Load Vulkan pipeline cache from disk if available
        if self.use_vulkan_pipeline_cache {
            self.vulkan_pipeline_cache =
                self.load_vulkan_pipeline_cache(&self.vulkan_pipeline_cache_filename.clone());
        }

        use std::cell::{Cell, RefCell};

        let mut built = 0usize;
        let skipped = Cell::new(0usize);
        let pipeline_statistics = self
            .device_owner
            .get()
            .is_khr_pipeline_executable_properties_enabled()
            .then(|| Arc::new(PipelineStatistics::new(self.device_owner.get())));
        let dynamic_features = self.dynamic_features;
        let loaded_compute: RefCell<Vec<(ComputePipelineCacheKey, FileEnvironment)>> =
            RefCell::new(Vec::new());
        let load_compute = |file: &mut std::fs::File, env: FileEnvironment| {
            let key = ComputePipelineCacheKey::read_from_file(file)?;
            loaded_compute.borrow_mut().push((key, env));
            Ok(())
        };
        let loaded_graphics: RefCell<Vec<(GraphicsPipelineKey, Vec<FileEnvironment>)>> =
            RefCell::new(Vec::new());
        let load_graphics = |file: &mut std::fs::File, envs: Vec<FileEnvironment>| {
            let key = GraphicsPipelineKey::read_from_file(file)?;
            if !graphics_key_dynamic_features_match(&key, &dynamic_features) {
                skipped.set(skipped.get() + 1);
                return Ok(());
            }
            loaded_graphics.borrow_mut().push((key, envs));
            Ok(())
        };
        load_pipelines(
            || stop_loading.load(Ordering::Acquire),
            &self.pipeline_cache_filename,
            CACHE_VERSION,
            Box::new(load_compute),
            Box::new(load_graphics),
        );

        let build_results = Arc::new(Mutex::new(Vec::<DiskPipelineBuildResult>::new()));
        let job_skipped = Arc::new(AtomicUsize::new(0));
        let load_state = Arc::new(Mutex::new(DiskResourceLoadState::default()));
        let mut queued_total = 0usize;

        let loaded_compute = loaded_compute.into_inner();
        for (key, env) in loaded_compute {
            if stop_loading.load(Ordering::Acquire) {
                break;
            }
            if self.compute_cache.contains_key(&key) {
                skipped.set(skipped.get() + 1);
                continue;
            }
            let device_ref = self.device_owner;
            let profile = self.profile.clone();
            let host_info = self.host_info.clone();
            let vulkan_pipeline_cache = self.vulkan_pipeline_cache;
            let shader_notify = self.shader_notify;
            let compute_runtime = self.compute_runtime;
            let results = build_results.clone();
            let skipped_jobs = job_skipped.clone();
            let state = Arc::clone(&load_state);
            let callback = Arc::clone(&callback);
            let statistics = pipeline_statistics.clone();
            self.workers.queue_stateless_work(move || {
                let mut env = env;
                match build_compute_pipeline_from_file_environment(
                    device_ref,
                    profile,
                    host_info,
                    vulkan_pipeline_cache,
                    shader_notify,
                    compute_runtime,
                    &key,
                    &mut env,
                    statistics,
                ) {
                    Some(pipeline) => results
                        .lock()
                        .unwrap()
                        .push(DiskPipelineBuildResult::Compute(key, pipeline)),
                    None => {
                        skipped_jobs.fetch_add(1, Ordering::Relaxed);
                    }
                }
                state.lock().unwrap().complete_one(&callback);
            });
            queued_total += 1;
        }

        let loaded_graphics = loaded_graphics.into_inner();
        for (key, envs) in loaded_graphics {
            if stop_loading.load(Ordering::Acquire) {
                break;
            }
            if self.graphics_cache.contains_key(&key) {
                skipped.set(skipped.get() + 1);
                continue;
            }
            let mut builder = self.graphics_pipeline_builder.clone_for_disk_worker();
            let vulkan_pipeline_cache = self.vulkan_pipeline_cache;
            let shader_notify = self.shader_notify;
            let graphics_runtime = self.graphics_runtime;
            let results = build_results.clone();
            let skipped_jobs = job_skipped.clone();
            let state = Arc::clone(&load_state);
            let callback = Arc::clone(&callback);
            let statistics = pipeline_statistics.clone();
            self.workers.queue_stateless_work(move || {
                let mut envs = envs;
                match builder.build_from_file_environments(
                    vulkan_pipeline_cache,
                    shader_notify,
                    &mut envs,
                    &key,
                    graphics_runtime,
                    statistics,
                ) {
                    Some(pipeline) => results
                        .lock()
                        .unwrap()
                        .push(DiskPipelineBuildResult::Graphics(key, pipeline)),
                    None => {
                        skipped_jobs.fetch_add(1, Ordering::Relaxed);
                    }
                }
                state.lock().unwrap().complete_one(&callback);
            });
            queued_total += 1;
        }

        {
            let mut state = load_state.lock().unwrap();
            state.total = queued_total;
            callback(LoadCallbackStage::Build, 0, state.total);
            state.has_loaded = true;
        }
        self.workers.wait_for_requests_or_stop(&stop_loading);

        let mut skipped_count = skipped.get() + job_skipped.load(Ordering::Relaxed);
        for result in build_results.lock().unwrap().drain(..) {
            match result {
                DiskPipelineBuildResult::Compute(key, pipeline) => {
                    if self.compute_cache.contains_key(&key) {
                        skipped_count += 1;
                    } else {
                        self.compute_cache.insert(key, Some(Box::new(pipeline)));
                        built += 1;
                    }
                }
                DiskPipelineBuildResult::Graphics(key, pipeline) => {
                    if self.graphics_cache.contains_key(&key) {
                        skipped_count += 1;
                    } else {
                        self.graphics_cache.insert(key, Some(Rc::new(pipeline)));
                        built += 1;
                    }
                }
            }
        }
        log::info!(
            "Total Pipeline Count: {} (built={}, skipped={})",
            queued_total,
            built,
            skipped_count
        );

        if self.use_vulkan_pipeline_cache {
            self.serialize_vulkan_pipeline_cache(&self.vulkan_pipeline_cache_filename);
        }
        if let Some(statistics) = pipeline_statistics {
            statistics.report();
        }
    }

    /// Port of `PipelineCache::SerializeVulkanPipelineCache`.
    ///
    /// Serializes the Vulkan pipeline cache to disk.
    pub fn serialize_vulkan_pipeline_cache(&self, filename: &std::path::Path) {
        if self.vulkan_pipeline_cache == vk::PipelineCache::null() {
            log::error!("Refusing to serialize a null Vulkan pipeline cache");
            return;
        }
        let data = unsafe {
            self.device
                .get_pipeline_cache_data(self.vulkan_pipeline_cache)
                .unwrap_or_default()
        };

        let mut output = Vec::with_capacity(VULKAN_CACHE_MAGIC_NUMBER.len() + 4 + data.len());
        output.extend_from_slice(&VULKAN_CACHE_MAGIC_NUMBER);
        output.extend_from_slice(&CACHE_VERSION.to_le_bytes());
        output.extend_from_slice(&data);

        if let Err(e) = std::fs::write(filename, &output) {
            log::error!("Failed to write Vulkan pipeline cache: {}", e);
        }
    }

    /// Port of loading Vulkan pipeline cache from disk.
    fn load_vulkan_pipeline_cache(&self, filename: &std::path::Path) -> vk::PipelineCache {
        let data = match std::fs::read(filename) {
            Ok(data) => data,
            Err(_) => return self.create_empty_vulkan_pipeline_cache(),
        };

        let cache_data = match parse_vulkan_pipeline_cache_blob(&data, CACHE_VERSION) {
            Ok(cache_data) => cache_data,
            Err(VulkanPipelineCacheHeaderError::TooSmall) => {
                let _ = std::fs::remove_file(filename);
                return self.create_empty_vulkan_pipeline_cache();
            }
            Err(VulkanPipelineCacheHeaderError::InvalidMagic) => {
                log::error!("Invalid Vulkan driver pipeline cache file");
                let _ = std::fs::remove_file(filename);
                return self.create_empty_vulkan_pipeline_cache();
            }
            Err(VulkanPipelineCacheHeaderError::VersionMismatch) => {
                log::info!(
                    "Pipeline cache version mismatch (expected {}, got {}), discarding",
                    CACHE_VERSION,
                    u32::from_le_bytes([
                        data[VULKAN_CACHE_MAGIC_NUMBER.len()],
                        data[VULKAN_CACHE_MAGIC_NUMBER.len() + 1],
                        data[VULKAN_CACHE_MAGIC_NUMBER.len() + 2],
                        data[VULKAN_CACHE_MAGIC_NUMBER.len() + 3],
                    ])
                );
                let _ = std::fs::remove_file(filename);
                return self.create_empty_vulkan_pipeline_cache();
            }
        };

        match self.create_vulkan_pipeline_cache(cache_data) {
            Ok(cache) => cache,
            Err(err) => {
                log::warn!(
                    "Vulkan rejected driver pipeline cache data from {}: {}; recreating it empty",
                    filename.display(),
                    err
                );
                if let Err(remove_err) = std::fs::remove_file(filename) {
                    log::warn!(
                        "Failed to remove rejected Vulkan pipeline cache {}: {}",
                        filename.display(),
                        remove_err
                    );
                }
                self.create_empty_vulkan_pipeline_cache()
            }
        }
    }
}

impl Drop for PipelineCache {
    fn drop(&mut self) {
        // Upstream waits for serialization work through ThreadWorker teardown.
        // Do it explicitly before the final driver-cache serialization so a
        // late `SerializePipeline` job cannot race `vulkan_pipelines.bin`.
        self.serialization_thread.wait_for_requests();

        // Save the pipeline cache before destroying.
        if self.use_vulkan_pipeline_cache && self.vulkan_pipeline_cache != vk::PipelineCache::null()
        {
            let filename = self.vulkan_pipeline_cache_filename.clone();
            if !filename.as_os_str().is_empty() {
                self.serialize_vulkan_pipeline_cache(&filename);
            }
            unsafe {
                self.device
                    .destroy_pipeline_cache(self.vulkan_pipeline_cache, None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::const_buffer_info::ConstBufferInfo;
    use crate::engines::maxwell_3d::{
        AntiAliasAlphaControlInfo, BlendColorInfo, BlendInfo, ColorMaskInfo, ComparisonOp,
        CullFace, DepthMode, DepthStencilInfo, DrawCall, FrontFace, IndexFormat, LogicOpInfo,
        PolygonMode, PrimitiveTopology, RasterizerInfo, RenderTargetInfo, RtControlInfo,
        SamplerBinding, ScissorInfo, ShaderStageInfo, StencilFaceInfo, ViewportInfo, ZetaInfo,
    };

    fn program_slots_with(
        program_index: usize,
        program: Program,
    ) -> [Option<Program>; NUM_PROGRAMS] {
        let mut programs = std::array::from_fn(|_| None);
        programs[program_index] = Some(program);
        programs
    }

    #[test]
    fn runtime_info_fragment_color_types_are_moltenvk_only() {
        let mut fixed_state = FixedPipelineState::default();
        fixed_state.color_formats[0] = crate::gpu::RenderTargetFormat::R32Uint as u8;
        let key = GraphicsPipelineKey {
            unique_hashes: [0, 1, 0, 0, 0, 1],
            fixed_state,
        };
        let fragment = Program::new(ShaderStage::Fragment);
        let programs = program_slots_with(5, fragment);

        let native = make_runtime_info_with_features(
            &programs,
            &key,
            programs[5].as_ref().unwrap(),
            None,
            RuntimeInfoDeviceFeatures {
                transform_feedback: false,
                molten_vk: false,
            },
        );
        assert_eq!(native.frag_color_types[0], AttributeType::Float);

        let molten_vk = make_runtime_info_with_features(
            &programs,
            &key,
            programs[5].as_ref().unwrap(),
            None,
            RuntimeInfoDeviceFeatures {
                transform_feedback: false,
                molten_vk: true,
            },
        );
        assert_eq!(molten_vk.frag_color_types[0], AttributeType::UnsignedInt);
    }

    #[test]
    fn runtime_info_geometry_point_size_uses_program_output_topology() {
        let mut fixed_state = FixedPipelineState::default();
        fixed_state.point_size = 2.5f32.to_bits();
        let key = GraphicsPipelineKey {
            unique_hashes: [0, 1, 0, 0, 1, 0],
            fixed_state,
        };
        let mut geometry = Program::new(ShaderStage::Geometry);
        geometry.output_topology = OutputTopology::PointList;
        let programs = program_slots_with(4, geometry);
        let runtime_info = make_runtime_info_with_features(
            &programs,
            &key,
            programs[4].as_ref().unwrap(),
            None,
            RuntimeInfoDeviceFeatures {
                transform_feedback: false,
                molten_vk: false,
            },
        );
        assert_eq!(runtime_info.fixed_state_point_size, Some(2.5));
    }

    #[test]
    fn runtime_info_uses_complete_fixed_pipeline_key() {
        let mut fixed_state = FixedPipelineState::default();
        fixed_state.set_topology(PrimitiveTopology::Points);
        fixed_state.set_ndc_minus_one_to_one(true);
        fixed_state.set_early_z(true);
        fixed_state.set_y_negate(true);
        fixed_state.point_size = 1.5f32.to_bits();
        fixed_state.set_alpha_test_func(ComparisonOp::Greater);
        fixed_state.alpha_test_ref = 0.25f32.to_bits();
        fixed_state.set_attachment0_dual_source_blend(true);
        fixed_state.set_tessellation_primitive(2);
        fixed_state.set_tessellation_spacing(1);
        fixed_state.set_tessellation_clockwise(true);
        let key = GraphicsPipelineKey {
            unique_hashes: [0, 1, 0, 1, 0, 1],
            fixed_state,
        };
        let features = RuntimeInfoDeviceFeatures {
            transform_feedback: false,
            molten_vk: false,
        };

        let vertex_programs = program_slots_with(1, Program::new(ShaderStage::VertexB));
        let vertex = make_runtime_info_with_features(
            &vertex_programs,
            &key,
            vertex_programs[1].as_ref().unwrap(),
            None,
            features,
        );
        assert_eq!(vertex.fixed_state_point_size, Some(1.5));
        assert!(vertex.convert_depth_mode);
        assert!(vertex.force_early_z);
        assert!(vertex.y_negate);
        assert_eq!(vertex.input_topology, InputTopology::Points);

        let tess_programs = program_slots_with(3, Program::new(ShaderStage::TessellationEval));
        let tess = make_runtime_info_with_features(
            &tess_programs,
            &key,
            tess_programs[3].as_ref().unwrap(),
            None,
            features,
        );
        assert_eq!(tess.tess_primitive, TessPrimitive::Quads);
        assert_eq!(tess.tess_spacing, TessSpacing::FractionalOdd);
        assert!(tess.tess_clockwise);

        let fragment_programs = program_slots_with(5, Program::new(ShaderStage::Fragment));
        let fragment = make_runtime_info_with_features(
            &fragment_programs,
            &key,
            fragment_programs[5].as_ref().unwrap(),
            None,
            features,
        );
        assert_eq!(fragment.alpha_test_func, Some(CompareFunction::Greater));
        assert_eq!(fragment.alpha_test_reference, 0.25);
        assert!(fragment.dual_source_blend);
    }

    #[test]
    fn broken_parallel_shader_compiling_uses_one_worker() {
        assert_eq!(get_pipeline_worker_count(true), 1);
        assert_eq!(
            get_pipeline_worker_count(false),
            get_total_pipeline_workers()
        );
    }

    #[test]
    fn gather_subpixel_offset_matches_upstream_driver_list() {
        for driver in [
            vk::DriverId::AMD_PROPRIETARY,
            vk::DriverId::AMD_OPEN_SOURCE,
            vk::DriverId::MESA_RADV,
            vk::DriverId::INTEL_PROPRIETARY_WINDOWS,
            vk::DriverId::INTEL_OPEN_SOURCE_MESA,
        ] {
            assert!(needs_gather_subpixel_offset(driver));
        }
        assert!(!needs_gather_subpixel_offset(
            vk::DriverId::NVIDIA_PROPRIETARY
        ));
    }

    #[test]
    fn shader_exception_scope_catches_only_shader_exceptions() {
        let shader_result = catch_shader_exception(|| {
            std::panic::panic_any(shader_recompiler::exception::NotImplementedException::new(
                "LC",
            ));
        });
        assert_eq!(shader_result.unwrap_err(), "LC is not implemented");

        let ordinary = std::panic::catch_unwind(|| {
            let _: Result<(), String> = catch_shader_exception(|| panic!("ordinary panic"));
        });
        assert!(ordinary.is_err(), "non-shader panics must not be swallowed");
    }

    #[test]
    fn missing_file_environment_data_is_a_caught_shader_error() {
        let env = FileEnvironment::new();
        let result = catch_shader_exception(|| env.read_cbuf_value(2, 0x20));

        assert_eq!(result.unwrap_err(), "Uncached read texture type");
    }

    fn make_test_draw_call() -> DrawCall {
        DrawCall {
            topology: PrimitiveTopology::Triangles,
            vertex_first: 0,
            vertex_count: 0,
            indexed: false,
            index_buffer_addr: 0,
            index_buffer_addr_end: 0,
            index_buffer_count: 0,
            index_buffer_first: 0,
            index_format: IndexFormat::UnsignedInt,
            vertex_streams: Default::default(),
            vertex_stream_instances: Default::default(),
            vertex_stream_limits: Default::default(),
            viewports: [ViewportInfo::default(); 16],
            viewport_transforms: Default::default(),
            scissors: [ScissorInfo::default(); 16],
            viewport_scale_offset_enabled: false,
            window_origin_lower_left: false,
            window_origin_flip_y: false,
            surface_clip: Default::default(),
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
                depth_mode: DepthMode::MinusOneToOne,
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
            primitive_restart: Default::default(),
            logic_op: LogicOpInfo::default(),
            depth_clamp_enabled: true,
            conservative_raster_enable: false,
            engine_state: crate::engines::maxwell_3d::EngineHint::None,
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
            cb_bindings: [[ConstBufferInfo::default(); 18]; 5],
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
            inline_index_data: Vec::new(),
            sampler_binding: SamplerBinding::Independently,
            render_targets: [RenderTargetInfo::default(); 8],
            zeta: ZetaInfo::default(),
            transform_feedback_enabled: false,
            transform_feedback_state: Default::default(),
            dirty_flags: [false; 256],
        }
    }

    #[test]
    fn parse_vulkan_pipeline_cache_blob_accepts_upstream_header() {
        let mut blob = Vec::new();
        blob.extend_from_slice(&VULKAN_CACHE_MAGIC_NUMBER);
        blob.extend_from_slice(&CACHE_VERSION.to_le_bytes());
        blob.extend_from_slice(&[1, 2, 3, 4]);

        let payload = parse_vulkan_pipeline_cache_blob(&blob, CACHE_VERSION)
            .expect("upstream-shaped header should parse");
        assert_eq!(payload, &[1, 2, 3, 4]);
    }

    #[test]
    fn parse_vulkan_pipeline_cache_blob_rejects_invalid_magic() {
        let mut blob = Vec::new();
        blob.extend_from_slice(b"badmagic");
        blob.extend_from_slice(&CACHE_VERSION.to_le_bytes());

        let result = parse_vulkan_pipeline_cache_blob(&blob, CACHE_VERSION);
        assert_eq!(result, Err(VulkanPipelineCacheHeaderError::InvalidMagic));
    }

    #[test]
    fn parse_vulkan_pipeline_cache_blob_rejects_version_mismatch() {
        let mut blob = Vec::new();
        blob.extend_from_slice(&VULKAN_CACHE_MAGIC_NUMBER);
        blob.extend_from_slice(&(CACHE_VERSION - 1).to_le_bytes());

        let result = parse_vulkan_pipeline_cache_blob(&blob, CACHE_VERSION);
        assert_eq!(result, Err(VulkanPipelineCacheHeaderError::VersionMismatch));
    }

    #[test]
    fn cache_version_matches_upstream_wire_format() {
        assert_eq!(CACHE_VERSION, 18);
    }

    #[test]
    fn gpu_shader_log_payloads_match_upstream_format() {
        let hash = 0x0123_4567_89ab_cdef;
        assert_eq!(
            shader_log_name(hash, "geometry"),
            "shader_0123456789abcdef_geometry"
        );
        assert_eq!(
            shader_log_name(hash, "compute"),
            "shader_0123456789abcdef_compute"
        );
        assert_eq!(
            shader_log_info(hash, 12),
            "SPIR-V size: 48 bytes, hash: 0123456789abcdef"
        );
    }

    #[test]
    fn should_allow_unbuilt_graphics_pipeline_allows_small_depth_draw() {
        let mut draw = make_test_draw_call();
        draw.zeta.enabled = true;
        draw.vertex_count = 4;

        assert!(should_allow_unbuilt_graphics_pipeline(
            true,
            draw.index_buffer_count,
            draw.vertex_count,
        ));
    }

    #[test]
    fn should_allow_unbuilt_graphics_pipeline_rejects_large_draw() {
        let mut draw = make_test_draw_call();
        draw.zeta.enabled = true;
        draw.vertex_count = 7;
        draw.index_buffer_count = 7;

        assert!(!should_allow_unbuilt_graphics_pipeline(
            true,
            draw.index_buffer_count,
            draw.vertex_count,
        ));
    }

    #[test]
    fn should_allow_unbuilt_graphics_pipeline_allows_small_draw_without_depth() {
        let mut draw = make_test_draw_call();
        draw.vertex_count = 4;
        draw.index_buffer_count = 4;

        assert!(should_allow_unbuilt_graphics_pipeline(
            true,
            draw.index_buffer_count,
            draw.vertex_count,
        ));
    }

    #[test]
    fn graphics_key_dynamic_features_filter_checks_all_upstream_flags() {
        let mut key = GraphicsPipelineKey::default();
        key.fixed_state.set_extended_dynamic_state(true);
        key.fixed_state.set_extended_dynamic_state_2(true);
        key.fixed_state.set_extended_dynamic_state_2_logic_op(true);
        key.fixed_state.set_extended_dynamic_state_3_blend(true);
        key.fixed_state.set_extended_dynamic_state_3_enables(true);
        key.fixed_state.set_color_write_enable_dynamic(true);
        key.fixed_state.set_dynamic_vertex_input(true);

        let features = DynamicFeatures {
            has_extended_dynamic_state: true,
            has_extended_dynamic_state_2: true,
            has_extended_dynamic_state_2_logic_op: true,
            has_extended_dynamic_state_3_blend: true,
            has_extended_dynamic_state_3_enables: true,
            has_color_write_enable: true,
            has_dynamic_vertex_input: true,
            has_provoking_vertex_last_mode: true,
            has_provoking_vertex_tf_preserve: true,
            ..Default::default()
        };
        assert!(graphics_key_dynamic_features_match(&key, &features));

        let mutations: [fn(&mut DynamicFeatures); 7] = [
            |f: &mut DynamicFeatures| f.has_extended_dynamic_state = false,
            |f: &mut DynamicFeatures| f.has_extended_dynamic_state_2 = false,
            |f: &mut DynamicFeatures| f.has_extended_dynamic_state_2_logic_op = false,
            |f: &mut DynamicFeatures| f.has_extended_dynamic_state_3_blend = false,
            |f: &mut DynamicFeatures| f.has_extended_dynamic_state_3_enables = false,
            |f: &mut DynamicFeatures| f.has_color_write_enable = false,
            |f: &mut DynamicFeatures| f.has_dynamic_vertex_input = false,
        ];
        for mutate in mutations {
            let mut mismatched = features;
            mutate(&mut mismatched);
            assert!(!graphics_key_dynamic_features_match(&key, &mismatched));
        }

        key.fixed_state.set_provoking_vertex_last(true);
        let mut no_last_mode = features;
        no_last_mode.has_provoking_vertex_last_mode = false;
        assert!(!graphics_key_dynamic_features_match(&key, &no_last_mode));
        key.fixed_state.set_xfb_enabled(true);
        let mut no_tf_preserve = features;
        no_tf_preserve.has_provoking_vertex_tf_preserve = false;
        assert!(!graphics_key_dynamic_features_match(&key, &no_tf_preserve));
    }

    #[test]
    fn pipeline_cache_paths_match_upstream_vulkan_names() {
        let root = std::path::Path::new("/tmp/shader");
        let (pipeline, driver) = pipeline_cache_paths(root, 0x0102030405060708).unwrap();
        assert_eq!(pipeline, root.join("0102030405060708").join("vulkan.bin"));
        assert_eq!(
            driver,
            root.join("0102030405060708").join("vulkan_pipelines.bin")
        );
    }

    #[test]
    fn pipeline_cache_paths_skip_zero_title_id() {
        assert!(pipeline_cache_paths(std::path::Path::new("/tmp/shader"), 0).is_none());
    }

    #[test]
    fn total_pipeline_workers_matches_upstream_minimum_policy() {
        let expected = std::thread::available_parallelism()
            .map(|threads| threads.get())
            .unwrap_or(2)
            .max(2)
            - 1;
        assert_eq!(get_total_pipeline_workers(), expected);
        assert!(get_total_pipeline_workers() >= 1);
    }

    #[test]
    fn graphics_pipeline_key_hash_matches_upstream_cityhash_bytes() {
        let mut keys = [
            GraphicsPipelineKey::default(),
            GraphicsPipelineKey::default(),
        ];
        keys[0].unique_hashes = [1, 2, 3, 4, 5, 6];
        keys[0].fixed_state.raw2 = 0x1234_5678;
        keys[0].fixed_state.viewport_swizzles[7] = 0xCAFE;

        keys[1].unique_hashes = [6, 5, 4, 3, 2, 1];
        keys[1].fixed_state.set_xfb_enabled(true);
        keys[1].fixed_state.xfb_state.layouts[2].stream = 3;
        keys[1].fixed_state.xfb_state.layouts[2].stride = 0x40;

        for key in keys {
            assert_eq!(key.hash_value(), city_hash64(&key.to_cache_bytes()));
        }
    }

    #[test]
    fn graphics_cache_applies_upstream_cityhash_and_unordered_dense_post_mix() {
        use std::hash::BuildHasher;

        let mut key = GraphicsPipelineKey::default();
        key.unique_hashes = [1, 2, 3, 4, 5, 6];
        key.fixed_state.raw2 = 0x1234_5678;
        let map: HashMap<GraphicsPipelineKey, (), BuildUnorderedDenseHasher> =
            HashMap::with_hasher(BuildUnorderedDenseHasher);
        let mut hasher = map.hasher().build_hasher();
        key.hash(&mut hasher);

        let product = (key.hash_value() as u128) * 0x9e37_79b9_7f4a_7c15_u128;
        assert_eq!(hasher.finish(), (product as u64) ^ (product >> 64) as u64);
    }

    #[test]
    fn compute_pipeline_cache_key_hash_changes_with_shared_memory_size() {
        let key_a = ComputePipelineCacheKey {
            unique_hash: 0x1234,
            shared_memory_size: 0x20,
            workgroup_size: [1, 2, 3],
        };
        let key_b = ComputePipelineCacheKey {
            shared_memory_size: 0x40,
            ..key_a
        };

        assert_ne!(key_a.hash_value(), key_b.hash_value());
    }

    #[test]
    fn compute_pipeline_cache_key_hash_changes_with_workgroup_size() {
        let key_a = ComputePipelineCacheKey {
            unique_hash: 0x1234,
            shared_memory_size: 0x20,
            workgroup_size: [1, 2, 3],
        };
        let key_b = ComputePipelineCacheKey {
            workgroup_size: [1, 2, 4],
            ..key_a
        };

        assert_ne!(key_a.hash_value(), key_b.hash_value());
    }

    #[test]
    fn compute_pipeline_cache_key_layout_matches_upstream_shape() {
        assert_eq!(std::mem::size_of::<ComputePipelineCacheKey>(), 24);
        assert_eq!(std::mem::align_of::<ComputePipelineCacheKey>(), 8);
    }

    #[test]
    fn compute_cache_applies_upstream_unordered_dense_post_mix() {
        use std::hash::BuildHasher;

        let key = ComputePipelineCacheKey {
            unique_hash: 0x1234_5678_9abc_def0,
            shared_memory_size: 0x4000,
            workgroup_size: [8, 4, 2],
        };
        let map: HashMap<ComputePipelineCacheKey, (), BuildUnorderedDenseHasher> =
            HashMap::default();
        let mut hasher = map.hasher().build_hasher();
        key.hash(&mut hasher);

        let product = (key.hash_value() as u128) * 0x9e37_79b9_7f4a_7c15_u128;
        assert_eq!(hasher.finish(), (product as u64) ^ (product >> 64) as u64);
    }

    #[test]
    fn compute_cache_can_retain_upstream_negative_entry() {
        let key = ComputePipelineCacheKey {
            unique_hash: 0x1234,
            shared_memory_size: 0x20,
            workgroup_size: [1, 2, 3],
        };
        let mut cache: HashMap<
            ComputePipelineCacheKey,
            Option<Box<ComputePipeline>>,
            BuildUnorderedDenseHasher,
        > = HashMap::default();

        cache.insert(key, None);

        assert!(cache.contains_key(&key));
        assert!(matches!(cache.get(&key), Some(None)));
    }

    #[test]
    fn disk_resource_progress_starts_after_total_is_known() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let callback_calls = Arc::clone(&calls);
        let callback: DiskResourceLoadCallback = Arc::new(move |stage, value, total| {
            callback_calls.lock().unwrap().push((stage, value, total));
        });
        let mut state = DiskResourceLoadState::default();

        state.complete_one(&callback);
        assert!(calls.lock().unwrap().is_empty());

        state.total = 3;
        callback(LoadCallbackStage::Build, 0, state.total);
        state.has_loaded = true;
        state.complete_one(&callback);
        state.complete_one(&callback);

        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                (LoadCallbackStage::Build, 0, 3),
                (LoadCallbackStage::Build, 2, 3),
                (LoadCallbackStage::Build, 3, 3),
            ]
        );
    }
}
