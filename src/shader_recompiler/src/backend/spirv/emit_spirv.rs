// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Top-level SPIR-V emission — maps to upstream `backend/spirv/emit_spirv.h` and
//! `emit_spirv.cpp`.
//!
//! Contains the `EmitSPIRV` entry point and constants for rescaling/render area
//! push constant layouts.

use super::spirv_emit_context::SpirvEmitContext;
use crate::backend::bindings::Bindings;
use crate::ir;
use crate::ir::types::{OutputTopology, ShaderStage};
use crate::ir::value::Attribute;
use crate::runtime_info::{InputTopology, RuntimeInfo, TessPrimitive, TessSpacing};
use crate::shader_info::Info;
use crate::Profile;
use rspirv::spirv;

/// Number of u32 words for texture rescaling data.
pub const NUM_TEXTURE_SCALING_WORDS: u32 = 4;

/// Number of u32 words for image rescaling data.
pub const NUM_IMAGE_SCALING_WORDS: u32 = 2;

/// Combined texture + image rescaling word count.
pub const NUM_TEXTURE_AND_IMAGE_SCALING_WORDS: u32 =
    NUM_TEXTURE_SCALING_WORDS + NUM_IMAGE_SCALING_WORDS;

/// Rescaling push constant layout.
///
/// Matches upstream `RescalingLayout`:
/// ```c
/// struct RescalingLayout {
///     alignas(16) std::array<u32, 4> rescaling_textures;
///     alignas(16) std::array<u32, 2> rescaling_images;
///     u32 down_factor;
/// };
/// ```
#[repr(C, align(16))]
pub struct RescalingLayout {
    pub rescaling_textures: [u32; NUM_TEXTURE_SCALING_WORDS as usize],
    pub rescaling_images: [u32; NUM_IMAGE_SCALING_WORDS as usize],
    pub down_factor: u32,
}

/// Render area push constant layout.
#[repr(C)]
pub struct RenderAreaLayout {
    pub render_area: [f32; 4],
}

/// Byte offset of `rescaling_textures` within `RescalingLayout`.
pub const RESCALING_LAYOUT_WORDS_OFFSET: u32 = 0;

/// Byte offset of `down_factor` within `RescalingLayout`.
pub const RESCALING_LAYOUT_DOWN_FACTOR_OFFSET: u32 = {
    // rescaling_textures: 16 bytes (align(16))
    // rescaling_images: 8 bytes padded to 16
    // down_factor at offset 24
    // But with repr(C, align(16)) the actual offset depends on padding.
    // Upstream: offsetof(RescalingLayout, down_factor)
    // textures = 4 * 4 = 16 bytes at offset 0
    // images = 2 * 4 = 8 bytes at offset 16
    // down_factor at offset 24
    24
};

/// Byte offset of `render_area` within `RenderAreaLayout`.
pub const RENDERAREA_LAYOUT_OFFSET: u32 = 0;

fn tess_primitive_execution_mode(primitive: TessPrimitive) -> spirv::ExecutionMode {
    match primitive {
        TessPrimitive::Isolines => spirv::ExecutionMode::Isolines,
        TessPrimitive::Triangles => spirv::ExecutionMode::Triangles,
        TessPrimitive::Quads => spirv::ExecutionMode::Quads,
    }
}

fn tess_spacing_execution_mode(spacing: TessSpacing) -> spirv::ExecutionMode {
    match spacing {
        TessSpacing::Equal => spirv::ExecutionMode::SpacingEqual,
        TessSpacing::FractionalOdd => spirv::ExecutionMode::SpacingFractionalOdd,
        TessSpacing::FractionalEven => spirv::ExecutionMode::SpacingFractionalEven,
    }
}

