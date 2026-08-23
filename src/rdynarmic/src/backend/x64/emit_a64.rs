use rxbyak::RegExp;
use rxbyak::{byte_ptr, dword_ptr, qword_ptr, xmmword_ptr};
use rxbyak::{JmpType, R15, RAX};

use crate::backend::x64::block_of_code::STACK_LAYOUT_RSP_OFFSET;
use crate::backend::x64::emit_context::EmitContext;
use crate::backend::x64::host_feature::HostFeature;
use crate::backend::x64::hostloc::*;
use crate::backend::x64::jit_state::A64JitState;
use crate::backend::x64::nzcv_util;
use crate::backend::x64::reg_alloc::{Argument, RegAlloc};
use crate::backend::x64::stack_layout::StackLayout;
use crate::ir::inst::Inst;
use crate::ir::opcode::Opcode;
use crate::ir::value::{InstRef, Value};

/// Walks Identity chain backwards from `value` and returns the producing
/// opcode (or None if `value` is an immediate / no Inst).
fn defining_opcode(ctx: &EmitContext, value: &Value) -> Option<Opcode> {
    let block = ctx.block?;
    let mut cur = match value {
        Value::Inst(r) => *r,
        _ => return None,
    };
    loop {
        let inst = block.get(cur);
        if inst.opcode == Opcode::Identity {
            match inst.args[0] {
                Value::Inst(next) => cur = next,
                _ => return None,
            }
        } else {
            return Some(inst.opcode);
        }
    }
}

fn is_memory_load_opcode(op: Opcode) -> bool {
    matches!(
        op,
        Opcode::A64ReadMemory8
            | Opcode::A64ReadMemory16
            | Opcode::A64ReadMemory32
            | Opcode::A64ReadMemory64
            | Opcode::A64ReadMemory128
            | Opcode::A64ExclusiveReadMemory8
            | Opcode::A64ExclusiveReadMemory16
            | Opcode::A64ExclusiveReadMemory32
            | Opcode::A64ExclusiveReadMemory64
            | Opcode::A64ExclusiveReadMemory128
    )
}

// ---------------------------------------------------------------------------
// GPR access
// ---------------------------------------------------------------------------

/// A64GetW: result = (u32) jit_state.reg[n]
pub fn emit_a64_get_w(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let reg_index = inst.args[0].get_a64_reg() as usize;
    let offset = A64JitState::reg_offset(reg_index);

    let result = ra.scratch_gpr();
    let r32 = result.cvt32().unwrap();
    ra.asm
        .mov(r32, dword_ptr(RegExp::from(R15) + offset as i32))
        .unwrap();
    ra.define_value(inst_ref, result);
}

/// A64GetX: result = (u64) jit_state.reg[n]
pub fn emit_a64_get_x(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let reg_index = inst.args[0].get_a64_reg() as usize;
    let offset = A64JitState::reg_offset(reg_index);

    let result = ra.scratch_gpr();
    ra.asm
        .mov(result, qword_ptr(RegExp::from(R15) + offset as i32))
        .unwrap();
    ra.define_value(inst_ref, result);
}

/// A64SetW: jit_state.reg[n] = zero_extend(value32)
pub fn emit_a64_set_w(_ctx: &EmitContext, ra: &mut RegAlloc, _inst_ref: InstRef, inst: &Inst) {
    let reg_index = inst.args[0].get_a64_reg() as usize;
    let offset = A64JitState::reg_offset(reg_index);
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());

    // Check if we can use an immediate store
    if args[1].is_immediate() && args[1].fits_in_immediate_s32() {
        let imm = args[1].get_immediate_u32();
        // Zero-extend by writing as 64-bit with zero-extended immediate
        ra.asm
            .mov(qword_ptr(RegExp::from(R15) + offset as i32), imm as i32)
            .unwrap();
    } else {
        let source = ra.use_scratch_gpr(&mut args[1]);
        // Zero-extend 32-bit to 64-bit: mov r32, r32 clears upper bits
        let s32 = source.cvt32().unwrap();
        ra.asm.mov(s32, s32).unwrap();
        ra.asm
            .mov(qword_ptr(RegExp::from(R15) + offset as i32), source)
            .unwrap();
    }
}

