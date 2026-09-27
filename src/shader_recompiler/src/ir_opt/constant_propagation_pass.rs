// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Constant propagation pass — fold compile-time constant expressions.
//!
//! Matches upstream `constant_propagation_pass.cpp`.
//!
//! For each instruction, if all arguments are immediates, evaluate the operation
//! and replace the instruction with the result immediate. Also handles special
//! cases like `GetRegister(RZ) -> 0` and `GetPred(PT) -> true`.

use crate::environment::Environment;
use crate::ir::instruction::Inst;
use crate::ir::opcodes::Opcode;
use crate::ir::program::Program;
use crate::ir::types::{TextureInstInfo, Type};
use crate::ir::value::{Attribute, InstRef, Value};
use crate::shader_info::{ReplaceConstant, TextureType as ShaderTextureType};

/// Run constant propagation on all instructions.
pub fn constant_propagation_pass(program: &mut Program) {
    constant_propagation_pass_impl(None, program);
}

fn constant_propagation_pass_impl(mut env: Option<&mut dyn Environment>, program: &mut Program) {
    // CollectShaderInfoPass runs later. Inspect the IR, including depth-only
    // prepasses, instead of relying on stores_frag_depth being populated.
    let preserves_depth_interpolation = program
        .blocks
        .iter()
        .any(|block| block.iter().any(|inst| inst.opcode == Opcode::SetFragDepth));
    let block_order = if program.post_order_blocks.is_empty() {
        (0..program.blocks.len() as u32).collect::<Vec<_>>()
    } else {
        program
            .post_order_blocks
            .iter()
            .rev()
            .copied()
            .collect::<Vec<_>>()
    };
    for block_index in block_order {
        let instruction_indices = program
            .block(block_index)
            .indexed_iter()
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        for inst_index in instruction_indices {
            let inst_ref = InstRef {
                block: block_index,
                inst: inst_index,
            };
            let is_cbuf = matches!(
                program.block(block_index).inst(inst_index).opcode,
                Opcode::GetCbufU32 | Opcode::GetCbufF32
            );
            if is_cbuf {
                if let Some(env) = env.as_deref_mut() {
                    fold_environment_constant_buffer(env, program, inst_ref);
                }
                continue;
            }
            let (resolved_args, resolved_phi_args) = {
                let inst = program.block(block_index).inst(inst_index);
                (
                    inst.args
                        .iter()
                        .map(|value| resolve_value(*value, program))
                        .collect::<Vec<_>>(),
                    inst.phi_args
                        .iter()
                        .map(|(block, value)| (*block, resolve_value(*value, program)))
                        .collect::<Vec<_>>(),
                )
            };
            {
                let inst = program.block_mut(block_index).inst_mut(inst_index);
                inst.args = resolved_args;
                inst.phi_args = resolved_phi_args;
            }
            let opcode = program.block(block_index).inst(inst_index).opcode;
            if !matches!(
                opcode,
                Opcode::IAdd32
                    | Opcode::IAdd64
                    | Opcode::ISub32
                    | Opcode::LogicalAnd
                    | Opcode::LogicalOr
                    | Opcode::LogicalNot
                    | Opcode::FPMul32
            ) {
                propagate(program.block_mut(block_index).inst_mut(inst_index));
            }
            fold_instruction_reference(program, inst_ref, preserves_depth_interpolation);
        }
    }
}

/// Run constant propagation with the shader environment available.
///
/// Upstream `ConstantPropagationPass(Environment&, IR::Program&)` uses the
/// environment both for HLE constant replacements and for folding driver
/// constant buffer 1 on proprietary-driver shaders. The environment-less
/// entry point remains for legacy callers that do not own an `Environment`.
pub fn constant_propagation_pass_with_env(env: &mut dyn Environment, program: &mut Program) {
    constant_propagation_pass_impl(Some(env), program);
}

fn fold_environment_constant_buffer(
    env: &mut dyn Environment,
    program: &mut Program,
    inst_ref: InstRef,
) {
    let has_hle_macro_state = env.has_hle_macro_state();
    let is_proprietary_driver = env.is_proprietary_driver();
    let inst = program.block(inst_ref.block).inst(inst_ref.inst).clone();
    let (Some(&Value::ImmU32(bank)), Some(&Value::ImmU32(offset))) =
        (inst.args.first(), inst.args.get(1))
    else {
        return;
    };
    if has_hle_macro_state {
        if let Some(replacement) = env.get_replace_const_buffer(bank, offset) {
            let attribute = match replacement {
                ReplaceConstant::BaseInstance => Attribute::BASE_INSTANCE,
                ReplaceConstant::BaseVertex => Attribute::BASE_VERTEX,
                ReplaceConstant::DrawID => Attribute::DRAW_ID,
            };
            let current = program.block_mut(inst_ref.block).inst_mut(inst_ref.inst);
            current.opcode = if inst.opcode == Opcode::GetCbufU32 {
                Opcode::GetAttributeU32
            } else {
                Opcode::GetAttribute
            };
            current.args = vec![Value::Attribute(attribute), Value::ImmU32(0)];
            return;
        }
    }
    if is_proprietary_driver && bank == 1 {
        let value = env.read_cbuf_value(bank, offset);
        let replacement = if inst.opcode == Opcode::GetCbufF32 {
            Value::ImmF32(f32::from_bits(value))
        } else {
            Value::ImmU32(value)
        };
        replace_with_identity(program, inst_ref, replacement);
    }
}