/// Port of upstream `DefineEntryPoint`.
fn define_entry_point(program: &ir::Program, ctx: &mut SpirvEmitContext, main: spirv::Word) {
    let execution_model = match program.stage {
        ShaderStage::Compute => {
            ctx.builder.execution_mode(
                main,
                spirv::ExecutionMode::LocalSize,
                program.workgroup_size.to_vec(),
            );
            spirv::ExecutionModel::GLCompute
        }
        ShaderStage::VertexB => spirv::ExecutionModel::Vertex,
        ShaderStage::TessellationControl => {
            ctx.builder.capability(spirv::Capability::Tessellation);
            ctx.builder.execution_mode(
                main,
                spirv::ExecutionMode::OutputVertices,
                vec![program.invocations],
            );
            spirv::ExecutionModel::TessellationControl
        }
        ShaderStage::TessellationEval => {
            ctx.builder.capability(spirv::Capability::Tessellation);
            ctx.builder.execution_mode(
                main,
                tess_primitive_execution_mode(ctx.runtime_info.tess_primitive),
                vec![],
            );
            ctx.builder.execution_mode(
                main,
                tess_spacing_execution_mode(ctx.runtime_info.tess_spacing),
                vec![],
            );
            ctx.builder.execution_mode(
                main,
                if ctx.runtime_info.tess_clockwise {
                    spirv::ExecutionMode::VertexOrderCw
                } else {
                    spirv::ExecutionMode::VertexOrderCcw
                },
                vec![],
            );
            spirv::ExecutionModel::TessellationEvaluation
        }
        ShaderStage::Geometry => {
            ctx.builder.capability(spirv::Capability::Geometry);
            if ctx.profile.support_geometry_streams {
                ctx.builder.capability(spirv::Capability::GeometryStreams);
            }
            let input_mode = match ctx.runtime_info.input_topology {
                InputTopology::Points => spirv::ExecutionMode::InputPoints,
                InputTopology::Lines => spirv::ExecutionMode::InputLines,
                InputTopology::LinesAdjacency => spirv::ExecutionMode::InputLinesAdjacency,
                InputTopology::Triangles => spirv::ExecutionMode::Triangles,
                InputTopology::TrianglesAdjacency => spirv::ExecutionMode::InputTrianglesAdjacency,
            };
            ctx.builder.execution_mode(main, input_mode, vec![]);
            let output_mode = match program.output_topology {
                OutputTopology::PointList => spirv::ExecutionMode::OutputPoints,
                OutputTopology::LineStrip => spirv::ExecutionMode::OutputLineStrip,
                OutputTopology::TriangleStrip => spirv::ExecutionMode::OutputTriangleStrip,
            };
            ctx.builder.execution_mode(main, output_mode, vec![]);
            if program.info.stores.get(Attribute::POINT_SIZE.0 as usize) {
                ctx.builder.capability(spirv::Capability::GeometryPointSize);
            }
            ctx.builder.execution_mode(
                main,
                spirv::ExecutionMode::OutputVertices,
                vec![program.output_vertices],
            );
            ctx.builder.execution_mode(
                main,
                spirv::ExecutionMode::Invocations,
                vec![program.invocations],
            );
            if program.is_geometry_passthrough {
                if ctx.profile.support_geometry_shader_passthrough {
                    ctx.builder.extension("SPV_NV_geometry_shader_passthrough");
                    ctx.builder
                        .capability(spirv::Capability::GeometryShaderPassthroughNV);
                } else {
                    log::warn!("Geometry shader passthrough used with no support");
                }
            }
            spirv::ExecutionModel::Geometry
        }
        ShaderStage::Fragment => {
            ctx.builder.execution_mode(
                main,
                if ctx.profile.lower_left_origin_mode {
                    spirv::ExecutionMode::OriginLowerLeft
                } else {
                    spirv::ExecutionMode::OriginUpperLeft
                },
                vec![],
            );
            if program.info.stores_frag_depth {
                ctx.builder
                    .execution_mode(main, spirv::ExecutionMode::DepthReplacing, vec![]);
            }
            if ctx.runtime_info.force_early_z {
                ctx.builder
                    .execution_mode(main, spirv::ExecutionMode::EarlyFragmentTests, vec![]);
            }
            spirv::ExecutionModel::Fragment
        }
        ShaderStage::VertexA => {
            unreachable!("VertexA must be merged into VertexB before SPIR-V emission")
        }
    };
    ctx.builder
        .entry_point(execution_model, main, "main", ctx.interfaces.clone());
}

fn add_float_execution_mode(
    ctx: &mut SpirvEmitContext,
    main: spirv::Word,
    capability: spirv::Capability,
    mode: spirv::ExecutionMode,
    width: u32,
) {
    ctx.builder.capability(capability);
    ctx.builder.execution_mode(main, mode, vec![width]);
}

