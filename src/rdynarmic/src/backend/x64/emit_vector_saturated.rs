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
// VectorSignedSaturatedAbs — fallback with QC flag
// ---------------------------------------------------------------------------

macro_rules! define_sat_abs {
    ($name:ident, $sty:ty, $count:expr) => {
        extern "C" fn $name(result: *mut [u8; 16], a: *const [u8; 16]) -> u32 {
            unsafe {
                let va: [$sty; $count] = std::mem::transmute(*a);
                let mut out = [0 as $sty; $count];
                let mut qc = 0u32;
                for i in 0..$count {
                    if va[i] == <$sty>::MIN {
                        out[i] = <$sty>::MAX;
                        qc = 1;
                    } else {
                        out[i] = va[i].abs();
                    }
                }
                *result = std::mem::transmute(out);
                qc
            }
        }
    };
}

define_sat_abs!(fallback_sat_abs8, i8, 16);
define_sat_abs!(fallback_sat_abs16, i16, 8);
define_sat_abs!(fallback_sat_abs32, i32, 4);
define_sat_abs!(fallback_sat_abs64, i64, 2);

pub fn emit_vector_signed_saturated_abs8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_one_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_abs8 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_abs16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_one_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_abs16 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_abs32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_one_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_abs32 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_abs64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_one_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_abs64 as *const () as usize,
    );
}

// ---------------------------------------------------------------------------
// VectorSignedSaturatedNeg — fallback with QC flag
// ---------------------------------------------------------------------------

macro_rules! define_sat_neg {
    ($name:ident, $sty:ty, $count:expr) => {
        extern "C" fn $name(result: *mut [u8; 16], a: *const [u8; 16]) -> u32 {
            unsafe {
                let va: [$sty; $count] = std::mem::transmute(*a);
                let mut out = [0 as $sty; $count];
                let mut qc = 0u32;
                for i in 0..$count {
                    if va[i] == <$sty>::MIN {
                        out[i] = <$sty>::MAX;
                        qc = 1;
                    } else {
                        out[i] = -va[i];
                    }
                }
                *result = std::mem::transmute(out);
                qc
            }
        }
    };
}

define_sat_neg!(fallback_sat_neg8, i8, 16);
define_sat_neg!(fallback_sat_neg16, i16, 8);
define_sat_neg!(fallback_sat_neg32, i32, 4);
define_sat_neg!(fallback_sat_neg64, i64, 2);

pub fn emit_vector_signed_saturated_neg8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_one_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_neg8 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_neg16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_one_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_neg16 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_neg32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_one_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_neg32 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_neg64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_one_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_neg64 as *const () as usize,
    );
}

// ---------------------------------------------------------------------------
// VectorSignedSaturatedAccumulateUnsigned — fallback with QC
// ---------------------------------------------------------------------------

macro_rules! define_sat_accumulate_su {
    ($name:ident, $sty:ty, $uty:ty, $count:expr) => {
        extern "C" fn $name(result: *mut [u8; 16], a: *const [u8; 16], b: *const [u8; 16]) -> u32 {
            unsafe {
                let va: [$sty; $count] = std::mem::transmute(*a);
                let vb: [$uty; $count] = std::mem::transmute(*b);
                let mut out = [0 as $sty; $count];
                let mut qc = 0u32;
                for i in 0..$count {
                    let sum = (va[i] as i128) + (vb[i] as i128);
                    if sum > <$sty>::MAX as i128 {
                        out[i] = <$sty>::MAX;
                        qc = 1;
                    } else if sum < <$sty>::MIN as i128 {
                        out[i] = <$sty>::MIN;
                        qc = 1;
                    } else {
                        out[i] = sum as $sty;
                    }
                }
                *result = std::mem::transmute(out);
                qc
            }
        }
    };
}

define_sat_accumulate_su!(fallback_sat_accum_su8, i8, u8, 16);
define_sat_accumulate_su!(fallback_sat_accum_su16, i16, u16, 8);
define_sat_accumulate_su!(fallback_sat_accum_su32, i32, u32, 4);
define_sat_accumulate_su!(fallback_sat_accum_su64, i64, u64, 2);