fn propagate(inst: &mut Inst) {
    // Upstream's FoldWhenAllImmediates and FoldAdd preserve an instruction
    // while it owns pseudo-operations: the backend emits those flags together
    // with their parent. Folding the parent would orphan Get*FromOp users.
    if inst.associated.is_some() {
        return;
    }

    match inst.opcode {
        // ── Special register/predicate folding ────────────────────────
        Opcode::GetRegister => {
            if let Some(Value::Reg(r)) = inst.args.first() {
                if r.is_zero() {
                    // RZ always reads as 0
                    inst.opcode = Opcode::Identity;
                    inst.args = vec![Value::ImmU32(0)];
                }
            }
        }
        Opcode::GetPred => {
            if let Some(Value::Pred(p)) = inst.args.first() {
                if p.is_true() {
                    // PT always reads as true
                    inst.opcode = Opcode::Identity;
                    inst.args = vec![Value::ImmU1(true)];
                }
            }
        }

        // ── Integer arithmetic folding ────────────────────────────────
        Opcode::IMul32 => {
            if let (Some(&Value::ImmU32(a)), Some(&Value::ImmU32(b))) =
                (inst.args.get(0), inst.args.get(1))
            {
                inst.opcode = Opcode::Identity;
                inst.args = vec![Value::ImmU32(a.wrapping_mul(b))];
            }
            // x * 0 = 0
            else if matches!(inst.args.get(1), Some(&Value::ImmU32(0))) {
                inst.opcode = Opcode::Identity;
                inst.args = vec![Value::ImmU32(0)];
            }
            // x * 1 = x
            else if matches!(inst.args.get(1), Some(&Value::ImmU32(1))) {
                let a = inst.args[0];
                inst.opcode = Opcode::Identity;
                inst.args = vec![a];
            }
        }
        Opcode::INeg32 => {
            if let Some(&Value::ImmU32(a)) = inst.args.first() {
                inst.opcode = Opcode::Identity;
                inst.args = vec![Value::ImmU32((a as i32).wrapping_neg() as u32)];
            }
        }

        // ── Shift folding ─────────────────────────────────────────────
        Opcode::ShiftLeftLogical32 => {
            if let (Some(&Value::ImmU32(a)), Some(&Value::ImmU32(b))) =
                (inst.args.get(0), inst.args.get(1))
            {
                let shift = b & 0x1F;
                inst.opcode = Opcode::Identity;
                inst.args = vec![Value::ImmU32(a.wrapping_shl(shift))];
            }
        }
        Opcode::ShiftRightLogical32 => {
            if let (Some(&Value::ImmU32(a)), Some(&Value::ImmU32(b))) =
                (inst.args.get(0), inst.args.get(1))
            {
                let shift = b & 0x1F;
                inst.opcode = Opcode::Identity;
                inst.args = vec![Value::ImmU32(a.wrapping_shr(shift))];
            }
        }
        Opcode::ShiftRightArithmetic32 => {
            if let (Some(&Value::ImmU32(a)), Some(&Value::ImmU32(b))) =
                (inst.args.get(0), inst.args.get(1))
            {
                let shift = b & 0x1F;
                inst.opcode = Opcode::Identity;
                inst.args = vec![Value::ImmU32((a as i32).wrapping_shr(shift) as u32)];
            }
        }

        // ── Bitwise folding ──────────────────────────────────────────
        Opcode::BitwiseAnd32 => {
            if let (Some(&Value::ImmU32(a)), Some(&Value::ImmU32(b))) =
                (inst.args.get(0), inst.args.get(1))
            {
                inst.opcode = Opcode::Identity;
                inst.args = vec![Value::ImmU32(a & b)];
            }
        }
        Opcode::BitwiseOr32 => {
            if let (Some(&Value::ImmU32(a)), Some(&Value::ImmU32(b))) =
                (inst.args.get(0), inst.args.get(1))
            {
                inst.opcode = Opcode::Identity;
                inst.args = vec![Value::ImmU32(a | b)];
            }
        }
        Opcode::BitwiseXor32 => {
            if let (Some(&Value::ImmU32(a)), Some(&Value::ImmU32(b))) =
                (inst.args.get(0), inst.args.get(1))
            {
                inst.opcode = Opcode::Identity;
                inst.args = vec![Value::ImmU32(a ^ b)];
            }
        }
        Opcode::BitwiseNot32 => {
            if let Some(&Value::ImmU32(a)) = inst.args.first() {
                inst.opcode = Opcode::Identity;
                inst.args = vec![Value::ImmU32(!a)];
            }
        }
        Opcode::BitFieldUExtract => {
            if let (
                Some(&Value::ImmU32(base)),
                Some(&Value::ImmU32(shift)),
                Some(&Value::ImmU32(count)),
            ) = (inst.args.first(), inst.args.get(1), inst.args.get(2))
            {
                assert!(
                    (shift as usize).saturating_add(count as usize) <= 32,
                    "undefined BitFieldUExtract({base}, {shift}, {count})"
                );
                let mask = if count == 32 {
                    u32::MAX
                } else {
                    (1u32 << count) - 1
                };
                inst.opcode = Opcode::Identity;
                inst.args = vec![Value::ImmU32((base >> shift) & mask)];
            }
        }
        Opcode::BitFieldSExtract => {
            if let (
                Some(&Value::ImmU32(base)),
                Some(&Value::ImmU32(shift)),
                Some(&Value::ImmU32(count)),
            ) = (inst.args.first(), inst.args.get(1), inst.args.get(2))
            {
                let back_shift = (shift as usize).saturating_add(count as usize);
                let left_shift = 32usize.saturating_sub(back_shift);
                let right_shift = 32usize.saturating_sub(count as usize);
                assert!(
                    back_shift <= 32 && left_shift < 32 && right_shift < 32,
                    "undefined BitFieldSExtract({base}, {shift}, {count})"
                );
                let value = ((base as i32) << left_shift) >> right_shift;
                inst.opcode = Opcode::Identity;
                inst.args = vec![Value::ImmU32(value as u32)];
            }
        }
        Opcode::BitFieldInsert => {
            if let (
                Some(&Value::ImmU32(base)),
                Some(&Value::ImmU32(insert)),
                Some(&Value::ImmU32(offset)),
                Some(&Value::ImmU32(bits)),
            ) = (
                inst.args.first(),
                inst.args.get(1),
                inst.args.get(2),
                inst.args.get(3),
            ) {
                assert!(
                    bits < 32 && offset < 32,
                    "undefined BitFieldInsert({base}, {insert}, {offset}, {bits})"
                );
                let field_mask = !(!0u32 << bits) << offset;
                inst.opcode = Opcode::Identity;
                inst.args = vec![Value::ImmU32(
                    (base & !field_mask) | (insert.wrapping_shl(offset) & field_mask),
                )];
            }
        }

        // ── FP32 arithmetic folding ──────────────────────────────────
        Opcode::FPAdd32 => {
            if let (Some(&Value::ImmF32(a)), Some(&Value::ImmF32(b))) =
                (inst.args.get(0), inst.args.get(1))
            {
                inst.opcode = Opcode::Identity;
                inst.args = vec![Value::ImmF32(a + b)];
            } else if matches!(inst.args.first(), Some(&Value::ImmF32(value)) if value.abs() == 0.0)
            {
                let rhs = inst.args[1];
                inst.opcode = Opcode::Identity;
                inst.args = vec![rhs];
            } else if matches!(inst.args.get(1), Some(&Value::ImmF32(value)) if value.abs() == 0.0)
            {
                let lhs = inst.args[0];
                inst.opcode = Opcode::Identity;
                inst.args = vec![lhs];
            }
        }
        Opcode::FPNeg32 => {
            if let Some(&Value::ImmF32(a)) = inst.args.first() {
                inst.opcode = Opcode::Identity;
                inst.args = vec![Value::ImmF32(-a)];
            }
        }
        Opcode::FPAbs32 => {
            if let Some(&Value::ImmF32(a)) = inst.args.first() {
                inst.opcode = Opcode::Identity;
                inst.args = vec![Value::ImmF32(a.abs())];
            }
        }

        // ── Bitcast folding ──────────────────────────────────────────
        Opcode::BitCastU32F32 => {
            if let Some(&Value::ImmF32(a)) = inst.args.first() {
                inst.opcode = Opcode::Identity;
                inst.args = vec![Value::ImmU32(a.to_bits())];
            }
        }
        Opcode::BitCastF32U32 => {
            if let Some(&Value::ImmU32(a)) = inst.args.first() {
                inst.opcode = Opcode::Identity;
                inst.args = vec![Value::ImmF32(f32::from_bits(a))];
            }
        }

        // ── Conversion folding ───────────────────────────────────────
        Opcode::ConvertF32U32 => {
            if let Some(&Value::ImmU32(a)) = inst.args.first() {
                inst.opcode = Opcode::Identity;
                inst.args = vec![Value::ImmF32(a as f32)];
            }
        }
        Opcode::ConvertF32S32 => {
            if let Some(&Value::ImmU32(a)) = inst.args.first() {
                inst.opcode = Opcode::Identity;
                inst.args = vec![Value::ImmF32(a as i32 as f32)];
            }
        }
        Opcode::ConvertU32F32 => {
            if let Some(&Value::ImmF32(a)) = inst.args.first() {
                inst.opcode = Opcode::Identity;
                inst.args = vec![Value::ImmU32(a as u32)];
            }
        }
        Opcode::ConvertS32F32 => {
            if let Some(&Value::ImmF32(a)) = inst.args.first() {
                inst.opcode = Opcode::Identity;
                inst.args = vec![Value::ImmU32(a as i32 as u32)];
            }
        }

        // ── Logic folding ────────────────────────────────────────────
        // ── Select folding ───────────────────────────────────────────
        Opcode::SelectU1
        | Opcode::SelectU8
        | Opcode::SelectU16
        | Opcode::SelectU32
        | Opcode::SelectU64
        | Opcode::SelectF16
        | Opcode::SelectF32
        | Opcode::SelectF64 => {
            if let Some(&Value::ImmU1(cond)) = inst.args.first() {
                let result = if cond { inst.args[1] } else { inst.args[2] };
                inst.opcode = Opcode::Identity;
                inst.args = vec![result];
            }
        }

        // ── Integer comparison folding ───────────────────────────────
        Opcode::IEqual => {
            if let (Some(&Value::ImmU32(a)), Some(&Value::ImmU32(b))) =
                (inst.args.get(0), inst.args.get(1))
            {
                inst.opcode = Opcode::Identity;
                inst.args = vec![Value::ImmU1(a == b)];
            }
        }
        Opcode::INotEqual => {
            if let (Some(&Value::ImmU32(a)), Some(&Value::ImmU32(b))) =
                (inst.args.get(0), inst.args.get(1))
            {
                inst.opcode = Opcode::Identity;
                inst.args = vec![Value::ImmU1(a != b)];
            }
        }
        Opcode::SLessThan => {
            if let (Some(&Value::ImmU32(a)), Some(&Value::ImmU32(b))) =
                (inst.args.get(0), inst.args.get(1))
            {
                inst.opcode = Opcode::Identity;
                inst.args = vec![Value::ImmU1((a as i32) < (b as i32))];
            }
        }
        Opcode::ULessThan => {
            if let (Some(&Value::ImmU32(a)), Some(&Value::ImmU32(b))) =
                (inst.args.get(0), inst.args.get(1))
            {
                inst.opcode = Opcode::Identity;
                inst.args = vec![Value::ImmU1(a < b)];
            }
        }
        Opcode::SLessThanEqual
        | Opcode::ULessThanEqual
        | Opcode::SGreaterThan
        | Opcode::UGreaterThan
        | Opcode::SGreaterThanEqual
        | Opcode::UGreaterThanEqual => {
            if let (Some(&Value::ImmU32(a)), Some(&Value::ImmU32(b))) =
                (inst.args.first(), inst.args.get(1))
            {
                let result = match inst.opcode {
                    Opcode::SLessThanEqual => (a as i32) <= (b as i32),
                    Opcode::ULessThanEqual => a <= b,
                    Opcode::SGreaterThan => (a as i32) > (b as i32),
                    Opcode::UGreaterThan => a > b,
                    Opcode::SGreaterThanEqual => (a as i32) >= (b as i32),
                    Opcode::UGreaterThanEqual => a >= b,
                    _ => unreachable!(),
                };
                inst.opcode = Opcode::Identity;
                inst.args = vec![Value::ImmU1(result)];
            }
        }

        _ => {
            // No folding for this opcode
        }
    }
}

fn fold_instruction_reference(
    program: &mut Program,
    inst_ref: InstRef,
    preserves_depth_interpolation: bool,
) {
    let opcode = program.block(inst_ref.block).inst(inst_ref.inst).opcode;
    match opcode {
        Opcode::IAdd32 => fold_add32(program, inst_ref),
        Opcode::IAdd64 => fold_add64(program, inst_ref),
        Opcode::ISub32 => fold_isub32(program, inst_ref),
        Opcode::LogicalAnd => fold_logical_commutative(program, inst_ref, false),
        Opcode::LogicalOr => fold_logical_commutative(program, inst_ref, true),
        Opcode::BitCastU32F32 => fold_bitcast(program, inst_ref, Opcode::BitCastF32U32),
        Opcode::BitCastF32U32 => fold_bitcast(program, inst_ref, Opcode::BitCastU32F32),
        Opcode::PackHalf2x16 => fold_inverse_function(program, inst_ref, Opcode::UnpackHalf2x16),
        Opcode::UnpackHalf2x16 => fold_inverse_function(program, inst_ref, Opcode::PackHalf2x16),
        Opcode::PackFloat2x16 => fold_inverse_function(program, inst_ref, Opcode::UnpackFloat2x16),
        Opcode::UnpackFloat2x16 => fold_inverse_function(program, inst_ref, Opcode::PackFloat2x16),
        Opcode::CompositeExtractU32x2 => fold_composite_extract(
            program,
            inst_ref,
            Opcode::CompositeConstructU32x2,
            Opcode::CompositeInsertU32x2,
        ),
        Opcode::CompositeExtractU32x3 => fold_composite_extract(
            program,
            inst_ref,
            Opcode::CompositeConstructU32x3,
            Opcode::CompositeInsertU32x3,
        ),
        Opcode::CompositeExtractU32x4 => fold_composite_extract(
            program,
            inst_ref,
            Opcode::CompositeConstructU32x4,
            Opcode::CompositeInsertU32x4,
        ),
        Opcode::CompositeExtractF16x2 => fold_composite_extract(
            program,
            inst_ref,
            Opcode::CompositeConstructF16x2,
            Opcode::CompositeInsertF16x2,
        ),
        Opcode::CompositeExtractF16x3 => fold_composite_extract(
            program,
            inst_ref,
            Opcode::CompositeConstructF16x3,
            Opcode::CompositeInsertF16x3,
        ),
        Opcode::CompositeExtractF16x4 => fold_composite_extract(
            program,
            inst_ref,
            Opcode::CompositeConstructF16x4,
            Opcode::CompositeInsertF16x4,
        ),
        Opcode::CompositeExtractF32x2 => fold_composite_extract(
            program,
            inst_ref,
            Opcode::CompositeConstructF32x2,
            Opcode::CompositeInsertF32x2,
        ),
        Opcode::CompositeExtractF32x3 => fold_composite_extract(
            program,
            inst_ref,
            Opcode::CompositeConstructF32x3,
            Opcode::CompositeInsertF32x3,
        ),
        Opcode::CompositeExtractF32x4 => fold_composite_extract(
            program,
            inst_ref,
            Opcode::CompositeConstructF32x4,
            Opcode::CompositeInsertF32x4,
        ),
        Opcode::FPMul32 => fold_fp_mul32(program, inst_ref, preserves_depth_interpolation),
        Opcode::LogicalNot => fold_logical_not(program, inst_ref),
        Opcode::FSwizzleAdd => fold_fswizzle_add(program, inst_ref),
        Opcode::ImageSampleImplicitLod
        | Opcode::BoundImageSampleImplicitLod
        | Opcode::BindlessImageSampleImplicitLod => {
            fold_image_sample_implicit_lod(program, inst_ref)
        }
        _ => {}
    }
}

fn fold_logical_commutative(program: &mut Program, inst_ref: InstRef, is_or: bool) {
    let inst = program.block(inst_ref.block).inst(inst_ref.inst).clone();
    if inst.opcode == Opcode::Identity {
        return;
    }
    let [lhs, rhs, ..] = inst.args.as_slice() else {
        return;
    };
    let combine = |a: bool, b: bool| if is_or { a || b } else { a && b };
    match (*lhs, *rhs) {
        (Value::ImmU1(a), Value::ImmU1(b)) => {
            replace_with_identity(program, inst_ref, Value::ImmU1(combine(a, b)));
            return;
        }
        (Value::ImmU1(a), rhs) => {
            if let Some((_, rhs_inst)) = instruction_with_opcode(program, rhs, inst.opcode) {
                if let Some(Value::ImmU1(b)) = rhs_inst.args.get(1).copied() {
                    let current = program.block_mut(inst_ref.block).inst_mut(inst_ref.inst);
                    current.args[0] = rhs_inst.args[0];
                    current.args[1] = Value::ImmU1(combine(a, b));
                } else {
                    let current = program.block_mut(inst_ref.block).inst_mut(inst_ref.inst);
                    current.args[0] = rhs;
                    current.args[1] = Value::ImmU1(a);
                }
            } else {
                let current = program.block_mut(inst_ref.block).inst_mut(inst_ref.inst);
                current.args[0] = rhs;
                current.args[1] = Value::ImmU1(a);
            }
        }
        (lhs, Value::ImmU1(b)) => {
            if let Some((_, lhs_inst)) = instruction_with_opcode(program, lhs, inst.opcode) {
                if let Some(Value::ImmU1(a)) = lhs_inst.args.get(1).copied() {
                    let current = program.block_mut(inst_ref.block).inst_mut(inst_ref.inst);
                    current.args[0] = lhs_inst.args[0];
                    current.args[1] = Value::ImmU1(combine(b, a));
                }
            }
        }
        _ => {}
    }
    let current = program.block(inst_ref.block).inst(inst_ref.inst).clone();
    if let Some(Value::ImmU1(value)) = current.args.get(1).copied() {
        let replacement = if is_or {
            if value {
                Value::ImmU1(true)
            } else {
                current.args[0]
            }
        } else if value {
            current.args[0]
        } else {
            Value::ImmU1(false)
        };
        replace_with_identity(program, inst_ref, replacement);
    }
}

