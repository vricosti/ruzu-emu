//! A32-specific IR opcode emit functions.
//!
//! These emit x86-64 code for the ~60 A32-prefixed IR opcodes.
//! They access `A32JitState` via R15 + offset (same convention as A64 emitters).

use rxbyak::{byte_ptr, dword_ptr, qword_ptr, xmmword_ptr};
use rxbyak::{RegExp, R15, RAX, RSP};

use crate::backend::x64::abi;
use crate::backend::x64::block_of_code::{
    emit_switch_mxcsr_on_entry, emit_switch_mxcsr_on_exit, STACK_LAYOUT_RSP_OFFSET,
};
use crate::backend::x64::callback::{Callback as X64Callback, SimpleCallback};
use crate::backend::x64::emit_context::EmitContext;
use crate::backend::x64::host_feature::HostFeature;
use crate::backend::x64::a32_jitstate::A32JitState;
use crate::backend::x64::nzcv_util;
use crate::backend::x64::reg_alloc::{Argument, RegAlloc};
use crate::backend::x64::stack_layout::StackLayout;
use crate::ir::inst::Inst;
use crate::ir::value::InstRef;
use crate::interface::a32::coprocessor::{
    Callback as CoprocessorCallback, CallbackOrAccessOneWord, CallbackOrAccessTwoWords,
};
use crate::interface::a32::coprocessor_util::CoprocReg;

// ---------------------------------------------------------------------------
// Conditional block prelude
// ---------------------------------------------------------------------------

/// Emit a condition check at the start of a conditional block.
///
/// Matches upstream dynarmic `A32EmitX64::EmitCondPrelude()`.
///
/// Uses raw assembly (not the register allocator) because this runs before
/// the block body emission starts. The upstream does the same — EmitCondPrelude
/// uses xbyak directly, not through register allocation.
///
/// If the block has a condition (set by `is_condition_passed` when the first
/// instruction in the block is conditional), we emit:
///   1. Load NZCV from jit_state into RAX
///   2. Set x86 flags from the ARM NZCV
///   3. If condition passes: jump past the fail path
///   4. If condition fails: subtract cycles, jump to condition_failed_location
pub fn emit_cond_prelude(ctx: &EmitContext, ra: &mut RegAlloc, block: &crate::ir::block::Block) {
    use crate::backend::x64::emit_terminal;
    use crate::ir::cond::Cond;
    use crate::ir::terminal::Terminal;

    let cond = match block.cond {
        Some(c) => c,
        None => return, // Unconditional — no prelude needed.
    };

    if cond == Cond::AL {
        return;
    }

    // Conditional block must have a fail location.
    let cfl = match block.condition_failed_location {
        Some(loc) => loc,
        None => return, // Defensive — shouldn't happen.
    };

    // Step 1: Load NZCV from jit_state into EAX using raw assembly.
    // This is the same as load_nzcv_into_flags but without going through
    // the register allocator's scratch_gpr_at method.
    let nzcv_offset = ctx.jit_state_info.offsetof_cpsr_nzcv;
    ra.asm
        .mov(
            rxbyak::EAX,
            rxbyak::dword_ptr(rxbyak::RegExp::from(rxbyak::R15) + nzcv_offset as i32),
        )
        .unwrap();

    // Step 2: Set x86 flags from the ARM NZCV value in AL/AH.
    // Match the same flag restoration logic as load_nzcv_into_flags.
    match cond {
        Cond::EQ | Cond::NE | Cond::CS | Cond::CC | Cond::MI | Cond::PL => {
            ra.asm.sahf().unwrap();
        }
        Cond::VS | Cond::VC => {
            ra.asm.cmp(rxbyak::AL, 0x81u32 as i32).unwrap();
        }
        Cond::HI | Cond::LS => {
            ra.asm.sahf().unwrap();
            ra.asm.cmc().unwrap();
        }
        Cond::GE | Cond::LT | Cond::GT | Cond::LE => {
            ra.asm.cmp(rxbyak::AL, 0x81u32 as i32).unwrap();
            ra.asm.sahf().unwrap();
        }
        Cond::AL | Cond::NV => {}
    }

    // Step 3: Conditional jump — if condition passes, skip the fail path.
    let pass_label = ra.asm.create_label();
    let t = rxbyak::JmpType::Near;
    match cond {
        Cond::EQ => ra.asm.jz(&pass_label, t),
        Cond::NE => ra.asm.jnz(&pass_label, t),
        Cond::CS => ra.asm.jc(&pass_label, t),
        Cond::CC => ra.asm.jnc(&pass_label, t),
        Cond::MI => ra.asm.js(&pass_label, t),
        Cond::PL => ra.asm.jns(&pass_label, t),
        Cond::VS => ra.asm.jo(&pass_label, t),
        Cond::VC => ra.asm.jno(&pass_label, t),
        Cond::HI => ra.asm.ja(&pass_label, t),
        Cond::LS => ra.asm.jbe(&pass_label, t),
        Cond::GE => ra.asm.jge(&pass_label, t),
        Cond::LT => ra.asm.jl(&pass_label, t),
        Cond::GT => ra.asm.jg(&pass_label, t),
        Cond::LE => ra.asm.jle(&pass_label, t),
        Cond::AL | Cond::NV => ra.asm.jmp(&pass_label, t),
    }
    .unwrap();

    // Step 4: Condition failed path.
    // Subtract cycles for the failed path if cycle counting is enabled.
    if ctx.config.enable_cycle_counting {
        let cycles_offset = STACK_LAYOUT_RSP_OFFSET
            + crate::backend::x64::stack_layout::StackLayout::cycles_remaining_offset();
        let fail_cycles = block.condition_failed_cycle_count as i32;
        ra.asm
            .sub(
                rxbyak::qword_ptr(rxbyak::RegExp::from(rxbyak::RSP) + cycles_offset as i32),
                fail_cycles,
            )
            .unwrap();
    }

    // Jump to condition_failed_location via LinkBlock terminal.
    // emit_terminal uses ra.asm directly for code emission (set PC, jump to
    // dispatcher), which is safe before the allocator has started tracking
    // registers for the block body.
    let cfl_term = Terminal::LinkBlock { next: cfl };
    emit_terminal::emit_terminal(ctx, ra, &cfl_term);

    // Step 5: Condition passed — block body starts here.
    ra.asm.bind(&pass_label).unwrap();
}

// ---------------------------------------------------------------------------
// GPR access
// ---------------------------------------------------------------------------

/// A32GetRegister: result = (u32) jit_state.reg[n], zero-extended to 64
pub fn emit_a32_get_register(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let reg_index = inst.args[0].get_a32_reg().number();
    let offset = A32JitState::reg_offset(reg_index);

    let result = ra.scratch_gpr();
    let r32 = result.cvt32().unwrap();
    ra.asm
        .mov(r32, dword_ptr(RegExp::from(R15) + offset as i32))
        .unwrap();
    ra.define_value(inst_ref, result);
}

/// A32SetRegister: jit_state.reg[n] = value32
pub fn emit_a32_set_register(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    _inst_ref: InstRef,
    inst: &Inst,
) {
    let reg_index = inst.args[0].get_a32_reg().number();
    let offset = A32JitState::reg_offset(reg_index);
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());

    if args[1].is_immediate() {
        let imm = args[1].get_immediate_u32();
        ra.asm
            .mov(dword_ptr(RegExp::from(R15) + offset as i32), imm as i32)
            .unwrap();
    } else {
        let source = ra.use_gpr(&mut args[1]);
        ra.asm
            .mov(
                dword_ptr(RegExp::from(R15) + offset as i32),
                source.cvt32().unwrap(),
            )
            .unwrap();
    }
}

// ---------------------------------------------------------------------------
// Extension register access (S/D/Q)
// ---------------------------------------------------------------------------

/// A32GetExtendedRegister32: result = (u32) ext_reg[backing_index]
pub fn emit_a32_get_extended_register32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let ext_reg = inst.args[0].get_a32_ext_reg();
    let backing = ext_reg.backing_index();
    let offset = A32JitState::ext_reg_offset(backing);

    let result = ra.scratch_xmm();
    ra.asm
        .movd(result, dword_ptr(RegExp::from(R15) + offset as i32))
        .unwrap();
    ra.define_value(inst_ref, result);
}

/// A32GetExtendedRegister64: result = (u64) ext_reg[backing_index..backing_index+1]
pub fn emit_a32_get_extended_register64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let ext_reg = inst.args[0].get_a32_ext_reg();
    let backing = ext_reg.backing_index();
    let offset = A32JitState::ext_reg_offset(backing);

    let result = ra.scratch_xmm();
    ra.asm
        .movq(result, qword_ptr(RegExp::from(R15) + offset as i32))
        .unwrap();
    ra.define_value(inst_ref, result);
}