pub fn emit_vector_signed_saturated_accumulate_unsigned8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_accum_su8 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_accumulate_unsigned16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_accum_su16 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_accumulate_unsigned32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_accum_su32 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_accumulate_unsigned64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_accum_su64 as *const () as usize,
    );
}

// ---------------------------------------------------------------------------
// VectorUnsignedSaturatedAccumulateSigned — fallback with QC
// ---------------------------------------------------------------------------

macro_rules! define_sat_accumulate_us {
    ($name:ident, $uty:ty, $sty:ty, $count:expr) => {
        extern "C" fn $name(result: *mut [u8; 16], a: *const [u8; 16], b: *const [u8; 16]) -> u32 {
            unsafe {
                let va: [$uty; $count] = std::mem::transmute(*a);
                let vb: [$sty; $count] = std::mem::transmute(*b);
                let mut out = [0 as $uty; $count];
                let mut qc = 0u32;
                for i in 0..$count {
                    let sum = (va[i] as i128) + (vb[i] as i128);
                    if sum > <$uty>::MAX as i128 {
                        out[i] = <$uty>::MAX;
                        qc = 1;
                    } else if sum < 0 {
                        out[i] = 0;
                        qc = 1;
                    } else {
                        out[i] = sum as $uty;
                    }
                }
                *result = std::mem::transmute(out);
                qc
            }
        }
    };
}

define_sat_accumulate_us!(fallback_sat_accum_us8, u8, i8, 16);
define_sat_accumulate_us!(fallback_sat_accum_us16, u16, i16, 8);
define_sat_accumulate_us!(fallback_sat_accum_us32, u32, i32, 4);
define_sat_accumulate_us!(fallback_sat_accum_us64, u64, i64, 2);

pub fn emit_vector_unsigned_saturated_accumulate_signed8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_accum_us8 as *const () as usize,
    );
}
pub fn emit_vector_unsigned_saturated_accumulate_signed16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_accum_us16 as *const () as usize,
    );
}
pub fn emit_vector_unsigned_saturated_accumulate_signed32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_accum_us32 as *const () as usize,
    );
}
pub fn emit_vector_unsigned_saturated_accumulate_signed64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_accum_us64 as *const () as usize,
    );
}

// ---------------------------------------------------------------------------
// VectorSignedSaturatedNarrowTo* — fallback with QC
// ---------------------------------------------------------------------------

macro_rules! define_sat_narrow_ss {
    ($name:ident, $sty:ty, $dty:ty, $count_in:expr, $count_out:expr) => {
        extern "C" fn $name(result: *mut [u8; 16], a: *const [u8; 16]) -> u32 {
            unsafe {
                let va: [$sty; $count_in] = std::mem::transmute(*a);
                let mut out = [0 as $dty; $count_out];
                let mut qc = 0u32;
                for i in 0..$count_in {
                    if va[i] > <$dty>::MAX as $sty {
                        out[i] = <$dty>::MAX;
                        qc = 1;
                    } else if va[i] < <$dty>::MIN as $sty {
                        out[i] = <$dty>::MIN;
                        qc = 1;
                    } else {
                        out[i] = va[i] as $dty;
                    }
                }
                *result = std::mem::transmute(out);
                qc
            }
        }
    };
}

define_sat_narrow_ss!(fallback_sat_narrow_ss16, i16, i8, 8, 16);
define_sat_narrow_ss!(fallback_sat_narrow_ss32, i32, i16, 4, 8);
define_sat_narrow_ss!(fallback_sat_narrow_ss64, i64, i32, 2, 4);

pub fn emit_vector_signed_saturated_narrow_to_signed16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_one_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_narrow_ss16 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_narrow_to_signed32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_one_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_narrow_ss32 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_narrow_to_signed64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_one_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_narrow_ss64 as *const () as usize,
    );
}

// VectorSignedSaturatedNarrowToUnsigned

