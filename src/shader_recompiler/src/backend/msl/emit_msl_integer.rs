// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::ir::instruction::Inst;
use crate::ir::opcodes::Opcode;
use crate::ir::types::Type;
use crate::ir::value::{InstRef, Value};

use super::msl_emit_context::MslEmitContext;
use super::MslError;

fn result_expression(context: &MslEmitContext, inst_ref: InstRef) -> Result<String, MslError> {
    context.value_expression(&Value::Inst(inst_ref), inst_ref, 0)
}

fn set_zero_sign_flags(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let result = result_expression(context, inst_ref)?;
    if let Some(flag) = inst.get_associated_pseudo(Opcode::GetZeroFromOp) {
        context.define(flag, Type::U1, format!("({result}) == 0u"), false)?;
    }
    if let Some(flag) = inst.get_associated_pseudo(Opcode::GetSignFromOp) {
        context.define(flag, Type::U1, format!("as_type<int>({result}) < 0"), false)?;
    }
    Ok(())
}

pub fn emit_binary(
    context: &mut MslEmitContext,
    program: &crate::ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
    operator: &'static str,
) -> Result<(), MslError> {
    context.emit_binary(program, inst_ref, inst, Type::U32, operator)
}

fn emit_binary_with_flags(
    context: &mut MslEmitContext,
    program: &crate::ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
    operator: &'static str,
) -> Result<(), MslError> {
    emit_binary(context, program, inst_ref, inst, operator)?;
    set_zero_sign_flags(context, inst_ref, inst)
}

pub fn emit_iadd_32(
    context: &mut MslEmitContext,
    _program: &crate::ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let lhs = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let rhs = context.value_expression(inst.arg(1), inst_ref, 1)?;
    context.define(inst_ref, Type::U32, format!("({lhs}) + ({rhs})"), false)?;
    let result = result_expression(context, inst_ref)?;
    set_zero_sign_flags(context, inst_ref, inst)?;
    if let Some(flag) = inst.get_associated_pseudo(Opcode::GetCarryFromOp) {
        context.define(flag, Type::U1, format!("({result}) < ({lhs})"), false)?;
    }
    if let Some(flag) = inst.get_associated_pseudo(Opcode::GetOverflowFromOp) {
        context.define(
            flag,
            Type::U1,
            format!(
                "(as_type<int>({lhs}) >= 0) ? (as_type<int>({rhs}) > as_type<int>(0x7FFFFFFFu - ({lhs}))) : (as_type<int>({rhs}) < as_type<int>(0x7FFFFFFFu - ({lhs})))"
            ),
            false,
        )?;
    }
    Ok(())
}

pub fn emit_isub_32(
    context: &mut MslEmitContext,
    program: &crate::ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    emit_binary(context, program, inst_ref, inst, "-")
}

pub fn emit_imul_32(
    context: &mut MslEmitContext,
    program: &crate::ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    emit_binary(context, program, inst_ref, inst, "*")
}

pub fn emit_udiv_32(
    context: &mut MslEmitContext,
    program: &crate::ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    emit_binary(context, program, inst_ref, inst, "/")
}

pub fn emit_sdiv_32(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let lhs = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let rhs = context.value_expression(inst.arg(1), inst_ref, 1)?;
    context.define(
        inst_ref,
        Type::U32,
        format!("as_type<uint>(as_type<int>({lhs}) / as_type<int>({rhs}))"),
        false,
    )
}

pub fn emit_ineg_32(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    context.define(inst_ref, Type::U32, format!("0u - ({value})"), false)
}

pub fn emit_iabs_32(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    context.define(
        inst_ref,
        Type::U32,
        format!("as_type<uint>(abs(as_type<int>({value})))"),
        false,
    )
}

pub fn emit_shift_right_arithmetic_32(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let shift = context.value_expression(inst.arg(1), inst_ref, 1)?;
    context.define(
        inst_ref,
        Type::U32,
        format!("as_type<uint>(as_type<int>({value}) >> ({shift}))"),
        false,
    )
}

pub fn emit_bitwise_with_flags(
    context: &mut MslEmitContext,
    program: &crate::ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
    operator: &'static str,
) -> Result<(), MslError> {
    emit_binary_with_flags(context, program, inst_ref, inst, operator)
}

pub fn emit_not_32(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    context.define(inst_ref, Type::U32, format!("~({value})"), false)
}

pub fn emit_bit_field_insert(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let base = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let insert = context.value_expression(inst.arg(1), inst_ref, 1)?;
    let offset = context.value_expression(inst.arg(2), inst_ref, 2)?;
    let count = context.value_expression(inst.arg(3), inst_ref, 3)?;
    context.define(
        inst_ref,
        Type::U32,
        format!("insert_bits({base}, {insert}, {offset}, {count})"),
        false,
    )
}

pub fn emit_bit_field_extract(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
    signed: bool,
) -> Result<(), MslError> {
    let base = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let offset = context.value_expression(inst.arg(1), inst_ref, 1)?;
    let count = context.value_expression(inst.arg(2), inst_ref, 2)?;
    let expression = if signed {
        format!("as_type<uint>(extract_bits(as_type<int>({base}), {offset}, {count}))")
    } else {
        format!("extract_bits({base}, {offset}, {count})")
    };
    context.define(inst_ref, Type::U32, expression, false)?;
    set_zero_sign_flags(context, inst_ref, inst)
}

pub fn emit_unary_intrinsic_32(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
    intrinsic: &'static str,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    context.define(inst_ref, Type::U32, format!("{intrinsic}({value})"), false)
}

pub fn emit_find_msb_32(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
    signed: bool,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let operand = if signed {
        format!("(as_type<int>({value}) < 0 ? ~({value}) : ({value}))")
    } else {
        value
    };
    context.define(inst_ref, Type::U32, format!("31u - clz({operand})"), false)
}

pub fn emit_min_max(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
    function: &'static str,
    signed: bool,
) -> Result<(), MslError> {
    let lhs = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let rhs = context.value_expression(inst.arg(1), inst_ref, 1)?;
    let expression = if signed {
        format!("as_type<uint>({function}(as_type<int>({lhs}), as_type<int>({rhs})))")
    } else {
        format!("{function}({lhs}, {rhs})")
    };
    context.define(inst_ref, Type::U32, expression, false)
}

pub fn emit_clamp(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
    signed: bool,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let min = context.value_expression(inst.arg(1), inst_ref, 1)?;
    let max = context.value_expression(inst.arg(2), inst_ref, 2)?;
    let expression = if signed {
        format!(
            "as_type<uint>(clamp(as_type<int>({value}), as_type<int>({min}), as_type<int>({max})))"
        )
    } else {
        format!("clamp({value}, {min}, {max})")
    };
    context.define(inst_ref, Type::U32, expression, false)?;
    set_zero_sign_flags(context, inst_ref, inst)
}

pub fn emit_comparison(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
    operator: &'static str,
    signed: bool,
) -> Result<(), MslError> {
    let lhs = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let rhs = context.value_expression(inst.arg(1), inst_ref, 1)?;
    let expression = if signed {
        format!("as_type<int>({lhs}) {operator} as_type<int>({rhs})")
    } else {
        format!("({lhs}) {operator} ({rhs})")
    };
    context.define(inst_ref, Type::U1, expression, false)
}
