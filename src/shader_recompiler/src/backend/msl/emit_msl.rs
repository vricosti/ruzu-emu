// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Main native-MSL emission entry point.
//!
//! The file boundary follows Eden's `backend/glsl/emit_glsl.{h,cpp}`. MSL is
//! a new target backend, so there is no upstream MSL source to mirror.

use crate::ir;
use crate::ir::opcodes::Opcode;
use crate::ir::program::SyntaxNode;
use crate::ir::value::{InstRef, Value};
use crate::profile::Profile;
use crate::runtime_info::RuntimeInfo;

use super::emit_msl_floating_point;
use super::emit_msl_integer;
use super::msl_emit_context::MslEmitContext;
use super::{MslError, MslOptions, MslShaderArtifact};

fn varying_mask_has_only_position(mask: &[u64; 8]) -> bool {
    mask.iter().enumerate().all(|(word_index, word)| {
        let allowed = if word_index == 0 {
            (0b1111u64) << 28
        } else {
            0
        };
        word & !allowed == 0
    })
}

fn first_unsupported_program_feature(program: &ir::Program) -> Option<&'static str> {
    let info = &program.info;

    if program.local_memory_size != 0 {
        return Some("local memory");
    }
    if program.shared_memory_size != 0 {
        return Some("shared memory");
    }
    if program.workgroup_size != [1, 1, 1] {
        return Some("workgroup size");
    }
    if program.output_vertices != 0 || program.invocations != 1 {
        return Some("geometry execution modes");
    }
    if program.is_geometry_passthrough {
        return Some("geometry passthrough");
    }
    let supported_vertex_position_stores = program.stage == crate::stage::Stage::VertexB
        && varying_mask_has_only_position(&info.stores.mask);
    let supported_fragment_colors = program.stage == crate::stage::Stage::Fragment;
    if info.loads.mask.iter().any(|word| *word != 0)
        || (!supported_vertex_position_stores && info.stores.mask.iter().any(|word| *word != 0))
        || info.passthrough.mask.iter().any(|word| *word != 0)
        || info.loads_indexed_attributes
        || info.stores_indexed_attributes
        || (!supported_fragment_colors && info.stores_frag_color.iter().any(|store| *store))
        || info.stores_sample_mask
        || info.stores_frag_depth
        || info.stores_tess_level_outer
        || info.stores_tess_level_inner
        || !info.legacy_stores_mapping.is_empty()
    {
        return Some("stage inputs or outputs");
    }
    if !info.constant_buffer_descriptors.is_empty()
        || !info.storage_buffers_descriptors.is_empty()
        || !info.texture_buffer_descriptors.is_empty()
        || !info.image_buffer_descriptors.is_empty()
        || !info.texture_descriptors.is_empty()
        || !info.image_descriptors.is_empty()
        || info.constant_buffer_mask != 0
        || info
            .constant_buffer_used_sizes
            .iter()
            .any(|size| *size != 0)
        || info.used_constant_buffer_types != 0
        || info.used_storage_buffer_types != 0
        || info.used_indirect_cbuf_types != 0
        || info.nvn_buffer_base != 0
        || info.nvn_buffer_used != 0
    {
        return Some("resource bindings");
    }
    if info.uses_patches.iter().any(|used| *used) {
        return Some("tessellation patches");
    }
    if info.stores_global_memory
        || info.uses_local_memory
        || info.uses_global_memory
        || info.uses_shared_increment
        || info.uses_shared_decrement
        || info.uses_global_increment
        || info.uses_global_decrement
        || info.uses_atomic_f32_add
        || info.uses_atomic_f16x2_add
        || info.uses_atomic_f16x2_min
        || info.uses_atomic_f16x2_max
        || info.uses_atomic_f32x2_add
        || info.uses_atomic_f32x2_min
        || info.uses_atomic_f32x2_max
        || info.uses_atomic_s32_min
        || info.uses_atomic_s32_max
        || info.uses_int64_bit_atomics
        || info.uses_atomic_image_u32
    {
        return Some("memory operations");
    }
    if info.uses_workgroup_id
        || info.uses_local_invocation_id
        || info.uses_invocation_id
        || info.uses_invocation_info
        || info.uses_sample_id
        || info.uses_is_helper_invocation
        || info.uses_subgroup_invocation_id
        || info.uses_subgroup_shuffles
        || info.uses_subgroup_vote
        || info.uses_subgroup_mask
        || info.requires_layer_emulation
        || info.emulated_layer != 0
        || info.used_clip_distances != 0
    {
        return Some("stage built-ins");
    }
    if info.uses_fp16
        || info.uses_fp64
        || info.uses_fp16_denorms_flush
        || info.uses_fp16_denorms_preserve
        || info.uses_fp32_denorms_flush
        || info.uses_fp32_denorms_preserve
        || info.uses_int8
        || info.uses_int16
        || info.uses_int64
        || info.uses_image_1d
        || info.uses_sampled_1d
        || info.uses_sparse_residency
        || info.uses_demote_to_helper_invocation
        || info.uses_fswzadd
        || info.uses_derivatives
        || info.uses_typeless_image_reads
        || info.uses_typeless_image_writes
        || info.uses_image_buffers
        || info.uses_shadow_lod
        || info.uses_rescaling_uniform
        || info.uses_cbuf_indirect
        || info.uses_render_area
    {
        return Some("shader capabilities");
    }
    None
}

