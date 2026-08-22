// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Shader backend: emits target-specific binary from IR.
//!
//! The primary backend is SPIR-V via the `spirv/` subdirectory, which contains
//! one Rust file per upstream `backend/spirv/emit_spirv_*.cpp` source file.
//!
//! The entry point is `emit_spirv()`, which takes an `IR::Program` and a
//! `Profile` and returns a `Vec<u32>` of SPIR-V words.

pub mod bindings;
pub mod glasm;
pub mod glsl;
pub mod msl;
pub mod spirv;

use crate::ir;
use crate::profile::Profile;
use crate::runtime_info::RuntimeInfo;

/// Emit SPIR-V binary from an IR program.
///
/// Returns the SPIR-V words ready to be loaded into a VkShaderModule.
/// Delegates to `spirv::emit_spirv::emit_spirv`.
///
/// `profile` is the upstream-faithful `Shader::Profile` (in
/// `crate::profile`). The previous duplicate `backend::Profile` was
/// removed when the SPIR-V and GLSL backends were unified onto a
/// single Profile type.
pub fn emit_spirv(
    program: &ir::Program,
    profile: &Profile,
    runtime_info: &RuntimeInfo,
) -> Vec<u32> {
    spirv::emit_spirv::emit_spirv(program, profile, runtime_info)
}