macro_rules! define_sat_narrow_su {
    ($name:ident, $sty:ty, $dty:ty, $count_in:expr, $count_out:expr) => {
        extern "C" fn $name(result: *mut [u8; 16], a: *const [u8; 16]) -> u32 {
            unsafe {
                let va: [$sty; $count_in] = std::mem::transmute(*a);
                let mut out = [0 as $dty; $count_out];
                let mut qc = 0u32;
                for i in 0..$count_in {
                    if va[i] > <$dty>::MAX as $sty {
                        out[i] = <$dty>::MAX;
                        qc = 1;
                    } else if va[i] < 0 {
                        out[i] = 0;
                        qc = 1;
                    } else {
                        out[i] = va[i] as $dty;
                    }
                }
                *result = std::mem::transmute(out);
                qc
            }
        }
    };
}

define_sat_narrow_su!(fallback_sat_narrow_su16, i16, u8, 8, 16);
define_sat_narrow_su!(fallback_sat_narrow_su32, i32, u16, 4, 8);
define_sat_narrow_su!(fallback_sat_narrow_su64, i64, u32, 2, 4);

pub fn emit_vector_signed_saturated_narrow_to_unsigned16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_one_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_narrow_su16 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_narrow_to_unsigned32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_one_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_narrow_su32 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_narrow_to_unsigned64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_one_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_narrow_su64 as *const () as usize,
    );
}

// VectorUnsignedSaturatedNarrow

macro_rules! define_sat_narrow_uu {
    ($name:ident, $uty:ty, $dty:ty, $count_in:expr, $count_out:expr) => {
        extern "C" fn $name(result: *mut [u8; 16], a: *const [u8; 16]) -> u32 {
            unsafe {
                let va: [$uty; $count_in] = std::mem::transmute(*a);
                let mut out = [0 as $dty; $count_out];
                let mut qc = 0u32;
                for i in 0..$count_in {
                    if va[i] > <$dty>::MAX as $uty {
                        out[i] = <$dty>::MAX;
                        qc = 1;
                    } else {
                        out[i] = va[i] as $dty;
                    }
                }
                *result = std::mem::transmute(out);
                qc
            }
        }
    };
}

define_sat_narrow_uu!(fallback_sat_narrow_uu16, u16, u8, 8, 16);
define_sat_narrow_uu!(fallback_sat_narrow_uu32, u32, u16, 4, 8);
define_sat_narrow_uu!(fallback_sat_narrow_uu64, u64, u32, 2, 4);

pub fn emit_vector_unsigned_saturated_narrow16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_one_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_narrow_uu16 as *const () as usize,
    );
}
pub fn emit_vector_unsigned_saturated_narrow32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_one_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_narrow_uu32 as *const () as usize,
    );
}
pub fn emit_vector_unsigned_saturated_narrow64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_one_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_narrow_uu64 as *const () as usize,
    );
}

// ---------------------------------------------------------------------------
// VectorSignedSaturatedShiftLeft — fallback with QC
// ---------------------------------------------------------------------------

macro_rules! define_sat_shift_left_signed {
    ($name:ident, $sty:ty, $uty:ty, $count:expr) => {
        extern "C" fn $name(result: *mut [u8; 16], a: *const [u8; 16], b: *const [u8; 16]) -> u32 {
            unsafe {
                let va: [$sty; $count] = std::mem::transmute(*a);
                let vb: [i8; 16] = std::mem::transmute(*b);
                let mut out = [0 as $sty; $count];
                let mut qc = 0u32;
                let bits = std::mem::size_of::<$sty>() as i32 * 8;
                for i in 0..$count {
                    let shift = vb[i * std::mem::size_of::<$sty>()] as i32;
                    if shift >= bits {
                        out[i] = if va[i] != 0 {
                            if va[i] > 0 {
                                <$sty>::MAX
                            } else {
                                <$sty>::MIN
                            }
                        } else {
                            0
                        };
                        if va[i] != 0 {
                            qc = 1;
                        }
                    } else if shift > 0 {
                        let shifted = ((va[i] as $uty) << shift as u32) as $sty;
                        if (shifted >> shift as u32) != va[i] {
                            out[i] = if va[i] > 0 { <$sty>::MAX } else { <$sty>::MIN };
                            qc = 1;
                        } else {
                            out[i] = shifted;
                        }
                    } else if shift <= -bits {
                        out[i] = va[i] >> (bits as u32 - 1);
                    } else if shift < 0 {
                        out[i] = va[i] >> (-shift) as u32;
                    } else {
                        out[i] = va[i];
                    }
                }
                *result = std::mem::transmute(out);
                qc
            }
        }
    };
}

