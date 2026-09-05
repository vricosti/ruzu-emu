#![allow(
    clippy::missing_transmute_annotations,
    clippy::useless_transmute,
    unnecessary_transmutes
)]

use crate::backend::x64::emit_context::EmitContext;
use crate::backend::x64::emit_vector_helpers::*;
use crate::backend::x64::host_feature::HostFeature;
use crate::backend::x64::reg_alloc::RegAlloc;
use crate::ir::inst::Inst;
use crate::ir::value::InstRef;

// ---------------------------------------------------------------------------
// VectorMultiply — native SSE for 16/32; fallback for 8/64
// ---------------------------------------------------------------------------

// VectorMultiply8: no pmullb in SSE, use pmullw on pairs then mask
// Upstream pattern:
//   tmp_a = a; tmp_b = b
//   pmullw(a, b)         — multiply even bytes as words
//   psrlw(tmp_a, 8); psrlw(tmp_b, 8)  — shift odd bytes to low position
//   pmullw(tmp_a, tmp_b) — multiply odd bytes
//   pand(a, mask_00FF)   — keep low byte of each word (even results)
//   psllw(tmp_a, 8)      — shift odd results to high byte
//   por(a, tmp_a)        — merge even and odd results
pub fn emit_vector_multiply8(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    if ctx.has_host_feature(HostFeature::AVX) {
        let a = ra.use_scratch_xmm(&mut args[0]);
        let b = ra.use_scratch_xmm(&mut args[1]);
        let product = ra.scratch_xmm();
        let mask = ra.scratch_xmm();
        let mask_addr = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0x00ff_00ff, 0);
        ra.asm
            .vbroadcastss(mask, rxbyak::dword_ptr(mask_addr))
            .unwrap();
        ra.asm.vpmullw(product, b, a).unwrap();
        ra.asm.vpandn(a, mask, a).unwrap();
        ra.asm.vpand(product, product, mask).unwrap();
        ra.asm.vpmaddubsw(a, b, a).unwrap();
        ra.asm.psllw_imm(a, 8).unwrap();
        ra.asm.vpor(a, product, a).unwrap();
        ra.release(b);
        ra.release(product);
        ra.release(mask);
        ra.define_value(inst_ref, a);
        return;
    }
    let result = ra.use_scratch_xmm(&mut args[0]); // a
    let b = ra.use_xmm(&mut args[1]);
    let tmp_a = ra.scratch_xmm();
    let tmp_b = ra.scratch_xmm();
    // Save copies for odd-byte multiplication
    ra.asm.movaps(tmp_a, result).unwrap(); // tmp_a = a
    ra.asm.movaps(tmp_b, b).unwrap(); // tmp_b = b
                                      // Even bytes: pmullw(a, b) — multiplies pairs of bytes, low word contains lo byte product
    ra.asm.pmullw(result, b).unwrap();
    // Odd bytes: shift right by 8 to move odd bytes to even position
    ra.asm.psrlw_imm(tmp_a, 8).unwrap();
    ra.asm.psrlw_imm(tmp_b, 8).unwrap();
    ra.asm.pmullw(tmp_a, tmp_b).unwrap();
    // Mask even results to low bytes
    let pool = ra.constant_pool.as_mut().expect("constant pool required");
    let mask_addr = pool.get_constant(0x00FF_00FF_00FF_00FF, 0x00FF_00FF_00FF_00FF);
    ra.asm.pand(result, rxbyak::xmmword_ptr(mask_addr)).unwrap();
    // Shift odd results to high byte position
    ra.asm.psllw_imm(tmp_a, 8).unwrap();
    // Merge
    ra.asm.por(result, tmp_a).unwrap();
    ra.release(tmp_a);
    ra.release(tmp_b);
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_multiply16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pmullw);
}
pub fn emit_vector_multiply32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    if ctx.has_host_feature(HostFeature::SSE41) {
        emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pmulld);
        return;
    }
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_scratch_xmm(&mut args[0]);
    let b = ra.use_scratch_xmm(&mut args[1]);
    let tmp = ra.scratch_xmm();
    ra.asm.movdqa(tmp, a).unwrap();
    ra.asm.psrlq_imm(a, 32).unwrap();
    ra.asm.pmuludq(tmp, b).unwrap();
    ra.asm.psrlq_imm(b, 32).unwrap();
    ra.asm.pmuludq(a, b).unwrap();
    ra.asm.pshufd(tmp, tmp, 0x08).unwrap();
    ra.asm.pshufd(b, a, 0x08).unwrap();
    ra.asm.punpckldq(tmp, b).unwrap();
    ra.release(a);
    ra.release(b);
    ra.define_value(inst_ref, tmp);
}

