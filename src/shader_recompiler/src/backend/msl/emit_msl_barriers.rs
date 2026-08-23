// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native-MSL barrier emission.
//!
//! `Barrier` is both an execution and workgroup-memory barrier in the IR,
//! matching Metal's `threadgroup_barrier(mem_flags::mem_threadgroup)`.

use super::msl_emit_context::MslEmitContext;
use super::MslError;

pub fn emit_barrier(context: &mut MslEmitContext) -> Result<(), MslError> {
    context.emit_statement("threadgroup_barrier(mem_flags::mem_threadgroup);");
    Ok(())
}