define_sat_shift_left_signed!(fallback_sat_shl_s8, i8, u8, 16);
define_sat_shift_left_signed!(fallback_sat_shl_s16, i16, u16, 8);
define_sat_shift_left_signed!(fallback_sat_shl_s32, i32, u32, 4);
define_sat_shift_left_signed!(fallback_sat_shl_s64, i64, u64, 2);

pub fn emit_vector_signed_saturated_shift_left8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_shl_s8 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_shift_left16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_shl_s16 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_shift_left32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_shl_s32 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_shift_left64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_shl_s64 as *const () as usize,
    );
}

// VectorSignedSaturatedShiftLeftUnsigned — shift left with unsigned saturation
macro_rules! define_sat_shl_unsigned {
    ($name:ident, $sty:ty, $uty:ty, $count:expr) => {
        extern "C" fn $name(result: *mut [u8; 16], a: *const [u8; 16], shift: u8) -> u32 {
            unsafe {
                let va: [$sty; $count] = std::mem::transmute(*a);
                let mut out = [0 as $uty; $count];
                let mut qc = 0u32;
                let bits = std::mem::size_of::<$sty>() as i32 * 8;
                let shift = shift as i32;
                for i in 0..$count {
                    if va[i] < 0 {
                        out[i] = 0;
                        qc = 1;
                    } else if shift >= bits {
                        if va[i] != 0 {
                            out[i] = <$uty>::MAX;
                            qc = 1;
                        } else {
                            out[i] = 0;
                        }
                    } else if shift > 0 {
                        let shifted = (va[i] as $uty) << shift as u32;
                        if (shifted >> shift as u32) as $sty != va[i] {
                            out[i] = <$uty>::MAX;
                            qc = 1;
                        } else {
                            out[i] = shifted;
                        }
                    } else {
                        out[i] = va[i] as $uty;
                    }
                }
                *result = std::mem::transmute(out);
                qc
            }
        }
    };
}

define_sat_shl_unsigned!(fallback_sat_shlu_s8, i8, u8, 16);
define_sat_shl_unsigned!(fallback_sat_shlu_s16, i16, u16, 8);
define_sat_shl_unsigned!(fallback_sat_shlu_s32, i32, u32, 4);
define_sat_shl_unsigned!(fallback_sat_shlu_s64, i64, u64, 2);

pub fn emit_vector_signed_saturated_shift_left_unsigned8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_with_saturation_and_immediate(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_shlu_s8 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_shift_left_unsigned16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_with_saturation_and_immediate(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_shlu_s16 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_shift_left_unsigned32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_with_saturation_and_immediate(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_shlu_s32 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_shift_left_unsigned64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_with_saturation_and_immediate(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_shlu_s64 as *const () as usize,
    );
}

// VectorUnsignedSaturatedShiftLeft
macro_rules! define_sat_shl_u {
    ($name:ident, $ty:ty, $count:expr) => {
        extern "C" fn $name(result: *mut [u8; 16], a: *const [u8; 16], b: *const [u8; 16]) -> u32 {
            unsafe {
                let va: [$ty; $count] = std::mem::transmute(*a);
                let vb: [i8; 16] = std::mem::transmute(*b);
                let mut out = [0 as $ty; $count];
                let mut qc = 0u32;
                let bits = std::mem::size_of::<$ty>() as i32 * 8;
                for i in 0..$count {
                    let shift = vb[i * std::mem::size_of::<$ty>()] as i32;
                    if shift >= bits {
                        if va[i] != 0 {
                            out[i] = <$ty>::MAX;
                            qc = 1;
                        }
                    } else if shift > 0 {
                        let shifted = va[i] << shift as u32;
                        if (shifted >> shift as u32) != va[i] {
                            out[i] = <$ty>::MAX;
                            qc = 1;
                        } else {
                            out[i] = shifted;
                        }
                    } else if shift <= -bits {
                        out[i] = 0;
                    } else if shift < 0 {
                        out[i] = va[i] >> (-shift) as u32;
                    } else {
                        out[i] = va[i];
                    }
                }
                *result = std::mem::transmute(out);
                qc
            }
        }
    };
}

define_sat_shl_u!(fallback_sat_shl_u8, u8, 16);
define_sat_shl_u!(fallback_sat_shl_u16, u16, 8);
define_sat_shl_u!(fallback_sat_shl_u32, u32, 4);
define_sat_shl_u!(fallback_sat_shl_u64, u64, 2);

pub fn emit_vector_unsigned_saturated_shift_left8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_shl_u8 as *const () as usize,
    );
}
pub fn emit_vector_unsigned_saturated_shift_left16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_shl_u16 as *const () as usize,
    );
}
pub fn emit_vector_unsigned_saturated_shift_left32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_shl_u32 as *const () as usize,
    );
}
pub fn emit_vector_unsigned_saturated_shift_left64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_shl_u64 as *const () as usize,
    );
}

