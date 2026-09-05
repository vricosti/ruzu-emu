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
// VectorAdd — native SSE: paddb/paddw/paddd/paddq
// ---------------------------------------------------------------------------

pub fn emit_vector_add8(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::paddb);
}
pub fn emit_vector_add16(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::paddw);
}
pub fn emit_vector_add32(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::paddd);
}
pub fn emit_vector_add64(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::paddq);
}

// ---------------------------------------------------------------------------
// VectorSub — native SSE: psubb/psubw/psubd/psubq
// ---------------------------------------------------------------------------

pub fn emit_vector_sub8(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::psubb);
}
pub fn emit_vector_sub16(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::psubw);
}
pub fn emit_vector_sub32(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::psubd);
}
pub fn emit_vector_sub64(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::psubq);
}

// ---------------------------------------------------------------------------
// Logical — native SSE: pand/pandn/por/pxor
// ---------------------------------------------------------------------------

pub fn emit_vector_and(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pand);
}
pub fn emit_vector_and_not(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_xmm(&mut args[0]);
    let result = ra.use_scratch_xmm(&mut args[1]);
    rxbyak::CodeAssembler::pandn(&mut *ra.asm, result, a).unwrap();
    ra.define_value(inst_ref, result);
}
pub fn emit_vector_or(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::por);
}
pub fn emit_vector_eor(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pxor);
}

// ---------------------------------------------------------------------------
// VectorNot — pcmpeqd(tmp,tmp) to get all-ones, then pxor
// ---------------------------------------------------------------------------

pub fn emit_vector_not(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let ones = ra.scratch_xmm();
    ra.asm.pcmpeqd(ones, ones).unwrap();
    ra.asm.pxor(result, ones).unwrap();
    ra.release(ones);
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// VectorAbs — native SSSE3: pabsb/pabsw/pabsd
// VectorAbs64 — fallback (no pabsq in SSE)
// ---------------------------------------------------------------------------

pub fn emit_vector_abs8(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_vector_unary_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pabsb);
}
pub fn emit_vector_abs16(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_vector_unary_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pabsw);
}
pub fn emit_vector_abs32(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_vector_unary_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pabsd);
}

