use rxbyak::JmpType;
use rxbyak::RegExp;
use rxbyak::R15;
use rxbyak::{byte_ptr, dword_ptr};

use crate::backend::x64::a64_jitstate::A64JitState;
use crate::backend::x64::emit_context::EmitContext;
use crate::backend::x64::reg_alloc::RegAlloc;
use crate::ir::inst::Inst;
use crate::ir::value::InstRef;

// ---------------------------------------------------------------------------
// Helper: OR the QC flag in jit_state
// ---------------------------------------------------------------------------

/// `or [r15 + fpsr_qc], 1` — sets the QC (saturation) sticky flag.
fn set_qc_flag(ra: &mut RegAlloc) {
    let offset = A64JitState::offset_of_fpsr_qc();
    ra.asm
        .or_(dword_ptr(RegExp::from(R15) + offset as i32), 1)
        .unwrap();
}

// ---------------------------------------------------------------------------
// Signed saturated add: result = clamp(a + b, MIN, MAX), set QC on overflow
// ---------------------------------------------------------------------------

fn emit_signed_saturated_add(
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    bitsize: usize,
    has_overflow_inst: bool,
    overflow_inst: Option<InstRef>,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    let op2 = ra.use_gpr(&mut args[1]);

    // Compute saturation value: sign bit of result → sat_val = MAX or MIN.
    let sat_val = ra.scratch_gpr();
    if bitsize < 64 {
        let int_max = (1u64 << (bitsize - 1)) - 1;
        ra.asm
            .xor_(sat_val.cvt32().unwrap(), sat_val.cvt32().unwrap())
            .unwrap();
        ra.asm
            .bt_imm(result.cvt32().unwrap(), (bitsize - 1) as u8)
            .unwrap();
        ra.asm
            .adc(sat_val.cvt32().unwrap(), int_max as i32)
            .unwrap();
    } else {
        ra.asm.mov(sat_val, i64::MAX).unwrap();
        ra.asm.bt_imm(result, 63).unwrap();
        ra.asm.adc(sat_val, 0i32).unwrap();
    }

    match bitsize {
        8 => ra
            .asm
            .add(result.cvt8().unwrap(), op2.cvt8().unwrap())
            .unwrap(),
        16 => ra
            .asm
            .add(result.cvt16().unwrap(), op2.cvt16().unwrap())
            .unwrap(),
        32 => ra
            .asm
            .add(result.cvt32().unwrap(), op2.cvt32().unwrap())
            .unwrap(),
        64 => ra.asm.add(result, op2).unwrap(),
        _ => unreachable!(),
    }

    // On overflow (OF=1), use the saturation value instead. x86 CMOV does
    // not have an 8-bit form, matching upstream's 32-bit operation there.
    if bitsize == 8 {
        ra.asm
            .cmovo(result.cvt32().unwrap(), sat_val.cvt32().unwrap())
            .unwrap();
    } else {
        let result = match bitsize {
            16 => result.cvt16().unwrap(),
            32 => result.cvt32().unwrap(),
            64 => result,
            _ => unreachable!(),
        };
        let sat_val_width = match bitsize {
            16 => sat_val.cvt16().unwrap(),
            32 => sat_val.cvt32().unwrap(),
            64 => sat_val,
            _ => unreachable!(),
        };
        ra.asm.cmovo(result, sat_val_width).unwrap();
    }

    ra.asm.seto(sat_val.cvt8().unwrap()).unwrap();

    if has_overflow_inst {
        if let Some(overflow_inst) = overflow_inst {
            ra.define_value(overflow_inst, sat_val);
        } else {
            ra.release(sat_val);
        }
    } else {
        let offset = A64JitState::offset_of_fpsr_qc();
        ra.asm
            .or_(
                byte_ptr(RegExp::from(R15) + offset as i32),
                sat_val.cvt8().unwrap(),
            )
            .unwrap();
        ra.release(sat_val);
    }

    ra.define_value(inst_ref, result);
}