/// A64SetX: jit_state.reg[n] = value64
pub fn emit_a64_set_x(ctx: &EmitContext, ra: &mut RegAlloc, _inst_ref: InstRef, inst: &Inst) {
    let reg_index = inst.args[0].get_a64_reg() as usize;
    let offset = A64JitState::reg_offset(reg_index);
    // Capture defining opcode of the source value BEFORE regalloc churn.
    let source_opcode = defining_opcode(ctx, &inst.args[1]);
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());

    if args[1].is_immediate() && args[1].fits_in_immediate_s32() {
        let imm = args[1].get_immediate_s32() as i32;
        ra.asm
            .mov(qword_ptr(RegExp::from(R15) + offset as i32), imm)
            .unwrap();
    } else {
        let source = ra.use_gpr(&mut args[1]);
        ra.asm
            .mov(qword_ptr(RegExp::from(R15) + offset as i32), source)
            .unwrap();
    }

    // RUZU_TRAP_SETX_BYTE5_21=1 — emit inline check for the
    // STK `(valid_addr << 8) | byte` corruption pattern.
    //
    // The wedge corrupt value is `0x0000_2101_xxxx_xxxx` = a heap
    // pointer (heap base 0x21_0160_0000) shifted left by 8 bits.
    // Filter:
    //   byte 7 == 0x00 (zero pad)
    //   byte 6 == 0x00 (zero pad)
    //   byte 5 == 0x21 (heap top byte)
    //   byte 4 == 0x01 (heap second byte; heap range is 0x21_01..)
    // 4-byte filter is tight enough to skip random string scans
    // (which usually have non-0x01 in byte 4). Traps with UD2 when
    // the just-stored X-register value matches.
    //
    // SIGILL handler dumps host RIP + RAX (= reg_index) + R15 + JitState.
    // RUZU_TRAP_SETX_SKIP_LOADS=1 — skip when source value comes from
    // a memory load (LDR/LDP). String-scan functions (strchr, strlen)
    // load random heap data that often happens to match our filter,
    // producing false positives. Disable this to catch the FIRST
    // corrupt SetX regardless of source.
    let skip_loads = std::env::var_os("RUZU_TRAP_SETX_SKIP_LOADS").is_some();
    let skip_due_to_load = skip_loads && source_opcode.is_some_and(is_memory_load_opcode);
    // RUZU_TRAP_SETX_SKIP_PC=0xPC,0xPC,... — skip specific guest PCs
    // (e.g., known-FP string-scan PCs).
    let block_pc = ctx.arch.extract_pc(ctx.location);
    let skip_due_to_pc = match std::env::var("RUZU_TRAP_SETX_SKIP_PC") {
        Ok(spec) => spec
            .split(',')
            .filter_map(|p| u64::from_str_radix(p.trim().trim_start_matches("0x"), 16).ok())
            .any(|p| p == block_pc),
        Err(_) => false,
    };
    if std::env::var_os("RUZU_TRAP_SETX_BYTE5_21").is_some() && !skip_due_to_load && !skip_due_to_pc
    {
        let ok = ra.asm.create_label();
        // byte 7 must be zero
        ra.asm
            .cmp(byte_ptr(RegExp::from(R15) + (offset + 7) as i32), 0i32)
            .unwrap();
        ra.asm.jne(&ok, JmpType::Near).unwrap();
        // byte 6 must be zero
        ra.asm
            .cmp(byte_ptr(RegExp::from(R15) + (offset + 6) as i32), 0i32)
            .unwrap();
        ra.asm.jne(&ok, JmpType::Near).unwrap();
        // byte 5 must be 0x21
        ra.asm
            .cmp(byte_ptr(RegExp::from(R15) + (offset + 5) as i32), 0x21i32)
            .unwrap();
        ra.asm.jne(&ok, JmpType::Near).unwrap();
        // byte 4 must be 0x01 (heap prefix byte 2 after shift)
        ra.asm
            .cmp(byte_ptr(RegExp::from(R15) + (offset + 4) as i32), 0x01i32)
            .unwrap();
        ra.asm.jne(&ok, JmpType::Near).unwrap();
        // matched — encode reg_index into RAX so the SIGILL handler
        // can identify which X-register triggered the trap. Then trap.
        ra.asm.mov(RAX, reg_index as i32).unwrap();
        ra.asm.ud2().unwrap();
        ra.asm.bind(&ok).unwrap();
    }
}

// ---------------------------------------------------------------------------
// SP access
// ---------------------------------------------------------------------------

/// A64GetSP: result = jit_state.sp
pub fn emit_a64_get_sp(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, _inst: &Inst) {
    let offset = A64JitState::offset_of_sp();
    let result = ra.scratch_gpr();
    ra.asm
        .mov(result, qword_ptr(RegExp::from(R15) + offset as i32))
        .unwrap();
    ra.define_value(inst_ref, result);
}

/// A64SetSP: jit_state.sp = value
pub fn emit_a64_set_sp(_ctx: &EmitContext, ra: &mut RegAlloc, _inst_ref: InstRef, inst: &Inst) {
    let offset = A64JitState::offset_of_sp();
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());

    if args[0].is_immediate() && args[0].fits_in_immediate_s32() {
        let imm = args[0].get_immediate_s32() as i32;
        ra.asm
            .mov(qword_ptr(RegExp::from(R15) + offset as i32), imm)
            .unwrap();
    } else {
        let source = ra.use_gpr(&mut args[0]);
        ra.asm
            .mov(qword_ptr(RegExp::from(R15) + offset as i32), source)
            .unwrap();
    }
}

