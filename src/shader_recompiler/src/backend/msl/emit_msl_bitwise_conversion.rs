// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::ir::instruction::Inst;
use crate::ir::types::Type;
use crate::ir::value::InstRef;

use super::msl_emit_context::MslEmitContext;
use super::MslError;

pub fn emit_bitcast(
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

pub fn emit_condition_ref(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    context.define(inst_ref, Type::U1, value, false)
}