pub fn emit_vector_multiply64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    if ctx.has_host_feature(HostFeature::AVX512_ORTHO | HostFeature::AVX512DQ) {
        let a = ra.use_scratch_xmm(&mut args[0]);
        let b = ra.use_xmm(&mut args[1]);
        ra.asm.vpmullq(a, a, b).unwrap();
        ra.define_value(inst_ref, a);
    } else if ctx.has_host_feature(HostFeature::SSE41) {
        let a = ra.use_scratch_xmm(&mut args[0]);
        let b = ra.use_xmm(&mut args[1]);
        let tmp1 = ra.scratch_gpr();
        let tmp2 = ra.scratch_gpr();
        ra.asm.movq(tmp1, a).unwrap();
        ra.asm.movq(tmp2, b).unwrap();
        ra.asm.imul(tmp2, tmp1).unwrap();
        ra.asm.pextrq(tmp1, a, 1).unwrap();
        ra.asm.movq(a, tmp2).unwrap();
        ra.asm.pextrq(tmp2, b, 1).unwrap();
        ra.asm.imul(tmp1, tmp2).unwrap();
        ra.asm.pinsrq(a, tmp1, 1).unwrap();
        ra.release(tmp1);
        ra.release(tmp2);
        ra.define_value(inst_ref, a);
    } else {
        let a = ra.use_xmm(&mut args[0]);
        let b = ra.use_scratch_xmm(&mut args[1]);
        let tmp1 = ra.scratch_xmm();
        let tmp2 = ra.scratch_xmm();
        let tmp3 = ra.scratch_xmm();
        ra.asm.movdqa(tmp1, a).unwrap();
        ra.asm.movdqa(tmp2, a).unwrap();
        ra.asm.movdqa(tmp3, b).unwrap();
        ra.asm.psrlq_imm(tmp1, 32).unwrap();
        ra.asm.psrlq_imm(tmp3, 32).unwrap();
        ra.asm.pmuludq(tmp2, b).unwrap();
        ra.asm.pmuludq(tmp3, a).unwrap();
        ra.asm.pmuludq(b, tmp1).unwrap();
        ra.asm.paddq(b, tmp3).unwrap();
        ra.asm.psllq_imm(b, 32).unwrap();
        ra.asm.paddq(tmp2, b).unwrap();
        ra.release(b);
        ra.release(tmp1);
        ra.release(tmp3);
        ra.define_value(inst_ref, tmp2);
    }
}

// ---------------------------------------------------------------------------
// VectorMultiplySignedWiden — eliminated by the mandatory x64 polyfill
// ---------------------------------------------------------------------------

pub fn emit_vector_multiply_signed_widen8(
    _ctx: &EmitContext,
    _ra: &mut RegAlloc,
    _inst_ref: InstRef,
    _inst: &Inst,
) {
    unreachable!();
}
pub fn emit_vector_multiply_signed_widen16(
    _ctx: &EmitContext,
    _ra: &mut RegAlloc,
    _inst_ref: InstRef,
    _inst: &Inst,
) {
    unreachable!();
}
pub fn emit_vector_multiply_signed_widen32(
    _ctx: &EmitContext,
    _ra: &mut RegAlloc,
    _inst_ref: InstRef,
    _inst: &Inst,
) {
    unreachable!();
}

// ---------------------------------------------------------------------------
// VectorMultiplyUnsignedWiden — eliminated by the mandatory x64 polyfill
// ---------------------------------------------------------------------------

pub fn emit_vector_multiply_unsigned_widen8(
    _ctx: &EmitContext,
    _ra: &mut RegAlloc,
    _inst_ref: InstRef,
    _inst: &Inst,
) {
    unreachable!();
}
pub fn emit_vector_multiply_unsigned_widen16(
    _ctx: &EmitContext,
    _ra: &mut RegAlloc,
    _inst_ref: InstRef,
    _inst: &Inst,
) {
    unreachable!();
}
pub fn emit_vector_multiply_unsigned_widen32(
    _ctx: &EmitContext,
    _ra: &mut RegAlloc,
    _inst_ref: InstRef,
    _inst: &Inst,
) {
    unreachable!();
}