/// A64SetPC: jit_state.pc = value
pub fn emit_a64_set_pc(_ctx: &EmitContext, ra: &mut RegAlloc, _inst_ref: InstRef, inst: &Inst) {
    let offset = A64JitState::offset_of_pc();
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());

    if args[0].is_immediate() && args[0].fits_in_immediate_s32() {
        let imm = args[0].get_immediate_s32() as i32;
        ra.asm
            .mov(qword_ptr(RegExp::from(R15) + offset as i32), imm)
            .unwrap();
    } else {
        let source = ra.use_gpr(&mut args[0]);
        ra.asm
            .mov(qword_ptr(RegExp::from(R15) + offset as i32), source)
            .unwrap();
    }
}

// ---------------------------------------------------------------------------
// Vector register access
// ---------------------------------------------------------------------------

/// A64GetS: result = (f32) jit_state.vec[n][0]
pub fn emit_a64_get_s(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let vec_index = inst.args[0].get_a64_vec() as usize;
    let offset = A64JitState::vec_offset(vec_index, 0);

    let result = ra.scratch_xmm();
    ra.asm
        .movd(result, dword_ptr(RegExp::from(R15) + offset as i32))
        .unwrap();
    ra.define_value(inst_ref, result);
}

/// A64GetD: result = (f64) jit_state.vec[n][0..1]
pub fn emit_a64_get_d(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let vec_index = inst.args[0].get_a64_vec() as usize;
    let offset = A64JitState::vec_offset(vec_index, 0);

    let result = ra.scratch_xmm();
    ra.asm
        .movq(result, qword_ptr(RegExp::from(R15) + offset as i32))
        .unwrap();
    ra.define_value(inst_ref, result);
}

/// A64GetQ: result = (u128) jit_state.vec[n]
pub fn emit_a64_get_q(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let vec_index = inst.args[0].get_a64_vec() as usize;
    let offset = A64JitState::vec_offset(vec_index, 0);

    let result = ra.scratch_xmm();
    ra.asm
        .movaps(result, xmmword_ptr(RegExp::from(R15) + offset as i32))
        .unwrap();
    ra.define_value(inst_ref, result);
}

/// A64SetS: jit_state.vec[n] = zero_extend_128(value32)
pub fn emit_a64_set_s(_ctx: &EmitContext, ra: &mut RegAlloc, _inst_ref: InstRef, inst: &Inst) {
    let vec_index = inst.args[0].get_a64_vec() as usize;
    let offset = A64JitState::vec_offset(vec_index, 0);
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());

    let source = ra.use_xmm(&mut args[1]);
    // Zero the destination, then move the scalar
    let tmp = ra.scratch_xmm();
    ra.asm.pxor(tmp, tmp).unwrap();
    ra.asm.movss(tmp, source).unwrap();
    ra.asm
        .movaps(xmmword_ptr(RegExp::from(R15) + offset as i32), tmp)
        .unwrap();
}

/// A64SetD: jit_state.vec[n] = zero_extend_128(value64)
pub fn emit_a64_set_d(_ctx: &EmitContext, ra: &mut RegAlloc, _inst_ref: InstRef, inst: &Inst) {
    let vec_index = inst.args[0].get_a64_vec() as usize;
    let offset = A64JitState::vec_offset(vec_index, 0);
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());

    let source = ra.use_scratch_xmm(&mut args[1]);
    // movq xmm, xmm zeros upper 64 bits
    ra.asm.movq(source, source).unwrap();
    ra.asm
        .movaps(xmmword_ptr(RegExp::from(R15) + offset as i32), source)
        .unwrap();
}

/// A64SetQ: jit_state.vec[n] = value128
pub fn emit_a64_set_q(_ctx: &EmitContext, ra: &mut RegAlloc, _inst_ref: InstRef, inst: &Inst) {
    let vec_index = inst.args[0].get_a64_vec() as usize;
    let offset = A64JitState::vec_offset(vec_index, 0);
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());

    let source = ra.use_xmm(&mut args[1]);
    ra.asm
        .movaps(xmmword_ptr(RegExp::from(R15) + offset as i32), source)
        .unwrap();
}

// ---------------------------------------------------------------------------
// NZCV / flags
// ---------------------------------------------------------------------------

