// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! SPIR-V special emission — maps to zuyu's
//! `backend/spirv/emit_spirv_special.cpp`.
//!
//! Handles prologue/epilogue, emit vertex, end primitive, depth mode
//! conversion, alpha test, and fixed pipeline point size.

use super::spirv_emit_context::SpirvEmitContext;
use crate::ir::types::ShaderStage;
use crate::ir::Value;
use crate::runtime_info::CompareFunction;
use rspirv::spirv::Word;

fn output_position(ctx: &SpirvEmitContext) -> Option<Word> {
    ctx.output_vars.get(&0xFFFF_0000).copied()
}

fn convert_depth_mode(ctx: &mut SpirvEmitContext) {
    let Some(position_var) = output_position(ctx) else {
        return;
    };
    let position = ctx
        .builder
        .load(ctx.f32_vec4_type, None, position_var, None, vec![])
        .unwrap();
    let z = ctx
        .builder
        .composite_extract(ctx.f32_type, None, position, vec![2])
        .unwrap();
    let w = ctx
        .builder
        .composite_extract(ctx.f32_type, None, position, vec![3])
        .unwrap();
    let z_plus_w = ctx.builder.f_add(ctx.f32_type, None, z, w).unwrap();
    let half = ctx.constant_f32(0.5);
    let screen_depth = ctx
        .builder
        .f_mul(ctx.f32_type, None, z_plus_w, half)
        .unwrap();
    let vector = ctx
        .builder
        .composite_insert(ctx.f32_vec4_type, None, screen_depth, position, vec![2])
        .unwrap();
    ctx.builder
        .store(position_var, vector, None, vec![])
        .unwrap();
}

fn set_fixed_pipeline_point_size(ctx: &mut SpirvEmitContext) {
    if let Some(point_size) = ctx.runtime_info.fixed_state_point_size {
        if ctx.output_point_size != 0 {
            let value = ctx.constant_f32(point_size);
            ctx.builder
                .store(ctx.output_point_size, value, None, vec![])
                .unwrap();
        }
    }
}

/// Port of upstream `DefaultVarying` in `emit_spirv_special.cpp`.
fn default_varying(
    ctx: &mut SpirvEmitContext,
    num_components: u32,
    element: u32,
    zero: Word,
    one: Word,
    default_vector: Word,
) -> Word {
    match num_components {
        1 => {
            if element == 3 {
                one
            } else {
                zero
            }
        }
        2 => ctx.builder.constant_composite(
            ctx.f32_vec2_type,
            vec![zero, if element + 1 == 3 { one } else { zero }],
        ),
        3 => ctx.builder.constant_composite(
            ctx.f32_vec3_type,
            vec![zero, zero, if element + 2 == 3 { one } else { zero }],
        ),
        4 => default_vector,
        _ => panic!("bad varying element count {num_components}"),
    }
}

/// Port of upstream `ComparisonFunction` in `emit_spirv_special.cpp`.
fn comparison_function(
    ctx: &mut SpirvEmitContext,
    comparison: CompareFunction,
    operand_1: Word,
    operand_2: Word,
) -> Word {
    match comparison {
        CompareFunction::Never => ctx.const_false,
        CompareFunction::Less => ctx
            .builder
            .f_ord_less_than(ctx.bool_type, None, operand_1, operand_2)
            .unwrap(),
        CompareFunction::Equal => ctx
            .builder
            .f_ord_equal(ctx.bool_type, None, operand_1, operand_2)
            .unwrap(),
        CompareFunction::LessThanEqual => ctx
            .builder
            .f_ord_less_than_equal(ctx.bool_type, None, operand_1, operand_2)
            .unwrap(),
        CompareFunction::Greater => ctx
            .builder
            .f_ord_greater_than(ctx.bool_type, None, operand_1, operand_2)
            .unwrap(),
        CompareFunction::NotEqual => ctx
            .builder
            .f_ord_not_equal(ctx.bool_type, None, operand_1, operand_2)
            .unwrap(),
        CompareFunction::GreaterThanEqual => ctx
            .builder
            .f_ord_greater_than_equal(ctx.bool_type, None, operand_1, operand_2)
            .unwrap(),
        CompareFunction::Always => ctx.const_true,
    }
}