// VectorAbs64: no pabsq in SSE; use pxor+psubq+blendvpd
// Upstream: zero = pxor; neg = psubq(zero, data); mask = pcmpgtq(data, zero); blendvpd(neg, data, mask)
// Wait — upstream SSE4.1: pxor(zero); psubq(tmp=zero, data); pcmpgtq(zero2, data) for negative detection
// Simpler approach: sign = psrad(data, 31); pshufd with sign in 31 → mask; pxor + psubq
pub fn emit_vector_abs64(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let data = ra.use_xmm(&mut args[0]);
    let result = ra.scratch_xmm();
    let zero = ra.scratch_xmm();
    let neg = ra.scratch_xmm();
    // zero = 0
    ra.asm.xorps(zero, zero).unwrap();
    // neg = 0 - data = two's complement negation
    ra.asm.movaps(neg, zero).unwrap();
    ra.asm.psubq(neg, data).unwrap();
    // mask where data >= 0: pcmpgtq(zero, data) gives 1s where 0 > data (i.e., data negative)
    // We want: where data >= 0 pick data, else pick neg
    // XMM0 = mask of negative elements (where zero > data)
    ra.asm.movaps(rxbyak::XMM0, zero).unwrap();
    ra.asm.pcmpgtq(rxbyak::XMM0, data).unwrap();
    // result = data; blendvpd(data, neg, XMM0): where negative, pick neg
    ra.asm.movaps(result, data).unwrap();
    ra.asm.blendvpd(result, neg).unwrap();
    ra.release(zero);
    ra.release(neg);
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// ZeroVector — xorps
// ---------------------------------------------------------------------------

pub fn emit_zero_vector(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, _inst: &Inst) {
    let result = ra.scratch_xmm();
    ra.asm.xorps(result, result).unwrap();
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// VectorZeroUpper — zero upper 64 bits, keep lower 64
// movq dst, src (SSE2 form: loads low 64, zeros high)
// ---------------------------------------------------------------------------

pub fn emit_vector_zero_upper(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let src = ra.use_xmm(&mut args[0]);
    let result = ra.scratch_xmm();
    ra.asm.movq(result, src).unwrap();
    ra.release(src);
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// VectorCountLeadingZeros8/16/32
// ---------------------------------------------------------------------------

extern "C" fn fallback_vector_clz8(result: *mut [u8; 16], a: *const [u8; 16]) {
    unsafe {
        let src = &*a;
        let dst = &mut *result;
        for i in 0..16 {
            dst[i] = src[i].leading_zeros() as u8;
        }
    }
}

extern "C" fn fallback_vector_clz16(result: *mut [u8; 16], a: *const [u8; 16]) {
    unsafe {
        let src: [u16; 8] = std::mem::transmute(*a);
        let mut out = [0u16; 8];
        for i in 0..8 {
            out[i] = src[i].leading_zeros() as u16;
        }
        *result = std::mem::transmute(out);
    }
}

pub fn emit_vector_clz8(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    if ctx.has_host_feature(HostFeature::GFNI) {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let data = ra.use_scratch_xmm(&mut args[0]);
        let result = ra.scratch_xmm();
        let reverse_matrix = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0x8040_2010_0804_0201, 0x8040_2010_0804_0201);
        ra.asm
            .gf2p8affineqb(data, rxbyak::xmmword_ptr(reverse_matrix), 0)
            .unwrap();
        ra.asm.pcmpeqb(result, result).unwrap();
        ra.asm.paddb(result, data).unwrap();
        ra.asm.pandn(result, data).unwrap();
        let index_matrix = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0xaacc_f0ff_0000_0000, 0xaacc_f0ff_0000_0000);
        ra.asm
            .gf2p8affineqb(result, rxbyak::xmmword_ptr(index_matrix), 8)
            .unwrap();
        ra.define_value(inst_ref, result);
    } else if ctx.has_host_feature(HostFeature::SSSE3) {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let data = ra.use_scratch_xmm(&mut args[0]);
        let tmp1 = ra.scratch_xmm();
        let tmp2 = ra.scratch_xmm();
        let lookup = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0x0101_0101_0202_0304, 0);
        ra.asm.movdqa(tmp1, rxbyak::xmmword_ptr(lookup)).unwrap();
        ra.asm.movdqa(tmp2, tmp1).unwrap();
        ra.asm.pshufb(tmp2, data).unwrap();
        ra.asm.psrlw_imm(data, 4).unwrap();
        let nibble_mask = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0x0f0f_0f0f_0f0f_0f0f, 0x0f0f_0f0f_0f0f_0f0f);
        ra.asm.pand(data, rxbyak::xmmword_ptr(nibble_mask)).unwrap();
        ra.asm.pshufb(tmp1, data).unwrap();
        let fours = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0x0404_0404_0404_0404, 0x0404_0404_0404_0404);
        ra.asm.movdqa(data, rxbyak::xmmword_ptr(fours)).unwrap();
        ra.asm.pcmpeqb(data, tmp1).unwrap();
        ra.asm.pand(data, tmp2).unwrap();
        ra.asm.paddb(data, tmp1).unwrap();
        ra.release(tmp1);
        ra.release(tmp2);
        ra.define_value(inst_ref, data);
    } else {
        emit_one_arg_fallback(
            ra,
            inst_ref,
            inst,
            fallback_vector_clz8 as *const () as usize,
        );
    }
}

