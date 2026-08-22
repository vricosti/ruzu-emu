// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Backend-neutral graphics shader runtime metadata.
//!
//! Vulkan and Metal consume the same normalized shader IR. State-dependent
//! `RuntimeInfo` construction therefore belongs above either renderer.

use shader_recompiler::host_translate_info::HostTranslateInfo;
use shader_recompiler::runtime_info::{
    AttributeType, CompareFunction, InputTopology, RuntimeInfo, TessPrimitive, TessSpacing,
};
use shader_recompiler::shader_info::Info as ShaderInfo;
use shader_recompiler::ShaderStage;

use crate::buffer_cache::buffer_cache_base::{
    UniformBufferSizes, NUM_GRAPHICS_UNIFORM_BUFFERS, NUM_STAGES,
};
use crate::engines::maxwell_3d::{ComparisonOp, PrimitiveTopology, VertexAttribType};
use crate::renderer_vulkan::fixed_pipeline_state::{FixedPipelineState, VertexAttribute};
use crate::shader_cache::{GraphicsEnvironments, NUM_PROGRAMS};

pub const NUM_GRAPHICS_STAGES: usize = 5;

/// Backend-neutral result of Maxwell frontend translation and normalization.
///
/// Vulkan emits SPIR-V from this object. Metal retains the same `Program` so
/// it can emit direct MSL and compare it with the temporary SPIR-V oracle.
pub struct TranslatedGraphicsShader {
    pub program: shader_recompiler::ir::Program,
    pub runtime_info: RuntimeInfo,
}

pub fn buffer_cache_metadata(
    stage_infos: &[ShaderInfo; NUM_GRAPHICS_STAGES],
) -> ([u32; NUM_STAGES as usize], UniformBufferSizes) {
    let mut masks = [0u32; NUM_STAGES as usize];
    let mut sizes = [[0u32; NUM_GRAPHICS_UNIFORM_BUFFERS as usize]; NUM_STAGES as usize];
    for stage in 0..NUM_STAGES as usize {
        let info = &stage_infos[stage];
        masks[stage] = info.constant_buffer_mask;
        sizes[stage].copy_from_slice(&info.constant_buffer_used_sizes);
    }
    (masks, sizes)
}

/// Translate all enabled graphics stages into the shared normalized IR.
///
/// Stage ordering and previous-stage metadata propagation match Eden's
/// `PipelineCache::CreateGraphicsPipeline`: VertexA/VertexB merge first,
/// followed by tessellation control/evaluation, geometry, and fragment.
pub fn translate_graphics_stages_from_environments(
    host_info: &HostTranslateInfo,
    fixed_state: &FixedPipelineState,
    unique_hashes: &[u64; NUM_PROGRAMS],
    pipeline_hash: u64,
    environments: &mut GraphicsEnvironments,
) -> Option<[Option<TranslatedGraphicsShader>; NUM_GRAPHICS_STAGES]> {
    let mut translated: [Option<TranslatedGraphicsShader>; NUM_GRAPHICS_STAGES] =
        Default::default();
    let uses_vertex_a = environment_has_stage(environments, 0);
    let uses_vertex_b = environment_has_stage(environments, 1);
    let dump_guest_shaders = *common::settings::values().dump_guest_shaders.get_value();
    if !uses_vertex_b {
        return None;
    }

    let vertex_runtime_info =
        make_runtime_info(fixed_state, unique_hashes, ShaderStage::VertexB, None);
    let vertex_program = if uses_vertex_a {
        let (vertex_a, vertex_b) = two_mut(&mut environments.envs, 0, 1)?;
        if vertex_a
            .generic_environment()
            .cached_code_slice()
            .is_empty()
            && vertex_a.generic_environment_mut().analyze().is_none()
        {
            return None;
        }
        if vertex_b
            .generic_environment()
            .cached_code_slice()
            .is_empty()
            && vertex_b.generic_environment_mut().analyze().is_none()
        {
            return None;
        }
        let vertex_a_code = vertex_a
            .generic_environment()
            .cached_instruction_slice()
            .to_vec();
        let vertex_b_code = vertex_b
            .generic_environment()
            .cached_instruction_slice()
            .to_vec();
        if vertex_a_code.is_empty() || vertex_b_code.is_empty() {
            return None;
        }
        let vertex_a_offset = vertex_a.generic_environment().cached_instruction_start();
        let vertex_b_offset = vertex_b.generic_environment().cached_instruction_start();
        let program = shader_recompiler::translate_dual_vertex_shader_from_env_with_host_info(
            &vertex_a_code,
            vertex_a_offset,
            vertex_a,
            &vertex_b_code,
            vertex_b_offset,
            vertex_b,
            &vertex_runtime_info,
            host_info,
        );
        if dump_guest_shaders {
            vertex_a
                .generic_environment_mut()
                .dump(pipeline_hash, unique_hashes[0]);
            vertex_b
                .generic_environment_mut()
                .dump(pipeline_hash, unique_hashes[1]);
        }
        program
    } else {
        let program =
            translate_stage_from_environment(host_info, environments, 1, &vertex_runtime_info)?;
        if dump_guest_shaders {
            environments.envs[1]
                .generic_environment_mut()
                .dump(pipeline_hash, unique_hashes[1]);
        }
        program
    };
    let mut previous_stage_info = Some(vertex_program.info.clone());
    translated[0] = Some(TranslatedGraphicsShader {
        program: vertex_program,
        runtime_info: vertex_runtime_info,
    });

    for program_index in 2..NUM_PROGRAMS {
        if !environment_has_stage(environments, program_index) {
            continue;
        }
        let stage = environments.envs[program_index]
            .generic_environment()
            .shader_stage();
        let stage_index = shader_stage_to_graphics_info_index(stage)?;
        let runtime_info = make_runtime_info(
            fixed_state,
            unique_hashes,
            stage,
            previous_stage_info.as_ref(),
        );
        let program = translate_stage_from_environment(
            host_info,
            environments,
            program_index,
            &runtime_info,
        )?;
        if dump_guest_shaders {
            environments.envs[program_index]
                .generic_environment_mut()
                .dump(pipeline_hash, unique_hashes[program_index]);
        }
        previous_stage_info = Some(program.info.clone());
        translated[stage_index] = Some(TranslatedGraphicsShader {
            program,
            runtime_info,
        });
    }
    Some(translated)
}