/// Port of upstream `AlphaTest` in `emit_spirv_special.cpp`.
fn alpha_test(ctx: &mut SpirvEmitContext) {
    let Some(comparison) = ctx.runtime_info.alpha_test_func else {
        return;
    };
    if comparison == CompareFunction::Always || ctx.frag_color[0] == 0 {
        return;
    }

    let rt0_color = ctx
        .builder
        .load(ctx.f32_vec4_type, None, ctx.frag_color[0], None, vec![])
        .unwrap();
    let alpha = ctx
        .builder
        .composite_extract(ctx.f32_type, None, rt0_color, vec![3])
        .unwrap();
    let alpha_reference = ctx.constant_f32(ctx.runtime_info.alpha_test_reference);
    let condition = comparison_function(ctx, comparison, alpha, alpha_reference);
    let true_label = ctx.builder.id();
    let discard_label = ctx.builder.id();

    ctx.builder
        .selection_merge(true_label, rspirv::spirv::SelectionControl::NONE)
        .unwrap();
    ctx.builder
        .branch_conditional(condition, true_label, discard_label, vec![])
        .unwrap();
    ctx.builder.begin_block(Some(discard_label)).unwrap();
    ctx.builder.kill().unwrap();
    ctx.builder.begin_block(Some(true_label)).unwrap();
}

/// Emit shader prologue.
///
/// Matches upstream `EmitPrologue(EmitContext&)`.
/// For vertex shaders, initializes output position to (0,0,0,1) and
/// sets default values for generic outputs. For geometry shaders,
/// sets fixed pipeline point size.
pub fn emit_prologue(ctx: &mut SpirvEmitContext) {
    log::trace!("SPIR-V: emit_prologue");
    if ctx.stage == ShaderStage::Fragment && ctx.runtime_info.dual_source_blend {
        let default_color = ctx.builder.constant_composite(
            ctx.f32_vec4_type,
            vec![
                ctx.const_zero_f32,
                ctx.const_zero_f32,
                ctx.const_zero_f32,
                ctx.const_one_f32,
            ],
        );
        for index in 0..2 {
            if ctx.frag_color[index] != 0 {
                ctx.builder
                    .store(ctx.frag_color[index], default_color, None, vec![])
                    .unwrap();
            }
        }
    }
    if ctx.stage == ShaderStage::VertexB {
        let default_position = ctx.builder.constant_composite(
            ctx.f32_vec4_type,
            vec![
                ctx.const_zero_f32,
                ctx.const_zero_f32,
                ctx.const_zero_f32,
                ctx.const_one_f32,
            ],
        );
        if let Some(position_var) = output_position(ctx) {
            ctx.builder
                .store(position_var, default_position, None, vec![])
                .unwrap();
        }
        for index in 0..32 {
            if ctx.output_generics[index][0].num_components == 0 {
                continue;
            }
            let mut element = 0;
            while element < 4 {
                let info = ctx.output_generics[index][element as usize];
                let value = default_varying(
                    ctx,
                    info.num_components,
                    element,
                    ctx.const_zero_f32,
                    ctx.const_one_f32,
                    default_position,
                );
                ctx.builder.store(info.id, value, None, vec![]).unwrap();
                element += info.num_components;
            }
        }
        if ctx.clip_distances != 0 {
            for index in 0..ctx.profile.max_user_clip_distances {
                if ctx.clip_distance_written[index as usize] {
                    continue;
                }
                let index = ctx.constant_u32(index);
                let element = ctx
                    .builder
                    .access_chain(ctx.output_f32_ptr, None, ctx.clip_distances, vec![index])
                    .unwrap();
                ctx.builder
                    .store(element, ctx.const_zero_f32, None, vec![])
                    .unwrap();
            }
        }
    }
    if matches!(ctx.stage, ShaderStage::VertexB | ShaderStage::Geometry) {
        set_fixed_pipeline_point_size(ctx);
    }
}

/// Emit shader epilogue.
///
/// Matches upstream `EmitEpilogue(EmitContext&)`.
/// For vertex shaders with depth mode conversion, transform Z coordinate.
/// For fragment shaders, run alpha test.
pub fn emit_epilogue(ctx: &mut SpirvEmitContext) {
    log::trace!("SPIR-V: emit_epilogue");
    if ctx.stage == ShaderStage::VertexB
        && ctx.runtime_info.convert_depth_mode
        && !ctx.profile.support_native_ndc
    {
        convert_depth_mode(ctx);
    }
    if ctx.stage == ShaderStage::Fragment {
        alpha_test(ctx);
    }
}

/// Emit a geometry shader vertex.
///
/// Matches upstream `EmitEmitVertex(EmitContext&, const IR::Value&)`.
pub fn emit_emit_vertex(ctx: &mut SpirvEmitContext, stream: &Value) {
    if ctx.runtime_info.convert_depth_mode && !ctx.profile.support_native_ndc {
        convert_depth_mode(ctx);
    }
    if !ctx.profile.support_geometry_streams {
        panic!("SPIR-V: geometry streams are not supported");
    }
    let stream = if stream.is_immediate() {
        ctx.resolve_value(stream)
    } else {
        log::warn!("SPIR-V: geometry stream is not immediate");
        ctx.const_zero_u32
    };
    ctx.builder.emit_stream_vertex(stream).unwrap();
    set_fixed_pipeline_point_size(ctx);
}