// ---------------------------------------------------------------------------
// VectorSignedSaturatedDoublingMultiplyHigh — fallback with QC
// ---------------------------------------------------------------------------

macro_rules! define_sat_doubling_mul_high {
    ($name:ident, $sty:ty, $wide:ty, $count:expr) => {
        extern "C" fn $name(result: *mut [u8; 16], a: *const [u8; 16], b: *const [u8; 16]) -> u32 {
            unsafe {
                let va: [$sty; $count] = std::mem::transmute(*a);
                let vb: [$sty; $count] = std::mem::transmute(*b);
                let mut out = [0 as $sty; $count];
                let mut qc = 0u32;
                let bits = std::mem::size_of::<$sty>() * 8;
                for i in 0..$count {
                    let product = (va[i] as $wide) * (vb[i] as $wide) * 2;
                    let high = (product >> bits) as $sty;
                    if va[i] == <$sty>::MIN && vb[i] == <$sty>::MIN {
                        out[i] = <$sty>::MAX;
                        qc = 1;
                    } else {
                        out[i] = high;
                    }
                }
                *result = std::mem::transmute(out);
                qc
            }
        }
    };
}

define_sat_doubling_mul_high!(fallback_sat_dbl_mul_high16, i16, i32, 8);
define_sat_doubling_mul_high!(fallback_sat_dbl_mul_high32, i32, i64, 4);

pub fn emit_vector_signed_saturated_doubling_multiply_high16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_dbl_mul_high16 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_doubling_multiply_high32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_dbl_mul_high32 as *const () as usize,
    );
}

// VectorSignedSaturatedDoublingMultiplyHighRounding
macro_rules! define_sat_doubling_mul_high_rounding {
    ($name:ident, $sty:ty, $wide:ty, $count:expr) => {
        extern "C" fn $name(result: *mut [u8; 16], a: *const [u8; 16], b: *const [u8; 16]) -> u32 {
            unsafe {
                let va: [$sty; $count] = std::mem::transmute(*a);
                let vb: [$sty; $count] = std::mem::transmute(*b);
                let mut out = [0 as $sty; $count];
                let mut qc = 0u32;
                let bits = std::mem::size_of::<$sty>() * 8;
                let round = 1 as $wide << (bits - 1);
                for i in 0..$count {
                    if va[i] == <$sty>::MIN && vb[i] == <$sty>::MIN {
                        out[i] = <$sty>::MAX;
                        qc = 1;
                    } else {
                        let product = (va[i] as $wide) * (vb[i] as $wide) * 2 + round;
                        out[i] = (product >> bits) as $sty;
                    }
                }
                *result = std::mem::transmute(out);
                qc
            }
        }
    };
}

define_sat_doubling_mul_high_rounding!(fallback_sat_dbl_mul_high_round16, i16, i32, 8);
define_sat_doubling_mul_high_rounding!(fallback_sat_dbl_mul_high_round32, i32, i64, 4);