// ---------------------------------------------------------------------------
// VectorPolynomialMultiply — fallback (GF(2) multiplication)
// ---------------------------------------------------------------------------

extern "C" fn fallback_poly_mul8(result: *mut [u8; 16], a: *const [u8; 16], b: *const [u8; 16]) {
    unsafe {
        let va = &*a;
        let vb = &*b;
        let dst = &mut *result;
        for i in 0..16 {
            let mut r = 0u8;
            for bit in 0..8 {
                if (vb[i] >> bit) & 1 != 0 {
                    r ^= va[i] << bit;
                }
            }
            dst[i] = r;
        }
    }
}

extern "C" fn fallback_poly_mul_long8(
    result: *mut [u8; 16],
    a: *const [u8; 16],
    b: *const [u8; 16],
) {
    unsafe {
        let va = &*a;
        let vb = &*b;
        let mut out = [0u16; 8];
        for i in 0..8 {
            let mut r = 0u16;
            for bit in 0..8 {
                if (vb[i] >> bit) & 1 != 0 {
                    r ^= (va[i] as u16) << bit;
                }
            }
            out[i] = r;
        }
        *result = std::mem::transmute(out);
    }
}

extern "C" fn fallback_poly_mul_long64(
    result: *mut [u8; 16],
    a: *const [u8; 16],
    b: *const [u8; 16],
) {
    unsafe {
        let va: [u64; 2] = std::mem::transmute(*a);
        let vb: [u64; 2] = std::mem::transmute(*b);
        let mut r = 0u128;
        for bit in 0..64 {
            if (vb[0] >> bit) & 1 != 0 {
                r ^= (va[0] as u128) << bit;
            }
        }
        *result = std::mem::transmute(r);
    }
}

pub fn emit_vector_polynomial_multiply8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(ra, inst_ref, inst, fallback_poly_mul8 as *const () as usize);
}
pub fn emit_vector_polynomial_multiply_long8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_poly_mul_long8 as *const () as usize,
    );
}
pub fn emit_vector_polynomial_multiply_long64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_poly_mul_long64 as *const () as usize,
    );
}

// ---------------------------------------------------------------------------
// VectorPairedAddLower
// ---------------------------------------------------------------------------

pub fn emit_vector_paired_add_lower8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_scratch_xmm(&mut args[0]);
    let b = ra.use_xmm(&mut args[1]);
    let tmp = ra.scratch_xmm();
    ra.asm.punpcklqdq(a, b).unwrap();
    ra.asm.movdqa(tmp, a).unwrap();
    ra.asm.psllw_imm(a, 8).unwrap();
    ra.asm.paddw(a, tmp).unwrap();
    ra.asm.pxor(tmp, tmp).unwrap();
    ra.asm.psrlw_imm(a, 8).unwrap();
    ra.asm.packuswb(a, tmp).unwrap();
    ra.release(tmp);
    ra.define_value(inst_ref, a);
}
pub fn emit_vector_paired_add_lower16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_scratch_xmm(&mut args[0]);
    let b = ra.use_xmm(&mut args[1]);
    let tmp = ra.scratch_xmm();
    ra.asm.punpcklqdq(a, b).unwrap();
    if ctx.has_host_feature(HostFeature::SSSE3) {
        ra.asm.pxor(tmp, tmp).unwrap();
        ra.asm.phaddw(a, tmp).unwrap();
    } else {
        ra.asm.movdqa(tmp, a).unwrap();
        ra.asm.pslld_imm(a, 16).unwrap();
        ra.asm.paddd(a, tmp).unwrap();
        ra.asm.pxor(tmp, tmp).unwrap();
        ra.asm.psrad_imm(a, 16).unwrap();
        ra.asm.packssdw(a, tmp).unwrap();
    }
    ra.release(tmp);
    ra.define_value(inst_ref, a);
}
pub fn emit_vector_paired_add_lower32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_scratch_xmm(&mut args[0]);
    let b = ra.use_xmm(&mut args[1]);
    let tmp = ra.scratch_xmm();
    ra.asm.punpcklqdq(a, b).unwrap();
    if ctx.has_host_feature(HostFeature::SSSE3) {
        ra.asm.pxor(tmp, tmp).unwrap();
        ra.asm.phaddd(a, tmp).unwrap();
    } else {
        ra.asm.movdqa(tmp, a).unwrap();
        ra.asm.psllq_imm(a, 32).unwrap();
        ra.asm.paddq(a, tmp).unwrap();
        ra.asm.psrlq_imm(a, 32).unwrap();
        ra.asm.pshufd(a, a, 0b1101_1000).unwrap();
    }
    ra.release(tmp);
    ra.define_value(inst_ref, a);
}