/// Port of upstream `SetupDenormControl`.
fn setup_denorm_control(
    profile: &Profile,
    program: &ir::Program,
    ctx: &mut SpirvEmitContext,
    main: spirv::Word,
) {
    let info = &program.info;
    if !(info.uses_fp32_denorms_flush && info.uses_fp32_denorms_preserve) {
        if info.uses_fp32_denorms_flush
            && profile.support_fp32_denorm_flush
            && !profile.has_broken_fp32_denorm_flush
        {
            add_float_execution_mode(
                ctx,
                main,
                spirv::Capability::DenormFlushToZero,
                spirv::ExecutionMode::DenormFlushToZero,
                32,
            );
        } else if info.uses_fp32_denorms_preserve && profile.support_fp32_denorm_preserve {
            add_float_execution_mode(
                ctx,
                main,
                spirv::Capability::DenormPreserve,
                spirv::ExecutionMode::DenormPreserve,
                32,
            );
        }
    }
    if !profile.support_separate_denorm_behavior || profile.has_broken_fp16_float_controls {
        return;
    }
    if !(info.uses_fp16_denorms_flush && info.uses_fp16_denorms_preserve) {
        if info.uses_fp16_denorms_flush && profile.support_fp16_denorm_flush {
            add_float_execution_mode(
                ctx,
                main,
                spirv::Capability::DenormFlushToZero,
                spirv::ExecutionMode::DenormFlushToZero,
                16,
            );
        } else if info.uses_fp16_denorms_preserve && profile.support_fp16_denorm_preserve {
            add_float_execution_mode(
                ctx,
                main,
                spirv::Capability::DenormPreserve,
                spirv::ExecutionMode::DenormPreserve,
                16,
            );
        }
    }
}

/// Port of upstream `SetupSignedNanCapabilities`.
fn setup_signed_nan_capabilities(
    profile: &Profile,
    program: &ir::Program,
    ctx: &mut SpirvEmitContext,
    main: spirv::Word,
) {
    if profile.has_broken_fp16_float_controls && program.info.uses_fp16 {
        return;
    }
    if program.info.uses_fp16 && profile.support_fp16_signed_zero_nan_preserve {
        add_float_execution_mode(
            ctx,
            main,
            spirv::Capability::SignedZeroInfNanPreserve,
            spirv::ExecutionMode::SignedZeroInfNanPreserve,
            16,
        );
    }
    if profile.support_fp32_signed_zero_nan_preserve {
        add_float_execution_mode(
            ctx,
            main,
            spirv::Capability::SignedZeroInfNanPreserve,
            spirv::ExecutionMode::SignedZeroInfNanPreserve,
            32,
        );
    }
    if program.info.uses_fp64 && profile.support_fp64_signed_zero_nan_preserve {
        add_float_execution_mode(
            ctx,
            main,
            spirv::Capability::SignedZeroInfNanPreserve,
            spirv::ExecutionMode::SignedZeroInfNanPreserve,
            64,
        );
    }
}

fn setup_float_controls(
    profile: &Profile,
    program: &ir::Program,
    ctx: &mut SpirvEmitContext,
    main: spirv::Word,
) {
    if !profile.support_float_controls {
        return;
    }
    ctx.builder.extension("SPV_KHR_float_controls");
    setup_denorm_control(profile, program, ctx, main);
    setup_signed_nan_capabilities(profile, program, ctx, main);
}