/// A64GetNZCVRaw: result = nzcv_from_x64(jit_state.cpsr_nzcv)
/// Returns the user-visible ARM NZCV (bits 31:28) by converting the JIT's
/// internal x64-packed storage format.
///
/// Upstream: `A64EmitX64::EmitA64GetNZCVRaw` (a64_emit_x64.cpp).
pub fn emit_a64_get_nzcv_raw(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    _inst: &Inst,
) {
    let offset = A64JitState::offset_of_cpsr_nzcv();
    let result = ra.scratch_gpr();
    let r32 = result.cvt32().unwrap();
    ra.asm
        .mov(r32, dword_ptr(RegExp::from(R15) + offset as i32))
        .unwrap();
    if ctx.has_host_feature(HostFeature::FAST_BMI2) {
        let tmp = ra.scratch_gpr();
        let tmp32 = tmp.cvt32().unwrap();
        ra.asm.mov(tmp32, nzcv_util::X64_MASK as i32).unwrap();
        ra.asm.pext(r32, r32, tmp32).unwrap();
        ra.asm.shl(r32, 28).unwrap();
    } else {
        // ((cpsr_nzcv & X64_MASK) * FROM_X64_MULTIPLIER) & ARM_MASK
        ra.asm.and_(r32, nzcv_util::X64_MASK as i32).unwrap();
        let tmp = ra.scratch_gpr();
        let tmp32 = tmp.cvt32().unwrap();
        ra.asm
            .mov(tmp32, nzcv_util::FROM_X64_MULTIPLIER as i32)
            .unwrap();
        ra.asm.imul(r32, tmp32).unwrap();
        ra.asm.and_(r32, nzcv_util::ARM_MASK as i32).unwrap();
    }
    ra.define_value(inst_ref, result);
}

/// A64SetNZCVRaw: jit_state.cpsr_nzcv = nzcv_to_x64(value)
/// The value is in ARM NZCV format (bits 31:28); convert to x86-64 packed
/// format (bits 15=N, 14=Z, 8=C, 0=V) and store.
///
/// Upstream: `A64EmitX64::EmitA64SetNZCVRaw` (a64_emit_x64.cpp): the "Raw"
/// variant takes user-visible ARM CPSR.NZCV format and converts it to the
/// JIT's internal x64 storage format.
pub fn emit_a64_set_nzcv_raw(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    _inst_ref: InstRef,
    inst: &Inst,
) {
    let offset = A64JitState::offset_of_cpsr_nzcv();
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());

    let nzcv = ra.use_scratch_gpr(&mut args[0]);
    let nzcv32 = nzcv.cvt32().unwrap();
    ra.asm.shr(nzcv32, 28).unwrap();
    if ctx.has_host_feature(HostFeature::FAST_BMI2) {
        let tmp = ra.scratch_gpr();
        let tmp32 = tmp.cvt32().unwrap();
        ra.asm.mov(tmp32, nzcv_util::X64_MASK as i32).unwrap();
        ra.asm.pdep(nzcv32, nzcv32, tmp32).unwrap();
    } else {
        // ((nzcv >> 28) * TO_X64_MULTIPLIER) & X64_MASK
        let tmp = ra.scratch_gpr();
        let tmp32 = tmp.cvt32().unwrap();
        ra.asm
            .mov(tmp32, nzcv_util::TO_X64_MULTIPLIER as i32)
            .unwrap();
        ra.asm.imul(nzcv32, tmp32).unwrap();
        ra.asm.and_(nzcv32, nzcv_util::X64_MASK as i32).unwrap();
    }
    ra.asm
        .mov(dword_ptr(RegExp::from(R15) + offset as i32), nzcv32)
        .unwrap();
}

/// A64SetNZCV: jit_state.cpsr_nzcv = value (input is already in x86-64 packed
/// flag format produced by `lahf+seto` / `GetNZCVFromOp`). Just store.
///
/// Upstream: `A64EmitX64::EmitA64SetNZCV` (a64_emit_x64.cpp): the non-Raw
/// variant takes the JIT's internal x64-packed format and stores it directly.
pub fn emit_a64_set_nzcv(_ctx: &EmitContext, ra: &mut RegAlloc, _inst_ref: InstRef, inst: &Inst) {
    let offset = A64JitState::offset_of_cpsr_nzcv();
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());

    let source = ra.use_gpr(&mut args[0]);
    ra.asm
        .mov(
            dword_ptr(RegExp::from(R15) + offset as i32),
            source.cvt32().unwrap(),
        )
        .unwrap();
}

/// A64GetCFlag: result = (cpsr_nzcv >> 8) & 1  (carry flag in x86-64 format)
pub fn emit_a64_get_c_flag(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, _inst: &Inst) {
    let offset = A64JitState::offset_of_cpsr_nzcv();
    let result = ra.scratch_gpr();
    let r32 = result.cvt32().unwrap();
    ra.asm
        .mov(r32, dword_ptr(RegExp::from(R15) + offset as i32))
        .unwrap();
    ra.asm.shr(r32, nzcv_util::X64_C_FLAG_BIT as u8).unwrap();
    ra.asm.and_(r32, 1).unwrap();
    ra.define_value(inst_ref, result);
}

/// A64SetCheckBit: stack_layout.check_bit = value & 1
pub fn emit_a64_set_check_bit(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    _inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());
    let source = ra.use_gpr(&mut args[0]);
    let offset = STACK_LAYOUT_RSP_OFFSET + StackLayout::check_bit_offset();
    let src8 = source.cvt8().unwrap();
    ra.asm
        .mov(
            rxbyak::byte_ptr(RegExp::from(rxbyak::RSP) + offset as i32),
            src8,
        )
        .unwrap();
}

