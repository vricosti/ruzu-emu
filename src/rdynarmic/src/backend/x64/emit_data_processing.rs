use rxbyak::dword_ptr;
use rxbyak::{JmpType, Reg, RegExp};
use rxbyak::{CL, R15};

use crate::backend::x64::emit_context::EmitContext;
use crate::backend::x64::host_feature::HostFeature;
use crate::backend::x64::hostloc::*;
use crate::backend::x64::nzcv_util;
use crate::backend::x64::reg_alloc::Argument;
use crate::backend::x64::reg_alloc::RegAlloc;
use crate::backend::x64::value_classify::{ir_value_is_vector_backed, ir_value_resolves_to_xmm};
use crate::ir::inst::Inst;
use crate::ir::types::Type;
use crate::ir::value::InstRef;

// ---------------------------------------------------------------------------
// Helper: load ARM NZCV into x86 flags for conditional operations
// ---------------------------------------------------------------------------

fn use_scalar_gpr_read(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    arg: &mut Argument,
    bitsize: usize,
) -> Reg {
    if ir_value_resolves_to_xmm(ctx, ra, &arg.value) || ir_value_is_vector_backed(ctx, &arg.value) {
        let source = ra.use_xmm(arg);
        let result = ra.scratch_gpr();
        if bitsize == 64 {
            ra.asm.movq(result, source).unwrap();
        } else {
            ra.asm.movd(result.cvt32().unwrap(), source).unwrap();
        }
        result
    } else {
        ra.use_gpr(arg)
    }
}

fn use_scalar_gpr_scratch(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    arg: &mut Argument,
    bitsize: usize,
) -> Reg {
    if ir_value_resolves_to_xmm(ctx, ra, &arg.value) || ir_value_is_vector_backed(ctx, &arg.value) {
        let source = ra.use_xmm(arg);
        let result = ra.scratch_gpr();
        if bitsize == 64 {
            ra.asm.movq(result, source).unwrap();
        } else {
            ra.asm.movd(result.cvt32().unwrap(), source).unwrap();
        }
        result
    } else {
        ra.use_scratch_gpr(arg)
    }
}

fn load_nzcv_into_flags_with_rax(
    ra: &mut RegAlloc,
    rax: Reg,
    cond: crate::ir::cond::Cond,
    cpsr_nzcv_offset: usize,
) {
    ra.asm
        .mov(
            rax.cvt32().unwrap(),
            dword_ptr(RegExp::from(R15) + cpsr_nzcv_offset as i32),
        )
        .unwrap();

    // Restore required flags based on condition
    use crate::ir::cond::Cond;
    match cond {
        // Only need SF/ZF/CF — SAHF is sufficient
        Cond::EQ | Cond::NE | Cond::CS | Cond::CC | Cond::MI | Cond::PL => {
            ra.asm.sahf().unwrap();
        }
        // Only need OF
        Cond::VS | Cond::VC => {
            ra.asm.cmp(rax.cvt8().unwrap(), 0x81u32 as i32).unwrap();
        }
        // Need CF and ZF — SAHF restores ARM-convention carry (C=1 = no borrow
        // after SUB, stored via cmc() after sub/sbb).  x86 `ja` / `jbe` expect
        // x86-native CF, so we invert once more.  Matches dynarmic's
        // `LoadRequiredFlagsForCondFromRax` which does `sahf(); cmc();` for HI/LS.
        Cond::HI | Cond::LS => {
            ra.asm.sahf().unwrap();
            ra.asm.cmc().unwrap();
        }
        // Need SF, ZF, OF — restore both
        Cond::GE | Cond::LT | Cond::GT | Cond::LE => {
            ra.asm.cmp(rax.cvt8().unwrap(), 0x81u32 as i32).unwrap();
            ra.asm.sahf().unwrap();
        }
        // Always/never
        Cond::AL | Cond::NV => {}
    }
}

/// Load NZCV from jit_state into x86 flags via RAX.
/// After this call, x86 flags reflect the ARM condition codes.
/// `cpsr_nzcv_offset` is the byte offset of the cpsr_nzcv field in the
/// architecture-specific JitState (A64 or A32).
pub fn load_nzcv_into_flags(
    ra: &mut RegAlloc,
    cond: crate::ir::cond::Cond,
    cpsr_nzcv_offset: usize,
) {
    let rax = ra.scratch_gpr_at(HOST_RAX);
    load_nzcv_into_flags_with_rax(ra, rax, cond, cpsr_nzcv_offset);
}

/// Emit the appropriate cmovcc for an ARM condition code.
fn emit_cmovcc(asm: &mut rxbyak::CodeAssembler, cond: crate::ir::cond::Cond, dst: Reg, src: Reg) {
    use crate::ir::cond::Cond;
    let r = match cond {
        Cond::EQ => asm.cmovz(dst, src),
        Cond::NE => asm.cmovnz(dst, src),
        Cond::CS => asm.cmovc(dst, src),
        Cond::CC => asm.cmovnc(dst, src),
        Cond::MI => asm.cmovs(dst, src),
        Cond::PL => asm.cmovns(dst, src),
        Cond::VS => asm.cmovo(dst, src),
        Cond::VC => asm.cmovno(dst, src),
        Cond::HI => asm.cmova(dst, src),
        Cond::LS => asm.cmovbe(dst, src),
        Cond::GE => asm.cmovge(dst, src),
        Cond::LT => asm.cmovl(dst, src),
        Cond::GT => asm.cmovg(dst, src),
        Cond::LE => asm.cmovle(dst, src),
        Cond::AL | Cond::NV => asm.mov(dst, src),
    };
    r.unwrap();
}

// ---------------------------------------------------------------------------
// Arithmetic: Add / Sub
// ---------------------------------------------------------------------------

/// Add32: result = a + b + carry_in
pub fn emit_add32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_add(ctx, ra, inst_ref, inst, 32);
}

/// Add64: result = a + b + carry_in
pub fn emit_add64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_add(ctx, ra, inst_ref, inst, 64);
}

/// Upstream: `static Xbyak::Reg8 DoCarry(RegAlloc&, Argument&, IR::Inst*)`
/// Pre-allocates a register for carry_out BEFORE the arithmetic instruction.
/// Returns None if no carry pseudo-op exists.
fn do_carry(ra: &mut RegAlloc, carry_in: &mut Argument, carry_out: Option<InstRef>) -> Option<Reg> {
    let _carry_out = carry_out?;
    let reg = if carry_in.is_immediate() {
        ra.scratch_gpr()
    } else {
        ra.use_scratch_gpr(carry_in)
    };
    Some(reg)
}

/// Upstream: `static Xbyak::Reg64 DoNZCV(BlockOfCode&, RegAlloc&, IR::Inst*)`
/// Pre-allocates RAX for LAHF/SETO BEFORE the arithmetic instruction.
/// Returns None if no nzcv pseudo-op exists.
fn do_nzcv(ra: &mut RegAlloc, nzcv_out: Option<InstRef>) -> Option<Reg> {
    let _nzcv_out = nzcv_out?;
    let rax = ra.scratch_gpr_at(HOST_RAX);
    ra.asm
        .xor_(rax.cvt32().unwrap(), rax.cvt32().unwrap())
        .unwrap();
    Some(rax)
}

