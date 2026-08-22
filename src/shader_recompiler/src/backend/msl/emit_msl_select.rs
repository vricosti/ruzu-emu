// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::ir::instruction::Inst;
use crate::ir::types::Type;
use crate::ir::value::InstRef;

use super::msl_emit_context::MslEmitContext;
use super::MslError;

pub fn emit_select(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
    ty: Type,
) -> Result<(), MslError> {
    let condition = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let true_value = context.value_expression(inst.arg(1), inst_ref, 1)?;
    let false_value = context.value_expression(inst.arg(2), inst_ref, 2)?;
    context.define(
        inst_ref,
        ty,
        format!("({condition}) ? ({true_value}) : ({false_value})"),
        false,
    )
}