/// Port of upstream `SetupCapabilities`.
pub(crate) fn setup_capabilities(
    profile: &Profile,
    info: &Info,
    stage: ShaderStage,
    ctx: &mut SpirvEmitContext,
) {
    if info.uses_sampled_1d {
        ctx.builder.capability(spirv::Capability::Sampled1D);
    }
    if info.uses_image_1d {
        ctx.builder.capability(spirv::Capability::Image1D);
    }
    if info.uses_sparse_residency {
        ctx.builder.capability(spirv::Capability::SparseResidency);
    }
    if info.uses_demote_to_helper_invocation && profile.support_demote_to_helper_invocation {
        if profile.supported_spirv < 0x0001_0600 {
            ctx.builder.extension("SPV_EXT_demote_to_helper_invocation");
        }
        ctx.builder
            .capability(spirv::Capability::DemoteToHelperInvocation);
    }
    if info.stores.get(Attribute::VIEWPORT_INDEX.0 as usize) && profile.support_multi_viewport {
        ctx.builder.capability(spirv::Capability::MultiViewport);
    }
    if info.stores.get(Attribute::VIEWPORT_MASK.0 as usize) && profile.support_viewport_mask {
        ctx.builder.extension("SPV_NV_viewport_array2");
        ctx.builder
            .capability(spirv::Capability::ShaderViewportMaskNV);
    }
    if (info.stores.get(Attribute::LAYER.0 as usize)
        || info.stores.get(Attribute::VIEWPORT_INDEX.0 as usize))
        && profile.support_viewport_index_layer_non_geometry
        && stage != ShaderStage::Geometry
    {
        ctx.builder.extension("SPV_EXT_shader_viewport_index_layer");
        ctx.builder
            .capability(spirv::Capability::ShaderViewportIndexLayerEXT);
    }
    if !profile.support_vertex_instance_id
        && (info.loads.get(Attribute::INSTANCE_ID.0 as usize)
            || info.loads.get(Attribute::VERTEX_ID.0 as usize))
    {
        ctx.builder.extension("SPV_KHR_shader_draw_parameters");
        ctx.builder.capability(spirv::Capability::DrawParameters);
    }
    if (info.uses_subgroup_vote || info.uses_subgroup_invocation_id || info.uses_subgroup_shuffles)
        && profile.support_vote
        && profile.supports_subgroup_stage(stage)
    {
        ctx.builder
            .capability(spirv::Capability::GroupNonUniformBallot);
        ctx.builder
            .capability(spirv::Capability::GroupNonUniformShuffle);
        if !profile.warp_size_potentially_larger_than_guest {
            ctx.builder
                .capability(spirv::Capability::GroupNonUniformVote);
        }
    }
    if info.uses_int64_bit_atomics && profile.support_int64_atomics {
        ctx.builder.capability(spirv::Capability::Int64Atomics);
    }
    if info.uses_typeless_image_reads && profile.support_typeless_image_loads {
        ctx.builder
            .capability(spirv::Capability::StorageImageReadWithoutFormat);
    }
    if info.uses_typeless_image_writes {
        ctx.builder
            .capability(spirv::Capability::StorageImageWriteWithoutFormat);
    }
    if info.uses_image_buffers {
        ctx.builder.capability(spirv::Capability::ImageBuffer);
    }
    if info.uses_sample_id {
        ctx.builder.capability(spirv::Capability::SampleRateShading);
    }
    if info.uses_derivatives {
        ctx.builder.capability(spirv::Capability::DerivativeControl);
    }
    ctx.builder
        .capability(spirv::Capability::ImageGatherExtended);
    ctx.builder.capability(spirv::Capability::ImageQuery);
    ctx.builder.capability(spirv::Capability::SampledBuffer);
    if !ctx.non_uniform_ids.is_empty() {
        if ctx.profile.supported_spirv < 0x0001_0500 {
            ctx.builder.extension("SPV_EXT_descriptor_indexing");
        }
        ctx.builder.capability(spirv::Capability::ShaderNonUniform);
        if ctx.uses_nonuniform_sampled_image {
            ctx.builder
                .capability(spirv::Capability::SampledImageArrayNonUniformIndexing);
        }
        if ctx.uses_nonuniform_storage_image {
            ctx.builder
                .capability(spirv::Capability::StorageImageArrayNonUniformIndexing);
        }
        if ctx.uses_nonuniform_uniform_texel_buffer {
            ctx.builder
                .capability(spirv::Capability::UniformTexelBufferArrayNonUniformIndexing);
        }
        if ctx.uses_nonuniform_storage_texel_buffer {
            ctx.builder
                .capability(spirv::Capability::StorageTexelBufferArrayNonUniformIndexing);
        }
    }
}

/// Port of upstream `SetupTransformFeedbackCapabilities`.
fn setup_transform_feedback_capabilities(ctx: &mut SpirvEmitContext, main: spirv::Word) {
    if ctx.runtime_info.xfb_count == 0 {
        return;
    }
    ctx.builder.capability(spirv::Capability::TransformFeedback);
    ctx.builder
        .execution_mode(main, spirv::ExecutionMode::Xfb, vec![]);
}

