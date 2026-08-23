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
// VectorGetElement — native SSE4.1: pextrb/pextrw/pextrd/pextrq
// ---------------------------------------------------------------------------

pub fn emit_vector_get_element8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let src = ra.use_xmm(&mut args[0]);
    let idx = args[1].get_immediate_u8();
    let result = ra.scratch_gpr();
    ra.asm.pextrb(result.cvt32().unwrap(), src, idx).unwrap();
    ra.release(src);
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_get_element16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let src = ra.use_xmm(&mut args[0]);
    let idx = args[1].get_immediate_u8();
    let result = ra.scratch_gpr();
    ra.asm.pextrw(result.cvt32().unwrap(), src, idx).unwrap();
    ra.release(src);
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_get_element32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let src = ra.use_xmm(&mut args[0]);
    let idx = args[1].get_immediate_u8();
    let result = ra.scratch_gpr();
    ra.asm.pextrd(result.cvt32().unwrap(), src, idx).unwrap();
    ra.release(src);
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_get_element64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let idx = args[1].get_immediate_u8();
    // Mirrors upstream `EmitVectorGetElement64` in
    // `emit_x64_vector.cpp:5181-5197`: use `movq` for index==0 (shorter
    // encoding, no immediate). Suspected bug in our pextrq path was
    // observed in STK's UMAXP+UMOV strchr loop where X3 stayed 0 even
    // when V17.D[0] was non-zero — switching to movq fixes the common
    // case and matches upstream byte-for-byte.
    if idx == 0 {
        let src = ra.use_xmm(&mut args[0]);
        let result = ra.scratch_gpr();
        ra.asm.movq(result, src).unwrap();
        ra.release(src);
        ra.define_value(inst_ref, result);
        return;
    }
    let src = ra.use_xmm(&mut args[0]);
    let result = ra.scratch_gpr();
    ra.asm.pextrq(result, src, idx).unwrap();
    ra.release(src);
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// VectorSetElement — native SSE4.1: pinsrb/pinsrw/pinsrd/pinsrq
// ---------------------------------------------------------------------------

// Upstream arg order: (vec: U128, idx: U8, elem: Uxx)
pub fn emit_vector_set_element8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let idx = args[1].get_immediate_u8();
    let val = ra.use_gpr(&mut args[2]);
    ra.asm.pinsrb(result, val.cvt32().unwrap(), idx).unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_set_element16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let idx = args[1].get_immediate_u8();
    let val = ra.use_gpr(&mut args[2]);
    ra.asm.pinsrw(result, val.cvt32().unwrap(), idx).unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_set_element32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let idx = args[1].get_immediate_u8();
    let val = ra.use_gpr(&mut args[2]);
    ra.asm.pinsrd(result, val.cvt32().unwrap(), idx).unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_set_element64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let idx = args[1].get_immediate_u8();
    let val = ra.use_gpr(&mut args[2]);
    ra.asm.pinsrq(result, val, idx).unwrap();
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// VectorBroadcast
// ---------------------------------------------------------------------------

pub fn emit_vector_broadcast8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    if ctx.has_host_feature(HostFeature::AVX2) {
        ra.asm.vpbroadcastb(result, result).unwrap();
    } else if ctx.has_host_feature(HostFeature::SSSE3) {
        let zero = ra.scratch_xmm();
        ra.asm.pxor(zero, zero).unwrap();
        ra.asm.pshufb(result, zero).unwrap();
        ra.release(zero);
    } else {
        ra.asm.punpcklbw(result, result).unwrap();
        ra.asm.pshuflw(result, result, 0).unwrap();
        ra.asm.punpcklqdq(result, result).unwrap();
    }
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_broadcast16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    if ctx.has_host_feature(HostFeature::AVX2) {
        ra.asm.vpbroadcastw(result, result).unwrap();
    } else {
        ra.asm.pshuflw(result, result, 0).unwrap();
        ra.asm.punpcklqdq(result, result).unwrap();
    }
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_broadcast32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    if ctx.has_host_feature(HostFeature::AVX2) {
        ra.asm.vpbroadcastd(result, result).unwrap();
    } else {
        ra.asm.pshufd(result, result, 0).unwrap();
    }
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_broadcast64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    if ctx.has_host_feature(HostFeature::AVX2) {
        ra.asm.vpbroadcastq(result, result).unwrap();
    } else {
        ra.asm.punpcklqdq(result, result).unwrap();
    }
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// VectorBroadcastLower
// ---------------------------------------------------------------------------