fn fold_add32(program: &mut Program, inst_ref: InstRef) {
    if program
        .block(inst_ref.block)
        .inst(inst_ref.inst)
        .associated
        .is_some()
    {
        return;
    }
    fold_commutative_u32(program, inst_ref, u32::wrapping_add);
    let inst = program.block(inst_ref.block).inst(inst_ref.inst).clone();
    if inst.opcode == Opcode::Identity {
        return;
    }
    if inst.args.get(1) == Some(&Value::ImmU32(0)) {
        replace_with_identity(program, inst_ref, inst.args[0]);
        return;
    }
    fold_xmad(program, inst_ref);
}

fn fold_add64(program: &mut Program, inst_ref: InstRef) {
    if program
        .block(inst_ref.block)
        .inst(inst_ref.inst)
        .associated
        .is_some()
    {
        return;
    }
    fold_commutative_u64(program, inst_ref, u64::wrapping_add);
    let inst = program.block(inst_ref.block).inst(inst_ref.inst).clone();
    if inst.opcode != Opcode::Identity && inst.args.get(1) == Some(&Value::ImmU64(0)) {
        replace_with_identity(program, inst_ref, inst.args[0]);
    }
}

fn fold_commutative_u32(
    program: &mut Program,
    inst_ref: InstRef,
    imm_fn: impl Fn(u32, u32) -> u32,
) {
    let inst = program.block(inst_ref.block).inst(inst_ref.inst).clone();
    let [lhs, rhs, ..] = inst.args.as_slice() else {
        return;
    };
    match (*lhs, *rhs) {
        (Value::ImmU32(a), Value::ImmU32(b)) => {
            replace_with_identity(program, inst_ref, Value::ImmU32(imm_fn(a, b)));
        }
        (Value::ImmU32(a), rhs) => {
            if let Some((_, rhs_inst)) = instruction_with_opcode(program, rhs, inst.opcode) {
                if let Some(Value::ImmU32(b)) = rhs_inst.args.get(1).copied() {
                    let current = program.block_mut(inst_ref.block).inst_mut(inst_ref.inst);
                    current.args[0] = rhs_inst.args[0];
                    current.args[1] = Value::ImmU32(imm_fn(a, b));
                    return;
                }
            }
            let current = program.block_mut(inst_ref.block).inst_mut(inst_ref.inst);
            current.args[0] = rhs;
            current.args[1] = Value::ImmU32(a);
        }
        (lhs, Value::ImmU32(b)) => {
            if let Some((_, lhs_inst)) = instruction_with_opcode(program, lhs, inst.opcode) {
                if let Some(Value::ImmU32(a)) = lhs_inst.args.get(1).copied() {
                    let current = program.block_mut(inst_ref.block).inst_mut(inst_ref.inst);
                    current.args[0] = lhs_inst.args[0];
                    current.args[1] = Value::ImmU32(imm_fn(b, a));
                }
            }
        }
        _ => {}
    }
}

fn fold_commutative_u64(
    program: &mut Program,
    inst_ref: InstRef,
    imm_fn: impl Fn(u64, u64) -> u64,
) {
    let inst = program.block(inst_ref.block).inst(inst_ref.inst).clone();
    let [lhs, rhs, ..] = inst.args.as_slice() else {
        return;
    };
    match (*lhs, *rhs) {
        (Value::ImmU64(a), Value::ImmU64(b)) => {
            replace_with_identity(program, inst_ref, Value::ImmU64(imm_fn(a, b)));
        }
        (Value::ImmU64(a), rhs) => {
            if let Some((_, rhs_inst)) = instruction_with_opcode(program, rhs, inst.opcode) {
                if let Some(Value::ImmU64(b)) = rhs_inst.args.get(1).copied() {
                    let current = program.block_mut(inst_ref.block).inst_mut(inst_ref.inst);
                    current.args[0] = rhs_inst.args[0];
                    current.args[1] = Value::ImmU64(imm_fn(a, b));
                    return;
                }
            }
            let current = program.block_mut(inst_ref.block).inst_mut(inst_ref.inst);
            current.args[0] = rhs;
            current.args[1] = Value::ImmU64(a);
        }
        (lhs, Value::ImmU64(b)) => {
            if let Some((_, lhs_inst)) = instruction_with_opcode(program, lhs, inst.opcode) {
                if let Some(Value::ImmU64(a)) = lhs_inst.args.get(1).copied() {
                    let current = program.block_mut(inst_ref.block).inst_mut(inst_ref.inst);
                    current.args[0] = lhs_inst.args[0];
                    current.args[1] = Value::ImmU64(imm_fn(b, a));
                }
            }
        }
        _ => {}
    }
}

fn equal_cbuf(program: &Program, lhs: InstRef, rhs: InstRef) -> bool {
    let lhs = program.block(lhs.block).inst(lhs.inst);
    let rhs = program.block(rhs.block).inst(rhs.inst);
    lhs.opcode == Opcode::GetCbufU32
        && rhs.opcode == Opcode::GetCbufU32
        && lhs.args.first() == rhs.args.first()
        && lhs.args.get(1) == rhs.args.get(1)
}

fn fold_isub32(program: &mut Program, inst_ref: InstRef) {
    let inst = program.block(inst_ref.block).inst(inst_ref.inst).clone();
    if let (Some(&Value::ImmU32(lhs)), Some(&Value::ImmU32(rhs))) =
        (inst.args.first(), inst.args.get(1))
    {
        if inst.associated.is_none() {
            replace_with_identity(program, inst_ref, Value::ImmU32(lhs.wrapping_sub(rhs)));
        }
        return;
    }
    if inst.args.first().is_some_and(Value::is_immediate)
        || inst.args.get(1).is_some_and(Value::is_immediate)
    {
        return;
    }
    let (Some(mut op_a), Some(mut op_b)) = (
        inst_recursive(inst.args[0], program),
        inst_recursive(inst.args[1], program),
    ) else {
        return;
    };
    if equal_cbuf(program, op_a, op_b) {
        replace_with_identity(program, inst_ref, Value::ImmU32(0));
        return;
    }
    if program.block(op_b.block).inst(op_b.inst).opcode == Opcode::IAdd32 {
        std::mem::swap(&mut op_a, &mut op_b);
    }
    if program.block(op_b.block).inst(op_b.inst).opcode != Opcode::GetCbufU32 {
        return;
    }
    if program.block(op_a.block).inst(op_a.inst).opcode != Opcode::IAdd32 {
        return;
    }
    let add = program.block(op_a.block).inst(op_a.inst).clone();
    let (mut add_op_a, mut add_op_b) = (add.args[0], add.args[1]);
    if add_op_b.is_immediate() {
        std::mem::swap(&mut add_op_a, &mut add_op_b);
    }
    if add_op_b.is_immediate() {
        return;
    }
    let Some(add_cbuf) = inst_recursive(add_op_b, program) else {
        return;
    };
    if equal_cbuf(program, add_cbuf, op_b) {
        replace_with_identity(program, inst_ref, add_op_a);
    }
}

fn values_are_equal(program: &Program, values: &[Value]) -> bool {
    let Some(&first) = values.first() else {
        return true;
    };
    let first = resolve_value(first, program);
    values.iter().skip(1).all(|&value| {
        let value = resolve_value(value, program);
        if value == first {
            return true;
        }
        let (Some(lhs), Some(rhs)) = (
            inst_recursive(first, program),
            inst_recursive(value, program),
        ) else {
            return false;
        };
        let lhs = program.block(lhs.block).inst(lhs.inst);
        let rhs = program.block(rhs.block).inst(rhs.inst);
        lhs.opcode == Opcode::GetCbufU32
            && rhs.opcode == Opcode::GetCbufU32
            && lhs.args.first() == rhs.args.first()
            && lhs.args.get(1) == rhs.args.get(1)
    })
}

fn instruction_with_opcode(
    program: &Program,
    value: Value,
    opcode: Opcode,
) -> Option<(InstRef, Inst)> {
    let inst_ref = inst_recursive(value, program)?;
    let inst = program.block(inst_ref.block).inst(inst_ref.inst);
    (inst.opcode == opcode).then(|| (inst_ref, inst.clone()))
}

/// Port of upstream `FoldXmadMultiply` and `FoldXmadMultiplyAdd`.
///
/// Maxwell integer FMA lowering can express an ordinary low-32-bit multiply
/// as several 16-bit extracts, multiplies and adds. Upstream recognizes the
/// exact generated shapes and restores a direct `IMul32` (plus the addend for
/// the FMA form), allowing the SPIR-V backend to emit one `OpIMul`.
fn fold_xmad(program: &mut Program, inst_ref: InstRef) {
    if program
        .block(inst_ref.block)
        .inst(inst_ref.inst)
        .associated
        .is_some()
    {
        return;
    }
    if fold_xmad_multiply(program, inst_ref) {
        return;
    }
    let _ = fold_xmad_multiply_add(program, inst_ref);
}

fn fold_xmad_multiply(program: &mut Program, inst_ref: InstRef) -> bool {
    let inst = program.block(inst_ref.block).inst(inst_ref.inst).clone();
    let (Some(&lhs_value), Some(&rhs_value)) = (inst.args.first(), inst.args.get(1)) else {
        return false;
    };
    let Some((_, lhs_shl)) =
        instruction_with_opcode(program, lhs_value, Opcode::ShiftLeftLogical32)
    else {
        return false;
    };
    if lhs_shl.args.get(1) != Some(&Value::ImmU32(16)) {
        return false;
    }
    let Some((_, lhs_mul)) = instruction_with_opcode(program, lhs_shl.args[0], Opcode::IMul32)
    else {
        return false;
    };
    let Some((_, rhs_mul)) = instruction_with_opcode(program, rhs_value, Opcode::IMul32) else {
        return false;
    };
    if !values_are_equal(program, &[lhs_mul.args[1], rhs_mul.args[1]]) {
        return false;
    }
    let Some((_, lhs_bfe)) =
        instruction_with_opcode(program, lhs_mul.args[0], Opcode::BitFieldUExtract)
    else {
        return false;
    };
    let Some((_, rhs_bfe)) =
        instruction_with_opcode(program, rhs_mul.args[0], Opcode::BitFieldUExtract)
    else {
        return false;
    };
    if lhs_bfe.args.get(1) != Some(&Value::ImmU32(16))
        || lhs_bfe.args.get(2) != Some(&Value::ImmU32(16))
        || rhs_bfe.args.get(1) != Some(&Value::ImmU32(0))
        || rhs_bfe.args.get(2) != Some(&Value::ImmU32(16))
        || !values_are_equal(program, &[lhs_bfe.args[0], rhs_bfe.args[0]])
    {
        return false;
    }
    let factor_a = resolve_value(lhs_bfe.args[0], program);
    let factor_b = resolve_value(lhs_mul.args[1], program);
    let multiply = program.block_mut(inst_ref.block).insert_inst_before(
        inst_ref.inst,
        Inst::new(Opcode::IMul32, vec![factor_a, factor_b]),
    );
    replace_with_identity(
        program,
        inst_ref,
        Value::Inst(InstRef {
            block: inst_ref.block,
            inst: multiply,
        }),
    );
    true
}

