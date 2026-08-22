// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::ir::instruction::Inst;
use crate::ir::types::Type;
use crate::ir::value::InstRef;

use super::msl_emit_context::MslEmitContext;
use super::MslError;

pub fn emit_iadd_32(
    context: &mut MslEmitContext,
    program: &crate::ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    context.emit_binary(program, inst_ref, inst, Type::U32, "+")
}
