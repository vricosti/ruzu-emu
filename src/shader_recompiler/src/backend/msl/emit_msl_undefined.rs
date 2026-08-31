// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native-MSL undefined value emission.
//!
//! MSL has no source-language equivalent of SPIR-V `OpUndef`. As in Eden's
//! textual GLSL backend, undefined scalar values are materialized as zero so
//! source-level phi assignments always have a valid expression.

use crate::ir;
use crate::ir::value::InstRef;

use super::msl_emit_context::MslEmitContext;
use super::MslError;

pub fn emit_undef(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &ir::Inst,
) -> Result<(), MslError> {
    let (ty, expression) = match inst.opcode {
        ir::Opcode::UndefU1 => (ir::Type::U1, "false"),
        ir::Opcode::UndefU8 | ir::Opcode::UndefU16 | ir::Opcode::UndefU32 => (ir::Type::U32, "0u"),
        ir::Opcode::UndefU64 => (ir::Type::U64, "0ul"),
        _ => unreachable!("non-undefined opcode {:?}", inst.opcode),
    };
    context.define(inst_ref, ty, expression.to_owned(), false)
}