pub fn emit_vector_clz16(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    if ctx.has_host_feature(HostFeature::AVX512_ORTHO | HostFeature::AVX512CD) {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let data = ra.use_scratch_xmm(&mut args[0]);
        let zero = ra.scratch_xmm();
        let wide = data.cvt256().unwrap();
        ra.asm.vpmovzxwd(wide, data).unwrap();
        ra.asm.vplzcntd(wide, wide).unwrap();
        ra.asm.vpxor(zero, zero, zero).unwrap();
        ra.asm.vpackusdw(data, data, zero).unwrap();
        let subtract_sixteen = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0xfff0_fff0_fff0_fff0, 0xfff0_fff0_fff0_fff0);
        ra.asm
            .vpaddw(data, data, rxbyak::xmmword_ptr(subtract_sixteen))
            .unwrap();
        ra.asm.vzeroupper().unwrap();
        ra.release(zero);
        ra.define_value(inst_ref, data);
    } else if ctx.has_host_feature(HostFeature::AVX) {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let data = ra.use_scratch_xmm(&mut args[0]);
        let result = ra.scratch_xmm();
        let zeros = ra.scratch_xmm();
        let tmp = ra.scratch_xmm();
        ra.asm.vpsrlw_imm(tmp, data, 1).unwrap();
        ra.asm.vpor(data, data, tmp).unwrap();
        ra.asm.vpsrlw_imm(tmp, data, 2).unwrap();
        ra.asm.vpor(data, data, tmp).unwrap();
        ra.asm.vpsrlw_imm(tmp, data, 4).unwrap();
        ra.asm.vpor(data, data, tmp).unwrap();
        ra.asm.vpsrlw_imm(tmp, data, 8).unwrap();
        ra.asm.vpor(data, data, tmp).unwrap();
        ra.asm.vpcmpeqw(zeros, zeros, zeros).unwrap();
        ra.asm.vpcmpeqw(tmp, tmp, tmp).unwrap();
        ra.asm.vpcmpeqw(zeros, zeros, data).unwrap();
        let multiplier = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0xf0d3_f0d3_f0d3_f0d3, 0xf0d3_f0d3_f0d3_f0d3);
        ra.asm
            .vpmullw(data, data, rxbyak::xmmword_ptr(multiplier))
            .unwrap();
        ra.asm.vpsllw_imm(tmp, tmp, 15).unwrap();
        ra.asm.vpsllw_imm(zeros, zeros, 7).unwrap();
        ra.asm.vpsrlw_imm(data, data, 12).unwrap();
        let lookup = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0x0903_060a_040b_0c10, 0x0f08_0e02_0705_0d01);
        ra.asm.vmovdqa(result, rxbyak::xmmword_ptr(lookup)).unwrap();
        ra.asm.vpor(tmp, tmp, zeros).unwrap();
        ra.asm.vpor(data, data, tmp).unwrap();
        ra.asm.vpshufb(result, result, data).unwrap();
        ra.release(zeros);
        ra.release(tmp);
        ra.define_value(inst_ref, result);
    } else if ctx.has_host_feature(HostFeature::SSSE3) {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let data = ra.use_scratch_xmm(&mut args[0]);
        let result = ra.scratch_xmm();
        let zeros = ra.scratch_xmm();
        let tmp = ra.scratch_xmm();
        ra.asm.movdqa(tmp, data).unwrap();
        ra.asm.psrlw_imm(tmp, 1).unwrap();
        ra.asm.por(data, tmp).unwrap();
        ra.asm.movdqa(tmp, data).unwrap();
        ra.asm.psrlw_imm(tmp, 2).unwrap();
        ra.asm.por(data, tmp).unwrap();
        ra.asm.movdqa(tmp, data).unwrap();
        ra.asm.psrlw_imm(tmp, 4).unwrap();
        ra.asm.por(data, tmp).unwrap();
        ra.asm.movdqa(tmp, data).unwrap();
        ra.asm.psrlw_imm(tmp, 8).unwrap();
        ra.asm.por(data, tmp).unwrap();
        ra.asm.pcmpeqw(zeros, zeros).unwrap();
        ra.asm.pcmpeqw(tmp, tmp).unwrap();
        ra.asm.pcmpeqw(zeros, data).unwrap();
        let multiplier = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0xf0d3_f0d3_f0d3_f0d3, 0xf0d3_f0d3_f0d3_f0d3);
        ra.asm
            .pmullw(data, rxbyak::xmmword_ptr(multiplier))
            .unwrap();
        ra.asm.psllw_imm(tmp, 15).unwrap();
        ra.asm.psllw_imm(zeros, 7).unwrap();
        ra.asm.psrlw_imm(data, 12).unwrap();
        let lookup = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0x0903_060a_040b_0c10, 0x0f08_0e02_0705_0d01);
        ra.asm.movdqa(result, rxbyak::xmmword_ptr(lookup)).unwrap();
        ra.asm.por(tmp, zeros).unwrap();
        ra.asm.por(data, tmp).unwrap();
        ra.asm.pshufb(result, data).unwrap();
        ra.release(zeros);
        ra.release(tmp);
        ra.define_value(inst_ref, result);
    } else {
        emit_one_arg_fallback(
            ra,
            inst_ref,
            inst,
            fallback_vector_clz16 as *const () as usize,
        );
    }
}