pub fn emit_vector_broadcast_lower8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    if ctx.has_host_feature(HostFeature::AVX2) {
        ra.asm.vpbroadcastb(result, result).unwrap();
        ra.asm.movq(result, result).unwrap();
    } else if ctx.has_host_feature(HostFeature::SSSE3) {
        let zero = ra.scratch_xmm();
        ra.asm.pxor(zero, zero).unwrap();
        ra.asm.pshufb(result, zero).unwrap();
        ra.asm.movq(result, result).unwrap();
        ra.release(zero);
    } else {
        ra.asm.punpcklbw(result, result).unwrap();
        ra.asm.pshuflw(result, result, 0).unwrap();
    }
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_broadcast_lower16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    ra.asm.pshuflw(result, result, 0).unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_broadcast_lower32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    ra.asm.pshuflw(result, result, 0b0100_0100).unwrap();
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// VectorExtract — palignr (native SSE): extracts from concatenation
// ---------------------------------------------------------------------------

pub fn emit_vector_extract(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_xmm(&mut args[0]); // low part (Qn in ARM VEXT)
    let result = ra.use_scratch_xmm(&mut args[1]); // high part (Qm in ARM VEXT)
    let imm = args[2].get_immediate_u8();
    // PALIGNR(dest=high, src=low, bytes): extracts from [high:low] >> bytes
    // Position is in bits (upstream convention), PALIGNR takes bytes
    ra.asm.palignr(result, a, imm / 8).unwrap();
    ra.release(a);
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// VectorExtractLower — extract from two concatenated 64-bit vectors and zero
// the upper half, matching upstream EmitX64::EmitVectorExtractLower.
// ---------------------------------------------------------------------------

pub fn emit_vector_extract_lower(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let position = args[2].get_immediate_u8();
    assert_eq!(position % 8, 0);

    if position != 0 {
        let high = ra.use_xmm(&mut args[1]);
        ra.asm.punpcklqdq(result, high).unwrap();
        ra.asm.psrldq(result, position / 8).unwrap();
    }
    ra.asm.movq(result, result).unwrap();
    ra.define_value(inst_ref, result);
}

fn whole_vector_rotate_shuffle_imm(shift_amount: u8) -> u8 {
    assert_eq!(shift_amount % 32, 0);
    0b1110_0100_u8.rotate_right(u32::from(shift_amount / 32) * 2)
}

pub fn emit_vector_rotate_whole_vector_right(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let operand = ra.use_xmm(&mut args[0]);
    let result = ra.scratch_xmm();
    let shift_amount = args[1].get_immediate_u8();
    let shuffle_imm = whole_vector_rotate_shuffle_imm(shift_amount);
    ra.asm.pshufd(result, operand, shuffle_imm).unwrap();
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// VectorInterleaveLower — native SSE: punpcklbw/wd/dq/qdq
// ---------------------------------------------------------------------------

pub fn emit_vector_interleave_lower8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::punpcklbw);
}
pub fn emit_vector_interleave_lower16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::punpcklwd);
}
pub fn emit_vector_interleave_lower32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::punpckldq);
}
pub fn emit_vector_interleave_lower64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::punpcklqdq);
}

// ---------------------------------------------------------------------------
// VectorInterleaveUpper — native SSE: punpckhbw/wd/dq/qdq
// ---------------------------------------------------------------------------

pub fn emit_vector_interleave_upper8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::punpckhbw);
}
pub fn emit_vector_interleave_upper16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::punpckhwd);
}
pub fn emit_vector_interleave_upper32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::punpckhdq);
}
pub fn emit_vector_interleave_upper64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::punpckhqdq);
}

// ---------------------------------------------------------------------------
// VectorDeinterleaveEven/Odd
// ---------------------------------------------------------------------------