// ---------------------------------------------------------------------------
// NZCV pseudo-ops (GetCarryFromOp, GetOverflowFromOp, GetNZCVFromOp)
// ---------------------------------------------------------------------------

/// GetCarryFromOp: result = CF after the producing instruction.
///
/// This is a pseudo-op. When the producing instruction (shift, add, sub, etc.)
/// has already captured the carry via GetAssociatedPseudoOperation and emitted
/// SETC inline, this handler is a no-op. Otherwise, it falls back to reading
/// the current CF from RFLAGS (which may be stale if other instructions were
/// emitted between the producer and this handler).
pub fn emit_get_carry_from_op(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    _inst: &Inst,
) {
    // If the producing instruction already defined this value inline, skip.
    if ra.is_value_defined(inst_ref) {
        ra.register_pseudo_operation(inst_ref, &_inst.args, _inst.num_args());
        return;
    }
    // Fallback: read CF from current RFLAGS (correct only if no intervening instructions).
    let result = ra.scratch_gpr();
    let r8 = result.cvt8().unwrap();
    ra.asm.setc(r8).unwrap();
    let r32 = result.cvt32().unwrap();
    ra.asm.movzx(r32, r8).unwrap();
    ra.define_value(inst_ref, result);
}

/// GetOverflowFromOp: result = OF after the producing instruction.
pub fn emit_get_overflow_from_op(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    _inst: &Inst,
) {
    if ra.is_value_defined(inst_ref) {
        ra.register_pseudo_operation(inst_ref, &_inst.args, _inst.num_args());
        return;
    }
    let result = ra.scratch_gpr();
    let r8 = result.cvt8().unwrap();
    ra.asm.seto(r8).unwrap();
    let r32 = result.cvt32().unwrap();
    ra.asm.movzx(r32, r8).unwrap();
    ra.define_value(inst_ref, result);
}

/// GetNZCVFromOp: result = packed NZCV in x86-64 RFLAGS format.
///
/// Uses `lahf` to get SF/ZF/CF into AH, and `seto` to get OF.
/// Produces: AH[7]=SF(N), AH[6]=ZF(Z), AH[0]=CF(C), result_low=OF(V)
/// Then packs into the x64 NZCV format: bits 15,14,8,0.
pub fn emit_get_nzcv_from_op(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    _inst: &Inst,
) {
    if ra.is_value_defined(inst_ref) {
        ra.register_pseudo_operation(inst_ref, &_inst.args, _inst.num_args());
        return;
    }
    // We need RAX for lahf (writes AH)
    let rax = ra.scratch_gpr_at(HOST_RAX);
    let al = rax.cvt8().unwrap();
    // seto al — stores OF in AL
    ra.asm.seto(al).unwrap();
    // lahf — stores SF:ZF:0:AF:0:PF:1:CF into AH
    ra.asm.lahf().unwrap();
    // Now AX = AH:AL = (flags_byte : overflow_byte)
    // We want bits: 15=SF(N), 14=ZF(Z), 8=CF(C), 0=OF(V)
    // AH has SF at bit 7 (= bit 15 of AX), ZF at bit 6 (= bit 14 of AX),
    // CF at bit 0 (= bit 8 of AX), AL has OF at bit 0.
    // So EAX already has the format we want! Just mask it.
    let eax = rax.cvt32().unwrap();
    ra.asm.and_(eax, nzcv_util::X64_MASK as i32).unwrap();
    ra.define_value(inst_ref, rax);
}

/// GetNZFromOp: result = packed NZ in x86-64 RFLAGS format (N=bit15, Z=bit14).
///
/// Upstream does `test value, value; lahf; movzx eax, ah` which puts N at bit7
/// and Z at bit6. Then SetCpsrNZ stores this at cpsr_nzcv+1 (byte offset),
/// mapping bits 7:6 to dword bits 15:14.
///
/// rdynarmic's SetCpsrNZ works on the full dword with N at bit15, Z at bit14.
/// So we do `test; lahf` which places SF at AH bit7 = EAX bit15, ZF at AH bit6
/// = EAX bit14. We then mask to keep only bits 15:14.
pub fn emit_get_nz_from_op(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    if ra.is_value_defined(inst_ref) {
        ra.register_pseudo_operation(inst_ref, &inst.args, inst.num_args());
        return;
    }
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let rax = ra.scratch_gpr_at(HOST_RAX);
    let value = ra.use_gpr(&mut args[0]);
    let value32 = value.cvt32().unwrap();
    // test sets SF and ZF based on the value
    ra.asm.test(value32, value32).unwrap();
    // lahf puts SF:ZF:0:AF:0:PF:1:CF into AH (bits 15:14:...:8 of EAX)
    ra.asm.lahf().unwrap();
    let eax = rax.cvt32().unwrap();
    // Mask to N (bit 15) and Z (bit 14) only
    ra.asm
        .and_(
            eax,
            (nzcv_util::X64_N_FLAG_MASK | nzcv_util::X64_Z_FLAG_MASK) as i32,
        )
        .unwrap();
    ra.define_value(inst_ref, rax);
}