/// End a geometry shader primitive.
///
/// Matches upstream `EmitEndPrimitive(EmitContext&, const IR::Value&)`.
pub fn emit_end_primitive(ctx: &mut SpirvEmitContext, stream: &Value) {
    if !ctx.profile.support_geometry_streams {
        panic!("SPIR-V: geometry streams are not supported");
    }
    let stream = if stream.is_immediate() {
        ctx.resolve_value(stream)
    } else {
        log::warn!("SPIR-V: geometry stream is not immediate");
        ctx.const_zero_u32
    };
    ctx.builder.end_stream_primitive(stream).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::emit_spirv;
    use crate::ir::basic_block::Block;
    use crate::ir::emitter::Emitter;
    use crate::ir::Program;
    use crate::profile::Profile;

    fn contains_opcode(words: &[u32], opcode: rspirv::spirv::Op) -> bool {
        count_opcode(words, opcode) != 0
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
            assert_ne!(word_count, 0, "invalid SPIR-V instruction word count");
            offset += word_count;
        }
        count
    }

    fn fragment_with_epilogue() -> Program {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        program.info.stores_frag_color[0] = true;
        let mut emitter = Emitter::new(&mut program, 0);
        emitter.set_frag_color(Value::ImmU32(0), Value::ImmU32(3), Value::ImmF32(0.25));
        emitter.epilogue();
        program
    }

    #[test]
    fn fragment_alpha_test_emits_ordered_comparison_and_kill() {
        let program = fragment_with_epilogue();
        let runtime_info = crate::runtime_info::RuntimeInfo {
            alpha_test_func: Some(CompareFunction::Greater),
            alpha_test_reference: 0.5,
            ..Default::default()
        };

        let words = emit_spirv(&program, &Profile::default(), &runtime_info);

        assert!(contains_opcode(&words, rspirv::spirv::Op::FOrdGreaterThan));
        assert!(contains_opcode(&words, rspirv::spirv::Op::SelectionMerge));
        assert!(contains_opcode(&words, rspirv::spirv::Op::Kill));
    }

    #[test]
    fn fragment_alpha_test_always_does_not_emit_kill() {
        let program = fragment_with_epilogue();
        let runtime_info = crate::runtime_info::RuntimeInfo {
            alpha_test_func: Some(CompareFunction::Always),
            alpha_test_reference: 0.5,
            ..Default::default()
        };

        let words = emit_spirv(&program, &Profile::default(), &runtime_info);

        assert!(!contains_opcode(&words, rspirv::spirv::Op::Kill));
    }

    #[test]
    fn fragment_dual_source_blend_initializes_both_outputs() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        program.info.stores_frag_color[0] = true;
        Emitter::new(&mut program, 0).prologue();
        let runtime_info = crate::runtime_info::RuntimeInfo {
            dual_source_blend: true,
            ..Default::default()
        };

        let words = emit_spirv(&program, &Profile::default(), &runtime_info);

        assert_eq!(count_opcode(&words, rspirv::spirv::Op::Store), 2);
    }

    #[test]
    fn vertex_prologue_initializes_each_split_varying_output() {
        use crate::ir::value::Attribute;
        use crate::runtime_info::TransformFeedbackVarying;

        let mut program = Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        program
            .info
            .stores
            .set(Attribute::generic(0, 0).0 as usize, true);
        Emitter::new(&mut program, 0).prologue();

        let base = Attribute::generic(0, 0).0 as usize;
        let mut xfb_varyings = [TransformFeedbackVarying::default(); 256];
        xfb_varyings[base].components = 1;
        let runtime_info = crate::runtime_info::RuntimeInfo {
            xfb_count: (base + 1) as u32,
            xfb_varyings,
            ..Default::default()
        };

        let words = emit_spirv(&program, &Profile::default(), &runtime_info);

        // Position plus the scalar XFB output and the remaining vec3 output.
        assert_eq!(count_opcode(&words, rspirv::spirv::Op::Store), 3);
    }

    #[test]
    fn vertex_prologue_initializes_clip_distance_outputs() {
        use crate::ir::value::Attribute;

        let mut program = Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        program
            .info
            .stores
            .set(Attribute::CLIP_DISTANCE_0.0 as usize, true);
        Emitter::new(&mut program, 0).prologue();
        let profile = Profile {
            max_user_clip_distances: 4,
            ..Default::default()
        };

        let words = emit_spirv(&program, &profile, &Default::default());

        // Position plus the three clip distances the shader does not write.
        assert_eq!(count_opcode(&words, rspirv::spirv::Op::Store), 4);
    }
}