fn fold_xmad_multiply_add(program: &mut Program, inst_ref: InstRef) -> bool {
    let inst = program.block(inst_ref.block).inst(inst_ref.inst).clone();
    let (Some(&value_25), Some(&value_27)) = (inst.args.first(), inst.args.get(1)) else {
        return false;
    };
    let Some((_, inst_25)) = instruction_with_opcode(program, value_25, Opcode::ShiftLeftLogical32)
    else {
        return false;
    };
    if inst_25.args.get(1) != Some(&Value::ImmU32(16)) {
        return false;
    }
    let Some((_, inst_27)) = instruction_with_opcode(program, value_27, Opcode::IAdd32) else {
        return false;
    };
    let Some((_, inst_24)) = instruction_with_opcode(program, inst_25.args[0], Opcode::IMul32)
    else {
        return false;
    };
    let Some((_, inst_22)) =
        instruction_with_opcode(program, inst_24.args[0], Opcode::BitFieldUExtract)
    else {
        return false;
    };
    let Some((_, inst_23)) =
        instruction_with_opcode(program, inst_24.args[1], Opcode::BitFieldUExtract)
    else {
        return false;
    };
    if inst_22.args.get(1) != Some(&Value::ImmU32(16))
        || inst_22.args.get(2) != Some(&Value::ImmU32(16))
        || inst_23.args.get(1) != Some(&Value::ImmU32(16))
        || inst_23.args.get(2) != Some(&Value::ImmU32(16))
    {
        return false;
    }
    let Some((inst_11_ref, inst_11)) =
        instruction_with_opcode(program, inst_23.args[0], Opcode::BitFieldInsert)
    else {
        return false;
    };
    if inst_11.args.get(2) != Some(&Value::ImmU32(16))
        || inst_11.args.get(3) != Some(&Value::ImmU32(16))
    {
        return false;
    }
    let Some((_, inst_8)) = instruction_with_opcode(program, inst_11.args[0], Opcode::IMul32)
    else {
        return false;
    };
    let Some((_, inst_10)) =
        instruction_with_opcode(program, inst_11.args[1], Opcode::BitFieldUExtract)
    else {
        return false;
    };
    let Some((_, inst_6)) =
        instruction_with_opcode(program, inst_8.args[0], Opcode::BitFieldUExtract)
    else {
        return false;
    };
    let Some((_, inst_7)) =
        instruction_with_opcode(program, inst_8.args[1], Opcode::BitFieldUExtract)
    else {
        return false;
    };
    if inst_6.args.get(1) != Some(&Value::ImmU32(0))
        || inst_6.args.get(2) != Some(&Value::ImmU32(16))
        || inst_7.args.get(1) != Some(&Value::ImmU32(16))
        || inst_7.args.get(2) != Some(&Value::ImmU32(16))
    {
        return false;
    }
    let Some((_, inst_26)) =
        instruction_with_opcode(program, inst_27.args[0], Opcode::ShiftLeftLogical32)
    else {
        return false;
    };
    if inst_26.args.get(1) != Some(&Value::ImmU32(16))
        || inst_recursive(inst_26.args[0], program) != Some(inst_11_ref)
    {
        return false;
    }
    let Some((_, inst_18)) = instruction_with_opcode(program, inst_27.args[1], Opcode::IAdd32)
    else {
        return false;
    };
    let Some((_, inst_17)) = instruction_with_opcode(program, inst_18.args[0], Opcode::IMul32)
    else {
        return false;
    };
    let Some((_, inst_15)) =
        instruction_with_opcode(program, inst_17.args[0], Opcode::BitFieldUExtract)
    else {
        return false;
    };
    let Some((_, inst_16)) =
        instruction_with_opcode(program, inst_17.args[1], Opcode::BitFieldUExtract)
    else {
        return false;
    };
    if inst_15.args.get(1) != Some(&Value::ImmU32(0))
        || inst_16.args.get(1) != Some(&Value::ImmU32(0))
        || inst_10.args.get(1) != Some(&Value::ImmU32(0))
        || inst_15.args.get(2) != Some(&Value::ImmU32(16))
        || inst_16.args.get(2) != Some(&Value::ImmU32(16))
        || inst_10.args.get(2) != Some(&Value::ImmU32(16))
        || !values_are_equal(program, &[inst_7.args[0], inst_16.args[0], inst_10.args[0]])
        || !values_are_equal(program, &[inst_22.args[0], inst_6.args[0], inst_15.args[0]])
    {
        return false;
    }

    let op_a = resolve_value(inst_7.args[0], program);
    let op_b = resolve_value(inst_6.args[0], program);
    let op_c = resolve_value(inst_18.args[1], program);
    let multiply = program
        .block_mut(inst_ref.block)
        .insert_inst_before(inst_ref.inst, Inst::new(Opcode::IMul32, vec![op_a, op_b]));
    let add = program.block_mut(inst_ref.block).insert_inst_before(
        inst_ref.inst,
        Inst::new(
            Opcode::IAdd32,
            vec![
                Value::Inst(InstRef {
                    block: inst_ref.block,
                    inst: multiply,
                }),
                op_c,
            ],
        ),
    );
    replace_with_identity(
        program,
        inst_ref,
        Value::Inst(InstRef {
            block: inst_ref.block,
            inst: add,
        }),
    );
    true
}

/// Port of upstream `FoldInverseFunc`.
fn fold_inverse_function(program: &mut Program, inst_ref: InstRef, reverse: Opcode) {
    let value = program
        .block(inst_ref.block)
        .inst(inst_ref.inst)
        .args
        .first()
        .copied();
    let Some(value) = value else {
        return;
    };
    if value.is_immediate() {
        return;
    }
    let Some(arg_ref) = inst_recursive(value, program) else {
        return;
    };
    let arg_inst = program.block(arg_ref.block).inst(arg_ref.inst);
    if arg_inst.opcode == reverse {
        if let Some(replacement) = arg_inst.args.first().copied() {
            replace_with_identity(program, inst_ref, replacement);
        }
    }
}

/// Port of upstream `FoldCompositeExtractImpl`.
fn fold_composite_extract_impl(
    program: &Program,
    value: Value,
    insert: Opcode,
    construct: Opcode,
    first_index: u32,
) -> Option<Value> {
    let inst_ref = inst_recursive(value, program)?;
    let inst = program.block(inst_ref.block).inst(inst_ref.inst);
    if inst.opcode == construct {
        return inst.args.get(first_index as usize).copied();
    }
    if inst.opcode != insert {
        return None;
    }
    let Value::ImmU32(second_index) = *inst.args.get(2)? else {
        return None;
    };
    if first_index == second_index {
        return inst.args.get(1).copied();
    }
    let composite = *inst.args.first()?;
    if composite.is_immediate() {
        return None;
    }
    fold_composite_extract_impl(program, composite, insert, construct, first_index)
}

/// Port of upstream `FoldCompositeExtract`.
fn fold_composite_extract(
    program: &mut Program,
    inst_ref: InstRef,
    construct: Opcode,
    insert: Opcode,
) {
    let inst = program.block(inst_ref.block).inst(inst_ref.inst);
    let (Some(&composite), Some(&Value::ImmU32(index))) = (inst.args.first(), inst.args.get(1))
    else {
        return;
    };
    if composite.is_immediate() {
        return;
    }
    if let Some(result) = fold_composite_extract_impl(program, composite, insert, construct, index)
    {
        replace_with_identity(program, inst_ref, result);
    }
}

/// Port of upstream `FoldLogicalNot`.
fn fold_logical_not(program: &mut Program, inst_ref: InstRef) {
    if let Some(Value::ImmU1(value)) = program
        .block(inst_ref.block)
        .inst(inst_ref.inst)
        .args
        .first()
        .copied()
    {
        replace_with_identity(program, inst_ref, Value::ImmU1(!value));
        return;
    }
    let Some(inner_ref) = inst_recursive(
        program.block(inst_ref.block).inst(inst_ref.inst).args[0],
        program,
    ) else {
        return;
    };
    let inner = program.block(inner_ref.block).inst(inner_ref.inst);
    if inner.opcode == Opcode::LogicalNot {
        replace_with_identity(program, inst_ref, inner.args[0]);
    }
}

fn inst_recursive(value: Value, program: &Program) -> Option<InstRef> {
    let mut current = match value {
        Value::Inst(inst_ref) => inst_ref,
        _ => return None,
    };
    loop {
        let inst = program.block(current.block).inst(current.inst);
        if inst.opcode == Opcode::Identity {
            let Some(Value::Inst(next)) = inst.args.first().copied() else {
                return Some(current);
            };
            current = next;
            continue;
        }
        return Some(current);
    }
}

fn resolve_value(mut value: Value, program: &Program) -> Value {
    while let Value::Inst(inst_ref) = value {
        let inst = program.block(inst_ref.block).inst(inst_ref.inst);
        if inst.opcode != Opcode::Identity || inst.args.is_empty() {
            return value;
        }
        value = inst.args[0];
    }
    value
}

fn get_through_cast(value: Value, expected_cast: Opcode, program: &Program) -> Value {
    let value = resolve_value(value, program);
    let Some(inst_ref) = inst_recursive(value, program) else {
        return value;
    };
    let inst = program.block(inst_ref.block).inst(inst_ref.inst);
    if inst.opcode != expected_cast {
        return value;
    }
    inst.args
        .first()
        .copied()
        .map(|arg| resolve_value(arg, program))
        .unwrap_or(value)
}

fn replace_with_identity(program: &mut Program, inst_ref: InstRef, value: Value) {
    program
        .block_mut(inst_ref.block)
        .inst_mut(inst_ref.inst)
        .replace_uses_with(value);
}