/// Port of upstream `Traverse` and `DefineMain` from `emit_spirv.cpp`.
fn define_main(ctx: &mut SpirvEmitContext, program: &ir::Program) -> spirv::Word {
    let syntax_list = if program.syntax_list.is_empty() {
        let mut list = Vec::with_capacity(program.blocks.len() + 1);
        for block_idx in 0..program.blocks.len() as u32 {
            list.push(ir::SyntaxNode::Block(block_idx));
        }
        list.push(ir::SyntaxNode::Return);
        list
    } else {
        program.syntax_list.clone()
    };

    // Upstream creates one Private counter for each Repeat node while
    // traversing the function. rspirv appends OpVariable to the selected
    // block once function emission has started, so create the same globals
    // beforehand and consume them in syntax-list order below.
    let loop_safety_enabled = !*common::settings::values()
        .disable_shader_loop_safety_checks
        .get_value();
    let repeat_count = syntax_list
        .iter()
        .filter(|node| matches!(node, ir::SyntaxNode::Repeat { .. }))
        .count();
    let mut safety_counters = Vec::with_capacity(repeat_count);
    if loop_safety_enabled && repeat_count != 0 {
        let safety_counter_initial = ctx.builder.constant_bit32(ctx.u32_type, 0x2000);
        for _ in 0..repeat_count {
            let safety_counter = ctx.builder.variable(
                ctx.private_u32_ptr,
                None,
                spirv::StorageClass::Private,
                Some(safety_counter_initial),
            );
            if ctx.profile.supported_spirv >= 0x0001_0400 {
                ctx.interfaces.push(safety_counter);
            }
            safety_counters.push(safety_counter);
        }
    }
    let mut safety_counters = safety_counters.into_iter();

    let main = ctx
        .builder
        .begin_function(
            ctx.void_type,
            None,
            spirv::FunctionControl::NONE,
            ctx.void_fn_type,
        )
        .unwrap();
    ctx.block_labels = (0..program.blocks.len())
        .map(|_| ctx.builder.id())
        .collect();

    let mut current_block = None;
    for node in &syntax_list {
        match *node {
            ir::SyntaxNode::Block(block_idx) => {
                let label = ctx.block_labels[block_idx as usize];
                if current_block.is_some() {
                    ctx.builder.branch(label).unwrap();
                }
                current_block = Some(block_idx);
                ctx.begin_ir_block(block_idx);
                ctx.emit_block_instructions(program, block_idx);
            }
            ir::SyntaxNode::If { cond, body, merge } => {
                let if_label = ctx.block_labels[body as usize];
                let endif_label = ctx.block_labels[merge as usize];
                let cond = ctx.resolve_value(&cond);
                ctx.builder
                    .selection_merge(endif_label, spirv::SelectionControl::NONE)
                    .unwrap();
                ctx.builder
                    .branch_conditional(cond, if_label, endif_label, std::iter::empty())
                    .unwrap();
                current_block = None;
            }
            ir::SyntaxNode::Loop {
                body,
                continue_block,
                merge,
            } => {
                let body_label = ctx.block_labels[body as usize];
                let continue_label = ctx.block_labels[continue_block as usize];
                let endloop_label = ctx.block_labels[merge as usize];
                ctx.builder
                    .loop_merge(
                        endloop_label,
                        continue_label,
                        spirv::LoopControl::NONE,
                        std::iter::empty(),
                    )
                    .unwrap();
                ctx.builder.branch(body_label).unwrap();
                current_block = None;
            }
            ir::SyntaxNode::Break { cond, merge, skip } => {
                let break_label = ctx.block_labels[merge as usize];
                let skip_label = ctx.block_labels[skip as usize];
                let cond = ctx.resolve_value(&cond);
                ctx.builder
                    .branch_conditional(cond, break_label, skip_label, std::iter::empty())
                    .unwrap();
                current_block = None;
            }
            ir::SyntaxNode::EndIf { merge } => {
                if current_block.is_some() {
                    ctx.builder
                        .branch(ctx.block_labels[merge as usize])
                        .unwrap();
                }
                current_block = None;
            }
            ir::SyntaxNode::Repeat {
                cond,
                loop_header,
                merge,
            } => {
                let mut cond = ctx.resolve_value(&cond);
                if loop_safety_enabled {
                    let safety_counter = safety_counters
                        .next()
                        .expect("one safety counter must exist for each Repeat node");
                    let old_counter = ctx
                        .builder
                        .load(ctx.u32_type, None, safety_counter, None, [])
                        .unwrap();
                    let new_counter = ctx
                        .builder
                        .i_sub(ctx.u32_type, None, old_counter, ctx.const_one_u32)
                        .unwrap();
                    ctx.builder
                        .store(safety_counter, new_counter, None, [])
                        .unwrap();
                    let safety_cond = ctx
                        .builder
                        .s_greater_than_equal(ctx.bool_type, None, new_counter, ctx.const_zero_u32)
                        .unwrap();
                    cond = ctx
                        .builder
                        .logical_and(ctx.bool_type, None, cond, safety_cond)
                        .unwrap();
                }
                let loop_header_label = ctx.block_labels[loop_header as usize];
                let merge_label = ctx.block_labels[merge as usize];
                ctx.builder
                    .branch_conditional(cond, loop_header_label, merge_label, std::iter::empty())
                    .unwrap();
                current_block = None;
            }
            ir::SyntaxNode::Return => {
                ctx.builder.ret().unwrap();
                current_block = None;
            }
            ir::SyntaxNode::Unreachable => {
                ctx.builder.unreachable().unwrap();
                current_block = None;
            }
        }
    }
    ctx.builder.end_function().unwrap();
    main
}