pub fn emit_vector_deinterleave_even8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let rhs = ra.use_scratch_xmm(&mut args[1]);
    let mask = ra.scratch_xmm();
    let constant = ra
        .constant_pool
        .as_mut()
        .expect("constant pool required")
        .get_constant(0x00ff_00ff_00ff_00ff, 0x00ff_00ff_00ff_00ff);
    ra.asm.movdqa(mask, rxbyak::xmmword_ptr(constant)).unwrap();
    ra.asm.pand(result, mask).unwrap();
    ra.asm.pand(rhs, mask).unwrap();
    ra.asm.packuswb(result, rhs).unwrap();
    ra.release(mask);
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_deinterleave_even16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let rhs = ra.use_scratch_xmm(&mut args[1]);
    if ctx.has_host_feature(HostFeature::SSE41) {
        let zero = ra.scratch_xmm();
        ra.asm.pxor(zero, zero).unwrap();
        ra.asm.pblendw(result, zero, 0b1010_1010).unwrap();
        ra.asm.pblendw(rhs, zero, 0b1010_1010).unwrap();
        ra.asm.packusdw(result, rhs).unwrap();
        ra.release(zero);
    } else {
        ra.asm.pslld_imm(result, 16).unwrap();
        ra.asm.psrad_imm(result, 16).unwrap();
        ra.asm.pslld_imm(rhs, 16).unwrap();
        ra.asm.psrad_imm(rhs, 16).unwrap();
        ra.asm.packssdw(result, rhs).unwrap();
    }
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_deinterleave_even32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let rhs = ra.use_xmm(&mut args[1]);
    ra.asm.shufps(result, rhs, 0b1000_1000).unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_deinterleave_even64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let rhs = ra.use_xmm(&mut args[1]);
    ra.asm.shufpd(result, rhs, 0).unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_deinterleave_odd8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let rhs = ra.use_scratch_xmm(&mut args[1]);
    ra.asm.psraw_imm(result, 8).unwrap();
    ra.asm.psraw_imm(rhs, 8).unwrap();
    ra.asm.packsswb(result, rhs).unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_deinterleave_odd16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let rhs = ra.use_scratch_xmm(&mut args[1]);
    ra.asm.psrad_imm(result, 16).unwrap();
    ra.asm.psrad_imm(rhs, 16).unwrap();
    ra.asm.packssdw(result, rhs).unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_deinterleave_odd32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let rhs = ra.use_xmm(&mut args[1]);
    ra.asm.shufps(result, rhs, 0b1101_1101).unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_deinterleave_odd64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let rhs = ra.use_xmm(&mut args[1]);
    ra.asm.shufpd(result, rhs, 0b11).unwrap();
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// VectorDeinterleaveEvenLower/OddLower
// ---------------------------------------------------------------------------

