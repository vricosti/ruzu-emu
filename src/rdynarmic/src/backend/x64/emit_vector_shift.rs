#![allow(
    clippy::missing_transmute_annotations,
    clippy::useless_transmute,
    unnecessary_transmutes
)]

use crate::backend::x64::constants::cmp_int;
use crate::backend::x64::emit_context::EmitContext;
use crate::backend::x64::emit_vector_helpers::*;
use crate::backend::x64::host_feature::HostFeature;
use crate::backend::x64::reg_alloc::RegAlloc;
use crate::ir::inst::Inst;
use crate::ir::value::InstRef;

// ---------------------------------------------------------------------------
// VectorLogicalShiftLeft — native SSE for 16/32/64 (imm form)
// 8-bit has no native SSE instruction → fallback
// ---------------------------------------------------------------------------

// VectorLogicalShiftLeft8: no psllb; use psllw + mask to clear overflow bits
// Upstream pattern: psllw(data, shift), pand(data, mask) where mask clears the bits that
// overflowed from the low byte into the high byte of each word
pub fn emit_vector_logical_shift_left8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let shift = args[1].get_immediate_u8();
    if shift >= 8 {
        ra.asm.xorps(result, result).unwrap();
    } else if shift > 0 {
        ra.asm.psllw_imm(result, shift).unwrap();
        // Mask: for shift=1, valid bits = 0xFE per byte → mask = 0xFEFE...
        // For shift=n, mask = (0xFF << n) & 0xFF per byte
        let mask_byte = (0xFFu8 << shift) as u64;
        let mask_word = mask_byte | (mask_byte << 8);
        let mask_dword = mask_word | (mask_word << 16);
        let mask_qword = mask_dword | (mask_dword << 32);
        let pool = ra.constant_pool.as_mut().expect("constant pool required");
        let mask_addr = pool.get_constant(mask_qword, mask_qword);
        ra.asm.pand(result, rxbyak::xmmword_ptr(mask_addr)).unwrap();
    }
    // shift == 0: result is already data, no-op
    ra.define_value(inst_ref, result);
}
pub fn emit_vector_logical_shift_left16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op_imm(ra, inst_ref, inst, rxbyak::CodeAssembler::psllw_imm);
}
pub fn emit_vector_logical_shift_left32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op_imm(ra, inst_ref, inst, rxbyak::CodeAssembler::pslld_imm);
}
pub fn emit_vector_logical_shift_left64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op_imm(ra, inst_ref, inst, rxbyak::CodeAssembler::psllq_imm);
}

// ---------------------------------------------------------------------------
// VectorLogicalShiftRight — native SSE for 16/32/64 (imm form)
// ---------------------------------------------------------------------------

// VectorLogicalShiftRight8: psrlw + mask (same pattern as LSL8)
pub fn emit_vector_logical_shift_right8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let shift = args[1].get_immediate_u8();
    if shift >= 8 {
        ra.asm.xorps(result, result).unwrap();
    } else if shift > 0 {
        ra.asm.psrlw_imm(result, shift).unwrap();
        // Mask: for shift=1, valid bits = 0x7F per byte
        // For shift=n, mask = 0xFF >> n per byte
        let mask_byte = (0xFFu8 >> shift) as u64;
        let mask_word = mask_byte | (mask_byte << 8);
        let mask_dword = mask_word | (mask_word << 16);
        let mask_qword = mask_dword | (mask_dword << 32);
        let pool = ra.constant_pool.as_mut().expect("constant pool required");
        let mask_addr = pool.get_constant(mask_qword, mask_qword);
        ra.asm.pand(result, rxbyak::xmmword_ptr(mask_addr)).unwrap();
    }
    ra.define_value(inst_ref, result);
}
pub fn emit_vector_logical_shift_right16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op_imm(ra, inst_ref, inst, rxbyak::CodeAssembler::psrlw_imm);
}
pub fn emit_vector_logical_shift_right32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op_imm(ra, inst_ref, inst, rxbyak::CodeAssembler::psrld_imm);
}
pub fn emit_vector_logical_shift_right64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op_imm(ra, inst_ref, inst, rxbyak::CodeAssembler::psrlq_imm);
}