pub(crate) fn emit_into_context(
    ctx: &mut SpirvEmitContext,
    program: &ir::Program,
    bindings: &mut Bindings,
) {
    ctx.define_global_variables(program, bindings);
    let main = define_main(ctx, program);
    define_entry_point(program, ctx, main);
    let profile = ctx.profile.clone();
    setup_float_controls(&profile, program, ctx, main);
    setup_capabilities(&profile, &program.info, program.stage, ctx);
    setup_transform_feedback_capabilities(ctx, main);
    ctx.patch_deferred_phis();
}

/// Emit SPIR-V binary from an IR program.
///
/// This is the main entry point matching upstream `EmitSPIRV()`.
/// Returns SPIR-V word vector ready for VkShaderModule creation.
pub fn emit_spirv(
    program: &ir::Program,
    profile: &super::super::Profile,
    runtime_info: &RuntimeInfo,
) -> Vec<u32> {
    let mut bindings = Bindings::default();
    emit_spirv_with_bindings(program, profile, runtime_info, &mut bindings)
}

pub fn emit_spirv_with_bindings(
    program: &ir::Program,
    profile: &super::super::Profile,
    runtime_info: &RuntimeInfo,
    bindings: &mut Bindings,
) -> Vec<u32> {
    let mut ctx = SpirvEmitContext::new(program, profile, runtime_info);
    emit_into_context(&mut ctx, program, bindings);
    ctx.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::basic_block::Block;
    use rspirv::dr::Operand;

    const MAIN: spirv::Word = 0x100;

    fn has_capability(ctx: &SpirvEmitContext, capability: spirv::Capability) -> bool {
        ctx.builder
            .module_ref()
            .capabilities
            .iter()
            .any(|instruction| {
                matches!(
                    instruction.operands.as_slice(),
                    [Operand::Capability(found)] if *found == capability
                )
            })
    }

    fn has_execution_mode(
        ctx: &SpirvEmitContext,
        mode: spirv::ExecutionMode,
        literals: &[u32],
    ) -> bool {
        ctx.builder
            .module_ref()
            .execution_modes
            .iter()
            .any(|instruction| {
                let operands = instruction.operands.as_slice();
                matches!(
                    operands,
                    [Operand::IdRef(MAIN), Operand::ExecutionMode(found), ..]
                        if *found == mode
                            && operands[2..]
                                .iter()
                                .map(|operand| match operand {
                                    Operand::LiteralBit32(value) => Some(*value),
                                    _ => None,
                                })
                                .eq(literals.iter().copied().map(Some))
                )
            })
    }

    #[test]
    fn compute_entry_point_declares_local_size() {
        let mut program = ir::Program::new(ShaderStage::Compute);
        program.workgroup_size = [8, 4, 2];
        let mut ctx = SpirvEmitContext::new(&program, &Profile::default(), &RuntimeInfo::default());

        define_entry_point(&program, &mut ctx, MAIN);

        assert!(has_execution_mode(
            &ctx,
            spirv::ExecutionMode::LocalSize,
            &[8, 4, 2]
        ));
        assert!(ctx.builder.module_ref().entry_points.iter().any(|entry| {
            matches!(
                entry.operands.as_slice(),
                [
                    Operand::ExecutionModel(spirv::ExecutionModel::GLCompute),
                    Operand::IdRef(MAIN),
                    Operand::LiteralString(name),
                    ..
                ] if name == "main"
            )
        }));
    }

    #[test]
    fn geometry_entry_point_declares_upstream_modes_and_capabilities() {
        let mut program = ir::Program::new(ShaderStage::Geometry);
        program.output_topology = OutputTopology::LineStrip;
        program.output_vertices = 7;
        program.invocations = 3;
        program.is_geometry_passthrough = true;
        program
            .info
            .stores
            .set(Attribute::POINT_SIZE.0 as usize, true);
        let profile = Profile {
            support_geometry_streams: true,
            support_geometry_shader_passthrough: true,
            ..Profile::default()
        };
        let runtime_info = RuntimeInfo {
            input_topology: InputTopology::TrianglesAdjacency,
            ..RuntimeInfo::default()
        };
        let mut ctx = SpirvEmitContext::new(&program, &profile, &runtime_info);

        define_entry_point(&program, &mut ctx, MAIN);

        for capability in [
            spirv::Capability::Geometry,
            spirv::Capability::GeometryStreams,
            spirv::Capability::GeometryPointSize,
            spirv::Capability::GeometryShaderPassthroughNV,
        ] {
            assert!(has_capability(&ctx, capability));
        }
        for (mode, literals) in [
            (spirv::ExecutionMode::InputTrianglesAdjacency, &[][..]),
            (spirv::ExecutionMode::OutputLineStrip, &[][..]),
            (spirv::ExecutionMode::OutputVertices, &[7][..]),
            (spirv::ExecutionMode::Invocations, &[3][..]),
        ] {
            assert!(has_execution_mode(&ctx, mode, literals));
        }
        assert!(ctx.builder.module_ref().extensions.iter().any(|extension| {
            matches!(
                extension.operands.as_slice(),
                [Operand::LiteralString(name)]
                    if name == "SPV_NV_geometry_shader_passthrough"
            )
        }));
    }

    #[test]
    fn fragment_entry_point_declares_origin_depth_and_early_tests() {
        let mut program = ir::Program::new(ShaderStage::Fragment);
        program.info.stores_frag_depth = true;
        let profile = Profile {
            lower_left_origin_mode: true,
            ..Profile::default()
        };
        let runtime_info = RuntimeInfo {
            force_early_z: true,
            ..RuntimeInfo::default()
        };
        let mut ctx = SpirvEmitContext::new(&program, &profile, &runtime_info);

        define_entry_point(&program, &mut ctx, MAIN);

        for mode in [
            spirv::ExecutionMode::OriginLowerLeft,
            spirv::ExecutionMode::DepthReplacing,
            spirv::ExecutionMode::EarlyFragmentTests,
        ] {
            assert!(has_execution_mode(&ctx, mode, &[]));
        }
    }

    #[test]
    fn transform_feedback_declares_capability_and_xfb_mode() {
        let program = ir::Program::new(ShaderStage::VertexB);
        let runtime_info = RuntimeInfo {
            xfb_count: 1,
            ..RuntimeInfo::default()
        };
        let mut ctx = SpirvEmitContext::new(&program, &Profile::default(), &runtime_info);

        setup_transform_feedback_capabilities(&mut ctx, MAIN);

        assert!(has_capability(&ctx, spirv::Capability::TransformFeedback));
        assert!(has_execution_mode(&ctx, spirv::ExecutionMode::Xfb, &[]));
    }

    #[test]
    fn repeat_respects_loop_safety_setting() {
        const CHILD: &str = "RUZU_TEST_LOOP_SAFETY_MODE";
        let Ok(mode) = std::env::var(CHILD) else {
            // Settings are process-global. Keep both cases isolated from other
            // shader tests, including when the suite runs in parallel.
            for mode in ["enabled", "disabled"] {
                let status = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "backend::spirv::emit_spirv::tests::repeat_respects_loop_safety_setting"])
                    .env(CHILD, mode)
                    .status()
                    .unwrap();
                assert!(status.success(), "loop safety {mode}");
            }
            return;
        };
        let disabled = mode == "disabled";
        common::settings::values_mut().disable_shader_loop_safety_checks.set_value(disabled);
        let mut program = ir::Program::new(ShaderStage::Fragment);
        program.blocks = vec![Block::new(), Block::new(), Block::new()];
        program.syntax_list = vec![
            ir::SyntaxNode::Block(0),
            ir::SyntaxNode::Loop {
                body: 1,
                continue_block: 1,
                merge: 2,
            },
            ir::SyntaxNode::Block(1),
            ir::SyntaxNode::Repeat {
                cond: ir::Value::ImmU1(true),
                loop_header: 0,
                merge: 2,
            },
            ir::SyntaxNode::Block(2),
            ir::SyntaxNode::Return,
        ];
        let profile = Profile {
            supported_spirv: 0x0001_0400,
            ..Profile::default()
        };
        let mut ctx = SpirvEmitContext::new(&program, &profile, &RuntimeInfo::default());

        define_main(&mut ctx, &program);

        let module = ctx.builder.module_ref();
        if disabled {
            assert!(!module.types_global_values.iter().any(|inst| {
                inst.class.opcode == spirv::Op::Variable
                    && matches!(inst.operands.first(), Some(Operand::StorageClass(spirv::StorageClass::Private)))
            }), "disabled safety must not allocate counters");
            let instructions: Vec<_> = module.functions.iter()
                .flat_map(|function| function.blocks.iter())
                .flat_map(|block| block.instructions.iter()).collect();
            assert!(!instructions.iter().any(|inst| matches!(inst.class.opcode,
                spirv::Op::ISub | spirv::Op::SGreaterThanEqual | spirv::Op::LogicalAnd)));
            let condition = module.types_global_values.iter()
                .find(|inst| inst.class.opcode == spirv::Op::ConstantTrue)
                .and_then(|inst| inst.result_id).unwrap();
            assert!(instructions.iter().any(|inst| inst.class.opcode == spirv::Op::BranchConditional
                && inst.operands.first() == Some(&Operand::IdRef(condition))),
                "repeat must branch directly on the original condition");
            return;
        }
        let safety_counter = module
            .types_global_values
            .iter()
            .find(|inst| {
                inst.class.opcode == spirv::Op::Variable
                    && matches!(
                        inst.operands.first(),
                        Some(Operand::StorageClass(spirv::StorageClass::Private))
                    )
            })
            .expect("Repeat must create a Private safety counter");
        let initializer = match safety_counter.operands.as_slice() {
            [Operand::StorageClass(spirv::StorageClass::Private), Operand::IdRef(id)] => *id,
            operands => panic!("unexpected safety-counter operands: {operands:?}"),
        };
        assert!(module.types_global_values.iter().any(|inst| {
            inst.result_id == Some(initializer)
                && matches!(inst.operands.as_slice(), [Operand::LiteralBit32(0x2000)])
        }));
        assert!(ctx.interfaces.contains(
            &safety_counter
                .result_id
                .expect("OpVariable has a result id")
        ));

        let emitted_ops = module
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
            .map(|inst| inst.class.opcode)
            .collect::<Vec<_>>();
        for opcode in [
            spirv::Op::Load,
            spirv::Op::ISub,
            spirv::Op::Store,
            spirv::Op::SGreaterThanEqual,
            spirv::Op::LogicalAnd,
        ] {
            assert!(emitted_ops.contains(&opcode), "missing {opcode:?}");
        }
    }

    #[test]
    fn shader_without_repeat_does_not_emit_loop_safety_initializer() {
        let mut program = ir::Program::new(ShaderStage::Fragment);
        program.blocks = vec![Block::new()];
        program.syntax_list = vec![ir::SyntaxNode::Block(0), ir::SyntaxNode::Return];
        let mut ctx = SpirvEmitContext::new(&program, &Profile::default(), &RuntimeInfo::default());

        define_main(&mut ctx, &program);

        assert!(!ctx
            .builder
            .module_ref()
            .types_global_values
            .iter()
            .any(|inst| { matches!(inst.operands.as_slice(), [Operand::LiteralBit32(0x2000)]) }));
    }
}