pub fn emit_vector_signed_saturated_doubling_multiply_high_rounding16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_dbl_mul_high_round16 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_doubling_multiply_high_rounding32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_dbl_mul_high_round32 as *const () as usize,
    );
}

// VectorSignedSaturatedDoublingMultiplyLong
extern "C" fn fallback_sat_dbl_mul_long16(
    result: *mut [u8; 16],
    a: *const [u8; 16],
    b: *const [u8; 16],
) -> u32 {
    unsafe {
        let va: [i16; 8] = std::mem::transmute(*a);
        let vb: [i16; 8] = std::mem::transmute(*b);
        let mut out = [0i32; 4];
        let mut qc = 0u32;
        for i in 0..4 {
            let product = (va[i] as i64) * (vb[i] as i64) * 2;
            if product > i32::MAX as i64 {
                out[i] = i32::MAX;
                qc = 1;
            } else if product < i32::MIN as i64 {
                out[i] = i32::MIN;
                qc = 1;
            } else {
                out[i] = product as i32;
            }
        }
        *result = std::mem::transmute(out);
        qc
    }
}

extern "C" fn fallback_sat_dbl_mul_long32(
    result: *mut [u8; 16],
    a: *const [u8; 16],
    b: *const [u8; 16],
) -> u32 {
    unsafe {
        let va: [i32; 4] = std::mem::transmute(*a);
        let vb: [i32; 4] = std::mem::transmute(*b);
        let mut out = [0i64; 2];
        let mut qc = 0u32;
        for i in 0..2 {
            let product = (va[i] as i128) * (vb[i] as i128) * 2;
            if product > i64::MAX as i128 {
                out[i] = i64::MAX;
                qc = 1;
            } else if product < i64::MIN as i128 {
                out[i] = i64::MIN;
                qc = 1;
            } else {
                out[i] = product as i64;
            }
        }
        *result = std::mem::transmute(out);
        qc
    }
}

pub fn emit_vector_signed_saturated_doubling_multiply_long16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_dbl_mul_long16 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_doubling_multiply_long32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_sat_dbl_mul_long32 as *const () as usize,
    );
}

// ---------------------------------------------------------------------------
// VectorHalvingAdd/Sub — fallback
// ---------------------------------------------------------------------------

macro_rules! define_halving_op {
    ($name:ident, $ty:ty, $wide:ty, $count:expr, $op:ident) => {
        extern "C" fn $name(result: *mut [u8; 16], a: *const [u8; 16], b: *const [u8; 16]) {
            unsafe {
                let va: [$ty; $count] = std::mem::transmute(*a);
                let vb: [$ty; $count] = std::mem::transmute(*b);
                let mut out = [0 as $ty; $count];
                for i in 0..$count {
                    out[i] = (((va[i] as $wide).$op(vb[i] as $wide)) >> 1) as $ty;
                }
                *result = std::mem::transmute(out);
            }
        }
    };
}

define_halving_op!(fallback_halving_add_s8, i8, i16, 16, wrapping_add);
define_halving_op!(fallback_halving_add_s16, i16, i32, 8, wrapping_add);
define_halving_op!(fallback_halving_add_s32, i32, i64, 4, wrapping_add);
define_halving_op!(fallback_halving_add_u8, u8, u16, 16, wrapping_add);
define_halving_op!(fallback_halving_add_u16, u16, u32, 8, wrapping_add);
define_halving_op!(fallback_halving_add_u32, u32, u64, 4, wrapping_add);
define_halving_op!(fallback_halving_sub_s8, i8, i16, 16, wrapping_sub);
define_halving_op!(fallback_halving_sub_s16, i16, i32, 8, wrapping_sub);
define_halving_op!(fallback_halving_sub_s32, i32, i64, 4, wrapping_sub);
define_halving_op!(fallback_halving_sub_u8, u8, u16, 16, wrapping_sub);
define_halving_op!(fallback_halving_sub_u16, u16, u32, 8, wrapping_sub);
define_halving_op!(fallback_halving_sub_u32, u32, u64, 4, wrapping_sub);