pub fn emit_signed_saturated_add_with_flag32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let overflow_inst = ctx.block.and_then(|block| {
        block
            .get_associated_pseudo_operation(inst_ref, crate::ir::opcode::Opcode::GetOverflowFromOp)
    });
    emit_signed_saturated_add(ra, inst_ref, inst, 32, true, overflow_inst);
}

pub fn emit_signed_saturated_add8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_signed_saturated_add(ra, inst_ref, inst, 8, false, None);
}
pub fn emit_signed_saturated_add16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_signed_saturated_add(ra, inst_ref, inst, 16, false, None);
}
pub fn emit_signed_saturated_add32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_signed_saturated_add(ra, inst_ref, inst, 32, false, None);
}
pub fn emit_signed_saturated_add64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_signed_saturated_add(ra, inst_ref, inst, 64, false, None);
}

// ---------------------------------------------------------------------------
// Signed saturated sub: result = clamp(a - b, MIN, MAX), set QC on overflow
// ---------------------------------------------------------------------------

fn emit_signed_saturated_sub(
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    bitsize: usize,
    has_overflow_inst: bool,
    overflow_inst: Option<InstRef>,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    let op2 = ra.use_gpr(&mut args[1]);

    let sat_val = ra.scratch_gpr();
    if bitsize < 64 {
        let int_max = (1u64 << (bitsize - 1)) - 1;
        ra.asm
            .xor_(sat_val.cvt32().unwrap(), sat_val.cvt32().unwrap())
            .unwrap();
        ra.asm
            .bt_imm(result.cvt32().unwrap(), (bitsize - 1) as u8)
            .unwrap();
        ra.asm
            .adc(sat_val.cvt32().unwrap(), int_max as i32)
            .unwrap();
    } else {
        ra.asm.mov(sat_val, i64::MAX).unwrap();
        ra.asm.bt_imm(result, 63).unwrap();
        ra.asm.adc(sat_val, 0i32).unwrap();
    }

    match bitsize {
        8 => ra
            .asm
            .sub(result.cvt8().unwrap(), op2.cvt8().unwrap())
            .unwrap(),
        16 => ra
            .asm
            .sub(result.cvt16().unwrap(), op2.cvt16().unwrap())
            .unwrap(),
        32 => ra
            .asm
            .sub(result.cvt32().unwrap(), op2.cvt32().unwrap())
            .unwrap(),
        64 => ra.asm.sub(result, op2).unwrap(),
        _ => unreachable!(),
    }

    if bitsize == 8 {
        ra.asm
            .cmovo(result.cvt32().unwrap(), sat_val.cvt32().unwrap())
            .unwrap();
    } else {
        let result = match bitsize {
            16 => result.cvt16().unwrap(),
            32 => result.cvt32().unwrap(),
            64 => result,
            _ => unreachable!(),
        };
        let sat_val_width = match bitsize {
            16 => sat_val.cvt16().unwrap(),
            32 => sat_val.cvt32().unwrap(),
            64 => sat_val,
            _ => unreachable!(),
        };
        ra.asm.cmovo(result, sat_val_width).unwrap();
    }

    ra.asm.seto(sat_val.cvt8().unwrap()).unwrap();

    if has_overflow_inst {
        if let Some(overflow_inst) = overflow_inst {
            ra.define_value(overflow_inst, sat_val);
        } else {
            ra.release(sat_val);
        }
    } else {
        let offset = A64JitState::offset_of_fpsr_qc();
        ra.asm
            .or_(
                byte_ptr(RegExp::from(R15) + offset as i32),
                sat_val.cvt8().unwrap(),
            )
            .unwrap();
        ra.release(sat_val);
    }

    ra.define_value(inst_ref, result);
}

pub fn emit_signed_saturated_sub_with_flag32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let overflow_inst = ctx.block.and_then(|block| {
        block
            .get_associated_pseudo_operation(inst_ref, crate::ir::opcode::Opcode::GetOverflowFromOp)
    });
    emit_signed_saturated_sub(ra, inst_ref, inst, 32, true, overflow_inst);
}