pub fn emit_vector_deinterleave_even_lower8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    if ctx.has_host_feature(HostFeature::SSSE3) {
        let rhs = ra.use_xmm(&mut args[1]);
        let mask = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0x0d09_0501_0c08_0400, 0x8080_8080_8080_8080);
        ra.asm.punpcklbw(result, rhs).unwrap();
        ra.asm.pshufb(result, rxbyak::xmmword_ptr(mask)).unwrap();
    } else {
        let rhs = ra.use_scratch_xmm(&mut args[1]);
        let mask = ra.scratch_xmm();
        let constant = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0x00ff_00ff_00ff_00ff, 0x00ff_00ff_00ff_00ff);
        ra.asm.movdqa(mask, rxbyak::xmmword_ptr(constant)).unwrap();
        ra.asm.pand(result, mask).unwrap();
        ra.asm.pand(rhs, mask).unwrap();
        ra.asm.packuswb(result, rhs).unwrap();
        ra.asm.pshufd(result, result, 0b1101_1000).unwrap();
        ra.asm.movq(result, result).unwrap();
        ra.release(mask);
    }
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_deinterleave_even_lower16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    if ctx.has_host_feature(HostFeature::SSSE3) {
        let rhs = ra.use_xmm(&mut args[1]);
        let mask = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0x0b0a_0302_0908_0100, 0x8080_8080_8080_8080);
        ra.asm.punpcklwd(result, rhs).unwrap();
        ra.asm.pshufb(result, rxbyak::xmmword_ptr(mask)).unwrap();
    } else {
        let rhs = ra.use_scratch_xmm(&mut args[1]);
        ra.asm.pslld_imm(result, 16).unwrap();
        ra.asm.psrad_imm(result, 16).unwrap();
        ra.asm.pslld_imm(rhs, 16).unwrap();
        ra.asm.psrad_imm(rhs, 16).unwrap();
        ra.asm.packssdw(result, rhs).unwrap();
        ra.asm.pshufd(result, result, 0b1101_1000).unwrap();
        ra.asm.movq(result, result).unwrap();
    }
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_deinterleave_even_lower32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let rhs = ra.use_xmm(&mut args[1]);
    if ctx.has_host_feature(HostFeature::SSE41) {
        ra.asm.insertps(result, rhs, 0b0001_1100).unwrap();
    } else {
        ra.asm.unpcklps(result, rhs).unwrap();
        ra.asm.movq(result, result).unwrap();
    }
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_deinterleave_odd_lower8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    if ctx.has_host_feature(HostFeature::SSSE3) {
        let rhs = ra.use_xmm(&mut args[1]);
        let mask = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0x0f0b_0703_0e0a_0602, 0x8080_8080_8080_8080);
        ra.asm.punpcklbw(result, rhs).unwrap();
        ra.asm.pshufb(result, rxbyak::xmmword_ptr(mask)).unwrap();
    } else {
        let rhs = ra.use_scratch_xmm(&mut args[1]);
        ra.asm.psraw_imm(result, 8).unwrap();
        ra.asm.psraw_imm(rhs, 8).unwrap();
        ra.asm.packsswb(result, rhs).unwrap();
        ra.asm.pshufd(result, result, 0b1101_1000).unwrap();
        ra.asm.movq(result, result).unwrap();
    }
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_deinterleave_odd_lower16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    if ctx.has_host_feature(HostFeature::SSSE3) {
        let rhs = ra.use_xmm(&mut args[1]);
        let mask = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0x0f0e_0706_0d0c_0504, 0x8080_8080_8080_8080);
        ra.asm.punpcklwd(result, rhs).unwrap();
        ra.asm.pshufb(result, rxbyak::xmmword_ptr(mask)).unwrap();
    } else {
        let rhs = ra.use_scratch_xmm(&mut args[1]);
        ra.asm.psrad_imm(result, 16).unwrap();
        ra.asm.psrad_imm(rhs, 16).unwrap();
        ra.asm.packssdw(result, rhs).unwrap();
        ra.asm.pshufd(result, result, 0b1101_1000).unwrap();
        ra.asm.movq(result, result).unwrap();
    }
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_deinterleave_odd_lower32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    if ctx.has_host_feature(HostFeature::SSE41) {
        let lhs = ra.use_xmm(&mut args[0]);
        let result = ra.use_scratch_xmm(&mut args[1]);
        ra.asm.insertps(result, lhs, 0b0100_1100).unwrap();
        ra.define_value(inst_ref, result);
    } else {
        let result = ra.use_scratch_xmm(&mut args[0]);
        let rhs = ra.use_xmm(&mut args[1]);
        let zero = ra.scratch_xmm();
        ra.asm.xorps(zero, zero).unwrap();
        ra.asm.unpcklps(result, rhs).unwrap();
        ra.asm.unpckhpd(result, zero).unwrap();
        ra.release(zero);
        ra.define_value(inst_ref, result);
    }
}

// ---------------------------------------------------------------------------
// VectorTranspose — native SSE2
// ---------------------------------------------------------------------------