fn fold_fswizzle_add(program: &mut Program, inst_ref: InstRef) {
    let inst = program.block(inst_ref.block).inst(inst_ref.inst).clone();
    let [op_a, op_b, swizzle, ..] = inst.args.as_slice() else {
        return;
    };
    let Value::ImmU32(swizzle) = resolve_value(*swizzle, program) else {
        return;
    };
    if !matches!(swizzle, 0x99 | 0xA5) {
        return;
    }
    let value_1 = get_through_cast(*op_a, Opcode::BitCastF32U32, program);
    let value_2 = get_through_cast(*op_b, Opcode::BitCastF32U32, program);
    if value_1.is_immediate() {
        return;
    }
    let Some(shuffle_ref) = inst_recursive(value_1, program) else {
        return;
    };
    let shuffle = program
        .block(shuffle_ref.block)
        .inst(shuffle_ref.inst)
        .clone();
    if shuffle.opcode != Opcode::ShuffleButterfly {
        return;
    }
    let Some(shuffle_value) = shuffle.args.first().copied() else {
        return;
    };
    let value_3 = get_through_cast(shuffle_value, Opcode::BitCastU32F32, program);
    if value_2 != value_3 {
        match (value_2, value_3) {
            (Value::ImmF32(lhs), Value::ImmU32(rhs)) if lhs.to_bits() == rhs => {}
            _ => return,
        }
    }
    let (
        Some(Value::ImmU32(index)),
        Some(Value::ImmU32(clamp)),
        Some(Value::ImmU32(segmentation_mask)),
    ) = (
        shuffle
            .args
            .get(1)
            .map(|value| resolve_value(*value, program)),
        shuffle
            .args
            .get(2)
            .map(|value| resolve_value(*value, program)),
        shuffle
            .args
            .get(3)
            .map(|value| resolve_value(*value, program)),
    )
    else {
        return;
    };
    if clamp != 3 || segmentation_mask != 28 {
        return;
    }
    let derivative = match (swizzle, index) {
        (0x99, 1) => Opcode::DPdxFine,
        (0xA5, 2) => Opcode::DPdyFine,
        _ => return,
    };
    let derivative_index = program
        .block_mut(inst_ref.block)
        .insert_inst_before(inst_ref.inst, Inst::new(derivative, vec![*op_b]));
    replace_with_identity(
        program,
        inst_ref,
        Value::Inst(InstRef {
            block: inst_ref.block,
            inst: derivative_index,
        }),
    );
}

fn resolved_type(value: Value, program: &Program) -> Type {
    match resolve_value(value, program) {
        Value::Inst(inst_ref) => program
            .block(inst_ref.block)
            .inst(inst_ref.inst)
            .return_type(),
        value => value.ir_type(),
    }
}

fn check_through_shuffle(program: &Program, input: Value) -> Option<Value> {
    let value = get_through_cast(
        resolve_value(input, program),
        Opcode::BitCastF32U32,
        program,
    );
    let inst_ref = inst_recursive(value, program)?;
    let inst = program.block(inst_ref.block).inst(inst_ref.inst);
    if inst.opcode != Opcode::ShuffleIndex {
        return None;
    }
    let index = resolve_value(inst.args[1], program);
    let clamp = resolve_value(inst.args[2], program);
    let segmentation_mask = resolve_value(inst.args[3], program);
    let (Value::ImmU32(index), Value::ImmU32(clamp), Value::ImmU32(_)) =
        (index, clamp, segmentation_mask)
    else {
        return None;
    };
    // Preserve upstream's condition literally (`&&`, not `||`).
    if index != 3 && clamp != 3 {
        return None;
    }
    Some(get_through_cast(
        resolve_value(inst.args[0], program),
        Opcode::BitCastU32F32,
        program,
    ))
}

fn resolve_swizzle_mask(value: Value) -> Option<[u32; 4]> {
    let Value::ImmU32(value) = value else {
        return None;
    };
    Some(std::array::from_fn(|index| (value >> (index * 2)) & 0x3))
}

fn resolve_pending_gradient(program: &Program, value: Value) -> Option<([Value; 2], [u32; 4])> {
    let inst_ref = inst_recursive(value, program)?;
    let inst = program.block(inst_ref.block).inst(inst_ref.inst);
    if inst.opcode != Opcode::FSwizzleAdd {
        return None;
    }
    let first = check_through_shuffle(program, resolve_value(inst.args[0], program))?;
    let second = check_through_shuffle(program, resolve_value(inst.args[1], program))?;
    let mask = resolve_swizzle_mask(resolve_value(inst.args[2], program))?;
    Some(([first, second], mask))
}

/// Port of upstream `FindGradient3DDerivatives`.
fn find_gradient_3d_derivatives(program: &Program, coord: Value) -> Option<[Value; 3]> {
    if coord.is_immediate() {
        return None;
    }
    let inst_ref = inst_recursive(coord, program)?;
    let inst = program.block(inst_ref.block).inst(inst_ref.inst);
    if inst.opcode != Opcode::FSwizzleAdd {
        return None;
    }
    let value_1 = resolve_value(inst.args[0], program);
    let value_2 = resolve_value(inst.args[1], program);
    let mask_a = resolve_swizzle_mask(resolve_value(inst.args[2], program))?;
    if value_1.is_immediate() || value_2.is_immediate() {
        return None;
    }

    let mut selected = None;
    let mut should_continue = false;
    if let Some(([pending_1, pending_2], mask_b)) = resolve_pending_gradient(program, value_1) {
        should_continue = if let Some(coordinate) = check_through_shuffle(program, value_2) {
            selected = Some(([coordinate, pending_1, pending_2], mask_b, 0usize));
            true
        } else {
            false
        };
    }
    if let Some(([pending_1, pending_2], mask_b)) = resolve_pending_gradient(program, value_2) {
        should_continue = if let Some(coordinate) = check_through_shuffle(program, value_1) {
            selected = Some(([coordinate, pending_1, pending_2], mask_b, 2usize));
            true
        } else {
            false
        };
    }
    if !should_continue {
        return None;
    }
    let (temporary, mask_b, coordinate_index) = selected?;
    let mut zero_mask_a = 0usize;
    let mut zero_mask_b = 0usize;
    for index in 0..4 {
        if mask_a[index] == 2 || mask_b[index] == 2 {
            return None;
        }
        zero_mask_a |= usize::from(mask_a[index] == 3) << index;
        zero_mask_b |= usize::from(mask_b[index] == 3) << index;
    }
    const DDX_PATTERN: usize = 0b1010;
    const DDX_PATTERN_INV: usize = !DDX_PATTERN & 0b1111;
    if zero_mask_a.count_ones() != 2 || zero_mask_b.count_ones() != 2 || zero_mask_a == zero_mask_b
    {
        return None;
    }
    let is_ddx = |mask: usize| mask == DDX_PATTERN || mask == DDX_PATTERN_INV;
    let mut results = [Value::Void; 3];
    results[0] = temporary[coordinate_index];
    if coordinate_index == 0 {
        if is_ddx(zero_mask_b) {
            results[1] = temporary[1];
            results[2] = temporary[2];
        } else {
            results[2] = temporary[1];
            results[1] = temporary[2];
        }
    } else {
        if is_ddx(zero_mask_b) {
            results[1] = temporary[1];
        } else {
            results[2] = temporary[1];
        }
        if is_ddx(zero_mask_a) {
            results[1] = temporary[0];
        } else {
            results[2] = temporary[0];
        }
    }
    Some(results)
}

fn insert_before(program: &mut Program, before: InstRef, inst: Inst) -> Value {
    let slot = program
        .block_mut(before.block)
        .insert_inst_before(before.inst, inst);
    Value::Inst(InstRef {
        block: before.block,
        inst: slot,
    })
}

/// Port of upstream `ConvertDerivatives`.
fn convert_derivatives(program: &mut Program, before: InstRef, results: &mut [Value; 3]) {
    for result in results {
        if resolved_type(*result, program) != Type::U32 {
            continue;
        }
        *result = match resolve_value(*result, program) {
            Value::ImmU32(value) => Value::ImmF32(f32::from_bits(value)),
            value => insert_before(
                program,
                before,
                Inst::new(Opcode::BitCastF32U32, vec![value]),
            ),
        };
    }
}

/// Port of upstream `FoldImageSampleImplicitLod`.
fn fold_image_sample_implicit_lod(program: &mut Program, inst_ref: InstRef) {
    let inst = program.block(inst_ref.block).inst(inst_ref.inst).clone();
    let mut info = TextureInstInfo::from_u32(inst.flags);
    if !info.ndv_is_active
        || ShaderTextureType::from_u8(info.texture_type) != ShaderTextureType::Color3D
    {
        return;
    }
    let [handle, coords, bias_lc, offset, ..] = inst.args.as_slice() else {
        return;
    };
    if !offset.is_immediate() {
        return;
    }
    let Some(coords_ref) = inst_recursive(*coords, program) else {
        return;
    };
    let coords_inst = program
        .block(coords_ref.block)
        .inst(coords_ref.inst)
        .clone();
    if coords_inst.args.len() < 3 {
        return;
    }
    let mut results_matrix = [[Value::Void; 3]; 3];
    for (index, results) in results_matrix.iter_mut().enumerate() {
        let Some(found) =
            find_gradient_3d_derivatives(program, resolve_value(coords_inst.args[index], program))
        else {
            return;
        };
        *results = found;
        convert_derivatives(program, inst_ref, results);
    }
    let lod_clamp = if info.has_lod_clamp {
        if bias_lc.is_immediate() {
            *bias_lc
        } else {
            let Some(bias_ref) = inst_recursive(*bias_lc, program) else {
                return;
            };
            let bias = program.block(bias_ref.block).inst(bias_ref.inst);
            bias.args
                .get(1)
                .copied()
                .map(|value| resolve_value(value, program))
                .unwrap_or(Value::Void)
        }
    } else {
        Value::Void
    };
    let new_coords = insert_before(
        program,
        inst_ref,
        Inst::new(
            Opcode::CompositeConstructF32x3,
            vec![
                results_matrix[0][0],
                results_matrix[1][0],
                results_matrix[2][0],
            ],
        ),
    );
    let derivatives_1 = insert_before(
        program,
        inst_ref,
        Inst::new(
            Opcode::CompositeConstructF32x4,
            vec![
                results_matrix[0][1],
                results_matrix[0][2],
                results_matrix[1][1],
                results_matrix[1][2],
            ],
        ),
    );
    let derivatives_2 = insert_before(
        program,
        inst_ref,
        Inst::new(
            Opcode::CompositeConstructF32x2,
            vec![results_matrix[2][1], results_matrix[2][2]],
        ),
    );
    info.num_derivatives = 3;
    let mut opcode = if handle.is_immediate() {
        Opcode::BoundImageGradient
    } else {
        Opcode::BindlessImageGradient
    };
    if inst.opcode == Opcode::ImageSampleImplicitLod {
        opcode = Opcode::ImageGradient;
    }
    let gradient = insert_before(
        program,
        inst_ref,
        Inst::with_flags(
            opcode,
            vec![*handle, new_coords, derivatives_1, derivatives_2, lod_clamp],
            info.to_u32(),
        ),
    );
    replace_with_identity(program, inst_ref, gradient);
}

fn fold_bitcast(program: &mut Program, inst_ref: InstRef, reverse: Opcode) {
    let opcode = program.block(inst_ref.block).inst(inst_ref.inst).opcode;
    let arg = match program
        .block(inst_ref.block)
        .inst(inst_ref.inst)
        .args
        .first()
    {
        Some(value) => *value,
        None => return,
    };
    let Some(arg_ref) = inst_recursive(arg, program) else {
        return;
    };
    let arg_inst = program.block(arg_ref.block).inst(arg_ref.inst);
    let arg_opcode = arg_inst.opcode;
    let arg_args = arg_inst.args.clone();
    if arg_opcode == reverse {
        let Some(value) = arg_args.first().copied() else {
            return;
        };
        replace_with_identity(program, inst_ref, value);
        return;
    }
    if opcode == Opcode::BitCastF32U32 && arg_opcode == Opcode::GetCbufU32 {
        let [binding, offset, ..] = arg_args.as_slice() else {
            return;
        };
        let inst = program.block_mut(inst_ref.block).inst_mut(inst_ref.inst);
        inst.opcode = Opcode::GetCbufF32;
        inst.args = vec![*binding, *offset];
    }
}

