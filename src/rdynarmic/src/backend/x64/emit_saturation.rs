use rxbyak::{byte_ptr, Reg, RegExp, R15};

use crate::backend::x64::emit_context::EmitContext;
use crate::backend::x64::jitstate_info::JitStateInfo;
use crate::backend::x64::reg_alloc::RegAlloc;
use crate::ir::inst::Inst;
use crate::ir::value::InstRef;

// ---------------------------------------------------------------------------
// Shared saturation helpers
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SaturationOp {
    Add,
    Sub,
}

fn emit_or_qc(ra: &mut RegAlloc, jit_state_info: JitStateInfo, overflow: Reg) {
    let offset = jit_state_info.offsetof_fpsr_qc;
    ra.asm
        .or_(
            byte_ptr(RegExp::from(R15) + offset as i32),
            overflow.cvt8().unwrap(),
        )
        .unwrap();
}

// ---------------------------------------------------------------------------
fn emit_signed_saturated_op(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    op: SaturationOp,
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

    match (op, bitsize) {
        (SaturationOp::Add, 8) => ra
            .asm
            .add(result.cvt8().unwrap(), op2.cvt8().unwrap())
            .unwrap(),
        (SaturationOp::Add, 16) => ra
            .asm
            .add(result.cvt16().unwrap(), op2.cvt16().unwrap())
            .unwrap(),
        (SaturationOp::Add, 32) => ra
            .asm
            .add(result.cvt32().unwrap(), op2.cvt32().unwrap())
            .unwrap(),
        (SaturationOp::Add, 64) => ra.asm.add(result, op2).unwrap(),
        (SaturationOp::Sub, 8) => ra
            .asm
            .sub(result.cvt8().unwrap(), op2.cvt8().unwrap())
            .unwrap(),
        (SaturationOp::Sub, 16) => ra
            .asm
            .sub(result.cvt16().unwrap(), op2.cvt16().unwrap())
            .unwrap(),
        (SaturationOp::Sub, 32) => ra
            .asm
            .sub(result.cvt32().unwrap(), op2.cvt32().unwrap())
            .unwrap(),
        (SaturationOp::Sub, 64) => ra.asm.sub(result, op2).unwrap(),
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
        emit_or_qc(ra, ctx.jit_state_info, sat_val);
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
    emit_signed_saturated_op(
        ctx,
        ra,
        inst_ref,
        inst,
        SaturationOp::Add,
        32,
        true,
        overflow_inst,
    );
}

pub fn emit_signed_saturated_add8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_signed_saturated_op(ctx, ra, inst_ref, inst, SaturationOp::Add, 8, false, None);
}
pub fn emit_signed_saturated_add16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_signed_saturated_op(ctx, ra, inst_ref, inst, SaturationOp::Add, 16, false, None);
}
pub fn emit_signed_saturated_add32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_signed_saturated_op(ctx, ra, inst_ref, inst, SaturationOp::Add, 32, false, None);
}
pub fn emit_signed_saturated_add64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_signed_saturated_op(ctx, ra, inst_ref, inst, SaturationOp::Add, 64, false, None);
}

// ---------------------------------------------------------------------------
// Signed saturated sub: result = clamp(a - b, MIN, MAX), set QC on overflow
// ---------------------------------------------------------------------------

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
    emit_signed_saturated_op(
        ctx,
        ra,
        inst_ref,
        inst,
        SaturationOp::Sub,
        32,
        true,
        overflow_inst,
    );
}

pub fn emit_signed_saturated_sub8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_signed_saturated_op(ctx, ra, inst_ref, inst, SaturationOp::Sub, 8, false, None);
}
pub fn emit_signed_saturated_sub16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_signed_saturated_op(ctx, ra, inst_ref, inst, SaturationOp::Sub, 16, false, None);
}
pub fn emit_signed_saturated_sub32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_signed_saturated_op(ctx, ra, inst_ref, inst, SaturationOp::Sub, 32, false, None);
}
pub fn emit_signed_saturated_sub64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_signed_saturated_op(ctx, ra, inst_ref, inst, SaturationOp::Sub, 64, false, None);
}

// ---------------------------------------------------------------------------
// Unsigned saturated add: result = min(a + b, MAX), set QC on carry
// ---------------------------------------------------------------------------