pub fn emit_signed_saturated_sub8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_signed_saturated_sub(ra, inst_ref, inst, 8, false, None);
}
pub fn emit_signed_saturated_sub16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_signed_saturated_sub(ra, inst_ref, inst, 16, false, None);
}
pub fn emit_signed_saturated_sub32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_signed_saturated_sub(ra, inst_ref, inst, 32, false, None);
}
pub fn emit_signed_saturated_sub64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_signed_saturated_sub(ra, inst_ref, inst, 64, false, None);
}

// ---------------------------------------------------------------------------
// Unsigned saturated add: result = min(a + b, MAX), set QC on carry
// ---------------------------------------------------------------------------

fn emit_unsigned_saturated_add(ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst, bitsize: usize) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    let op2 = ra.use_gpr(&mut args[1]);

    match bitsize {
        8 => {
            ra.asm
                .add(result.cvt8().unwrap(), op2.cvt8().unwrap())
                .unwrap();
        }
        16 => {
            ra.asm
                .add(result.cvt16().unwrap(), op2.cvt16().unwrap())
                .unwrap();
        }
        32 => {
            ra.asm
                .add(result.cvt32().unwrap(), op2.cvt32().unwrap())
                .unwrap();
        }
        64 => {
            ra.asm.add(result, op2).unwrap();
        }
        _ => unreachable!(),
    }

    // On carry (CF=1), set result to all-ones (MAX)
    let sat_val = ra.scratch_gpr();
    ra.asm.mov(sat_val.cvt32().unwrap(), -1i32).unwrap();
    if bitsize == 64 {
        ra.asm.mov(sat_val, -1i64).unwrap();
    }
    ra.asm.cmovb(result, sat_val).unwrap();

    // Set QC if carry
    let label_no_carry = ra.asm.create_label();
    ra.asm.jae(&label_no_carry, JmpType::Near).unwrap();
    set_qc_flag(ra);
    ra.asm.bind(&label_no_carry).unwrap();

    ra.release(sat_val);
    ra.define_value(inst_ref, result);
}

pub fn emit_unsigned_saturated_add8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_unsigned_saturated_add(ra, inst_ref, inst, 8);
}
pub fn emit_unsigned_saturated_add16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_unsigned_saturated_add(ra, inst_ref, inst, 16);
}
pub fn emit_unsigned_saturated_add32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_unsigned_saturated_add(ra, inst_ref, inst, 32);
}
pub fn emit_unsigned_saturated_add64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_unsigned_saturated_add(ra, inst_ref, inst, 64);
}

// ---------------------------------------------------------------------------
// Unsigned saturated sub: result = max(a - b, 0), set QC on borrow
// ---------------------------------------------------------------------------

fn emit_unsigned_saturated_sub(ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst, bitsize: usize) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    let op2 = ra.use_gpr(&mut args[1]);

    match bitsize {
        8 => {
            ra.asm
                .sub(result.cvt8().unwrap(), op2.cvt8().unwrap())
                .unwrap();
        }
        16 => {
            ra.asm
                .sub(result.cvt16().unwrap(), op2.cvt16().unwrap())
                .unwrap();
        }
        32 => {
            ra.asm
                .sub(result.cvt32().unwrap(), op2.cvt32().unwrap())
                .unwrap();
        }
        64 => {
            ra.asm.sub(result, op2).unwrap();
        }
        _ => unreachable!(),
    }

    // On borrow (CF=1), set result to 0
    let zero = ra.scratch_gpr();
    ra.asm
        .xor_(zero.cvt32().unwrap(), zero.cvt32().unwrap())
        .unwrap();
    ra.asm.cmovb(result, zero).unwrap();

    // Set QC if borrow
    let label_no_borrow = ra.asm.create_label();
    ra.asm.jae(&label_no_borrow, JmpType::Near).unwrap();
    set_qc_flag(ra);
    ra.asm.bind(&label_no_borrow).unwrap();

    ra.release(zero);
    ra.define_value(inst_ref, result);
}

