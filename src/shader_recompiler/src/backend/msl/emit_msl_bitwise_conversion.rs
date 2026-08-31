// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::ir::instruction::Inst;
use crate::ir::types::Type;
use crate::ir::value::InstRef;

use super::msl_emit_context::MslEmitContext;
use super::MslError;

pub fn emit_bit_cast_u16_f16(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    context.define(
        inst_ref,
        Type::U32,
        format!("uint(as_type<ushort>({value}))"),
        false,
    )
}

fn emit_bitcast(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
    ty: Type,
    type_name: &'static str,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    context.define(
        inst_ref,
        ty,
        format!("as_type<{type_name}>({value})"),
        false,
    )
}

pub fn emit_bit_cast_u32_f32(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    emit_bitcast(context, inst_ref, inst, Type::U32, "uint")
}

pub fn emit_bit_cast_f16_u16(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    context.define(
        inst_ref,
        Type::F16,
        format!("as_type<half>(ushort({value}))"),
        false,
    )
}

pub fn emit_bit_cast_f32_u32(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    emit_bitcast(context, inst_ref, inst, Type::F32, "float")
}

pub fn emit_pack_uint2x32(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    context.define(
        inst_ref,
        Type::U64,
        format!("as_type<ulong>({value})"),
        false,
    )
}

pub fn emit_unpack_uint2x32(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    context.define(
        inst_ref,
        Type::U32x2,
        format!("as_type<uint2>({value})"),
        false,
    )
}

pub fn emit_pack_float2x16(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    context.define(
        inst_ref,
        Type::U32,
        format!("as_type<uint>({value})"),
        false,
    )
}

pub fn emit_unpack_float2x16(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    context.define(
        inst_ref,
        Type::F16x2,
        format!("as_type<half2>({value})"),
        false,
    )
}

pub fn emit_pack_half2x16(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    context.define(
        inst_ref,
        Type::U32,
        format!("as_type<uint>(half2({value}))"),
        false,
    )
}

pub fn emit_unpack_half2x16(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    context.define(
        inst_ref,
        Type::F32x2,
        format!("float2(as_type<half2>({value}))"),
        false,
    )
}

pub fn emit_condition_ref(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    context.define(inst_ref, Type::U1, value, false)
}