/// A32SetExtendedRegister32: ext_reg[backing_index] = value32
pub fn emit_a32_set_extended_register32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    _inst_ref: InstRef,
    inst: &Inst,
) {
    let ext_reg = inst.args[0].get_a32_ext_reg();
    let backing = ext_reg.backing_index();
    let offset = A32JitState::ext_reg_offset(backing);
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());

    let source = ra.use_xmm(&mut args[1]);
    ra.asm
        .movd(dword_ptr(RegExp::from(R15) + offset as i32), source)
        .unwrap();
}

/// A32SetExtendedRegister64: ext_reg[backing_index..+1] = value64
pub fn emit_a32_set_extended_register64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    _inst_ref: InstRef,
    inst: &Inst,
) {
    let ext_reg = inst.args[0].get_a32_ext_reg();
    let backing = ext_reg.backing_index();
    let offset = A32JitState::ext_reg_offset(backing);
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());

    let source = ra.use_xmm(&mut args[1]);
    ra.asm
        .movq(qword_ptr(RegExp::from(R15) + offset as i32), source)
        .unwrap();
}

/// A32GetVector: result = (u128) ext_reg[backing_index..+3]
pub fn emit_a32_get_vector(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let ext_reg = inst.args[0].get_a32_ext_reg();
    let backing = ext_reg.backing_index();
    let offset = A32JitState::ext_reg_offset(backing);

    let result = ra.scratch_xmm();
    if ext_reg.is_double() {
        ra.asm
            .movsd(result, qword_ptr(RegExp::from(R15) + offset as i32))
            .unwrap();
    } else {
        ra.asm
            .movaps(result, xmmword_ptr(RegExp::from(R15) + offset as i32))
            .unwrap();
    }
    ra.define_value(inst_ref, result);
}

/// A32SetVector: ext_reg[backing_index..+3] = value128
pub fn emit_a32_set_vector(_ctx: &EmitContext, ra: &mut RegAlloc, _inst_ref: InstRef, inst: &Inst) {
    let ext_reg = inst.args[0].get_a32_ext_reg();
    let backing = ext_reg.backing_index();
    let offset = A32JitState::ext_reg_offset(backing);
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());

    let source = ra.use_xmm(&mut args[1]);
    if ext_reg.is_double() {
        ra.asm
            .movsd(qword_ptr(RegExp::from(R15) + offset as i32), source)
            .unwrap();
    } else {
        ra.asm
            .movaps(xmmword_ptr(RegExp::from(R15) + offset as i32), source)
            .unwrap();
    }
}

// ---------------------------------------------------------------------------
// CPSR / NZCV flags
// ---------------------------------------------------------------------------

/// A32GetCpsr: compose the architectural CPSR from the split JIT-state fields.
pub fn emit_a32_get_cpsr(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, _inst: &Inst) {
    let result = ra.scratch_gpr();
    let result32 = result.cvt32().unwrap();
    let tmp = ra.scratch_gpr();
    let tmp32 = tmp.cvt32().unwrap();
    let tmp2 = ra.scratch_gpr();
    let tmp232 = tmp2.cvt32().unwrap();
    let upper_offset = A32JitState::offset_of_upper_location_descriptor();
    let ge_offset = A32JitState::offset_of_cpsr_ge();

    if ctx.has_host_feature(HostFeature::FAST_BMI2) {
        debug_assert_eq!(upper_offset + 4, ge_offset);
        ra.asm
            .mov(result, qword_ptr(RegExp::from(R15) + upper_offset as i32))
            .unwrap();
        ra.asm.mov(tmp, 0x8080_8080_0000_0003u64 as i64).unwrap();
        ra.asm.pext(result, result, tmp).unwrap();
        ra.asm.mov(tmp32, 0x000f_0220).unwrap();
        ra.asm.pdep(result32, result32, tmp32).unwrap();
    } else {
        ra.asm
            .mov(result32, dword_ptr(RegExp::from(R15) + upper_offset as i32))
            .unwrap();
        ra.asm.mov(tmp32, 0x120).unwrap();
        ra.asm.imul(result32, tmp32).unwrap();
        ra.asm.and_(result32, 0x0000_0220).unwrap();

        ra.asm
            .mov(tmp32, dword_ptr(RegExp::from(R15) + ge_offset as i32))
            .unwrap();
        ra.asm.and_(tmp32, 0x8080_8080u32).unwrap();
        ra.asm.mov(tmp232, 0x0020_4081).unwrap();
        ra.asm.imul(tmp32, tmp232).unwrap();
        ra.asm.shr(tmp32, 12).unwrap();
        ra.asm.and_(tmp32, 0x000f_0000).unwrap();
        ra.asm.or_(result32, tmp32).unwrap();
    }

    ra.asm
        .mov(
            tmp32,
            dword_ptr(RegExp::from(R15) + A32JitState::offset_of_cpsr_q() as i32),
        )
        .unwrap();
    ra.asm.shl(tmp32, 27).unwrap();
    ra.asm.or_(result32, tmp32).unwrap();

    ra.asm
        .mov(
            tmp232,
            dword_ptr(RegExp::from(R15) + A32JitState::offset_of_cpsr_nzcv() as i32),
        )
        .unwrap();
    if ctx.has_host_feature(HostFeature::FAST_BMI2) {
        ra.asm.mov(tmp32, nzcv_util::X64_MASK as i32).unwrap();
        ra.asm.pext(tmp232, tmp232, tmp32).unwrap();
        ra.asm.shl(tmp232, 28).unwrap();
    } else {
        ra.asm.and_(tmp232, nzcv_util::X64_MASK as i32).unwrap();
        ra.asm
            .mov(tmp32, nzcv_util::FROM_X64_MULTIPLIER as i32)
            .unwrap();
        ra.asm.imul(tmp232, tmp32).unwrap();
        ra.asm.and_(tmp232, nzcv_util::ARM_MASK as i32).unwrap();
    }
    ra.asm.or_(result32, tmp232).unwrap();
    ra.asm
        .or_(
            result32,
            dword_ptr(RegExp::from(R15) + A32JitState::offset_of_cpsr_jaifm() as i32),
        )
        .unwrap();
    ra.define_value(inst_ref, result);
}

