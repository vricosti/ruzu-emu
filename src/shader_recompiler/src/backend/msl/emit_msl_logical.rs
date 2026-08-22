// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::ir::instruction::Inst;
use crate::ir::types::Type;
use crate::ir::value::InstRef;

use super::msl_emit_context::MslEmitContext;
use super::MslError;

pub fn emit_binary(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
    operator: &'static str,
) -> Result<(), MslError> {
    let lhs = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let rhs = context.value_expression(inst.arg(1), inst_ref, 1)?;
    context.define(
        inst_ref,
        Type::U1,
        format!("({lhs}) {operator} ({rhs})"),
        false,
    )
}

pub fn emit_not(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let value = context.value_expression(inst.arg(0), inst_ref, 0)?;
    context.define(inst_ref, Type::U1, format!("!({value})"), false)
}