// ---------------------------------------------------------------------------
// VectorPairedAdd
// ---------------------------------------------------------------------------

pub fn emit_vector_paired_add8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_scratch_xmm(&mut args[0]);
    let b = ra.use_scratch_xmm(&mut args[1]);
    let c = ra.scratch_xmm();
    let d = ra.scratch_xmm();

    ra.asm.movdqa(c, a).unwrap();
    ra.asm.movdqa(d, b).unwrap();
    ra.asm.psllw_imm(a, 8).unwrap();
    ra.asm.psllw_imm(b, 8).unwrap();
    ra.asm.paddw(a, c).unwrap();
    ra.asm.paddw(b, d).unwrap();
    ra.asm.psrlw_imm(a, 8).unwrap();
    ra.asm.psrlw_imm(b, 8).unwrap();
    ra.asm.packuswb(a, b).unwrap();

    ra.release(c);
    ra.release(d);
    ra.define_value(inst_ref, a);
}
pub fn emit_vector_paired_add16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    if ctx.has_host_feature(HostFeature::SSSE3) {
        let a = ra.use_scratch_xmm(&mut args[0]);
        let b = ra.use_xmm(&mut args[1]);
        ra.asm.phaddw(a, b).unwrap();
        ra.define_value(inst_ref, a);
    } else {
        let a = ra.use_scratch_xmm(&mut args[0]);
        let b = ra.use_scratch_xmm(&mut args[1]);
        let c = ra.scratch_xmm();
        let d = ra.scratch_xmm();
        ra.asm.movdqa(c, a).unwrap();
        ra.asm.movdqa(d, b).unwrap();
        ra.asm.pslld_imm(a, 16).unwrap();
        ra.asm.pslld_imm(b, 16).unwrap();
        ra.asm.paddd(a, c).unwrap();
        ra.asm.paddd(b, d).unwrap();
        ra.asm.psrad_imm(a, 16).unwrap();
        ra.asm.psrad_imm(b, 16).unwrap();
        ra.asm.packssdw(a, b).unwrap();
        ra.release(b);
        ra.release(c);
        ra.release(d);
        ra.define_value(inst_ref, a);
    }
}
pub fn emit_vector_paired_add32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    if ctx.has_host_feature(HostFeature::SSSE3) {
        let a = ra.use_scratch_xmm(&mut args[0]);
        let b = ra.use_xmm(&mut args[1]);
        ra.asm.phaddd(a, b).unwrap();
        ra.define_value(inst_ref, a);
    } else {
        let a = ra.use_scratch_xmm(&mut args[0]);
        let b = ra.use_scratch_xmm(&mut args[1]);
        let c = ra.scratch_xmm();
        let d = ra.scratch_xmm();
        ra.asm.movdqa(c, a).unwrap();
        ra.asm.movdqa(d, b).unwrap();
        ra.asm.psllq_imm(a, 32).unwrap();
        ra.asm.psllq_imm(b, 32).unwrap();
        ra.asm.paddq(a, c).unwrap();
        ra.asm.paddq(b, d).unwrap();
        ra.asm.shufps(a, b, 0b1101_1101).unwrap();
        ra.release(b);
        ra.release(c);
        ra.release(d);
        ra.define_value(inst_ref, a);
    }
}
pub fn emit_vector_paired_add64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_scratch_xmm(&mut args[0]);
    let b = ra.use_xmm(&mut args[1]);
    let c = ra.scratch_xmm();
    ra.asm.movdqa(c, a).unwrap();
    ra.asm.punpcklqdq(a, b).unwrap();
    ra.asm.punpckhqdq(c, b).unwrap();
    ra.asm.paddq(a, c).unwrap();
    ra.release(c);
    ra.define_value(inst_ref, a);
}

