// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Main native-MSL emission entry point.
//!
//! The file boundary follows Eden's `backend/glsl/emit_glsl.{h,cpp}`. MSL is
//! a new target backend, so there is no upstream MSL source to mirror.

use crate::ir;
use crate::profile::Profile;
use crate::runtime_info::RuntimeInfo;

use super::msl_emit_context::MslEmitContext;
use super::{MslError, MslShaderArtifact};

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
    if info.loads.mask.iter().any(|word| *word != 0)
        || info.stores.mask.iter().any(|word| *word != 0)
        || info.passthrough.mask.iter().any(|word| *word != 0)
        || info.loads_indexed_attributes
        || info.stores_indexed_attributes
        || info.stores_frag_color.iter().any(|store| *store)
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

/// Emit native MSL directly from the backend-neutral shader IR.
///
/// The initial supported language is intentionally exact: empty vertex and
/// fragment programs are valid, while every unported instruction is reported
/// as an error. Callers must never substitute a fallback shader for this
/// error.
pub fn emit_msl(
    program: &ir::Program,
    _profile: &Profile,
    _runtime_info: &RuntimeInfo,
) -> Result<MslShaderArtifact, MslError> {
    let context = MslEmitContext::new(program.stage)?;
    if let Some(feature) = first_unsupported_program_feature(program) {
        return Err(MslError::UnsupportedProgramFeature(feature));
    }
    for (block_index, block) in program.blocks.iter().enumerate() {
        if let Some((inst_index, inst)) = block.indexed_iter().next() {
            return Err(MslError::UnsupportedOpcode {
                block: block_index as u32,
                inst: inst_index,
                opcode: inst.opcode,
            });
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
        program.blocks[0].append_new_inst(Opcode::IAdd32, vec![Value::ImmU32(1), Value::ImmU32(2)]);

        assert_eq!(
            emit_msl(&program, &Profile::default(), &RuntimeInfo::default()),
            Err(MslError::UnsupportedOpcode {
                block: 0,
                inst: 0,
                opcode: Opcode::IAdd32,
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
        program.info.stores_frag_color[0] = true;

        assert_eq!(
            emit_msl(&program, &Profile::default(), &RuntimeInfo::default()),
            Err(MslError::UnsupportedProgramFeature(
                "stage inputs or outputs"
            ))
        );
    }
}