pub fn emit_vector_clz32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    if ctx.has_host_feature(HostFeature::AVX512_ORTHO | HostFeature::AVX512CD) {
        let data = ra.use_scratch_xmm(&mut args[0]);
        ra.asm.vplzcntd(data, data).unwrap();
        ra.define_value(inst_ref, data);
    } else if ctx.has_host_feature(HostFeature::AVX2) {
        let data = ra.use_scratch_xmm(&mut args[0]);
        let temp = ra.scratch_xmm();
        ra.asm.vmovdqa(temp, data).unwrap();
        ra.asm.vpsrld_imm(data, data, 8).unwrap();
        ra.asm.vpandn(data, data, temp).unwrap();
        let exponent = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0x0000_009e_0000_009e, 0x0000_009e_0000_009e);
        ra.asm.vmovdqa(temp, rxbyak::xmmword_ptr(exponent)).unwrap();
        ra.asm.vcvtdq2ps(data, data).unwrap();
        ra.asm.vpsrld_imm(data, data, 23).unwrap();
        ra.asm.vpsubusw(data, temp, data).unwrap();
        let thirty_two = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0x0000_0020_0000_0020, 0x0000_0020_0000_0020);
        ra.asm
            .vpminsw(data, data, rxbyak::xmmword_ptr(thirty_two))
            .unwrap();
        ra.release(temp);
        ra.define_value(inst_ref, data);
    } else {
        let result = ra.use_scratch_xmm(&mut args[0]);
        let tmp1 = ra.scratch_xmm();
        let tmp2 = ra.scratch_xmm();
        ra.asm.pxor(tmp1, tmp1).unwrap();
        ra.asm.movdqa(tmp2, result).unwrap();
        ra.asm.pcmpeqd(tmp1, result).unwrap();
        ra.asm.psrld_imm(result, 1).unwrap();
        ra.asm.psrld_imm(tmp2, 2).unwrap();
        ra.asm.pandn(tmp2, result).unwrap();
        ra.asm.cvtdq2ps(result, tmp2).unwrap();
        ra.asm.addps(result, result).unwrap();
        let one = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0x3f80_0000_3f80_0000, 0x3f80_0000_3f80_0000);
        ra.asm.addps(result, rxbyak::xmmword_ptr(one)).unwrap();
        ra.asm.psrld_imm(result, 23).unwrap();
        ra.asm.paddd(tmp1, result).unwrap();
        let exponent = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0x0000_009e_0000_009e, 0x0000_009e_0000_009e);
        ra.asm
            .movdqa(result, rxbyak::xmmword_ptr(exponent))
            .unwrap();
        ra.asm.psubd(result, tmp1).unwrap();
        ra.release(tmp1);
        ra.release(tmp2);
        ra.define_value(inst_ref, result);
    }
}

// ---------------------------------------------------------------------------
// VectorPopulationCount
// ---------------------------------------------------------------------------

extern "C" fn fallback_vector_popcount(result: *mut [u8; 16], a: *const [u8; 16]) {
    unsafe {
        let src = &*a;
        let dst = &mut *result;
        for i in 0..16 {
            dst[i] = src[i].count_ones() as u8;
        }
    }
}

pub fn emit_vector_popcount(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    if ctx.has_host_feature(HostFeature::AVX512VL | HostFeature::AVX512BITALG) {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let data = ra.use_scratch_xmm(&mut args[0]);
        ra.asm.vpopcntb(data, data).unwrap();
        ra.define_value(inst_ref, data);
        return;
    }

    if ctx.has_host_feature(HostFeature::SSSE3) {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let low = ra.use_scratch_xmm(&mut args[0]);
        let high = ra.scratch_xmm();
        let tmp1 = ra.scratch_xmm();
        let tmp2 = ra.scratch_xmm();
        ra.asm.movdqa(high, low).unwrap();
        ra.asm.psrlw_imm(high, 4).unwrap();
        let nibble_mask = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0x0f0f_0f0f_0f0f_0f0f, 0x0f0f_0f0f_0f0f_0f0f);
        ra.asm
            .movdqa(tmp1, rxbyak::xmmword_ptr(nibble_mask))
            .unwrap();
        ra.asm.pand(high, tmp1).unwrap();
        ra.asm.pand(low, tmp1).unwrap();
        let lookup = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0x0302_0201_0201_0100, 0x0403_0302_0302_0201);
        ra.asm.movdqa(tmp1, rxbyak::xmmword_ptr(lookup)).unwrap();
        ra.asm.movdqa(tmp2, tmp1).unwrap();
        ra.asm.pshufb(tmp1, low).unwrap();
        ra.asm.pshufb(tmp2, high).unwrap();
        ra.asm.paddb(tmp1, tmp2).unwrap();
        ra.release(high);
        ra.release(tmp2);
        ra.define_value(inst_ref, tmp1);
        return;
    }

    emit_one_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_vector_popcount as *const () as usize,
    );
}