/// GetCFlagFromNZCV: extract carry flag from a packed NZCV value.
pub fn emit_get_c_flag_from_nzcv(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let nzcv = ra.use_scratch_gpr(&mut args[0]);
    let r32 = nzcv.cvt32().unwrap();
    ra.asm.shr(r32, nzcv_util::X64_C_FLAG_BIT as u8).unwrap();
    ra.asm.and_(r32, 1).unwrap();
    ra.define_value(inst_ref, nzcv);
}

/// NZCVFromPackedFlags: convert ARM-format NZCV (bits 31:28) to the x64
/// packed flag layout used internally (bits 15=N, 14=Z, 8=C, 0=V).
///
/// Upstream: `EmitX64::EmitNZCVFromPackedFlags` (emit_x64.cpp). For the
/// immediate path it builds the result bit-by-bit; for the register path it
/// uses `shr 28; imul TO_X64_MULTIPLIER; and X64_MASK`. Both produce a value
/// in the same format the rest of the JIT (lahf/seto pattern) expects, so
/// `ConditionalSelectNZCV` can compare/merge it with values produced by
/// `GetNZCVFromOp` without a format mismatch.
pub fn emit_nzcv_from_packed_flags(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    if args[0].is_immediate() {
        let arm_value = args[0].get_immediate_u32();
        let mut x64_value: u32 = 0;
        if arm_value & (1 << 31) != 0 {
            x64_value |= 1 << 15;
        } // N -> SF
        if arm_value & (1 << 30) != 0 {
            x64_value |= 1 << 14;
        } // Z -> ZF
        if arm_value & (1 << 29) != 0 {
            x64_value |= 1 << 8;
        } // C -> CF
        if arm_value & (1 << 28) != 0 {
            x64_value |= 1 << 0;
        } // V -> OF
        let result = ra.scratch_gpr();
        ra.asm
            .mov(result.cvt32().unwrap(), x64_value as i32)
            .unwrap();
        ra.define_value(inst_ref, result);
    } else if ctx.has_host_feature(HostFeature::FAST_BMI2) {
        let nzcv = ra.use_scratch_gpr(&mut args[0]);
        let nzcv32 = nzcv.cvt32().unwrap();
        let tmp = ra.scratch_gpr();
        let tmp32 = tmp.cvt32().unwrap();
        ra.asm.shr(nzcv32, 28).unwrap();
        ra.asm.mov(tmp32, nzcv_util::X64_MASK as i32).unwrap();
        ra.asm.pdep(nzcv32, nzcv32, tmp32).unwrap();
        ra.define_value(inst_ref, nzcv);
    } else {
        let nzcv = ra.use_scratch_gpr(&mut args[0]);
        let nzcv32 = nzcv.cvt32().unwrap();
        // ((nzcv >> 28) * TO_X64_MULTIPLIER) & X64_MASK
        ra.asm.shr(nzcv32, 28).unwrap();
        let tmp = ra.scratch_gpr();
        ra.asm
            .mov(tmp.cvt32().unwrap(), nzcv_util::TO_X64_MULTIPLIER as i32)
            .unwrap();
        ra.asm.imul(nzcv32, tmp.cvt32().unwrap()).unwrap();
        ra.asm.and_(nzcv32, nzcv_util::X64_MASK as i32).unwrap();
        ra.define_value(inst_ref, nzcv);
    }
}

// ---------------------------------------------------------------------------
// FPCR / FPSR
// ---------------------------------------------------------------------------

/// A64GetFPCR: result = jit_state.fpcr
pub fn emit_a64_get_fpcr(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, _inst: &Inst) {
    let offset = A64JitState::offset_of_fpcr();
    let result = ra.scratch_gpr();
    ra.asm
        .mov(
            result.cvt32().unwrap(),
            dword_ptr(RegExp::from(R15) + offset as i32),
        )
        .unwrap();
    ra.define_value(inst_ref, result);
}

/// A64SetFPCR: jit_state.fpcr = value (also updates guest MXCSR)
pub fn emit_a64_set_fpcr(_ctx: &EmitContext, ra: &mut RegAlloc, _inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());
    let value = ra.use_gpr(&mut args[0]);

    // Store the raw FPCR value
    let fpcr_offset = A64JitState::offset_of_fpcr();
    ra.asm
        .mov(
            dword_ptr(RegExp::from(R15) + fpcr_offset as i32),
            value.cvt32().unwrap(),
        )
        .unwrap();

    // TODO: Update guest_mxcsr based on FPCR rounding mode.
    // This requires calling A64JitState::set_fpcr() or emitting inline conversion.
    // For now, the interpreter path handles MXCSR updates.
}