/// A32SetCpsr: decompose full CPSR into split JIT state fields.
pub fn emit_a32_set_cpsr(ctx: &EmitContext, ra: &mut RegAlloc, _inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());
    let cpsr = ra.use_scratch_gpr(&mut args[0]);
    let cpsr32 = cpsr.cvt32().unwrap();
    let tmp = ra.scratch_gpr();
    let tmp32 = tmp.cvt32().unwrap();
    let tmp2 = ra.scratch_gpr();
    let tmp232 = tmp2.cvt32().unwrap();

    let cpsr_q_offset = A32JitState::offset_of_cpsr_q();
    let cpsr_nzcv_offset = A32JitState::offset_of_cpsr_nzcv();
    let cpsr_jaifm_offset = A32JitState::offset_of_cpsr_jaifm();
    let upper_offset = A32JitState::offset_of_upper_location_descriptor();
    let ge_offset = A32JitState::offset_of_cpsr_ge();

    // Switch/Horizon is always little-endian. Match upstream's
    // conf.always_little_endian path by clearing CPSR.E before decomposition.
    ra.asm.and_(cpsr32, 0xFFFF_FDFFu32 as i32).unwrap();

    // cpsr_q: bit 27
    ra.asm.bt_imm(cpsr32, 27).unwrap();
    ra.asm
        .setc(byte_ptr(RegExp::from(R15) + cpsr_q_offset as i32))
        .unwrap();

    // cpsr_nzcv
    ra.asm.mov(tmp32, cpsr32).unwrap();
    ra.asm.shr(tmp32, 28).unwrap();
    if ctx.has_host_feature(HostFeature::FAST_BMI2) {
        ra.asm.mov(tmp232, nzcv_util::X64_MASK as i32).unwrap();
        ra.asm.pdep(tmp32, tmp32, tmp232).unwrap();
    } else {
        ra.asm
            .mov(tmp232, nzcv_util::TO_X64_MULTIPLIER as i32)
            .unwrap();
        ra.asm.imul(tmp32, tmp232).unwrap();
        ra.asm.and_(tmp32, nzcv_util::X64_MASK as i32).unwrap();
    }
    ra.asm
        .mov(
            dword_ptr(RegExp::from(R15) + cpsr_nzcv_offset as i32),
            tmp32,
        )
        .unwrap();

    // cpsr_jaifm
    ra.asm.mov(tmp32, cpsr32).unwrap();
    ra.asm.and_(tmp32, 0x0100_01DFu32 as i32).unwrap();
    ra.asm
        .mov(
            dword_ptr(RegExp::from(R15) + cpsr_jaifm_offset as i32),
            tmp32,
        )
        .unwrap();

    if ctx.has_host_feature(HostFeature::FAST_BMI2) {
        debug_assert_eq!(upper_offset + 4, ge_offset);
        ra.asm
            .and_(
                qword_ptr(RegExp::from(R15) + upper_offset as i32),
                0x7fff_0000u32,
            )
            .unwrap();
        ra.asm.mov(tmp32, 0x000f_0220).unwrap();
        ra.asm.pext(cpsr32, cpsr32, tmp32).unwrap();
        ra.asm.mov(tmp, 0x0101_0101_0000_0003u64 as i64).unwrap();
        ra.asm.pdep(cpsr, cpsr, tmp).unwrap();
        ra.asm.mov(tmp, 0x8080_8080_0000_0003u64 as i64).unwrap();
        ra.asm.mov(tmp2, tmp).unwrap();
        ra.asm.sub(tmp, cpsr).unwrap();
        ra.asm.xor_(tmp, tmp2).unwrap();
        ra.asm
            .or_(qword_ptr(RegExp::from(R15) + upper_offset as i32), tmp)
            .unwrap();
    } else {
        // upper_location_descriptor: keep FPSCR mode bits, replace E/T/IT bits
        ra.asm
            .and_(
                dword_ptr(RegExp::from(R15) + upper_offset as i32),
                0xFFFF_0000u32,
            )
            .unwrap();
        ra.asm.mov(tmp32, cpsr32).unwrap();
        ra.asm.and_(tmp32, 0x0000_0220u32).unwrap();
        ra.asm.mov(tmp232, 0x0090_0000u32).unwrap();
        ra.asm.imul(tmp32, tmp232).unwrap();
        ra.asm.shr(tmp32, 28).unwrap();
        ra.asm
            .or_(dword_ptr(RegExp::from(R15) + upper_offset as i32), tmp32)
            .unwrap();

        // cpsr_ge: expand CPSR GE[3:0] bits into byte lanes
        ra.asm.and_(cpsr32, 0x000F_0000u32).unwrap();
        ra.asm.shr(cpsr32, 16).unwrap();
        ra.asm.mov(tmp232, 0x0020_4081u32).unwrap();
        ra.asm.imul(cpsr32, tmp232).unwrap();
        ra.asm.and_(cpsr32, 0x0101_0101u32).unwrap();
        ra.asm.mov(tmp32, 0x8080_8080u32).unwrap();
        ra.asm.sub(tmp32, cpsr32).unwrap();
        ra.asm.xor_(tmp32, 0x8080_8080u32).unwrap();
        ra.asm
            .mov(dword_ptr(RegExp::from(R15) + ge_offset as i32), tmp32)
            .unwrap();
    }
}

/// A32SetCpsrNZCVRaw: cpsr_nzcv = nzcv_to_x64(value) (ARM format input)
pub fn emit_a32_set_cpsr_nzcv_raw(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    _inst_ref: InstRef,
    inst: &Inst,
) {
    let offset = A32JitState::offset_of_cpsr_nzcv();
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());

    if args[0].is_immediate() {
        let imm = args[0].get_immediate_u32();
        let x64 = nzcv_util::to_x64(imm);
        ra.asm
            .mov(dword_ptr(RegExp::from(R15) + offset as i32), x64 as i32)
            .unwrap();
        return;
    }

    let source = ra.use_scratch_gpr(&mut args[0]);
    let source32 = source.cvt32().unwrap();
    let tmp = ra.scratch_gpr();
    let tmp32 = tmp.cvt32().unwrap();
    ra.asm.shr(source32, 28).unwrap();
    if ctx.has_host_feature(HostFeature::FAST_BMI2) {
        ra.asm.mov(tmp32, nzcv_util::X64_MASK as i32).unwrap();
        ra.asm.pdep(source32, source32, tmp32).unwrap();
    } else {
        ra.asm
            .mov(tmp32, nzcv_util::TO_X64_MULTIPLIER as i32)
            .unwrap();
        ra.asm.imul(source32, tmp32).unwrap();
        ra.asm.and_(source32, nzcv_util::X64_MASK as i32).unwrap();
    }
    ra.asm
        .mov(dword_ptr(RegExp::from(R15) + offset as i32), source32)
        .unwrap();
}

/// A32SetCpsrNZCV: cpsr_nzcv = value (already in x64 NZCV format)
pub fn emit_a32_set_cpsr_nzcv(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    _inst_ref: InstRef,
    inst: &Inst,
) {
    let offset = A32JitState::offset_of_cpsr_nzcv();
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());
    let nzcv = ra.use_gpr(&mut args[0]);
    ra.asm
        .mov(
            dword_ptr(RegExp::from(R15) + offset as i32),
            nzcv.cvt32().unwrap(),
        )
        .unwrap();
}

/// A32SetCpsrNZCVQ: set NZCV and Q from a single ARM-format value
pub fn emit_a32_set_cpsr_nzcvq(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    _inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());
    let nzcv_offset = A32JitState::offset_of_cpsr_nzcv();
    let q_offset = A32JitState::offset_of_cpsr_q();

    if args[0].is_immediate() {
        let imm = args[0].get_immediate_u32();
        let x64 = nzcv_util::to_x64(imm);
        ra.asm
            .mov(
                dword_ptr(RegExp::from(R15) + nzcv_offset as i32),
                x64 as i32,
            )
            .unwrap();
        ra.asm
            .mov(
                byte_ptr(RegExp::from(R15) + q_offset as i32),
                if (imm & 0x0800_0000) != 0 { 1 } else { 0 },
            )
            .unwrap();
        return;
    }

    let value = ra.use_scratch_gpr(&mut args[0]);
    let value32 = value.cvt32().unwrap();
    let tmp = ra.scratch_gpr();
    let tmp32 = tmp.cvt32().unwrap();

    ra.asm.shr(value32, 28).unwrap();
    ra.asm
        .setc(byte_ptr(RegExp::from(R15) + q_offset as i32))
        .unwrap();
    if ctx.has_host_feature(HostFeature::FAST_BMI2) {
        ra.asm.mov(tmp32, nzcv_util::X64_MASK as i32).unwrap();
        ra.asm.pdep(value32, value32, tmp32).unwrap();
    } else {
        ra.asm
            .mov(tmp32, nzcv_util::TO_X64_MULTIPLIER as i32)
            .unwrap();
        ra.asm.imul(value32, tmp32).unwrap();
        ra.asm.and_(value32, nzcv_util::X64_MASK as i32).unwrap();
    }
    ra.asm
        .mov(dword_ptr(RegExp::from(R15) + nzcv_offset as i32), value32)
        .unwrap();
}

/// A32SetCpsrNZ: set only N and Z flags (from x86 format packed value)
pub fn emit_a32_set_cpsr_nz(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    _inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());
    let nz = ra.use_scratch_gpr(&mut args[0]);
    let nz32 = nz.cvt32().unwrap();

    // Mask to keep only N and Z bits, preserve C and V from current state
    let offset = A32JitState::offset_of_cpsr_nzcv();
    let tmp = ra.scratch_gpr();
    ra.asm
        .mov(
            tmp.cvt32().unwrap(),
            dword_ptr(RegExp::from(R15) + offset as i32),
        )
        .unwrap();
    // Clear N,Z bits in current, keep C,V
    ra.asm
        .and_(
            tmp.cvt32().unwrap(),
            (nzcv_util::X64_C_FLAG_MASK | nzcv_util::X64_V_FLAG_MASK) as i32,
        )
        .unwrap();
    // Mask new value to N,Z only
    ra.asm
        .and_(
            nz32,
            (nzcv_util::X64_N_FLAG_MASK | nzcv_util::X64_Z_FLAG_MASK) as i32,
        )
        .unwrap();
    ra.asm.or_(nz32, tmp.cvt32().unwrap()).unwrap();
    ra.asm
        .mov(dword_ptr(RegExp::from(R15) + offset as i32), nz32)
        .unwrap();
}

