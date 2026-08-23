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
// VectorGreaterEqualSigned — a >= b ≡ NOT(b > a) ≡ NOT(pcmpgt(b, a))
// Upstream pattern: pcmpgt(b, a) → pcmpeqd(ones,ones) → pxor(result, ones)
// ---------------------------------------------------------------------------

fn emit_vector_ge_signed(
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    cmpgt: fn(&mut rxbyak::CodeAssembler, rxbyak::Reg, rxbyak::Reg) -> rxbyak::Result<()>,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_xmm(&mut args[0]);
    let result = ra.use_scratch_xmm(&mut args[1]); // result starts as b
                                                   // result = pcmpgt(b, a)
    cmpgt(&mut *ra.asm, result, a).unwrap();
    // Invert: result = NOT(result) via XOR with all-ones
    let ones = ra.scratch_xmm();
    ra.asm.pcmpeqd(ones, ones).unwrap();
    ra.asm.pxor(result, ones).unwrap();
    ra.release(ones);
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_greater_equal_signed8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_ge_signed(ra, inst_ref, inst, rxbyak::CodeAssembler::pcmpgtb);
}
pub fn emit_vector_greater_equal_signed16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_ge_signed(ra, inst_ref, inst, rxbyak::CodeAssembler::pcmpgtw);
}
pub fn emit_vector_greater_equal_signed32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_ge_signed(ra, inst_ref, inst, rxbyak::CodeAssembler::pcmpgtd);
}
pub fn emit_vector_greater_equal_signed64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_ge_signed(ra, inst_ref, inst, rxbyak::CodeAssembler::pcmpgtq);
}

fn emit_vector_le_signed(
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    cmpgt: fn(&mut rxbyak::CodeAssembler, rxbyak::Reg, rxbyak::Reg) -> rxbyak::Result<()>,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_xmm(&mut args[0]);
    let b = ra.use_scratch_xmm(&mut args[1]);
    cmpgt(&mut *ra.asm, result, b).unwrap();
    let ones = ra.scratch_xmm();
    ra.asm.pcmpeqd(ones, ones).unwrap();
    ra.asm.pxor(result, ones).unwrap();
    ra.release(ones);
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_less_equal_signed8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_le_signed(ra, inst_ref, inst, rxbyak::CodeAssembler::pcmpgtb);
}
pub fn emit_vector_less_equal_signed16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_le_signed(ra, inst_ref, inst, rxbyak::CodeAssembler::pcmpgtw);
}
pub fn emit_vector_less_equal_signed32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_le_signed(ra, inst_ref, inst, rxbyak::CodeAssembler::pcmpgtd);
}
pub fn emit_vector_less_equal_signed64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_le_signed(ra, inst_ref, inst, rxbyak::CodeAssembler::pcmpgtq);
}

fn emit_vector_lt_signed(
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    cmpgt: fn(&mut rxbyak::CodeAssembler, rxbyak::Reg, rxbyak::Reg) -> rxbyak::Result<()>,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_xmm(&mut args[0]);
    let result = ra.use_scratch_xmm(&mut args[1]);
    cmpgt(&mut *ra.asm, result, a).unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_less_signed8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_lt_signed(ra, inst_ref, inst, rxbyak::CodeAssembler::pcmpgtb);
}
pub fn emit_vector_less_signed16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_lt_signed(ra, inst_ref, inst, rxbyak::CodeAssembler::pcmpgtw);
}
pub fn emit_vector_less_signed32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_lt_signed(ra, inst_ref, inst, rxbyak::CodeAssembler::pcmpgtd);
}
pub fn emit_vector_less_signed64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_lt_signed(ra, inst_ref, inst, rxbyak::CodeAssembler::pcmpgtq);
}

// ---------------------------------------------------------------------------
// VectorGreaterEqualUnsigned — a >= b ≡ pminuX(a, b) == b
// Upstream pattern: tmp = pminuX(a, b); pcmpeqX(tmp, b) → result
// ---------------------------------------------------------------------------

fn emit_vector_ge_unsigned(
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    pmin: fn(&mut rxbyak::CodeAssembler, rxbyak::Reg, rxbyak::Reg) -> rxbyak::Result<()>,
    pcmpeq: fn(&mut rxbyak::CodeAssembler, rxbyak::Reg, rxbyak::Reg) -> rxbyak::Result<()>,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]); // result starts as a
    let b = ra.use_xmm(&mut args[1]);
    // result = pminuX(a, b)
    pmin(&mut *ra.asm, result, b).unwrap();
    // result = pcmpeqX(result, b) — if min(a,b)==b then a>=b
    pcmpeq(&mut *ra.asm, result, b).unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_greater_equal_unsigned8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_ge_unsigned(
        ra,
        inst_ref,
        inst,
        rxbyak::CodeAssembler::pminub,
        rxbyak::CodeAssembler::pcmpeqb,
    );
}
pub fn emit_vector_greater_equal_unsigned16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_ge_unsigned(
        ra,
        inst_ref,
        inst,
        rxbyak::CodeAssembler::pminuw,
        rxbyak::CodeAssembler::pcmpeqw,
    );
}
pub fn emit_vector_greater_equal_unsigned32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_ge_unsigned(
        ra,
        inst_ref,
        inst,
        rxbyak::CodeAssembler::pminud,
        rxbyak::CodeAssembler::pcmpeqd,
    );
}

// VectorGreaterEqualUnsigned64 — no pminuq in SSE, use: NOT(a < b) = NOT(b > a signed with XOR trick)
// Upstream flips sign bits, does pcmpgtq, then inverts. We use fallback for now.
extern "C" fn fallback_ge_unsigned64(
    result: *mut [u8; 16],
    a: *const [u8; 16],
    b: *const [u8; 16],
) {
    unsafe {
        let va: [u64; 2] = std::mem::transmute(*a);
        let vb: [u64; 2] = std::mem::transmute(*b);
        let mut out = [0u64; 2];
        for i in 0..2 {
            out[i] = if va[i] >= vb[i] { u64::MAX } else { 0 };
        }
        *result = std::mem::transmute(out);
    }
}

pub fn emit_vector_greater_equal_unsigned64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(ra, inst_ref, inst, fallback_ge_unsigned64 as usize);
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
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_greater_equal_signed8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_less_equal_signed8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_less_signed8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_greater_equal_unsigned64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_min_signed8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_max_unsigned64;
    }

    // Test removed: fallback_min_signed64 replaced with inline SSE (blendvpd)
    // Correctness verified via a32_diff fuzzing
}