/// Upstream: static void EmitAdd(BlockOfCode&, EmitContext&, IR::Inst*, int bitsize)
fn emit_add(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst, bitsize: usize) {
    use crate::ir::opcode::Opcode;

    let carry_inst = ctx
        .block
        .and_then(|b| b.get_associated_pseudo_operation(inst_ref, Opcode::GetCarryFromOp));
    let overflow_inst = ctx
        .block
        .and_then(|b| b.get_associated_pseudo_operation(inst_ref, Opcode::GetOverflowFromOp));
    let nzcv_inst = ctx
        .block
        .and_then(|b| b.get_associated_pseudo_operation(inst_ref, Opcode::GetNZCVFromOp));

    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let carry_in_is_zero = args[2].is_immediate() && !args[2].get_immediate_u1();

    if carry_inst.is_none() && overflow_inst.is_none() && nzcv_inst.is_none() && carry_in_is_zero {
        let result = ra.use_scratch_gpr(&mut args[0]);
        let result_sized = if bitsize == 32 {
            result.cvt32().unwrap()
        } else {
            result
        };
        let address = if args[1].is_immediate() && args[1].fits_in_immediate_s32() {
            rxbyak::ptr(RegExp::from(result) + args[1].get_immediate_s32() as i32)
        } else {
            let op2 = ra.use_gpr(&mut args[1]);
            rxbyak::ptr(RegExp::from(result) + op2)
        };
        ra.asm.lea(result_sized, address).unwrap();
        ra.define_value(inst_ref, result);
        return;
    }

    // Pre-allocate registers for pseudo-ops BEFORE the main instruction.
    // This is upstream's DoNZCV/DoCarry pattern.
    let nzcv_reg = do_nzcv(ra, nzcv_inst);
    let carry_reg = do_carry(ra, &mut args[2], carry_inst);
    let overflow_reg = if overflow_inst.is_some() {
        Some(ra.scratch_gpr())
    } else {
        None
    };

    let result = ra.use_scratch_gpr(&mut args[0]);
    let result_sized = if bitsize == 32 {
        result.cvt32().unwrap()
    } else {
        result
    };

    if args[1].is_immediate() && args[1].get_type() == Type::U32 {
        let op_arg = args[1].get_immediate_u32();
        if args[2].is_immediate() {
            if args[2].get_immediate_u1() {
                let signed = op_arg as i32 as i64;
                let in_range = (-0x7fff_fffe..=0x7fff_fffe).contains(&signed);
                if in_range
                    && (carry_inst.is_some() || nzcv_inst.is_some() || overflow_inst.is_some())
                {
                    ra.asm.stc().unwrap();
                    ra.asm.adc(result_sized, op_arg).unwrap();
                } else {
                    ra.asm
                        .lea(
                            result_sized,
                            rxbyak::ptr(RegExp::from(result) + op_arg.wrapping_add(1) as i32),
                        )
                        .unwrap();
                }
            } else {
                ra.asm.add(result_sized, op_arg).unwrap();
            }
        } else {
            let carry = ra.use_gpr(&mut args[2]);
            ra.asm.bt_imm(carry.cvt32().unwrap(), 0).unwrap();
            ra.asm.adc(result_sized, op_arg).unwrap();
        }
    } else {
        let op2 = ra.use_gpr(&mut args[1]);
        let op2_sized = if bitsize == 32 {
            op2.cvt32().unwrap()
        } else {
            op2
        };
        if carry_in_is_zero {
            ra.asm.add(result_sized, op2_sized).unwrap();
        } else if args[2].is_immediate() && args[2].get_immediate_u1() {
            ra.asm.stc().unwrap();
            ra.asm.adc(result_sized, op2_sized).unwrap();
        } else {
            let carry = ra.use_gpr(&mut args[2]);
            ra.asm.bt_imm(carry.cvt32().unwrap(), 0).unwrap();
            ra.asm.adc(result_sized, op2_sized).unwrap();
        }
    }

    // Capture flags immediately after the ADD/ADC, using pre-allocated registers.
    if let (Some(nzcv_ref), Some(nzcv)) = (nzcv_inst, nzcv_reg) {
        ra.asm.lahf().unwrap();
        ra.asm.seto(nzcv.cvt8().unwrap()).unwrap();
        ra.asm
            .and_(nzcv.cvt32().unwrap(), nzcv_util::X64_MASK as i32)
            .unwrap();
        ra.define_value(nzcv_ref, nzcv);
    }
    if let (Some(carry_ref), Some(carry)) = (carry_inst, carry_reg) {
        ra.asm.setc(carry.cvt8().unwrap()).unwrap();
        ra.asm
            .movzx(carry.cvt32().unwrap(), carry.cvt8().unwrap())
            .unwrap();
        ra.define_value(carry_ref, carry);
    }
    if let (Some(overflow_ref), Some(overflow)) = (overflow_inst, overflow_reg) {
        ra.asm.seto(overflow.cvt8().unwrap()).unwrap();
        ra.asm
            .movzx(overflow.cvt32().unwrap(), overflow.cvt8().unwrap())
            .unwrap();
        ra.define_value(overflow_ref, overflow);
    }

    ra.define_value(inst_ref, result);
}

/// Sub32: result = a - b - !carry_in (ARM: result = a + NOT(b) + carry_in)
pub fn emit_sub32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_sub(ctx, ra, inst_ref, inst, 32);
}

/// Sub64: result = a - b - !carry_in
pub fn emit_sub64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_sub(ctx, ra, inst_ref, inst, 64);
}

/// Upstream: static void EmitSub(BlockOfCode&, EmitContext&, IR::Inst*, int bitsize)
fn emit_sub(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst, bitsize: usize) {
    use crate::ir::opcode::Opcode;

    let carry_inst = ctx
        .block
        .and_then(|b| b.get_associated_pseudo_operation(inst_ref, Opcode::GetCarryFromOp));
    let overflow_inst = ctx
        .block
        .and_then(|b| b.get_associated_pseudo_operation(inst_ref, Opcode::GetOverflowFromOp));
    let nzcv_inst = ctx
        .block
        .and_then(|b| b.get_associated_pseudo_operation(inst_ref, Opcode::GetNZCVFromOp));

    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let carry_in_is_one = args[2].is_immediate() && args[2].get_immediate_u1();
    let pseudo_use_count =
        carry_inst.is_some() as u32 + overflow_inst.is_some() as u32 + nzcv_inst.is_some() as u32;
    let is_cmp = inst.use_count == pseudo_use_count && carry_in_is_one;

    if carry_inst.is_none()
        && overflow_inst.is_none()
        && nzcv_inst.is_none()
        && carry_in_is_one
        && args[1].is_immediate()
        && args[1].fits_in_immediate_s32()
        && args[1].get_immediate_s32() != 0xffff_ffff_8000_0000
    {
        let op1 = ra.use_gpr(&mut args[0]);
        let result = ra.scratch_gpr();
        let result_sized = if bitsize == 32 {
            result.cvt32().unwrap()
        } else {
            result
        };
        ra.asm
            .lea(
                result_sized,
                rxbyak::ptr(RegExp::from(op1) - args[1].get_immediate_s32() as i32),
            )
            .unwrap();
        ra.define_value(inst_ref, result);
        return;
    }

    // Pre-allocate registers for pseudo-ops BEFORE the main instruction (DoNZCV/DoCarry).
    let nzcv_reg = do_nzcv(ra, nzcv_inst);
    let carry_reg = do_carry(ra, &mut args[2], carry_inst);
    let overflow_reg = if overflow_inst.is_some() {
        Some(ra.scratch_gpr())
    } else {
        None
    };

    let result = if is_cmp {
        ra.use_gpr(&mut args[0])
    } else {
        ra.use_scratch_gpr(&mut args[0])
    };
    let result_sized = if bitsize == 32 {
        result.cvt32().unwrap()
    } else {
        result
    };

    let mut invert_output_carry = true;

    if is_cmp {
        if args[1].is_immediate() && args[1].get_type() == Type::U32 {
            ra.asm
                .cmp(result_sized, args[1].get_immediate_u32())
                .unwrap();
        } else {
            let op2 = ra.use_gpr(&mut args[1]);
            let op2_sized = if bitsize == 32 {
                op2.cvt32().unwrap()
            } else {
                op2
            };
            ra.asm.cmp(result_sized, op2_sized).unwrap();
        }
    } else if args[1].is_immediate() && args[1].get_type() == Type::U32 {
        let op_arg = args[1].get_immediate_u32();
        if args[2].is_immediate() {
            if carry_in_is_one {
                ra.asm.sub(result_sized, op_arg).unwrap();
            } else {
                // Upstream deliberately expresses SBC-with-zero-carry as
                // ADD(~op_arg). Besides saving STC, this makes x64 CF already
                // use ARM's no-borrow convention.
                ra.asm.add(result_sized, !op_arg).unwrap();
                invert_output_carry = false;
            }
        } else {
            let carry = ra.use_gpr(&mut args[2]);
            ra.asm.bt_imm(carry.cvt32().unwrap(), 0).unwrap();
            ra.asm.adc(result_sized, !op_arg).unwrap();
            invert_output_carry = false;
        }
    } else {
        let op2 = ra.use_gpr(&mut args[1]);
        let op2_sized = if bitsize == 32 {
            op2.cvt32().unwrap()
        } else {
            op2
        };
        if carry_in_is_one {
            ra.asm.sub(result_sized, op2_sized).unwrap();
        } else if args[2].is_immediate() {
            // carry_in=0: a + NOT(b) + 0 — use STC;SBB
            ra.asm.stc().unwrap();
            ra.asm.sbb(result_sized, op2_sized).unwrap();
        } else {
            // Dynamic carry: bt carry, 0; cmc; sbb result, op2
            let carry = ra.use_gpr(&mut args[2]);
            ra.asm.bt_imm(carry.cvt32().unwrap(), 0).unwrap();
            ra.asm.cmc().unwrap();
            ra.asm.sbb(result_sized, op2_sized).unwrap();
        }
    }

    // Capture flags immediately after the SUB/SBB, using pre-allocated registers.
    // Upstream: x86 CF is inverse of ARM carry for subtraction.
    // invert_output_carry controls whether CMC/SETNC is needed.
    if let (Some(nzcv_ref), Some(nzcv)) = (nzcv_inst, nzcv_reg) {
        if invert_output_carry {
            ra.asm.cmc().unwrap();
        }
        ra.asm.lahf().unwrap();
        ra.asm.seto(nzcv.cvt8().unwrap()).unwrap();
        ra.asm
            .and_(nzcv.cvt32().unwrap(), nzcv_util::X64_MASK as i32)
            .unwrap();
        ra.define_value(nzcv_ref, nzcv);
    }
    if let (Some(carry_ref), Some(carry)) = (carry_inst, carry_reg) {
        // Upstream: if (invert_output_carry) code.setnc(carry) else code.setc(carry)
        if invert_output_carry {
            ra.asm.setnc(carry.cvt8().unwrap()).unwrap();
        } else {
            ra.asm.setc(carry.cvt8().unwrap()).unwrap();
        }
        ra.asm
            .movzx(carry.cvt32().unwrap(), carry.cvt8().unwrap())
            .unwrap();
        ra.define_value(carry_ref, carry);
    }
    if let (Some(overflow_ref), Some(overflow)) = (overflow_inst, overflow_reg) {
        ra.asm.seto(overflow.cvt8().unwrap()).unwrap();
        ra.asm
            .movzx(overflow.cvt32().unwrap(), overflow.cvt8().unwrap())
            .unwrap();
        ra.define_value(overflow_ref, overflow);
    }

    if !is_cmp {
        ra.define_value(inst_ref, result);
    }
}

// ---------------------------------------------------------------------------
// Multiplication
// ---------------------------------------------------------------------------

/// Mul32: result = a * b (lower 32 bits)
pub fn emit_mul32(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    let op2 = ra.use_gpr(&mut args[1]);
    ra.asm
        .imul(result.cvt32().unwrap(), op2.cvt32().unwrap())
        .unwrap();
    ra.define_value(inst_ref, result);
}