pub fn emit_spirv_with_bindings(
    program: &ir::Program,
    profile: &Profile,
    runtime_info: &RuntimeInfo,
    bindings: &mut bindings::Bindings,
) -> Vec<u32> {
    spirv::emit_spirv::emit_spirv_with_bindings(program, profile, runtime_info, bindings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::basic_block::Block;
    use crate::ir::emitter::Emitter;
    use crate::ir::types::ShaderStage;
    use crate::ir::value::Value;
    use crate::runtime_info::RuntimeInfo;

    fn contains_opcode(words: &[u32], opcode: rspirv::spirv::Op) -> bool {
        let mut offset = 5;
        while offset < words.len() {
            let header = words[offset];
            if header & 0xffff == opcode as u32 {
                return true;
            }
            let word_count = (header >> 16) as usize;
            assert!(word_count != 0, "invalid SPIR-V instruction word count");
            offset += word_count;
        }
        false
    }

    fn count_opcode(words: &[u32], opcode: rspirv::spirv::Op) -> usize {
        let mut count = 0;
        let mut offset = 5;
        while offset < words.len() {
            let header = words[offset];
            if header & 0xffff == opcode as u32 {
                count += 1;
            }
            let word_count = (header >> 16) as usize;
            assert!(word_count != 0, "invalid SPIR-V instruction word count");
            offset += word_count;
        }
        count
    }

    #[test]
    fn test_emit_empty_vertex_shader() {
        let mut program = ir::Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());

        let profile = Profile::default();
        let words = emit_spirv(&program, &profile, &RuntimeInfo::default());

        // SPIR-V magic number is 0x07230203
        assert!(words.len() >= 5, "SPIR-V should have at least a header");
        assert_eq!(words[0], 0x07230203, "SPIR-V magic number mismatch");
        assert_eq!(count_opcode(&words, rspirv::spirv::Op::Label), 1);
    }

    #[test]
    fn test_emit_empty_fragment_shader() {
        let mut program = ir::Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());

        let profile = Profile::default();
        let words = emit_spirv(&program, &profile, &RuntimeInfo::default());

        assert!(words.len() >= 5);
        assert_eq!(words[0], 0x07230203);
    }

    #[test]
    fn vertex_prologue_initializes_position_and_generic_outputs() {
        let mut program = ir::Program::new(ShaderStage::VertexB);
        program
            .info
            .stores
            .set(crate::ir::value::Attribute::generic(1, 0).0 as usize, true);
        program.blocks.push(Block::new());
        Emitter::new(&mut program, 0).prologue();

        let words = emit_spirv(&program, &Profile::default(), &RuntimeInfo::default());

        assert_eq!(count_opcode(&words, rspirv::spirv::Op::Store), 2);
    }

    #[test]
    fn vertex_prologue_initializes_fixed_point_size() {
        let mut program = ir::Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        Emitter::new(&mut program, 0).prologue();
        let runtime_info = RuntimeInfo {
            fixed_state_point_size: Some(3.0),
            ..RuntimeInfo::default()
        };

        let words = emit_spirv(&program, &Profile::default(), &runtime_info);

        assert_eq!(count_opcode(&words, rspirv::spirv::Op::Store), 2);
    }

    #[test]
    fn test_emit_vertex_with_arithmetic() {
        let mut program = ir::Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());

        {
            let mut emitter = Emitter::new(&mut program, 0);
            // Create a simple FP add
            let a = emitter.fp_add_32(Value::ImmF32(1.0), Value::ImmF32(2.0));
            let b = emitter.fp_mul_32(a, Value::ImmF32(3.0));
            let _ = b; // Result is not stored but instructions should emit
        }

        let profile = Profile::default();
        let words = emit_spirv(&program, &profile, &RuntimeInfo::default());

        assert!(words.len() > 5, "Should have more than just a header");
        assert_eq!(words[0], 0x07230203);
    }

    #[test]
    fn test_emit_fragment_with_output() {
        let mut program = ir::Program::new(ShaderStage::Fragment);
        // Fragment doesn't store generics (VaryingState defaults to all-zero).
        program.blocks.push(Block::new());

        {
            let mut emitter = Emitter::new(&mut program, 0);
            let _ = emitter.iadd_32(Value::ImmU32(1), Value::ImmU32(2));
        }

        let profile = Profile::default();
        let words = emit_spirv(&program, &profile, &RuntimeInfo::default());

        assert!(words.len() >= 5);
        assert_eq!(words[0], 0x07230203);
    }

    #[test]
    fn iadd_with_carry_uses_upstream_iadd_carry_path() {
        let mut program = ir::Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        {
            let mut emitter = Emitter::new(&mut program, 0);
            let add = emitter.iadd_32(Value::ImmU32(u32::MAX), Value::ImmU32(1));
            let _ = emitter.get_zero_from_op(add);
            let _ = emitter.get_sign_from_op(add);
            let _ = emitter.get_carry_from_op(add);
            let _ = emitter.get_overflow_from_op(add);
        }
        let words = emit_spirv(&program, &Profile::default(), &RuntimeInfo::default());
        assert!(contains_opcode(&words, rspirv::spirv::Op::IAddCarry));
        assert!(contains_opcode(&words, rspirv::spirv::Op::IEqual));
        assert!(contains_opcode(&words, rspirv::spirv::Op::SLessThan));
        assert!(contains_opcode(&words, rspirv::spirv::Op::Select));
    }

    #[test]
    fn integer_clamps_emit_glsl_std450_operations() {
        let mut program = ir::Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        {
            let mut emitter = Emitter::new(&mut program, 0);
            let signed = emitter.s_clamp_32(
                Value::ImmU32(7),
                Value::ImmU32((-2i32) as u32),
                Value::ImmU32(5),
            );
            let unsigned = emitter.u_clamp_32(Value::ImmU32(7), Value::ImmU32(2), Value::ImmU32(5));
            let _ = emitter.get_zero_from_op(signed);
            let _ = emitter.get_sign_from_op(unsigned);
        }
        let words = emit_spirv(&program, &Profile::default(), &RuntimeInfo::default());
        assert!(contains_opcode(&words, rspirv::spirv::Op::ExtInst));
        assert!(contains_opcode(&words, rspirv::spirv::Op::IEqual));
        assert!(contains_opcode(&words, rspirv::spirv::Op::SLessThan));
    }

    #[test]
    fn test_emit_with_cbuf_descriptors() {
        let mut program = ir::Program::new(ShaderStage::VertexB);
        program
            .info
            .constant_buffer_descriptors
            .push(crate::ir::program::CbufDescriptor { index: 0, count: 1 });
        program.blocks.push(Block::new());

        {
            let mut emitter = Emitter::new(&mut program, 0);
            let _ = emitter.get_cbuf_u32(Value::ImmU32(0), Value::ImmU32(0));
        }

        let profile = Profile::default();
        let words = emit_spirv(&program, &profile, &RuntimeInfo::default());

        assert!(words.len() > 5);
        assert_eq!(words[0], 0x07230203);
    }

    /// Upstream value-initializes every `bool` in `Profile` to false; only
    /// `supported_spirv` and `supported_subgroup_stages` carry a non-zero
    /// default.
    #[test]
    fn test_profile_default() {
        let profile = Profile::default();
        assert_eq!(profile.supported_spirv, 0x00010000);
        assert_eq!(profile.supported_subgroup_stages, 0x7F);
        assert!(!profile.support_demote_to_helper_invocation);
    }

    #[test]
    fn default_profile_supports_every_subgroup_stage() {
        use crate::stage::Stage;

        let profile = Profile::default();
        for stage in [
            Stage::VertexB,
            Stage::TessellationControl,
            Stage::TessellationEval,
            Stage::Geometry,
            Stage::Fragment,
            Stage::Compute,
            Stage::VertexA,
        ] {
            assert!(profile.supports_subgroup_stage(stage), "{stage}");
        }
    }
}
