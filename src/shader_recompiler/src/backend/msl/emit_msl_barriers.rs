// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native-MSL barrier emission.
//!
//! `Barrier` is both an execution and workgroup-memory barrier in the IR,
//! matching Metal's `threadgroup_barrier(mem_flags::mem_threadgroup)`.

use super::msl_emit_context::MslEmitContext;
use super::{MslError, MslVersion};

const MEMORY_FLAGS: &str =
    "mem_flags::mem_device | mem_flags::mem_threadgroup | mem_flags::mem_texture";

pub fn emit_barrier(context: &mut MslEmitContext) -> Result<(), MslError> {
    context.emit_statement("threadgroup_barrier(mem_flags::mem_threadgroup);");
    Ok(())
}

fn emit_memory_barrier(
    context: &mut MslEmitContext,
    thread_scope: &'static str,
) -> Result<(), MslError> {
    if context.language_version() >= MslVersion::V3_2 {
        context.emit_statement(&format!(
            "atomic_thread_fence({MEMORY_FLAGS}, memory_order_seq_cst, {thread_scope});"
        ));
    } else {
        // Before MSL 3.2 Metal has no memory-only fence. This is the same
        // conservative control-barrier fallback used by SPIRV-Cross.
        context.emit_statement(&format!("threadgroup_barrier({MEMORY_FLAGS});"));
    }
    Ok(())
}

pub fn emit_workgroup_memory_barrier(context: &mut MslEmitContext) -> Result<(), MslError> {
    emit_memory_barrier(context, "thread_scope_threadgroup")
}

pub fn emit_device_memory_barrier(context: &mut MslEmitContext) -> Result<(), MslError> {
    emit_memory_barrier(context, "thread_scope_device")
}