/// A64GetFPSR: result = jit_state.fpsr (reconstructed from MXCSR exception bits)
pub fn emit_a64_get_fpsr(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, _inst: &Inst) {
    let offset = A64JitState::offset_of_fpsr_exc();
    let result = ra.scratch_gpr();
    ra.asm
        .mov(
            result.cvt32().unwrap(),
            dword_ptr(RegExp::from(R15) + offset as i32),
        )
        .unwrap();
    ra.define_value(inst_ref, result);
}

/// A64SetFPSR: jit_state.fpsr_exc = value
pub fn emit_a64_set_fpsr(_ctx: &EmitContext, ra: &mut RegAlloc, _inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());
    let value = ra.use_gpr(&mut args[0]);
    let offset = A64JitState::offset_of_fpsr_exc();
    ra.asm
        .mov(
            dword_ptr(RegExp::from(R15) + offset as i32),
            value.cvt32().unwrap(),
        )
        .unwrap();
}

// ---------------------------------------------------------------------------
// System registers
// ---------------------------------------------------------------------------

/// A64GetTPIDR: result = jit_state.tpidr_el0 (stored as fixed u64 field)
pub fn emit_a64_get_tpidr(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, _inst: &Inst) {
    let offset = A64JitState::offset_of_tpidr_el0();
    let result = ra.scratch_gpr();
    ra.asm
        .mov(result, qword_ptr(RegExp::from(R15) + offset as i32))
        .unwrap();
    ra.define_value(inst_ref, result);
}

/// A64SetTPIDR: jit_state.tpidr_el0 = value
pub fn emit_a64_set_tpidr(_ctx: &EmitContext, ra: &mut RegAlloc, _inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());
    let value = ra.use_gpr(&mut args[0]);
    let offset = A64JitState::offset_of_tpidr_el0();
    ra.asm
        .mov(qword_ptr(RegExp::from(R15) + offset as i32), value)
        .unwrap();
}

