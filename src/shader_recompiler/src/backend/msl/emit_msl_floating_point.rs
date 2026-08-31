// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::ir::instruction::Inst;
use crate::ir::types::{FpControl, Type};
use crate::ir::value::InstRef;

use super::msl_emit_context::MslEmitContext;
use super::MslError;

fn emit_binary(
    context: &mut MslEmitContext,
    program: &crate::ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
    ty: Type,
    operator: &'static str,
) -> Result<(), MslError> {
    context.emit_binary_with_precision(
        program,
        inst_ref,
        inst,
        ty,
        operator,
        FpControl::from_u32(inst.flags).no_contraction,
    )
}

pub fn emit_fp_add_32(
    context: &mut MslEmitContext,
    program: &crate::ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    emit_binary(context, program, inst_ref, inst, Type::F32, "+")
}

pub fn emit_fp_mul_32(
    context: &mut MslEmitContext,
    program: &crate::ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    emit_binary(context, program, inst_ref, inst, Type::F32, "*")
}

pub fn emit_fp_fma_32(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    context.emit_fma(inst_ref, inst, Type::F32)
}

pub fn emit_binary_operator_32(
    context: &mut MslEmitContext,
    program: &crate::ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
    operator: &'static str,
) -> Result<(), MslError> {
    emit_binary(context, program, inst_ref, inst, Type::F32, operator)
}

pub fn emit_fp_add_16(
    context: &mut MslEmitContext,
    program: &crate::ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    emit_binary(context, program, inst_ref, inst, Type::F16, "+")
}

pub fn emit_fp_mul_16(
    context: &mut MslEmitContext,
    program: &crate::ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    emit_binary(context, program, inst_ref, inst, Type::F16, "*")
}

pub fn emit_fp_fma_16(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    context.emit_fma(inst_ref, inst, Type::F16)
}

pub fn emit_unary_operator_32(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
    operator: &'static str,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    context.define(inst_ref, Type::F32, format!("{operator}({value})"), false)
}

pub fn emit_unary_operator_16(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
    operator: &'static str,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    context.define(inst_ref, Type::F16, format!("{operator}({value})"), false)
}

pub fn emit_intrinsic_32(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
    intrinsic: &'static str,
) -> Result<(), MslError> {
    let arguments = (0..inst.num_args())
        .map(|arg| context.value_expression(inst.arg(arg), inst_ref, arg as u32))
        .collect::<Result<Vec<_>, _>>()?;
    context.define(
        inst_ref,
        Type::F32,
        format!("{intrinsic}({})", arguments.join(", ")),
        false,
    )
}

pub fn emit_intrinsic_16(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
    intrinsic: &'static str,
) -> Result<(), MslError> {
    let arguments = (0..inst.num_args())
        .map(|arg| context.value_expression(inst.arg(arg), inst_ref, arg as u32))
        .collect::<Result<Vec<_>, _>>()?;
    context.define(
        inst_ref,
        Type::F16,
        format!("{intrinsic}({})", arguments.join(", ")),
        false,
    )
}

pub fn emit_recip_32(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    context.define(inst_ref, Type::F32, format!("1.0f / ({value})"), false)
}

pub fn emit_ordered_comparison_32(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
    operator: &'static str,
) -> Result<(), MslError> {
    let lhs = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let rhs = context.value_expression(inst.arg(1), inst_ref, 1)?;
    let comparison = format!("({lhs}) {operator} ({rhs})");
    let expression = if operator == "!=" {
        format!("!isnan({lhs}) && !isnan({rhs}) && ({comparison})")
    } else {
        comparison
    };
    context.define(inst_ref, Type::U1, expression, false)
}

pub fn emit_ordered_comparison_16(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
    operator: &'static str,
) -> Result<(), MslError> {
    emit_ordered_comparison_32(context, inst_ref, inst, operator)
}

pub fn emit_unordered_comparison_32(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
    operator: &'static str,
) -> Result<(), MslError> {
    let lhs = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let rhs = context.value_expression(inst.arg(1), inst_ref, 1)?;
    let expression = if operator == "!=" {
        format!("({lhs}) != ({rhs})")
    } else {
        format!("isnan({lhs}) || isnan({rhs}) || (({lhs}) {operator} ({rhs}))")
    };
    context.define(inst_ref, Type::U1, expression, false)
}

pub fn emit_unordered_comparison_16(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
    operator: &'static str,
) -> Result<(), MslError> {
    emit_unordered_comparison_32(context, inst_ref, inst, operator)
}

pub fn emit_is_nan_32(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    context.define(inst_ref, Type::U1, format!("isnan({value})"), false)
}

pub fn emit_is_nan_16(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    emit_is_nan_32(context, inst_ref, inst)
}