fn immediate_u32(inst: &crate::ir::instruction::Inst, arg: u32) -> Result<u32, MslError> {
    match inst.arg(arg as usize) {
        Value::ImmU32(value) => Ok(*value),
        _ => Err(MslError::ExpectedImmediate {
            opcode: inst.opcode,
            arg,
            expected: "u32",
        }),
    }
}

fn emit_inst(
    context: &mut MslEmitContext,
    program: &ir::Program,
    inst_ref: InstRef,
) -> Result<(), MslError> {
    let inst = program.block(inst_ref.block).inst(inst_ref.inst);
    match inst.opcode {
        Opcode::Void | Opcode::Prologue | Opcode::Epilogue => Ok(()),
        Opcode::Identity => context.emit_identity(program, inst_ref, inst),
        Opcode::IAdd32 => emit_msl_integer::emit_iadd_32(context, program, inst_ref, inst),
        Opcode::FPAdd32 => {
            emit_msl_floating_point::emit_fp_add_32(context, program, inst_ref, inst)
        }
        Opcode::FPMul32 => {
            emit_msl_floating_point::emit_fp_mul_32(context, program, inst_ref, inst)
        }
        Opcode::SetAttribute => {
            let Value::Attribute(attribute) = inst.arg(0) else {
                return Err(MslError::ExpectedImmediate {
                    opcode: inst.opcode,
                    arg: 0,
                    expected: "attribute",
                });
            };
            if immediate_u32(inst, 2)? != 0 {
                return Err(MslError::UnsupportedProgramFeature(
                    "per-vertex output indexing",
                ));
            }
            if !attribute.is_position() {
                return Err(MslError::UnsupportedAttribute(attribute.0));
            }
            context.emit_set_position(inst_ref, attribute.position_element(), inst.arg(1))
        }
        Opcode::SetFragColor => {
            let render_target = immediate_u32(inst, 0)?;
            let component = immediate_u32(inst, 1)?;
            if render_target >= 8 || component >= 4 {
                return Err(MslError::UnsupportedProgramFeature("fragment output index"));
            }
            context.emit_set_frag_color(inst_ref, render_target, component, inst.arg(2))
        }
        opcode => Err(MslError::UnsupportedOpcode {
            block: inst_ref.block,
            inst: inst_ref.inst,
            opcode,
        }),
    }
}

fn emit_block(
    context: &mut MslEmitContext,
    program: &ir::Program,
    block_index: u32,
) -> Result<(), MslError> {
    for (inst_index, _) in program.block(block_index).indexed_iter() {
        emit_inst(
            context,
            program,
            InstRef {
                block: block_index,
                inst: inst_index,
            },
        )?;
    }
    Ok(())
}

/// Emit native MSL directly from the backend-neutral shader IR.
///
/// The initial supported language is intentionally exact: empty vertex and
/// fragment programs are valid, while every unported instruction is reported
/// as an error. Callers must never substitute a fallback shader for this
/// error.
pub fn emit_msl(
    program: &ir::Program,
    profile: &Profile,
    runtime_info: &RuntimeInfo,
) -> Result<MslShaderArtifact, MslError> {
    emit_msl_with_options(program, profile, runtime_info, &MslOptions::default())
}

pub fn emit_msl_with_options(
    program: &ir::Program,
    _profile: &Profile,
    _runtime_info: &RuntimeInfo,
    options: &MslOptions,
) -> Result<MslShaderArtifact, MslError> {
    let mut context = MslEmitContext::new(program, options)?;
    if let Some(feature) = first_unsupported_program_feature(program) {
        return Err(MslError::UnsupportedProgramFeature(feature));
    }
    if program.syntax_list.is_empty() {
        for block_index in 0..program.blocks.len() as u32 {
            emit_block(&mut context, program, block_index)?;
        }
    } else {
        for node in &program.syntax_list {
            match node {
                SyntaxNode::Block(block_index) => emit_block(&mut context, program, *block_index)?,
                SyntaxNode::Return => {}
                _ => {
                    return Err(MslError::UnsupportedProgramFeature(
                        "structured control flow",
                    ))
                }
            }
        }
    }
    Ok(context.finish())
}

#[cfg(test)]
mod tests {
    use crate::ir::basic_block::Block;
    use crate::ir::opcodes::Opcode;
    use crate::ir::value::Value;
    use crate::profile::Profile;
    use crate::runtime_info::RuntimeInfo;
    use crate::stage::Stage;

    use super::*;

    fn empty_program(stage: Stage) -> ir::Program {
        let mut program = ir::Program::new(stage);
        program.blocks.push(Block::new());
        program
    }