fn emit_unsigned_saturated_op(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    op: SaturationOp,
    bitsize: usize,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let op_result = ra.use_scratch_gpr(&mut args[0]);
    let addend = ra.use_scratch_gpr(&mut args[1]);

    match (op, bitsize) {
        (SaturationOp::Add, 8) => {
            ra.asm
                .add(op_result.cvt8().unwrap(), addend.cvt8().unwrap())
                .unwrap();
        }
        (SaturationOp::Add, 16) => {
            ra.asm
                .add(op_result.cvt16().unwrap(), addend.cvt16().unwrap())
                .unwrap();
        }
        (SaturationOp::Add, 32) => {
            ra.asm
                .add(op_result.cvt32().unwrap(), addend.cvt32().unwrap())
                .unwrap();
        }
        (SaturationOp::Add, 64) => {
            ra.asm.add(op_result, addend).unwrap();
        }
        (SaturationOp::Sub, 8) => {
            ra.asm
                .sub(op_result.cvt8().unwrap(), addend.cvt8().unwrap())
                .unwrap();
        }
        (SaturationOp::Sub, 16) => {
            ra.asm
                .sub(op_result.cvt16().unwrap(), addend.cvt16().unwrap())
                .unwrap();
        }
        (SaturationOp::Sub, 32) => {
            ra.asm
                .sub(op_result.cvt32().unwrap(), addend.cvt32().unwrap())
                .unwrap();
        }
        (SaturationOp::Sub, 64) => ra.asm.sub(op_result, addend).unwrap(),
        _ => unreachable!(),
    }

    let boundary = match op {
        SaturationOp::Add if bitsize == 64 => u64::MAX,
        SaturationOp::Add => (1u64 << bitsize) - 1,
        SaturationOp::Sub => 0,
    };
    match bitsize {
        8 => ra.asm.mov(addend.cvt8().unwrap(), boundary as i32).unwrap(),
        16 => ra
            .asm
            .mov(addend.cvt16().unwrap(), boundary as i32)
            .unwrap(),
        32 => ra
            .asm
            .mov(addend.cvt32().unwrap(), boundary as i32)
            .unwrap(),
        64 => ra.asm.mov(addend, boundary as i64).unwrap(),
        _ => unreachable!(),
    }
    if bitsize == 8 {
        ra.asm
            .cmovae(addend.cvt32().unwrap(), op_result.cvt32().unwrap())
            .unwrap();
    } else {
        let addend_width = match bitsize {
            16 => addend.cvt16().unwrap(),
            32 => addend.cvt32().unwrap(),
            64 => addend,
            _ => unreachable!(),
        };
        let result_width = match bitsize {
            16 => op_result.cvt16().unwrap(),
            32 => op_result.cvt32().unwrap(),
            64 => op_result,
            _ => unreachable!(),
        };
        ra.asm.cmovae(addend_width, result_width).unwrap();
    }

    let overflow = ra.scratch_gpr();
    ra.asm.setb(overflow.cvt8().unwrap()).unwrap();
    emit_or_qc(ra, ctx.jit_state_info, overflow);
    ra.release(overflow);
    ra.define_value(inst_ref, addend);
}

pub fn emit_unsigned_saturated_add8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_unsigned_saturated_op(ctx, ra, inst_ref, inst, SaturationOp::Add, 8);
}
pub fn emit_unsigned_saturated_add16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_unsigned_saturated_op(ctx, ra, inst_ref, inst, SaturationOp::Add, 16);
}
pub fn emit_unsigned_saturated_add32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_unsigned_saturated_op(ctx, ra, inst_ref, inst, SaturationOp::Add, 32);
}
pub fn emit_unsigned_saturated_add64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_unsigned_saturated_op(ctx, ra, inst_ref, inst, SaturationOp::Add, 64);
}

// ---------------------------------------------------------------------------
// Unsigned saturated sub: result = max(a - b, 0), set QC on borrow
// ---------------------------------------------------------------------------