// ---------------------------------------------------------------------------
// VectorPairedAddSignedWiden
// ---------------------------------------------------------------------------

pub fn emit_vector_paired_add_signed_widen8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_scratch_xmm(&mut args[0]);
    let c = ra.scratch_xmm();
    ra.asm.movdqa(c, a).unwrap();
    ra.asm.psllw_imm(a, 8).unwrap();
    ra.asm.psraw_imm(c, 8).unwrap();
    ra.asm.psraw_imm(a, 8).unwrap();
    ra.asm.paddw(a, c).unwrap();
    ra.release(c);
    ra.define_value(inst_ref, a);
}
pub fn emit_vector_paired_add_signed_widen16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_scratch_xmm(&mut args[0]);
    let c = ra.scratch_xmm();
    ra.asm.movdqa(c, a).unwrap();
    ra.asm.pslld_imm(a, 16).unwrap();
    ra.asm.psrad_imm(c, 16).unwrap();
    ra.asm.psrad_imm(a, 16).unwrap();
    ra.asm.paddd(a, c).unwrap();
    ra.release(c);
    ra.define_value(inst_ref, a);
}
pub fn emit_vector_paired_add_signed_widen32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_scratch_xmm(&mut args[0]);
    if ctx.has_host_feature(HostFeature::AVX512_ORTHO) {
        let c = ra.scratch_xmm();
        ra.asm.vpsraq_imm(c, a, 32).unwrap();
        ra.asm.vpsllq_imm(a, a, 32).unwrap();
        ra.asm.vpsraq_imm(a, a, 32).unwrap();
        ra.asm.vpaddq(a, a, c).unwrap();
        ra.release(c);
    } else {
        let tmp1 = ra.scratch_xmm();
        let tmp2 = ra.scratch_xmm();
        let c = ra.scratch_xmm();
        let sign_addr = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0x8000_0000_0000_0000, 0x8000_0000_0000_0000);
        ra.asm.movdqa(c, a).unwrap();
        ra.asm.psllq_imm(a, 32).unwrap();
        ra.asm.movdqa(tmp1, rxbyak::xmmword_ptr(sign_addr)).unwrap();
        ra.asm.movdqa(tmp2, tmp1).unwrap();
        ra.asm.pand(tmp1, a).unwrap();
        ra.asm.pand(tmp2, c).unwrap();
        ra.asm.psrlq_imm(a, 32).unwrap();
        ra.asm.psrlq_imm(c, 32).unwrap();
        ra.asm.psrad_imm(tmp1, 31).unwrap();
        ra.asm.psrad_imm(tmp2, 31).unwrap();
        ra.asm.por(a, tmp1).unwrap();
        ra.asm.por(c, tmp2).unwrap();
        ra.asm.paddq(a, c).unwrap();
        ra.release(tmp1);
        ra.release(tmp2);
        ra.release(c);
    }
    ra.define_value(inst_ref, a);
}

// ---------------------------------------------------------------------------
// VectorPairedAddUnsignedWiden
// ---------------------------------------------------------------------------

pub fn emit_vector_paired_add_unsigned_widen8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_scratch_xmm(&mut args[0]);
    let c = ra.scratch_xmm();
    ra.asm.movdqa(c, a).unwrap();
    ra.asm.psllw_imm(a, 8).unwrap();
    ra.asm.psrlw_imm(c, 8).unwrap();
    ra.asm.psrlw_imm(a, 8).unwrap();
    ra.asm.paddw(a, c).unwrap();
    ra.release(c);
    ra.define_value(inst_ref, a);
}
pub fn emit_vector_paired_add_unsigned_widen16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_scratch_xmm(&mut args[0]);
    let c = ra.scratch_xmm();
    ra.asm.movdqa(c, a).unwrap();
    ra.asm.pslld_imm(a, 16).unwrap();
    ra.asm.psrld_imm(c, 16).unwrap();
    ra.asm.psrld_imm(a, 16).unwrap();
    ra.asm.paddd(a, c).unwrap();
    ra.release(c);
    ra.define_value(inst_ref, a);
}
pub fn emit_vector_paired_add_unsigned_widen32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_scratch_xmm(&mut args[0]);
    let c = ra.scratch_xmm();
    ra.asm.movdqa(c, a).unwrap();
    ra.asm.psllq_imm(a, 32).unwrap();
    ra.asm.psrlq_imm(c, 32).unwrap();
    ra.asm.psrlq_imm(a, 32).unwrap();
    ra.asm.paddq(a, c).unwrap();
    ra.release(c);
    ra.define_value(inst_ref, a);
}