/// A32SetCpsrNZC: set N, Z, and C flags.
///
/// Matches upstream `A32EmitX64::EmitA32SetCpsrNZC()` literally: this opcode
/// only updates the single byte containing N/Z/C in `cpsr_nzcv`.
///
/// `args[0]` may be `EmptyNZCVImmediateMarker` after GetSetElimination rewrites
/// dead NZ inputs. The immediate path must therefore be handled before trying to
/// materialize `args[0]` in a GPR.
pub fn emit_a32_set_cpsr_nzc(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    _inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());
    let offset = A32JitState::offset_of_cpsr_nzcv();
    let nzc_byte = byte_ptr(RegExp::from(R15) + offset as i32 + 1);

    if args[0].is_immediate() {
        if args[1].is_immediate() {
            let c = args[1].get_immediate_u1();
            ra.asm.mov(nzc_byte, if c { 1 } else { 0 }).unwrap();
        } else {
            let c = ra.use_gpr(&mut args[1]).cvt8().unwrap();
            ra.asm.mov(nzc_byte, c).unwrap();
        }
        return;
    }

    let nz = ra.use_scratch_gpr(&mut args[0]).cvt32().unwrap();
    // GetNZFromOp returns x64-packed NZ in bits 15:14 (AH from LAHF).
    // This opcode stores the compressed N/Z/C byte at cpsr_nzcv+1, so fold
    // those bits down into bits 7:6 before combining carry.
    ra.asm.shr(nz, 8).unwrap();
    if args[1].is_immediate() {
        let c = args[1].get_immediate_u1();
        ra.asm.or_(nz, if c { 1 } else { 0 }).unwrap();
        ra.asm.mov(nzc_byte, nz.cvt8().unwrap()).unwrap();
    } else {
        let c = ra.use_gpr(&mut args[1]).cvt32().unwrap();
        ra.asm.or_(nz, c).unwrap();
        ra.asm.mov(nzc_byte, nz.cvt8().unwrap()).unwrap();
    }
}

/// A32GetCFlag: result = (cpsr_nzcv >> C_FLAG_BIT) & 1
pub fn emit_a32_get_c_flag(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, _inst: &Inst) {
    let offset = A32JitState::offset_of_cpsr_nzcv();
    let result = ra.scratch_gpr();
    let r32 = result.cvt32().unwrap();
    ra.asm
        .mov(r32, dword_ptr(RegExp::from(R15) + offset as i32))
        .unwrap();
    ra.asm.shr(r32, nzcv_util::X64_C_FLAG_BIT as u8).unwrap();
    ra.asm.and_(r32, 1).unwrap();
    ra.define_value(inst_ref, result);
}

/// A32OrQFlag: cpsr_q |= value
pub fn emit_a32_or_q_flag(_ctx: &EmitContext, ra: &mut RegAlloc, _inst_ref: InstRef, inst: &Inst) {
    let offset = A32JitState::offset_of_cpsr_q();
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());
    if args[0].is_immediate() {
        if args[0].get_immediate_u1() {
            ra.asm
                .mov(dword_ptr(RegExp::from(R15) + offset as i32), 1)
                .unwrap();
        }
    } else {
        let value = ra.use_gpr(&mut args[0]);
        ra.asm
            .or_(
                byte_ptr(RegExp::from(R15) + offset as i32),
                value.cvt8().unwrap(),
            )
            .unwrap();
    }
}

/// A32SetCheckBit: stack_layout.check_bit = value & 1
pub fn emit_a32_set_check_bit(
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
        .mov(byte_ptr(RegExp::from(rxbyak::RSP) + offset as i32), src8)
        .unwrap();
}

// ---------------------------------------------------------------------------
// GE flags
// ---------------------------------------------------------------------------

/// A32GetGEFlags: result = cpsr_ge (u32, byte-lane format).
/// Matches upstream EmitA32GetGEFlags.
pub fn emit_a32_get_ge_flags(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    _inst: &Inst,
) {
    let offset = A32JitState::offset_of_cpsr_ge();
    let result = ra.scratch_gpr();
    let r32 = result.cvt32().unwrap();
    ra.asm
        .mov(r32, dword_ptr(RegExp::from(R15) + offset as i32))
        .unwrap();
    ra.define_value(inst_ref, result);
}

/// A32SetGEFlags: store GE flags (u32, byte-lane format).
/// Matches upstream EmitA32SetGEFlags.
pub fn emit_a32_set_ge_flags(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    _inst_ref: InstRef,
    inst: &Inst,
) {
    let offset = A32JitState::offset_of_cpsr_ge();
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());
    let source = ra.use_gpr(&mut args[0]);
    let s32 = source.cvt32().unwrap();
    ra.asm
        .mov(dword_ptr(RegExp::from(R15) + offset as i32), s32)
        .unwrap();
}

/// A32SetGEFlagsCompressed: expand compressed GE bits (19:16) to byte-lane format.
/// Each GE bit becomes a full byte (0x00 or 0xFF).
/// Matches upstream EmitA32SetGEFlagsCompressed.
pub fn emit_a32_set_ge_flags_compressed(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    _inst_ref: InstRef,
    inst: &Inst,
) {
    let offset = A32JitState::offset_of_cpsr_ge();
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());
    if args[0].is_immediate() {
        let imm = args[0].get_immediate_u32();
        let mut ge = 0u32;
        ge |= if imm & (1 << 19) != 0 { 0xff00_0000 } else { 0 };
        ge |= if imm & (1 << 18) != 0 { 0x00ff_0000 } else { 0 };
        ge |= if imm & (1 << 17) != 0 { 0x0000_ff00 } else { 0 };
        ge |= if imm & (1 << 16) != 0 { 0x0000_00ff } else { 0 };
        ra.asm
            .mov(dword_ptr(RegExp::from(R15) + offset as i32), ge)
            .unwrap();
        return;
    }

    let source = ra.use_scratch_gpr(&mut args[0]);
    let s32 = source.cvt32().unwrap();
    if ctx.has_host_feature(HostFeature::FAST_BMI2) {
        let mask = ra.scratch_gpr();
        let mask32 = mask.cvt32().unwrap();
        ra.asm.mov(mask32, 0x0101_0101).unwrap();
        ra.asm.shr(s32, 16).unwrap();
        ra.asm.pdep(s32, s32, mask32).unwrap();
        ra.asm.mov(mask32, 0xff).unwrap();
        ra.asm.imul(s32, mask32).unwrap();
    } else {
        let tmp = ra.scratch_gpr();
        let tmp32 = tmp.cvt32().unwrap();
        ra.asm.shr(s32, 16).unwrap();
        ra.asm.and_(s32, 0xf).unwrap();
        ra.asm.mov(tmp32, 0x0020_4081).unwrap();
        ra.asm.imul(s32, tmp32).unwrap();
        ra.asm.and_(s32, 0x0101_0101).unwrap();
        ra.asm.mov(tmp32, 0xff).unwrap();
        ra.asm.imul(s32, tmp32).unwrap();
    }
    ra.asm
        .mov(dword_ptr(RegExp::from(R15) + offset as i32), s32)
        .unwrap();
}

// ---------------------------------------------------------------------------
// FPSCR
// ---------------------------------------------------------------------------

/// Extern "C" trampoline for GetFpscr: called from JIT-generated code.
/// Matches upstream `GetFpscrImpl(A32JitState* jit_state) -> u32`.
unsafe extern "C" fn get_fpscr_impl(jit_state: *const A32JitState) -> u32 {
    (*jit_state).get_fpscr()
}

/// A32GetFpscr: call get_fpscr_impl to reconstruct full FPSCR.
///
/// Matches upstream EmitA32GetFpscr which calls GetFpscrImpl via host call.
/// Returns the combined FPSCR value including nzcv, mode bits, and exception flags.
pub fn emit_a32_get_fpscr(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, _inst: &Inst) {
    ra.host_call(Some(inst_ref), &mut []);
    ra.asm.mov(abi::ABI_PARAMS[0].to_reg64(), R15).unwrap();
    ra.asm
        .stmxcsr(dword_ptr(
            RegExp::from(R15) + A32JitState::offset_of_guest_mxcsr() as i32,
        ))
        .unwrap();
    let fn_ptr = get_fpscr_impl as usize;
    ra.asm.mov(RAX, fn_ptr as i64).unwrap();
    ra.asm.call_reg(RAX).unwrap();
}

/// Extern "C" trampoline for SetFpscr: called from JIT-generated code.
/// Matches upstream `SetFpscrImpl(u32 value, A32JitState* jit_state)`.
///
/// Updates all FPSCR shadow state AND upper_location_descriptor mode bits.
unsafe extern "C" fn set_fpscr_impl(value: u32, jit_state: *mut A32JitState) {
    (*jit_state).set_fpscr(value);
}