pub fn emit_unsigned_saturated_sub8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_unsigned_saturated_op(ctx, ra, inst_ref, inst, SaturationOp::Sub, 8);
}
pub fn emit_unsigned_saturated_sub16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_unsigned_saturated_op(ctx, ra, inst_ref, inst, SaturationOp::Sub, 16);
}
pub fn emit_unsigned_saturated_sub32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_unsigned_saturated_op(ctx, ra, inst_ref, inst, SaturationOp::Sub, 32);
}
pub fn emit_unsigned_saturated_sub64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_unsigned_saturated_op(ctx, ra, inst_ref, inst, SaturationOp::Sub, 64);
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
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_signed_saturated_doubling_multiply_return_high(ctx, ra, inst_ref, inst, 16);
}

pub fn emit_signed_saturated_doubling_multiply_return_high32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_signed_saturated_doubling_multiply_return_high(ctx, ra, inst_ref, inst, 32);
}

fn emit_signed_saturated_doubling_multiply_return_high(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    bitsize: usize,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());

    match bitsize {
        16 => {
            let x = ra.use_scratch_gpr(&mut args[0]);
            let y = ra.use_scratch_gpr(&mut args[1]);
            let tmp = ra.scratch_gpr();

            ra.asm
                .movsx(x.cvt32().unwrap(), x.cvt16().unwrap())
                .unwrap();
            ra.asm
                .movsx(y.cvt32().unwrap(), y.cvt16().unwrap())
                .unwrap();
            ra.asm.imul(x.cvt32().unwrap(), y.cvt32().unwrap()).unwrap();
            ra.asm
                .lea(
                    y.cvt32().unwrap(),
                    rxbyak::ptr(RegExp::from(x) + RegExp::from(x)),
                )
                .unwrap();
            ra.asm
                .mov(tmp.cvt32().unwrap(), x.cvt32().unwrap())
                .unwrap();
            ra.asm.shr(tmp.cvt32().unwrap(), 15u8).unwrap();
            ra.asm.xor_(y.cvt32().unwrap(), x.cvt32().unwrap()).unwrap();
            ra.asm.mov(y.cvt32().unwrap(), 0x7FFF).unwrap();
            ra.asm
                .cmovns(y.cvt32().unwrap(), tmp.cvt32().unwrap())
                .unwrap();
            ra.asm.sets(tmp.cvt8().unwrap()).unwrap();
            emit_or_qc(ra, ctx.jit_state_info, tmp);
            ra.release(tmp);
            ra.define_value(inst_ref, y);
        }
        32 => {
            let x = ra.use_scratch_gpr(&mut args[0]);
            let y = ra.use_scratch_gpr(&mut args[1]);
            let tmp = ra.scratch_gpr();

            ra.asm.movsxd(x, x.cvt32().unwrap()).unwrap();
            ra.asm.movsxd(y, y.cvt32().unwrap()).unwrap();
            ra.asm.imul(x, y).unwrap();
            ra.asm
                .lea(y, rxbyak::ptr(RegExp::from(x) + RegExp::from(x)))
                .unwrap();
            ra.asm.mov(tmp, x).unwrap();
            ra.asm.shr(tmp, 31u8).unwrap();
            ra.asm.xor_(y, x).unwrap();
            ra.asm.mov(y.cvt32().unwrap(), 0x7FFF_FFFF).unwrap();
            ra.asm
                .cmovns(y.cvt32().unwrap(), tmp.cvt32().unwrap())
                .unwrap();
            ra.asm.sets(tmp.cvt8().unwrap()).unwrap();
            emit_or_qc(ra, ctx.jit_state_info, tmp);
            ra.release(tmp);
            ra.define_value(inst_ref, y);
        }
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qc_write_uses_the_selected_jit_state_layout() {
        let mut asm = rxbyak::CodeAssembler::new(4096).unwrap();
        {
            let mut ra = RegAlloc::new_default(&mut asm, vec![]);
            let overflow = ra.scratch_gpr();
            emit_or_qc(&mut ra, JitStateInfo::from_a32(), overflow);
        }

        let code = asm.code();
        let a32_displacement = (JitStateInfo::from_a32().offsetof_fpsr_qc as u32).to_le_bytes();
        let a64_displacement = (JitStateInfo::from_a64().offsetof_fpsr_qc as u32).to_le_bytes();
        assert!(
            code.windows(a32_displacement.len())
                .any(|window| window == a32_displacement),
            "QC write must address the A32 fpsr_qc field"
        );
        assert!(
            !code
                .windows(a64_displacement.len())
                .any(|window| window == a64_displacement),
            "QC write must not retain the former A64-only displacement"
        );
    }

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