/// Port of upstream `FoldDerivativeYFromCorrection`.
fn fold_derivative_y_from_correction(
    program: &mut Program,
    lhs_value: Value,
    rhs_value: Value,
) -> Option<Value> {
    let lhs_ref = inst_recursive(lhs_value, program)?;
    let rhs_ref = inst_recursive(rhs_value, program)?;
    let lhs_opcode = program.block(lhs_ref.block).inst(lhs_ref.inst).opcode;
    let rhs_opcode = program.block(rhs_ref.block).inst(rhs_ref.inst).opcode;
    if lhs_opcode == Opcode::YDirection && rhs_opcode == Opcode::DPdyFine {
        Some(rhs_value)
    } else if rhs_opcode == Opcode::YDirection && lhs_opcode == Opcode::DPdyFine {
        Some(lhs_value)
    } else {
        None
    }
}

/// Port of upstream `FoldFPMul32`.
fn fold_fp_mul32(
    program: &mut Program,
    inst_ref: InstRef,
    preserves_depth_interpolation: bool,
) {
    let inst = program.block(inst_ref.block).inst(inst_ref.inst).clone();
    if let (Some(Value::ImmF32(lhs)), Some(Value::ImmF32(rhs))) =
        (inst.args.first().copied(), inst.args.get(1).copied())
    {
        if inst.associated.is_none() {
            replace_with_identity(program, inst_ref, Value::ImmF32(lhs * rhs));
        }
        return;
    }
    if crate::ir::types::FpControl::from_u32(inst.flags).no_contraction {
        return;
    }
    let (Some(lhs_value), Some(rhs_value)) =
        (inst.args.first().copied(), inst.args.get(1).copied())
    else {
        return;
    };
    if lhs_value.is_immediate() || rhs_value.is_immediate() {
        return;
    }
    if let Some(replacement) = fold_derivative_y_from_correction(program, lhs_value, rhs_value) {
        replace_with_identity(program, inst_ref, replacement);
        return;
    }
    // Intentional correction to upstream FoldFPMul32: (a*w)*rcp(w) is not
    // bit-equivalent to a. Folding only one of two depth-writing shaders (the
    // other may hide rcp(w) behind a Phi) breaks equal-depth testing: a depth
    // prepass and a G-buffer pass then differ by one depth ULP and drop
    // fragment scanlines. Preserve the guest arithmetic for depth-exporting shaders;
    // do not remove their depth offsets or weaken the depth comparison.
    if preserves_depth_interpolation {
        return;
    }
    let (Some(lhs_ref), Some(rhs_ref)) = (
        inst_recursive(lhs_value, program),
        inst_recursive(rhs_value, program),
    ) else {
        return;
    };
    let lhs_op = program.block(lhs_ref.block).inst(lhs_ref.inst);
    let rhs_op = program.block(rhs_ref.block).inst(rhs_ref.inst);
    if lhs_op.opcode != Opcode::FPMul32 || rhs_op.opcode != Opcode::FPRecip32 {
        return;
    }
    let Some(recip_source) = rhs_op.args.first().copied() else {
        return;
    };
    let Some(lhs_mul_source) = lhs_op.args.get(1).copied() else {
        return;
    };
    let lhs_mul_source = resolve_value(lhs_mul_source, program);
    if recip_source.is_immediate() || lhs_mul_source.is_immediate() {
        return;
    }
    let (Some(attr_a_ref), Some(attr_b_ref)) = (
        inst_recursive(recip_source, program),
        inst_recursive(lhs_mul_source, program),
    ) else {
        return;
    };
    let attr_a = program.block(attr_a_ref.block).inst(attr_a_ref.inst);
    let attr_b = program.block(attr_b_ref.block).inst(attr_b_ref.inst);
    if attr_a.opcode != Opcode::GetAttribute || attr_b.opcode != Opcode::GetAttribute {
        return;
    }
    if attr_a.args.first() != attr_b.args.first() {
        return;
    }
    let Some(replacement) = lhs_op.args.first().copied() else {
        return;
    };
    replace_with_identity(program, inst_ref, replacement);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend;
    use crate::environment::Environment;
    use crate::ir::basic_block::Block;
    use crate::ir::emitter::Emitter;
    use crate::ir::program::SyntaxNode;
    use crate::ir::types::ShaderStage;
    use crate::ir::value::Attribute;
    use crate::program_header::ProgramHeader;
    use crate::runtime_info::RuntimeInfo;
    use crate::shader_info::{ReplaceConstant, TexturePixelFormat, TextureType};
    use crate::Profile;

    fn inst(block: u32, inst: u32) -> Value {
        Value::Inst(InstRef { block, inst })
    }

    #[test]
    fn immediate_parent_with_pseudo_is_not_folded() {
        let mut program = Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        let (extract_ref, zero_ref) = {
            let mut ir = Emitter::new(&mut program, 0);
            let extract =
                ir.bit_field_u_extract(Value::ImmU32(1), Value::ImmU32(0), Value::ImmU32(1));
            let zero = ir.get_zero_from_op(extract);
            ir.select_u1(zero, Value::ImmU1(false), Value::ImmU1(true));
            (extract.inst_ref(), zero.inst_ref())
        };
        program.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];

        constant_propagation_pass(&mut program);

        let extract = program.block(0).inst(extract_ref.inst);
        assert_eq!(extract.opcode, Opcode::BitFieldUExtract);
        assert_eq!(
            extract.get_associated_pseudo(Opcode::GetZeroFromOp),
            Some(zero_ref)
        );
        assert_eq!(
            program.block(0).inst(zero_ref.inst).args,
            vec![Value::Inst(extract_ref)]
        );

        let spirv = backend::emit_spirv(&program, &Profile::default(), &RuntimeInfo::default());
        assert!(!spirv.is_empty());
    }

    #[test]
    fn bitcast_inverse_fold_matches_upstream() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        let block = program.block_mut(0);
        block.append_inst(Inst::new(
            Opcode::GetAttribute,
            vec![Value::Attribute(Attribute::generic(1, 0)), Value::ImmU32(0)],
        ));
        block.append_inst(Inst::new(Opcode::BitCastU32F32, vec![inst(0, 0)]));
        block.append_inst(Inst::new(Opcode::BitCastF32U32, vec![inst(0, 1)]));

        constant_propagation_pass(&mut program);

        let folded = program.block(0).inst(2);
        assert_eq!(folded.opcode, Opcode::Identity);
        assert_eq!(folded.args, vec![inst(0, 0)]);
    }

    #[test]
    fn packed_half_insert_extract_eliminates_overwritten_cbuf_lane() {
        let mut program = Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        let block = program.block_mut(0);
        block.append_inst(Inst::new(
            Opcode::GetCbufU32,
            vec![Value::ImmU32(3), Value::ImmU32(0xb4)],
        ));
        block.append_inst(Inst::new(Opcode::UnpackFloat2x16, vec![inst(0, 0)]));
        block.append_inst(Inst::new(Opcode::ConvertF32F16, vec![Value::ImmF32(0.75)]));
        block.append_inst(Inst::new(
            Opcode::CompositeInsertF16x2,
            vec![inst(0, 1), inst(0, 2), Value::ImmU32(0)],
        ));
        block.append_inst(Inst::new(Opcode::PackFloat2x16, vec![inst(0, 3)]));
        block.append_inst(Inst::new(Opcode::UnpackFloat2x16, vec![inst(0, 4)]));
        block.append_inst(Inst::new(
            Opcode::CompositeExtractF16x2,
            vec![inst(0, 5), Value::ImmU32(0)],
        ));

        constant_propagation_pass(&mut program);

        let unpack = program.block(0).inst(5);
        assert_eq!(unpack.opcode, Opcode::Identity);
        assert_eq!(unpack.args, vec![inst(0, 3)]);
        let extract = program.block(0).inst(6);
        assert_eq!(extract.opcode, Opcode::Identity);
        assert_eq!(extract.args, vec![inst(0, 2)]);

        crate::ir_opt::dead_code_elimination_pass::dead_code_elimination_pass(&mut program);
        assert!(!program
            .block(0)
            .iter()
            .any(|inst| inst.opcode == Opcode::GetCbufU32));
    }

    #[test]
    fn bitcast_cbuf_u32_becomes_typed_cbuf_f32_like_upstream() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        let block = program.block_mut(0);
        block.append_inst(Inst::new(
            Opcode::GetCbufU32,
            vec![Value::ImmU32(3), Value::ImmU32(0x1a0)],
        ));
        block.append_inst(Inst::new(Opcode::BitCastF32U32, vec![inst(0, 0)]));

        constant_propagation_pass(&mut program);

        let folded = program.block(0).inst(1);
        assert_eq!(folded.opcode, Opcode::GetCbufF32);
        assert_eq!(folded.args, vec![Value::ImmU32(3), Value::ImmU32(0x1a0)]);
    }

    #[test]
    fn signed_bitfield_extract_folds_texture_offsets_like_upstream() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        let block = program.block_mut(0);
        block.append_inst(Inst::new(
            Opcode::BitFieldSExtract,
            vec![
                Value::ImmU32(0x0000_00f1),
                Value::ImmU32(0),
                Value::ImmU32(4),
            ],
        ));
        block.append_inst(Inst::new(
            Opcode::BitFieldSExtract,
            vec![
                Value::ImmU32(0x0000_00f1),
                Value::ImmU32(4),
                Value::ImmU32(4),
            ],
        ));

        constant_propagation_pass(&mut program);

        assert_eq!(program.block(0).inst(0).args, vec![Value::ImmU32(1)]);
        assert_eq!(program.block(0).inst(1).args, vec![Value::ImmU32(u32::MAX)]);
    }

    #[test]
    fn bitfield_insert_masks_bits_outside_the_inserted_field() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        program.block_mut(0).append_inst(Inst::new(
            Opcode::BitFieldInsert,
            vec![
                Value::ImmU32(0x1234_5670),
                Value::ImmU32(0xff),
                Value::ImmU32(0),
                Value::ImmU32(4),
            ],
        ));

        constant_propagation_pass(&mut program);

        let folded = program.block(0).inst(0);
        assert_eq!(folded.opcode, Opcode::Identity);
        assert_eq!(folded.args, vec![Value::ImmU32(0x1234_567f)]);
    }

    #[test]
    fn fp_add_folds_positive_and_negative_zero_like_upstream() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        let block = program.block_mut(0);
        block.append_inst(Inst::new(
            Opcode::GetAttribute,
            vec![Value::Attribute(Attribute::generic(1, 0)), Value::ImmU32(0)],
        ));
        block.append_inst(Inst::new(
            Opcode::FPAdd32,
            vec![inst(0, 0), Value::ImmF32(-0.0)],
        ));
        block.append_inst(Inst::new(
            Opcode::FPAdd32,
            vec![Value::ImmF32(0.0), inst(0, 0)],
        ));

        constant_propagation_pass(&mut program);

        for index in [1, 2] {
            let folded = program.block(0).inst(index);
            assert_eq!(folded.opcode, Opcode::Identity);
            assert_eq!(folded.args, vec![inst(0, 0)]);
        }
    }

    #[test]
    fn fp_mul_by_zero_is_not_folded_like_upstream() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        let block = program.block_mut(0);
        block.append_inst(Inst::new(
            Opcode::GetAttribute,
            vec![Value::Attribute(Attribute::generic(1, 0)), Value::ImmU32(0)],
        ));
        block.append_inst(Inst::new(
            Opcode::FPMul32,
            vec![inst(0, 0), Value::ImmF32(0.0)],
        ));

        constant_propagation_pass(&mut program);

        assert_eq!(program.block(0).inst(1).opcode, Opcode::FPMul32);
    }

    #[test]
    fn no_contraction_prevents_derivative_correction_fold() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        let block = program.block_mut(0);
        block.append_inst(Inst::new(Opcode::YDirection, vec![]));
        block.append_inst(Inst::new(Opcode::DPdyFine, vec![Value::ImmF32(1.0)]));
        block.append_inst(Inst::with_flags(
            Opcode::FPMul32,
            vec![inst(0, 0), inst(0, 1)],
            crate::ir::types::FpControl {
                no_contraction: true,
                ..Default::default()
            }
            .to_u32(),
        ));

        constant_propagation_pass(&mut program);

        assert_eq!(program.block(0).inst(2).opcode, Opcode::FPMul32);
    }

    #[test]
    fn derivative_y_correction_folds_like_upstream() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        let block = program.block_mut(0);
        block.append_inst(Inst::new(Opcode::YDirection, vec![]));
        block.append_inst(Inst::new(Opcode::DPdyFine, vec![Value::ImmF32(1.0)]));
        block.append_inst(Inst::new(Opcode::FPMul32, vec![inst(0, 0), inst(0, 1)]));

        constant_propagation_pass(&mut program);

        assert_eq!(program.block(0).inst(2).opcode, Opcode::Identity);
        assert_eq!(program.block(0).inst(2).args, vec![inst(0, 1)]);
    }

    #[test]
    fn missing_integer_comparisons_and_iadd64_fold_like_upstream() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        let block = program.block_mut(0);
        for opcode in [
            Opcode::SLessThanEqual,
            Opcode::ULessThanEqual,
            Opcode::SGreaterThan,
            Opcode::UGreaterThan,
            Opcode::SGreaterThanEqual,
            Opcode::UGreaterThanEqual,
        ] {
            block.append_inst(Inst::new(opcode, vec![Value::ImmU32(2), Value::ImmU32(1)]));
        }
        block.append_inst(Inst::new(
            Opcode::IAdd64,
            vec![Value::ImmU64(u64::MAX), Value::ImmU64(2)],
        ));

        constant_propagation_pass(&mut program);

        let expected = [false, false, true, true, true, true];
        for (index, expected) in expected.into_iter().enumerate() {
            assert_eq!(
                program.block(0).inst(index as u32).args,
                vec![Value::ImmU1(expected)]
            );
        }
        assert_eq!(program.block(0).inst(6).args, vec![Value::ImmU64(1)]);
    }

    #[test]
    fn commutative_add_normalizes_and_reassociates_immediates() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        let block = program.block_mut(0);
        block.append_inst(Inst::new(Opcode::UndefU32, vec![]));
        block.append_inst(Inst::new(
            Opcode::IAdd32,
            vec![inst(0, 0), Value::ImmU32(3)],
        ));
        block.append_inst(Inst::new(
            Opcode::IAdd32,
            vec![Value::ImmU32(4), inst(0, 1)],
        ));

        constant_propagation_pass(&mut program);

        let folded = program.block(0).inst(2);
        assert_eq!(folded.opcode, Opcode::IAdd32);
        assert_eq!(folded.args, vec![inst(0, 0), Value::ImmU32(7)]);
    }

    #[test]
    fn isub_cbuf_patterns_fold_like_upstream() {
        let mut program = Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        let block = program.block_mut(0);
        block.append_inst(Inst::new(
            Opcode::GetCbufU32,
            vec![Value::ImmU32(2), Value::ImmU32(0x40)],
        ));
        block.append_inst(Inst::new(
            Opcode::GetCbufU32,
            vec![Value::ImmU32(2), Value::ImmU32(0x40)],
        ));
        block.append_inst(Inst::new(Opcode::ISub32, vec![inst(0, 0), inst(0, 1)]));
        block.append_inst(Inst::new(Opcode::UndefU32, vec![]));
        block.append_inst(Inst::new(Opcode::IAdd32, vec![inst(0, 3), inst(0, 0)]));
        block.append_inst(Inst::new(Opcode::ISub32, vec![inst(0, 4), inst(0, 1)]));

        constant_propagation_pass(&mut program);

        assert_eq!(program.block(0).inst(2).args, vec![Value::ImmU32(0)]);
        assert_eq!(program.block(0).inst(5).args, vec![inst(0, 3)]);
    }

    #[test]
    fn implicit_lod_color_3d_ndv_pattern_becomes_three_derivative_gradient() {
        fn append_shuffle_value(block: &mut Block, source: Value) -> Value {
            let source_u32 = block.append_inst(Inst::new(Opcode::BitCastU32F32, vec![source]));
            let shuffle = block.append_inst(Inst::new(
                Opcode::ShuffleIndex,
                vec![
                    inst(0, source_u32),
                    Value::ImmU32(3),
                    Value::ImmU32(3),
                    Value::ImmU32(28),
                ],
            ));
            let as_f32 =
                block.append_inst(Inst::new(Opcode::BitCastF32U32, vec![inst(0, shuffle)]));
            inst(0, as_f32)
        }

        fn append_gradient_coord(block: &mut Block, attribute: u32) -> Value {
            let sources: [Value; 3] = std::array::from_fn(|component| {
                let slot = block.append_inst(Inst::new(
                    Opcode::GetAttribute,
                    vec![
                        Value::Attribute(Attribute::generic(attribute, component as u32)),
                        Value::ImmU32(0),
                    ],
                ));
                inst(0, slot)
            });
            let ddx = append_shuffle_value(block, sources[1]);
            let ddy = append_shuffle_value(block, sources[2]);
            let pending = block.append_inst(Inst::new(
                Opcode::FSwizzleAdd,
                vec![ddx, ddy, Value::ImmU32(0xcc)],
            ));
            let coordinate = append_shuffle_value(block, sources[0]);
            let outer = block.append_inst(Inst::new(
                Opcode::FSwizzleAdd,
                vec![inst(0, pending), coordinate, Value::ImmU32(0x33)],
            ));
            inst(0, outer)
        }

        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        let block = program.block_mut(0);
        let coordinates: [Value; 3] =
            std::array::from_fn(|attribute| append_gradient_coord(block, attribute as u32));
        let coords = block.append_inst(Inst::new(
            Opcode::CompositeConstructF32x3,
            coordinates.to_vec(),
        ));
        let clamp_source = block.append_inst(Inst::new(
            Opcode::GetAttribute,
            vec![Value::Attribute(Attribute::generic(4, 0)), Value::ImmU32(0)],
        ));
        let clamp_identity =
            block.append_inst(Inst::new(Opcode::Identity, vec![inst(0, clamp_source)]));
        let bias_clamp = block.append_inst(Inst::new(
            Opcode::CompositeConstructF32x2,
            vec![Value::ImmF32(0.0), inst(0, clamp_identity)],
        ));
        let info = TextureInstInfo {
            texture_type: ShaderTextureType::Color3D as u8,
            ndv_is_active: true,
            has_lod_clamp: true,
            ..Default::default()
        };
        let sample = block.append_inst(Inst::with_flags(
            Opcode::ImageSampleImplicitLod,
            vec![
                Value::ImmU32(2),
                inst(0, coords),
                inst(0, bias_clamp),
                Value::ImmU32(0),
            ],
            info.to_u32(),
        ));

        constant_propagation_pass(&mut program);

        let folded = program.block(0).inst(sample);
        assert_eq!(folded.opcode, Opcode::Identity);
        let Value::Inst(gradient_ref) = folded.args[0] else {
            panic!("implicit sample must resolve to ImageGradient");
        };
        let gradient = program.block(0).inst(gradient_ref.inst);
        assert_eq!(gradient.opcode, Opcode::ImageGradient);
        assert_eq!(TextureInstInfo::from_u32(gradient.flags).num_derivatives, 3);
        assert_eq!(gradient.args.len(), 5);
        assert_eq!(gradient.args[4], inst(0, clamp_source));
    }

    #[test]
    fn implicit_lod_uses_handle_immediacy_to_select_bound_gradient() {
        fn append_shuffle_value(block: &mut Block, source: Value) -> Value {
            let source_u32 = block.append_inst(Inst::new(Opcode::BitCastU32F32, vec![source]));
            let shuffle = block.append_inst(Inst::new(
                Opcode::ShuffleIndex,
                vec![
                    inst(0, source_u32),
                    Value::ImmU32(3),
                    Value::ImmU32(3),
                    Value::ImmU32(28),
                ],
            ));
            let as_f32 =
                block.append_inst(Inst::new(Opcode::BitCastF32U32, vec![inst(0, shuffle)]));
            inst(0, as_f32)
        }

        fn append_gradient_coord(block: &mut Block, attribute: u32) -> Value {
            let sources: [Value; 3] = std::array::from_fn(|component| {
                let slot = block.append_inst(Inst::new(
                    Opcode::GetAttribute,
                    vec![
                        Value::Attribute(Attribute::generic(attribute, component as u32)),
                        Value::ImmU32(0),
                    ],
                ));
                inst(0, slot)
            });
            let ddx = append_shuffle_value(block, sources[1]);
            let ddy = append_shuffle_value(block, sources[2]);
            let pending = block.append_inst(Inst::new(
                Opcode::FSwizzleAdd,
                vec![ddx, ddy, Value::ImmU32(0xcc)],
            ));
            let coordinate = append_shuffle_value(block, sources[0]);
            let outer = block.append_inst(Inst::new(
                Opcode::FSwizzleAdd,
                vec![inst(0, pending), coordinate, Value::ImmU32(0x33)],
            ));
            inst(0, outer)
        }

        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        let block = program.block_mut(0);
        let coordinates: [Value; 3] =
            std::array::from_fn(|attribute| append_gradient_coord(block, attribute as u32));
        let coords = block.append_inst(Inst::new(
            Opcode::CompositeConstructF32x3,
            coordinates.to_vec(),
        ));
        let info = TextureInstInfo {
            texture_type: ShaderTextureType::Color3D as u8,
            ndv_is_active: true,
            ..Default::default()
        };
        let sample = block.append_inst(Inst::with_flags(
            Opcode::BindlessImageSampleImplicitLod,
            vec![
                Value::ImmU32(2),
                inst(0, coords),
                Value::ImmF32(0.0),
                Value::ImmU32(0),
            ],
            info.to_u32(),
        ));

        constant_propagation_pass(&mut program);

        let Value::Inst(gradient_ref) = program.block(0).inst(sample).args[0] else {
            panic!("implicit sample must resolve to a gradient");
        };
        assert_eq!(
            program.block(0).inst(gradient_ref.inst).opcode,
            Opcode::BoundImageGradient
        );
    }

    #[test]
    fn folded_constant_is_immediately_visible_to_later_instructions() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        let block = program.block_mut(0);
        block.append_inst(Inst::new(
            Opcode::GetAttribute,
            vec![Value::Attribute(Attribute::generic(1, 0)), Value::ImmU32(0)],
        ));
        block.append_inst(Inst::new(Opcode::FPNeg32, vec![Value::ImmF32(0.0)]));
        block.append_inst(Inst::new(Opcode::FPAdd32, vec![inst(0, 0), inst(0, 1)]));

        constant_propagation_pass(&mut program);

        let folded = program.block(0).inst(2);
        assert_eq!(folded.opcode, Opcode::Identity);
        assert_eq!(folded.args, vec![inst(0, 0)]);
    }

    #[test]
    fn integer_add_folds_zero_like_upstream() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        let block = program.block_mut(0);
        block.append_inst(Inst::new(
            Opcode::GetAttributeU32,
            vec![Value::Attribute(Attribute::generic(1, 0)), Value::ImmU32(0)],
        ));
        block.append_inst(Inst::new(
            Opcode::IAdd32,
            vec![inst(0, 0), Value::ImmU32(0)],
        ));

        constant_propagation_pass(&mut program);

        let folded = program.block(0).inst(1);
        assert_eq!(folded.opcode, Opcode::Identity);
        assert_eq!(folded.args, vec![inst(0, 0)]);
    }

    #[test]
    fn xmad_multiply_pattern_folds_to_direct_imul_like_upstream() {
        let mut program = Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        let block = program.block_mut(0);
        block.append_inst(Inst::new(
            Opcode::GetAttributeU32,
            vec![Value::Attribute(Attribute::generic(0, 0)), Value::ImmU32(0)],
        ));
        block.append_inst(Inst::new(
            Opcode::GetAttributeU32,
            vec![Value::Attribute(Attribute::generic(1, 0)), Value::ImmU32(0)],
        ));
        block.append_inst(Inst::new(
            Opcode::BitFieldUExtract,
            vec![inst(0, 0), Value::ImmU32(0), Value::ImmU32(16)],
        ));
        block.append_inst(Inst::new(Opcode::IMul32, vec![inst(0, 2), inst(0, 1)]));
        block.append_inst(Inst::new(
            Opcode::BitFieldUExtract,
            vec![inst(0, 0), Value::ImmU32(16), Value::ImmU32(16)],
        ));
        block.append_inst(Inst::new(Opcode::IMul32, vec![inst(0, 4), inst(0, 1)]));
        block.append_inst(Inst::new(
            Opcode::ShiftLeftLogical32,
            vec![inst(0, 5), Value::ImmU32(16)],
        ));
        block.append_inst(Inst::new(Opcode::IAdd32, vec![inst(0, 6), inst(0, 3)]));

        constant_propagation_pass(&mut program);

        let folded = program.block(0).inst(7);
        assert_eq!(folded.opcode, Opcode::Identity);
        let Value::Inst(direct_mul) = folded.args[0] else {
            panic!("XMAD multiply must resolve to a direct IMul32");
        };
        let direct_mul = program.block(0).inst(direct_mul.inst);
        assert_eq!(direct_mul.opcode, Opcode::IMul32);
        assert_eq!(direct_mul.args, vec![inst(0, 0), inst(0, 1)]);
    }

    #[test]
    fn logical_neutral_and_double_not_folds_match_upstream() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        let block = program.block_mut(0);
        block.append_inst(Inst::new(Opcode::UndefU1, vec![]));
        block.append_inst(Inst::new(
            Opcode::LogicalAnd,
            vec![inst(0, 0), Value::ImmU1(true)],
        ));
        block.append_inst(Inst::new(
            Opcode::LogicalOr,
            vec![inst(0, 1), Value::ImmU1(false)],
        ));
        block.append_inst(Inst::new(Opcode::LogicalNot, vec![inst(0, 2)]));
        block.append_inst(Inst::new(Opcode::LogicalNot, vec![inst(0, 3)]));

        constant_propagation_pass(&mut program);

        for index in [1, 2, 4] {
            let folded = program.block(0).inst(index);
            assert_eq!(folded.opcode, Opcode::Identity);
            assert_eq!(folded.args, vec![inst(0, 0)]);
        }
    }

    #[test]
    fn interpolation_correction_fold_matches_upstream() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        let block = program.block_mut(0);
        block.append_inst(Inst::new(
            Opcode::GetAttribute,
            vec![Value::Attribute(Attribute::generic(2, 0)), Value::ImmU32(0)],
        ));
        block.append_inst(Inst::new(
            Opcode::GetAttribute,
            vec![Value::Attribute(Attribute::POSITION_W), Value::ImmU32(0)],
        ));
        block.append_inst(Inst::new(Opcode::FPMul32, vec![inst(0, 0), inst(0, 1)]));
        block.append_inst(Inst::new(Opcode::FPRecip32, vec![inst(0, 1)]));
        block.append_inst(Inst::new(Opcode::FPMul32, vec![inst(0, 2), inst(0, 3)]));

        constant_propagation_pass(&mut program);

        let folded = program.block(0).inst(4);
        assert_eq!(folded.opcode, Opcode::Identity);
        assert_eq!(folded.args, vec![inst(0, 0)]);
    }

    #[test]
    fn depth_export_preserves_interpolation_rounding() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        let block = program.block_mut(0);
        block.append_inst(Inst::new(
            Opcode::GetAttribute,
            vec![Value::Attribute(Attribute::generic(1, 2)), Value::ImmU32(0)],
        ));
        block.append_inst(Inst::new(
            Opcode::GetAttribute,
            vec![Value::Attribute(Attribute::POSITION_W), Value::ImmU32(0)],
        ));
        block.append_inst(Inst::new(Opcode::FPMul32, vec![inst(0, 0), inst(0, 1)]));
        block.append_inst(Inst::new(Opcode::FPRecip32, vec![inst(0, 1)]));
        block.append_inst(Inst::new(Opcode::FPMul32, vec![inst(0, 2), inst(0, 3)]));
        // Shader info is collected after this pass; inspect the actual export,
        // not the (still false) cached stores_frag_depth flag.
        block.append_inst(Inst::new(Opcode::SetFragDepth, vec![inst(0, 4)]));

        constant_propagation_pass(&mut program);

        assert_eq!(program.block(0).inst(4).opcode, Opcode::FPMul32);
        assert_eq!(program.block(0).inst(4).args, vec![inst(0, 2), inst(0, 3)]);
    }

    #[test]
    fn captured_pixel_depth_changes_when_interpolation_roundtrip_is_removed() {
        // Inputs captured from a single pixel of a depth prepass/G-buffer pair.
        // Prepass and G-buffer have identical inputs but the latter used to
        // fold (attribute * position_w) * reciprocal(position_w) to attribute.
        let attribute = 0.0999755859375_f32;
        let position_w = 0.0003517017175909132_f32;
        let position_z = 0.0035170172341167927_f32;
        let reciprocal_w = 1.0_f32 / position_w;
        let uncorrected = attribute * position_w;
        let corrected = uncorrected * reciprocal_w;
        let numerator = position_z * reciprocal_w;
        let prepass = numerator * (1.0_f32 / (reciprocal_w + corrected));
        let folded = numerator * (1.0_f32 / (reciprocal_w + attribute));

        assert_eq!(folded.to_bits(), prepass.to_bits() + 1);
        assert_ne!(prepass, folded, "Equal depth testing rejects the folded result");
    }

    #[test]
    fn fswizzle_add_derivative_patterns_match_upstream() {
        for (swizzle, index, derivative) in
            [(0x99, 1, Opcode::DPdxFine), (0xA5, 2, Opcode::DPdyFine)]
        {
            let mut program = Program::new(ShaderStage::Fragment);
            program.blocks.push(Block::new());
            let block = program.block_mut(0);
            block.append_inst(Inst::new(
                Opcode::GetAttribute,
                vec![Value::Attribute(Attribute::generic(1, 0)), Value::ImmU32(0)],
            ));
            block.append_inst(Inst::new(Opcode::BitCastU32F32, vec![inst(0, 0)]));
            block.append_inst(Inst::new(
                Opcode::ShuffleButterfly,
                vec![
                    inst(0, 1),
                    Value::ImmU32(index),
                    Value::ImmU32(3),
                    Value::ImmU32(28),
                ],
            ));
            block.append_inst(Inst::new(Opcode::BitCastF32U32, vec![inst(0, 2)]));
            block.append_inst(Inst::new(
                Opcode::FSwizzleAdd,
                vec![inst(0, 3), inst(0, 0), Value::ImmU32(swizzle)],
            ));

            constant_propagation_pass(&mut program);

            let derivative_value = program
                .block(0)
                .indexed_iter()
                .find_map(|(index, inst)| {
                    (inst.opcode == derivative).then_some(Value::Inst(InstRef {
                        block: 0,
                        inst: index,
                    }))
                })
                .expect("derivative instruction was not materialized");
            let folded = program.block(0).inst(4);
            assert_eq!(folded.opcode, Opcode::Identity);
            assert_eq!(folded.args, vec![derivative_value]);
        }
    }

    struct DriverEnvironment {
        texture_pass_caches: crate::environment::TexturePassCaches,
        sph: ProgramHeader,
    }

    impl Default for DriverEnvironment {
        fn default() -> Self {
            Self {
                texture_pass_caches: Default::default(),
                sph: ProgramHeader::default(),
            }
        }
    }

    impl Environment for DriverEnvironment {
        fn texture_pass_caches(&mut self) -> &mut crate::environment::TexturePassCaches {
            &mut self.texture_pass_caches
        }

        fn read_instruction(&mut self, _address: u32) -> u64 {
            0
        }

        fn read_cbuf_value(&mut self, bank: u32, offset: u32) -> u32 {
            assert_eq!(bank, 1);
            assert_eq!(offset, 0x10);
            1.5f32.to_bits()
        }

        fn read_texture_type(&mut self, _raw_handle: u32) -> TextureType {
            TextureType::Color2D
        }

        fn read_texture_pixel_format(&mut self, _raw_handle: u32) -> TexturePixelFormat {
            TexturePixelFormat::A8B8G8R8Unorm
        }

        fn is_texture_pixel_format_integer(&mut self, _raw_handle: u32) -> bool {
            false
        }

        fn read_viewport_transform_state(&mut self) -> u32 {
            0
        }

        fn texture_bound_buffer(&self) -> u32 {
            2
        }

        fn local_memory_size(&self) -> u32 {
            0
        }

        fn shared_memory_size(&self) -> u32 {
            0
        }

        fn workgroup_size(&self) -> [u32; 3] {
            [1, 1, 1]
        }

        fn has_hle_macro_state(&self) -> bool {
            false
        }

        fn get_replace_const_buffer(
            &mut self,
            _bank: u32,
            _offset: u32,
        ) -> Option<ReplaceConstant> {
            None
        }

        fn dump(&mut self, _pipeline_hash: u64, _shader_hash: u64) {}

        fn sph(&self) -> &ProgramHeader {
            &self.sph
        }

        fn gp_passthrough_mask(&self) -> &[u32; 8] {
            static MASK: [u32; 8] = [0; 8];
            &MASK
        }

        fn shader_stage(&self) -> ShaderStage {
            ShaderStage::Fragment
        }

        fn start_address(&self) -> u32 {
            0
        }

        fn is_proprietary_driver(&self) -> bool {
            true
        }
    }

    #[test]
    fn proprietary_driver_cbuf_one_is_folded_from_environment() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        let block = program.block_mut(0);
        block.append_inst(Inst::new(
            Opcode::GetCbufF32,
            vec![Value::ImmU32(1), Value::ImmU32(0x10)],
        ));
        block.append_inst(Inst::new(
            Opcode::FPMul32,
            vec![inst(0, 0), Value::ImmF32(2.0)],
        ));
        block.append_inst(Inst::new(
            Opcode::GetCbufF32,
            vec![Value::ImmU32(3), Value::ImmU32(0)],
        ));

        constant_propagation_pass_with_env(&mut DriverEnvironment::default(), &mut program);

        assert_eq!(program.block(0).inst(0).opcode, Opcode::Identity);
        assert_eq!(program.block(0).inst(0).args, vec![Value::ImmF32(1.5)]);
        assert_eq!(program.block(0).inst(1).opcode, Opcode::Identity);
        assert_eq!(program.block(0).inst(1).args, vec![Value::ImmF32(3.0)]);
        assert_eq!(program.block(0).inst(2).opcode, Opcode::GetCbufF32);
    }
}