/// A32SetFpscr: call set_fpscr_impl to update full FPSCR state.
///
/// Matches upstream EmitA32SetFpscr which calls SetFpscrImpl via host call.
/// This updates fpsr_nzcv, fpsr_qc, fpsr_exc, guest_mxcsr, asimd_mxcsr,
/// AND upper_location_descriptor mode bits.
pub fn emit_a32_set_fpscr(_ctx: &EmitContext, ra: &mut RegAlloc, _inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());

    ra.host_call(None, &mut [Some(&mut args[0])]);
    ra.asm.mov(abi::ABI_PARAMS[1].to_reg64(), R15).unwrap();
    let fn_ptr = set_fpscr_impl as usize;
    ra.asm.mov(RAX, fn_ptr as i64).unwrap();
    ra.asm.call_reg(RAX).unwrap();
    ra.asm
        .ldmxcsr(dword_ptr(
            RegExp::from(R15) + A32JitState::offset_of_guest_mxcsr() as i32,
        ))
        .unwrap();
}

/// A32GetFpscrNZCV: result = fpsr_nzcv
pub fn emit_a32_get_fpscr_nzcv(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    _inst: &Inst,
) {
    let offset = A32JitState::offset_of_fpsr_nzcv();
    let result = ra.scratch_gpr();
    ra.asm
        .mov(
            result.cvt32().unwrap(),
            dword_ptr(RegExp::from(R15) + offset as i32),
        )
        .unwrap();
    ra.define_value(inst_ref, result);
}

/// A32SetFpscrNZCV: fpsr_nzcv = value
pub fn emit_a32_set_fpscr_nzcv(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    _inst_ref: InstRef,
    inst: &Inst,
) {
    let offset = A32JitState::offset_of_fpsr_nzcv();
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());
    if args[0].is_immediate() {
        let imm = args[0].get_immediate_u32();
        let arm = nzcv_util::from_x64(imm);
        ra.asm
            .mov(dword_ptr(RegExp::from(R15) + offset as i32), arm as i32)
            .unwrap();
        return;
    }

    if ctx.has_host_feature(HostFeature::FAST_BMI2) {
        let value = ra.use_gpr(&mut args[0]);
        let value32 = value.cvt32().unwrap();
        let tmp = ra.scratch_gpr();
        let tmp32 = tmp.cvt32().unwrap();
        ra.asm.mov(tmp32, nzcv_util::X64_MASK as i32).unwrap();
        ra.asm.pext(tmp32, value32, tmp32).unwrap();
        ra.asm.shl(tmp32, 28).unwrap();
        ra.asm
            .mov(dword_ptr(RegExp::from(R15) + offset as i32), tmp32)
            .unwrap();
    } else {
        let value = ra.use_scratch_gpr(&mut args[0]);
        let value32 = value.cvt32().unwrap();
        ra.asm.and_(value32, nzcv_util::X64_MASK as i32).unwrap();
        let tmp = ra.scratch_gpr();
        ra.asm
            .mov(tmp.cvt32().unwrap(), nzcv_util::FROM_X64_MULTIPLIER as i32)
            .unwrap();
        ra.asm.imul(value32, tmp.cvt32().unwrap()).unwrap();
        ra.asm.and_(value32, nzcv_util::ARM_MASK as i32).unwrap();
        ra.asm
            .mov(dword_ptr(RegExp::from(R15) + offset as i32), value32)
            .unwrap();
    }
}

// ---------------------------------------------------------------------------
// Special: BXWritePC, upper location descriptor, supervisor, exceptions
// ---------------------------------------------------------------------------

/// A32BXWritePC: interworking branch — write PC and update T flag in upper_location_descriptor.
///
/// Matches dynarmic EmitA32BXWritePC:
///   if (new_pc & 1) { new_pc &= 0xFFFFFFFE; cpsr.T = 1; }
///   else            { new_pc &= 0xFFFFFFFC; cpsr.T = 0; }
/// Also writes upper_location_descriptor so the dispatcher picks up the correct T flag.
fn a32_bx_upper_without_t(ctx: &EmitContext) -> u32 {
    // Matches dynarmic:
    //   (ctx.EndLocation().SetSingleStepping(false).UniqueHash() >> 32) & 0xFFFFFFFE
    // The single-stepping bit has already been normalized in end_location by the
    // emitter context; BXWritePC only clears the T bit before recomputing it.
    let upper_source = ctx.end_location.unwrap_or(ctx.location);
    ctx.arch.extract_upper_location_descriptor(upper_source) & !1u32
}

pub fn emit_a32_bx_write_pc(ctx: &EmitContext, ra: &mut RegAlloc, _inst_ref: InstRef, inst: &Inst) {
    let pc_offset = A32JitState::reg_offset(15);
    let upper_offset = A32JitState::offset_of_upper_location_descriptor();
    let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());
    let upper_without_t = a32_bx_upper_without_t(ctx);

    if args[0].is_immediate() {
        let new_pc = args[0].get_immediate_u32();
        let mask: u32 = if new_pc & 1 != 0 {
            0xFFFF_FFFE
        } else {
            0xFFFF_FFFC
        };
        let new_upper = upper_without_t | (new_pc & 1);

        ra.asm
            .mov(
                dword_ptr(RegExp::from(R15) + pc_offset as i32),
                (new_pc & mask) as i32,
            )
            .unwrap();
        ra.asm
            .mov(
                dword_ptr(RegExp::from(R15) + upper_offset as i32),
                new_upper as i32,
            )
            .unwrap();
    } else {
        let new_pc = ra.use_scratch_gpr(&mut args[0]);
        let new_pc32 = new_pc.cvt32().unwrap();
        let mask = ra.scratch_gpr();
        let mask32 = mask.cvt32().unwrap();
        let new_upper = ra.scratch_gpr();
        let new_upper32 = new_upper.cvt32().unwrap();

        // mask = new_pc & 1  (extract T flag from bit 0)
        ra.asm.mov(mask32, new_pc32).unwrap();
        ra.asm.and_(mask32, 1).unwrap();

        // new_upper = upper_without_t | bit0
        ra.asm.mov(new_upper32, upper_without_t as i32).unwrap();
        ra.asm.or_(new_upper32, mask32).unwrap();

        // Compute PC alignment mask:
        //   bit0=0 (ARM)   → mask = 0*2 - 4 = -4 = 0xFFFFFFFC
        //   bit0=1 (Thumb) → mask = 1*2 - 4 = -2 = 0xFFFFFFFE
        // Matches dynarmic: lea(mask, ptr[mask.cvt64() + mask.cvt64() * 1 - 4])
        ra.asm.shl(mask32, 1).unwrap();
        ra.asm.sub(mask32, 4).unwrap();

        // Apply alignment mask to PC and store both
        ra.asm.and_(new_pc32, mask32).unwrap();
        ra.asm
            .mov(dword_ptr(RegExp::from(R15) + pc_offset as i32), new_pc32)
            .unwrap();
        ra.asm
            .mov(
                dword_ptr(RegExp::from(R15) + upper_offset as i32),
                new_upper32,
            )
            .unwrap();
    }
}

/// A32UpdateUpperLocationDescriptor: update the upper descriptor for block lookup.
///
/// Matches upstream EmitA32UpdateUpperLocationDescriptor:
///   - If the block contains a BXWritePC, skip (BXWritePC already updates it)
///   - Otherwise, compute new upper from end_location vs start_location and
///     write if different.
///
/// This opcode takes no arguments.
pub fn emit_a32_update_upper_location_descriptor(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    _inst_ref: InstRef,
    _inst: &Inst,
) {
    // If the block contains BXWritePC, it already handles the upper descriptor.
    if ctx.has_bx_write_pc {
        return;
    }

    // Compute new upper from end_location, compare with start location.
    if let Some(end_loc) = ctx.end_location {
        let offset = A32JitState::offset_of_upper_location_descriptor();
        let new_upper = ctx.arch.extract_upper_location_descriptor(end_loc) & !4u32; // strip single_stepping
        let old_upper = ctx.arch.extract_upper_location_descriptor(ctx.location) & !4u32;
        if new_upper != old_upper {
            ra.asm
                .mov(
                    dword_ptr(RegExp::from(R15) + offset as i32),
                    new_upper as i32,
                )
                .unwrap();
        }
    }
}