// ---------------------------------------------------------------------------
// VectorPairedMax/Min — fallback
// ---------------------------------------------------------------------------

macro_rules! define_paired_minmax {
    ($name:ident, $ty:ty, $count:expr, $op:ident) => {
        extern "C" fn $name(result: *mut [u8; 16], a: *const [u8; 16], b: *const [u8; 16]) {
            unsafe {
                let va: [$ty; $count] = std::mem::transmute(*a);
                let vb: [$ty; $count] = std::mem::transmute(*b);
                let mut out = [0 as $ty; $count];
                let half = $count / 2;
                for i in 0..half {
                    out[i] = va[i * 2].$op(va[i * 2 + 1]);
                }
                for i in 0..half {
                    out[half + i] = vb[i * 2].$op(vb[i * 2 + 1]);
                }
                *result = std::mem::transmute(out);
            }
        }
    };
}

define_paired_minmax!(fallback_paired_max_s8, i8, 16, max);
define_paired_minmax!(fallback_paired_max_s16, i16, 8, max);
define_paired_minmax!(fallback_paired_max_s32, i32, 4, max);
define_paired_minmax!(fallback_paired_max_u8, u8, 16, max);
define_paired_minmax!(fallback_paired_max_u16, u16, 8, max);
define_paired_minmax!(fallback_paired_max_u32, u32, 4, max);
define_paired_minmax!(fallback_paired_min_s8, i8, 16, min);
define_paired_minmax!(fallback_paired_min_s16, i16, 8, min);
define_paired_minmax!(fallback_paired_min_s32, i32, 4, min);
define_paired_minmax!(fallback_paired_min_u8, u8, 16, min);
define_paired_minmax!(fallback_paired_min_u16, u16, 8, min);
define_paired_minmax!(fallback_paired_min_u32, u32, 4, min);

// D-form (64-bit) paired min/max. Mirrors upstream `LowerPairedOperation`
// (emit_x64_vector.cpp:2750-2761): pairs reduce HALF of each source vector
// (the lower 64 bits) into HALF of the destination, with upper destination
// lanes zeroed. For u8 (count=16): range=4 pairs per input → 8 output lanes
// set (4 from a + 4 from b), upper 8 zero. The previous implementation only
// emitted 2 output lanes (one pair per input), producing wrong results for
// AArch64 `umaxp/uminp v.8b, v.8b, v.8b` which libnx string functions and
// fsdev path handling rely on.
macro_rules! define_paired_minmax_lower {
    ($name:ident, $ty:ty, $count:expr, $func:ident) => {
        extern "C" fn $name(result: *mut [u8; 16], a: *const [u8; 16], b: *const [u8; 16]) {
            unsafe {
                let va: [$ty; $count] = std::mem::transmute(*a);
                let vb: [$ty; $count] = std::mem::transmute(*b);
                let mut out = [0 as $ty; $count];
                let range = $count / 4;
                for i in 0..range {
                    out[i] = std::cmp::$func(va[2 * i], va[2 * i + 1]);
                }
                for i in 0..range {
                    out[range + i] = std::cmp::$func(vb[2 * i], vb[2 * i + 1]);
                }
                *result = std::mem::transmute(out);
            }
        }
    };
}