pub fn emit_unsigned_saturated_sub8(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_unsigned_saturated_sub(ra, inst_ref, inst, 8);
}
pub fn emit_unsigned_saturated_sub16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_unsigned_saturated_sub(ra, inst_ref, inst, 16);
}
pub fn emit_unsigned_saturated_sub32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_unsigned_saturated_sub(ra, inst_ref, inst, 32);
}
pub fn emit_unsigned_saturated_sub64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_unsigned_saturated_sub(ra, inst_ref, inst, 64);
}

// ---------------------------------------------------------------------------
// SignedSaturation: clamp value to signed N-bit range and expose overflow
// Args: (value: U32, bit_width: U8)
// ---------------------------------------------------------------------------

pub fn emit_signed_saturation(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let overflow_inst = ctx.block.and_then(|block| {
        block
            .get_associated_pseudo_operation(inst_ref, crate::ir::opcode::Opcode::GetOverflowFromOp)
    });
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let n = args[1].get_immediate_u8();
    assert!((1..=32).contains(&n));

    let source = ra.use_gpr(&mut args[0]);
    let result = ra.scratch_gpr();
    if n == 32 {
        ra.asm
            .mov(result.cvt32().unwrap(), source.cvt32().unwrap())
            .unwrap();
        if let Some(overflow_inst) = overflow_inst {
            let overflow = ra.scratch_gpr();
            ra.asm
                .xor_(overflow.cvt32().unwrap(), overflow.cvt32().unwrap())
                .unwrap();
            ra.define_value(overflow_inst, overflow);
        }
        ra.define_value(inst_ref, result);
        return;
    }

    let mask = (1u32 << n) - 1;
    let positive_saturated_value = (1u32 << (n - 1)) - 1;
    let negative_saturated_value = 1u32 << (n - 1);
    let overflow = ra.scratch_gpr();

    ra.asm
        .lea(
            overflow.cvt32().unwrap(),
            rxbyak::ptr(RegExp::from(source.cvt64().unwrap()) + negative_saturated_value as i32),
        )
        .unwrap();
    ra.asm
        .mov(result.cvt32().unwrap(), source.cvt32().unwrap())
        .unwrap();
    ra.asm.sar(result.cvt32().unwrap(), 31u8).unwrap();
    ra.asm
        .xor_(result.cvt32().unwrap(), positive_saturated_value as i32)
        .unwrap();
    ra.asm.cmp(overflow.cvt32().unwrap(), mask as i32).unwrap();
    ra.asm
        .cmovbe(result.cvt32().unwrap(), source.cvt32().unwrap())
        .unwrap();

    if let Some(overflow_inst) = overflow_inst {
        ra.asm.seta(overflow.cvt8().unwrap()).unwrap();
        ra.define_value(overflow_inst, overflow);
    } else {
        ra.release(overflow);
    }
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// UnsignedSaturation: clamp value to unsigned N-bit range and expose overflow
// Args: (value: U32, bit_width: U8)
// ---------------------------------------------------------------------------

pub fn emit_unsigned_saturation(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let overflow_inst = ctx.block.and_then(|block| {
        block
            .get_associated_pseudo_operation(inst_ref, crate::ir::opcode::Opcode::GetOverflowFromOp)
    });
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let n = args[1].get_immediate_u8();
    assert!(n <= 31);

    let saturated_value = (1u32 << n) - 1;
    let source = ra.use_gpr(&mut args[0]);
    let result = ra.scratch_gpr();
    let overflow = ra.scratch_gpr();
    ra.asm
        .xor_(overflow.cvt32().unwrap(), overflow.cvt32().unwrap())
        .unwrap();
    ra.asm
        .cmp(source.cvt32().unwrap(), saturated_value as i32)
        .unwrap();
    ra.asm
        .mov(result.cvt32().unwrap(), saturated_value as i32)
        .unwrap();
    ra.asm
        .cmovle(result.cvt32().unwrap(), overflow.cvt32().unwrap())
        .unwrap();
    ra.asm
        .cmovbe(result.cvt32().unwrap(), source.cvt32().unwrap())
        .unwrap();

    if let Some(overflow_inst) = overflow_inst {
        ra.asm.seta(overflow.cvt8().unwrap()).unwrap();
        ra.define_value(overflow_inst, overflow);
    } else {
        ra.release(overflow);
    }
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// SignedSaturatedDoublingMultiplyReturnHigh: (a * b * 2) >> N, with saturation
// Args: (a: U16/U32, b: U16/U32)
// ---------------------------------------------------------------------------

pub fn emit_signed_saturated_doubling_multiply_return_high16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_signed_saturated_doubling_multiply_return_high(ra, inst_ref, inst, 16);
}

pub fn emit_signed_saturated_doubling_multiply_return_high32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_signed_saturated_doubling_multiply_return_high(ra, inst_ref, inst, 32);
}