// ---------------------------------------------------------------------------
// VectorArithmeticShiftRight — native SSE for 16/32 (imm form)
// 8/64-bit have no native SSE → fallback
// ---------------------------------------------------------------------------

// VectorArithmeticShiftRight8: psrlw + sign extension via pcmpgtb
// result = (data >> shift) | (sign_mask where data < 0)
pub fn emit_vector_arithmetic_shift_right8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let shift = args[1].get_immediate_u8().min(7);
    if shift == 0 {
        ra.define_value(inst_ref, result);
        return;
    }

    let data_sign = ra.scratch_xmm();
    let zero = ra.scratch_xmm();
    ra.asm.xorps(zero, zero).unwrap();

    // data_sign = 0xFF where result < 0, 0x00 where >= 0
    ra.asm.movaps(data_sign, zero).unwrap();
    ra.asm.pcmpgtb(data_sign, result).unwrap();

    // Logical shift right (word-level) then mask to byte boundaries
    ra.asm.psrlw_imm(result, shift).unwrap();
    let lsr_mask_byte = (0xFFu8 >> shift) as u64;
    let lsr_mask = lsr_mask_byte * 0x01_01_01_01_01_01_01_01u64;
    let pool = ra.constant_pool.as_mut().expect("constant pool required");
    let lsr_mask_addr = pool.get_constant(lsr_mask, lsr_mask);
    ra.asm
        .pand(result, rxbyak::xmmword_ptr(lsr_mask_addr))
        .unwrap();

    // Sign extension: OR in upper bits for negative bytes
    let sign_ext_byte = (!lsr_mask_byte) as u64 & 0xFF;
    let sign_ext = sign_ext_byte * 0x01_01_01_01_01_01_01_01u64;
    let pool = ra.constant_pool.as_mut().expect("constant pool required");
    let sign_ext_addr = pool.get_constant(sign_ext, sign_ext);
    ra.asm
        .pand(data_sign, rxbyak::xmmword_ptr(sign_ext_addr))
        .unwrap();
    ra.asm.por(result, data_sign).unwrap();

    ra.release(data_sign);
    ra.release(zero);
    ra.define_value(inst_ref, result);
}
pub fn emit_vector_arithmetic_shift_right16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op_imm(ra, inst_ref, inst, rxbyak::CodeAssembler::psraw_imm);
}
pub fn emit_vector_arithmetic_shift_right32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_op_imm(ra, inst_ref, inst, rxbyak::CodeAssembler::psrad_imm);
}
pub fn emit_vector_arithmetic_shift_right64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let shift = args[1].get_immediate_u8().min(63);
    let sign = ra.scratch_xmm();
    let extension = ra.scratch_xmm();

    let sign_bit = 0x8000_0000_0000_0000u64 >> shift;
    ra.asm.xorps(extension, extension).unwrap();
    ra.asm.psrlq_imm(result, shift).unwrap();
    let sign_mask = ra
        .constant_pool
        .as_mut()
        .expect("constant pool required")
        .get_constant(sign_bit, sign_bit);
    ra.asm.movdqa(sign, rxbyak::xmmword_ptr(sign_mask)).unwrap();
    ra.asm.pand(sign, result).unwrap();
    ra.asm.psubq(extension, sign).unwrap();
    ra.asm.por(result, extension).unwrap();

    ra.release(sign);
    ra.release(extension);
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// VectorLogicalVShift — variable shift per element, fallback
// ---------------------------------------------------------------------------