    #[test]
    fn emits_minimal_vertex_entry_point_without_spirv() {
        let artifact = emit_msl(
            &empty_program(Stage::VertexB),
            &Profile::default(),
            &RuntimeInfo::default(),
        )
        .unwrap();

        assert_eq!(artifact.source.stage, Stage::VertexB);
        assert_eq!(artifact.entry_point, "main0");
        assert!(artifact
            .source
            .source
            .contains("vertex MslVertexOut main0()"));
        assert!(artifact
            .source
            .source
            .contains("float4 position [[position]]"));
        assert!(!artifact
            .source
            .source
            .to_ascii_lowercase()
            .contains("spir-v"));
        assert_eq!(artifact.bindings, Default::default());
    }

    #[test]
    fn emits_minimal_fragment_entry_point_without_spirv() {
        let artifact = emit_msl(
            &empty_program(Stage::Fragment),
            &Profile::default(),
            &RuntimeInfo::default(),
        )
        .unwrap();

        assert_eq!(artifact.source.stage, Stage::Fragment);
        assert!(artifact.source.source.contains("fragment void main0()"));
        assert!(!artifact
            .source
            .source
            .to_ascii_lowercase()
            .contains("spir-v"));
    }

    #[test]
    fn rejects_unported_ir_instead_of_emitting_a_fallback() {
        let mut program = empty_program(Stage::Fragment);
        program.blocks[0].append_new_inst(Opcode::FPCos, vec![Value::ImmF32(1.0)]);

        assert_eq!(
            emit_msl(&program, &Profile::default(), &RuntimeInfo::default()),
            Err(MslError::UnsupportedOpcode {
                block: 0,
                inst: 0,
                opcode: Opcode::FPCos,
            })
        );
    }

    #[test]
    fn rejects_unmerged_vertex_a() {
        assert_eq!(
            emit_msl(
                &empty_program(Stage::VertexA),
                &Profile::default(),
                &RuntimeInfo::default()
            ),
            Err(MslError::UnmergedVertexA)
        );
    }

    #[test]
    fn rejects_unported_resources_even_without_instructions() {
        let mut program = empty_program(Stage::Fragment);
        program
            .info
            .constant_buffer_descriptors
            .push(crate::shader_info::ConstantBufferDescriptor { index: 0, count: 1 });

        assert_eq!(
            emit_msl(&program, &Profile::default(), &RuntimeInfo::default()),
            Err(MslError::UnsupportedProgramFeature("resource bindings"))
        );
    }

    #[test]
    fn rejects_unported_stage_interfaces_even_without_instructions() {
        let mut program = empty_program(Stage::Fragment);
        program.info.loads.mask[0] = 1u64 << 32;

        assert_eq!(
            emit_msl(&program, &Profile::default(), &RuntimeInfo::default()),
            Err(MslError::UnsupportedProgramFeature(
                "stage inputs or outputs"
            ))
        );
    }

    #[test]
    fn emits_typed_ssa_integer_and_float_expressions() {
        let mut program = empty_program(Stage::VertexB);
        let integer = program.blocks[0]
            .append_new_inst(Opcode::IAdd32, vec![Value::ImmU32(1), Value::ImmU32(2)]);
        let float = program.blocks[0].append_new_inst(
            Opcode::FPAdd32,
            vec![Value::ImmF32(-0.0), Value::ImmF32(2.0)],
        );
        program.blocks[0].inst_mut(float).flags = crate::ir::types::FpControl {
            no_contraction: true,
            ..Default::default()
        }
        .to_u32();

        let artifact = emit_msl(&program, &Profile::default(), &RuntimeInfo::default()).unwrap();
        assert!(artifact
            .source
            .source
            .contains("uint v_0_0 = (0x00000001u) + (0x00000002u);"));
        assert!(artifact.source.source.contains(
            "float v_0_1 = spvFAdd(as_type<float>(0x80000000u), as_type<float>(0x40000000u));"
        ));
        assert!(artifact
            .source
            .source
            .contains("[[clang::optnone]] T spvFAdd"));
        assert_eq!(integer, 0);
    }

    #[test]
    fn emits_vertex_position_and_fragment_color_outputs() {
        let mut vertex = empty_program(Stage::VertexB);
        vertex.info.stores.set(28, true);
        vertex.blocks[0].append_new_inst(
            Opcode::SetAttribute,
            vec![
                Value::Attribute(crate::ir::Attribute::POSITION_X),
                Value::ImmF32(1.0),
                Value::ImmU32(0),
            ],
        );
        let vertex = emit_msl(&vertex, &Profile::default(), &RuntimeInfo::default()).unwrap();
        assert!(vertex
            .source
            .source
            .contains("output.position.x = as_type<float>(0x3F800000u);"));

        let mut fragment = empty_program(Stage::Fragment);
        fragment.info.stores_frag_color[0] = true;
        fragment.blocks[0].append_new_inst(
            Opcode::SetFragColor,
            vec![Value::ImmU32(0), Value::ImmU32(2), Value::ImmF32(0.5)],
        );
        let fragment = emit_msl(&fragment, &Profile::default(), &RuntimeInfo::default()).unwrap();
        assert!(fragment
            .source
            .source
            .contains("float4 color0 [[color(0)]];"));
        assert!(fragment
            .source
            .source
            .contains("output.color0.z = as_type<float>(0x3F000000u);"));
    }
}