fn emit_signed_saturated_doubling_multiply_return_high(
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    bitsize: usize,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());

    match bitsize {
        16 => {
            // Sign-extend both to 32-bit, multiply, shift >> 15 (double >> 16 = >> 15)
            let a = ra.use_scratch_gpr(&mut args[0]);
            let b = ra.use_gpr(&mut args[1]);

            // Sign-extend 16→32
            ra.asm
                .movsx(a.cvt32().unwrap(), a.cvt16().unwrap())
                .unwrap();
            let b_ext = ra.scratch_gpr();
            ra.asm
                .movsx(b_ext.cvt32().unwrap(), b.cvt16().unwrap())
                .unwrap();

            // imul
            ra.asm
                .imul(a.cvt32().unwrap(), b_ext.cvt32().unwrap())
                .unwrap();
            // Double and take high half: (a*b*2) >> 16 = (a*b) >> 15
            ra.asm.sar(a.cvt32().unwrap(), 15u8).unwrap();

            // Check for INT16_MIN * INT16_MIN overflow (result should be INT16_MAX)
            ra.asm.cmp(a.cvt32().unwrap(), 0x8000i32).unwrap();
            let label_no_overflow = ra.asm.create_label();
            ra.asm.jne(&label_no_overflow, JmpType::Near).unwrap();
            ra.asm.mov(a.cvt32().unwrap(), 0x7FFFi32).unwrap();
            set_qc_flag(ra);
            ra.asm.bind(&label_no_overflow).unwrap();

            ra.release(b_ext);
            ra.define_value(inst_ref, a);
        }
        32 => {
            // Sign-extend both to 64-bit, multiply, shift >> 31
            let a = ra.use_scratch_gpr(&mut args[0]);
            let b = ra.use_gpr(&mut args[1]);

            ra.asm.movsxd(a, a.cvt32().unwrap()).unwrap();
            let b_ext = ra.scratch_gpr();
            ra.asm.movsxd(b_ext, b.cvt32().unwrap()).unwrap();

            ra.asm.imul(a, b_ext).unwrap();
            ra.asm.sar(a, 31u8).unwrap();

            // Check for INT32_MIN * INT32_MIN overflow
            ra.asm.mov(b_ext, 0x8000_0000i64).unwrap();
            ra.asm.cmp(a, b_ext).unwrap();
            let label_no_overflow = ra.asm.create_label();
            ra.asm.jne(&label_no_overflow, JmpType::Near).unwrap();
            ra.asm.mov(a.cvt32().unwrap(), 0x7FFF_FFFFi32).unwrap();
            set_qc_flag(ra);
            ra.asm.bind(&label_no_overflow).unwrap();

            ra.release(b_ext);
            ra.define_value(inst_ref, a);
        }
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_saturation_fn_signatures() {
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_signed_saturated_add8;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_signed_saturated_add_with_flag32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_signed_saturated_add64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_signed_saturated_sub_with_flag32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_unsigned_saturated_sub32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_signed_saturation;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_unsigned_saturation;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_signed_saturated_doubling_multiply_return_high16;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_signed_saturated_doubling_multiply_return_high32;
    }
}