macro_rules! define_logical_vshift {
    ($name:ident, $ty:ty, $count:expr) => {
        extern "C" fn $name(result: *mut [u8; 16], a: *const [u8; 16], b: *const [u8; 16]) {
            unsafe {
                let va: [$ty; $count] = std::mem::transmute(*a);
                let vb: [i8; 16] = std::mem::transmute(*b);
                let mut out = [0 as $ty; $count];
                let elem_bits = (std::mem::size_of::<$ty>() * 8) as i8;
                for i in 0..$count {
                    let shift = vb[i * std::mem::size_of::<$ty>()];
                    if shift >= elem_bits || shift <= -elem_bits {
                        out[i] = 0;
                    } else if shift >= 0 {
                        out[i] = va[i] << (shift as u32);
                    } else {
                        out[i] = va[i] >> ((-shift) as u32);
                    }
                }
                *result = std::mem::transmute(out);
            }
        }
    };
}

define_logical_vshift!(fallback_lvshift8, u8, 16);
define_logical_vshift!(fallback_lvshift16, u16, 8);
define_logical_vshift!(fallback_lvshift32, u32, 4);
define_logical_vshift!(fallback_lvshift64, u64, 2);

pub fn emit_vector_logical_vshift8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    if ctx.has_host_feature(HostFeature::AVX512_ORTHO | HostFeature::AVX512BW | HostFeature::GFNI) {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let result = ra.use_scratch_xmm(&mut args[0]);
        let left_shift = ra.use_scratch_xmm(&mut args[1]);
        let tmp = ra.scratch_xmm();

        let matrix = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0x8040_2010_0804_0201, 0x8040_2010_0804_0201);
        let valid_bits = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0xf8f8_f8f8_f8f8_f8f8, 0xf8f8_f8f8_f8f8_f8f8);
        let overflow_masks = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0x0103_070f_1f3f_7fff, 0);

        ra.asm.pxor(tmp, tmp).unwrap();
        ra.asm
            .vpcmpb(rxbyak::K1, left_shift, tmp, cmp_int::LESS_THAN)
            .unwrap();

        ra.asm
            .vmovaps(rxbyak::XMM0, rxbyak::xmmword_ptr(matrix))
            .unwrap();
        ra.asm
            .vgf2p8affineqb(result.k(1), result, rxbyak::XMM0, 0)
            .unwrap();

        ra.asm.pabsb(left_shift, left_shift).unwrap();

        ra.asm
            .vptestnmb(rxbyak::K2, left_shift, rxbyak::xmmword_ptr(valid_bits))
            .unwrap();

        ra.asm
            .movdqa(tmp, rxbyak::xmmword_ptr(overflow_masks))
            .unwrap();
        ra.asm.vpshufb(tmp.k(2).z(), tmp, left_shift).unwrap();
        ra.asm.pand(result, tmp).unwrap();

        ra.asm.pxor(tmp, tmp).unwrap();
        ra.asm.movsd(tmp, rxbyak::XMM0).unwrap();
        ra.asm.pshufb(tmp, left_shift).unwrap();
        ra.asm.gf2p8mulb(result, tmp).unwrap();

        ra.asm
            .vgf2p8affineqb(result.k(1), result, rxbyak::XMM0, 0)
            .unwrap();

        ra.release(left_shift);
        ra.release(tmp);
        ra.define_value(inst_ref, result);
        return;
    }
    emit_two_arg_fallback(ra, inst_ref, inst, fallback_lvshift8 as usize);
}
pub fn emit_vector_logical_vshift16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    if ctx.has_host_feature(HostFeature::AVX512_ORTHO | HostFeature::AVX512BW) {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let result = ra.use_scratch_xmm(&mut args[0]);
        let left_shift = ra.use_scratch_xmm(&mut args[1]);
        let right_shift = ra.scratch_xmm();
        let tmp = ra.scratch_xmm();
        let mask = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0x00ff_00ff_00ff_00ff, 0x00ff_00ff_00ff_00ff);

        ra.asm.pxor(right_shift, right_shift).unwrap();
        ra.asm.psubw(right_shift, left_shift).unwrap();
        ra.asm.pand(left_shift, rxbyak::xmmword_ptr(mask)).unwrap();
        ra.asm.pand(right_shift, rxbyak::xmmword_ptr(mask)).unwrap();
        ra.asm.vpsllvw(tmp, result, left_shift).unwrap();
        ra.asm.vpsrlvw(result, result, right_shift).unwrap();
        ra.asm.por(result, tmp).unwrap();

        ra.release(left_shift);
        ra.release(right_shift);
        ra.release(tmp);
        ra.define_value(inst_ref, result);
        return;
    }
    emit_two_arg_fallback(ra, inst_ref, inst, fallback_lvshift16 as usize);
}
// LogicalVShift32: AVX2 vpsllvd/vpsrlvd with sign-based split, fallback without AVX2
pub fn emit_vector_logical_vshift32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    if ctx.has_host_feature(HostFeature::AVX2) {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let a = ra.use_scratch_xmm(&mut args[0]);
        let shift = ra.use_scratch_xmm(&mut args[1]);
        let result = ra.scratch_xmm();

        // Preserve the sign bit of the lowest byte of each 32-bit element.
        // XMM0 is reserved by the allocator and is the implicit blend mask.
        ra.asm.movaps(rxbyak::XMM0, shift).unwrap();
        ra.asm.pslld_imm(rxbyak::XMM0, 24).unwrap();

        // x86 variable shifts accept positive counts only. ARM uses the
        // signed lowest byte, so take the byte-wise absolute value and mask
        // away all other bytes before shifting.
        ra.asm.vpabsb(shift, shift).unwrap();
        let shift_mask = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0x0000_00FF_0000_00FF, 0x0000_00FF_0000_00FF);
        ra.asm
            .vpand(shift, shift, rxbyak::xmmword_ptr(shift_mask))
            .unwrap();
        ra.asm.vpsllvd(result, a, shift).unwrap();
        ra.asm.vpsrlvd(a, a, shift).unwrap();
        ra.asm.blendvps(result, a).unwrap();

        ra.release(a);
        ra.release(shift);
        ra.define_value(inst_ref, result);
        return;
    }
    emit_two_arg_fallback(ra, inst_ref, inst, fallback_lvshift32 as usize);
}
// LogicalVShift64: AVX2 vpsllvq/vpsrlvq
pub fn emit_vector_logical_vshift64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    if ctx.has_host_feature(HostFeature::AVX2) {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let a = ra.use_scratch_xmm(&mut args[0]);
        let shift = ra.use_scratch_xmm(&mut args[1]);
        let result = ra.scratch_xmm();

        ra.asm.movaps(rxbyak::XMM0, shift).unwrap();
        ra.asm.psllq_imm(rxbyak::XMM0, 56).unwrap();
        ra.asm.vpabsb(shift, shift).unwrap();
        let shift_mask = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0xFF, 0xFF);
        ra.asm
            .vpand(shift, shift, rxbyak::xmmword_ptr(shift_mask))
            .unwrap();
        ra.asm.vpsllvq(result, a, shift).unwrap();
        ra.asm.vpsrlvq(a, a, shift).unwrap();
        ra.asm.blendvpd(result, a).unwrap();

        ra.release(a);
        ra.release(shift);
        ra.define_value(inst_ref, result);
        return;
    }
    emit_two_arg_fallback(ra, inst_ref, inst, fallback_lvshift64 as usize);
}