pub fn emit_vector_transpose8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let lower = ra.use_scratch_xmm(&mut args[0]);
    let upper = ra.use_scratch_xmm(&mut args[1]);
    let part = args[2].get_immediate_u1();

    let pool = ra.constant_pool.as_mut().expect("constant pool required");
    if !part {
        let mask = pool.get_constant(0x00FF_00FF_00FF_00FF, 0x00FF_00FF_00FF_00FF);
        ra.asm.pand(lower, rxbyak::xmmword_ptr(mask)).unwrap();
        ra.asm.psllw_imm(upper, 8).unwrap();
    } else {
        ra.asm.psrlw_imm(lower, 8).unwrap();
        let mask = pool.get_constant(0xFF00_FF00_FF00_FF00, 0xFF00_FF00_FF00_FF00);
        ra.asm.pand(upper, rxbyak::xmmword_ptr(mask)).unwrap();
    }
    ra.asm.por(lower, upper).unwrap();

    ra.define_value(inst_ref, lower);
}

pub fn emit_vector_transpose16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let lower = ra.use_scratch_xmm(&mut args[0]);
    let upper = ra.use_scratch_xmm(&mut args[1]);
    let part = args[2].get_immediate_u1();

    let pool = ra.constant_pool.as_mut().expect("constant pool required");
    if !part {
        let mask = pool.get_constant(0x0000_FFFF_0000_FFFF, 0x0000_FFFF_0000_FFFF);
        ra.asm.pand(lower, rxbyak::xmmword_ptr(mask)).unwrap();
        ra.asm.pslld_imm(upper, 16).unwrap();
    } else {
        ra.asm.psrld_imm(lower, 16).unwrap();
        let mask = pool.get_constant(0xFFFF_0000_FFFF_0000, 0xFFFF_0000_FFFF_0000);
        ra.asm.pand(upper, rxbyak::xmmword_ptr(mask)).unwrap();
    }
    ra.asm.por(lower, upper).unwrap();

    ra.define_value(inst_ref, lower);
}

pub fn emit_vector_transpose32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let lower = ra.use_scratch_xmm(&mut args[0]);
    let upper = ra.use_xmm(&mut args[1]);
    let part = args[2].get_immediate_u1();

    ra.asm
        .shufps(lower, upper, if !part { 0x88 } else { 0xDD })
        .unwrap();
    ra.asm.pshufd(lower, lower, 0xD8).unwrap();

    ra.define_value(inst_ref, lower);
}

pub fn emit_vector_transpose64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let lower = ra.use_scratch_xmm(&mut args[0]);
    let upper = ra.use_xmm(&mut args[1]);
    let part = args[2].get_immediate_u1();

    ra.asm
        .shufpd(lower, upper, if !part { 0x00 } else { 0x03 })
        .unwrap();

    ra.define_value(inst_ref, lower);
}

// ---------------------------------------------------------------------------
// VectorShuffle — native SSE: pshufd/pshufhw/pshuflw
// ---------------------------------------------------------------------------

pub fn emit_vector_shuffle_words(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_shuffle_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pshufd);
}
pub fn emit_vector_shuffle_high_halfwords(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_shuffle_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pshufhw);
}
pub fn emit_vector_shuffle_low_halfwords(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_shuffle_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pshuflw);
}

// Narrow16: truncate 8×u16 from a to 8×u8 in the low half, zero upper half.
pub fn emit_vector_narrow16(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    if ctx.has_host_feature(HostFeature::AVX512_ORTHO | HostFeature::AVX512BW) {
        let a = ra.use_xmm(&mut args[0]);
        let result = ra.scratch_xmm();
        ra.asm.vpmovwb(result, a).unwrap();
        ra.define_value(inst_ref, result);
        return;
    }
    let result = ra.use_scratch_xmm(&mut args[0]);
    let zeros = ra.scratch_xmm();
    let narrow_mask = ra
        .constant_pool
        .as_mut()
        .expect("constant pool required")
        .get_constant(0x00ff_00ff_00ff_00ff, 0x00ff_00ff_00ff_00ff);
    ra.asm.pxor(zeros, zeros).unwrap();
    ra.asm
        .pand(result, rxbyak::xmmword_ptr(narrow_mask))
        .unwrap();
    ra.asm.packuswb(result, zeros).unwrap();
    ra.release(zeros);
    ra.define_value(inst_ref, result);
}

