// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! GLSL floating-point operation emission.
//!
//! Maps to upstream `backend/glsl/emit_glsl_floating_point.cpp`.

use super::glsl_emit_context::EmitContext;

pub fn emit_fp_abs16(_ctx: &mut EmitContext) {
    panic!("GLSL FPAbs16 not implemented");
}
pub fn emit_fp_abs32(ctx: &mut EmitContext, v: &str) {
    ctx.add_fmt(format!("f_0=abs({});", v));
}
pub fn emit_fp_abs64(ctx: &mut EmitContext, v: &str) {
    ctx.add_fmt(format!("d_0=abs({});", v));
}
pub fn emit_fp_add32(ctx: &mut EmitContext, a: &str, b: &str) {
    ctx.add_fmt(format!("f_0={}+{};", a, b));
}
pub fn emit_fp_add64(ctx: &mut EmitContext, a: &str, b: &str) {
    ctx.add_fmt(format!("d_0={}+{};", a, b));
}
pub fn emit_fp_fma32(ctx: &mut EmitContext, a: &str, b: &str, c: &str) {
    ctx.add_fmt(format!("f_0=fma({},{},{});", a, b, c));
}
pub fn emit_fp_fma64(ctx: &mut EmitContext, a: &str, b: &str, c: &str) {
    ctx.add_fmt(format!("d_0=fma({},{},{});", a, b, c));
}
pub fn emit_fp_max32(ctx: &mut EmitContext, a: &str, b: &str) {
    ctx.add_fmt(format!("f_0=max({},{});", a, b));
}
pub fn emit_fp_max64(ctx: &mut EmitContext, a: &str, b: &str) {
    ctx.add_fmt(format!("d_0=max({},{});", a, b));
}
pub fn emit_fp_min32(ctx: &mut EmitContext, a: &str, b: &str) {
    ctx.add_fmt(format!("f_0=min({},{});", a, b));
}
pub fn emit_fp_min64(ctx: &mut EmitContext, a: &str, b: &str) {
    ctx.add_fmt(format!("d_0=min({},{});", a, b));
}
pub fn emit_fp_mul32(ctx: &mut EmitContext, a: &str, b: &str) {
    ctx.add_fmt(format!("f_0={}*{};", a, b));
}
pub fn emit_fp_mul64(ctx: &mut EmitContext, a: &str, b: &str) {
    ctx.add_fmt(format!("d_0={}*{};", a, b));
}
pub fn emit_fp_neg32(ctx: &mut EmitContext, v: &str) {
    ctx.add_fmt(format!("f_0=-({});", v));
}
pub fn emit_fp_neg64(ctx: &mut EmitContext, v: &str) {
    ctx.add_fmt(format!("d_0=-({});", v));
}
pub fn emit_fp_sin(ctx: &mut EmitContext, v: &str) {
    ctx.add_fmt(format!("f_0=sin({});", v));
}
pub fn emit_fp_cos(ctx: &mut EmitContext, v: &str) {
    ctx.add_fmt(format!("f_0=cos({});", v));
}
pub fn emit_fp_exp2(ctx: &mut EmitContext, v: &str) {
    ctx.add_fmt(format!("f_0=exp2({});", v));
}
/// Upstream `EmitFPTanh` (Lichii emit_glsl_floating_point.cpp, eaeb09b098).
pub fn emit_fp_tanh(ctx: &mut EmitContext, v: &str) {
    ctx.add_fmt(format!("f_0=tanh({});", v));
}
pub fn emit_fp_log2(ctx: &mut EmitContext, v: &str) {
    ctx.add_fmt(format!("f_0=log2({});", v));
}
pub fn emit_fp_recip32(ctx: &mut EmitContext, v: &str) {
    ctx.add_fmt(format!("f_0=1.0f/({});", v));
}
pub fn emit_fp_recip_sqrt32(ctx: &mut EmitContext, v: &str) {
    ctx.add_fmt(format!("f_0=inversesqrt({});", v));
}
pub fn emit_fp_sqrt(ctx: &mut EmitContext, v: &str) {
    ctx.add_fmt(format!("f_0=sqrt({});", v));
}
pub fn emit_fp_saturate32(ctx: &mut EmitContext, v: &str) {
    ctx.add_fmt(format!("f_0=clamp({},0.0f,1.0f);", v));
}
pub fn emit_fp_clamp32(ctx: &mut EmitContext, v: &str, lo: &str, hi: &str) {
    ctx.add_fmt(format!("f_0=clamp({},{},{});", v, lo, hi));
}
pub fn emit_fp_clamp64(ctx: &mut EmitContext, v: &str, lo: &str, hi: &str) {
    ctx.add_fmt(format!("d_0=clamp({},{},{});", v, lo, hi));
}
pub fn emit_fp_round_even32(ctx: &mut EmitContext, v: &str) {
    ctx.add_fmt(format!("f_0=roundEven({});", v));
}
pub fn emit_fp_round_even64(ctx: &mut EmitContext, v: &str) {
    ctx.add_fmt(format!("d_0=roundEven({});", v));
}
pub fn emit_fp_floor32(ctx: &mut EmitContext, v: &str) {
    ctx.add_fmt(format!("f_0=floor({});", v));
}
pub fn emit_fp_floor64(ctx: &mut EmitContext, v: &str) {
    ctx.add_fmt(format!("d_0=floor({});", v));
}
pub fn emit_fp_ceil32(ctx: &mut EmitContext, v: &str) {
    ctx.add_fmt(format!("f_0=ceil({});", v));
}
pub fn emit_fp_ceil64(ctx: &mut EmitContext, v: &str) {
    ctx.add_fmt(format!("d_0=ceil({});", v));
}
pub fn emit_fp_trunc32(ctx: &mut EmitContext, v: &str) {
    ctx.add_fmt(format!("f_0=trunc({});", v));
}
pub fn emit_fp_trunc64(ctx: &mut EmitContext, v: &str) {
    ctx.add_fmt(format!("d_0=trunc({});", v));
}