/// Mul64: result = a * b (lower 64 bits)
pub fn emit_mul64(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    let op2 = ra.use_gpr(&mut args[1]);
    ra.asm.imul(result, op2).unwrap();
    ra.define_value(inst_ref, result);
}

/// SignedMultiplyHigh64: result = (i128(a) * i128(b)) >> 64
///
/// Mirrors upstream `EmitX64::EmitSignedMultiplyHigh64` in
/// `emit_x64_data_processing.cpp:1146`:
/// ```text
/// ScratchGpr(HostLoc::RDX);
/// UseScratch(args[0], HostLoc::RAX);
/// OpArg op_arg = UseOpArg(args[1]);
/// code.imul(*op_arg);
/// DefineValue(inst, rdx);
/// ```
/// Reserve RDX FIRST so `op2` cannot grab it; without this, the
/// reg-allocator panics with "All candidate registers have already been
/// allocated" when emitting SMULH (observed booting STK).
pub fn emit_signed_multiply_high_64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());

    let rdx = ra.scratch_gpr_at(HOST_RDX);
    ra.use_scratch(&mut args[0], HOST_RAX);
    let op2 = ra.use_gpr(&mut args[1]);

    // Single-operand signed multiply: RDX:RAX = RAX * op2
    ra.asm.imul_1op(op2).unwrap();
    ra.define_value(inst_ref, rdx);
}

/// UnsignedMultiplyHigh64: result = (u128(a) * u128(b)) >> 64
///
/// Mirrors upstream `EmitX64::EmitUnsignedMultiplyHigh64` — see
/// `emit_signed_multiply_high_64` above for the RDX-before-RAX rationale.
pub fn emit_unsigned_multiply_high_64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());

    let rdx = ra.scratch_gpr_at(HOST_RDX);
    ra.use_scratch(&mut args[0], HOST_RAX);
    let op2 = ra.use_gpr(&mut args[1]);

    // Single-operand mul
    ra.asm.mul(op2).unwrap();
    ra.define_value(inst_ref, rdx);
}

// ---------------------------------------------------------------------------
// Division
// ---------------------------------------------------------------------------

/// UnsignedDiv32: result = a / b (unsigned, 32-bit)
pub fn emit_unsigned_div32(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());

    // Upstream: reserve RAX/RDX first, then keep dividend/divisor in arbitrary
    // GPRs and move dividend into EAX only after the zero-divisor check.
    let _rax = ra.scratch_gpr_at(HOST_RAX);
    let _rdx = ra.scratch_gpr_at(HOST_RDX);
    let dividend = ra.use_gpr(&mut args[0]);
    let divisor = ra.use_gpr(&mut args[1]);
    let divisor32 = divisor.cvt32().unwrap();

    let end = ra.asm.create_label();
    ra.asm.xor_(rxbyak::EAX, rxbyak::EAX).unwrap();
    ra.asm.test(divisor32, divisor32).unwrap();
    ra.asm.jz(&end, JmpType::Near).unwrap();
    ra.asm.mov(rxbyak::EAX, dividend.cvt32().unwrap()).unwrap();
    ra.asm.xor_(rxbyak::EDX, rxbyak::EDX).unwrap();
    ra.asm.div(divisor32).unwrap();
    ra.asm.bind(&end).unwrap();

    ra.define_value(inst_ref, Reg::gpr64(0));
}

/// UnsignedDiv64: result = a / b (unsigned, 64-bit)
pub fn emit_unsigned_div64(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());

    let _rax = ra.scratch_gpr_at(HOST_RAX);
    let _rdx = ra.scratch_gpr_at(HOST_RDX);
    let dividend = ra.use_gpr(&mut args[0]);
    let divisor = ra.use_gpr(&mut args[1]);

    let end = ra.asm.create_label();
    ra.asm.xor_(rxbyak::EAX, rxbyak::EAX).unwrap();
    ra.asm.test(divisor, divisor).unwrap();
    ra.asm.jz(&end, JmpType::Near).unwrap();
    ra.asm.mov(rxbyak::RAX, dividend).unwrap();
    ra.asm.xor_(rxbyak::EDX, rxbyak::EDX).unwrap();
    ra.asm.div(divisor).unwrap();
    ra.asm.bind(&end).unwrap();

    ra.define_value(inst_ref, Reg::gpr64(0));
}

/// SignedDiv32: result = a / b (signed, 32-bit)
pub fn emit_signed_div32(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());

    let _rax = ra.scratch_gpr_at(HOST_RAX);
    let _rdx = ra.scratch_gpr_at(HOST_RDX);
    let dividend = ra.use_gpr(&mut args[0]);
    let divisor = ra.use_scratch_gpr(&mut args[1]);
    let dividend32 = dividend.cvt32().unwrap();
    let divisor32 = divisor.cvt32().unwrap();

    let end = ra.asm.create_label();
    ra.asm.xor_(rxbyak::EAX, rxbyak::EAX).unwrap();
    ra.asm.test(divisor32, divisor32).unwrap();
    ra.asm.jz(&end, JmpType::Near).unwrap();
    ra.asm.movsxd(rxbyak::RAX, dividend32).unwrap();
    ra.asm.movsxd(divisor, divisor32).unwrap();
    ra.asm.cqo().unwrap();
    ra.asm.idiv(divisor).unwrap();
    ra.asm.bind(&end).unwrap();

    ra.define_value(inst_ref, Reg::gpr64(0));
}

/// SignedDiv64: result = a / b (signed, 64-bit)
pub fn emit_signed_div64(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());

    let _rax = ra.scratch_gpr_at(HOST_RAX);
    let _rdx = ra.scratch_gpr_at(HOST_RDX);
    let dividend = ra.use_gpr(&mut args[0]);
    let divisor = ra.use_gpr(&mut args[1]);

    let end = ra.asm.create_label();
    let ok = ra.asm.create_label();
    ra.asm.xor_(rxbyak::EAX, rxbyak::EAX).unwrap();
    ra.asm.test(divisor, divisor).unwrap();
    ra.asm.jz(&end, JmpType::Near).unwrap();
    ra.asm.cmp(divisor, -1i32).unwrap();
    ra.asm.jne(&ok, JmpType::Near).unwrap();
    ra.asm
        .mov(rxbyak::RAX, 0x8000_0000_0000_0000u64 as i64)
        .unwrap();
    ra.asm.cmp(dividend, rxbyak::RAX).unwrap();
    ra.asm.je(&end, JmpType::Near).unwrap();
    ra.asm.bind(&ok).unwrap();
    ra.asm.mov(rxbyak::RAX, dividend).unwrap();
    ra.asm.cqo().unwrap();
    ra.asm.idiv(divisor).unwrap();
    ra.asm.bind(&end).unwrap();

    ra.define_value(inst_ref, Reg::gpr64(0));
}

// ---------------------------------------------------------------------------
// Logical operations
// ---------------------------------------------------------------------------

pub fn emit_and32(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_binop(ra, inst_ref, inst, 32, BinOp::And);
}

pub fn emit_and64(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_binop(ra, inst_ref, inst, 64, BinOp::And);
}

pub fn emit_or32(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_binop(ra, inst_ref, inst, 32, BinOp::Or);
}

pub fn emit_or64(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_binop(ra, inst_ref, inst, 64, BinOp::Or);
}

pub fn emit_eor32(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_binop(ra, inst_ref, inst, 32, BinOp::Eor);
}

pub fn emit_eor64(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_binop(ra, inst_ref, inst, 64, BinOp::Eor);
}

enum BinOp {
    And,
    Or,
    Eor,
}

fn emit_binop(ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst, bitsize: usize, op: BinOp) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    let result_sized = if bitsize == 32 {
        result.cvt32().unwrap()
    } else {
        result
    };

    if args[1].is_immediate() && args[1].fits_in_immediate_s32() {
        let imm = args[1].get_immediate_s32() as i32;
        match op {
            BinOp::And => ra.asm.and_(result_sized, imm).unwrap(),
            BinOp::Or => ra.asm.or_(result_sized, imm).unwrap(),
            BinOp::Eor => ra.asm.xor_(result_sized, imm).unwrap(),
        }
    } else {
        let op2 = ra.use_gpr(&mut args[1]);
        let op2_sized = if bitsize == 32 {
            op2.cvt32().unwrap()
        } else {
            op2
        };
        match op {
            BinOp::And => ra.asm.and_(result_sized, op2_sized).unwrap(),
            BinOp::Or => ra.asm.or_(result_sized, op2_sized).unwrap(),
            BinOp::Eor => ra.asm.xor_(result_sized, op2_sized).unwrap(),
        }
    }
    ra.define_value(inst_ref, result);
}

/// Not32: result = ~a
pub fn emit_not32(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    ra.asm.not_(result.cvt32().unwrap()).unwrap();
    ra.define_value(inst_ref, result);
}

/// Not64: result = ~a
pub fn emit_not64(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    ra.asm.not_(result).unwrap();
    ra.define_value(inst_ref, result);
}

/// AndNot32: result = a & ~b
pub fn emit_and_not32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    if !args[0].is_immediate() && !args[1].is_immediate() && ctx.has_host_feature(HostFeature::BMI1)
    {
        let op1 = ra.use_gpr(&mut args[0]).cvt32().unwrap();
        let op2 = ra.use_gpr(&mut args[1]).cvt32().unwrap();
        let result = ra.scratch_gpr();
        ra.asm.andn(result.cvt32().unwrap(), op2, op1).unwrap();
        ra.define_value(inst_ref, result);
        return;
    }

    let op2 = ra.use_scratch_gpr(&mut args[1]);
    ra.asm.not_(op2.cvt32().unwrap()).unwrap();
    let op1 = ra.use_gpr(&mut args[0]);
    ra.asm
        .and_(op2.cvt32().unwrap(), op1.cvt32().unwrap())
        .unwrap();
    ra.define_value(inst_ref, op2);
}

