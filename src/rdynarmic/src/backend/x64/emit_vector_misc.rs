#![allow(
    clippy::missing_transmute_annotations,
    clippy::useless_transmute,
    unnecessary_transmutes
)]

use crate::backend::x64::emit_context::EmitContext;
use crate::backend::x64::emit_vector_helpers::*;
use crate::backend::x64::reg_alloc::RegAlloc;
use crate::common::math_util::{recip_estimate, recip_sqrt_estimate};
use crate::ir::inst::Inst;
use crate::ir::value::InstRef;

// ---------------------------------------------------------------------------
// VectorSignedAbsoluteDifference — fallback
// ---------------------------------------------------------------------------

macro_rules! define_signed_abs_diff {
    ($name:ident, $signed:ty, $unsigned:ty, $count:expr) => {
        extern "C" fn $name(result: *mut [u8; 16], a: *const [u8; 16], b: *const [u8; 16]) {
            unsafe {
                let va: [$signed; $count] = std::mem::transmute(*a);
                let vb: [$signed; $count] = std::mem::transmute(*b);
                let mut out = [0 as $unsigned; $count];
                for i in 0..$count {
                    out[i] = (va[i] as i64 - vb[i] as i64).unsigned_abs() as $unsigned;
                }
                *result = std::mem::transmute(out);
            }
        }
    };
}

define_signed_abs_diff!(fallback_signed_abs_diff8, i8, u8, 16);
define_signed_abs_diff!(fallback_signed_abs_diff16, i16, u16, 8);
define_signed_abs_diff!(fallback_signed_abs_diff32, i32, u32, 4);

pub fn emit_vector_signed_absolute_difference8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_signed_abs_diff8 as *const () as usize,
    );
}
pub fn emit_vector_signed_absolute_difference16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_signed_abs_diff16 as *const () as usize,
    );
}
pub fn emit_vector_signed_absolute_difference32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_signed_abs_diff32 as *const () as usize,
    );
}

// ---------------------------------------------------------------------------
// VectorUnsignedAbsoluteDifference — fallback
// ---------------------------------------------------------------------------

macro_rules! define_unsigned_abs_diff {
    ($name:ident, $ty:ty, $count:expr) => {
        extern "C" fn $name(result: *mut [u8; 16], a: *const [u8; 16], b: *const [u8; 16]) {
            unsafe {
                let va: [$ty; $count] = std::mem::transmute(*a);
                let vb: [$ty; $count] = std::mem::transmute(*b);
                let mut out = [0 as $ty; $count];
                for i in 0..$count {
                    out[i] = if va[i] >= vb[i] {
                        va[i] - vb[i]
                    } else {
                        vb[i] - va[i]
                    };
                }
                *result = std::mem::transmute(out);
            }
        }
    };
}

define_unsigned_abs_diff!(fallback_unsigned_abs_diff8, u8, 16);
define_unsigned_abs_diff!(fallback_unsigned_abs_diff16, u16, 8);
define_unsigned_abs_diff!(fallback_unsigned_abs_diff32, u32, 4);

pub fn emit_vector_unsigned_absolute_difference8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_unsigned_abs_diff8 as *const () as usize,
    );
}
pub fn emit_vector_unsigned_absolute_difference16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_unsigned_abs_diff16 as *const () as usize,
    );
}
pub fn emit_vector_unsigned_absolute_difference32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_unsigned_abs_diff32 as *const () as usize,
    );
}

// ---------------------------------------------------------------------------
// VectorRoundingHalvingAddSigned
// ---------------------------------------------------------------------------

fn emit_vector_rounding_halving_add_signed(
    element_size: usize,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_scratch_xmm(&mut args[0]);
    let b = ra.use_scratch_xmm(&mut args[1]);

    match element_size {
        8 => {
            let vec_128 = ra.scratch_xmm();
            let sign_bias = ra
                .constant_pool
                .as_mut()
                .expect("constant pool required")
                .get_constant(0x8080_8080_8080_8080, 0x8080_8080_8080_8080);
            ra.asm
                .movdqa(vec_128, rxbyak::xmmword_ptr(sign_bias))
                .unwrap();
            ra.asm.paddb(a, vec_128).unwrap();
            ra.asm.paddb(b, vec_128).unwrap();
            ra.asm.pavgb(a, b).unwrap();
            ra.asm.paddb(a, vec_128).unwrap();
            ra.release(vec_128);
        }
        16 => {
            let vec_32768 = ra.scratch_xmm();
            let sign_bias = ra
                .constant_pool
                .as_mut()
                .expect("constant pool required")
                .get_constant(0x8000_8000_8000_8000, 0x8000_8000_8000_8000);
            ra.asm
                .movdqa(vec_32768, rxbyak::xmmword_ptr(sign_bias))
                .unwrap();
            ra.asm.paddw(a, vec_32768).unwrap();
            ra.asm.paddw(b, vec_32768).unwrap();
            ra.asm.pavgw(a, b).unwrap();
            ra.asm.paddw(a, vec_32768).unwrap();
            ra.release(vec_32768);
        }
        32 => {
            let tmp = ra.scratch_xmm();
            ra.asm.movdqa(tmp, a).unwrap();
            ra.asm.por(a, b).unwrap();
            ra.asm.psrad_imm(tmp, 1).unwrap();
            ra.asm.psrad_imm(b, 1).unwrap();
            ra.asm.pslld_imm(a, 31).unwrap();
            ra.asm.paddd(b, tmp).unwrap();
            ra.asm.psrld_imm(a, 31).unwrap();
            ra.asm.paddd(a, b).unwrap();
            ra.release(tmp);
        }
        _ => unreachable!("unsupported signed RHADD element size"),
    }

    ra.release(b);
    ra.define_value(inst_ref, a);
}