pub fn emit_vector_halving_add_signed8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_halving_add_s8 as *const () as usize,
    );
}
pub fn emit_vector_halving_add_signed16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_halving_add_s16 as *const () as usize,
    );
}
pub fn emit_vector_halving_add_signed32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_halving_add_s32 as *const () as usize,
    );
}
pub fn emit_vector_halving_add_unsigned8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_halving_add_u8 as *const () as usize,
    );
}
pub fn emit_vector_halving_add_unsigned16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_halving_add_u16 as *const () as usize,
    );
}
pub fn emit_vector_halving_add_unsigned32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_halving_add_u32 as *const () as usize,
    );
}
pub fn emit_vector_halving_sub_signed8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_halving_sub_s8 as *const () as usize,
    );
}
pub fn emit_vector_halving_sub_signed16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_halving_sub_s16 as *const () as usize,
    );
}
pub fn emit_vector_halving_sub_signed32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_halving_sub_s32 as *const () as usize,
    );
}
pub fn emit_vector_halving_sub_unsigned8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_halving_sub_u8 as *const () as usize,
    );
}
pub fn emit_vector_halving_sub_unsigned16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_halving_sub_u16 as *const () as usize,
    );
}
pub fn emit_vector_halving_sub_unsigned32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_halving_sub_u32 as *const () as usize,
    );
}

// ---------------------------------------------------------------------------
// VectorSignedSaturatedAdd/Sub — fallback with QC flag
// ---------------------------------------------------------------------------

macro_rules! define_saturated_add_sub_op {
    ($name:ident, $ty:ty, $wide:ty, $count:expr, $op:ident, $min:expr, $max:expr) => {
        extern "C" fn $name(result: *mut [u8; 16], a: *const [u8; 16], b: *const [u8; 16]) -> u32 {
            unsafe {
                let va: [$ty; $count] = std::mem::transmute(*a);
                let vb: [$ty; $count] = std::mem::transmute(*b);
                let mut out = [0 as $ty; $count];
                let mut qc = 0u32;
                for i in 0..$count {
                    let wide = (va[i] as $wide).$op(vb[i] as $wide);
                    if wide > ($max as $wide) {
                        out[i] = $max;
                        qc = 1;
                    } else if wide < ($min as $wide) {
                        out[i] = $min;
                        qc = 1;
                    } else {
                        out[i] = wide as $ty;
                    }
                }
                *result = std::mem::transmute(out);
                qc
            }
        }
    };
}

define_saturated_add_sub_op!(
    fallback_signed_sat_add_8,
    i8,
    i16,
    16,
    wrapping_add,
    i8::MIN,
    i8::MAX
);
define_saturated_add_sub_op!(
    fallback_signed_sat_add_16,
    i16,
    i32,
    8,
    wrapping_add,
    i16::MIN,
    i16::MAX
);
define_saturated_add_sub_op!(
    fallback_signed_sat_add_32,
    i32,
    i64,
    4,
    wrapping_add,
    i32::MIN,
    i32::MAX
);
define_saturated_add_sub_op!(
    fallback_signed_sat_add_64,
    i64,
    i128,
    2,
    wrapping_add,
    i64::MIN,
    i64::MAX
);
define_saturated_add_sub_op!(
    fallback_signed_sat_sub_8,
    i8,
    i16,
    16,
    wrapping_sub,
    i8::MIN,
    i8::MAX
);
define_saturated_add_sub_op!(
    fallback_signed_sat_sub_16,
    i16,
    i32,
    8,
    wrapping_sub,
    i16::MIN,
    i16::MAX
);
define_saturated_add_sub_op!(
    fallback_signed_sat_sub_32,
    i32,
    i64,
    4,
    wrapping_sub,
    i32::MIN,
    i32::MAX
);
define_saturated_add_sub_op!(
    fallback_signed_sat_sub_64,
    i64,
    i128,
    2,
    wrapping_sub,
    i64::MIN,
    i64::MAX
);