/// AndNot64: result = a & ~b
pub fn emit_and_not64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    if !args[0].is_immediate() && !args[1].is_immediate() && ctx.has_host_feature(HostFeature::BMI1)
    {
        let op1 = ra.use_gpr(&mut args[0]);
        let op2 = ra.use_gpr(&mut args[1]);
        let result = ra.scratch_gpr();
        ra.asm.andn(result, op2, op1).unwrap();
        ra.define_value(inst_ref, result);
        return;
    }

    let op2 = ra.use_scratch_gpr(&mut args[1]);
    ra.asm.not_(op2).unwrap();
    let op1 = ra.use_gpr(&mut args[0]);
    ra.asm.and_(op2, op1).unwrap();
    ra.define_value(inst_ref, op2);
}

// ---------------------------------------------------------------------------
// Shifts (immediate)
// ---------------------------------------------------------------------------

pub fn emit_logical_shift_left32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_shift(ctx, ra, inst_ref, inst, 32, ShiftOp::Shl);
}

pub fn emit_logical_shift_left64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_shift(ctx, ra, inst_ref, inst, 64, ShiftOp::Shl);
}

pub fn emit_logical_shift_right32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_shift(ctx, ra, inst_ref, inst, 32, ShiftOp::Shr);
}

pub fn emit_logical_shift_right64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_shift(ctx, ra, inst_ref, inst, 64, ShiftOp::Shr);
}

pub fn emit_arithmetic_shift_right32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_shift(ctx, ra, inst_ref, inst, 32, ShiftOp::Sar);
}

pub fn emit_arithmetic_shift_right64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_shift(ctx, ra, inst_ref, inst, 64, ShiftOp::Sar);
}

pub fn emit_rotate_right32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_shift(ctx, ra, inst_ref, inst, 32, ShiftOp::Ror);
}

pub fn emit_rotate_right64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_shift(ctx, ra, inst_ref, inst, 64, ShiftOp::Ror);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ShiftOp {
    Shl,
    Shr,
    Sar,
    Ror,
}

/// Emit a shift instruction, optionally capturing the carry flag for
/// an associated GetCarryFromOp pseudo-op.
///
/// Matches upstream pattern: the shift handler finds its GetCarryFromOp
/// via GetAssociatedPseudoOperation and emits SETC immediately after the
/// shift, guaranteeing no register allocator interference.
fn emit_shift(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    bitsize: usize,
    op: ShiftOp,
) {
    use crate::ir::opcode::Opcode;

    // Upstream: const auto carry_inst = inst->GetAssociatedPseudoOperation(IR::Opcode::GetCarryFromOp);
    let carry_inst = ctx
        .block
        .and_then(|b| b.get_associated_pseudo_operation(inst_ref, Opcode::GetCarryFromOp));

    if bitsize == 32 {
        if let Some(carry_ref) = carry_inst {
            emit_shift32_with_carry(ra, inst_ref, inst, carry_ref, op);
            return;
        }
    }

    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());

    if args[1].is_immediate() {
        if op == ShiftOp::Ror && carry_inst.is_none() && ctx.has_host_feature(HostFeature::BMI2) {
            let operand = ra.use_gpr(&mut args[0]);
            let result = ra.scratch_gpr();
            let result_sized = if bitsize == 32 {
                result.cvt32().unwrap()
            } else {
                result
            };
            let operand_sized = if bitsize == 32 {
                operand.cvt32().unwrap()
            } else {
                operand
            };
            ra.asm
                .rorx(result_sized, operand_sized, args[1].get_immediate_u8())
                .unwrap();
            ra.define_value(inst_ref, result);
            return;
        }

        let result = ra.use_scratch_gpr(&mut args[0]);
        let result_sized = if bitsize == 32 {
            result.cvt32().unwrap()
        } else {
            result
        };
        let shift = args[1].get_immediate_u8();
        let max_shift = bitsize as u8;

        match op {
            ShiftOp::Ror => {
                if shift == 0 && carry_inst.is_none() {
                    // No-op
                } else {
                    ra.asm.ror(result_sized, shift % max_shift).unwrap();
                }
            }
            ShiftOp::Sar => {
                if shift == 0 && carry_inst.is_none() {
                    // No-op
                } else if shift <= (max_shift - 1) {
                    // Normal SAR — CF from SAR is correct for shift <= 31
                    ra.asm.sar(result_sized, shift).unwrap();
                } else {
                    // Upstream: shift > 31 — SAR by 31, then use BT to get
                    // the sign bit as carry (CF from SAR would be bit[30], wrong).
                    ra.asm.sar(result_sized, max_shift - 1).unwrap();
                    if carry_inst.is_some() {
                        ra.asm.bt_imm(result_sized, (max_shift - 1) as u8).unwrap();
                    }
                }
            }
            ShiftOp::Shl => {
                if shift == 0 && carry_inst.is_none() {
                    // No-op
                } else if shift < max_shift {
                    ra.asm.shl(result_sized, shift).unwrap();
                } else if shift == max_shift && carry_inst.is_some() {
                    // Upstream: bt(result, 0); setc(carry); mov(result, 0);
                    ra.asm.bt_imm(result_sized, 0).unwrap();
                    // SETC will be emitted in the carry capture below
                    ra.asm.mov(result_sized, 0i32).unwrap();
                } else {
                    // shift > max_shift: result=0, carry=0
                    ra.asm
                        .xor_(result.cvt32().unwrap(), result.cvt32().unwrap())
                        .unwrap();
                }
            }
            ShiftOp::Shr => {
                if shift == 0 && carry_inst.is_none() {
                    // No-op
                } else if shift < max_shift {
                    ra.asm.shr(result_sized, shift).unwrap();
                } else if shift == max_shift && carry_inst.is_some() {
                    // Upstream: bt(result, 31); setc(carry); mov(result, 0);
                    ra.asm.bt_imm(result_sized, (max_shift - 1) as u8).unwrap();
                    ra.asm.mov(result_sized, 0i32).unwrap();
                } else {
                    // shift > max_shift: result=0, carry=0
                    ra.asm
                        .xor_(result.cvt32().unwrap(), result.cvt32().unwrap())
                        .unwrap();
                }
            }
        }

        // Emit carry capture immediately after the shift instruction.
        if let Some(carry_ref) = carry_inst {
            let carry = ra.scratch_gpr();
            ra.asm.setc(carry.cvt8().unwrap()).unwrap();
            ra.asm
                .movzx(carry.cvt32().unwrap(), carry.cvt8().unwrap())
                .unwrap();
            ra.define_value(carry_ref, carry);
        }

        ra.define_value(inst_ref, result);
    } else {
        if carry_inst.is_none() && op != ShiftOp::Ror && ctx.has_host_feature(HostFeature::BMI2) {
            let shift = if op == ShiftOp::Sar {
                ra.use_scratch_gpr(&mut args[1])
            } else {
                ra.use_gpr(&mut args[1])
            };
            let operand = ra.use_gpr(&mut args[0]);
            let result = ra.scratch_gpr();
            let shift_sized = if bitsize == 32 {
                shift.cvt32().unwrap()
            } else {
                shift
            };
            let operand_sized = if bitsize == 32 {
                operand.cvt32().unwrap()
            } else {
                operand
            };
            let result_sized = if bitsize == 32 {
                result.cvt32().unwrap()
            } else {
                result
            };

            match op {
                ShiftOp::Shl | ShiftOp::Shr => {
                    if op == ShiftOp::Shl {
                        ra.asm
                            .shlx(result_sized, operand_sized, shift_sized)
                            .unwrap();
                    } else {
                        ra.asm
                            .shrx(result_sized, operand_sized, shift_sized)
                            .unwrap();
                    }
                    let zero = ra.scratch_gpr();
                    ra.asm
                        .xor_(zero.cvt32().unwrap(), zero.cvt32().unwrap())
                        .unwrap();
                    // This deliberately follows Eden's exact thresholds,
                    // including its SHR64 BMI2 comparison against 63.
                    let limit = if op == ShiftOp::Shr && bitsize == 64 {
                        63
                    } else {
                        bitsize as i32
                    };
                    ra.asm.cmp(shift.cvt8().unwrap(), limit).unwrap();
                    if bitsize == 32 {
                        ra.asm
                            .cmovnb(result.cvt32().unwrap(), zero.cvt32().unwrap())
                            .unwrap();
                    } else {
                        ra.asm.cmovnb(result, zero).unwrap();
                    }
                    ra.release(zero);
                }
                ShiftOp::Sar => {
                    let saturated_shift = ra.scratch_gpr();
                    let saturated_shift_sized = if bitsize == 32 {
                        saturated_shift.cvt32().unwrap()
                    } else {
                        saturated_shift
                    };
                    ra.asm
                        .mov(saturated_shift.cvt32().unwrap(), bitsize as i32 - 1)
                        .unwrap();
                    ra.asm
                        .cmp(shift.cvt8().unwrap(), bitsize as i32 - 1)
                        .unwrap();
                    ra.asm.cmovnb(shift_sized, saturated_shift_sized).unwrap();
                    ra.asm
                        .sarx(result_sized, operand_sized, shift_sized)
                        .unwrap();
                    ra.release(saturated_shift);
                }
                ShiftOp::Ror => unreachable!(),
            }

            ra.define_value(inst_ref, result);
            return;
        }

        // Upstream non-BMI2 path takes the shift argument in RCX before
        // allocating the destination scratch register. That ordering matters:
        // otherwise the destination can consume RCX and make the shift input
        // impossible to allocate.
        if carry_inst.is_none() && op == ShiftOp::Sar {
            ra.use_scratch(&mut args[1], HostLoc::Gpr(1)); // RCX
        } else {
            ra.use_loc(&mut args[1], HostLoc::Gpr(1)); // RCX
        }
        let result = ra.use_scratch_gpr(&mut args[0]);
        let result_sized = if bitsize == 32 {
            result.cvt32().unwrap()
        } else {
            result
        };

        match op {
            ShiftOp::Shl => ra.asm.shl_cl(result_sized).unwrap(),
            ShiftOp::Shr => ra.asm.shr_cl(result_sized).unwrap(),
            ShiftOp::Sar => {
                if carry_inst.is_some() {
                    ra.asm.sar_cl(result_sized).unwrap();
                }
            }
            ShiftOp::Ror => ra.asm.ror_cl(result_sized).unwrap(),
        }

        // For SHL/SHR: if shift >= width, result should be zero (ARM behavior)
        // x86 masks shift count, so we need to check and zero if >= width
        match op {
            ShiftOp::Shl | ShiftOp::Shr => {
                let zero = ra.scratch_gpr();
                ra.asm
                    .xor_(zero.cvt32().unwrap(), zero.cvt32().unwrap())
                    .unwrap();
                ra.asm.cmp(CL, bitsize as i32).unwrap();
                // cmovnb: if shift >= width, replace with zero
                if bitsize == 32 {
                    ra.asm
                        .cmovnb(result.cvt32().unwrap(), zero.cvt32().unwrap())
                        .unwrap();
                } else {
                    ra.asm.cmovnb(result, zero).unwrap();
                }
            }
            ShiftOp::Sar => {
                if carry_inst.is_none() {
                    // ARM saturates the count; x86 masks it. Clamp RCX before
                    // the shift exactly as Eden does.
                    let saturated_shift = ra.scratch_gpr();
                    ra.asm
                        .mov(saturated_shift.cvt32().unwrap(), bitsize as i32 - 1)
                        .unwrap();
                    ra.asm.cmp(CL, bitsize as i32 - 1).unwrap();
                    if bitsize == 32 {
                        ra.asm
                            .cmova(rxbyak::ECX, saturated_shift.cvt32().unwrap())
                            .unwrap();
                    } else {
                        ra.asm
                            .cmovnb(rxbyak::ECX, saturated_shift.cvt32().unwrap())
                            .unwrap();
                    }
                    ra.asm.sar_cl(result_sized).unwrap();
                    ra.release(saturated_shift);
                }
            }
            ShiftOp::Ror => {
                // Rotate: any amount works correctly with x86 masking
            }
        }

        // Capture carry immediately after the shift (register path).
        if let Some(carry_ref) = carry_inst {
            let carry = ra.scratch_gpr();
            ra.asm.setc(carry.cvt8().unwrap()).unwrap();
            ra.asm
                .movzx(carry.cvt32().unwrap(), carry.cvt8().unwrap())
                .unwrap();
            ra.define_value(carry_ref, carry);
        }

        ra.define_value(inst_ref, result);
    }
}

