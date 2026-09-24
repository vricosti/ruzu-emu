// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! GLSL atomic operation emission.
//!
//! Maps to upstream `backend/glsl/emit_glsl_atomic.cpp`.

use super::glsl_emit_context::EmitContext;

/// Ruzu extension: compare first, replacement second, return the old word.
pub fn emit_storage_compare_exchange(
    ctx: &mut EmitContext,
    program: &mut crate::ir::Program,
    inst_ref: crate::ir::value::InstRef,
    inst: &crate::ir::Inst,
) {
    let binding = inst.arg(0).imm_u32();
    let offset = ctx.var_alloc.consume(program, inst.arg(1));
    let compare = ctx.var_alloc.consume(program, inst.arg(2));
    let replacement = ctx.var_alloc.consume(program, inst.arg(3));
    let dst = ctx.var_alloc.define(
        program.block_mut(inst_ref.block).inst_mut(inst_ref.inst),
        super::var_alloc::GlslVarType::U32,
    );
    ctx.add_fmt(format!(
        "{dst}=atomicCompSwap({}_ssbo{binding}[{offset}>>2],{compare},{replacement});",
        ctx.stage_name
    ));
}

pub fn emit_storage_atomic_iadd32(ctx: &mut EmitContext, binding: &str, offset: &str) {
    ctx.add_fmt(format!("u_0=atomicAdd(ssbo{}[{}/4],u_1);", binding, offset));
}
pub fn emit_storage_atomic_smin32(ctx: &mut EmitContext, binding: &str, offset: &str) {
    ctx.add_fmt(format!(
        "u_0=uint(atomicMin(ssbo_s{}[{}/4],int(u_1)));",
        binding, offset
    ));
}
pub fn emit_storage_atomic_umin32(ctx: &mut EmitContext, binding: &str, offset: &str) {
    ctx.add_fmt(format!("u_0=atomicMin(ssbo{}[{}/4],u_1);", binding, offset));
}
pub fn emit_storage_atomic_smax32(ctx: &mut EmitContext, binding: &str, offset: &str) {
    ctx.add_fmt(format!(
        "u_0=uint(atomicMax(ssbo_s{}[{}/4],int(u_1)));",
        binding, offset
    ));
}
pub fn emit_storage_atomic_umax32(ctx: &mut EmitContext, binding: &str, offset: &str) {
    ctx.add_fmt(format!("u_0=atomicMax(ssbo{}[{}/4],u_1);", binding, offset));
}
pub fn emit_storage_atomic_and32(ctx: &mut EmitContext, binding: &str, offset: &str) {
    ctx.add_fmt(format!("u_0=atomicAnd(ssbo{}[{}/4],u_1);", binding, offset));
}
pub fn emit_storage_atomic_or32(ctx: &mut EmitContext, binding: &str, offset: &str) {
    ctx.add_fmt(format!("u_0=atomicOr(ssbo{}[{}/4],u_1);", binding, offset));
}
pub fn emit_storage_atomic_xor32(ctx: &mut EmitContext, binding: &str, offset: &str) {
    ctx.add_fmt(format!("u_0=atomicXor(ssbo{}[{}/4],u_1);", binding, offset));
}
pub fn emit_storage_atomic_exchange32(ctx: &mut EmitContext, binding: &str, offset: &str) {
    ctx.add_fmt(format!(
        "u_0=atomicExchange(ssbo{}[{}/4],u_1);",
        binding, offset
    ));
}

// Shared memory atomics
pub fn emit_shared_atomic_iadd32(ctx: &mut EmitContext, offset: &str) {
    ctx.add_fmt(format!("u_0=atomicAdd(smem[{}/4],u_1);", offset));
}
pub fn emit_shared_atomic_smin32(ctx: &mut EmitContext, offset: &str) {
    ctx.add_fmt(format!(
        "u_0=uint(atomicMin(smem_s[{}/4],int(u_1)));",
        offset
    ));
}
pub fn emit_shared_atomic_umin32(ctx: &mut EmitContext, offset: &str) {
    ctx.add_fmt(format!("u_0=atomicMin(smem[{}/4],u_1);", offset));
}
pub fn emit_shared_atomic_smax32(ctx: &mut EmitContext, offset: &str) {
    ctx.add_fmt(format!(
        "u_0=uint(atomicMax(smem_s[{}/4],int(u_1)));",
        offset
    ));
}
pub fn emit_shared_atomic_umax32(ctx: &mut EmitContext, offset: &str) {
    ctx.add_fmt(format!("u_0=atomicMax(smem[{}/4],u_1);", offset));
}
pub fn emit_shared_atomic_and32(ctx: &mut EmitContext, offset: &str) {
    ctx.add_fmt(format!("u_0=atomicAnd(smem[{}/4],u_1);", offset));
}
pub fn emit_shared_atomic_or32(ctx: &mut EmitContext, offset: &str) {
    ctx.add_fmt(format!("u_0=atomicOr(smem[{}/4],u_1);", offset));
}
pub fn emit_shared_atomic_xor32(ctx: &mut EmitContext, offset: &str) {
    ctx.add_fmt(format!("u_0=atomicXor(smem[{}/4],u_1);", offset));
}
pub fn emit_shared_atomic_exchange32(ctx: &mut EmitContext, offset: &str) {
    ctx.add_fmt(format!("u_0=atomicExchange(smem[{}/4],u_1);", offset));
}