// ---------------------------------------------------------------------------
// VectorArithmeticVShift — variable arithmetic shift per element, fallback
// ---------------------------------------------------------------------------

macro_rules! define_arith_vshift {
    ($name:ident, $sty:ty, $uty:ty, $count:expr) => {
        extern "C" fn $name(result: *mut [u8; 16], a: *const [u8; 16], b: *const [u8; 16]) {
            unsafe {
                let va: [$sty; $count] = std::mem::transmute(*a);
                let vb: [i8; 16] = std::mem::transmute(*b);
                let mut out = [0 as $sty; $count];
                let elem_bits = (std::mem::size_of::<$sty>() * 8) as i8;
                for i in 0..$count {
                    let shift = vb[i * std::mem::size_of::<$sty>()];
                    if shift >= elem_bits {
                        out[i] = 0;
                    } else if shift >= 0 {
                        out[i] = ((va[i] as $uty) << (shift as u32)) as $sty;
                    } else if shift <= -elem_bits {
                        out[i] = va[i] >> (elem_bits as u32 - 1);
                    } else {
                        out[i] = va[i] >> ((-shift) as u32);
                    }
                }
                *result = std::mem::transmute(out);
            }
        }
    };
}

define_arith_vshift!(fallback_avshift8, i8, u8, 16);
define_arith_vshift!(fallback_avshift16, i16, u16, 8);
define_arith_vshift!(fallback_avshift32, i32, u32, 4);
define_arith_vshift!(fallback_avshift64, i64, u64, 2);