// ---------------------------------------------------------------------------
// VectorReverseBits
// ---------------------------------------------------------------------------

pub fn emit_vector_reverse_bits(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let data = ra.use_scratch_xmm(&mut args[0]);
    if ctx.has_host_feature(HostFeature::GFNI) {
        let reverse_matrix = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0x8040_2010_0804_0201, 0x8040_2010_0804_0201);
        ra.asm
            .gf2p8affineqb(data, rxbyak::xmmword_ptr(reverse_matrix), 0)
            .unwrap();
    } else {
        let high_nibble = ra.scratch_xmm();
        let high_mask = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0xf0f0_f0f0_f0f0_f0f0, 0xf0f0_f0f0_f0f0_f0f0);
        ra.asm
            .movdqa(high_nibble, rxbyak::xmmword_ptr(high_mask))
            .unwrap();
        ra.asm.pand(high_nibble, data).unwrap();
        ra.asm.pxor(data, high_nibble).unwrap();
        ra.asm.psrld_imm(high_nibble, 4).unwrap();

        if ctx.has_host_feature(HostFeature::SSSE3) {
            let high_reversed = ra.scratch_xmm();
            let high_lookup = ra
                .constant_pool
                .as_mut()
                .expect("constant pool required")
                .get_constant(0xe060_a020_c040_8000, 0xf070_b030_d050_9010);
            ra.asm
                .movdqa(high_reversed, rxbyak::xmmword_ptr(high_lookup))
                .unwrap();
            ra.asm.pshufb(high_reversed, data).unwrap();
            let low_lookup = ra
                .constant_pool
                .as_mut()
                .expect("constant pool required")
                .get_constant(0x0e06_0a02_0c04_0800, 0x0f07_0b03_0d05_0901);
            ra.asm
                .movdqa(data, rxbyak::xmmword_ptr(low_lookup))
                .unwrap();
            ra.asm.pshufb(data, high_nibble).unwrap();
            ra.asm.por(data, high_reversed).unwrap();
            ra.release(high_reversed);
        } else {
            ra.asm.pslld_imm(data, 4).unwrap();
            ra.asm.por(data, high_nibble).unwrap();
            let pairs = ra
                .constant_pool
                .as_mut()
                .expect("constant pool required")
                .get_constant(0xcccc_cccc_cccc_cccc, 0xcccc_cccc_cccc_cccc);
            ra.asm
                .movdqa(high_nibble, rxbyak::xmmword_ptr(pairs))
                .unwrap();
            ra.asm.pand(high_nibble, data).unwrap();
            ra.asm.pxor(data, high_nibble).unwrap();
            ra.asm.psrld_imm(high_nibble, 2).unwrap();
            ra.asm.pslld_imm(data, 2).unwrap();
            ra.asm.por(data, high_nibble).unwrap();
            let alternating = ra
                .constant_pool
                .as_mut()
                .expect("constant pool required")
                .get_constant(0xaaaa_aaaa_aaaa_aaaa, 0xaaaa_aaaa_aaaa_aaaa);
            ra.asm
                .movdqa(high_nibble, rxbyak::xmmword_ptr(alternating))
                .unwrap();
            ra.asm.pand(high_nibble, data).unwrap();
            ra.asm.pxor(data, high_nibble).unwrap();
            ra.asm.psrld_imm(high_nibble, 1).unwrap();
            ra.asm.paddd(data, data).unwrap();
            ra.asm.por(data, high_nibble).unwrap();
        }
        ra.release(high_nibble);
    }
    ra.define_value(inst_ref, data);
}

// ---------------------------------------------------------------------------
// VectorReverseElementsIn*Groups* — native SSE, matching emit_x64_vector.cpp
// ---------------------------------------------------------------------------