/// A32CallSupervisor: call callback with SVC number.
pub fn emit_a32_call_supervisor(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_switch_mxcsr_on_exit(ra.asm, A32JitState::offset_of_guest_mxcsr()).unwrap();

    if ctx.config.enable_cycle_counting {
        let mut no_args: [Option<&mut Argument>; 0] = [];
        ra.host_call(None, &mut no_args);
        let cycles_to_run = STACK_LAYOUT_RSP_OFFSET + StackLayout::cycles_to_run_offset();
        let cycles_remaining = STACK_LAYOUT_RSP_OFFSET + StackLayout::cycles_remaining_offset();
        ctx.config
            .callbacks
            .add_ticks
            .emit_call(&mut *ra.asm, &|code, params| {
                code.mov(
                    params[0],
                    qword_ptr(RegExp::from(RSP) + cycles_to_run as i32),
                )?;
                code.sub(
                    params[0],
                    qword_ptr(RegExp::from(RSP) + cycles_remaining as i32),
                )
            })
            .unwrap();
        ra.end_of_alloc_scope();
    }

    let args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let svc_num = args[0].value.get_imm_as_u64() as u32;
    let mut no_args: [Option<&mut Argument>; 0] = [];
    ra.host_call(None, &mut no_args);
    ctx.config
        .callbacks
        .call_supervisor
        .emit_call(&mut *ra.asm, &|code, params| {
            code.mov(params[0], svc_num as i64)
        })
        .unwrap();

    if ctx.config.enable_cycle_counting {
        let cycles_to_run = STACK_LAYOUT_RSP_OFFSET + StackLayout::cycles_to_run_offset();
        let cycles_remaining = STACK_LAYOUT_RSP_OFFSET + StackLayout::cycles_remaining_offset();
        ctx.config
            .callbacks
            .get_ticks_remaining
            .emit_call_simple(&mut *ra.asm)
            .unwrap();
        ra.asm
            .mov(qword_ptr(RegExp::from(RSP) + cycles_to_run as i32), RAX)
            .unwrap();
        ra.asm
            .mov(qword_ptr(RegExp::from(RSP) + cycles_remaining as i32), RAX)
            .unwrap();
        emit_switch_mxcsr_on_entry(ra.asm, A32JitState::offset_of_guest_mxcsr()).unwrap();
    }
}

/// A32ExceptionRaised: call exception callback.
///
/// The host callback decides whether the exception should halt execution
/// (for example NoExecuteFault → PrefetchAbort in yuzu/ruzu). Do not pre-set
/// EXCEPTION_RAISED here, matching the A64 path and upstream Dynarmic.
pub fn emit_a32_exception_raised(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_switch_mxcsr_on_exit(ra.asm, A32JitState::offset_of_guest_mxcsr()).unwrap();

    let mut no_args: [Option<&mut Argument>; 0] = [];
    ra.host_call(None, &mut no_args);

    if ctx.config.enable_cycle_counting {
        let cycles_to_run = STACK_LAYOUT_RSP_OFFSET + StackLayout::cycles_to_run_offset();
        let cycles_remaining = STACK_LAYOUT_RSP_OFFSET + StackLayout::cycles_remaining_offset();
        ctx.config
            .callbacks
            .add_ticks
            .emit_call(&mut *ra.asm, &|code, params| {
                code.mov(
                    params[0],
                    qword_ptr(RegExp::from(RSP) + cycles_to_run as i32),
                )?;
                code.sub(
                    params[0],
                    qword_ptr(RegExp::from(RSP) + cycles_remaining as i32),
                )
            })
            .unwrap();
    }
    ra.end_of_alloc_scope();

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

    if ctx.config.enable_cycle_counting {
        let cycles_to_run = STACK_LAYOUT_RSP_OFFSET + StackLayout::cycles_to_run_offset();
        let cycles_remaining = STACK_LAYOUT_RSP_OFFSET + StackLayout::cycles_remaining_offset();
        ctx.config
            .callbacks
            .get_ticks_remaining
            .emit_call_simple(&mut *ra.asm)
            .unwrap();
        ra.asm
            .mov(qword_ptr(RegExp::from(RSP) + cycles_to_run as i32), RAX)
            .unwrap();
        ra.asm
            .mov(qword_ptr(RegExp::from(RSP) + cycles_remaining as i32), RAX)
            .unwrap();
        emit_switch_mxcsr_on_entry(ra.asm, A32JitState::offset_of_guest_mxcsr()).unwrap();
    }
}

// ---------------------------------------------------------------------------
// Barriers
// ---------------------------------------------------------------------------

/// A32DataSynchronizationBarrier: x86 mfence + lfence
///
/// Matches upstream `A32EmitX64::EmitA32DataSynchronizationBarrier` which emits
/// `mfence; lfence`. DSB is the strongest ARM barrier: blocks both loads and
/// stores from being reordered around it. `mfence` alone (store fence) is
/// upstream's choice for DMB; DSB additionally needs `lfence` to serialize
/// against later loads.
pub fn emit_a32_dsb(_ctx: &EmitContext, ra: &mut RegAlloc, _inst_ref: InstRef, _inst: &Inst) {
    ra.asm.mfence().unwrap();
    ra.asm.lfence().unwrap();
}

/// A32DataMemoryBarrier: x86 mfence
///
/// Matches upstream `A32EmitX64::EmitA32DataMemoryBarrier` exactly.
pub fn emit_a32_dmb(_ctx: &EmitContext, ra: &mut RegAlloc, _inst_ref: InstRef, _inst: &Inst) {
    ra.asm.mfence().unwrap();
}

/// A32InstructionSynchronizationBarrier: invoke the configured user callback.
pub fn emit_a32_isb(ctx: &EmitContext, ra: &mut RegAlloc, _inst_ref: InstRef, _inst: &Inst) {
    if !ctx.config.hook_isb {
        return;
    }
    ctx.config
        .callbacks
        .instruction_synchronization_barrier
        .emit_call_simple(&mut *ra.asm)
        .unwrap();
}


// ---------------------------------------------------------------------------
// Coprocessor operations
// ---------------------------------------------------------------------------

fn emit_coprocessor_exception() -> ! {
    unreachable!("A32 coprocessor operation has no compile-time action")
}

fn call_coproc_callback(
    ra: &mut RegAlloc,
    callback: CoprocessorCallback,
    inst_ref: Option<InstRef>,
    arg0: Option<&mut Argument>,
    arg1: Option<&mut Argument>,
) {
    ra.host_call(inst_ref, &mut [None, arg0, arg1, None]);

    if let Some(user_arg) = callback.user_arg {
        ra.asm
            .mov(
                abi::ABI_PARAMS[0].to_reg64(),
                user_arg as usize as i64,
            )
            .unwrap();
    }

    SimpleCallback::new(callback.function as usize as u64)
        .emit_call_simple(&mut *ra.asm)
        .unwrap();
}

pub fn emit_a32_coproc_internal_operation(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    _inst_ref: InstRef,
    inst: &Inst,
) {
    let coproc_info = inst.args[0].get_coproc_info().to_le_bytes();
    let coproc_num = coproc_info[0] as usize;
    let two = coproc_info[1] != 0;
    let opc1 = coproc_info[2] as u32;
    let crd = CoprocReg::from_u8(coproc_info[3]);
    let crn = CoprocReg::from_u8(coproc_info[4]);
    let crm = CoprocReg::from_u8(coproc_info[5]);
    let opc2 = coproc_info[6] as u32;

    let Some(coproc) = ctx.config.coprocessors[coproc_num].as_ref() else {
        emit_coprocessor_exception();
    };
    let Some(action) = coproc.compile_internal_operation(two, opc1, crd, crn, crm, opc2) else {
        emit_coprocessor_exception();
    };

    call_coproc_callback(ra, action, None, None, None);
}

pub fn emit_a32_coproc_send_one_word(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let coproc_info = inst.args[0].get_coproc_info().to_le_bytes();
    let coproc_num = coproc_info[0] as usize;
    let two = coproc_info[1] != 0;
    let opc1 = coproc_info[2] as u32;
    let crn = CoprocReg::from_u8(coproc_info[3]);
    let crm = CoprocReg::from_u8(coproc_info[4]);
    let opc2 = coproc_info[5] as u32;

    let Some(coproc) = ctx.config.coprocessors[coproc_num].as_ref() else {
        emit_coprocessor_exception();
    };
    match coproc.compile_send_one_word(two, opc1, crn, crm, opc2) {
        CallbackOrAccessOneWord::CoprocessorException => emit_coprocessor_exception(),
        CallbackOrAccessOneWord::Callback(callback) => {
            call_coproc_callback(ra, callback, None, Some(&mut args[1]), None);
        }
        CallbackOrAccessOneWord::Memory(destination_ptr) => {
            let word = ra.use_gpr(&mut args[1]);
            let destination_addr = ra.scratch_gpr();
            ra.asm
                .mov(destination_addr, destination_ptr as usize as i64)
                .unwrap();
            ra.asm
                .mov(
                    dword_ptr(RegExp::from(destination_addr)),
                    word.cvt32().unwrap(),
                )
                .unwrap();
        }
    }
}