fn emit_shift32_with_carry(
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    carry_ref: InstRef,
    op: ShiftOp,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());

    if args[1].is_immediate() {
        let shift = args[1].get_immediate_u8();
        let result = ra.use_scratch_gpr(&mut args[0]).cvt32().unwrap();
        let carry = ra.use_scratch_gpr(&mut args[2]);
        let carry32 = carry.cvt32().unwrap();
        let carry8 = carry.cvt8().unwrap();

        match op {
            ShiftOp::Shl => {
                if shift == 0 {
                    // Preserve both the operand and incoming carry.
                } else if shift < 32 {
                    ra.asm.bt_imm(carry32, 0).unwrap();
                    ra.asm.shl(result, shift).unwrap();
                    ra.asm.setc(carry8).unwrap();
                } else if shift > 32 {
                    ra.asm.xor_(result, result).unwrap();
                    ra.asm.xor_(carry32, carry32).unwrap();
                } else {
                    ra.asm.mov(carry32, result).unwrap();
                    ra.asm.xor_(result, result).unwrap();
                    ra.asm.and_(carry32, 1).unwrap();
                }
            }
            ShiftOp::Shr => {
                if shift == 0 {
                    // Preserve both the operand and incoming carry.
                } else if shift < 32 {
                    ra.asm.shr(result, shift).unwrap();
                    ra.asm.setc(carry8).unwrap();
                } else if shift == 32 {
                    ra.asm.bt_imm(result, 31).unwrap();
                    ra.asm.setc(carry8).unwrap();
                    ra.asm.mov(result, 0).unwrap();
                } else {
                    ra.asm.xor_(result, result).unwrap();
                    ra.asm.xor_(carry32, carry32).unwrap();
                }
            }
            ShiftOp::Sar => {
                if shift == 0 {
                    // Preserve both the operand and incoming carry.
                } else if shift <= 31 {
                    ra.asm.sar(result, shift).unwrap();
                    ra.asm.setc(carry8).unwrap();
                } else {
                    ra.asm.sar(result, 31).unwrap();
                    ra.asm.bt_imm(result, 31).unwrap();
                    ra.asm.setc(carry8).unwrap();
                }
            }
            ShiftOp::Ror => {
                if shift == 0 {
                    // Preserve both the operand and incoming carry.
                } else if shift & 0x1f == 0 {
                    ra.asm.bt_imm(result, 31).unwrap();
                    ra.asm.setc(carry8).unwrap();
                } else {
                    ra.asm.ror(result, shift).unwrap();
                    ra.asm.setc(carry8).unwrap();
                }
            }
        }

        ra.define_value(carry_ref, carry);
        ra.define_value(inst_ref, result);
        return;
    }

    ra.use_scratch(&mut args[1], HostLoc::Gpr(1));
    match op {
        ShiftOp::Shl => {
            let result = ra.use_scratch_gpr(&mut args[0]).cvt32().unwrap();
            let tmp = ra.scratch_gpr().cvt32().unwrap();
            let carry = ra.use_scratch_gpr(&mut args[2]);
            ra.asm.mov(tmp, 63).unwrap();
            ra.asm.cmp(CL, 63).unwrap();
            ra.asm.cmova(rxbyak::ECX, tmp).unwrap();
            ra.asm.shl(result.cvt64().unwrap(), 32).unwrap();
            ra.asm.bt_imm(carry.cvt32().unwrap(), 0).unwrap();
            ra.asm.shl_cl(result.cvt64().unwrap()).unwrap();
            ra.asm.setc(carry.cvt8().unwrap()).unwrap();
            ra.asm.shr(result.cvt64().unwrap(), 32).unwrap();
            ra.define_value(carry_ref, carry);
            ra.define_value(inst_ref, result);
        }
        ShiftOp::Shr => {
            let operand = ra.use_gpr(&mut args[0]).cvt32().unwrap();
            let result = ra.scratch_gpr().cvt32().unwrap();
            let carry = ra.use_scratch_gpr(&mut args[2]);
            ra.asm.mov(result, 63).unwrap();
            ra.asm.cmp(CL, 63).unwrap();
            ra.asm.cmovnb(rxbyak::ECX, result).unwrap();
            ra.asm.mov(result, operand).unwrap();
            ra.asm.bt_imm(carry.cvt32().unwrap(), 0).unwrap();
            ra.asm.shr_cl(result.cvt64().unwrap()).unwrap();
            ra.asm.setc(carry.cvt8().unwrap()).unwrap();
            ra.define_value(carry_ref, carry);
            ra.define_value(inst_ref, result);
        }
        ShiftOp::Sar => {
            let operand = ra.use_gpr(&mut args[0]).cvt32().unwrap();
            let result = ra.scratch_gpr().cvt32().unwrap();
            let carry = ra.use_scratch_gpr(&mut args[2]);
            ra.asm.mov(result, 63).unwrap();
            ra.asm.cmp(CL, 63).unwrap();
            ra.asm.cmovnb(rxbyak::ECX, result).unwrap();
            ra.asm.movsxd(result.cvt64().unwrap(), operand).unwrap();
            ra.asm.bt_imm(carry.cvt32().unwrap(), 0).unwrap();
            ra.asm.sar_cl(result.cvt64().unwrap()).unwrap();
            ra.asm.setc(carry.cvt8().unwrap()).unwrap();
            ra.define_value(carry_ref, carry);
            ra.define_value(inst_ref, result);
        }
        ShiftOp::Ror => {
            let result = ra.use_scratch_gpr(&mut args[0]).cvt32().unwrap();
            let carry = ra.use_scratch_gpr(&mut args[2]);
            let end = ra.asm.create_label();
            ra.asm.test(CL, CL).unwrap();
            ra.asm.jz(&end, JmpType::Near).unwrap();
            ra.asm.ror_cl(result).unwrap();
            ra.asm.bt_imm(result, 31).unwrap();
            ra.asm.setc(carry.cvt8().unwrap()).unwrap();
            ra.asm.bind(&end).unwrap();
            ra.define_value(carry_ref, carry);
            ra.define_value(inst_ref, result);
        }
    }
}

// ---------------------------------------------------------------------------
// Masked shifts (shift amount already in valid range)
// ---------------------------------------------------------------------------

pub fn emit_logical_shift_left_masked32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_masked_shift(ctx, ra, inst_ref, inst, 32, ShiftOp::Shl);
}