pub fn emit_vector_reverse_half_groups_8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let data = ra.use_scratch_xmm(&mut args[0]);
    let tmp = ra.scratch_xmm();
    ra.asm.movdqa(tmp, data).unwrap();
    ra.asm.psllw_imm(tmp, 8).unwrap();
    ra.asm.psrlw_imm(data, 8).unwrap();
    ra.asm.por(data, tmp).unwrap();
    ra.release(tmp);
    ra.define_value(inst_ref, data);
}

pub fn emit_vector_reverse_word_groups_8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let data = ra.use_scratch_xmm(&mut args[0]);
    let tmp = ra.scratch_xmm();
    ra.asm.movdqa(tmp, data).unwrap();
    ra.asm.psllw_imm(tmp, 8).unwrap();
    ra.asm.psrlw_imm(data, 8).unwrap();
    ra.asm.por(data, tmp).unwrap();
    ra.asm.pshuflw(data, data, 0b1011_0001).unwrap();
    ra.asm.pshufhw(data, data, 0b1011_0001).unwrap();
    ra.release(tmp);
    ra.define_value(inst_ref, data);
}

pub fn emit_vector_reverse_word_groups_16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let data = ra.use_scratch_xmm(&mut args[0]);
    ra.asm.pshuflw(data, data, 0b1011_0001).unwrap();
    ra.asm.pshufhw(data, data, 0b1011_0001).unwrap();
    ra.define_value(inst_ref, data);
}

pub fn emit_vector_reverse_long_groups_8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let data = ra.use_scratch_xmm(&mut args[0]);
    let tmp = ra.scratch_xmm();
    ra.asm.movdqa(tmp, data).unwrap();
    ra.asm.psllw_imm(tmp, 8).unwrap();
    ra.asm.psrlw_imm(data, 8).unwrap();
    ra.asm.por(data, tmp).unwrap();
    ra.asm.pshuflw(data, data, 0b0001_1011).unwrap();
    ra.asm.pshufhw(data, data, 0b0001_1011).unwrap();
    ra.release(tmp);
    ra.define_value(inst_ref, data);
}

pub fn emit_vector_reverse_long_groups_16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let data = ra.use_scratch_xmm(&mut args[0]);
    ra.asm.pshuflw(data, data, 0b0001_1011).unwrap();
    ra.asm.pshufhw(data, data, 0b0001_1011).unwrap();
    ra.define_value(inst_ref, data);
}

pub fn emit_vector_reverse_long_groups_32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let data = ra.use_scratch_xmm(&mut args[0]);
    ra.asm.pshuflw(data, data, 0b0100_1110).unwrap();
    ra.asm.pshufhw(data, data, 0b0100_1110).unwrap();
    ra.define_value(inst_ref, data);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fn_signatures() {
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_add8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_sub64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_and;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_not;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_abs8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_abs64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_zero_vector;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_zero_upper;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_clz8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_popcount;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_reverse_bits;
    }

    #[test]
    fn test_fallback_vector_clz8() {
        for base in (0..=u8::MAX).step_by(16) {
            let input = std::array::from_fn(|lane| base.wrapping_add(lane as u8));
            let mut output = [0u8; 16];
            fallback_vector_clz8(&mut output, &input);
            assert_eq!(output, input.map(|value| value.leading_zeros() as u8));
        }
    }

    #[test]
    fn test_fallback_vector_clz16() {
        let input: [u16; 8] = [0, 1, 2, 0x7f, 0x80, 0xff, 0x8000, 0xffff];
        let input_bytes: [u8; 16] = unsafe { std::mem::transmute(input) };
        let mut output_bytes = [0u8; 16];
        fallback_vector_clz16(&mut output_bytes, &input_bytes);
        let output: [u16; 8] = unsafe { std::mem::transmute(output_bytes) };
        assert_eq!(output, input.map(|value| value.leading_zeros() as u16));
    }

    #[test]
    fn test_fallback_vector_popcount() {
        for base in (0..=u8::MAX).step_by(16) {
            let input = std::array::from_fn(|lane| base.wrapping_add(lane as u8));
            let mut output = [0u8; 16];
            fallback_vector_popcount(&mut output, &input);
            assert_eq!(output, input.map(|value| value.count_ones() as u8));
        }
    }
}