define_paired_minmax_lower!(fallback_paired_max_lower_s8, i8, 16, max);
define_paired_minmax_lower!(fallback_paired_max_lower_s16, i16, 8, max);
define_paired_minmax_lower!(fallback_paired_max_lower_s32, i32, 4, max);
define_paired_minmax_lower!(fallback_paired_max_lower_u8, u8, 16, max);
define_paired_minmax_lower!(fallback_paired_max_lower_u16, u16, 8, max);
define_paired_minmax_lower!(fallback_paired_max_lower_u32, u32, 4, max);
define_paired_minmax_lower!(fallback_paired_min_lower_s8, i8, 16, min);
define_paired_minmax_lower!(fallback_paired_min_lower_s16, i16, 8, min);
define_paired_minmax_lower!(fallback_paired_min_lower_s32, i32, 4, min);
define_paired_minmax_lower!(fallback_paired_min_lower_u8, u8, 16, min);
define_paired_minmax_lower!(fallback_paired_min_lower_u16, u16, 8, min);
define_paired_minmax_lower!(fallback_paired_min_lower_u32, u32, 4, min);

pub fn emit_vector_paired_max_signed8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_paired_max_s8 as *const () as usize,
    );
}
pub fn emit_vector_paired_max_signed16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_paired_max_s16 as *const () as usize,
    );
}
pub fn emit_vector_paired_max_signed32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_paired_max_s32 as *const () as usize,
    );
}
pub fn emit_vector_paired_max_unsigned8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    if std::env::var_os("RUZU_FORCE_PAIRED_MAX_U8_FALLBACK").is_some() {
        emit_two_arg_fallback(
            ra,
            inst_ref,
            inst,
            fallback_paired_max_u8 as *const () as usize,
        );
        return;
    }
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let x = ra.use_scratch_xmm(&mut args[0]);
    let y = ra.use_scratch_xmm(&mut args[1]);
    let tmp = ra.scratch_xmm();

    let pool = ra.constant_pool.as_mut().expect("constant pool required");
    let shuffle_mask =
        pool.get_constant(0x0E_0C_0A_08_06_04_02_00u64, 0x0F_0D_0B_09_07_05_03_01u64);

    ra.asm.pshufb(x, rxbyak::xmmword_ptr(shuffle_mask)).unwrap();
    ra.asm.pshufb(y, rxbyak::xmmword_ptr(shuffle_mask)).unwrap();
    ra.asm.movaps(tmp, x).unwrap();
    ra.asm.shufps(tmp, y, 0b01_00_01_00).unwrap();
    ra.asm.shufps(x, y, 0b11_10_11_10).unwrap();
    ra.asm.pmaxub(x, tmp).unwrap();

    ra.release(tmp);
    ra.define_value(inst_ref, x);
}
pub fn emit_vector_paired_max_unsigned16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_paired_max_u16 as *const () as usize,
    );
}
pub fn emit_vector_paired_max_unsigned32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_paired_max_u32 as *const () as usize,
    );
}
pub fn emit_vector_paired_max_signed_lower8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_paired_max_lower_s8 as *const () as usize,
    );
}
pub fn emit_vector_paired_max_signed_lower16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_paired_max_lower_s16 as *const () as usize,
    );
}
pub fn emit_vector_paired_max_signed_lower32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_paired_max_lower_s32 as *const () as usize,
    );
}
pub fn emit_vector_paired_max_unsigned_lower8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_paired_max_lower_u8 as *const () as usize,
    );
}
pub fn emit_vector_paired_max_unsigned_lower16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_paired_max_lower_u16 as *const () as usize,
    );
}
pub fn emit_vector_paired_max_unsigned_lower32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_paired_max_lower_u32 as *const () as usize,
    );
}
pub fn emit_vector_paired_min_signed8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_paired_min_s8 as *const () as usize,
    );
}
pub fn emit_vector_paired_min_signed16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_paired_min_s16 as *const () as usize,
    );
}
pub fn emit_vector_paired_min_signed32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_paired_min_s32 as *const () as usize,
    );
}
pub fn emit_vector_paired_min_unsigned8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_paired_min_u8 as *const () as usize,
    );
}
pub fn emit_vector_paired_min_unsigned16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_paired_min_u16 as *const () as usize,
    );
}
pub fn emit_vector_paired_min_unsigned32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_paired_min_u32 as *const () as usize,
    );
}
pub fn emit_vector_paired_min_signed_lower8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_paired_min_lower_s8 as *const () as usize,
    );
}
pub fn emit_vector_paired_min_signed_lower16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_paired_min_lower_s16 as *const () as usize,
    );
}
pub fn emit_vector_paired_min_signed_lower32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_paired_min_lower_s32 as *const () as usize,
    );
}
pub fn emit_vector_paired_min_unsigned_lower8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_paired_min_lower_u8 as *const () as usize,
    );
}
pub fn emit_vector_paired_min_unsigned_lower16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_paired_min_lower_u16 as *const () as usize,
    );
}
pub fn emit_vector_paired_min_unsigned_lower32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_paired_min_lower_u32 as *const () as usize,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fn_signatures() {
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_multiply8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_multiply16;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_multiply_signed_widen8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_multiply_unsigned_widen32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_polynomial_multiply8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_paired_add8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_paired_add_lower32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_paired_add_signed_widen32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_paired_max_signed8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_paired_max_signed_lower8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_paired_min_unsigned32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_paired_min_unsigned_lower32;
    }

    // Test removed: fallback_multiply8 replaced with inline SSE (pmullw trick)
    // Correctness verified via a32_diff fuzzing

    #[test]
    fn fallback_paired_max_lower_u8_matches_upstream_lower_paired_operation() {
        // AArch64 `umaxp v.8b, v.8b, v.8b`: pairs the lower 64 bits of each
        // input. Upstream (emit_x64_vector.cpp:2750) uses range = count/4 = 4,
        // producing 4 pairs from `a` then 4 pairs from `b` in the lower 64
        // bits of the output, with the upper 64 bits zero.
        let mut a = [0u8; 16];
        let mut b = [0u8; 16];
        // a lower 8: 0x10..0x17
        for i in 0..8 {
            a[i] = 0x10 + i as u8;
        }
        // b lower 8: 0xA0..0xA7
        for i in 0..8 {
            b[i] = 0xA0 + i as u8;
        }
        let mut out = [0u8; 16];
        fallback_paired_max_lower_u8(&mut out, &a, &b);
        // Pairs from a: max(0x10,0x11)=0x11, max(0x12,0x13)=0x13,
        //               max(0x14,0x15)=0x15, max(0x16,0x17)=0x17
        // Pairs from b: max(0xA0,0xA1)=0xA1, ..., max(0xA6,0xA7)=0xA7
        let expected: [u8; 16] = [
            0x11, 0x13, 0x15, 0x17, 0xA1, 0xA3, 0xA5, 0xA7, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(
            out, expected,
            "umaxp v.8b mismatch — only first 2 lanes set means the broken pre-fix implementation"
        );
    }

    #[test]
    fn fallback_paired_min_lower_u8_matches_upstream_lower_paired_operation() {
        let mut a = [0u8; 16];
        let mut b = [0u8; 16];
        for i in 0..8 {
            a[i] = 0x10 + i as u8;
        }
        for i in 0..8 {
            b[i] = 0xA0 + i as u8;
        }
        let mut out = [0u8; 16];
        fallback_paired_min_lower_u8(&mut out, &a, &b);
        let expected: [u8; 16] = [
            0x10, 0x12, 0x14, 0x16, 0xA0, 0xA2, 0xA4, 0xA6, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(out, expected);
    }

    #[test]
    fn fallback_paired_max_lower_u16_produces_two_pairs_per_input() {
        // For u16 (count=8): range = 8/4 = 2 pairs from each.
        let mut a_bytes = [0u8; 16];
        let mut b_bytes = [0u8; 16];
        // a as u16 lower 4: [0x0001, 0x0002, 0x0003, 0x0004]
        let a_words: [u16; 4] = [0x0001, 0x0002, 0x0003, 0x0004];
        let b_words: [u16; 4] = [0x00A0, 0x00A1, 0x00A2, 0x00A3];
        a_bytes[..8].copy_from_slice(&unsafe { std::mem::transmute::<_, [u8; 8]>(a_words) });
        b_bytes[..8].copy_from_slice(&unsafe { std::mem::transmute::<_, [u8; 8]>(b_words) });
        let mut out = [0u8; 16];
        fallback_paired_max_lower_u16(&mut out, &a_bytes, &b_bytes);
        let out_words: [u16; 8] = unsafe { std::mem::transmute(out) };
        assert_eq!(
            out_words,
            [0x0002, 0x0004, 0x00A1, 0x00A3, 0, 0, 0, 0],
            "umaxp v.4h must produce 2 pairs from a then 2 pairs from b"
        );
    }
}