define_saturated_add_sub_op!(
    fallback_unsigned_sat_add_8,
    u8,
    u16,
    16,
    wrapping_add,
    u8::MIN,
    u8::MAX
);
define_saturated_add_sub_op!(
    fallback_unsigned_sat_add_16,
    u16,
    u32,
    8,
    wrapping_add,
    u16::MIN,
    u16::MAX
);
define_saturated_add_sub_op!(
    fallback_unsigned_sat_add_32,
    u32,
    u64,
    4,
    wrapping_add,
    u32::MIN,
    u32::MAX
);
define_saturated_add_sub_op!(
    fallback_unsigned_sat_add_64,
    u64,
    u128,
    2,
    wrapping_add,
    u64::MIN,
    u64::MAX
);
define_saturated_add_sub_op!(
    fallback_unsigned_sat_sub_8,
    u8,
    u16,
    16,
    wrapping_sub,
    u8::MIN,
    u8::MAX
);
define_saturated_add_sub_op!(
    fallback_unsigned_sat_sub_16,
    u16,
    u32,
    8,
    wrapping_sub,
    u16::MIN,
    u16::MAX
);
define_saturated_add_sub_op!(
    fallback_unsigned_sat_sub_32,
    u32,
    u64,
    4,
    wrapping_sub,
    u32::MIN,
    u32::MAX
);
define_saturated_add_sub_op!(
    fallback_unsigned_sat_sub_64,
    u64,
    u128,
    2,
    wrapping_sub,
    u64::MIN,
    u64::MAX
);

pub fn emit_vector_signed_saturated_add8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_signed_sat_add_8 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_add16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_signed_sat_add_16 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_add32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_signed_sat_add_32 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_add64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_signed_sat_add_64 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_sub8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_signed_sat_sub_8 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_sub16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_signed_sat_sub_16 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_sub32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_signed_sat_sub_32 as *const () as usize,
    );
}
pub fn emit_vector_signed_saturated_sub64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_signed_sat_sub_64 as *const () as usize,
    );
}
pub fn emit_vector_unsigned_saturated_add8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_unsigned_sat_add_8 as *const () as usize,
    );
}
pub fn emit_vector_unsigned_saturated_add16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_unsigned_sat_add_16 as *const () as usize,
    );
}
pub fn emit_vector_unsigned_saturated_add32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_unsigned_sat_add_32 as *const () as usize,
    );
}
pub fn emit_vector_unsigned_saturated_add64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_unsigned_sat_add_64 as *const () as usize,
    );
}
pub fn emit_vector_unsigned_saturated_sub8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_unsigned_sat_sub_8 as *const () as usize,
    );
}
pub fn emit_vector_unsigned_saturated_sub16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_unsigned_sat_sub_16 as *const () as usize,
    );
}
pub fn emit_vector_unsigned_saturated_sub32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_unsigned_sat_sub_32 as *const () as usize,
    );
}
pub fn emit_vector_unsigned_saturated_sub64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback_saturated(
        _ctx,
        ra,
        inst_ref,
        inst,
        fallback_unsigned_sat_sub_64 as *const () as usize,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_saturated_shift_left_unsigned_applies_immediate_to_every_lane() {
        let input = [1_i32, 2, 0x2000_0000, -1];
        let input_bytes: [u8; 16] = unsafe { std::mem::transmute(input) };
        let mut output_bytes = [0_u8; 16];

        let qc = fallback_sat_shlu_s32(&mut output_bytes, &input_bytes, 3);
        let output: [u32; 4] = unsafe { std::mem::transmute(output_bytes) };

        assert_eq!(output, [8, 16, u32::MAX, 0]);
        assert_eq!(qc, 1);
    }

    #[test]
    fn test_fn_signatures() {
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_signed_saturated_abs8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_signed_saturated_neg64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_signed_saturated_accumulate_unsigned8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_unsigned_saturated_accumulate_signed64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_signed_saturated_narrow_to_signed16;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_signed_saturated_narrow_to_unsigned64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_unsigned_saturated_narrow64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_signed_saturated_shift_left8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_vector_unsigned_saturated_shift_left64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_halving_add_signed8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_vector_halving_sub_unsigned32;
    }
}