fn translate_stage_from_environment(
    host_info: &HostTranslateInfo,
    environments: &mut GraphicsEnvironments,
    stage_index: usize,
    runtime_info: &RuntimeInfo,
) -> Option<shader_recompiler::ir::Program> {
    if stage_index >= NUM_PROGRAMS || !environment_has_stage(environments, stage_index) {
        return None;
    }
    let env = &mut environments.envs[stage_index];
    if env.generic_environment().cached_code_slice().is_empty()
        && (!env.generic_environment().has_runtime_gpu_memory_owner()
            || env.generic_environment_mut().analyze().is_none())
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
    let base_offset = env.generic_environment().cached_instruction_start();
    Some(shader_recompiler::translate_shader_from_env_with_host_info(
        &code,
        base_offset,
        env,
        runtime_info,
        host_info,
    ))
}

fn environment_has_stage(environments: &GraphicsEnvironments, stage_index: usize) -> bool {
    environments
        .env_ptrs
        .iter()
        .flatten()
        .any(|&index| index == stage_index)
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

fn shader_stage_to_graphics_info_index(stage: ShaderStage) -> Option<usize> {
    match stage {
        ShaderStage::VertexA | ShaderStage::VertexB => Some(0),
        ShaderStage::TessellationControl => Some(1),
        ShaderStage::TessellationEval => Some(2),
        ShaderStage::Geometry => Some(3),
        ShaderStage::Fragment => Some(4),
        ShaderStage::Compute => None,
    }
}

pub fn make_runtime_info(
    fixed_state: &FixedPipelineState,
    unique_hashes: &[u64; NUM_PROGRAMS],
    stage: ShaderStage,
    previous_program: Option<&ShaderInfo>,
) -> RuntimeInfo {
    let mut info = RuntimeInfo::default();
    if let Some(previous_program) = previous_program {
        info.previous_stage_stores = previous_program.stores.clone();
        info.previous_stage_legacy_stores_mapping = previous_program.legacy_stores_mapping.clone();
    } else {
        info.previous_stage_stores.mask.fill(u64::MAX);
    }
    match stage {
        ShaderStage::VertexB => {
            let has_geometry = unique_hashes[4] != 0;
            if !has_geometry {
                if fixed_state.topology() == PrimitiveTopology::Points {
                    info.fixed_state_point_size = Some(f32::from_bits(fixed_state.point_size));
                }
                if fixed_state.xfb_enabled() {
                    fill_transform_feedback_runtime_info(&mut info, fixed_state);
                }
                info.convert_depth_mode = fixed_state.ndc_minus_one_to_one();
            }
            for (index, attrib) in fixed_state.attributes.iter().enumerate() {
                info.generic_input_types[index] = if fixed_state.dynamic_vertex_input() {
                    attribute_type_from_dynamic_state(fixed_state.dynamic_attribute_type(index))
                } else {
                    cast_attribute_type_from_state(*attrib)
                };
            }
        }
        ShaderStage::TessellationEval => {
            info.tess_clockwise = fixed_state.tessellation_clockwise();
            info.tess_primitive = tess_primitive_from_state(fixed_state.tessellation_primitive());
            info.tess_spacing = tess_spacing_from_state(fixed_state.tessellation_spacing());
        }
        ShaderStage::Geometry => {
            if fixed_state.xfb_enabled() {
                fill_transform_feedback_runtime_info(&mut info, fixed_state);
            }
            info.convert_depth_mode = fixed_state.ndc_minus_one_to_one();
        }
        ShaderStage::Fragment => {
            info.alpha_test_func =
                Some(compare_function_from_maxwell(fixed_state.alpha_test_func()));
            info.alpha_test_reference = f32::from_bits(fixed_state.alpha_test_ref);
            info.dual_source_blend = fixed_state.attachment0_dual_source_blend();
            for (index, &format) in fixed_state.color_formats.iter().enumerate() {
                if format == 0 {
                    info.frag_color_types[index] = AttributeType::Float;
                    continue;
                }
                let pixel_format =
                    crate::surface::pixel_format_from_render_target_format(format as u32);
                info.frag_color_types[index] =
                    if crate::surface::is_pixel_format_signed_integer(pixel_format) {
                        AttributeType::SignedInt
                    } else if crate::surface::is_pixel_format_integer(pixel_format) {
                        AttributeType::UnsignedInt
                    } else {
                        AttributeType::Float
                    };
            }
        }
        _ => {}
    }
    info.input_topology = input_topology_from_state(fixed_state.topology());
    info.force_early_z = fixed_state.early_z();
    info.y_negate = fixed_state.y_negate();
    info
}

fn fill_transform_feedback_runtime_info(info: &mut RuntimeInfo, fixed_state: &FixedPipelineState) {
    let (varyings, count) =
        crate::transform_feedback::make_transform_feedback_varyings(&fixed_state.xfb_state);
    info.xfb_varyings = varyings
        .iter()
        .map(
            |varying| shader_recompiler::runtime_info::TransformFeedbackVarying {
                buffer: varying.buffer,
                stream: varying.stream,
                stride: varying.stride,
                offset: varying.offset,
                components: varying.components,
            },
        )
        .collect();
    info.xfb_count = count;
}

fn attribute_type_from_dynamic_state(value: u32) -> AttributeType {
    match value {
        0 => AttributeType::Disabled,
        1 => AttributeType::Float,
        2 => AttributeType::SignedInt,
        3 => AttributeType::UnsignedInt,
        _ => AttributeType::Disabled,
    }
}

fn tess_primitive_from_state(value: u32) -> TessPrimitive {
    match value {
        0 => TessPrimitive::Isolines,
        1 => TessPrimitive::Triangles,
        2 => TessPrimitive::Quads,
        _ => TessPrimitive::Triangles,
    }
}

fn tess_spacing_from_state(value: u32) -> TessSpacing {
    match value {
        0 => TessSpacing::Equal,
        1 => TessSpacing::FractionalOdd,
        2 => TessSpacing::FractionalEven,
        _ => TessSpacing::Equal,
    }
}

fn compare_function_from_maxwell(op: ComparisonOp) -> CompareFunction {
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

fn input_topology_from_state(topology: PrimitiveTopology) -> InputTopology {
    match topology {
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
    }
}

fn cast_attribute_type_from_state(attrib: VertexAttribute) -> AttributeType {
    if !attrib.is_enabled() {
        return AttributeType::Disabled;
    }
    match VertexAttribType::from_raw(attrib.attrib_type()) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_info_preserves_fixed_graphics_state() {
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
        let unique_hashes = [0, 1, 0, 3, 0, 5];

        let vertex = make_runtime_info(&fixed_state, &unique_hashes, ShaderStage::VertexB, None);
        assert_eq!(vertex.fixed_state_point_size, Some(1.5));
        assert!(vertex.convert_depth_mode);
        assert!(vertex.force_early_z);
        assert!(vertex.y_negate);
        assert_eq!(vertex.input_topology, InputTopology::Points);

        let tess = make_runtime_info(
            &fixed_state,
            &unique_hashes,
            ShaderStage::TessellationEval,
            None,
        );
        assert_eq!(tess.tess_primitive, TessPrimitive::Quads);
        assert_eq!(tess.tess_spacing, TessSpacing::FractionalOdd);
        assert!(tess.tess_clockwise);

        let fragment = make_runtime_info(&fixed_state, &unique_hashes, ShaderStage::Fragment, None);
        assert_eq!(fragment.alpha_test_func, Some(CompareFunction::Greater));
        assert_eq!(fragment.alpha_test_reference, 0.25);
        assert!(fragment.dual_source_blend);
    }

    #[test]
    fn buffer_metadata_is_derived_from_stage_shader_info() {
        let mut infos: [ShaderInfo; NUM_GRAPHICS_STAGES] = Default::default();
        infos[0].constant_buffer_mask = 0x15;
        infos[0].constant_buffer_used_sizes[2] = 0x180;
        infos[4].constant_buffer_mask = 0x80;
        infos[4].constant_buffer_used_sizes[7] = 0x240;

        let (masks, sizes) = buffer_cache_metadata(&infos);

        assert_eq!(masks[0], 0x15);
        assert_eq!(masks[4], 0x80);
        assert_eq!(sizes[0][2], 0x180);
        assert_eq!(sizes[4][7], 0x240);
    }
}