pub fn emit_vector_arithmetic_vshift8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(ra, inst_ref, inst, fallback_avshift8 as usize);
}
pub fn emit_vector_arithmetic_vshift16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    if ctx.has_host_feature(HostFeature::AVX512_ORTHO | HostFeature::AVX512BW) {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let result = ra.use_scratch_xmm(&mut args[0]);
        let left_shift = ra.use_scratch_xmm(&mut args[1]);
        let right_shift = ra.scratch_xmm();
        let tmp = ra.scratch_xmm();
        let mask = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0x00ff_00ff_00ff_00ff, 0x00ff_00ff_00ff_00ff);

        ra.asm.vmovdqa32(tmp, rxbyak::xmmword_ptr(mask)).unwrap();
        ra.asm
            .vpxord(right_shift, right_shift, right_shift)
            .unwrap();
        ra.asm.vpsubw(right_shift, right_shift, left_shift).unwrap();
        ra.asm.movaps(rxbyak::XMM0, left_shift).unwrap();
        ra.asm.psllw_imm(rxbyak::XMM0, 8).unwrap();
        ra.asm.psraw_imm(rxbyak::XMM0, 15).unwrap();
        ra.asm.vpmovb2m(rxbyak::K1, rxbyak::XMM0).unwrap();
        ra.asm.vpandd(right_shift, right_shift, tmp).unwrap();
        ra.asm.vpandd(left_shift, left_shift, tmp).unwrap();
        ra.asm.vpsravw(tmp, result, right_shift).unwrap();
        ra.asm.vpsllvw(result, result, left_shift).unwrap();
        ra.asm.vpblendmb(result.k(1), result, tmp).unwrap();

        ra.release(left_shift);
        ra.release(right_shift);
        ra.release(tmp);
        ra.define_value(inst_ref, result);
        return;
    }
    emit_two_arg_fallback(ra, inst_ref, inst, fallback_avshift16 as usize);
}
// ArithmeticVShift32: AVX2 vpsllvd/vpsravd with sign split
// Positive shift = left (logical), negative = right (arithmetic)
pub fn emit_vector_arithmetic_vshift32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    if ctx.has_host_feature(HostFeature::AVX2) {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let result = ra.use_scratch_xmm(&mut args[0]);
        let shift = ra.use_xmm(&mut args[1]);
        let absolute_shift = ra.scratch_xmm();
        let right = ra.scratch_xmm();

        ra.asm.vpabsb(absolute_shift, shift).unwrap();
        ra.asm.movaps(rxbyak::XMM0, shift).unwrap();
        ra.asm.pslld_imm(rxbyak::XMM0, 24).unwrap();
        let shift_mask = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0x0000_00FF_0000_00FF, 0x0000_00FF_0000_00FF);
        ra.asm
            .vpand(
                absolute_shift,
                absolute_shift,
                rxbyak::xmmword_ptr(shift_mask),
            )
            .unwrap();
        ra.asm.vpsravd(right, result, absolute_shift).unwrap();
        ra.asm.vpsllvd(result, result, absolute_shift).unwrap();
        ra.asm.blendvps(result, right).unwrap();

        ra.release(absolute_shift);
        ra.release(right);
        ra.define_value(inst_ref, result);
        return;
    }
    emit_two_arg_fallback(ra, inst_ref, inst, fallback_avshift32 as usize);
}
// ArithmeticVShift64: AVX512 vpsravq or fallback
pub fn emit_vector_arithmetic_vshift64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    if ctx.has_host_feature(HostFeature::AVX512_ORTHO) {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let result = ra.use_scratch_xmm(&mut args[0]);
        let left_shift = ra.use_scratch_xmm(&mut args[1]);
        let right_shift = ra.scratch_xmm();
        let tmp = ra.scratch_xmm();
        let mask = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required")
            .get_constant(0xff, 0xff);

        ra.asm.vmovdqa32(tmp, rxbyak::xmmword_ptr(mask)).unwrap();
        ra.asm
            .vpxorq(right_shift, right_shift, right_shift)
            .unwrap();
        ra.asm.vpsubq(right_shift, right_shift, left_shift).unwrap();
        ra.asm.movaps(rxbyak::XMM0, left_shift).unwrap();
        ra.asm.psllq_imm(rxbyak::XMM0, 56).unwrap();
        ra.asm.vpmovq2m(rxbyak::K1, rxbyak::XMM0).unwrap();
        ra.asm.vpandq(right_shift, right_shift, tmp).unwrap();
        ra.asm.vpandq(left_shift, left_shift, tmp).unwrap();
        ra.asm.vpsravq(tmp, result, right_shift).unwrap();
        ra.asm.vpsllvq(result, result, left_shift).unwrap();
        ra.asm.vpblendmq(result.k(1), result, tmp).unwrap();

        ra.release(left_shift);
        ra.release(right_shift);
        ra.release(tmp);
        ra.define_value(inst_ref, result);
        return;
    }
    emit_two_arg_fallback(ra, inst_ref, inst, fallback_avshift64 as usize);
}

