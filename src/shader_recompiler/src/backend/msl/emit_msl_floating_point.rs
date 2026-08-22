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
    operator: &'static str,
) -> Result<(), MslError> {
    context.emit_binary_with_precision(
        program,
        inst_ref,
        inst,
        Type::F32,
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
    emit_binary(context, program, inst_ref, inst, "+")
}

pub fn emit_fp_mul_32(
    context: &mut MslEmitContext,
    program: &crate::ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    emit_binary(context, program, inst_ref, inst, "*")
}