// Narrow32: truncate 4×u32 to 4×u16 in the low half, zero upper half.
pub fn emit_vector_narrow32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    if ctx.has_host_feature(HostFeature::AVX512_ORTHO) {
        let a = ra.use_xmm(&mut args[0]);
        let result = ra.scratch_xmm();
        ra.asm.vpmovdw(result, a).unwrap();
        ra.define_value(inst_ref, result);
        return;
    }
    let result = ra.use_scratch_xmm(&mut args[0]);
    let zeros = ra.scratch_xmm();
    ra.asm.pxor(zeros, zeros).unwrap();
    if ctx.has_host_feature(HostFeature::SSE41) {
        ra.asm.pblendw(result, zeros, 0xaa).unwrap();
        ra.asm.packusdw(result, zeros).unwrap();
    } else {
        ra.asm.pslld_imm(result, 16).unwrap();
        ra.asm.psrad_imm(result, 16).unwrap();
        ra.asm.packssdw(result, zeros).unwrap();
    }
    ra.release(zeros);
    ra.define_value(inst_ref, result);
}

// Narrow64: truncate 2×u64 to 2×u32 in the low half, zero upper half.
pub fn emit_vector_narrow64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    if ctx.has_host_feature(HostFeature::AVX512_ORTHO) {
        let a = ra.use_xmm(&mut args[0]);
        let result = ra.scratch_xmm();
        ra.asm.vpmovqd(result, a).unwrap();
        ra.define_value(inst_ref, result);
        return;
    }
    let result = ra.use_scratch_xmm(&mut args[0]);
    let zeros = ra.scratch_xmm();
    ra.asm.pxor(zeros, zeros).unwrap();
    ra.asm.shufps(result, zeros, 0x08).unwrap();
    ra.release(zeros);
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// VectorSignExtend — SSE4.1 fast paths with Eden's SSE2 fallbacks
// ---------------------------------------------------------------------------

pub fn emit_vector_sign_extend8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    if ctx.has_host_feature(HostFeature::SSE41) {
        let result = ra.use_scratch_xmm(&mut args[0]);
        ra.asm.pmovsxbw(result, result).unwrap();
        ra.define_value(inst_ref, result);
        return;
    }

    let source = ra.use_xmm(&mut args[0]);
    let result = ra.scratch_xmm();
    ra.asm.pxor(result, result).unwrap();
    ra.asm.punpcklbw(result, source).unwrap();
    ra.asm.psraw_imm(result, 8).unwrap();
    ra.release(source);
    ra.define_value(inst_ref, result);
}
pub fn emit_vector_sign_extend16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    if ctx.has_host_feature(HostFeature::SSE41) {
        let result = ra.use_scratch_xmm(&mut args[0]);
        ra.asm.pmovsxwd(result, result).unwrap();
        ra.define_value(inst_ref, result);
        return;
    }

    let source = ra.use_xmm(&mut args[0]);
    let result = ra.scratch_xmm();
    ra.asm.pxor(result, result).unwrap();
    ra.asm.punpcklwd(result, source).unwrap();
    ra.asm.psrad_imm(result, 16).unwrap();
    ra.release(source);
    ra.define_value(inst_ref, result);
}
pub fn emit_vector_sign_extend32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    if ctx.has_host_feature(HostFeature::SSE41) {
        ra.asm.pmovsxdq(result, result).unwrap();
    } else {
        let sign = ra.scratch_xmm();
        ra.asm.movaps(sign, result).unwrap();
        ra.asm.psrad_imm(sign, 31).unwrap();
        ra.asm.punpckldq(result, sign).unwrap();
        ra.release(sign);
    }
    ra.define_value(inst_ref, result);
}

pub fn emit_vector_sign_extend64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let data = ra.use_scratch_xmm(&mut args[0]);
    let sign = ra.scratch_gpr();
    ra.asm.movq(sign, data).unwrap();
    ra.asm.sar(sign, 63).unwrap();

    if ctx.has_host_feature(HostFeature::SSE41) {
        ra.asm.pinsrq(data, sign, 1).unwrap();
    } else {
        let sign_vector = ra.scratch_xmm();
        ra.asm.movq(sign_vector, sign).unwrap();
        ra.asm.punpcklqdq(data, sign_vector).unwrap();
        ra.release(sign_vector);
    }

    ra.release(sign);
    ra.define_value(inst_ref, data);
}

