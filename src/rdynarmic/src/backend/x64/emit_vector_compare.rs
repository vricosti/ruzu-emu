#![allow(
    clippy::missing_transmute_annotations,
    clippy::useless_transmute,
    unnecessary_transmutes
)]

use crate::backend::x64::emit_context::EmitContext;
use crate::backend::x64::emit_vector_helpers::*;
use crate::backend::x64::reg_alloc::RegAlloc;
use crate::ir::inst::Inst;
use crate::ir::value::InstRef;

// ---------------------------------------------------------------------------
// VectorEqual — native SSE: pcmpeqb/w/d/q
// ---------------------------------------------------------------------------

pub fn emit_vector_equal8(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pcmpeqb);
}
pub fn emit_vector_equal16(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pcmpeqw);
}
pub fn emit_vector_equal32(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pcmpeqd);
}
pub fn emit_vector_equal64(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pcmpeqq);
}

// VectorEqual128: pcmpeqq then AND both qwords to get full 128-bit equality
// Upstream: pcmpeqq(a,b) → pshufd(tmp,a,0b01001110) → pand(a,tmp)
pub fn emit_vector_equal128(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let op2 = ra.use_xmm(&mut args[1]);
    let tmp = ra.scratch_xmm();
    ra.asm.pcmpeqq(result, op2).unwrap();
    // Swap high and low qwords: pshufd with 0b01_00_11_10 = 0x4E
    ra.asm.pshufd(tmp, result, 0x4E).unwrap();
    ra.asm.pand(result, tmp).unwrap();
    ra.release(tmp);
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// VectorGreaterSigned — native SSE: pcmpgtb/w/d/q
// ---------------------------------------------------------------------------

pub fn emit_vector_greater_signed8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pcmpgtb);
}
pub fn emit_vector_greater_signed16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pcmpgtw);
}
pub fn emit_vector_greater_signed32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pcmpgtd);
}
pub fn emit_vector_greater_signed64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pcmpgtq);
}

// ---------------------------------------------------------------------------
// VectorMinSigned — native SSE4.1: pminsb/pminsw/pminsd
// VectorMinS64 — fallback
// ---------------------------------------------------------------------------

pub fn emit_vector_min_signed8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pminsb);
}
pub fn emit_vector_min_signed16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pminsw);
}
pub fn emit_vector_min_signed32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pminsd);
}

// VectorMinS64: pcmpgtq(a,b) gives mask where a>b; blendvpd selects b where a>b, else a
// result = min(a,b): where a > b, pick b; otherwise pick a
// Upstream: pcmpgtq(tmp=a, b) → XMM0=tmp → blendvpd(a, b, XMM0) → result=a
pub fn emit_vector_min_signed64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]); // starts as a
    let b = ra.use_xmm(&mut args[1]);
    let mask = ra.scratch_xmm();
    // mask = a
    ra.asm.movaps(mask, result).unwrap();
    // mask = pcmpgtq(a, b) — all 1s where a > b
    ra.asm.pcmpgtq(mask, b).unwrap();
    // blendvpd uses XMM0 as implicit mask; move mask there
    ra.asm.movaps(rxbyak::XMM0, mask).unwrap();
    // result = blendvpd(a, b, XMM0): where mask=1 pick b, else keep a
    ra.asm.blendvpd(result, b).unwrap();
    ra.release(mask);
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// VectorMaxSigned — native SSE4.1: pmaxsb/pmaxsw/pmaxsd
// VectorMaxS64 — fallback
// ---------------------------------------------------------------------------

pub fn emit_vector_max_signed8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pmaxsb);
}
pub fn emit_vector_max_signed16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pmaxsw);
}
pub fn emit_vector_max_signed32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pmaxsd);
}

// VectorMaxS64: pcmpgtq(a,b) gives mask where a>b; blendvpd selects a where a>b, else b
// result = max(a,b): where a > b, pick a; otherwise pick b
pub fn emit_vector_max_signed64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_xmm(&mut args[0]);
    let result = ra.use_scratch_xmm(&mut args[1]); // starts as b
    let mask = ra.scratch_xmm();
    // mask = a
    ra.asm.movaps(mask, a).unwrap();
    // mask = pcmpgtq(a, b) — all 1s where a > b
    ra.asm.pcmpgtq(mask, result).unwrap();
    // blendvpd uses XMM0 as implicit mask
    ra.asm.movaps(rxbyak::XMM0, mask).unwrap();
    // result = blendvpd(b, a, XMM0): where mask=1 pick a, else keep b
    ra.asm.blendvpd(result, a).unwrap();
    ra.release(mask);
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// VectorMinUnsigned — native SSE4.1: pminub/pminuw/pminud
// VectorMinU64 — fallback
// ---------------------------------------------------------------------------

