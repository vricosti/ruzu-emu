// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native-MSL control-flow operations.
//!
//! This owns the MSL equivalent of Eden's
//! `backend/spirv/emit_spirv_control_flow.cpp` demote operation.

use super::msl_emit_context::MslEmitContext;
use super::MslError;

pub fn emit_demote_to_helper_invocation(context: &mut MslEmitContext) -> Result<(), MslError> {
    context.emit_statement("if (!helper_invocation) {");
    context.emit_statement("    helper_invocation = true;");
    context.emit_statement("    discard_fragment();");
    context.emit_statement("}");
    Ok(())
}
