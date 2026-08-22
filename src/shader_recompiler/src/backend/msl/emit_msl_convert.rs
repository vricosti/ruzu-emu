// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::ir::instruction::Inst;
use crate::ir::types::Type;
use crate::ir::value::InstRef;

use super::msl_emit_context::MslEmitContext;
use super::MslError;

fn value_expression(
    context: &MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<String, MslError> {
    context.value_expression(inst.arg(0), inst_ref, 0)
}

fn define_cast(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
    ty: Type,
    expression: impl FnOnce(String) -> String,
) -> Result<(), MslError> {
    let value = value_expression(context, inst_ref, inst)?;
    context.define(inst_ref, ty, expression(value), false)
}

pub fn emit_convert_s32_f32(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    context.define(
        inst_ref,
        Type::U32,
        format!("as_type<uint>(int({value}))"),
        false,
    )
}

pub fn emit_convert_s16_f16(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    define_cast(context, inst_ref, inst, Type::U32, |value| {
        format!("as_type<uint>(int(short({value})))")
    })
}

pub fn emit_convert_s32_f16(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    define_cast(context, inst_ref, inst, Type::U32, |value| {
        format!("as_type<uint>(int({value}))")
    })
}

pub fn emit_convert_s64_float(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    define_cast(context, inst_ref, inst, Type::U64, |value| {
        format!("as_type<ulong>(long({value}))")
    })
}

pub fn emit_convert_u32_f32(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    context.define(inst_ref, Type::U32, format!("uint({value})"), false)
}

pub fn emit_convert_u16_f16(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    define_cast(context, inst_ref, inst, Type::U32, |value| {
        format!("uint(ushort({value}))")
    })
}

pub fn emit_convert_u32_f16(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    define_cast(context, inst_ref, inst, Type::U32, |value| {
        format!("uint({value})")
    })
}

pub fn emit_convert_u64_float(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    define_cast(context, inst_ref, inst, Type::U64, |value| {
        format!("ulong({value})")
    })
}

pub fn emit_convert_u64_u32(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    define_cast(context, inst_ref, inst, Type::U64, |value| {
        format!("ulong({value})")
    })
}

pub fn emit_convert_u32_u64(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    define_cast(context, inst_ref, inst, Type::U32, |value| {
        format!("uint({value})")
    })
}

pub fn emit_convert_f32_s32(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    context.define(
        inst_ref,
        Type::F32,
        format!("float(as_type<int>({value}))"),
        false,
    )
}

pub fn emit_convert_f32_u32(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    context.define(inst_ref, Type::F32, format!("float({value})"), false)
}

pub fn emit_convert_f16_f32(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    define_cast(context, inst_ref, inst, Type::F16, |value| {
        let converted = format!("half({value})");
        format!("isnan({converted}) ? as_type<half>(ushort(0u)) : {converted}")
    })
}

pub fn emit_convert_f32_f16(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    define_cast(context, inst_ref, inst, Type::F32, |value| {
        format!("float({value})")
    })
}

pub fn emit_convert_f16_signed(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
    bits: u32,
) -> Result<(), MslError> {
    define_cast(context, inst_ref, inst, Type::F16, |value| {
        let signed = match bits {
            8 => format!("(as_type<int>((({value}) & 0xFFu) << 24u) >> 24)"),
            16 => format!("(as_type<int>((({value}) & 0xFFFFu) << 16u) >> 16)"),
            32 => format!("as_type<int>({value})"),
            64 => format!("as_type<long>({value})"),
            _ => unreachable!("IR integer conversion width"),
        };
        format!("half({signed})")
    })
}

pub fn emit_convert_f16_unsigned(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
    bits: u32,
) -> Result<(), MslError> {
    define_cast(context, inst_ref, inst, Type::F16, |value| {
        let unsigned = match bits {
            8 => format!("({value}) & 0xFFu"),
            16 => format!("({value}) & 0xFFFFu"),
            32 | 64 => value,
            _ => unreachable!("IR integer conversion width"),
        };
        format!("half({unsigned})")
    })
}

pub fn emit_convert_f32_s64(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    define_cast(context, inst_ref, inst, Type::F32, |value| {
        format!("float(as_type<long>({value}))")
    })
}

pub fn emit_convert_f32_u64(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    define_cast(context, inst_ref, inst, Type::F32, |value| {
        format!("float({value})")
    })
}