// ---------------------------------------------------------------------------
// VectorZeroExtend — SSE4.1 fast paths with Eden's SSE2 fallbacks
// ---------------------------------------------------------------------------

pub fn emit_vector_zero_extend8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    if ctx.has_host_feature(HostFeature::SSE41) {
        ra.asm.pmovzxbw(result, result).unwrap();
    } else {
        let zero = ra.scratch_xmm();
        ra.asm.pxor(zero, zero).unwrap();
        ra.asm.punpcklbw(result, zero).unwrap();
        ra.release(zero);
    }
    ra.define_value(inst_ref, result);
}
pub fn emit_vector_zero_extend16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    if ctx.has_host_feature(HostFeature::SSE41) {
        ra.asm.pmovzxwd(result, result).unwrap();
    } else {
        let zero = ra.scratch_xmm();
        ra.asm.pxor(zero, zero).unwrap();
        ra.asm.punpcklwd(result, zero).unwrap();
        ra.release(zero);
    }
    ra.define_value(inst_ref, result);
}
pub fn emit_vector_zero_extend32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    if ctx.has_host_feature(HostFeature::SSE41) {
        ra.asm.pmovzxdq(result, result).unwrap();
    } else {
        let zero = ra.scratch_xmm();
        ra.asm.pxor(zero, zero).unwrap();
        ra.asm.punpckldq(result, zero).unwrap();
        ra.release(zero);
    }
    ra.define_value(inst_ref, result);
}