// Comparisons
pub fn emit_fp_ord_equal32(ctx: &mut EmitContext, a: &str, b: &str) {
    ctx.add_fmt(format!("b_0={}=={};", a, b));
}
pub fn emit_fp_ord_not_equal32(ctx: &mut EmitContext, a: &str, b: &str) {
    ctx.add_fmt(format!("b_0={}!={};", a, b));
}
pub fn emit_fp_ord_less_than32(ctx: &mut EmitContext, a: &str, b: &str) {
    ctx.add_fmt(format!("b_0={}<{};", a, b));
}
pub fn emit_fp_ord_greater_than32(ctx: &mut EmitContext, a: &str, b: &str) {
    ctx.add_fmt(format!("b_0={}>{};", a, b));
}
pub fn emit_fp_ord_less_than_equal32(ctx: &mut EmitContext, a: &str, b: &str) {
    ctx.add_fmt(format!("b_0={}<={};", a, b));
}
pub fn emit_fp_ord_greater_than_equal32(ctx: &mut EmitContext, a: &str, b: &str) {
    ctx.add_fmt(format!("b_0={}>={};", a, b));
}
pub fn emit_fp_unord_equal32(ctx: &mut EmitContext, a: &str, b: &str) {
    ctx.add_fmt(format!("b_0=!({}!={})||isnan({})||isnan({});", a, b, a, b));
}
pub fn emit_fp_unord_not_equal32(ctx: &mut EmitContext, a: &str, b: &str) {
    ctx.add_fmt(format!("b_0=!({}=={})||isnan({})||isnan({});", a, b, a, b));
}
pub fn emit_fp_unord_less_than32(ctx: &mut EmitContext, a: &str, b: &str) {
    ctx.add_fmt(format!("b_0=!({}>={});", a, b));
}
pub fn emit_fp_unord_greater_than32(ctx: &mut EmitContext, a: &str, b: &str) {
    ctx.add_fmt(format!("b_0=!({}<={});", a, b));
}
pub fn emit_fp_unord_less_than_equal32(ctx: &mut EmitContext, a: &str, b: &str) {
    ctx.add_fmt(format!("b_0=!({}>{});", a, b));
}
pub fn emit_fp_unord_greater_than_equal32(ctx: &mut EmitContext, a: &str, b: &str) {
    ctx.add_fmt(format!("b_0=!({}<{});", a, b));
}
pub fn emit_fp_is_nan32(ctx: &mut EmitContext, v: &str) {
    ctx.add_fmt(format!("b_0=isnan({});", v));
}