// ---------------------------------------------------------------------------
// VectorRoundingShiftLeft — fallback
// ---------------------------------------------------------------------------

macro_rules! define_rounding_shift_signed {
    ($name:ident, $sty:ty, $uty:ty, $count:expr) => {
        extern "C" fn $name(result: *mut [u8; 16], a: *const [u8; 16], b: *const [u8; 16]) {
            unsafe {
                let va: [$sty; $count] = std::mem::transmute(*a);
                let vb: [i8; 16] = std::mem::transmute(*b);
                let mut out = [0 as $sty; $count];
                let elem_bits = std::mem::size_of::<$sty>() as i32 * 8;
                for i in 0..$count {
                    let shift = vb[i * std::mem::size_of::<$sty>()] as i32;
                    if shift >= elem_bits {
                        out[i] = 0;
                    } else if shift > 0 {
                        out[i] = ((va[i] as $uty) << shift as u32) as $sty;
                    } else if shift <= -elem_bits {
                        out[i] = va[i] >> (elem_bits as u32 - 1);
                    } else {
                        let neg = (-shift) as u32;
                        let round_bit = if neg > 0 { (va[i] >> (neg - 1)) & 1 } else { 0 };
                        out[i] = (va[i] >> neg) + round_bit;
                    }
                }
                *result = std::mem::transmute(out);
            }
        }
    };
}

macro_rules! define_rounding_shift_unsigned {
    ($name:ident, $ty:ty, $count:expr) => {
        extern "C" fn $name(result: *mut [u8; 16], a: *const [u8; 16], b: *const [u8; 16]) {
            unsafe {
                let va: [$ty; $count] = std::mem::transmute(*a);
                let vb: [i8; 16] = std::mem::transmute(*b);
                let mut out = [0 as $ty; $count];
                let elem_bits = std::mem::size_of::<$ty>() as i32 * 8;
                for i in 0..$count {
                    let shift = vb[i * std::mem::size_of::<$ty>()] as i32;
                    if shift >= elem_bits || shift <= -elem_bits {
                        out[i] = 0;
                    } else if shift >= 0 {
                        out[i] = va[i] << shift as u32;
                    } else {
                        let neg = (-shift) as u32;
                        let round_bit = if neg > 0 { (va[i] >> (neg - 1)) & 1 } else { 0 };
                        out[i] = (va[i] >> neg) + round_bit;
                    }
                }
                *result = std::mem::transmute(out);
            }
        }
    };
}

