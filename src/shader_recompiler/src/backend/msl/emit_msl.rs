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
}
