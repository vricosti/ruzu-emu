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
use shader_recompiler::environment::TexturePoolState;
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
use crate::engines::kepler_compute::KeplerCompute;
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
        support_fp32_only_denorm_independence: float_control.denorm_behavior_independence
            == vk::ShaderFloatControlsIndependence::TYPE_32_ONLY,
        support_separate_rounding_mode: float_control.rounding_mode_independence
            == vk::ShaderFloatControlsIndependence::ALL,
        support_fp16_denorm_preserve: float_control.shader_denorm_preserve_float16 != 0,
        support_fp32_denorm_preserve: float_control.shader_denorm_preserve_float32 != 0,
        support_fp16_denorm_flush: float_control.shader_denorm_flush_to_zero_float16 != 0,
        support_fp32_denorm_flush: float_control.shader_denorm_flush_to_zero_float32 != 0,
        has_broken_fp32_denorm_flush: driver_id == vk::DriverId::QUALCOMM_PROPRIETARY,
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
        support_shader_quad_control: device.is_khr_shader_quad_control_supported(),
        support_quad_shuffles: device.is_subgroup_feature_supported(vk::SubgroupFeatureFlags::QUAD),
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
        has_broken_spirv_subgroup_shuffle: driver_id == vk::DriverId::QUALCOMM_PROPRIETARY,
        has_broken_spirv_fragment_subgroup_shuffle: driver_id == vk::DriverId::AMD_PROPRIETARY,
        max_subgroup_size: device.get_max_subgroup_size(),
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
        // The arrays are indexed per invocation, which needs nonuniform
        // indexing to be defined (Lichii vk_pipeline_cache.cpp:705-706).
        support_handle_table_arrays: device.is_sampled_image_array_non_uniform_indexing_supported(),
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

pub(crate) fn make_runtime_info_with_features(
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
    let translated = translate_graphics_stages_from_environments_with_features(
        host_info,
        key,
        environments,
        device_features,
    )?;
    let mut bindings = Bindings::default();
    let mut compiled_stages: [Option<CompiledShader>; 5] = std::array::from_fn(|_| None);
    for (index, translated_stage) in translated.into_iter().enumerate() {
        let Some(translated_stage) = translated_stage else {
            continue;
        };
        let spirv_words = shader_recompiler::backend::emit_spirv_with_bindings(
            &translated_stage.program,
            profile,
            &translated_stage.runtime_info,
            &mut bindings,
        );
        compiled_stages[index] = Some(CompiledShader {
            spirv_words,
            info: translated_stage.program.info.clone(),
            stage: translated_stage.program.stage,
        });
    }
    compiled_stages[0].as_ref()?;
    Some(compiled_stages)
}

pub(crate) struct TranslatedGraphicsShader {
    pub(crate) program: Program,
    pub(crate) runtime_info: RuntimeInfo,
}

pub(crate) const NUM_GRAPHICS_STAGES: usize = 5;

pub(crate) const GEOMETRY_CAPTURE_VERSION: u32 = 2;

fn pre_raster_capture_directory(
    key: &GraphicsPipelineKey,
    has_geometry: bool,
) -> Option<&'static PathBuf> {
    static TESSELLATION_DIRECTORY: std::sync::OnceLock<Option<PathBuf>> =
        std::sync::OnceLock::new();
    if key.unique_hashes[2] != 0 || key.unique_hashes[3] != 0 {
        if let Some(directory) = TESSELLATION_DIRECTORY
            .get_or_init(|| std::env::var_os("RUZU_DUMP_TESSELLATION_PIPELINES").map(PathBuf::from))
            .as_ref()
        {
            return Some(directory);
        }
    }
    if !has_geometry {
        return None;
    }
    static DIRECTORY: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    DIRECTORY
        .get_or_init(|| std::env::var_os("RUZU_DUMP_GEOMETRY_PIPELINES").map(PathBuf::from))
        .as_ref()
}

// Keep all translation-time lookups, not only the Maxwell words. FileEnvironment
// can then replay the translation without consulting a later guest-memory state.
fn capture_geometry_environments(
    path: &std::path::Path,
    key: &GraphicsPipelineKey,
    environments: &[&crate::shader_environment::GenericEnvironment],
) -> std::io::Result<bool> {
    if environments.is_empty() || environments.iter().any(|env| !env.can_be_serialized()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "geometry environments cannot be replayed from their cached instructions",
        ));
    }
    // Claim each key once, including when several disk-cache workers translate it.
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(file) => drop(file),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(error),
    }
    serialize_pipeline(
        &key.to_cache_bytes(),
        environments,
        path,
        GEOMETRY_CAPTURE_VERSION,
    );
    if std::fs::metadata(path)?.len() <= 12 {
        return Err(std::io::Error::other(
            "geometry environment serialization failed",
        ));
    }
    Ok(true)
}

// Opt-in diagnostic at the shared translation boundary (also used by Metal).
// Eden has no equivalent: this records the inputs needed to design a geometry
// fallback without confusing guest geometry with generated layer passthrough.
// Tessellation capture uses the same complete environment format and is opt-in
// independently, including pipelines without a geometry stage.
fn dump_geometry_pipeline_stage(
    directory: &std::path::Path,
    key: &GraphicsPipelineKey,
    program: &Program,
    runtime_info: &RuntimeInfo,
    code: &[u64],
    code_start: u32,
) {
    use std::io::Write;
    let result = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(directory)?;
        let path = directory.join(format!("{:016x}_{:?}.txt", key.hash_value(), program.stage));
        let file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
            Err(error) => return Err(error),
        };
        let mut file = std::io::BufWriter::new(file);
        writeln!(file, "hashes={:016x?}", key.unique_hashes)?;
        writeln!(file, "guest_geometry={}", key.unique_hashes[4] != 0)?;
        writeln!(
            file,
            "guest_tessellation={}",
            key.unique_hashes[2] != 0 || key.unique_hashes[3] != 0
        )?;
        writeln!(file, "code_start={code_start:#x}\nkey={key:?}")?;
        writeln!(file, "runtime={runtime_info:#?}\nprogram={program:#?}")?;
        writeln!(file, "\nMaxwell code (header included when present):")?;
        for (index, word) in code.iter().enumerate() {
            writeln!(
                file,
                "{:08x}: {word:016x}",
                u64::from(code_start) + index as u64 * 8
            )?;
        }
        file.flush()?;
        log::info!("Pre-raster pipeline stage captured: {}", path.display());
        Ok(())
    })();
    if let Err(error) = result {
        log::error!("Pre-raster pipeline capture failed: {error}");
    }
}