pub fn emit_logical_shift_left_masked64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_masked_shift(ctx, ra, inst_ref, inst, 64, ShiftOp::Shl);
}

pub fn emit_logical_shift_right_masked32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_masked_shift(ctx, ra, inst_ref, inst, 32, ShiftOp::Shr);
}

pub fn emit_logical_shift_right_masked64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_masked_shift(ctx, ra, inst_ref, inst, 64, ShiftOp::Shr);
}

pub fn emit_arithmetic_shift_right_masked32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_masked_shift(ctx, ra, inst_ref, inst, 32, ShiftOp::Sar);
}

pub fn emit_arithmetic_shift_right_masked64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_masked_shift(ctx, ra, inst_ref, inst, 64, ShiftOp::Sar);
}

pub fn emit_rotate_right_masked32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_masked_shift(ctx, ra, inst_ref, inst, 32, ShiftOp::Ror);
}

pub fn emit_rotate_right_masked64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_masked_shift(ctx, ra, inst_ref, inst, 64, ShiftOp::Ror);
}

fn emit_masked_shift(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    bitsize: usize,
    op: ShiftOp,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());

    if !args[1].is_immediate() && op != ShiftOp::Ror && ctx.has_host_feature(HostFeature::BMI2) {
        let operand = ra.use_gpr(&mut args[0]);
        let shift = ra.use_gpr(&mut args[1]);
        let result = ra.scratch_gpr();
        let operand_sized = if bitsize == 32 {
            operand.cvt32().unwrap()
        } else {
            operand
        };
        let shift_sized = if bitsize == 32 {
            shift.cvt32().unwrap()
        } else {
            shift
        };
        let result_sized = if bitsize == 32 {
            result.cvt32().unwrap()
        } else {
            result
        };
        match op {
            ShiftOp::Shl => ra
                .asm
                .shlx(result_sized, operand_sized, shift_sized)
                .unwrap(),
            ShiftOp::Shr => ra
                .asm
                .shrx(result_sized, operand_sized, shift_sized)
                .unwrap(),
            ShiftOp::Sar => ra
                .asm
                .sarx(result_sized, operand_sized, shift_sized)
                .unwrap(),
            ShiftOp::Ror => unreachable!(),
        }
        ra.define_value(inst_ref, result);
        return;
    }

    // The non-BMI2 register form requires CL as its implicit count.
    if !args[1].is_immediate() {
        ra.use_loc(&mut args[1], HostLoc::Gpr(1)); // RCX
    }

    let result = ra.use_scratch_gpr(&mut args[0]);
    let result_sized = if bitsize == 32 {
        result.cvt32().unwrap()
    } else {
        result
    };

    if args[1].is_immediate() {
        // The IR signature for ArithmeticShiftRightMasked32/64 is
        // `(U32, U32)` / `(U64, U64)` (see opcode.rs:1127-1128) so the
        // shift arrives as ImmU32 or ImmU64 — not ImmU8. Use the
        // size-agnostic accessor and truncate to u8 (x86 shifts only
        // care about the low byte; the masking is implicit per Intel SDM
        // Vol. 2A "SHL/SHR/SAR/ROR — Shift count is masked to 5 or 6
        // bits depending on operand size", which matches AArch64
        // `…ShiftRightMasked` semantics).
        let shift = args[1].get_immediate_u64() as u8;
        match op {
            ShiftOp::Shl => ra.asm.shl(result_sized, shift).unwrap(),
            ShiftOp::Shr => ra.asm.shr(result_sized, shift).unwrap(),
            ShiftOp::Sar => ra.asm.sar(result_sized, shift).unwrap(),
            ShiftOp::Ror => ra.asm.ror(result_sized, shift).unwrap(),
        }
    } else {
        // Shift amount is already masked to valid range — x86's masking matches
        match op {
            ShiftOp::Shl => ra.asm.shl_cl(result_sized).unwrap(),
            ShiftOp::Shr => ra.asm.shr_cl(result_sized).unwrap(),
            ShiftOp::Sar => ra.asm.sar_cl(result_sized).unwrap(),
            ShiftOp::Ror => ra.asm.ror_cl(result_sized).unwrap(),
        }
    }

    ra.define_value(inst_ref, result);
}

/// RotateRightExtended: 33-bit rotate through carry (RCR by 1).
/// Upstream: EmitRotateRightExtended — captures GetCarryFromOp inline.
pub fn emit_rotate_right_extended(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    use crate::ir::opcode::Opcode;
    let carry_inst = ctx
        .block
        .and_then(|b| b.get_associated_pseudo_operation(inst_ref, Opcode::GetCarryFromOp));

    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);

    // Load carry into CF
    let carry = ra.use_gpr(&mut args[1]);
    ra.asm.bt_imm(carry.cvt32().unwrap(), 0).unwrap();

    // RCR by 1: rotate right through carry
    ra.asm.rcr(result.cvt32().unwrap(), 1).unwrap();

    // Capture carry inline
    if let Some(carry_ref) = carry_inst {
        let carry_out = ra.scratch_gpr();
        ra.asm.setc(carry_out.cvt8().unwrap()).unwrap();
        ra.asm
            .movzx(carry_out.cvt32().unwrap(), carry_out.cvt8().unwrap())
            .unwrap();
        ra.define_value(carry_ref, carry_out);
    }

    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// Extensions
// ---------------------------------------------------------------------------