pub fn emit_vector_rounding_halving_add_signed8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_rounding_halving_add_signed(8, ra, inst_ref, inst);
}
pub fn emit_vector_rounding_halving_add_signed16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_rounding_halving_add_signed(16, ra, inst_ref, inst);
}
pub fn emit_vector_rounding_halving_add_signed32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_rounding_halving_add_signed(32, ra, inst_ref, inst);
}

// ---------------------------------------------------------------------------
// VectorRoundingHalvingAddUnsigned
// ---------------------------------------------------------------------------

fn emit_vector_rounding_halving_add_unsigned(
    element_size: usize,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    match element_size {
        8 => emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pavgb),
        16 => emit_vector_op(ra, inst_ref, inst, rxbyak::CodeAssembler::pavgw),
        32 => {
            let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
            let a = ra.use_scratch_xmm(&mut args[0]);
            let b = ra.use_scratch_xmm(&mut args[1]);
            let tmp = ra.scratch_xmm();
            ra.asm.movdqa(tmp, a).unwrap();
            ra.asm.por(a, b).unwrap();
            ra.asm.psrld_imm(tmp, 1).unwrap();
            ra.asm.psrld_imm(b, 1).unwrap();
            ra.asm.pslld_imm(a, 31).unwrap();
            ra.asm.paddd(b, tmp).unwrap();
            ra.asm.psrld_imm(a, 31).unwrap();
            ra.asm.paddd(a, b).unwrap();
            ra.release(tmp);
            ra.release(b);
            ra.define_value(inst_ref, a);
        }
        _ => unreachable!("unsupported unsigned RHADD element size"),
    }
}

pub fn emit_vector_rounding_halving_add_unsigned8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_rounding_halving_add_unsigned(8, ra, inst_ref, inst);
}
pub fn emit_vector_rounding_halving_add_unsigned16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_rounding_halving_add_unsigned(16, ra, inst_ref, inst);
}
pub fn emit_vector_rounding_halving_add_unsigned32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_rounding_halving_add_unsigned(32, ra, inst_ref, inst);
}

// ---------------------------------------------------------------------------
// VectorUnsignedRecipEstimate — fallback (1-arg, per-element u32)
// ---------------------------------------------------------------------------

fn unsigned_recip_estimate(a: u32) -> u32 {
    if (a & 0x8000_0000) == 0 {
        return 0xFFFF_FFFF;
    }
    let input = (a >> 23) & 0x1ff;
    let estimate = recip_estimate(input as u64) as u32;
    (0x100 | estimate) << 23
}

extern "C" fn fallback_unsigned_recip_estimate(result: *mut [u8; 16], a: *const [u8; 16]) {
    unsafe {
        let va: [u32; 4] = std::mem::transmute(*a);
        let out: [u32; 4] = [
            unsigned_recip_estimate(va[0]),
            unsigned_recip_estimate(va[1]),
            unsigned_recip_estimate(va[2]),
            unsigned_recip_estimate(va[3]),
        ];
        *result = std::mem::transmute(out);
    }
}

pub fn emit_vector_unsigned_recip_estimate(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_one_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_unsigned_recip_estimate as *const () as usize,
    );
}

// ---------------------------------------------------------------------------
// VectorUnsignedRecipSqrtEstimate — fallback (1-arg, per-element u32)
// ---------------------------------------------------------------------------

fn unsigned_recip_sqrt_estimate(a: u32) -> u32 {
    if (a & 0xC000_0000) == 0 {
        return 0xFFFF_FFFF;
    }
    let input = (a >> 23) & 0x1ff;
    let estimate = recip_sqrt_estimate(input as u64) as u32;
    (0x100 | estimate) << 23
}

extern "C" fn fallback_unsigned_recip_sqrt_estimate(result: *mut [u8; 16], a: *const [u8; 16]) {
    unsafe {
        let va: [u32; 4] = std::mem::transmute(*a);
        let out: [u32; 4] = [
            unsigned_recip_sqrt_estimate(va[0]),
            unsigned_recip_sqrt_estimate(va[1]),
            unsigned_recip_sqrt_estimate(va[2]),
            unsigned_recip_sqrt_estimate(va[3]),
        ];
        *result = std::mem::transmute(out);
    }
}

pub fn emit_vector_unsigned_recip_sqrt_estimate(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_one_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_unsigned_recip_sqrt_estimate as *const () as usize,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fn_signatures() {
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_signed_absolute_difference8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_unsigned_absolute_difference32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_rounding_halving_add_signed8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_rounding_halving_add_unsigned32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_unsigned_recip_estimate;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_unsigned_recip_sqrt_estimate;
    }

    #[test]
    fn unsigned_estimates_match_upstream_reference_values() {
        assert_eq!(unsigned_recip_estimate(0x7fff_ffff), 0xffff_ffff);
        assert_eq!(unsigned_recip_estimate(0x8000_0000), 0xff80_0000);
        assert_eq!(unsigned_recip_sqrt_estimate(0x3fff_ffff), 0xffff_ffff);
        assert_eq!(unsigned_recip_sqrt_estimate(0x8000_0000), 0xb480_0000);
    }

    #[test]
    fn test_fallback_unsigned_abs_diff8() {
        let a: [u8; 16] = [
            10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 200, 255,
        ];
        let b: [u8; 16] = [
            5, 25, 30, 45, 50, 55, 75, 80, 95, 100, 115, 120, 125, 145, 100, 0,
        ];
        let mut result = [0u8; 16];
        fallback_unsigned_abs_diff8(&mut result, &a, &b);
        assert_eq!(result[0], 5);
        assert_eq!(result[1], 5);
        assert_eq!(result[2], 0);
        assert_eq!(result[14], 100);
        assert_eq!(result[15], 255);
    }
}