/// A64GetTPIDRRO: result = jit_state.tpidrro_el0
pub fn emit_a64_get_tpidrro(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    _inst: &Inst,
) {
    let offset = A64JitState::offset_of_tpidrro_el0();
    let result = ra.scratch_gpr();
    ra.asm
        .mov(result, qword_ptr(RegExp::from(R15) + offset as i32))
        .unwrap();
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// System operations (SVC, exceptions, barriers)
// ---------------------------------------------------------------------------

/// A64CallSupervisor: store PC, set halt_reason, return to dispatch.
///
/// args[0] = immediate u32 (SVC number)
pub fn emit_a64_call_supervisor(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut no_args: [Option<&mut Argument>; 0] = [];
    ra.host_call(None, &mut no_args);

    let args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let svc_num = args[0].value.get_imm_as_u64() as u32;

    // Call the supervisor callback
    ctx.config
        .callbacks
        .call_supervisor
        .emit_call(&mut *ra.asm, &|code, params| {
            code.mov(params[0], svc_num as i64)
        })
        .unwrap();

    // Mirror upstream `A64EmitX64::EmitA64CallSupervisor` (a64_emit_x64.cpp:488):
    // "The kernel would have to execute ERET to get here, which would clear
    // exclusive state." Without this, an LDAXR-then-SVC-context-switch can
    // leave `exclusive_state=1` set in the JIT state for the next thread that
    // gets scheduled on this core, leading to spurious STLXR success in the
    // new thread (host hardware CAS still atomic, but `expected_value` from
    // a different thread's LDAXR can satisfy the CAS by accident).
    let exclusive_state_offset = A64JitState::offset_of_exclusive_state();
    ra.asm
        .mov(
            rxbyak::byte_ptr(RegExp::from(R15) + exclusive_state_offset as i32),
            0i32,
        )
        .unwrap();
}

/// A64ExceptionRaised: call the host exception callback.
///
/// args[0] = pc (ImmU64), args[1] = exception code (ImmU64)
pub fn emit_a64_exception_raised(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut no_args: [Option<&mut Argument>; 0] = [];
    ra.host_call(None, &mut no_args);

    let args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let pc_val = args[0].value.get_imm_as_u64();
    let exc_val = args[1].value.get_imm_as_u64();
    ctx.config
        .callbacks
        .exception_raised
        .emit_call(&mut *ra.asm, &|code, params| {
            code.mov(params[0], pc_val as i64)?;
            code.mov(params[1], exc_val as i64)
        })
        .unwrap();
}

/// A64DataCacheOperationRaised: signal data cache maintenance.
pub fn emit_a64_data_cache_operation_raised(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    _inst_ref: InstRef,
    _inst: &Inst,
) {
    ctx.config
        .callbacks
        .data_cache_operation
        .emit_call_simple(&mut *ra.asm)
        .unwrap();
}

/// A64InstructionCacheOperationRaised: signal instruction cache maintenance.
pub fn emit_a64_instruction_cache_operation_raised(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    _inst_ref: InstRef,
    _inst: &Inst,
) {
    ctx.config
        .callbacks
        .instruction_cache_operation
        .emit_call_simple(&mut *ra.asm)
        .unwrap();
}

/// A64DataSynchronizationBarrier / A64DataMemoryBarrier / A64InstructionSynchronizationBarrier:
/// On x86-64 these are handled by mfence/lfence or are no-ops.
pub fn emit_a64_dsb(_ctx: &EmitContext, ra: &mut RegAlloc, _inst_ref: InstRef, _inst: &Inst) {
    ra.asm.mfence().unwrap();
}

pub fn emit_a64_dmb(_ctx: &EmitContext, ra: &mut RegAlloc, _inst_ref: InstRef, _inst: &Inst) {
    ra.asm.mfence().unwrap();
}

pub fn emit_a64_isb(ctx: &EmitContext, ra: &mut RegAlloc, _inst_ref: InstRef, _inst: &Inst) {
    if !ctx.config.memory.hook_isb {
        return;
    }
    ctx.config
        .callbacks
        .instruction_synchronization_barrier
        .emit_call_simple(&mut *ra.asm)
        .unwrap();
}

// ---------------------------------------------------------------------------
// Read-only system registers
// ---------------------------------------------------------------------------

/// A64GetCNTFRQ / A64GetCNTPCT / A64GetCTR / A64GetDCZID:
/// These return constants or call host callbacks. For now, return 0 placeholders.
pub fn emit_a64_get_cntfrq(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, _inst: &Inst) {
    let result = ra.scratch_gpr();
    // Upstream A64::UserConfig::cntfrq_el0, forwarded from the emulator.
    ra.asm
        .mov(result.cvt32().unwrap(), ctx.config.cntfrq_el0 as i32)
        .unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_a64_get_cntpct(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, _inst: &Inst) {
    // Call host callback to get CNTPCT_EL0 value.
    // Upstream: calls UserCallbacks::GetCNTPCT() which returns CoreTiming::GetClockTicks().
    ra.host_call(Some(inst_ref), &mut [None, None, None, None]);
    ctx.config
        .callbacks
        .get_cntpct
        .emit_call_simple(&mut *ra.asm)
        .unwrap();
}

pub fn emit_a64_get_ctr(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, _inst: &Inst) {
    let result = ra.scratch_gpr();
    // CTR_EL0: typical value with 64-byte cache lines
    // IminLine=4 (16 words=64 bytes), DminLine=4 (16 words=64 bytes)
    ra.asm
        .mov(result.cvt32().unwrap(), 0x8444_C004u32 as i32)
        .unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_a64_get_dczid(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, _inst: &Inst) {
    let result = ra.scratch_gpr();
    // DCZID_EL0: DZP=0 (DC ZVA permitted), BS=4 (64 bytes)
    ra.asm.mov(result.cvt32().unwrap(), 4i32).unwrap();
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// RSB (Return Stack Buffer)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Breakpoint
// ---------------------------------------------------------------------------

/// Breakpoint: emit int3.
pub fn emit_breakpoint(_ctx: &EmitContext, ra: &mut RegAlloc, _inst_ref: InstRef, _inst: &Inst) {
    ra.asm.int3().unwrap();
}

/// Void: no-op.
pub fn emit_void(_ctx: &EmitContext, _ra: &mut RegAlloc, _inst_ref: InstRef, _inst: &Inst) {
    // Nothing to do
}

/// Identity: forward value to result (copy elision).
pub fn emit_identity(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    ra.define_value_from_arg(inst_ref, &args[0]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxbyak::CodeAssembler;

    fn make_inst_info(count: usize) -> Vec<(u32, usize)> {
        vec![(1, 64); count]
    }

    #[test]
    fn test_emit_a64_get_x_generates_code() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let inst_info = make_inst_info(2);
        let mut ra = RegAlloc::new_default(&mut asm, inst_info);

        let inst_ref = InstRef(0);

        let start = ra.asm.size();
        // Can't call full emit since we need EmitContext, but verify RegAlloc API works
        let result = ra.scratch_gpr();
        let offset = A64JitState::reg_offset(0);
        ra.asm
            .mov(result, qword_ptr(RegExp::from(R15) + offset as i32))
            .unwrap();
        ra.define_value(inst_ref, result);
        ra.end_of_alloc_scope();

        assert!(
            ra.asm.size() > start,
            "Should have emitted code for A64GetX"
        );
    }

    #[test]
    fn test_jit_state_offsets_are_valid() {
        // Verify key offsets are reasonable
        assert!(A64JitState::reg_offset(0) < 500);
        assert!(A64JitState::reg_offset(30) < 500);
        assert!(A64JitState::offset_of_sp() < 500);
        assert!(A64JitState::offset_of_pc() < 500);
        assert!(A64JitState::offset_of_cpsr_nzcv() < 500);
        assert!(A64JitState::vec_offset(0, 0) > 0);
        assert!(A64JitState::vec_offset(31, 0) < 2000);
    }
}