define_rounding_shift_signed!(fallback_rsl_s8, i8, u8, 16);
define_rounding_shift_signed!(fallback_rsl_s16, i16, u16, 8);
define_rounding_shift_signed!(fallback_rsl_s32, i32, u32, 4);
define_rounding_shift_signed!(fallback_rsl_s64, i64, u64, 2);
define_rounding_shift_unsigned!(fallback_rsl_u8, u8, 16);
define_rounding_shift_unsigned!(fallback_rsl_u16, u16, 8);
define_rounding_shift_unsigned!(fallback_rsl_u32, u32, 4);
define_rounding_shift_unsigned!(fallback_rsl_u64, u64, 2);

pub fn emit_vector_rounding_shift_left_signed8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(ra, inst_ref, inst, fallback_rsl_s8 as usize);
}
pub fn emit_vector_rounding_shift_left_signed16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(ra, inst_ref, inst, fallback_rsl_s16 as usize);
}
pub fn emit_vector_rounding_shift_left_signed32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(ra, inst_ref, inst, fallback_rsl_s32 as usize);
}
pub fn emit_vector_rounding_shift_left_signed64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(ra, inst_ref, inst, fallback_rsl_s64 as usize);
}
pub fn emit_vector_rounding_shift_left_unsigned8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(ra, inst_ref, inst, fallback_rsl_u8 as usize);
}
pub fn emit_vector_rounding_shift_left_unsigned16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(ra, inst_ref, inst, fallback_rsl_u16 as usize);
}
pub fn emit_vector_rounding_shift_left_unsigned32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(ra, inst_ref, inst, fallback_rsl_u32 as usize);
}
pub fn emit_vector_rounding_shift_left_unsigned64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(ra, inst_ref, inst, fallback_rsl_u64 as usize);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_lvshift32(a: [u32; 4], b: [u32; 4]) -> [u32; 4] {
        let mut result = [0u8; 16];
        let a_bytes: [u8; 16] = unsafe { std::mem::transmute(a) };
        let b_bytes: [u8; 16] = unsafe { std::mem::transmute(b) };
        fallback_lvshift32(&mut result, &a_bytes, &b_bytes);
        unsafe { std::mem::transmute(result) }
    }

    fn run_avshift32(a: [i32; 4], b: [u32; 4]) -> [i32; 4] {
        let mut result = [0u8; 16];
        let a_bytes: [u8; 16] = unsafe { std::mem::transmute(a) };
        let b_bytes: [u8; 16] = unsafe { std::mem::transmute(b) };
        fallback_avshift32(&mut result, &a_bytes, &b_bytes);
        unsafe { std::mem::transmute(result) }
    }

    #[test]
    fn test_fn_signatures() {
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_logical_shift_left8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_logical_shift_left16;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_logical_shift_right32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_arithmetic_shift_right64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_logical_vshift8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_arithmetic_vshift64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_rounding_shift_left_signed8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_rounding_shift_left_unsigned64;
    }

    #[test]
    fn logical_vshift32_uses_only_each_elements_low_signed_byte() {
        let input = [0x8000_0001, 0x8000_0001, 0x8000_0001, 0x8000_0001];
        let shifts = [0x7F00_0001, 0x0000_00FF, 0x1234_5600, 0xFFFF_FF20];

        assert_eq!(
            run_lvshift32(input, shifts),
            [0x0000_0002, 0x4000_0000, 0x8000_0001, 0]
        );
    }

    #[test]
    fn arithmetic_vshift32_uses_only_each_elements_low_signed_byte() {
        let input = [i32::MIN, i32::MIN, -3, i32::MIN];
        let shifts = [0x7F00_0001, 0x0000_00FF, 0x1234_56FF, 0xFFFF_FF20];

        assert_eq!(run_avshift32(input, shifts), [0, -1_073_741_824, -2, 0]);
    }
}