pub(crate) fn translate_graphics_stages_from_environments_with_features(
    host_info: &HostTranslateInfo,
    key: &GraphicsPipelineKey,
    environments: &mut GraphicsEnvironments,
    device_features: RuntimeInfoDeviceFeatures,
) -> Option<[Option<TranslatedGraphicsShader>; NUM_GRAPHICS_STAGES]> {
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
        let translate_start = std::time::Instant::now();
        let mut program =
            shader_recompiler::pipeline_cache::translate_program_from_env_with_host_info(
                &code, cfg_offset, env, host_info,
            );
        crate::nx2_profile::TRANSLATE_NS.fetch_add(
            translate_start.elapsed().as_nanos() as u64,
            std::sync::atomic::Ordering::Relaxed,
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

    let mut runtime_infos: [Option<RuntimeInfo>; NUM_PROGRAMS] = std::array::from_fn(|_| None);
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

        runtime_infos[program_index] = Some({
            let program = programs[program_index].as_ref()?;
            let previous_program = previous_stage_index.and_then(|index| programs[index].as_ref());
            make_runtime_info_with_features(
                &programs,
                key,
                program,
                previous_program,
                device_features,
            )
        });
        previous_stage_index = Some(program_index);
    }

    let mut translated: [Option<TranslatedGraphicsShader>; NUM_GRAPHICS_STAGES] =
        std::array::from_fn(|_| None);
    let has_geometry = programs[4].is_some();
    let capture_directory = pre_raster_capture_directory(key, has_geometry);
    if let Some(directory) = capture_directory {
        let path = directory.join(format!("{:016x}.bin", key.hash_value()));
        let result = std::fs::create_dir_all(directory).and_then(|()| {
            capture_geometry_environments(&path, key, &environments.span())
        });
        match result {
            Ok(true) => log::info!("Pre-raster pipeline environments captured: {}", path.display()),
            Ok(false) => {}
            Err(error) => log::error!("Pre-raster environment capture failed: {error}"),
        }
    }
    for program_index in first_program..NUM_PROGRAMS {
        let Some(runtime_info) = runtime_infos[program_index].take() else {
            continue;
        };
        let mut program = programs[program_index].take()?;
        convert_legacy_to_generic(&mut program, &runtime_info);
        if let Some(directory) = capture_directory {
            let env = environments.envs[program_index].generic_environment();
            let (code, start) = if key.unique_hashes[program_index] == 0 {
                (&[][..], 0)
            } else {
                (env.cached_code_slice(), env.cached_code_start())
            };
            dump_geometry_pipeline_stage(directory, key, &program, &runtime_info, code, start);
        }
        translated[program_index - 1] = Some(TranslatedGraphicsShader {
            program,
            runtime_info,
        });
    }
    translated[0].as_ref()?;
    Some(translated)
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
        let translate_start = std::time::Instant::now();
        let mut program =
            shader_recompiler::pipeline_cache::translate_program_from_env_with_host_info(
                &code, cfg_offset, env, host_info,
            );
        crate::nx2_profile::TRANSLATE_NS.fetch_add(
            translate_start.elapsed().as_nanos() as u64,
            std::sync::atomic::Ordering::Relaxed,
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
    let has_geometry = programs[4].is_some();
    let capture_directory = pre_raster_capture_directory(key, has_geometry);
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
        if let Some(directory) = capture_directory {
            let env = environments
                .iter()
                .find(|env| env.shader_stage() == program.stage);
            let (code, start) = env.map_or((&[][..], 0), |env| {
                (env.cached_instruction_slice(), env.cached_instruction_start())
            });
            dump_geometry_pipeline_stage(directory, key, program, &runtime_info, code, start);
        }
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

pub(crate) fn stage_infos_from_compiled(
    compiled_stages: &[Option<CompiledShader>; 5],
) -> [ShaderInfo; 5] {
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

fn mesh_graphics_output_binding(fragment: Option<&ShaderInfo>) -> u32 {
    fragment.map_or(0, |info| (info.constant_buffer_descriptors.len()
        + info.storage_buffers_descriptors.len() + info.texture_buffer_descriptors.len()
        + info.image_buffer_descriptors.len() + info.texture_descriptors.len()
        + info.image_descriptors.len()) as u32)
}

/// Compute-backed mesh still uses the real guest fragment program and normal
/// fixed-state runtime specialization. Only the preceding vertex stage is the
/// generated reader of the output transport.
fn compile_mesh_graphics_stages(
    device: &Device, profile: &Profile, host_info: &HostTranslateInfo,
    key: &GraphicsPipelineKey, environments: &mut GraphicsEnvironments,
    mesh: &shader_recompiler::shader_info::MeshInfo,
) -> Result<[Option<CompiledShader>; NUM_GRAPHICS_STAGES], String> {
    let fragment = if key.unique_hashes[5] != 0 {
        if !environments.env_ptrs.contains(&Some(5)) { return Err("Missing mesh fragment environment".into()); }
        let env = &mut environments.envs[5];
        if env.generic_environment().cached_code_slice().is_empty()
            && env.generic_environment_mut().analyze().is_none() {
            return Err("Mesh fragment analysis failed".into());
        }
        let code = env.generic_environment().cached_instruction_slice().to_vec();
        let start = env.generic_environment().cached_instruction_start();
        Some(shader_recompiler::pipeline_cache::translate_program_from_env_with_host_info(&code, start, env, host_info))
    } else { None };
    let output_binding = mesh_graphics_output_binding(fragment.as_ref().map(|program| &program.info));
    let programs = std::array::from_fn(|_| None);
    let mut vertex = Program::new(ShaderStage::VertexB);
    let runtime = make_runtime_info(&programs, key, &vertex, None, device);
    let (spirv_words, info) = shader_recompiler::backend::spirv::emit_spirv::emit_mesh_vertex_spirv_with_info(
        mesh, profile, &runtime, output_binding).map_err(|error| error.to_string())?;
    vertex.info = info;
    let mut stages = std::array::from_fn(|_| None);
    stages[0] = Some(CompiledShader { spirv_words, info: vertex.info.clone(), stage: ShaderStage::VertexB });
    if let Some(fragment) = fragment {
        let runtime = make_runtime_info(&programs, key, &fragment, Some(&vertex), device);
        let mut bindings = Bindings::default();
        let spirv_words = shader_recompiler::backend::spirv::emit_spirv::emit_spirv_with_bindings(
            &fragment, profile, &runtime, &mut bindings);
        if bindings.unified != output_binding { return Err("Mesh graphics descriptor binding mismatch".into()); }
        stages[4] = Some(CompiledShader { spirv_words, info: fragment.info.clone(), stage: fragment.stage });
    }
    Ok(stages)
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
            false,
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
                false,
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
        has_mesh_output: bool,
    ) -> Option<GraphicsPipeline> {
        let shader_modules = self.create_shader_modules(key, &compiled_stages)?;
        let pipeline = match GraphicsPipeline::new_unbuilt(
            self.device_owner,
            pipeline_cache,
            shader_notify,
            key,
            stage_infos_from_compiled(&compiled_stages),
            shader_modules,
            has_mesh_output,
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
            let create_info = vk::ShaderModuleCreateInfo::default()
                .code(&compiled.spirv_words);
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
pub struct ComputePipelineCacheKey {
    pub unique_hash: u64,
    pub shared_memory_size: u32,
    pub workgroup_size: [u32; 3],
    pub texture_pool_state: Option<TexturePoolState>,
}

// Versioned key extension for shader-side descriptor selection. Serialize
// fields explicitly: Option and bool have no portable raw-byte representation.
pub(super) fn texture_pool_key_bytes(state: Option<TexturePoolState>) -> [u8; 16] {
    let words = state.map_or([0; 4], |s| [1, s.texture_limit, s.sampler_limit, u32::from(s.linked_sampler)]);
    let mut bytes = [0; 16];
    for (dst, word) in bytes.chunks_exact_mut(4).zip(words) {
        dst.copy_from_slice(&word.to_le_bytes());
    }
    bytes
}

pub(super) fn read_texture_pool_key(file: &mut std::fs::File) -> std::io::Result<Option<TexturePoolState>> {
    use std::io::Read;
    let mut bytes = [0; 16];
    file.read_exact(&mut bytes)?;
    let mut words = [0u32; 4];
    for (word, src) in words.iter_mut().zip(bytes.chunks_exact(4)) {
        *word = u32::from_le_bytes(src.try_into().unwrap());
    }
    let invalid = || std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid texture pool key");
    if words[0] == 0 {
        return if words[1..] == [0, 0, 0] { Ok(None) } else { Err(invalid()) };
    }
    if words[0] != 1 || words[3] > 1 { return Err(invalid()); }
    let state = TexturePoolState { texture_limit: words[1], sampler_limit: words[2], linked_sampler: words[3] != 0 };
    state.descriptor_counts().ok_or_else(invalid)?;
    Ok(Some(state))
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

/// Lichii `MaxComputeRebuilds` (vk_pipeline_cache.cpp:949).
const MAX_COMPUTE_REBUILDS: u32 = 4;

/// Port of Lichii `ComputePipelineCacheEntry` (vk_pipeline_cache.h:110-123). Upstream's
/// node-based cache keeps returned `ComputePipeline*` stable and retains a null pipeline after a
/// failed build; `Box` keeps successful pipelines stable across HashMap growth.
#[derive(Default)]
pub(crate) struct ComputePipelineCacheEntry {
    pipeline: Option<Box<ComputePipeline>>,
    /// Constant buffer slots that were null pointers at translation
    null_pointer_slots: Vec<(u32, u32)>,
    /// Constant buffer slots bound as storage buffer pointers
    mapped_pointers: Vec<u64>,
    rebuilds: u32,
}

/// Port of Lichii `PipelineCache::StaleNullPointerSlot` (vk_pipeline_cache.cpp:987-1012): a slot
/// that was null at translation now holds a mapped pointer.
fn stale_null_pointer_slot(
    entry: &ComputePipelineCacheEntry,
    kepler_compute: &KeplerCompute,
    gpu_memory: &crate::memory_manager::MemoryManager,
    shader_hash: u64,
) -> bool {
    let qmd = kepler_compute.launch_description();
    for &(index, offset) in &entry.null_pointer_slots {
        if index as usize >= qmd.const_buffers.len() || (qmd.const_buffer_enable_mask >> index) & 1 == 0 {
            continue;
        }
        let cbuf = qmd.const_buffers[index as usize];
        if u64::from(offset) + 8 > u64::from(cbuf.size) {
            continue; // The translator reads past the bound size as zero, too
        }
        let slot = cbuf.address.wrapping_add(u64::from(offset));
        if !gpu_memory.is_fully_mapped_range(slot, 8) {
            continue;
        }
        let address = gpu_memory.read::<u64>(slot);
        if address != 0
            && gpu_memory.is_within_gpu_address_range(address)
            && gpu_memory.is_fully_mapped_range(address, 1)
        {
            log::info!(
                "Rebuilding compute program {shader_hash:016x}: c{index}[{offset:#x}] was null at \
                 translation and now points to {address:#x}"
            );
            return true;
        }
    }
    false
}

enum DiskPipelineBuildResult {
    Compute(ComputePipelineCacheKey, ComputePipeline, Vec<(u32, u32)>, Vec<u64>),
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
    /// CityHash64 over the explicitly serialized key, including dependencies.
    pub fn hash_value(&self) -> u64 {
        city_hash64(&compute_key_to_cache_bytes(self))
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
            texture_pool_state: read_texture_pool_key(file)?,
        })
    }
}

impl GraphicsPipelineKey {
    /// Port of `GraphicsPipelineCacheKey::Hash` from
    /// `vk_pipeline_cache.cpp`.
    ///
    /// The upstream prefix is followed by the explicit resource-pool key tail.
    /// Keep hashing allocation-free, including on the transition fast path.
    pub fn hash_value(&self) -> u64 {
        const HASH_BYTES: usize =
            std::mem::size_of::<[u64; NUM_PROGRAMS]>() + FixedPipelineState::FULL_SIZE + 16;
        let mut bytes = [0u8; HASH_BYTES];
        let mut offset = 0usize;
        for unique_hash in self.unique_hashes {
            let end = offset + std::mem::size_of::<u64>();
            bytes[offset..end].copy_from_slice(&unique_hash.to_le_bytes());
            offset = end;
        }
        let (state_bytes, state_size) = self.fixed_state.prefix_bytes();
        bytes[offset..offset + state_size].copy_from_slice(&state_bytes[..state_size]);
        offset += state_size;
        bytes[offset..offset + 16].copy_from_slice(&texture_pool_key_bytes(self.texture_pool_state));
        city_hash64(&bytes[..offset + 16])
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
// caches carry per-draw dynamic state baked into keys â€” thousands of
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
// Version 19: native shader environments use the compact software matrix
// collective. Previous captures contain the expanded Maxwell instruction
// stream and cannot be converted back to the original native instructions.
// Version 20: TEX.F16 lowering preserves low/high component order. Cached
// expanded instructions from the previous lowering retain reversed halves.
// Version 21: native control-flow validation and high-register lowering changed.
// Serialized expanded programs must be rebuilt from their native instructions.
// Version 22: native texture/arithmetic lowering and cbuf-backed branch proofs.
// Rebuild expanded programs and record their branch-table constant dependencies.
// Version 23: uniform compare/select, FP16 per-tap gather and LEA.HI negation.
// Previously expanded native programs retain the old lowering behavior.
// Version 24: corrected NOINC capture boundaries, depth gather and constant PTP offsets.
/// 26: the storage buffer pass reads pointer classifications that records made by the earlier
/// pass lack (they failed to load with "Uncached storage pointer classification").
/// 27: constant propagation folds products by 0 and 1 and recombined halves (Lichii f26ad432ae,
/// which bumped its own versions for the same reason): the storage buffer pass then classifies
/// pointers that older records never asked about.
const CACHE_VERSION: u32 = 27;
// Upstream: flush the Vulkan pipeline cache to disk at most once per
// max(30 s, 1 s per MiB of cache) once 128 new pipelines were built.
const VULKAN_CACHE_FLUSH_PIPELINES: usize = 128;
const VULKAN_CACHE_FLUSH_MIN_SECONDS: u64 = 30;
const VULKAN_CACHE_MAGIC_NUMBER: [u8; 8] = *b"yuzuvkch";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VulkanPipelineCacheHeaderError {
    TooSmall,
    InvalidMagic,
    VersionMismatch,
}

/// Port of `SerializeVulkanPipelineCache`: write the Vulkan pipeline cache
/// data with the magic number and cache version header.
fn serialize_vulkan_pipeline_cache_blob(
    device: &ash::Device,
    cache: vk::PipelineCache,
    filename: &std::path::Path,
) {
    if cache == vk::PipelineCache::null() {
        log::error!("Refusing to serialize a null Vulkan pipeline cache");
        return;
    }
    let data = unsafe { device.get_pipeline_cache_data(cache).unwrap_or_default() };

    let mut output = Vec::with_capacity(VULKAN_CACHE_MAGIC_NUMBER.len() + 4 + data.len());
    output.extend_from_slice(&VULKAN_CACHE_MAGIC_NUMBER);
    output.extend_from_slice(&CACHE_VERSION.to_le_bytes());
    output.extend_from_slice(&data);

    if let Err(e) = std::fs::write(filename, &output) {
        log::error!("Failed to write Vulkan pipeline cache: {}", e);
    }
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

/// Maximum number of cached pipelines to rebuild at boot, from
/// `RUZU_PIPELINE_BOOT_LIMIT`. Unset means no extra bound beyond the used-list
/// gating (`should_build_graphics_at_boot`); see the call site for the measured
/// host cost of an unbounded build. Rust extension, absent from Lichii.
fn boot_pipeline_limit() -> Option<usize> {
    static LIMIT: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    *LIMIT.get_or_init(|| {
        std::env::var("RUZU_PIPELINE_BOOT_LIMIT")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .filter(|limit| *limit > 0)
    })
}

/// Sort key for `RUZU_PIPELINE_BOOT_LIMIT`: the boot drops the front of the
/// sorted list, so keys the used list does not name come first, then the
/// pipelines used the most sessions ago; the most recently used come last.
fn boot_limit_rank(age: Option<u8>) -> std::cmp::Reverse<u16> {
    std::cmp::Reverse(age.map_or(u16::MAX, u16::from))
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

fn compute_key_to_cache_bytes(key: &ComputePipelineCacheKey) -> [u8; 40] {
    let mut bytes = [0; 40];
    bytes[..8].copy_from_slice(&key.unique_hash.to_le_bytes());
    bytes[8..12].copy_from_slice(&key.shared_memory_size.to_le_bytes());
    for (dst, value) in bytes[12..24].chunks_exact_mut(4).zip(key.workgroup_size) {
        dst.copy_from_slice(&value.to_le_bytes());
    }
    bytes[24..].copy_from_slice(&texture_pool_key_bytes(key.texture_pool_state));
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
    let translate_start = std::time::Instant::now();
    let mut program = shader_recompiler::pipeline_cache::translate_program_from_env_with_host_info(
        code,
        base_offset,
        env,
        host_info,
    );
    crate::nx2_profile::TRANSLATE_NS.fetch_add(
        translate_start.elapsed().as_nanos() as u64,
        std::sync::atomic::Ordering::Relaxed,
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
    log::debug!("{hash:#016x}");
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
    let create_info = vk::ShaderModuleCreateInfo::default()
        .code(&compiled.spirv_words);
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
    let max_core_threads = common::thread::hardware_concurrency().max(2) - 1;

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

// ---------------------------------------------------------------------------
// Precompiled pipelines (issue #24; Fritchii `be76402ab5`, `790bca8969`)
// ---------------------------------------------------------------------------

/// `<title>/vulkan_used.bin` magic: "EUP1", matching Fritchii's `UsedPipelinesMagic`.
const USED_PIPELINES_MAGIC: u32 = 0x3150_5545;
/// Sessions a used pipeline stays in `vulkan_used.bin` (Lichii
/// `UsedPipelinesMaxAge`, vk_pipeline_cache.cpp:794-803): each built pipeline
/// holds driver memory, but a pipeline built while playing stalls the draw that
/// needs it, and two sessions still left 1-2 s of every 10 s building early in
/// play on Lichii's NX2 reference title.
const USED_PIPELINES_MAX_AGE: u8 = 16;
/// MoltenVK keeps fewer sessions (Lichii `UsedPipelinesMaxAgeMoltenVK`): a
/// pipeline built while playing goes through SPIR-V -> MSL -> Metal.
const USED_PIPELINES_MAX_AGE_MOLTENVK: u8 = 8;
/// Without a used list (first boot with this cache, or a session that ended
/// before saving one), at most this many cached graphics pipelines are built at
/// boot, the rest when first needed (Lichii `MaxPipelinesWithoutUsedList`,
/// vk_pipeline_cache.cpp:811-817).
const MAX_PIPELINES_WITHOUT_USED_LIST: usize = 3000;
/// A session too short to say what the game uses (a boot to the menu) keeps the old list.
const MINIMUM_USED_PIPELINES: usize = 64;
/// How often a session's first-use records are written while the game runs
/// (Lichii `save_interval`, vk_pipeline_cache.cpp:2114).
const USED_PIPELINES_SAVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

/// Port of Lichii `MaxUsedPipelineAge` (vk_pipeline_cache.cpp:805-810).
fn max_used_pipeline_age(is_molten_vk: bool) -> u8 {
    if is_molten_vk {
        return USED_PIPELINES_MAX_AGE_MOLTENVK;
    }
    USED_PIPELINES_MAX_AGE
}

/// Port of Lichii `LoadUsedPipelines` (vk_pipeline_cache_records.cpp:13-33):
/// key hash -> sessions since the last use, without the pipelines last used
/// `max_age` or more sessions ago (lists saved with a longer window).
fn load_used_pipelines(path: &std::path::Path, max_age: u8) -> HashMap<u64, u8> {
    use std::io::Read;

    let mut used = HashMap::new();
    let Ok(mut file) = std::fs::File::open(path) else {
        return used;
    };
    let mut magic = [0u8; 4];
    let mut count = [0u8; 4];
    if file.read_exact(&mut magic).is_err()
        || u32::from_le_bytes(magic) != USED_PIPELINES_MAGIC
        || file.read_exact(&mut count).is_err()
    {
        return used;
    }
    for _ in 0..u32::from_le_bytes(count) {
        let mut entry = [0u8; 9];
        if file.read_exact(&mut entry).is_err() {
            break;
        }
        let hash = u64::from_le_bytes(entry[..8].try_into().expect("8 bytes"));
        if entry[8] < max_age {
            used.insert(hash, entry[8]);
        }
    }
    used
}

/// Port of Fritchii `PreloadAllPipelines`: `RUZU_PRELOAD_ALL_PIPELINES=1` builds
/// every cached pipeline at boot, as before. (Upstream
/// `EDEN_NX2_PRELOAD_ALL_PIPELINES`.)
fn preload_all_pipelines() -> bool {
    common::env_flag!("RUZU_PRELOAD_ALL_PIPELINES")
}

/// The boot-gating decision of Lichii `LoadDiskResources`
/// (vk_pipeline_cache.cpp:1475-1476).
///
/// Build at boot only what a recent session used; the rest is built when a draw
/// first needs it and is not stored in the disk cache again. Without a used
/// list, only the first `MAX_PIPELINES_WITHOUT_USED_LIST` graphics records
/// (`queued_graphics` counts those already queued).
fn should_build_graphics_at_boot(
    key_hash: u64,
    used_pipelines: &HashMap<u64, u8>,
    preload_all: bool,
    queued_graphics: usize,
) -> bool {
    if preload_all {
        return true;
    }
    if used_pipelines.is_empty() {
        return queued_graphics < MAX_PIPELINES_WITHOUT_USED_LIST;
    }
    used_pipelines.contains_key(&key_hash)
}

/// Serialize `vulkan_used.bin` from this session's uses and the loaded ages
/// (Fritchii `SaveUsedPipelines`).
///
/// The ages come from a file: `saturating_add` keeps an out-of-range age (255
/// in a damaged or foreign list) from overflowing; such an entry is past any
/// `max_age` and dropped. The list is written whole to a temporary file and
/// renamed over the old one, like the other cache sidecars, so a crash or a
/// full disk mid-write never leaves a truncated list that the next boot would
/// read as "build almost nothing".
fn write_used_pipelines(
    path: &std::path::Path,
    used_this_session: &std::collections::HashSet<u64>,
    used_pipelines: &HashMap<u64, u8>,
    max_age: u8,
) -> std::io::Result<()> {
    use std::io::Write;

    let mut next = HashMap::with_capacity(used_pipelines.len() + used_this_session.len());
    for hash in used_this_session {
        next.insert(*hash, 0u8);
    }
    for (hash, age) in used_pipelines {
        let age = age.saturating_add(1);
        if age < max_age {
            next.entry(*hash).or_insert(age);
        }
    }
    let mut bytes = Vec::with_capacity(8 + next.len() * 9);
    bytes.extend_from_slice(&USED_PIPELINES_MAGIC.to_le_bytes());
    bytes.extend_from_slice(&(next.len() as u32).to_le_bytes());
    for (hash, age) in next {
        bytes.extend_from_slice(&hash.to_le_bytes());
        bytes.push(age);
    }
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    let written = std::fs::File::create(&temporary)
        .and_then(|mut file| file.write_all(&bytes).and_then(|()| file.sync_all()))
        .and_then(|()| std::fs::rename(&temporary, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

/// Bytes before the first pipeline record: the 8-byte magic and the u32 cache
/// version (`serialize_pipeline` / `load_pipelines` header).
fn cache_file_header_len() -> usize {
    crate::shader_environment::MAGIC_NUMBER.len() + std::mem::size_of::<u32>()
}

/// Read one captured graphics record: its environments and its key.
///
/// The parse mirrors `load_pipelines`'s body and additionally requires the record
/// to end exactly where boot measured it, so a record corrupted by a partial
/// append is rejected instead of desynchronizing the caller's cursor.
fn read_graphics_record(
    file: &mut std::fs::File,
    offset: u64,
    length: u64,
) -> std::io::Result<(GraphicsPipelineKey, Vec<FileEnvironment>)> {
    use std::io::{Read, Seek, SeekFrom};

    file.seek(SeekFrom::Start(offset))?;
    let mut buf4 = [0u8; 4];
    file.read_exact(&mut buf4)?;
    let num_envs = u32::from_le_bytes(buf4) as usize;
    if num_envs == 0 || num_envs > NUM_PROGRAMS {
        return Err(crate::shader_environment::invalid_cache_data(format!(
            "pipeline environment count {num_envs} is outside 1..={NUM_PROGRAMS}"
        )));
    }
    let mut environments = Vec::new();
    environments
        .try_reserve_exact(num_envs)
        .map_err(|_| crate::shader_environment::invalid_cache_data("record is too large"))?;
    for _ in 0..num_envs {
        let mut environment = FileEnvironment::new();
        environment.deserialize(file)?;
        environments.push(environment);
    }
    let key = GraphicsPipelineKey::read_from_file(file)?;
    let consumed = file.stream_position()? - offset;
    if consumed != length {
        return Err(crate::shader_environment::invalid_cache_data(format!(
            "record is {consumed} bytes, boot measured {length}"
        )));
    }
    Ok((key, environments))
}

/// Fritchii `PreparePrecompiled`'s stamp file: the step runs once per cache file,
/// driver and emulator build.
fn precompiled_stamp_path(base_dir: &std::path::Path) -> PathBuf {
    base_dir.join(format!("precompiled_prepared.ruzu-v{CACHE_VERSION}.txt"))
}

/// Contents of the preparation stamp: file size, modification time, driver
/// version, build revision and device model.
fn precompiled_stamp(
    filename: &std::path::Path,
    driver_version: u32,
    model_name: &str,
) -> std::io::Result<String> {
    let metadata = std::fs::metadata(filename)?;
    let modified = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(format!(
        "{} {} {driver_version:#x} {} {model_name}\n",
        metadata.len(),
        modified,
        common::scm_rev::SCM_REV,
    ))
}

fn read_precompiled_stamp(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Fritchii `PreparePrecompiled`, adapted: rebuild and drop every cached graphics
/// pipeline this boot did not build, so the driver's own cache holds their
/// compiled code before a draw needs them.
///
/// Draws never use these pipelines: they build their own from the guest's
/// shaders and find the driver's compiled code. Progress is reported as
/// `WarmProgress` kind 3.
fn prepare_precompiled_graphics(
    filename: &std::path::Path,
    base_dir: &std::path::Path,
    records: &[(u64, u64)],
    mut builder: GraphicsPipelineBuilder,
    pipeline_cache: vk::PipelineCache,
    shader_notify: crate::shader_notify::ShaderNotifyHandle,
    graphics_runtime: GraphicsPipelineRuntime,
    stop_loading: &std::sync::atomic::AtomicBool,
    cancel: &std::sync::atomic::AtomicBool,
    force: bool,
) {
    let stamp_path = precompiled_stamp_path(base_dir);
    let stamp = match precompiled_stamp(
        filename,
        builder.device_owner.get().get_driver_version(),
        &builder.device_owner.get().get_model_name(),
    ) {
        Ok(stamp) => stamp,
        Err(error) => {
            log::debug!("Precompiled pipelines skipped: {}", error);
            return;
        }
    };
    if !force && read_precompiled_stamp(&stamp_path).as_deref() == Some(stamp.as_str()) {
        return;
    }

    let total = records.len();
    let done = std::sync::atomic::AtomicUsize::new(0);
    let built = std::sync::atomic::AtomicUsize::new(0);
    let failed = std::sync::atomic::AtomicUsize::new(0);
    let begin = std::time::Instant::now();
    log::info!("Precompiled pipelines: preparing {total} cached pipelines in the background");

    // The records boot captured, read one at a time. The file is append-only while
    // the game runs, so a captured range is stable; a record that no longer parses
    // (or whose hash moved) is skipped without touching the cache file.
    match std::fs::OpenOptions::new().read(true).open(filename) {
        Ok(mut file) => {
            for &(offset, length) in records {
                if stop_loading.load(Ordering::Acquire) || cancel.load(Ordering::Acquire) {
                    break;
                }
                let (key, mut environments) = match read_graphics_record(&mut file, offset, length)
                {
                    Ok(record) => record,
                    Err(error) => {
                        failed.fetch_add(1, Ordering::Relaxed);
                        log::debug!(
                            "Precompiled pipelines: record at {offset}+{length} unreadable: {error}"
                        );
                        done.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                };
                let pipeline = builder.build_from_file_environments(
                    pipeline_cache,
                    shader_notify,
                    &mut environments,
                    &key,
                    graphics_runtime,
                    None,
                );
                if pipeline.is_some() {
                    built.fetch_add(1, Ordering::Relaxed);
                } else {
                    failed.fetch_add(1, Ordering::Relaxed);
                }
                // The driver's cache keeps the compiled code; the pipeline itself
                // is not retained (Fritchii builds and drops).
                drop(pipeline);
                let finished = done.fetch_add(1, Ordering::Relaxed) + 1;
                if finished % 32 == 0 || finished == total {
                    shader_notify.set_warm_progress(3, finished, total);
                }
                if finished % 500 == 0 || finished == total {
                    log::info!(
                        "Precompiled pipelines: {finished} of {total} done, {} built, {} failed, {:.0} s",
                        built.load(Ordering::Relaxed),
                        failed.load(Ordering::Relaxed),
                        begin.elapsed().as_secs_f64()
                    );
                }
            }
        }
        Err(error) => {
            log::debug!("Precompiled pipelines skipped: cannot open the cache: {error}");
            return;
        }
    }

    let finished = done.load(Ordering::Relaxed);
    if finished < total && (stop_loading.load(Ordering::Acquire) || cancel.load(Ordering::Acquire)) {
        log::info!(
            "Precompiled pipelines: stopped after {finished} of {total}; the rest is prepared next time"
        );
        return;
    }
    // An interrupted preparation is not stamped as done: it continues next boot.
    if failed.load(Ordering::Relaxed) * 2 > total {
        log::warn!(
            "Precompiled pipelines: {} of {total} failed; not stamping the preparation",
            failed.load(Ordering::Relaxed)
        );
        return;
    }
    if let Err(error) = std::fs::write(&stamp_path, stamp) {
        log::warn!("Precompiled pipelines: cannot write the stamp: {error}");
    }
}

/// Port of `PipelineCache` class.
///
/// Cache-owned compute portion of a guest mesh graphics pipeline. The task
/// stage is absent for direct mesh draws. Metadata remains the translated
/// upstream MeshInfo used by both transport allocation and generated vertices.
pub(crate) struct MeshComputePrograms {
    pub task: Option<super::compute_pipeline::MeshComputeStage>,
    pub task_info: Option<shader_recompiler::shader_info::MeshInfo>,
    /// With a task program: the distribution pass that resolves the mesh
    /// workgroup count on the GPU (Lichii mesh emulation, see vk_rasterizer).
    pub distribution: Option<super::compute_pipeline::MeshEmulationDistribution>,
    pub mesh: super::compute_pipeline::MeshComputeStage,
    pub mesh_info: shader_recompiler::shader_info::MeshInfo,
}

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
    // Compute execution is independent of fixed rasterization state. Shader
    // hashes already include upstream mesh-register/primitive-header identity.
    mesh_compute_cache: HashMap<([u64; NUM_PROGRAMS], TexturePoolState), Option<Rc<MeshComputePrograms>>, BuildUnorderedDenseHasher>,
    mesh_graphics_cache: HashMap<GraphicsPipelineKey, Option<Rc<GraphicsPipeline>>, BuildUnorderedDenseHasher>,
    current_pipeline: Option<Rc<GraphicsPipeline>>,
    dynamic_features: DynamicFeatures,

    main_pools: ShaderPools,

    pipeline_cache_filename: PathBuf,

    /// Key hashes of the graphics pipelines in the disk cache. Only those a
    /// recent session used are built at boot; the rest are built when first
    /// needed, and not stored again. Each built pipeline holds driver memory
    /// (Lichii `disk_pipelines`, vk_pipeline_cache.h:316). It is also the only
    /// duplicate-append guard for graphics records: a key whose hash is here is
    /// never serialized again (Lichii vk_pipeline_cache.cpp:2998-3003).
    disk_pipelines: std::collections::HashSet<u64>,
    /// Key hash -> sessions since it was last used (`vulkan_used.bin`).
    used_pipelines: HashMap<u64, u8>,
    /// `max_used_pipeline_age` of this device.
    used_pipelines_max_age: u8,
    /// Graphics pipelines this session handed out at least once. The pipelines
    /// themselves carry the mark (`GraphicsPipeline::mark_used`, Lichii
    /// `MarkUsed`/`WasUsed`); `save_used_pipelines` collects them from
    /// `graphics_cache`, off the lookup path. This count only gates the
    /// minimum-size rule without walking the cache.
    used_this_session: usize,
    used_pipelines_filename: PathBuf,
    /// When `vulkan_used.bin` was last written; `None` until a session takes it over.
    used_pipelines_saved: Option<std::time::Instant>,
    /// Idle-priority worker that prepares cached pipelines the boot did not
    /// build (Fritchii `precompile_thread`). Created when a preparation starts.
    precompile_worker: Option<ThreadWorker>,
    /// Set when a preparation is running so the preparation loop can be asked
    /// to end with the renderer.
    precompile_cancel: Arc<std::sync::atomic::AtomicBool>,

    vulkan_pipeline_cache_filename: PathBuf,
    /// Compute keys already present in the on-disk pipeline file, kept to avoid
    /// duplicate appends (graphics keys use `disk_pipelines`).
    disk_compute_keys: std::collections::HashSet<ComputePipelineCacheKey>,
    vulkan_pipeline_cache: vk::PipelineCache,

    // Upstream's node-based cache keeps returned `ComputePipeline*` stable and
    // retains a null unique_ptr after a failed build. `None` preserves that
    // negative-cache entry; `Box` keeps successful pipelines stable across
    // HashMap growth.
    compute_cache:
        HashMap<ComputePipelineCacheKey, ComputePipelineCacheEntry, BuildUnorderedDenseHasher>,
    /// Pipelines replaced by a rebuild (Lichii `retired_compute_pipelines`); dispatches already
    /// recorded may still use them.
    retired_compute_pipelines: Vec<Box<ComputePipeline>>,
    /// Upstream `Common::ThreadWorker workers`, owned by `PipelineCache`.
    ///
    /// This is the required owner for disk-cache rebuild jobs and asynchronous
    /// `GraphicsPipeline` / `ComputePipeline` creation.
    workers: ThreadWorker,
    /// Upstream `Common::ThreadWorker serialization_thread`.
    serialization_thread: ThreadWorker,
    // Periodic Vulkan pipeline cache flush state (upstream fields of the same
    // names). The atomics are shared with the serialization worker.
    pipelines_since_flush: usize,
    last_flush: Option<std::time::Instant>,
    last_cache_size: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    flush_in_flight: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl PipelineCache {
    fn create_vulkan_pipeline_cache(
        &self,
        initial_data: &[u8],
    ) -> Result<vk::PipelineCache, vk::Result> {
        let cache_ci = vk::PipelineCacheCreateInfo::default()
            .initial_data(initial_data);
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
        let use_asynchronous_shaders = common::settings::values()
            .effective_use_asynchronous_shaders();
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
            mesh_compute_cache: HashMap::with_hasher(BuildUnorderedDenseHasher),
            mesh_graphics_cache: HashMap::with_hasher(BuildUnorderedDenseHasher),
            current_pipeline: None,
            dynamic_features,
            main_pools: ShaderPools::new(),
            pipeline_cache_filename: PathBuf::new(),
            disk_pipelines: std::collections::HashSet::new(),
            used_pipelines: HashMap::new(),
            used_pipelines_max_age: USED_PIPELINES_MAX_AGE,
            used_this_session: 0,
            used_pipelines_filename: PathBuf::new(),
            used_pipelines_saved: None,
            precompile_worker: None,
            precompile_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            vulkan_pipeline_cache_filename: PathBuf::new(),
            disk_compute_keys: std::collections::HashSet::new(),
            vulkan_pipeline_cache: vk::PipelineCache::null(),
            compute_cache: HashMap::with_hasher(BuildUnorderedDenseHasher),
            retired_compute_pipelines: Vec::new(),
            workers: ThreadWorker::new_stateless(
                get_pipeline_worker_count(has_broken_parallel_shader_compiling),
                "VkPipelineBuilder".to_string(),
            ),
            serialization_thread: ThreadWorker::new_stateless(
                1,
                "VkPipelineSerialization".to_string(),
            ),
            pipelines_since_flush: 0,
            last_flush: None,
            last_cache_size: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            flush_in_flight: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
        // `native_turing` is read once per lookup, without a lock as upstream
        // (GetNativeMacroEngine().native_turing): Maxwell3D::native_turing.
        let maxwell = shared_cache.current_maxwell3d();
        let native_turing = maxwell
            .is_some_and(|engine| engine.native_turing());
        // Refresh before mesh lookup, Next(), and the slow-path lookup alike.
        self.graphics_key.texture_pool_state = maxwell
            .filter(|_| native_turing)
            .map(|engine| TexturePoolState {
                texture_limit: engine.tex_header_pool_limit(),
                sampler_limit: engine.tex_sampler_pool_limit(),
                linked_sampler: engine.sampler_binding() == crate::engines::maxwell_3d::SamplerBinding::ViaHeaderBinding,
            });
        // Launch-time opt-in for the compute mesh adaptation; avoid querying
        // the host environment on every ordinary graphics-pipeline lookup.
        if super::vk_rasterizer::mesh_draw_enabled()
            && native_turing
            && maxwell.is_some_and(|maxwell| maxwell.is_mesh_enabled()) {
            let programs = self.current_mesh_compute_programs(shared_cache)?;
            self.current_pipeline = self.current_mesh_graphics_pipeline(draw, shared_cache, &programs.mesh_info);
            return self.current_pipeline.as_deref();
        }
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

    fn current_mesh_graphics_pipeline(
        &mut self, draw: &mut Maxwell3DDrawView<'_>, shared_cache: &SharedShaderCache,
        mesh: &shader_recompiler::shader_info::MeshInfo,
    ) -> Option<Rc<GraphicsPipeline>> {
        self.graphics_key.fixed_state.refresh(draw, &self.dynamic_features);
        let mut key = self.graphics_key.clone();
        let topology = match mesh.topology {
            0 => PrimitiveTopology::Points,
            1 => PrimitiveTopology::Lines,
            _ => PrimitiveTopology::Triangles,
        };
        key.fixed_state.set_topology(topology);
        key.fixed_state.dynamic_state.set_primitive_restart_enable(false);
        let rasterizer = draw.rasterizer();
        key.fixed_state.dynamic_state.set_depth_bias_enable(match topology {
            PrimitiveTopology::Points => rasterizer.polygon_offset_point_enable,
            PrimitiveTopology::Lines => rasterizer.polygon_offset_line_enable,
            _ => rasterizer.polygon_offset_fill_enable,
        });
        if !self.mesh_graphics_cache.contains_key(&key) {
            let mut environments = GraphicsEnvironments::default();
            shared_cache.get_graphics_environments(&mut environments, &key.unique_hashes);
            let result = catch_shader_exception(|| {
                let stages = compile_mesh_graphics_stages(self.device_owner.get(), &self.profile, &self.host_info,
                    &key, &mut environments, mesh)?;
                self.graphics_pipeline_builder.build_pipeline(self.vulkan_pipeline_cache, self.shader_notify,
                    &key, stages, None, self.graphics_runtime, None, true)
                    .ok_or_else(|| "Mesh graphics pipeline creation failed".to_string())
            }).and_then(|result| result);
            let pipeline = match result {
                Ok(pipeline) => Some(Rc::new(pipeline)),
                Err(error) => { log::error!("Mesh graphics pipeline: {error}"); None }
            };
            self.mesh_graphics_cache.insert(key.clone(), pipeline);
            self.queue_vulkan_pipeline_cache_flush();
        }
        self.mesh_graphics_cache.get(&key)?.as_ref().map(Rc::clone)
    }

    /// Mesh-compute counterpart of CurrentGraphicsPipeline/CreateGraphicsPipeline.
    /// The caller supplies the shared cache constructed with new_mesh_compute;
    /// it must be the sole consumer of the channel's shader dirty flags.
    pub(crate) fn current_mesh_compute_programs(
        &mut self, shared_cache: &mut SharedShaderCache,
    ) -> Option<Rc<MeshComputePrograms>> {
        let maxwell = shared_cache.current_maxwell3d()?;
        if !maxwell.is_mesh_enabled()
            || !maxwell.native_turing() {
            return None;
        }
        // Share the persistent hashes with ordinary graphics lookup: RefreshStages
        // leaves them untouched when shaders aren't dirty, including on draws
        // following a task/mesh draw in the same channel.
        let pool_state = TexturePoolState {
            texture_limit: maxwell.tex_header_pool_limit(),
            sampler_limit: maxwell.tex_sampler_pool_limit(),
            linked_sampler: maxwell.sampler_binding() == crate::engines::maxwell_3d::SamplerBinding::ViaHeaderBinding,
        };
        if !shared_cache.refresh_stages(&mut self.graphics_key.unique_hashes) {
            return None;
        }
        let hashes = self.graphics_key.unique_hashes;
        let key = (hashes, pool_state);
        if !self.mesh_compute_cache.contains_key(&key) {
            let mut environments = GraphicsEnvironments::default();
            shared_cache.get_graphics_environments(&mut environments, &hashes);
            let built = catch_shader_exception(|| -> Result<MeshComputePrograms, String> {
                let mut task = None;
                let mut task_info = None;
                let mut distribution = None;
                let mut mesh = None;
                let mut mesh_info = None;
                for index in [1, 3] {
                    if hashes[index] == 0 { continue; }
                    if !environments.env_ptrs.contains(&Some(index)) {
                        return Err(format!("Missing mesh environment at graphics program {index}"));
                    }
                    let env = &mut environments.envs[index];
                    if env.generic_environment().cached_code_slice().is_empty()
                        && env.generic_environment_mut().analyze().is_none() {
                        return Err(format!("Mesh analysis failed at graphics program {index}"));
                    }
                    let code = env.generic_environment().cached_instruction_slice().to_vec();
                    let start = env.generic_environment().cached_instruction_start();
                    let program = shader_recompiler::pipeline_cache::translate_program_from_env_with_host_info(
                        &code, start, env, &self.host_info);
                    // A mesh after a task is dispatched by the distribution
                    // pass and finds its task itself (Lichii mesh emulation).
                    let task_payload_stride = task_info.as_ref()
                        .map(|info: &shader_recompiler::shader_info::MeshInfo| info.payload_words);
                    let shader = match task_payload_stride {
                        Some(stride) if !program.info.is_task =>
                            shader_recompiler::backend::spirv::emit_spirv::emit_mesh_compute_spirv_with_task_distribution(
                                &program, &self.profile, &RuntimeInfo::default(), &mut Bindings::default(), stride),
                        _ => shader_recompiler::backend::spirv::emit_spirv::emit_mesh_compute_spirv(
                            &program, &self.profile, &RuntimeInfo::default(), &mut Bindings::default()),
                    }.map_err(|error| error.to_string())?;
                    let stage = super::compute_pipeline::MeshComputeStage::new(self.device_owner.get(),
                        program.info.clone(), &shader, index - 1, self.vulkan_pipeline_cache)
                        .map_err(|error| format!("Mesh pipeline creation: {error:?}"))?;
                    if program.info.is_task {
                        if task.is_some() { return Err("Duplicate task stage".into()); }
                        task = Some(stage);
                        task_info = Some(program.mesh.clone());
                        let code = shader_recompiler::backend::spirv::emit_spirv_mesh::emit_mesh_emulation_distribution(&self.profile);
                        distribution = Some(super::compute_pipeline::MeshEmulationDistribution::new(
                            self.device_owner.get(), &code, self.vulkan_pipeline_cache)
                            .map_err(|error| format!("Mesh distribution pipeline creation: {error:?}"))?);
                    } else {
                        if mesh.is_some() { return Err("Duplicate mesh stage".into()); }
                        mesh = Some(stage);
                        mesh_info = Some(program.mesh.clone());
                    }
                }
                let mesh = mesh.ok_or("Mesh graphics pipeline has no mesh stage")?;
                if mesh.has_task_args() != task.is_some() {
                    return Err("Task program must precede its mesh program".into());
                }
                Ok(MeshComputePrograms {
                    task, task_info, distribution,
                    mesh,
                    mesh_info: mesh_info.ok_or("Mesh graphics pipeline has no mesh metadata")?,
                })
            }).and_then(|result| result);
            let programs = match built {
                Ok(programs) => Some(Rc::new(programs)),
                Err(error) => {
                    log::error!("Mesh compute pipeline: {error}");
                    None
                }
            };
            self.mesh_compute_cache.insert(key, programs);
            self.queue_vulkan_pipeline_cache_flush();
        }
        self.mesh_compute_cache.get(&key)?.as_ref().map(Rc::clone)
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
            texture_pool_state: kepler_compute.is_native_mode().then(|| TexturePoolState {
                texture_limit: kepler_compute.tic_limit(),
                sampler_limit: kepler_compute.tsc_limit(),
                linked_sampler: qmd.linked_tsc,
            }),
            unique_hash: shader_hash,
            shared_memory_size: qmd.shared_alloc,
            workgroup_size: [qmd.block_dim_x, qmd.block_dim_y, qmd.block_dim_z],
        };
        if !self.compute_cache.contains_key(&key) {
            self.compute_cache.insert(key, ComputePipelineCacheEntry::default());
            let entry = self.create_runtime_compute_pipeline(shared_cache, &key, shader_size, &[])?;
            *self
                .compute_cache
                .get_mut(&key)
                .expect("new compute cache entry disappeared") = entry;
        } else {
            let stale = {
                let entry = self.compute_cache.get(&key).expect("checked above");
                !entry.null_pointer_slots.is_empty()
                    && entry.rebuilds < MAX_COMPUTE_REBUILDS
                    && shared_cache.current_gpu_memory().is_some_and(|gpu_memory| {
                        stale_null_pointer_slot(entry, kepler_compute, &gpu_memory.lock(), shader_hash)
                    })
            };
            if stale {
                // The translation dropped accesses through a pointer that was null back then.
                // Which slots the game fills depends on the area, so the new one keeps binding
                // every pointer this one binds, null now or not: a null pointer binds nothing.
                let keep = self.compute_cache[&key].mapped_pointers.clone();
                let rebuilt = self.create_runtime_compute_pipeline(shared_cache, &key, shader_size, &keep);
                let entry = self.compute_cache.get_mut(&key).expect("checked above");
                match rebuilt {
                    Some(mut rebuilt) if rebuilt.pipeline.is_some() => {
                        if let Some(old) = entry.pipeline.take() {
                            self.retired_compute_pipelines.push(old);
                        }
                        rebuilt.rebuilds = entry.rebuilds + 1;
                        *entry = rebuilt;
                    }
                    _ => {
                        // Keep the working translation when the new one cannot be built, and stop
                        // retrying.
                        log::warn!("Keeping compute shader {shader_hash:016x}: rebuild failed");
                        entry.rebuilds = MAX_COMPUTE_REBUILDS;
                    }
                }
            }
        }
        self.compute_cache
            .get_mut(&key)
            .and_then(|entry| entry.pipeline.as_deref_mut())
            .map(|pipeline| CurrentComputePipeline {
                pipeline_owner: NonNull::from(pipeline),
            })
    }

    /// The runtime half of Lichii `PipelineCache::CreateComputePipeline(key, shader,
    /// keep_pointers)` (vk_pipeline_cache.cpp:3024-3074): `keep_pointers` stay storage buffer
    /// pointers whatever their slots hold now.
    fn create_runtime_compute_pipeline(
        &mut self,
        shared_cache: &mut SharedShaderCache,
        key: &ComputePipelineCacheKey,
        shader_size: usize,
        keep_pointers: &[u64],
    ) -> Option<ComputePipelineCacheEntry> {
        let kepler_compute = shared_cache.current_kepler_compute()?;
        let gpu_memory = shared_cache.current_gpu_memory()?;
        let mut env = ComputeEnvironment::from_kepler_compute(kepler_compute, gpu_memory);
        env.generic_environment_mut().set_cached_size(shader_size);
        env.generic_environment_mut().keep_storage_pointers(keep_pointers);
        self.main_pools.release_contents();
        let pipeline = self.create_compute_pipeline_from_environment(key, &mut env);
        self.queue_vulkan_pipeline_cache_flush();
        let pipeline = pipeline?;
        if !self.pipeline_cache_filename.as_os_str().is_empty()
            && self.disk_compute_keys.insert(*key)
        {
            let key_bytes = compute_key_to_cache_bytes(key);
            let filename = self.pipeline_cache_filename.clone();
            let generic_env = env.generic_environment().clone();
            self.serialization_thread.queue_stateless_work(move || {
                serialize_pipeline(&key_bytes, &[&generic_env], &filename, CACHE_VERSION);
            });
        }
        let generic = env.generic_environment();
        let native = generic.has_native_instructions();
        Some(ComputePipelineCacheEntry {
            pipeline: Some(Box::new(pipeline)),
            null_pointer_slots: if native { generic.null_storage_pointer_slots() } else { Vec::new() },
            mapped_pointers: if native { generic.mapped_storage_pointer_keys() } else { Vec::new() },
            rebuilds: 0,
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
            self.queue_vulkan_pipeline_cache_flush();
            self.graphics_cache.insert(key.clone(), pipeline);
        }

        let pipeline = Rc::clone(self.graphics_cache.get(&key)?.as_ref()?);
        // Lichii `CurrentGraphicsPipelineSlowPath` (vk_pipeline_cache.cpp:2114-2123):
        // record what the session draws, and write it out at most every
        // `USED_PIPELINES_SAVE_INTERVAL`. Only the slow path hands a pipeline out
        // for the first time: the fast path (`GraphicsPipeline::next`) follows
        // transitions, which only this path adds, to pipelines it already
        // returned. The mark is a flag on the pipeline, so neither path hashes
        // the key nor touches a hash set.
        if !pipeline.was_used() {
            pipeline.mark_used();
            self.used_this_session += 1;
            let due = self.used_pipelines_saved.is_none_or(|saved| {
                saved.elapsed() >= USED_PIPELINES_SAVE_INTERVAL
            });
            if due {
                self.save_used_pipelines();
            }
        }
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
            // Lichii vk_pipeline_cache.cpp:2998-3003: a key the disk cache
            // already holds is only not built at boot; nothing new to store.
            if !self.disk_pipelines.insert(graphics_key_cache_hash(key)) {
                self.queue_vulkan_pipeline_cache_flush();
                return Some(pipeline);
            }
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
            .expect("pipeline cache path should have a parent directory")
            .to_path_buf();
        if let Err(err) = std::fs::create_dir_all(&base_dir) {
            log::error!("Failed to create pipeline cache directories: {}", err);
            return;
        }

        self.pipeline_cache_filename = pipeline_cache_filename;
        self.vulkan_pipeline_cache_filename = vulkan_pipeline_cache_filename;
        // Fritchii `be76402ab5`: remember which pipelines recent sessions used so
        // boot builds only those (each built pipeline holds driver memory).
        self.used_pipelines_filename = base_dir.join("vulkan_used.bin");
        self.used_pipelines_max_age =
            max_used_pipeline_age(self.device_owner.get().is_molten_vk());
        self.used_pipelines =
            load_used_pipelines(&self.used_pipelines_filename, self.used_pipelines_max_age);
        self.used_pipelines_saved = Some(std::time::Instant::now());
        self.used_this_session = 0;
        self.disk_pipelines.clear();
        let preload_all = preload_all_pipelines();
        let mut skipped_pipelines = 0usize;
        log::info!(
            "Loading Vulkan disk pipeline cache title_id={:016X} file={}",
            title_id,
            self.pipeline_cache_filename.display()
        );

        // Load Vulkan pipeline cache from disk if available
        if self.use_vulkan_pipeline_cache {
            self.vulkan_pipeline_cache =
                self.load_vulkan_pipeline_cache(&self.vulkan_pipeline_cache_filename.clone());
            // Upstream: record the loaded cache size and start the flush timer.
            let size = unsafe {
                self.device
                    .get_pipeline_cache_data(self.vulkan_pipeline_cache)
                    .map(|data| data.len())
                    .unwrap_or(0)
            };
            self.last_cache_size
                .store(size, std::sync::atomic::Ordering::Relaxed);
            self.last_flush = Some(std::time::Instant::now());
        }

        use std::cell::{Cell, RefCell};
        use std::io::Seek as _;

        let mut built = 0usize;
        let skipped = Cell::new(0usize);
        let pipeline_statistics = self
            .device_owner
            .get()
            .is_khr_pipeline_executable_properties_enabled()
            .then(|| Arc::new(PipelineStatistics::new(self.device_owner.get())));
        let dynamic_features = self.dynamic_features;
        let used_pipelines = self.used_pipelines.clone();
        let disk_pipelines = RefCell::new(std::collections::HashSet::new());
        // (offset, length) of every graphics record boot skipped by the used
        // gating. The preparation reads these exact records afterwards; it must
        // not stream the file, which the GPU thread keeps appending to.
        let skipped_records = RefCell::new(Vec::<(u64, u64)>::new());
        // Start of the record being parsed: both callbacks advance it past their
        // record, and `load_pipelines` delivers records strictly in file order.
        let record_start = Cell::new(cache_file_header_len() as u64);
        let queued_graphics = Cell::new(0usize);
        let loaded_compute: RefCell<Vec<(ComputePipelineCacheKey, FileEnvironment)>> =
            RefCell::new(Vec::new());
        let load_compute = |file: &mut std::fs::File, env: FileEnvironment| {
            let key = ComputePipelineCacheKey::read_from_file(file)?;
            record_start.set(file.stream_position()?);
            if env.has_ambiguous_depth_stencil_formats() {
                skipped.set(skipped.get() + 1);
                return Ok(());
            }
            loaded_compute.borrow_mut().push((key, env));
            Ok(())
        };
        // The record range travels with each entry: the optional boot limit
        // below may still defer it to the background preparation.
        let loaded_graphics: RefCell<
            Vec<(GraphicsPipelineKey, Vec<FileEnvironment>, (u64, u64))>,
        > = RefCell::new(Vec::new());
        let load_graphics = |file: &mut std::fs::File, envs: Vec<FileEnvironment>| {
            // Read the key even for skipped entries: the next record starts after it.
            let key = GraphicsPipelineKey::read_from_file(file)?;
            let record_end = file.stream_position()?;
            let record = (record_start.get(), record_end - record_start.get());
            record_start.set(record_end);
            if envs.iter().any(FileEnvironment::has_ambiguous_depth_stencil_formats) {
                skipped.set(skipped.get() + 1);
                return Ok(());
            }
            if !graphics_key_dynamic_features_match(&key, &dynamic_features) {
                skipped.set(skipped.get() + 1);
                return Ok(());
            }
            let hash = graphics_key_cache_hash(&key);
            // Lichii vk_pipeline_cache.cpp:1467-1470: a later record of a key
            // already seen is neither built at boot nor prepared.
            if !disk_pipelines.borrow_mut().insert(hash) {
                return Ok(());
            }
            if !should_build_graphics_at_boot(
                hash,
                &used_pipelines,
                preload_all,
                queued_graphics.get(),
            ) {
                skipped_pipelines += 1;
                skipped_records.borrow_mut().push(record);
                return Ok(());
            }
            queued_graphics.set(queued_graphics.get() + 1);
            loaded_graphics.borrow_mut().push((key, envs, record));
            Ok(())
        };
        load_pipelines(
            || stop_loading.load(Ordering::Acquire),
            &self.pipeline_cache_filename,
            CACHE_VERSION,
            Box::new(load_compute),
            Box::new(load_graphics),
        );

        // Remember which keys the file already holds so runtime creations do
        // not append duplicates, whether or not they get precompiled now.
        // Graphics: every record that fits the device, built at boot or not
        // (Lichii `disk_pipelines`). Taken here, before the GPU thread starts
        // adding to it and before the optional early return below.
        for (key, _) in loaded_compute.borrow().iter() {
            self.disk_compute_keys.insert(*key);
        }
        self.disk_pipelines = std::mem::take(&mut *disk_pipelines.borrow_mut());

        // RUZU_SKIP_BOOT_PRECOMPILE=1 keeps the cache paths, the loaded
        // Vulkan driver cache and the known-key sets, but skips the blocking
        // rebuild (measured ~4 minutes for 25k pipelines, 59% of worker time
        // in AMD driver vkCreate): pipelines build on demand through the
        // asynchronous shader path instead.
        if std::env::var_os("RUZU_SKIP_BOOT_PRECOMPILE")
            .is_some_and(|value| value.to_string_lossy().starts_with('1'))
        {
            let skipped_total =
                loaded_compute.borrow().len() + loaded_graphics.borrow().len();
            log::info!(
                "Skipping boot precompilation of {skipped_total} pipelines (RUZU_SKIP_BOOT_PRECOMPILE)"
            );
            callback(crate::rasterizer_interface::LoadCallbackStage::Complete, 0, 0);
            return;
        }

        let build_results = Arc::new(Mutex::new(Vec::<DiskPipelineBuildResult>::new()));
        let job_skipped = Arc::new(AtomicUsize::new(0));
        let load_state = Arc::new(Mutex::new(DiskResourceLoadState::default()));
        let mut queued_total = 0usize;

        let mut loaded_compute = loaded_compute.into_inner();
        let mut loaded_graphics_entries = loaded_graphics.into_inner();
        let mut skipped_records = skipped_records.into_inner();
        if let Some(limit) = boot_pipeline_limit() {
            // Building every known pipeline is what the host pays for: on
            // Mario Kart World the 26k-pipeline build adds 9.65 GiB of private
            // and 9.9 GiB of mapped memory to the process, measured across one
            // session's own lifetime, which is what drives a 32 GiB host into
            // paging.
            //
            // Which ones are kept decides whether the bound helps at all.
            // Ordering by position in the append-ordered file keeps the most
            // recently *recorded* pipelines, and measurement showed the race
            // then rebuilt what it needed and residency came back. Rank by the
            // used list instead (`vulkan_used.bin`, sessions since last use), so
            // the kept set is the one recent sessions actually drew with; keys
            // the list does not name rank lowest. The deferred graphics records
            // go to the background preparation like the gated ones.
            let total = loaded_compute.len() + loaded_graphics_entries.len();
            if total > limit {
                let ranked = !self.used_pipelines.is_empty();
                if ranked {
                    let used = &self.used_pipelines;
                    loaded_graphics_entries.sort_by_key(|(key, _, _)| {
                        boot_limit_rank(used.get(&graphics_key_cache_hash(key)).copied())
                    });
                }
                let graphics_keep = limit.min(loaded_graphics_entries.len());
                let compute_keep = limit - graphics_keep;
                let graphics_drop = loaded_graphics_entries.len() - graphics_keep;
                let compute_drop = loaded_compute.len().saturating_sub(compute_keep);
                skipped_records.extend(
                    loaded_graphics_entries
                        .drain(..graphics_drop)
                        .map(|(_, _, record)| record),
                );
                loaded_compute.drain(..compute_drop);
                log::info!(
                    "Boot pipeline limit {limit} ({}): building {} graphics + {} compute, \
                     deferring {} to on-demand compilation",
                    if ranked { "ranked by the used list" } else { "file order, no used list yet" },
                    loaded_graphics_entries.len(),
                    loaded_compute.len(),
                    total - limit
                );
            }
        }
        let loaded_compute = loaded_compute;
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
                let native = env.has_native_instructions();
                let null_pointer_slots =
                    if native { env.null_storage_pointer_slots() } else { Vec::new() };
                let mapped_pointers =
                    if native { env.mapped_storage_pointer_keys() } else { Vec::new() };
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
                        .push(DiskPipelineBuildResult::Compute(
                            key,
                            pipeline,
                            null_pointer_slots,
                            mapped_pointers,
                        )),
                    None => {
                        skipped_jobs.fetch_add(1, Ordering::Relaxed);
                    }
                }
                state.lock().unwrap().complete_one(&callback);
            });
            queued_total += 1;
        }

        for (key, envs, _record) in loaded_graphics_entries {
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
                DiskPipelineBuildResult::Compute(key, pipeline, null_pointer_slots, mapped_pointers) => {
                    if self.compute_cache.contains_key(&key) {
                        skipped_count += 1;
                    } else {
                        self.compute_cache.insert(key, ComputePipelineCacheEntry {
                            pipeline: Some(Box::new(pipeline)),
                            null_pointer_slots,
                            mapped_pointers,
                            rebuilds: 0,
                        });
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
            "Total Pipeline Count: {} (built={}, skipped={}, translate={:.1}s, vkcreate={:.1}s cumulative worker time)",
            queued_total,
            built,
            skipped_count,
            crate::nx2_profile::TRANSLATE_NS.load(std::sync::atomic::Ordering::Relaxed) as f64
                / 1e9,
            crate::nx2_profile::VKCREATE_NS.load(std::sync::atomic::Ordering::Relaxed) as f64
                / 1e9,
        );
        if skipped_pipelines != 0 {
            if self.used_pipelines.is_empty() && !preload_all {
                log::info!(
                    "No used pipeline list yet: {skipped_pipelines} cached graphics pipelines past the first {MAX_PIPELINES_WITHOUT_USED_LIST} are built when first needed"
                );
            } else {
                log::info!(
                    "{skipped_pipelines} cached graphics pipelines no recent session used are built when first needed"
                );
            }
        }

        if self.use_vulkan_pipeline_cache {
            self.serialize_vulkan_pipeline_cache(&self.vulkan_pipeline_cache_filename);
        }
        if let Some(statistics) = pipeline_statistics {
            statistics.report();
        }

        // Fritchii `StartPrecompiled`: prepare what boot did not build in the
        // background while the game runs (see `start_precompiled`).
        self.start_precompiled(&base_dir, skipped_records, stop_loading);
    }

    /// Fritchii `StartPrecompiled`: background preparation of the cached
    /// pipelines boot did not build.
    ///
    /// Boot builds only what recent sessions used; the skipped pipelines would
    /// otherwise be translated and compiled inside the first draw that needs
    /// them. This prepares them on an idle-priority thread while the game runs so
    /// the driver's own cache holds their compiled code first (Fritchii
    /// `StartPrecompiled` + `PreparePrecompiled`, adapted to ruzu's `vulkan.bin`
    /// instead of an `NX2PREP1` file).
    fn start_precompiled(
        &mut self,
        base_dir: &std::path::Path,
        records: Vec<(u64, u64)>,
        stop_loading: DiskResourceLoadStop,
    ) {
        self.precompile_worker = None;
        self.precompile_cancel
            .store(false, std::sync::atomic::Ordering::Release);
        self.shader_notify.set_precompile_available(false);
        self.shader_notify.set_warm_progress(0, 0, 0);

        if common::env_flag!("RUZU_DISABLE_PRECOMPILE") {
            return;
        }
        // The prepared pipelines are kept by the driver's own cache. Without one
        // they would have to live in the emulator's pipeline cache file, which is
        // loaded whole at boot: preparation would not save the stutter.
        if !self.use_vulkan_pipeline_cache {
            log::info!(
                "Precompiled pipelines skipped: the driver keeps no pipeline cache of its own"
            );
            return;
        }

        if records.is_empty() {
            return;
        }

        let stamp_path = precompiled_stamp_path(base_dir);
        let current_stamp = precompiled_stamp(
            &self.pipeline_cache_filename,
            self.device_owner.get().get_driver_version(),
            &self.device_owner.get().get_model_name(),
        );
        if let Ok(stamp) = current_stamp {
            if read_precompiled_stamp(&stamp_path).as_deref() == Some(stamp.as_str()) {
                log::info!("Precompiled pipelines already prepared for this driver");
                return;
            }
        }

        let worker = ThreadWorker::new_stateless_with_placement(
            1,
            "VkPrecompile".to_owned(),
            common::thread::ThreadPlacement::Background,
        );
        let filename = self.pipeline_cache_filename.clone();
        let base_dir = base_dir.to_path_buf();
        let records = Arc::new(records);
        let builder = self.graphics_pipeline_builder.clone_for_disk_worker();
        let pipeline_cache = self.vulkan_pipeline_cache;
        let shader_notify = self.shader_notify;
        let graphics_runtime = self.graphics_runtime;
        let cancel = Arc::clone(&self.precompile_cancel);
        let stop_loading = Arc::clone(&stop_loading);

        self.shader_notify.set_precompile_available(true);
        worker.queue_stateless_work(move || {
            let mut force = false;
            loop {
                shader_notify.precompile_started();
                prepare_precompiled_graphics(
                    &filename,
                    &base_dir,
                    &records,
                    builder.clone_for_disk_worker(),
                    pipeline_cache,
                    shader_notify,
                    graphics_runtime,
                    &stop_loading,
                    &cancel,
                    force,
                );
                shader_notify.set_warm_progress(0, 0, 0);
                shader_notify.precompile_ended();

                // Prepare Shaders Now prepares the file once more, whatever the
                // stamp says (Fritchii `StartPrecompiled`'s request loop).
                while !stop_loading.load(Ordering::Acquire)
                    && !cancel.load(Ordering::Acquire)
                    && !shader_notify.precompile_requested()
                {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
                if stop_loading.load(Ordering::Acquire) || cancel.load(Ordering::Acquire) {
                    break;
                }
                log::info!("Precompiled pipelines: prepared again (Prepare Shaders Now)");
                force = true;
            }
            if shader_notify.precompile_requested() {
                // A request that came with the stop: it was counted as a warm-up.
                shader_notify.precompile_started();
                shader_notify.precompile_ended();
            }
            shader_notify.set_precompile_available(false);
        });
        self.precompile_worker = Some(worker);
    }

    /// Ask the preparation thread to prepare the title's cached pipelines again.
    /// Returns false when this title has nothing to prepare.
    pub fn request_precompile(&self) -> bool {
        self.shader_notify.request_precompile()
    }

    /// Port of Fritchii `PipelineCache::SaveUsedPipelines`.
    ///
    /// A session too short to say what the game uses (a boot to the menu) keeps
    /// the old list; the caller's `vulkan_used` ages are carried forward and
    /// dropped after `max_used_pipeline_age` sessions.
    fn save_used_pipelines(&mut self) {
        self.used_pipelines_saved = Some(std::time::Instant::now());
        log::debug!(
            "Used pipelines: {} this session ({} needed to save)",
            self.used_this_session,
            MINIMUM_USED_PIPELINES
        );
        if self.used_pipelines_filename.as_os_str().is_empty()
            || self.used_this_session < MINIMUM_USED_PIPELINES
        {
            return;
        }
        // Lichii `SaveUsedPipelines` (vk_pipeline_cache.cpp:897-917): the hashes
        // of the pipelines in `graphics_cache` this session handed out. The
        // file is written by the serialization thread, as Lichii appends its
        // record through the cache writer's thread; `Drop` waits for it.
        let used: std::collections::HashSet<u64> = self
            .graphics_cache
            .values()
            .flatten()
            .filter(|pipeline| pipeline.was_used())
            .map(|pipeline| pipeline.key_hash())
            .collect();
        let filename = self.used_pipelines_filename.clone();
        let loaded = self.used_pipelines.clone();
        let max_age = self.used_pipelines_max_age;
        self.serialization_thread.queue_stateless_work(move || {
            if let Err(error) = write_used_pipelines(&filename, &used, &loaded, max_age) {
                log::warn!("Failed to save the used pipeline list: {error}");
            }
        });
    }

    /// Port of `PipelineCache::SerializeVulkanPipelineCache`.
    ///
    /// Serializes the Vulkan pipeline cache to disk.
    pub fn serialize_vulkan_pipeline_cache(&self, filename: &std::path::Path) {
        serialize_vulkan_pipeline_cache_blob(&self.device, self.vulkan_pipeline_cache, filename);
    }

    /// Port of `PipelineCache::QueueVulkanPipelineCacheFlush`: after
    /// `VULKAN_CACHE_FLUSH_PIPELINES` new pipelines, and no more often than
    /// `max(VULKAN_CACHE_FLUSH_MIN_SECONDS, cache size in MiB)` seconds,
    /// serialize the Vulkan pipeline cache on the serialization thread.
    fn queue_vulkan_pipeline_cache_flush(&mut self) {
        use std::sync::atomic::Ordering;
        if !self.use_vulkan_pipeline_cache
            || self.vulkan_pipeline_cache_filename.as_os_str().is_empty()
        {
            return;
        }
        self.pipelines_since_flush += 1;
        if self.pipelines_since_flush < VULKAN_CACHE_FLUSH_PIPELINES {
            return;
        }
        let now = std::time::Instant::now();
        let megabytes = self.last_cache_size.load(Ordering::Relaxed) / (1024 * 1024);
        let interval =
            std::time::Duration::from_secs(VULKAN_CACHE_FLUSH_MIN_SECONDS.max(megabytes as u64));
        if let Some(last_flush) = self.last_flush {
            if now.duration_since(last_flush) < interval {
                return;
            }
        }
        if self.flush_in_flight.swap(true, Ordering::AcqRel) {
            return;
        }
        self.pipelines_since_flush = 0;
        self.last_flush = Some(now);
        let device = self.device.clone();
        let cache = self.vulkan_pipeline_cache;
        let filename = self.vulkan_pipeline_cache_filename.clone();
        let last_cache_size = std::sync::Arc::clone(&self.last_cache_size);
        let flush_in_flight = std::sync::Arc::clone(&self.flush_in_flight);
        self.serialization_thread.queue_stateless_work(move || {
            serialize_vulkan_pipeline_cache_blob(&device, cache, &filename);
            let size = unsafe {
                device
                    .get_pipeline_cache_data(cache)
                    .map(|data| data.len())
                    .unwrap_or(0)
            };
            last_cache_size.store(size, Ordering::Relaxed);
            flush_in_flight.store(false, Ordering::Release);
        });
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
        // Fritchii `~PipelineCache` saves the session's uses first.
        self.save_used_pipelines();
        // A preparation still running must end before the renderer's owners go away.
        self.precompile_cancel
            .store(true, std::sync::atomic::Ordering::Release);
        self.precompile_worker = None;
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

    #[test]
    fn boot_limit_keeps_the_most_recently_used_pipelines() {
        // `RUZU_PIPELINE_BOOT_LIMIT` drains the front of the sorted list: keys the
        // used list does not name go first, then the oldest, so draining the
        // front drops exactly the coldest entries.
        let mut entries = vec![
            (0xA, Some(3u8)),
            (0xB, None),
            (0xC, Some(0u8)),
            (0xD, Some(15u8)),
            (0xE, None),
        ];
        entries.sort_by_key(|(_, age)| boot_limit_rank(*age));
        let order: Vec<u64> = entries.iter().map(|(hash, _)| *hash).collect();
        assert_eq!(order, vec![0xB, 0xE, 0xD, 0xA, 0xC]);
        // u8::MAX is still older than any listed age, never equal to "unlisted".
        assert!(boot_limit_rank(None) < boot_limit_rank(Some(u8::MAX)));
    }

    #[test]
    fn mesh_output_binding_follows_descriptor_declarations_not_array_elements() {
        use shader_recompiler::shader_info::{ConstantBufferDescriptor, StorageBufferDescriptor};
        assert_eq!(mesh_graphics_output_binding(None), 0);
        let mut fragment = ShaderInfo::default();
        fragment.constant_buffer_descriptors.push(ConstantBufferDescriptor { index: 0, count: 4 });
        fragment.storage_buffers_descriptors.push(StorageBufferDescriptor {
            cbuf_index: 0, cbuf_offset: 16, count: 8, is_written: false, ..Default::default() });
        assert_eq!(mesh_graphics_output_binding(Some(&fragment)), 2);
    }

    use crate::engines::const_buffer_info::ConstBufferInfo;
    use crate::engines::maxwell_3d::{
        AntiAliasAlphaControlInfo, BlendColorInfo, BlendInfo, ColorMaskInfo, ComparisonOp,
        CullFace, DepthMode, DepthStencilInfo, DrawCall, FrontFace, IndexFormat, LogicOpInfo,
        PolygonMode, PrimitiveTopology, RasterizerInfo, RenderTargetInfo, RtControlInfo,
        SamplerBinding, ScissorInfo, ShaderStageInfo, StencilFaceInfo, ViewportInfo, ZetaInfo,
    };

    /// Offline prerequisite inspection, not a rendering test. The native host
    /// profile uses the shared Maxwell translator. TCS is compiled as a native
    /// compute entry, TES through its production native PSO. No rendered oracle.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires RUZU_REPLAY_VULKAN_CACHE and a new RUZU_DUMP_TESSELLATION_PIPELINES directory"]
    fn inspect_cached_tessellation_with_native_host_profile() {
        use crate::renderer_metal::{
            metal_device::MetalDevice,
            metal_pipeline_cache::{make_host_translate_info as native_host_info, make_shader_profile},
            metal_shader::compile_msl_library,
        };
        use crate::shader_environment::GraphicsEnvironment;
        use std::io::Write;
        use std::os::unix::fs::DirBuilderExt;

        let source = PathBuf::from(std::env::var_os("RUZU_REPLAY_VULKAN_CACHE")
            .expect("set RUZU_REPLAY_VULKAN_CACHE to a Vulkan environment cache"));
        let directory = PathBuf::from(std::env::var_os("RUZU_DUMP_TESSELLATION_PIPELINES")
            .expect("set RUZU_DUMP_TESSELLATION_PIPELINES to a new output directory"));
        std::fs::DirBuilder::new().mode(0o700).create(&directory).unwrap();
        let copy = directory.join("input-snapshot.bin");
        // LoadPipelines may delete invalid files. Only expose this private copy,
        // and validate its survival before treating an empty result as evidence.
        let mut snapshot = std::fs::OpenOptions::new().write(true).create_new(true)
            .open(&copy).unwrap();
        std::io::copy(&mut std::fs::File::open(source).unwrap(), &mut snapshot).unwrap();
        snapshot.flush().unwrap();
        drop(snapshot);
        let mut graphics_count = 0usize;
        let mut compute_count = 0usize;
        let mut entries = Vec::new();
        load_pipelines(
            || false, &copy, CACHE_VERSION,
            Box::new(|file, _| {
                ComputePipelineCacheKey::read_from_file(file)?;
                compute_count += 1;
                Ok(())
            }),
            Box::new(|file, environments| {
                let key = GraphicsPipelineKey::read_from_file(file)?;
                graphics_count += 1;
                if key.unique_hashes[2] != 0 || key.unique_hashes[3] != 0 {
                    entries.push((key, environments));
                }
                Ok(())
            }),
        );
        assert!(copy.exists(), "cache snapshot rejected; counts are not valid");
        assert!(graphics_count + compute_count != 0, "cache snapshot is empty");
        eprintln!("Environment inventory: graphics={graphics_count} compute={compute_count} tessellation={}", entries.len());
        let device = MetalDevice::new().unwrap();
        let host_info = native_host_info(device.profile());
        for (key, files) in entries {
            let mut environments = GraphicsEnvironments::default();
            for file in files {
                let slot = (0..NUM_PROGRAMS)
                    .find(|&slot| shader_stage_for_program(slot) == Some(file.shader_stage()))
                    .expect("graphics entry contains a graphics stage");
                environments.envs[slot] = GraphicsEnvironment::from_file_environment(file);
                environments.env_ptrs[slot] = Some(slot);
            }
            let translated = catch_shader_exception(|| {
                translate_graphics_stages_from_environments_with_features(
                    &host_info, &key, &mut environments,
                    RuntimeInfoDeviceFeatures { transform_feedback: true, molten_vk: true },
                )
            }).expect("captured shader translation must succeed")
                .expect("captured pipeline must have a vertex stage");
            let stages: Vec<_> = translated.iter().flatten().map(|stage| stage.program.stage).collect();
            eprintln!("Tessellation pipeline {:016x}: {stages:?}", key.hash_value());
            if let Some(control) = &translated[1] {
                use shader_recompiler::backend::msl::{
                    emit_msl::emit_msl_tessellation_control_function,
                    emit_msl_tessellation::TessellationControlLayout, MslOptions,
                };
                use crate::renderer_metal::metal_tessellation_pipeline::MetalTessellationControlPipeline;
                let artifact = emit_msl_tessellation_control_function(&control.program,
                    &make_shader_profile(device.profile()), &control.runtime_info,
                    &MslOptions::default(), &mut Bindings::default()).unwrap();
                std::fs::write(directory.join(format!("{:016x}_control.metal", key.hash_value())),
                    &artifact.source.source).unwrap();
                let layout = TessellationControlLayout::new(&control.program, &control.runtime_info).unwrap();
                MetalTessellationControlPipeline::new(&device, &artifact, &layout).unwrap();
                eprintln!("Runtime native TCS compute pipeline compiled: {:016x}", key.hash_value());
            }
            if let (Some(control), Some(evaluation)) = (&translated[1], &translated[2]) {
                use shader_recompiler::backend::msl::{
                    emit_msl::emit_msl_tessellation_evaluation_function,
                    emit_msl_tessellation::TessellationControlLayout,
                    MslOptions,
                };
                use crate::renderer_metal::{metal_pipeline_cache::MetalRenderPipelineKey,
                    metal_tessellation_pipeline::MetalTessellationEvaluationPipeline};
                use objc2_foundation::NSString;
                use objc2_metal::{MTLLibrary as _, MTLPixelFormat};
                let layout = TessellationControlLayout::new(&control.program, &control.runtime_info).unwrap();
                let artifact = emit_msl_tessellation_evaluation_function(&evaluation.program,
                    &make_shader_profile(device.profile()), &evaluation.runtime_info,
                    &MslOptions::default(), &mut Bindings::default()).unwrap();
                std::fs::write(directory.join(format!("{:016x}_evaluation.metal", key.hash_value())),
                    &artifact.source.source).unwrap();
                // Diagnostic-only fragment: this verifies the real TES entry,
                // not framebuffer formats or the complete captured guest pipeline.
                let library = compile_msl_library(device.device(),
                    "#include <metal_stdlib>\nusing namespace metal;\nfragment float4 probe_fragment() { return float4(1.0f); }",
                    artifact.language_version).unwrap();
                let fragment = library.newFunctionWithName(&NSString::from_str("probe_fragment")).unwrap();
                let mut render_key = MetalRenderPipelineKey::new(0, 0);
                render_key.color_attachments[0].format = MTLPixelFormat::RGBA8Unorm;
                MetalTessellationEvaluationPipeline::new(&device, &render_key, &artifact,
                    &layout, &evaluation.runtime_info, 64, Some(&fragment)).unwrap();
                eprintln!("Native TES render pipeline compiled: {:016x}", key.hash_value());
            }
            let mut cache = crate::renderer_metal::metal_pipeline_cache::MetalPipelineCache::new(device.clone());
            cache.validate_captured_tessellation(&key, &mut environments)
                .expect("complete captured native tessellation stage chain must compile");
            eprintln!("Native captured VS/TCS/TES/FS pipeline and cache reuse verified: {:016x}", key.hash_value());
        }
    }

    #[test]
    fn pre_raster_environment_capture_replays_every_stage_once_and_rejects_unbound_code() {
        use crate::shader_environment::{GenericEnvironment, ShaderStage as EnvironmentStage};

        let path = std::env::temp_dir().join(format!(
            "ruzu-geometry-environments-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let mut key = GraphicsPipelineKey::default();
        key.unique_hashes = [0, 0x1234, 0x2345, 0x3456, 0x5678, 0x9abc];
        let stages = [
            EnvironmentStage::VertexB,
            EnvironmentStage::TessellationControl,
            EnvironmentStage::TessellationEval,
            EnvironmentStage::Geometry,
            EnvironmentStage::Fragment,
        ];
        let mut environments: Vec<_> = stages
            .into_iter()
            .map(|stage| {
                let mut env = GenericEnvironment::new()
                    .with_program(0, 0x100)
                    .with_stage(stage)
                    .with_gpu_read(Arc::new(move |_, bytes| {
                        for chunk in bytes.chunks_exact_mut(8) {
                            chunk.copy_from_slice(&(stage as u64 + 1).to_le_bytes());
                        }
                    }));
                env.set_cached_size(16);
                env
            })
            .collect();
        let references: Vec<_> = environments.iter().collect();
        assert!(capture_geometry_environments(&path, &key, &references).unwrap());
        let bytes = std::fs::read(&path).unwrap();
        assert!(!capture_geometry_environments(&path, &key, &references).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let mut calls = 0;
        load_pipelines(
            || false,
            &path,
            GEOMETRY_CAPTURE_VERSION,
            Box::new(|_, _| panic!("graphics capture must not contain a compute entry")),
            Box::new(|file, loaded| {
                assert_eq!(loaded.len(), stages.len());
                for (env, stage) in loaded.iter().zip(stages) {
                    assert_eq!(env.shader_stage(), stage);
                    assert_eq!(env.read_instruction(0x100), stage as u64 + 1);
                    assert_eq!(env.read_instruction(0x108), stage as u64 + 1);
                }
                let loaded_key = GraphicsPipelineKey::read_from_file(file)?;
                assert_eq!(loaded_key.to_cache_bytes(), key.to_cache_bytes());
                calls += 1;
                Ok(())
            }),
        );
        assert_eq!(calls, 1);
        std::fs::remove_file(&path).unwrap();
        environments[1].read_instruction(0x110);
        let references: Vec<_> = environments.iter().collect();
        assert!(capture_geometry_environments(&path, &key, &references).is_err());
        assert!(!path.exists());
    }

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
            texture_pool_state: None,
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
            texture_pool_state: None,
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
            texture_pool_state: None,
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
    fn invalid_predicate_boolean_op_is_a_caught_shader_error() {
        use shader_recompiler::frontend::translate::{common_funcs, TranslatorVisitor};
        use shader_recompiler::ir::{basic_block::Block, program::Program, types::ShaderStage, value::Value};

        let result = catch_shader_exception(|| {
            let mut program = Program::new(ShaderStage::VertexB);
            program.blocks.push(Block::new());
            let mut visitor = TranslatorVisitor::new(&mut program, 0);
            common_funcs::predicate_combine(
                &mut visitor, Value::ImmU1(true), Value::ImmU1(false), 3,
            );
        });
        assert_eq!(result.unwrap_err(), "Invalid bop 3 is not implemented");
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
    fn unsupported_imad32i_is_caught_and_next_shader_can_compile() {
        use shader_recompiler::frontend::translate::TranslatorVisitor;
        use shader_recompiler::ir::{basic_block::Block, program::Program, types::ShaderStage};

        let failed = catch_shader_exception(|| {
            let mut program = Program::new(ShaderStage::Fragment);
            program.blocks.push(Block::new());
            TranslatorVisitor::new(&mut program, 0).translate_instruction(0x8000_0000_0007_0000);
        });
        assert_eq!(
            failed.unwrap_err(),
            "Instruction IMAD32I is not implemented is not implemented"
        );

        let next = catch_shader_exception(|| {
            let mut program = Program::new(ShaderStage::Fragment);
            program.blocks.push(Block::new());
            TranslatorVisitor::new(&mut program, 0).translate_nop(0);
            program.blocks[0].is_empty()
        });
        assert!(next.unwrap());
    }

    #[test]
    fn missing_file_environment_data_is_a_caught_shader_error() {
        let env = FileEnvironment::new();
        let result = catch_shader_exception(|| env.read_cbuf_value(2, 0x20));

        assert_eq!(result.unwrap_err(), "Uncached read texture type");
    }

    #[test]
    fn unsupported_geometry_stream_emission_is_caught_and_compilation_can_continue() {
        use shader_recompiler::ir::{basic_block::Block, emitter::Emitter, Program, Value};

        for emit_vertex in [true, false] {
            let result = catch_shader_exception(|| {
                let mut program = Program::new(ShaderStage::Geometry);
                program.blocks.push(Block::new());
                let mut emitter = Emitter::new(&mut program, 0);
                if emit_vertex {
                    emitter.emit_vertex(Value::ImmU32(0));
                } else {
                    emitter.end_primitive(Value::ImmU32(0));
                }
                let profile = shader_recompiler::profile::Profile {
                    support_geometry_streams: false,
                    ..Default::default()
                };
                shader_recompiler::backend::emit_spirv(&program, &profile, &Default::default())
            });
            assert_eq!(result.unwrap_err(), "Geometry streams is not implemented");
        }

        let result = catch_shader_exception(|| {
            let mut program = Program::new(ShaderStage::Fragment);
            program.blocks.push(shader_recompiler::ir::basic_block::Block::new());
            shader_recompiler::backend::emit_spirv(
                &program,
                &Default::default(),
                &Default::default(),
            )
        });
        assert!(!result.unwrap().is_empty());
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
    fn cache_rejects_pre_compact_matrix_entries() {
        // Version 18 persisted expanded Maxwell instructions. Reusing them
        // would bypass the new software matrix collective on a cached load.
        let mut blob = Vec::from(VULKAN_CACHE_MAGIC_NUMBER);
        blob.extend_from_slice(&18u32.to_le_bytes());
        assert_eq!(parse_vulkan_pipeline_cache_blob(&blob, CACHE_VERSION),
            Err(VulkanPipelineCacheHeaderError::VersionMismatch));
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
    fn used_pipelines_round_trip_matches_fritchii_layout() {
        let dir = std::env::temp_dir().join(format!(
            "ruzu-used-pipelines-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vulkan_used.bin");

        // "EUP1", u32 count, then {u64 hash, u8 age} little-endian.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0x3150_5545u32.to_le_bytes());
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&0x99AA_BBCC_DDEE_FF00u64.to_le_bytes());
        bytes.push(7);
        std::fs::write(&path, &bytes).unwrap();

        let loaded = load_used_pipelines(&path, USED_PIPELINES_MAX_AGE);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[&0x1122_3344_5566_7788], 0);
        assert_eq!(loaded[&0x99AA_BBCC_DDEE_FF00], 7);

        // A wrong magic yields an empty list, never an error.
        std::fs::write(&path, [0u8; 8]).unwrap();
        assert!(load_used_pipelines(&path, USED_PIPELINES_MAX_AGE).is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn used_pipelines_age_and_drop_after_max_age() {
        let dir = std::env::temp_dir().join(format!(
            "ruzu-used-aging-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vulkan_used.bin");

        let max = USED_PIPELINES_MAX_AGE;
        let mut loaded = HashMap::new();
        loaded.insert(1u64, 0u8); // used last session
        loaded.insert(2u64, 1u8); // used two sessions ago
        loaded.insert(3u64, max - 2); // one session left before the age limit
        loaded.insert(4u64, max - 1); // `age + 1 == MAX` drops it
        let mut session = std::collections::HashSet::new();
        session.insert(9u64); // used now

        write_used_pipelines(&path, &session, &loaded, max).unwrap();
        let next = load_used_pipelines(&path, max);
        assert_eq!(next[&9], 0);
        assert_eq!(next[&1], 1);
        assert_eq!(next[&2], 2);
        assert_eq!(next[&3], max - 1);
        assert!(!next.contains_key(&4));
        assert_eq!(next.len(), 4);

        // Lichii keeps 16 sessions, 8 on MoltenVK; a list saved with the longer
        // window loses its older entries when read with the shorter one.
        assert_eq!(max_used_pipeline_age(false), 16);
        assert_eq!(max_used_pipeline_age(true), 8);
        let shorter = load_used_pipelines(&path, max_used_pipeline_age(true));
        assert!(!shorter.contains_key(&3));
        assert_eq!(shorter.len(), 3);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn used_pipelines_survive_out_of_range_ages_and_write_atomically() {
        let dir = std::env::temp_dir().join(format!(
            "ruzu-used-defects-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vulkan_used.bin");

        // An age of 255 (damaged or foreign list) must neither overflow nor be
        // kept: `age + 1` on a u8 read from the file used to panic in debug.
        let mut loaded = HashMap::new();
        loaded.insert(1u64, u8::MAX);
        loaded.insert(2u64, 0u8);
        let session = std::collections::HashSet::new();
        write_used_pipelines(&path, &session, &loaded, u8::MAX).unwrap();
        let next = load_used_pipelines(&path, u8::MAX);
        assert_eq!(next.len(), 1);
        assert_eq!(next[&2], 1);

        // The file is replaced through a temporary that does not stay behind,
        // and a rewrite replaces the whole list.
        let mut temporary = path.as_os_str().to_owned();
        temporary.push(".tmp");
        assert!(!std::path::Path::new(&temporary).exists());
        let mut again = std::collections::HashSet::new();
        again.insert(7u64);
        write_used_pipelines(&path, &again, &HashMap::new(), USED_PIPELINES_MAX_AGE).unwrap();
        let next = load_used_pipelines(&path, USED_PIPELINES_MAX_AGE);
        assert_eq!(next.len(), 1);
        assert_eq!(next[&7], 0);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 4 + 4 + 9);

        // A list torn mid-record keeps what was whole, as Lichii's reader.
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 3]).unwrap();
        assert!(load_used_pipelines(&path, USED_PIPELINES_MAX_AGE).is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn boot_gating_builds_recently_used_pipelines_only() {
        let mut used = HashMap::new();
        used.insert(0xAAu64, 0u8);

        // No list yet: the first MAX_PIPELINES_WITHOUT_USED_LIST records only
        // (Lichii `MaxPipelinesWithoutUsedList`), not everything.
        let none = HashMap::new();
        assert!(should_build_graphics_at_boot(0xAA, &none, false, 0));
        assert!(should_build_graphics_at_boot(
            0xBB,
            &none,
            false,
            MAX_PIPELINES_WITHOUT_USED_LIST - 1
        ));
        assert!(!should_build_graphics_at_boot(
            0xBB,
            &none,
            false,
            MAX_PIPELINES_WITHOUT_USED_LIST
        ));

        // A list: only what it names is built at boot, however many are queued.
        assert!(should_build_graphics_at_boot(0xAA, &used, false, 1_000_000));
        assert!(!should_build_graphics_at_boot(0xBB, &used, false, 0));

        // RUZU_PRELOAD_ALL_PIPELINES keeps building all of them.
        assert!(should_build_graphics_at_boot(0xBB, &used, true, 0));
        assert!(should_build_graphics_at_boot(0xBB, &none, true, 1_000_000));
    }

    #[test]
    fn captured_graphics_records_read_back_bounded() {
        use crate::shader_environment::MAGIC_NUMBER;
        use std::io::{Seek as _, Write};
        use shader_recompiler::program_header::ProgramHeader;

        let dir = std::env::temp_dir().join(format!(
            "ruzu-precompile-record-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vulkan.bin");

        // One graphics record in `serialize_pipeline`'s layout: u32 environment
        // count, the environment, then the key. The environment has no facts and
        // an empty program body, which is what a zero-fact translation writes.
        let mut key = GraphicsPipelineKey::default();
        key.unique_hashes[0] = 0x1234_5678_9ABC_DEF0;
        let mut record = Vec::new();
        record.extend_from_slice(&1u32.to_le_bytes());
        for _ in 0..5 {
            record.extend_from_slice(&0u64.to_le_bytes()); // code size and fact counts
        }
        record.extend_from_slice(&0u32.to_le_bytes()); // local memory size
        record.extend_from_slice(&0u32.to_le_bytes()); // texture bound
        record.extend_from_slice(&0u32.to_le_bytes()); // start address
        record.extend_from_slice(&0u32.to_le_bytes()); // cached lowest
        record.extend_from_slice(&0u32.to_le_bytes()); // cached highest
        record.extend_from_slice(&1u32.to_le_bytes()); // viewport transform state
        record.extend_from_slice(&(ShaderStage::Fragment as u32).to_le_bytes());
        record.extend_from_slice(&[0u8; std::mem::size_of::<ProgramHeader>()]);
        record.extend_from_slice(&key.to_cache_bytes());

        let mut file_bytes = Vec::new();
        file_bytes.extend_from_slice(&MAGIC_NUMBER);
        file_bytes.extend_from_slice(&CACHE_VERSION.to_le_bytes());
        assert_eq!(file_bytes.len(), cache_file_header_len());
        let offset = file_bytes.len() as u64;
        file_bytes.extend_from_slice(&record);
        std::fs::write(&path, &file_bytes).unwrap();
        let length = record.len() as u64;

        let mut file = std::fs::File::open(&path).unwrap();
        let (read_key, environments) = read_graphics_record(&mut file, offset, length).unwrap();
        assert_eq!(read_key.unique_hashes[0], 0x1234_5678_9ABC_DEF0);
        assert_eq!(read_key.hash_value(), key.hash_value());
        assert_eq!(environments.len(), 1);
        assert_eq!(environments[0].shader_stage(), ShaderStage::Fragment);

        // A range that does not end where the record does is rejected.
        assert!(read_graphics_record(&mut file, offset, length - 4).is_err());
        assert!(read_graphics_record(&mut file, offset, length + 4).is_err());
        // A range inside the payload cannot parse as a record.
        assert!(read_graphics_record(&mut file, offset + 4, length - 4).is_err());

        // A torn append stays out of the captured range: the same range still reads.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&1u32.to_le_bytes())
            .unwrap();
        assert!(read_graphics_record(&mut file, offset, length).is_ok());
        assert!(file.seek(std::io::SeekFrom::Start(0)).is_ok());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn precompiled_stamp_tracks_file_driver_and_build() {
        let dir = std::env::temp_dir().join(format!(
            "ruzu-precompile-stamp-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let cache = dir.join("vulkan.bin");
        std::fs::write(&cache, [1u8; 16]).unwrap();
        let stamp_path = precompiled_stamp_path(&dir);
        assert_eq!(
            stamp_path.file_name().unwrap().to_str().unwrap(),
            format!("precompiled_prepared.ruzu-v{CACHE_VERSION}.txt")
        );

        let stamp = precompiled_stamp(&cache, 0x1234, "Test Device").unwrap();
        // Fritchii layout: size, modification time, driver, build revision, model.
        assert!(stamp.contains(" 0x1234 "));
        assert!(stamp.ends_with(" Test Device\n"));
        assert!(stamp.contains(common::scm_rev::SCM_REV));
        assert!(stamp.starts_with("16 "));

        // Missing cache file: no stamp, so no preparation.
        assert!(precompiled_stamp(&dir.join("missing.bin"), 0x1234, "Test Device").is_err());

        std::fs::write(&stamp_path, &stamp).unwrap();
        assert_eq!(read_precompiled_stamp(&stamp_path).as_deref(), Some(stamp.as_str()));

        // A new file size changes the stamp, so the step runs again.
        std::fs::write(&cache, [1u8; 17]).unwrap();
        let changed = precompiled_stamp(&cache, 0x1234, "Test Device").unwrap();
        assert_ne!(changed, stamp);
        assert_ne!(read_precompiled_stamp(&stamp_path).as_deref(), Some(changed.as_str()));

        // Another driver or device also invalidates it.
        assert_ne!(
            precompiled_stamp(&cache, 0x9999, "Test Device").unwrap(),
            changed
        );
        assert_ne!(
            precompiled_stamp(&cache, 0x1234, "Other Device").unwrap(),
            changed
        );

        std::fs::remove_dir_all(&dir).unwrap();
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
        let expected = common::thread::hardware_concurrency().max(2) - 1;
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
            texture_pool_state: None,
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
            texture_pool_state: None,
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
    fn compute_pipeline_cache_key_serialization_has_explicit_dependency_tail() {
        let key = ComputePipelineCacheKey {
            unique_hash: 17, shared_memory_size: 32, workgroup_size: [1, 2, 3],
            texture_pool_state: None,
        };
        let bytes = compute_key_to_cache_bytes(&key);
        assert_eq!(bytes.len(), 40);
        assert_eq!(&bytes[..8], &17u64.to_le_bytes());
        assert_eq!(&bytes[24..], &[0; 16]);
        assert_eq!(key.hash_value(), city_hash64(&bytes));
    }

    #[test]
    fn pipeline_keys_roundtrip_pool_dependencies_and_separate_variants() {
        use std::io::{Seek, SeekFrom, Write};
        let path = std::env::temp_dir().join(format!("ruzu-pool-keys-{}-{}.bin",
            std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let mut file = std::fs::OpenOptions::new().create_new(true).read(true).write(true).open(&path).unwrap();
        let states = [None,
            Some(TexturePoolState { texture_limit: 7, sampler_limit: 3, linked_sampler: false }),
            Some(TexturePoolState { texture_limit: 15, sampler_limit: 3, linked_sampler: false }),
            Some(TexturePoolState { texture_limit: 7, sampler_limit: 7, linked_sampler: false }),
            Some(TexturePoolState { texture_limit: 7, sampler_limit: 3, linked_sampler: true }),
        ];
        let mut graphics_keys = std::collections::HashSet::new();
        let mut compute_keys = std::collections::HashSet::new();
        for state in states {
            let compute = ComputePipelineCacheKey {
                unique_hash: 29, shared_memory_size: 64, workgroup_size: [1, 2, 3],
                texture_pool_state: state,
            };
            let graphics = GraphicsPipelineKey { texture_pool_state: state, ..Default::default() };
            graphics_keys.insert(graphics.clone());
            compute_keys.insert(compute);
            file.seek(SeekFrom::Start(0)).unwrap();
            file.write_all(&compute_key_to_cache_bytes(&compute)).unwrap();
            file.write_all(&graphics.to_cache_bytes()).unwrap();
            let end = file.stream_position().unwrap();
            file.seek(SeekFrom::Start(0)).unwrap();
            assert_eq!(ComputePipelineCacheKey::read_from_file(&mut file).unwrap(), compute);
            assert_eq!(GraphicsPipelineKey::read_from_file(&mut file).unwrap(), graphics);
            assert_eq!(file.stream_position().unwrap(), end);
            assert_eq!(graphics.hash_value(), city_hash64(&graphics.to_cache_bytes()));
            assert_eq!(compute.hash_value(), city_hash64(&compute_key_to_cache_bytes(&compute)));
        }
        assert_eq!(graphics_keys.len(), states.len());
        assert_eq!(compute_keys.len(), states.len());
        // Reject noncanonical absent values, invalid tags/booleans, and
        // inclusive limits whose descriptor counts would overflow.
        for words in [
            [0u32, 1, 0, 0],
            [2, 0, 0, 0],
            [1, 7, 3, 2],
            [1, u32::MAX, 3, 0],
            [1, 7, u32::MAX, 0],
        ] {
            file.seek(SeekFrom::Start(0)).unwrap();
            for word in words {
                file.write_all(&word.to_le_bytes()).unwrap();
            }
            file.seek(SeekFrom::Start(0)).unwrap();
            assert_eq!(read_texture_pool_key(&mut file).unwrap_err().kind(), std::io::ErrorKind::InvalidData);
        }
        for size in 0..16 {
            file.set_len(size).unwrap();
            file.seek(SeekFrom::Start(0)).unwrap();
            assert_eq!(read_texture_pool_key(&mut file).unwrap_err().kind(), std::io::ErrorKind::UnexpectedEof);
        }
        drop(file);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn compute_cache_applies_upstream_unordered_dense_post_mix() {
        use std::hash::BuildHasher;

        let key = ComputePipelineCacheKey {
            texture_pool_state: None,
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
            texture_pool_state: None,
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