pub fn emit_a32_coproc_send_two_words(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let coproc_info = inst.args[0].get_coproc_info().to_le_bytes();
    let coproc_num = coproc_info[0] as usize;
    let two = coproc_info[1] != 0;
    let opc = coproc_info[2] as u32;
    let crm = CoprocReg::from_u8(coproc_info[3]);

    let Some(coproc) = ctx.config.coprocessors[coproc_num].as_ref() else {
        emit_coprocessor_exception();
    };
    match coproc.compile_send_two_words(two, opc, crm) {
        CallbackOrAccessTwoWords::CoprocessorException => emit_coprocessor_exception(),
        CallbackOrAccessTwoWords::Callback(callback) => {
            let (first, second) = args.split_at_mut(2);
            call_coproc_callback(
                ra,
                callback,
                None,
                Some(&mut first[1]),
                Some(&mut second[0]),
            );
        }
        CallbackOrAccessTwoWords::Memory(destination_ptrs) => {
            let (first, second) = args.split_at_mut(2);
            let word1 = ra.use_gpr(&mut first[1]);
            let word2 = ra.use_gpr(&mut second[0]);
            let destination_addr = ra.scratch_gpr();
            ra.asm
                .mov(destination_addr, destination_ptrs[0] as usize as i64)
                .unwrap();
            ra.asm
                .mov(
                    dword_ptr(RegExp::from(destination_addr)),
                    word1.cvt32().unwrap(),
                )
                .unwrap();
            ra.asm
                .mov(destination_addr, destination_ptrs[1] as usize as i64)
                .unwrap();
            ra.asm
                .mov(
                    dword_ptr(RegExp::from(destination_addr)),
                    word2.cvt32().unwrap(),
                )
                .unwrap();
        }
    }
}

pub fn emit_a32_coproc_get_one_word(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let coproc_info = inst.args[0].get_coproc_info().to_le_bytes();
    let coproc_num = coproc_info[0] as usize;
    let two = coproc_info[1] != 0;
    let opc1 = coproc_info[2] as u32;
    let crn = CoprocReg::from_u8(coproc_info[3]);
    let crm = CoprocReg::from_u8(coproc_info[4]);
    let opc2 = coproc_info[5] as u32;

    let Some(coproc) = ctx.config.coprocessors[coproc_num].as_ref() else {
        emit_coprocessor_exception();
    };
    match coproc.compile_get_one_word(two, opc1, crn, crm, opc2) {
        CallbackOrAccessOneWord::CoprocessorException => emit_coprocessor_exception(),
        CallbackOrAccessOneWord::Callback(callback) => {
            call_coproc_callback(ra, callback, Some(inst_ref), None, None);
        }
        CallbackOrAccessOneWord::Memory(source_ptr) => {
            let word = ra.scratch_gpr();
            let source_addr = ra.scratch_gpr();
            ra.asm
                .mov(source_addr, source_ptr as usize as i64)
                .unwrap();
            ra.asm
                .mov(
                    word.cvt32().unwrap(),
                    dword_ptr(RegExp::from(source_addr)),
                )
                .unwrap();
            ra.define_value(inst_ref, word);
        }
    }
}

pub fn emit_a32_coproc_get_two_words(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let coproc_info = inst.args[0].get_coproc_info().to_le_bytes();
    let coproc_num = coproc_info[0] as usize;
    let two = coproc_info[1] != 0;
    let opc = coproc_info[2] as u32;
    let crm = CoprocReg::from_u8(coproc_info[3]);

    let Some(coproc) = ctx.config.coprocessors[coproc_num].as_ref() else {
        emit_coprocessor_exception();
    };
    match coproc.compile_get_two_words(two, opc, crm) {
        CallbackOrAccessTwoWords::CoprocessorException => emit_coprocessor_exception(),
        CallbackOrAccessTwoWords::Callback(callback) => {
            call_coproc_callback(ra, callback, Some(inst_ref), None, None);
        }
        CallbackOrAccessTwoWords::Memory(source_ptrs) => {
            let result = ra.scratch_gpr();
            let source_addr = ra.scratch_gpr();
            let temporary = ra.scratch_gpr();
            ra.asm
                .mov(source_addr, source_ptrs[1] as usize as i64)
                .unwrap();
            ra.asm
                .mov(
                    result.cvt32().unwrap(),
                    dword_ptr(RegExp::from(source_addr)),
                )
                .unwrap();
            ra.asm.shl(result, 32).unwrap();
            ra.asm
                .mov(source_addr, source_ptrs[0] as usize as i64)
                .unwrap();
            ra.asm
                .mov(
                    temporary.cvt32().unwrap(),
                    dword_ptr(RegExp::from(source_addr)),
                )
                .unwrap();
            ra.asm.or_(result, temporary).unwrap();
            ra.define_value(inst_ref, result);
        }
    }
}

pub fn emit_a32_coproc_load_words(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let coproc_info = inst.args[0].get_coproc_info().to_le_bytes();
    let coproc_num = coproc_info[0] as usize;
    let two = coproc_info[1] != 0;
    let long_transfer = coproc_info[2] != 0;
    let crd = CoprocReg::from_u8(coproc_info[3]);
    let option = (coproc_info[4] != 0).then_some(coproc_info[5]);

    let Some(coproc) = ctx.config.coprocessors[coproc_num].as_ref() else {
        emit_coprocessor_exception();
    };
    let Some(action) = coproc.compile_load_words(two, long_transfer, crd, option) else {
        emit_coprocessor_exception();
    };
    call_coproc_callback(ra, action, None, Some(&mut args[1]), None);
}

pub fn emit_a32_coproc_store_words(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let coproc_info = inst.args[0].get_coproc_info().to_le_bytes();
    let coproc_num = coproc_info[0] as usize;
    let two = coproc_info[1] != 0;
    let long_transfer = coproc_info[2] != 0;
    let crd = CoprocReg::from_u8(coproc_info[3]);
    let option = (coproc_info[4] != 0).then_some(coproc_info[5]);

    let Some(coproc) = ctx.config.coprocessors[coproc_num].as_ref() else {
        emit_coprocessor_exception();
    };
    let Some(action) = coproc.compile_store_words(two, long_transfer, crd, option) else {
        emit_coprocessor_exception();
    };
    call_coproc_callback(ra, action, None, Some(&mut args[1]), None);
}

#[cfg(test)]
mod tests {
    use super::{a32_bx_upper_without_t, emit_a32_coproc_send_one_word};
    use crate::backend::x64::callback::{Callback, SimpleCallback};
    use crate::backend::x64::emit::emit_block;
    use crate::backend::x64::emit_context::{ArchConfig, EmitCallbacks, EmitConfig, EmitContext};
    use crate::backend::x64::hostloc::{ANY_GPR, ANY_XMM, HOST_R13};
    use crate::backend::x64::reg_alloc::RegAlloc;
    use crate::frontend::a32::fpscr::FPSCR;
    use crate::frontend::a32::psr::PSR;
    use crate::frontend::a32::types::Reg;
    use crate::ir::acc_type::AccType;
    use crate::ir::block::Block;
    use crate::ir::inst::Inst;
    use crate::ir::location::A32LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::terminal::Terminal;
    use crate::ir::types::Type;
    use crate::ir::value::{InstRef, Value};
    use crate::interface::a32::coprocessor::{
        Callback as CoprocessorCallback, CallbackOrAccessOneWord, CallbackOrAccessTwoWords,
        Coprocessor,
    };
    use crate::interface::a32::coprocessor_util::CoprocReg;
    use std::cell::UnsafeCell;
    use std::sync::{Arc, Mutex};