// ZeroExtend64: preserve the low u64 and clear the high u64.
pub fn emit_vector_zero_extend64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let zero = ra.scratch_xmm();
    ra.asm.pxor(zero, zero).unwrap();
    ra.asm.punpcklqdq(result, zero).unwrap();
    ra.release(zero);
    ra.define_value(inst_ref, result);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::x64::callback::Callback;
    use crate::backend::x64::emit_context::{EmitCallbacks, EmitConfig};
    use crate::ir::location::LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::value::Value;
    use rxbyak::CodeAssembler;

    struct NoopCallback;

    impl Callback for NoopCallback {
        fn emit_call(
            &self,
            _code: &mut CodeAssembler,
            _setup: &dyn Fn(&mut CodeAssembler, &[rxbyak::Reg]) -> rxbyak::Result<()>,
        ) -> rxbyak::Result<()> {
            unreachable!("callback emission is not used in this unit test");
        }

        fn emit_call_with_return_pointer(
            &self,
            _code: &mut CodeAssembler,
            _setup: &dyn Fn(&mut CodeAssembler, rxbyak::Reg, &[rxbyak::Reg]) -> rxbyak::Result<()>,
        ) -> rxbyak::Result<()> {
            unreachable!("callback emission is not used in this unit test");
        }
    }

    fn dummy_emit_config() -> EmitConfig {
        fn cb() -> Box<dyn Callback> {
            Box::new(NoopCallback)
        }

        EmitConfig {
            coprocessors: crate::interface::a32::config::empty_coprocessors(),
            callbacks: EmitCallbacks {
                memory_read_8: cb(),
                memory_read_16: cb(),
                memory_read_32: cb(),
                memory_read_64: cb(),
                memory_read_128: cb(),
                memory_write_8: cb(),
                memory_write_16: cb(),
                memory_write_32: cb(),
                memory_write_64: cb(),
                memory_write_128: cb(),
                call_supervisor: cb(),
                interpreter_fallback: cb(),
                exception_raised: cb(),
                data_cache_operation: cb(),
                instruction_cache_operation: cb(),
                instruction_synchronization_barrier: cb(),
                add_ticks: cb(),
                get_ticks_remaining: cb(),
                exclusive_clear: cb(),
                exclusive_read_8: cb(),
                exclusive_read_16: cb(),
                exclusive_read_32: cb(),
                exclusive_read_64: cb(),
                exclusive_read_128: cb(),
                get_cntpct: cb(),
                exclusive_write_8: cb(),
                exclusive_write_16: cb(),
                exclusive_write_32: cb(),
                exclusive_write_64: cb(),
                exclusive_write_128: cb(),
            },
            raw_exclusive_write_callbacks: None,
            enable_cycle_counting: false,
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
            global_monitor: None,
            cntfrq_el0: 600_000_000,
            ctr_el0: 0x8444_c004,
            dczid_el0: 4,
            hook_data_cache_operations: false,
            hook_isb: false,
        }
    }

    fn emit_broadcast8_with_features(host_features: HostFeature) -> Vec<u8> {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let mut ra = RegAlloc::new_default(&mut asm, vec![(1, 8), (0, 128)]);
        let source = ra.scratch_gpr();
        ra.define_value(InstRef(0), source);
        ra.end_of_alloc_scope();

        let config = dummy_emit_config();
        let mut ctx = EmitContext::new(LocationDescriptor::new(0), &config);
        ctx.host_features = host_features;
        let inst = Inst::new(Opcode::VectorBroadcast8, &[Value::Inst(InstRef(0))]);
        emit_vector_broadcast8(&ctx, &mut ra, InstRef(1), &inst);
        ra.end_of_alloc_scope();
        ra.asm.code().to_vec()
    }

    #[test]
    fn test_fn_signatures() {
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_get_element8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_set_element64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_broadcast8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_broadcast_lower32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_extract;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_extract_lower;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_rotate_whole_vector_right;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_interleave_lower8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_interleave_upper64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_deinterleave_even8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_deinterleave_odd64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_transpose8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_shuffle_words;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_narrow16;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_sign_extend8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_sign_extend64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_zero_extend64;
    }

    #[test]
    fn whole_vector_rotate_uses_upstream_pshufd_controls() {
        assert_eq!(whole_vector_rotate_shuffle_imm(0), 0b11_10_01_00);
        assert_eq!(whole_vector_rotate_shuffle_imm(32), 0b00_11_10_01);
        assert_eq!(whole_vector_rotate_shuffle_imm(64), 0b01_00_11_10);
        assert_eq!(whole_vector_rotate_shuffle_imm(96), 0b10_01_00_11);
    }

    #[test]
    fn broadcast8_selects_edens_host_feature_paths() {
        let avx2 = emit_broadcast8_with_features(HostFeature::AVX2);
        let ssse3 = emit_broadcast8_with_features(HostFeature::SSSE3);
        let sse2 = emit_broadcast8_with_features(HostFeature::empty());

        assert!(avx2
            .windows(4)
            .any(|bytes| bytes[0] == 0xc4 && bytes[3] == 0x78));
        assert!(ssse3
            .windows(3)
            .any(|bytes| bytes[..2] == [0x0f, 0x38] && bytes[2] == 0x00));
        assert!(!sse2
            .windows(3)
            .any(|bytes| bytes[..2] == [0x0f, 0x38] && bytes[2] == 0x00));
        assert!(sse2.windows(2).any(|bytes| bytes == [0x0f, 0x60]));
    }

    #[test]
    fn broadcast_lower32_uses_edens_pshuflw_control() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let mut ra = RegAlloc::new_default(&mut asm, vec![(1, 32), (0, 128)]);
        let source = ra.scratch_gpr();
        ra.define_value(InstRef(0), source);
        ra.end_of_alloc_scope();

        let config = dummy_emit_config();
        let ctx = EmitContext::new(LocationDescriptor::new(0), &config);
        let inst = Inst::new(Opcode::VectorBroadcastLower32, &[Value::Inst(InstRef(0))]);
        emit_vector_broadcast_lower32(&ctx, &mut ra, InstRef(1), &inst);
        ra.end_of_alloc_scope();

        assert!(ra.asm.code().windows(4).any(|bytes| bytes[0] == 0xf2
            && bytes[1] == 0x0f
            && bytes[2] == 0x70
            && bytes[3] & 0xc0 == 0xc0));
        assert_eq!(ra.asm.code().last(), Some(&0b0100_0100));
    }
}