pub fn emit_zero_extend_byte_to_word(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    ra.asm
        .movzx(result.cvt32().unwrap(), result.cvt8().unwrap())
        .unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_zero_extend_half_to_word(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    ra.asm
        .movzx(result.cvt32().unwrap(), result.cvt16().unwrap())
        .unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_zero_extend_byte_to_long(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    ra.asm
        .movzx(result.cvt32().unwrap(), result.cvt8().unwrap())
        .unwrap();
    // movzx to 32-bit implicitly zero-extends to 64-bit
    ra.define_value(inst_ref, result);
}

pub fn emit_zero_extend_half_to_long(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    ra.asm
        .movzx(result.cvt32().unwrap(), result.cvt16().unwrap())
        .unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_zero_extend_word_to_long(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    // mov r32, r32 zero-extends to 64 bits
    ra.asm
        .mov(result.cvt32().unwrap(), result.cvt32().unwrap())
        .unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_zero_extend_long_to_quad(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    // Move 64-bit value into XMM for 128-bit zero-extension
    let source = ra.use_gpr(&mut args[0]);
    let result = ra.scratch_xmm();
    ra.asm.pxor(result, result).unwrap();
    ra.asm.movq(result, source).unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_sign_extend_byte_to_word(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    ra.asm
        .movsx(result.cvt32().unwrap(), result.cvt8().unwrap())
        .unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_sign_extend_half_to_word(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    ra.asm
        .movsx(result.cvt32().unwrap(), result.cvt16().unwrap())
        .unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_sign_extend_byte_to_long(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    ra.asm.movsx(result, result.cvt8().unwrap()).unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_sign_extend_half_to_long(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    ra.asm.movsx(result, result.cvt16().unwrap()).unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_sign_extend_word_to_long(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    ra.asm.movsxd(result, result.cvt32().unwrap()).unwrap();
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// Bit operations
// ---------------------------------------------------------------------------

/// IsZero32: result = (a == 0) ? 1 : 0
pub fn emit_is_zero32(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    ra.asm
        .test(result.cvt32().unwrap(), result.cvt32().unwrap())
        .unwrap();
    ra.asm.sete(result.cvt8().unwrap()).unwrap();
    ra.asm
        .movzx(result.cvt32().unwrap(), result.cvt8().unwrap())
        .unwrap();
    ra.define_value(inst_ref, result);
}

/// IsZero64: result = (a == 0) ? 1 : 0
pub fn emit_is_zero64(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    ra.asm.test(result, result).unwrap();
    ra.asm.sete(result.cvt8().unwrap()).unwrap();
    ra.asm
        .movzx(result.cvt32().unwrap(), result.cvt8().unwrap())
        .unwrap();
    ra.define_value(inst_ref, result);
}

/// TestBit: result = (a >> bit) & 1
pub fn emit_test_bit(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());

    // Fast path: bit_idx is an immediate (e.g. TBZ/TBNZ with constant bit pos).
    if args[1].is_immediate() {
        let source = ra.use_gpr(&mut args[0]);
        let bit_idx = args[1].get_immediate_u8();
        let result = ra.scratch_gpr();
        ra.asm.bt_imm(source, bit_idx).unwrap();
        ra.asm.setc(result.cvt8().unwrap()).unwrap();
        ra.asm
            .movzx(result.cvt32().unwrap(), result.cvt8().unwrap())
            .unwrap();
        ra.define_value(inst_ref, result);
        return;
    }

    // General path: bit_idx in a register. x86 BT requires both operands to
    // share size; previously this passed bit_idx.cvt32() against a 64-bit
    // source, which the encoder rejects.
    let source = ra.use_gpr(&mut args[0]);
    let bit_idx = ra.use_gpr(&mut args[1]);

    let result = ra.scratch_gpr();
    ra.asm.bt(source, bit_idx).unwrap();
    ra.asm.setc(result.cvt8().unwrap()).unwrap();
    ra.asm
        .movzx(result.cvt32().unwrap(), result.cvt8().unwrap())
        .unwrap();
    ra.define_value(inst_ref, result);
}

/// MostSignificantBit: result = (a >> 31) & 1 (or >> 63 for 64-bit)
pub fn emit_most_significant_bit(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    // Shift right by 31 to get MSB into bit 0
    ra.asm.shr(result.cvt32().unwrap(), 31).unwrap();
    ra.define_value(inst_ref, result);
}

/// CountLeadingZeros32.
pub fn emit_count_leading_zeros32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = if ctx.has_host_feature(HostFeature::LZCNT) {
        let source = ra.use_gpr(&mut args[0]);
        let result = ra.scratch_gpr();
        ra.asm
            .lzcnt(result.cvt32().unwrap(), source.cvt32().unwrap())
            .unwrap();
        result
    } else {
        let source = ra.use_scratch_gpr(&mut args[0]);
        let result = ra.scratch_gpr();
        let temp = ra.scratch_gpr();
        ra.asm
            .bsr(result.cvt32().unwrap(), source.cvt32().unwrap())
            .unwrap();
        ra.asm.mov(temp.cvt32().unwrap(), 32i32).unwrap();
        ra.asm.xor_(result.cvt32().unwrap(), 31i32).unwrap();
        ra.asm
            .test(source.cvt32().unwrap(), source.cvt32().unwrap())
            .unwrap();
        ra.asm
            .cmove(result.cvt32().unwrap(), temp.cvt32().unwrap())
            .unwrap();
        ra.release(source);
        ra.release(temp);
        result
    };
    ra.define_value(inst_ref, result);
}

/// CountLeadingZeros64.
pub fn emit_count_leading_zeros64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = if ctx.has_host_feature(HostFeature::LZCNT) {
        let source = ra.use_gpr(&mut args[0]);
        let result = ra.scratch_gpr();
        ra.asm.lzcnt(result, source).unwrap();
        result
    } else {
        let source = ra.use_scratch_gpr(&mut args[0]);
        let result = ra.scratch_gpr();
        let temp = ra.scratch_gpr();
        ra.asm.bsr(result, source).unwrap();
        ra.asm.mov(temp.cvt32().unwrap(), 64i32).unwrap();
        ra.asm.xor_(result.cvt32().unwrap(), 63i32).unwrap();
        ra.asm.test(source, source).unwrap();
        ra.asm
            .cmove(result.cvt32().unwrap(), temp.cvt32().unwrap())
            .unwrap();
        ra.release(source);
        ra.release(temp);
        result
    };
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// Byte reversal
// ---------------------------------------------------------------------------

/// ByteReverseWord: result = bswap32(a)
pub fn emit_byte_reverse_word(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    ra.asm.bswap(result.cvt32().unwrap()).unwrap();
    ra.define_value(inst_ref, result);
}

/// ByteReverseDual: result = bswap64(a)
pub fn emit_byte_reverse_dual(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    ra.asm.bswap(result).unwrap();
    ra.define_value(inst_ref, result);
}

/// ByteReverseHalf: result = bswap16(a) = rol16(a, 8)
pub fn emit_byte_reverse_half(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    // Swap bytes within the low 16 bits
    ra.asm.rol(result.cvt16().unwrap(), 8).unwrap();
    // Zero-extend to 32 bits
    ra.asm
        .movzx(result.cvt32().unwrap(), result.cvt16().unwrap())
        .unwrap();
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// Extract / Pack
// ---------------------------------------------------------------------------

fn emit_extract_register(ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst, bit_size: u16) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());

    let result = ra
        .use_scratch_gpr(&mut args[0])
        .change_bit(bit_size)
        .unwrap();
    let operand = ra.use_gpr(&mut args[1]).change_bit(bit_size).unwrap();
    let lsb = args[2].get_immediate_u8();

    ra.asm.shrd(result, operand, lsb).unwrap();

    ra.define_value(inst_ref, result);
}

/// ExtractRegister32: result = (b:a) >> lsb.
pub fn emit_extract_register32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_extract_register(ra, inst_ref, inst, 32);
}

/// ExtractRegister64: result = (b:a) >> lsb.
pub fn emit_extract_register64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_extract_register(ra, inst_ref, inst, 64);
}

/// Pack2x32To1x64: result = (high << 32) | low
pub fn emit_pack_2x32_to_1x64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let lo = ra.use_scratch_gpr(&mut args[0]);
    let hi = ra.use_gpr(&mut args[1]);

    // Zero-extend low to 64-bit
    ra.asm
        .mov(lo.cvt32().unwrap(), lo.cvt32().unwrap())
        .unwrap();
    // Shift high left by 32
    let hi_scratch = ra.scratch_gpr();
    ra.asm.mov(hi_scratch, hi).unwrap();
    ra.asm.shl(hi_scratch, 32).unwrap();
    // OR them together
    ra.asm.or_(lo, hi_scratch).unwrap();
    ra.define_value(inst_ref, lo);
}

/// LeastSignificantWord: result = (u32) a
pub fn emit_least_significant_word(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    // mov r32, r32 zero-extends
    ra.asm
        .mov(result.cvt32().unwrap(), result.cvt32().unwrap())
        .unwrap();
    ra.define_value(inst_ref, result);
}

/// MostSignificantWord: result = (u32)(a >> 32)
/// Upstream: EmitMostSignificantWord — captures GetCarryFromOp inline.
pub fn emit_most_significant_word(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    use crate::ir::opcode::Opcode;
    let carry_inst = ctx
        .block
        .and_then(|b| b.get_associated_pseudo_operation(inst_ref, Opcode::GetCarryFromOp));

    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    ra.asm.shr(result, 32).unwrap();

    if let Some(carry_ref) = carry_inst {
        let carry = ra.scratch_gpr();
        ra.asm.setc(carry.cvt8().unwrap()).unwrap();
        ra.asm
            .movzx(carry.cvt32().unwrap(), carry.cvt8().unwrap())
            .unwrap();
        ra.define_value(carry_ref, carry);
    }

    ra.define_value(inst_ref, result);
}

/// LeastSignificantHalf: result = (u16) a
pub fn emit_least_significant_half(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    ra.asm
        .movzx(result.cvt32().unwrap(), result.cvt16().unwrap())
        .unwrap();
    ra.define_value(inst_ref, result);
}

/// LeastSignificantByte: result = (u8) a
pub fn emit_least_significant_byte(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    ra.asm
        .movzx(result.cvt32().unwrap(), result.cvt8().unwrap())
        .unwrap();
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// Conditional select
// ---------------------------------------------------------------------------

/// ConditionalSelect32: result = cond ? then : else
pub fn emit_conditional_select32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_conditional_select(ctx, ra, inst_ref, inst, 32);
}

/// ConditionalSelect64: result = cond ? then : else
pub fn emit_conditional_select64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_conditional_select(ctx, ra, inst_ref, inst, 64);
}

fn emit_conditional_select(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    bitsize: usize,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());

    let cond = args[0].get_immediate_cond();
    let rax = ra.scratch_gpr_at(HOST_RAX);
    let then_ = use_scalar_gpr_read(ctx, ra, &mut args[1], bitsize);
    let else_ = use_scalar_gpr_scratch(ctx, ra, &mut args[2], bitsize);

    let then_sized = if bitsize == 32 {
        then_.cvt32().unwrap()
    } else {
        then_
    };
    let else_sized = if bitsize == 32 {
        else_.cvt32().unwrap()
    } else {
        else_
    };

    // Load NZCV from jit_state into x86 flags
    load_nzcv_into_flags_with_rax(ra, rax, cond, ctx.jit_state_info.offsetof_cpsr_nzcv);

    // cmovcc: if condition true, replace else_ with then_
    emit_cmovcc(ra.asm, cond, else_sized, then_sized);

    ra.define_value(inst_ref, else_);
}

/// ConditionalSelectNZCV: result = cond ? nzcv_then : nzcv_else
pub fn emit_conditional_select_nzcv(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());

    let cond = args[0].get_immediate_cond();
    let rax = ra.scratch_gpr_at(HOST_RAX);
    let then_ = ra.use_gpr(&mut args[1]);
    let else_ = ra.use_scratch_gpr(&mut args[2]);

    let then32 = then_.cvt32().unwrap();
    let else32 = else_.cvt32().unwrap();

    load_nzcv_into_flags_with_rax(ra, rax, cond, ctx.jit_state_info.offsetof_cpsr_nzcv);
    emit_cmovcc(ra.asm, cond, else32, then32);

    ra.define_value(inst_ref, else_);
}

// ---------------------------------------------------------------------------
// ReplicateBit
// ---------------------------------------------------------------------------

/// ReplicateBit32: result = (a & (1 << bit)) ? 0xFFFFFFFF : 0
pub fn emit_replicate_bit32(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    let bit_idx = args[1].get_immediate_u8();
    // Arithmetic shift right to replicate the bit
    ra.asm.shl(result.cvt32().unwrap(), 31 - bit_idx).unwrap();
    ra.asm.sar(result.cvt32().unwrap(), 31).unwrap();
    ra.define_value(inst_ref, result);
}

/// ReplicateBit64: result = (a & (1 << bit)) ? 0xFFFF...FF : 0
pub fn emit_replicate_bit64(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_gpr(&mut args[0]);
    let bit_idx = args[1].get_immediate_u8();
    ra.asm.shl(result, 63 - bit_idx).unwrap();
    ra.asm.sar(result, 63).unwrap();
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// Max / Min (scalar)
// ---------------------------------------------------------------------------

pub fn emit_max_signed32(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_scratch_gpr(&mut args[0]);
    let b = ra.use_gpr(&mut args[1]);
    ra.asm.cmp(a.cvt32().unwrap(), b.cvt32().unwrap()).unwrap();
    ra.asm
        .cmovl(a.cvt32().unwrap(), b.cvt32().unwrap())
        .unwrap();
    ra.define_value(inst_ref, a);
}

pub fn emit_max_signed64(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_scratch_gpr(&mut args[0]);
    let b = ra.use_gpr(&mut args[1]);
    ra.asm.cmp(a, b).unwrap();
    ra.asm.cmovl(a, b).unwrap();
    ra.define_value(inst_ref, a);
}

pub fn emit_max_unsigned32(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_scratch_gpr(&mut args[0]);
    let b = ra.use_gpr(&mut args[1]);
    ra.asm.cmp(a.cvt32().unwrap(), b.cvt32().unwrap()).unwrap();
    ra.asm
        .cmovb(a.cvt32().unwrap(), b.cvt32().unwrap())
        .unwrap();
    ra.define_value(inst_ref, a);
}

pub fn emit_max_unsigned64(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_scratch_gpr(&mut args[0]);
    let b = ra.use_gpr(&mut args[1]);
    ra.asm.cmp(a, b).unwrap();
    ra.asm.cmovb(a, b).unwrap();
    ra.define_value(inst_ref, a);
}

pub fn emit_min_signed32(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_scratch_gpr(&mut args[0]);
    let b = ra.use_gpr(&mut args[1]);
    ra.asm.cmp(a.cvt32().unwrap(), b.cvt32().unwrap()).unwrap();
    ra.asm
        .cmovg(a.cvt32().unwrap(), b.cvt32().unwrap())
        .unwrap();
    ra.define_value(inst_ref, a);
}

pub fn emit_min_signed64(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_scratch_gpr(&mut args[0]);
    let b = ra.use_gpr(&mut args[1]);
    ra.asm.cmp(a, b).unwrap();
    ra.asm.cmovg(a, b).unwrap();
    ra.define_value(inst_ref, a);
}

pub fn emit_min_unsigned32(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_scratch_gpr(&mut args[0]);
    let b = ra.use_gpr(&mut args[1]);
    ra.asm.cmp(a.cvt32().unwrap(), b.cvt32().unwrap()).unwrap();
    ra.asm
        .cmova(a.cvt32().unwrap(), b.cvt32().unwrap())
        .unwrap();
    ra.define_value(inst_ref, a);
}

pub fn emit_min_unsigned64(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_scratch_gpr(&mut args[0]);
    let b = ra.use_gpr(&mut args[1]);
    ra.asm.cmp(a, b).unwrap();
    ra.asm.cmova(a, b).unwrap();
    ra.define_value(inst_ref, a);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::x64::callback::Callback;
    use crate::backend::x64::emit_context::{EmitCallbacks, EmitConfig, EmitContext};
    use crate::backend::x64::hostloc::ANY_GPR;
    use crate::backend::x64::reg_alloc::RegAlloc;
    use crate::ir::inst::Inst;
    use crate::ir::location::LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::value::Value;
    use rxbyak::CodeAssembler;

    struct NoopCallback;

    impl Callback for NoopCallback {
        fn emit_call(
            &self,
            _code: &mut rxbyak::CodeAssembler,
            _setup: &dyn Fn(&mut rxbyak::CodeAssembler, &[rxbyak::Reg]) -> rxbyak::Result<()>,
        ) -> rxbyak::Result<()> {
            unreachable!("callback emission is not used in this unit test");
        }

        fn emit_call_with_return_pointer(
            &self,
            _code: &mut rxbyak::CodeAssembler,
            _setup: &dyn Fn(
                &mut rxbyak::CodeAssembler,
                rxbyak::Reg,
                &[rxbyak::Reg],
            ) -> rxbyak::Result<()>,
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
            tpidrro_el0: None,
            tpidr_el0: None,
            cntfrq_el0: 600_000_000,
            ctr_el0: 0x8444_c004,
            dczid_el0: 4,
            hook_data_cache_operations: false,
            hook_isb: false,
        }
    }

    fn make_inst_info(count: usize) -> Vec<(u32, usize)> {
        vec![(1, 64); count]
    }

    #[test]
    fn extract_register_immediates_emit_shrd_for_both_widths() {
        for opcode in [Opcode::ExtractRegister32, Opcode::ExtractRegister64] {
            let mut asm = CodeAssembler::new(4096).unwrap();
            let inst_info = make_inst_info(3);
            let mut ra = RegAlloc::new_default(&mut asm, inst_info);
            let lhs = ra.scratch_gpr();
            ra.define_value(InstRef(0), lhs);
            ra.end_of_alloc_scope();
            let rhs = ra.scratch_gpr();
            ra.define_value(InstRef(1), rhs);
            ra.end_of_alloc_scope();

            let inst = Inst::new(
                opcode,
                &[
                    Value::Inst(InstRef(0)),
                    Value::Inst(InstRef(1)),
                    Value::ImmU8(7),
                ],
            );
            let config = dummy_emit_config();
            let ctx = EmitContext::new(LocationDescriptor::new(0), &config);
            let start = ra.asm.size();

            match opcode {
                Opcode::ExtractRegister32 => {
                    emit_extract_register32(&ctx, &mut ra, InstRef(2), &inst)
                }
                Opcode::ExtractRegister64 => {
                    emit_extract_register64(&ctx, &mut ra, InstRef(2), &inst)
                }
                _ => unreachable!(),
            }
            ra.end_of_alloc_scope();

            let code = &ra.asm.code()[start..];
            let shrd = code
                .windows(2)
                .position(|bytes| bytes == [0x0f, 0xac])
                .unwrap_or_else(|| panic!("missing SHRD for {opcode:?}: {code:02x?}"));
            assert_eq!(code.last(), Some(&7), "opcode={opcode:?}");

            let has_rex_w = shrd > 0 && code[shrd - 1] & 0xf8 == 0x48;
            assert_eq!(has_rex_w, opcode == Opcode::ExtractRegister64);
        }
    }

    #[test]
    fn test_shift_cl_generates_code() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let start = asm.size();
        asm.shl_cl(Reg::gpr32(0)).unwrap(); // shl eax, cl
        assert!(asm.size() > start);
    }

    #[test]
    fn test_shift_cl_64bit() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let start = asm.size();
        asm.shl_cl(Reg::gpr64(0)).unwrap(); // shl rax, cl
        assert!(asm.size() > start);
        // Should have REX prefix
        assert!(asm.size() - start >= 3); // REX.W + D3 + ModRM
    }

    #[test]
    fn test_shift_cl_high_register() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let start = asm.size();
        asm.shl_cl(Reg::gpr64(8)).unwrap(); // shl r8, cl
        assert!(asm.size() > start);
        // REX.W + REX.B + D3 + ModRM
        assert!(asm.size() - start >= 3);
    }

    #[test]
    fn test_emit_logical_shift_left_masked64_reserves_rcx_before_result() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let inst_info = make_inst_info(5);
        let mut ra = RegAlloc::new_default(&mut asm, inst_info);

        // Occupy RAX and RBX so RCX becomes the first empty candidate for the
        // destination if the emitter allocates the result before reserving the
        // shift-count register.
        let rax = ra.scratch_gpr_at(HOST_RAX);
        ra.define_value(InstRef(3), rax);
        let rbx = ra.scratch_gpr_at(HOST_RBX);
        ra.define_value(InstRef(4), rbx);

        // Source and shift values live in other registers.
        let src = ra.scratch_gpr_at(HOST_R8);
        ra.define_value(InstRef(0), src);
        let shift = ra.scratch_gpr_at(HOST_R9);
        ra.define_value(InstRef(1), shift);
        ra.end_of_alloc_scope();

        let inst = Inst::new(
            Opcode::LogicalShiftLeftMasked64,
            &[Value::Inst(InstRef(0)), Value::Inst(InstRef(1))],
        );

        let emit_config = dummy_emit_config();
        let ctx = EmitContext::new(LocationDescriptor::new(0), &emit_config);
        emit_logical_shift_left_masked64(&ctx, &mut ra, InstRef(2), &inst);
    }

    #[test]
    fn test_emit_add32_immediate() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let inst_info = make_inst_info(4);
        let mut ra = RegAlloc::new_default(&mut asm, inst_info);

        // Define a value for arg[0]
        let reg = ra.scratch_gpr();
        ra.define_value(InstRef(0), reg);
        ra.end_of_alloc_scope();

        let inst = Inst::new(
            Opcode::Add32,
            &[
                Value::Inst(InstRef(0)),
                Value::ImmU32(42),
                Value::ImmU1(false),
            ],
        );

        let start = ra.asm.size();
        let mut args = ra.get_argument_info(InstRef(1), &inst.args, inst.num_args());
        let result = ra.use_scratch_gpr(&mut args[0]);
        ra.asm.add(result.cvt32().unwrap(), 42i32).unwrap();
        ra.define_value(InstRef(1), result);
        ra.end_of_alloc_scope();

        assert!(ra.asm.size() > start, "Should have emitted code for add32");
    }

    #[test]
    fn test_emit_unsigned_div32_under_full_gpr_pressure() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let inst_info = make_inst_info(ANY_GPR.len() + 2);
        let mut ra = RegAlloc::new_default(&mut asm, inst_info);
        let config = dummy_emit_config();
        let ctx = EmitContext::new(LocationDescriptor::new(0), &config);

        for (i, &loc) in ANY_GPR.iter().enumerate() {
            let reg = ra.scratch_gpr_at(loc);
            ra.define_value(InstRef(i as u32), reg);
            ra.end_of_alloc_scope();
        }

        let inst = Inst::new(
            Opcode::UnsignedDiv32,
            &[Value::Inst(InstRef(0)), Value::Inst(InstRef(1))],
        );
        let start = ra.asm.size();
        emit_unsigned_div32(&ctx, &mut ra, InstRef(ANY_GPR.len() as u32), &inst);
        ra.end_of_alloc_scope();

        assert!(
            ra.asm.size() > start,
            "Should have emitted code for unsigned div32"
        );
    }
}