    fn dummy_emit_config() -> EmitConfig {
        fn cb() -> Box<dyn Callback> {
            Box::new(SimpleCallback::new(0))
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

    #[test]
    fn bx_write_pc_preserves_single_step_and_e_bits_when_clearing_t() {
        let config = dummy_emit_config();
        let mut cpsr = PSR::default();
        cpsr.set_t(true);
        cpsr.set_e(true);
        let fpscr = FPSCR::default();
        let location = A32LocationDescriptor::new(0x1000, cpsr, fpscr, false).to_location();
        let end_location = A32LocationDescriptor::new(0x1004, cpsr, fpscr, true).to_location();
        let mut ctx = EmitContext::new(location, &config);
        ctx.set_arch(ArchConfig::A32);
        ctx.end_location = Some(end_location);

        let upper = a32_bx_upper_without_t(&ctx);
        assert_eq!(upper & 1, 0);
        assert_ne!(upper & (1 << 1), 0);
        assert_ne!(upper & (1 << 2), 0);
    }

    fn type_bit_width(ty: Type) -> usize {
        match ty {
            Type::U1 | Type::U8 => 8,
            Type::U16 => 16,
            Type::U32 | Type::NZCVFlags => 32,
            Type::U64 => 64,
            Type::U128 => 128,
            _ => 64,
        }
    }

    fn coproc_info(cp: u8, two: bool, opc1: u8, crn: u8, crm: u8, opc2: u8) -> u64 {
        cp as u64
            | ((two as u64) << 8)
            | ((opc1 as u64) << 16)
            | ((crn as u64) << 24)
            | ((crm as u64) << 32)
            | ((opc2 as u64) << 40)
    }

    struct RecordingCoprocessor {
        destination: UnsafeCell<u32>,
        send_one_word: Mutex<Option<(bool, u32, CoprocReg, CoprocReg, u32)>>,
    }

    unsafe impl Send for RecordingCoprocessor {}
    unsafe impl Sync for RecordingCoprocessor {}

    impl Coprocessor for RecordingCoprocessor {
        fn compile_internal_operation(
            &self,
            _two: bool,
            _opc1: u32,
            _crd: CoprocReg,
            _crn: CoprocReg,
            _crm: CoprocReg,
            _opc2: u32,
        ) -> Option<CoprocessorCallback> {
            None
        }

        fn compile_send_one_word(
            &self,
            two: bool,
            opc1: u32,
            crn: CoprocReg,
            crm: CoprocReg,
            opc2: u32,
        ) -> CallbackOrAccessOneWord {
            *self.send_one_word.lock().unwrap() = Some((two, opc1, crn, crm, opc2));
            CallbackOrAccessOneWord::Memory(self.destination.get())
        }

        fn compile_send_two_words(
            &self,
            _two: bool,
            _opc: u32,
            _crm: CoprocReg,
        ) -> CallbackOrAccessTwoWords {
            CallbackOrAccessTwoWords::CoprocessorException
        }

        fn compile_get_one_word(
            &self,
            _two: bool,
            _opc1: u32,
            _crn: CoprocReg,
            _crm: CoprocReg,
            _opc2: u32,
        ) -> CallbackOrAccessOneWord {
            CallbackOrAccessOneWord::CoprocessorException
        }

        fn compile_get_two_words(
            &self,
            _two: bool,
            _opc: u32,
            _crm: CoprocReg,
        ) -> CallbackOrAccessTwoWords {
            CallbackOrAccessTwoWords::CoprocessorException
        }

        fn compile_load_words(
            &self,
            _two: bool,
            _long_transfer: bool,
            _crd: CoprocReg,
            _option: Option<u8>,
        ) -> Option<CoprocessorCallback> {
            None
        }

        fn compile_store_words(
            &self,
            _two: bool,
            _long_transfer: bool,
            _crd: CoprocReg,
            _option: Option<u8>,
        ) -> Option<CoprocessorCallback> {
            None
        }
    }

    fn emit_send_one_word(info: u64, coprocessor: Arc<dyn Coprocessor>) -> Vec<u8> {
        let mut config = dummy_emit_config();
        config.coprocessors[15] = Some(coprocessor);
        let ctx = EmitContext::new(A32LocationDescriptor::at(0x1000).to_location(), &config);
        let inst = Inst::new(
            Opcode::A32CoprocSendOneWord,
            &[Value::ImmCoprocInfo(info), Value::ImmU32(0)],
        );
        let mut asm = rxbyak::CodeAssembler::new(4096).unwrap();
        {
            let mut ra = RegAlloc::new_default(&mut asm, vec![]);
            emit_a32_coproc_send_one_word(&ctx, &mut ra, InstRef(0), &inst);
        }
        asm.code().to_vec()
    }

    #[test]
    fn configured_coprocessor_receives_exact_send_one_word_fields() {
        let coprocessor = Arc::new(RecordingCoprocessor {
            destination: UnsafeCell::new(0),
            send_one_word: Mutex::new(None),
        });
        let destination = coprocessor.destination.get() as usize as u64;
        let code = emit_send_one_word(
            coproc_info(15, true, 6, 7, 10, 5),
            coprocessor.clone(),
        );

        assert_eq!(
            *coprocessor.send_one_word.lock().unwrap(),
            Some((true, 6, CoprocReg::C7, CoprocReg::C10, 5))
        );
        assert!(code
            .windows(8)
            .any(|window| window == destination.to_le_bytes()));
    }

    #[test]
    fn a32_write_memory32_after_shift_and_add_emits_without_losing_address_value() {
        let config = dummy_emit_config();
        let mut cpsr = PSR::default();
        cpsr.set_t(false);
        let fpscr = FPSCR::default();
        let location = A32LocationDescriptor::new(0x006A8084, cpsr, fpscr, false).to_location();

        let mut block = Block::new(location);
        let get_r2 = block.append(Opcode::A32GetRegister, &[Value::ImmA32Reg(Reg::R2)]);
        let get_c = block.append(Opcode::A32GetCFlag, &[]);
        let lsl = block.append(
            Opcode::LogicalShiftLeft32,
            &[Value::Inst(get_r2), Value::ImmU8(2), Value::Inst(get_c)],
        );
        let carry = block.append(Opcode::GetCarryFromOp, &[Value::Inst(lsl)]);
        block.get_mut(lsl).next_pseudoop = Some(carry);
        let get_r3 = block.append(Opcode::A32GetRegister, &[Value::ImmA32Reg(Reg::R3)]);
        let add = block.append(
            Opcode::Add32,
            &[Value::Inst(get_r3), Value::Inst(lsl), Value::ImmU1(false)],
        );
        let get_r1 = block.append(Opcode::A32GetRegister, &[Value::ImmA32Reg(Reg::R1)]);
        block.append(
            Opcode::A32WriteMemory32,
            &[
                Value::ImmU64(0x006A80F4),
                Value::Inst(add),
                Value::Inst(get_r1),
                Value::ImmAccType(AccType::Normal),
            ],
        );
        block.set_terminal(Terminal::ReturnToDispatch);
        block.set_end_location(location);

        let inst_info: Vec<(u32, usize)> = block
            .instructions
            .iter()
            .map(|inst| (inst.use_count, type_bit_width(inst.return_type())))
            .collect();

        let mut asm = rxbyak::CodeAssembler::new(2 * 1024 * 1024).unwrap();
        let fastmem_fallbacks =
            crate::backend::x64::a32_emit_x64_memory::gen_fastmem_fallbacks(
                &mut asm,
                &config.callbacks,
                None,
            );
        let mut gpr_order = ANY_GPR.to_vec();
        gpr_order.retain(|&loc| loc != HOST_R13);
        let mut ra = RegAlloc::new(&mut asm, gpr_order, ANY_XMM.to_vec(), inst_info);

        let mut ctx = EmitContext::new(location, &config);
        ctx.set_arch(ArchConfig::A32);
        ctx.fastmem_available = true;
        ctx.fastmem_fallbacks = Some(&fastmem_fallbacks as *const _ as *const ());
        ctx.block = Some(&block);
        ctx.end_location = Some(block.end_location());

        emit_block(&ctx, &mut ra, &block);
    }

    #[test]
    fn fp_single_to_fixed_s32_tie_away_registers_source_once() {
        let config = dummy_emit_config();
        let mut cpsr = PSR::default();
        cpsr.set_t(false);
        let fpscr = FPSCR::default();
        let location = A32LocationDescriptor::new(0x1000, cpsr, fpscr, false).to_location();

        let mut block = Block::new(location);
        let source = block.append(Opcode::FPAdd32, &[Value::ImmU32(0), Value::ImmU32(0)]);
        let converted = block.append(
            Opcode::FPSingleToFixedS32,
            &[Value::Inst(source), Value::ImmU8(0), Value::ImmU8(4)],
        );
        block.append(
            Opcode::A32SetRegister,
            &[Value::ImmA32Reg(Reg::R0), Value::Inst(converted)],
        );
        block.set_terminal(Terminal::ReturnToDispatch);
        block.set_end_location(location);

        assert_eq!(block.get(source).use_count, 1);

        let inst_info: Vec<(u32, usize)> = block
            .instructions
            .iter()
            .map(|inst| (inst.use_count, type_bit_width(inst.return_type())))
            .collect();

        let mut asm = rxbyak::CodeAssembler::new(4096).unwrap();
        let mut gpr_order = ANY_GPR.to_vec();
        gpr_order.retain(|&loc| loc != HOST_R13);
        let mut ra = RegAlloc::new(&mut asm, gpr_order, ANY_XMM.to_vec(), inst_info);

        let mut ctx = EmitContext::new(location, &config);
        ctx.set_arch(ArchConfig::A32);
        ctx.block = Some(&block);
        ctx.end_location = Some(block.end_location());

        emit_block(&ctx, &mut ra, &block);
    }
}