pub fn emit_vector_min_unsigned8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pminub);
}
pub fn emit_vector_min_unsigned16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pminuw);
}
pub fn emit_vector_min_unsigned32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pminud);
}

// VectorMinU64: XOR sign bits to convert unsigned → signed comparison, then blendvpd
// Upstream: sign_bit = 0x8000000000000000; a_s = a^sign; b_s = b^sign; pcmpgtq(a_s,b_s) → blend
pub fn emit_vector_min_unsigned64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]); // starts as a
    let b = ra.use_xmm(&mut args[1]);
    // Load sign bit constant from constant pool
    let pool = ra.constant_pool.as_mut().expect("constant pool required");
    let sign_addr = pool.get_constant(0x8000_0000_0000_0000, 0x8000_0000_0000_0000);
    let a_signed = ra.scratch_xmm();
    let b_signed = ra.scratch_xmm();
    // a_signed = a ^ sign_bit, b_signed = b ^ sign_bit
    ra.asm.movaps(a_signed, result).unwrap();
    ra.asm
        .pxor(a_signed, rxbyak::xmmword_ptr(sign_addr))
        .unwrap();
    ra.asm.movaps(b_signed, b).unwrap();
    ra.asm
        .pxor(b_signed, rxbyak::xmmword_ptr(sign_addr))
        .unwrap();
    // mask = pcmpgtq(a_signed, b_signed) — where a > b unsigned
    ra.asm.pcmpgtq(a_signed, b_signed).unwrap();
    // blendvpd: where a>b pick b, else keep a → min
    ra.asm.movaps(rxbyak::XMM0, a_signed).unwrap();
    ra.asm.blendvpd(result, b).unwrap();
    ra.release(a_signed);
    ra.release(b_signed);
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// VectorMaxUnsigned — native SSE4.1: pmaxub/pmaxuw/pmaxud
// VectorMaxU64 — fallback
// ---------------------------------------------------------------------------

pub fn emit_vector_max_unsigned8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pmaxub);
}
pub fn emit_vector_max_unsigned16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pmaxuw);
}
pub fn emit_vector_max_unsigned32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pmaxud);
}

// VectorMaxU64: same sign-flip trick, but pick a where a>b
pub fn emit_vector_max_unsigned64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_xmm(&mut args[0]);
    let result = ra.use_scratch_xmm(&mut args[1]); // starts as b
    let pool = ra.constant_pool.as_mut().expect("constant pool required");
    let sign_addr = pool.get_constant(0x8000_0000_0000_0000, 0x8000_0000_0000_0000);
    let a_signed = ra.scratch_xmm();
    let b_signed = ra.scratch_xmm();
    ra.asm.movaps(a_signed, a).unwrap();
    ra.asm
        .pxor(a_signed, rxbyak::xmmword_ptr(sign_addr))
        .unwrap();
    ra.asm.movaps(b_signed, result).unwrap();
    ra.asm
        .pxor(b_signed, rxbyak::xmmword_ptr(sign_addr))
        .unwrap();
    // mask = pcmpgtq(a_signed, b_signed) — where a > b unsigned
    ra.asm.pcmpgtq(a_signed, b_signed).unwrap();
    // blendvpd: where a>b pick a, else keep b → max
    ra.asm.movaps(rxbyak::XMM0, a_signed).unwrap();
    ra.asm.blendvpd(result, a).unwrap();
    ra.release(a_signed);
    ra.release(b_signed);
    ra.define_value(inst_ref, result);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fn_signatures() {
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_equal8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_equal128;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_greater_signed8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_min_signed8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_max_unsigned64;
    }

    // Test removed: fallback_min_signed64 replaced with inline SSE (blendvpd)
    // Correctness verified via a32_diff fuzzing
}
