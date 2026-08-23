//! A32-specific IR opcode emit functions.
//!
//! These emit x86-64 code for the ~60 A32-prefixed IR opcodes.
//! They access `A32JitState` via R15 + offset (same convention as A64 emitters).

use rxbyak::{byte_ptr, dword_ptr, qword_ptr, word_ptr, xmmword_ptr, JmpType};
use rxbyak::{Reg, RegExp, R10, R11, R15, R8, R9, RAX, RCX, RDI, RDX, RSI, RSP};

use crate::backend::x64::a64_emit_x64_memory::{emit_call_to_offset, FastmemFallbacksTable};
use crate::backend::x64::abi;
use crate::backend::x64::block_of_code::{
    emit_switch_mxcsr_on_entry, emit_switch_mxcsr_on_exit, STACK_LAYOUT_RSP_OFFSET,
};
use crate::backend::x64::emit_context::EmitContext;
use crate::backend::x64::emit_x64_memory::{
    emit_read_memory_mov, emit_vaddr_lookup_a32, emit_write_memory_mov, is_ordered,
};
use crate::backend::x64::host_feature::HostFeature;
use crate::backend::x64::jit_state::A32JitState;
use crate::backend::x64::nzcv_util;
use crate::backend::x64::reg_alloc::{Argument, RegAlloc};
use crate::backend::x64::stack_layout::StackLayout;
use crate::ir::inst::Inst;
use crate::ir::value::InstRef;

fn emit_bitsize_read_mov(
    ra: &mut RegAlloc,
    bitsize: usize,
    value_idx: u8,
    addr: RegExp,
    ordered: bool,
) -> usize {
    match bitsize {
        8 => emit_read_memory_mov::<8>(ra.asm, value_idx, addr, ordered),
        16 => emit_read_memory_mov::<16>(ra.asm, value_idx, addr, ordered),
        32 => emit_read_memory_mov::<32>(ra.asm, value_idx, addr, ordered),
        64 => emit_read_memory_mov::<64>(ra.asm, value_idx, addr, ordered),
        _ => unreachable!(),
    }
}

fn emit_bitsize_write_mov(
    ra: &mut RegAlloc,
    bitsize: usize,
    addr: RegExp,
    value_idx: u8,
    ordered: bool,
) -> usize {
    match bitsize {
        8 => emit_write_memory_mov::<8>(ra.asm, addr, value_idx, ordered),
        16 => emit_write_memory_mov::<16>(ra.asm, addr, value_idx, ordered),
        32 => emit_write_memory_mov::<32>(ra.asm, addr, value_idx, ordered),
        64 => emit_write_memory_mov::<64>(ra.asm, addr, value_idx, ordered),
        _ => unreachable!(),
    }
}

fn parse_fastmem_vaddr_range_env() -> Option<(u64, u64)> {
    std::env::var("RUZU_TRAP_FASTMEM_ANY_VADDR_RANGE")
        .ok()
        .and_then(|s| {
            let parts: Vec<&str> = s.split(':').collect();
            if parts.len() != 2 {
                return None;
            }
            let parse_hex = |raw: &str| {
                let raw = raw.trim();
                let digits = raw
                    .strip_prefix("0x")
                    .or_else(|| raw.strip_prefix("0X"))
                    .unwrap_or(raw);
                u64::from_str_radix(digits, 16).ok()
            };
            Some((parse_hex(parts[0])?, parse_hex(parts[1])?))
        })
}

fn parse_trace_fastmem_write_range_env() -> Option<(u64, u64)> {
    std::env::var("RUZU_TRACE_FASTMEM_W_RANGE")
        .ok()
        .and_then(|s| {
            let parts: Vec<&str> = s.split(':').collect();
            if parts.len() != 2 {
                return None;
            }
            let parse_hex = |raw: &str| {
                let raw = raw.trim();
                let digits = raw
                    .strip_prefix("0x")
                    .or_else(|| raw.strip_prefix("0X"))
                    .unwrap_or(raw);
                u64::from_str_radix(digits, 16).ok()
            };
            Some((parse_hex(parts[0])?, parse_hex(parts[1])?))
        })
}

fn parse_trap_fastmem_write_value_env() -> Option<u64> {
    std::env::var("RUZU_TRAP_FASTMEM_W_VALUE")
        .ok()
        .and_then(|raw| {
            let raw = raw.trim();
            let digits = raw
                .strip_prefix("0x")
                .or_else(|| raw.strip_prefix("0X"))
                .unwrap_or(raw);
            u64::from_str_radix(digits, 16).ok()
        })
}

fn emit_preserved_a32_fastmem_write_trace_hook(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    vaddr_idx: u8,
    value_idx: u8,
    bitsize: usize,
) {
    ra.asm.push(rxbyak::Reg::gpr64(vaddr_idx)).unwrap();
    ra.asm.push(rxbyak::Reg::gpr64(value_idx)).unwrap();

    let caller_save_gprs: &[Reg] = &[RAX, RCX, RDX, RDI, RSI, R8, R9, R10, R11];
    for &reg in caller_save_gprs {
        ra.asm.push(reg).unwrap();
    }
    ra.asm.sub(RSP, 8i32).unwrap();

    const XMM_SAVE_BYTES: i32 = 16 * 16;
    ra.asm.sub(RSP, XMM_SAVE_BYTES).unwrap();
    let rsp = RegExp::from(RSP);
    for i in 0..16 {
        ra.asm
            .movups(
                xmmword_ptr(rsp.clone() + (i * 16) as i32),
                Reg::xmm(i as u8),
            )
            .unwrap();
    }

    const SAVED_VALUE_OFFSET: i32 = XMM_SAVE_BYTES + 8 + 9 * 8;
    const SAVED_VADDR_OFFSET: i32 = SAVED_VALUE_OFFSET + 8;
    ra.asm
        .mov(RDX, qword_ptr(RegExp::from(RSP) + SAVED_VADDR_OFFSET))
        .unwrap();
    ra.asm
        .mov(R8, qword_ptr(RegExp::from(RSP) + SAVED_VALUE_OFFSET))
        .unwrap();
    ra.asm.mov(RDI, R15).unwrap();
    ra.asm
        .mov(RSI, ctx.arch.extract_pc(ctx.location) as i64)
        .unwrap();
    ra.asm.mov(RCX, bitsize as i64).unwrap();
    ra.asm
        .mov(
            RAX,
            crate::jit::a32_fastmem_write_trace_hook as usize as i64,
        )
        .unwrap();
    ra.asm.call_reg(RAX).unwrap();

    let rsp = RegExp::from(RSP);
    for i in 0..16 {
        ra.asm
            .movups(
                Reg::xmm(i as u8),
                xmmword_ptr(rsp.clone() + (i * 16) as i32),
            )
            .unwrap();
    }
    ra.asm.add(RSP, XMM_SAVE_BYTES).unwrap();
    ra.asm.add(RSP, 8i32).unwrap();

    for &reg in caller_save_gprs.iter().rev() {
        ra.asm.pop(reg).unwrap();
    }
    ra.asm.add(RSP, 16i32).unwrap();
}

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
    let nzcv_offset = ctx.arch.cpsr_nzcv_offset();
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
    let value = ra.use_gpr(&mut args[0]);
    ra.asm
        .or_(
            dword_ptr(RegExp::from(R15) + offset as i32),
            value.cvt32().unwrap(),
        )
        .unwrap();
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

/// A32PcExecHook: debug-only per-instruction PC execution hook.
///
/// Lowered exactly like `emit_a32_call_supervisor` (so `host_call` flushes the
/// guest register file back into `A32JitState` before the call — the hook can
/// therefore read accurate r0-r15 even though this fires MID-block). Instead of
/// the SVC callback it calls the free function `crate::jit::a32_pc_trace_hook`
/// with `(jit_state=R15, fastmem_base=R13, tag=pc)`, reusing the existing
/// aggregation that `RUZU_A32_PC_TRACE` already feeds. Emitted only for guest
/// PCs in the `RUZU_A32_PC_EXEC` target set, so unset = no codegen at all.
pub fn emit_a32_pc_exec_hook(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut no_args: [Option<&mut Argument>; 0] = [];
    ra.host_call(None, &mut no_args);
    ra.end_of_alloc_scope();

    let args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let pc = args[0].value.get_imm_as_u64();

    // a32_pc_trace_hook(jit_state_ptr: R15, fastmem_base: R13, tag: pc).
    ra.asm.mov(abi::ABI_PARAMS[0].to_reg64(), R15).unwrap();
    ra.asm
        .mov(abi::ABI_PARAMS[1].to_reg64(), rxbyak::R13)
        .unwrap();
    ra.asm
        .mov(abi::ABI_PARAMS[2].to_reg64(), pc as i64)
        .unwrap();
    ra.asm
        .mov(RAX, crate::jit::a32_pc_trace_hook as usize as i64)
        .unwrap();
    ra.asm.call_reg(RAX).unwrap();
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
// Memory operations (delegate to shared memory callbacks)
// ---------------------------------------------------------------------------

/// Matches upstream `A32EmitX64::ShouldFastmem`.
fn should_fastmem(ctx: &EmitContext, inst_ref: InstRef) -> bool {
    if !ctx.fastmem_available {
        return false;
    }

    let marker = (ctx.location, inst_ref.0);
    ctx.do_not_fastmem
        .map(|markers| !markers.contains(&marker))
        .unwrap_or(true)
}

fn emit_a32_memory_read(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    bitsize: usize,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    // args[2] is the AccType immediate (LDA / LDAH / LDAB / LDAEX use Ordered).
    let ordered = is_ordered(args[2].value.get_acc_type());

    // `RUZU_NO_FASTMEM_R{8,16,32,64}=1` — force this width's loads through
    // the slow callback path. Counterpart to the write gates above.
    let force_callback = match bitsize {
        8 => std::env::var_os("RUZU_NO_FASTMEM_R8").is_some(),
        16 => std::env::var_os("RUZU_NO_FASTMEM_R16").is_some(),
        32 => std::env::var_os("RUZU_NO_FASTMEM_R32").is_some(),
        64 => std::env::var_os("RUZU_NO_FASTMEM_R64").is_some(),
        _ => false,
    } || (bitsize == 64
        && std::env::var("RUZU_NO_FASTMEM_R64_AT_PC")
            .ok()
            .and_then(|raw| {
                u64::from_str_radix(
                    raw.trim()
                        .strip_prefix("0x")
                        .or_else(|| raw.trim().strip_prefix("0X"))
                        .unwrap_or(raw.trim()),
                    16,
                )
                .ok()
            })
            .is_some_and(|pc| ctx.arch.extract_pc(ctx.location) == pc));

    if should_fastmem(ctx, inst_ref) && bitsize <= 64 && !force_callback {
        // Fastmem path: result = [R13+vaddr]. Upstream's ShouldFastmem result
        // takes precedence over the configured page table; the page table is
        // only used after a fault disables fastmem for this instruction.
        // R13 = fastmem_pointer, loaded in the dispatcher prelude.
        //
        // Upstream-faithful: for ordered loads (LDA / LDAB / LDAH /
        // LDAEX) emit `xor reg, reg; lock xadd [r13+vaddr], reg`
        // (mirrors `EmitReadMemoryMov<bitsize>` ordered path in
        // `emit_x64_memory.h:204-247`). The xor pre-zeroes the
        // destination so the xadd performs `mem += 0` — i.e. a pure
        // atomic load with full memory-ordering semantics. The
        // returned register holds the pre-add value.
        //
        // The `inst_offset` captured below is the start of the `lock`
        // prefix (or of the `mov` in the unordered case). The SIGSEGV
        // handler delivers RIP at that exact byte when the access
        // faults on unmapped memory.
        let vaddr = ra.use_gpr(&mut args[1]);
        let result = ra.scratch_gpr();
        let vaddr_idx = vaddr.get_idx();
        let result_idx = result.get_idx();
        let addr = RegExp::from(rxbyak::R13) + vaddr;
        if ordered {
            // xor zero-extends the 64-bit register too (Intel SDM:
            // 32-bit operand zeroes upper 32 bits).
            ra.asm
                .xor_(result.cvt32().unwrap(), result.cvt32().unwrap())
                .unwrap();
        }
        let inst_offset = ra.asm.size();
        if ordered {
            ra.asm.lock().unwrap();
            match bitsize {
                8 => {
                    ra.asm.xadd(byte_ptr(addr), result.cvt8().unwrap()).unwrap();
                }
                16 => {
                    ra.asm
                        .xadd(word_ptr(addr), result.cvt16().unwrap())
                        .unwrap();
                }
                32 => {
                    ra.asm
                        .xadd(dword_ptr(addr), result.cvt32().unwrap())
                        .unwrap();
                }
                64 => {
                    ra.asm.xadd(qword_ptr(addr), result).unwrap();
                }
                _ => unreachable!(),
            }
        } else {
            match bitsize {
                8 => {
                    ra.asm
                        .movzx(result.cvt32().unwrap(), byte_ptr(addr))
                        .unwrap();
                }
                16 => {
                    ra.asm
                        .movzx(result.cvt32().unwrap(), word_ptr(addr))
                        .unwrap();
                }
                32 => {
                    ra.asm
                        .mov(result.cvt32().unwrap(), dword_ptr(addr))
                        .unwrap();
                }
                64 => {
                    ra.asm.mov(result, qword_ptr(addr)).unwrap();
                }
                _ => unreachable!(),
            }
        }
        let resume_offset = ra.asm.size();
        let fallbacks = unsafe {
            &*(ctx
                .fastmem_fallbacks
                .expect("A32 fastmem path requires fallback table")
                as *const FastmemFallbacksTable)
        };
        let wrapped_fn_off = fallbacks.read_stub(ordered, bitsize, vaddr_idx, result_idx);
        let marker = (ctx.location, inst_ref.0);
        let recompile = ctx.config.memory.recompile_on_fastmem_failure;
        ctx.deferred_emits.borrow_mut().push(Box::new(move |dctx| {
            dctx.fastmem_patches.add(
                dctx.code_base + inst_offset as u64,
                crate::backend::x64::exception_handler::FastmemPatchInfo::new(
                    dctx.code_base + resume_offset as u64,
                    dctx.code_base + wrapped_fn_off as u64,
                    Some(marker),
                    recompile,
                ),
            );
        }));
        ra.define_value(inst_ref, result);
        return;
    }

    if ctx.config.memory.page_table_present && !force_callback {
        // When this instruction has been removed from `ShouldFastmem` after a
        // fault, upstream falls through to the page-table path rather than
        // calling the callback unconditionally.
        let vaddr = ra.use_gpr(&mut args[1]);
        let result = ra.scratch_gpr();
        let vaddr_idx = vaddr.get_idx();
        let result_idx = result.get_idx();
        let fallbacks = unsafe {
            &*(ctx
                .fastmem_fallbacks
                .expect("A32 page-table path requires fallback table")
                as *const FastmemFallbacksTable)
        };
        let wrapped_fn_off = fallbacks.read_stub(ordered, bitsize, vaddr_idx, result_idx);
        let abort = ra.asm.create_label();
        let end = ra.asm.create_label();
        let addr = emit_vaddr_lookup_a32(ra, ctx, bitsize, abort, vaddr);
        emit_bitsize_read_mov(ra, bitsize, result_idx, addr, ordered);

        ctx.deferred_emits.borrow_mut().push(Box::new(move |dctx| {
            let asm = &mut *dctx.asm;
            asm.bind(&abort).unwrap();
            emit_call_to_offset(asm, wrapped_fn_off);
            asm.jmp(&end, JmpType::Near).unwrap();
        }));
        ra.asm.bind(&end).unwrap();
        ra.define_value(inst_ref, result);
        return;
    }

    // Callback path (slow)
    ra.host_call(Some(inst_ref), &mut [None, Some(&mut args[1]), None, None]);
    if ordered {
        // Drain pending stores before the slow-path load. Mirrors upstream
        // `EmitMemoryRead<bitsize>` ordered path in
        // `emit_x64_memory.cpp.inc:64-66`.
        ra.asm.mfence().unwrap();
    }
    let callback = match bitsize {
        8 => &ctx.config.callbacks.memory_read_8,
        16 => &ctx.config.callbacks.memory_read_16,
        32 => &ctx.config.callbacks.memory_read_32,
        64 => &ctx.config.callbacks.memory_read_64,
        _ => unreachable!(),
    };
    callback.emit_call_simple(&mut *ra.asm).unwrap();
}

fn emit_a32_memory_write(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    bitsize: usize,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    // args[3] is the AccType immediate (STL / STLH / STLB / STLEX use Ordered).
    let ordered = is_ordered(args[3].value.get_acc_type());

    // `RUZU_NO_FASTMEM_W{8,16,32,64}=1` — force this width through the slow
    // callback path. Used to diagnose userspace-vs-kernel memory-visibility
    // slow-path traceable via the existing `RUZU_TRACE_W_AT_VADDR` hook in
    // `arm_dynarmic_32.rs::watch_write`. Mirrors A64's per-width gates in
    // `a64_emit_x64_memory.rs:711-716`.
    let force_callback = match bitsize {
        8 => std::env::var_os("RUZU_NO_FASTMEM_W8").is_some(),
        16 => std::env::var_os("RUZU_NO_FASTMEM_W16").is_some(),
        32 => std::env::var_os("RUZU_NO_FASTMEM_W32").is_some(),
        64 => std::env::var_os("RUZU_NO_FASTMEM_W64").is_some(),
        _ => false,
    };

    if should_fastmem(ctx, inst_ref) && bitsize <= 64 && !force_callback {
        // Fastmem path: [R13+vaddr] = value. As on the read side, upstream
        // gives a valid ShouldFastmem marker precedence over the page table.
        //
        // Upstream-faithful: for ordered stores (STL / STLB / STLH /
        // STLEX) emit `xchg [r13+vaddr], value` (mirrors
        // `EmitWriteMemoryMov<bitsize>` ordered path in
        // `emit_x64_memory.h:274-339`). `xchg` with a memory operand
        // has an implicit `lock` prefix on x86, providing full
        // memory-ordering semantics (release-store).
        //
        // Note: `xchg` *destroys* the value register (it ends up
        // holding the previous memory contents). Upstream uses
        // `UseScratchGpr` for the value when ordered to mark this;
        // we do the same below.
        let vaddr = ra.use_gpr(&mut args[1]);
        let value = if ordered {
            ra.use_scratch_gpr(&mut args[2])
        } else {
            ra.use_gpr(&mut args[2])
        };
        let vaddr_idx = vaddr.get_idx();
        let value_idx = value.get_idx();
        let addr = RegExp::from(rxbyak::R13) + vaddr;
        let inst_offset = ra.asm.size();
        if let Some(expected) = parse_trap_fastmem_write_value_env().filter(|_| bitsize == 32) {
            let ok = ra.asm.create_label();
            let r11 = rxbyak::Reg::gpr64(11);

            // Match ruzu-cmd's A32 SIGILL recovery convention: [rsp+16]
            // contains the guest destination vaddr while the trap fires.
            ra.asm.push(vaddr).unwrap();
            ra.asm.push(r11).unwrap();
            ra.asm.pushf().unwrap();

            match bitsize {
                8 => {
                    ra.asm
                        .mov(r11.cvt32().unwrap(), (expected & 0xFF) as i32)
                        .unwrap();
                    if value_idx == 11 {
                        ra.asm
                            .cmp(byte_ptr(RegExp::from(RSP) + 8), r11.cvt8().unwrap())
                            .unwrap();
                    } else {
                        ra.asm
                            .cmp(value.cvt8().unwrap(), r11.cvt8().unwrap())
                            .unwrap();
                    }
                }
                16 => {
                    ra.asm
                        .mov(r11.cvt32().unwrap(), (expected & 0xFFFF) as i32)
                        .unwrap();
                    if value_idx == 11 {
                        ra.asm
                            .cmp(word_ptr(RegExp::from(RSP) + 8), r11.cvt16().unwrap())
                            .unwrap();
                    } else {
                        ra.asm
                            .cmp(value.cvt16().unwrap(), r11.cvt16().unwrap())
                            .unwrap();
                    }
                }
                32 => {
                    ra.asm.mov(r11.cvt32().unwrap(), expected as i32).unwrap();
                    if value_idx == 11 {
                        ra.asm
                            .cmp(dword_ptr(RegExp::from(RSP) + 8), r11.cvt32().unwrap())
                            .unwrap();
                    } else {
                        ra.asm
                            .cmp(value.cvt32().unwrap(), r11.cvt32().unwrap())
                            .unwrap();
                    }
                }
                64 => {
                    ra.asm.mov(r11, expected as i64).unwrap();
                    if value_idx == 11 {
                        ra.asm.cmp(qword_ptr(RegExp::from(RSP) + 8), r11).unwrap();
                    } else {
                        ra.asm.cmp(value, r11).unwrap();
                    }
                }
                _ => unreachable!(),
            }
            ra.asm.jne(&ok, rxbyak::JmpType::Near).unwrap();
            ra.asm.ud2().unwrap();
            ra.asm.bind(&ok).unwrap();
            ra.asm.popf().unwrap();
            ra.asm.pop(r11).unwrap();
            ra.asm.pop(vaddr).unwrap();
        }
        if ordered {
            match bitsize {
                8 => {
                    ra.asm.xchg(byte_ptr(addr), value.cvt8().unwrap()).unwrap();
                }
                16 => {
                    ra.asm.xchg(word_ptr(addr), value.cvt16().unwrap()).unwrap();
                }
                32 => {
                    ra.asm
                        .xchg(dword_ptr(addr), value.cvt32().unwrap())
                        .unwrap();
                }
                64 => {
                    ra.asm.xchg(qword_ptr(addr), value).unwrap();
                }
                _ => unreachable!(),
            }
        } else {
            match bitsize {
                8 => {
                    ra.asm.mov(byte_ptr(addr), value.cvt8().unwrap()).unwrap();
                }
                16 => {
                    ra.asm.mov(word_ptr(addr), value.cvt16().unwrap()).unwrap();
                }
                32 => {
                    ra.asm.mov(dword_ptr(addr), value.cvt32().unwrap()).unwrap();
                }
                64 => {
                    ra.asm.mov(qword_ptr(addr), value).unwrap();
                }
                _ => unreachable!(),
            }
        }

        if let Some((lo, hi)) = parse_fastmem_vaddr_range_env() {
            let ok = ra.asm.create_label();
            let vaddr_reg = rxbyak::Reg::gpr64(vaddr_idx);
            let scratch_idx = if vaddr_idx == 11 { 10 } else { 11 };
            let scratch = rxbyak::Reg::gpr64(scratch_idx);
            let trap_nonzero =
                std::env::var_os("RUZU_TRAP_FASTMEM_ANY_VADDR_RANGE_NONZERO").is_some();
            let trap_value = std::env::var("RUZU_TRAP_FASTMEM_ANY_VADDR_RANGE_VALUE")
                .ok()
                .and_then(|raw| {
                    let raw = raw.trim();
                    let digits = raw
                        .strip_prefix("0x")
                        .or_else(|| raw.strip_prefix("0X"))
                        .unwrap_or(raw);
                    u64::from_str_radix(digits, 16).ok()
                });

            // Match A64's diagnostic stack convention: after
            // push(vaddr), push(scratch), pushf(), ruzu-cmd's SIGILL handler
            // recovers the trapped guest vaddr from [rsp+16].
            ra.asm.push(vaddr_reg).unwrap();
            ra.asm.push(scratch).unwrap();
            ra.asm.pushf().unwrap();

            // Trap when the emitted store range overlaps [lo, hi), not
            // only when the store starts inside the range. STM/STRD can
            // start before the interesting word and still overwrite it.
            ra.asm.mov(scratch, hi as i64).unwrap();
            ra.asm.cmp(vaddr_reg, scratch).unwrap();
            ra.asm.jae(&ok, rxbyak::JmpType::Near).unwrap();

            ra.asm.mov(scratch, vaddr_reg).unwrap();
            ra.asm.add(scratch, (bitsize / 8) as i32).unwrap();
            ra.asm.cmp(scratch, lo as i64).unwrap();
            ra.asm.jbe(&ok, rxbyak::JmpType::Near).unwrap();

            if trap_nonzero {
                let addr = RegExp::from(rxbyak::R13) + lo as i32;
                match bitsize {
                    8 => ra.asm.cmp(byte_ptr(addr), 0i32).unwrap(),
                    16 => ra.asm.cmp(word_ptr(addr), 0i32).unwrap(),
                    32 => ra.asm.cmp(dword_ptr(addr), 0i32).unwrap(),
                    64 => ra.asm.cmp(qword_ptr(addr), 0i32).unwrap(),
                    _ => unreachable!(),
                }
                ra.asm.je(&ok, rxbyak::JmpType::Near).unwrap();
            }

            if let Some(expected) = trap_value {
                let addr = RegExp::from(rxbyak::R13) + lo as i32;
                match bitsize {
                    8 => {
                        ra.asm.mov(scratch.cvt32().unwrap(), 0i32).unwrap();
                        ra.asm
                            .mov(scratch.cvt8().unwrap(), (expected & 0xFF) as i32)
                            .unwrap();
                        ra.asm.cmp(byte_ptr(addr), scratch.cvt8().unwrap()).unwrap();
                    }
                    16 => {
                        ra.asm
                            .mov(scratch.cvt32().unwrap(), (expected & 0xFFFF) as i32)
                            .unwrap();
                        ra.asm
                            .cmp(word_ptr(addr), scratch.cvt16().unwrap())
                            .unwrap();
                    }
                    32 => {
                        ra.asm
                            .mov(scratch.cvt32().unwrap(), expected as i32)
                            .unwrap();
                        ra.asm
                            .cmp(dword_ptr(addr), scratch.cvt32().unwrap())
                            .unwrap();
                    }
                    64 => {
                        ra.asm.mov(scratch, expected as i64).unwrap();
                        ra.asm.cmp(qword_ptr(addr), scratch).unwrap();
                    }
                    _ => unreachable!(),
                }
                ra.asm.jne(&ok, rxbyak::JmpType::Near).unwrap();
            }

            let sentinel: u32 = 0xCAFE_F000 | (bitsize as u32);
            ra.asm.mov(scratch, sentinel as i32).unwrap();
            ra.asm.ud2().unwrap();

            ra.asm.bind(&ok).unwrap();
            ra.asm.popf().unwrap();
            ra.asm.pop(scratch).unwrap();
            ra.asm.pop(vaddr_reg).unwrap();
        }

        if let Some((lo, hi)) = parse_trace_fastmem_write_range_env() {
            let skip = ra.asm.create_label();
            let vaddr_reg = rxbyak::Reg::gpr32(vaddr_idx);

            ra.asm.cmp(vaddr_reg, lo as u32 as i32).unwrap();
            ra.asm.jb(&skip, rxbyak::JmpType::Near).unwrap();
            ra.asm.cmp(vaddr_reg, hi as u32 as i32).unwrap();
            ra.asm.jae(&skip, rxbyak::JmpType::Near).unwrap();

            emit_preserved_a32_fastmem_write_trace_hook(ctx, ra, vaddr_idx, value_idx, bitsize);
            ra.asm.bind(&skip).unwrap();
        }

        let resume_offset = ra.asm.size();
        let fallbacks = unsafe {
            &*(ctx
                .fastmem_fallbacks
                .expect("A32 fastmem path requires fallback table")
                as *const FastmemFallbacksTable)
        };
        let wrapped_fn_off = fallbacks.write_stub(ordered, bitsize, vaddr_idx, value_idx);
        let marker = (ctx.location, inst_ref.0);
        let recompile = ctx.config.memory.recompile_on_fastmem_failure;
        ctx.deferred_emits.borrow_mut().push(Box::new(move |dctx| {
            dctx.fastmem_patches.add(
                dctx.code_base + inst_offset as u64,
                crate::backend::x64::exception_handler::FastmemPatchInfo::new(
                    dctx.code_base + resume_offset as u64,
                    dctx.code_base + wrapped_fn_off as u64,
                    Some(marker),
                    recompile,
                ),
            );
        }));
        return;
    }

    if ctx.config.memory.page_table_present && !force_callback {
        let vaddr = ra.use_gpr(&mut args[1]);
        let value = if ordered {
            ra.use_scratch_gpr(&mut args[2])
        } else {
            ra.use_gpr(&mut args[2])
        };
        let vaddr_idx = vaddr.get_idx();
        let value_idx = value.get_idx();
        let fallbacks = unsafe {
            &*(ctx
                .fastmem_fallbacks
                .expect("A32 page-table path requires fallback table")
                as *const FastmemFallbacksTable)
        };
        let wrapped_fn_off = fallbacks.write_stub(ordered, bitsize, vaddr_idx, value_idx);
        let abort = ra.asm.create_label();
        let end = ra.asm.create_label();
        let addr = emit_vaddr_lookup_a32(ra, ctx, bitsize, abort, vaddr);
        emit_bitsize_write_mov(ra, bitsize, addr, value_idx, ordered);

        ctx.deferred_emits.borrow_mut().push(Box::new(move |dctx| {
            let asm = &mut *dctx.asm;
            asm.bind(&abort).unwrap();
            emit_call_to_offset(asm, wrapped_fn_off);
            asm.jmp(&end, JmpType::Near).unwrap();
        }));
        ra.asm.bind(&end).unwrap();
        return;
    }

    // Callback path (slow)
    let (first, rest) = args.split_at_mut(2);
    ra.host_call(
        None,
        &mut [None, Some(&mut first[1]), Some(&mut rest[0]), None],
    );
    let callback = match bitsize {
        8 => &ctx.config.callbacks.memory_write_8,
        16 => &ctx.config.callbacks.memory_write_16,
        32 => &ctx.config.callbacks.memory_write_32,
        64 => &ctx.config.callbacks.memory_write_64,
        _ => unreachable!(),
    };
    callback.emit_call_simple(&mut *ra.asm).unwrap();
    if ordered {
        // Drain the store buffer after the slow-path write so subsequent
        // memory ops observe it. Mirrors upstream `EmitMemoryWrite<bitsize>`
        // ordered path in `emit_x64_memory.cpp.inc:159-161`.
        ra.asm.mfence().unwrap();
    }
}

pub fn emit_a32_read_memory_8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_memory_read(ctx, ra, inst_ref, inst, 8);
}
pub fn emit_a32_read_memory_16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_memory_read(ctx, ra, inst_ref, inst, 16);
}
pub fn emit_a32_read_memory_32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_memory_read(ctx, ra, inst_ref, inst, 32);
}
pub fn emit_a32_read_memory_64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_memory_read(ctx, ra, inst_ref, inst, 64);
}

pub fn emit_a32_write_memory_8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_memory_write(ctx, ra, inst_ref, inst, 8);
}
pub fn emit_a32_write_memory_16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_memory_write(ctx, ra, inst_ref, inst, 16);
}
pub fn emit_a32_write_memory_32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_memory_write(ctx, ra, inst_ref, inst, 32);
}
pub fn emit_a32_write_memory_64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_memory_write(ctx, ra, inst_ref, inst, 64);
}

// ---------------------------------------------------------------------------
// Exclusive memory operations
// ---------------------------------------------------------------------------

fn emit_a32_exclusive_read(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    bitsize: usize,
) {
    // Inline fast path: monitor + fastmem both configured. Mirrors upstream
    // `EmitExclusiveReadMemoryInline` (emit_x64_memory.cpp.inc:334-408).
    // Without this, every LDREX/LDAEX takes the trampoline path which costs
    // a virtual callback + SpinLock + page-table-walk per instruction —
    // dominant CPU hotspot for ARM32 binaries using userspace mutexes.
    if should_fastmem(ctx, inst_ref)
        && ctx.config.global_monitor.is_some()
        && std::env::var_os("RUZU_NO_EXCLUSIVE_INLINE").is_none()
    {
        emit_a32_exclusive_read_inline(ctx, ra, inst_ref, inst, bitsize);
        return;
    }

    // Set exclusive_state = 1
    let excl_offset = A32JitState::offset_of_exclusive_state();
    ra.asm
        .mov(dword_ptr(RegExp::from(R15) + excl_offset as i32), 1i32)
        .unwrap();

    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    // args[2] is the AccType immediate. LDAEX uses Ordered.
    let ordered = is_ordered(args[2].value.get_acc_type());
    // args[0] = location descriptor (upper), args[1] = vaddr, args[2] = acc_type
    // ArgCallback: position 0 = None (context), position 1 = vaddr
    ra.host_call(Some(inst_ref), &mut [None, Some(&mut args[1]), None, None]);
    if ordered {
        // Drain pending stores before the exclusive load. Mirrors upstream
        // `EmitExclusiveReadMemory<bitsize>` ordered path in
        // `emit_x64_memory.cpp.inc:235-237`.
        ra.asm.mfence().unwrap();
    }

    let callback = match bitsize {
        8 => &ctx.config.callbacks.exclusive_read_8,
        16 => &ctx.config.callbacks.exclusive_read_16,
        32 => &ctx.config.callbacks.exclusive_read_32,
        64 => &ctx.config.callbacks.exclusive_read_64,
        _ => unreachable!(),
    };
    callback.emit_call_simple(&mut *ra.asm).unwrap();
}

/// Inline LDREX/LDAEX emit — upstream-faithful port of
/// `EmitExclusiveReadMemoryInline`.
///
/// Layout (with `pid` = `ctx.config.memory.processor_id`):
///
/// 1. Take monitor spin lock.
/// 2. `JitState.exclusive_state = 1`.
/// 3. `monitor.exclusive_addresses[pid] = vaddr`.
/// 4. Inline fastmem read: `mov result, [r13 + vaddr]` (with sub-32-bit
///    `movzx` so the value reg always holds the zero-extended 64-bit
///    result). The mov is registered as a FastmemEntry so the SIGSEGV
///    handler can patch it to the slow callback path on fault.
/// 5. `monitor.exclusive_values[pid] = result`.
/// 6. Release monitor spin lock.
fn emit_a32_exclusive_read_inline(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    bitsize: usize,
) {
    use crate::backend::x64::emit_exclusive_memory::{
        emit_exclusive_lock, emit_exclusive_unlock, exclusive_address_ptr, exclusive_value_ptr,
    };

    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let pid = ctx.config.memory.processor_id;
    let addr_ptr = exclusive_address_ptr(ctx, pid).expect("global_monitor checked Some by caller");
    let value_ptr = exclusive_value_ptr(ctx, pid).expect("global_monitor checked Some by caller");

    // Reserve registers up-front so reg_alloc spills cleanly before any
    // emit; mirrors upstream's `ScratchGpr` calls in
    // `EmitExclusiveReadMemoryInline`.
    let vaddr = ra.use_gpr(&mut args[1]);
    let result = ra.scratch_gpr();
    let tmp = ra.scratch_gpr();
    let tmp2 = ra.scratch_gpr();

    // Take monitor lock (jmp/pause spin via `lock xchg [lock_storage], tmp2`).
    let locked = emit_exclusive_lock(ctx, &mut *ra.asm, tmp, tmp2.cvt32().unwrap());

    // Set exclusive_state = 1.
    let excl_state_off = A32JitState::offset_of_exclusive_state();
    ra.asm
        .mov(dword_ptr(RegExp::from(R15) + excl_state_off as i32), 1i32)
        .unwrap();

    // monitor.exclusive_addresses[pid] = vaddr (mov reg, imm64; then qword[reg], vaddr).
    ra.asm.mov(tmp, addr_ptr as u64 as i64).unwrap();
    ra.asm.mov(qword_ptr(RegExp::from(tmp)), vaddr).unwrap();

    // Inline fastmem read at [r13 + vaddr]. The exact opcode depends on
    // bitsize; for ordered LDA/LDAEX the existing memory_read path uses
    // `lock xadd [mem], reg` with reg pre-zeroed (atomic load + barrier).
    // Mirror that here so memory ordering matches non-exclusive ordered
    // reads. inst_offset is captured for the SIGSEGV-handler patch.
    let addr_expr = RegExp::from(rxbyak::R13) + vaddr;
    let inst_offset = ra.asm.size();
    match bitsize {
        8 => {
            ra.asm
                .movzx(result.cvt32().unwrap(), byte_ptr(addr_expr))
                .unwrap();
        }
        16 => {
            ra.asm
                .movzx(result.cvt32().unwrap(), word_ptr(addr_expr))
                .unwrap();
        }
        32 => {
            ra.asm
                .mov(result.cvt32().unwrap(), dword_ptr(addr_expr))
                .unwrap();
        }
        64 => {
            ra.asm.mov(result, qword_ptr(addr_expr)).unwrap();
        }
        _ => unreachable!("emit_a32_exclusive_read_inline: bitsize {}", bitsize),
    }
    let resume_offset = ra.asm.size();
    let vaddr_idx = vaddr.get_idx();
    let result_idx = result.get_idx();
    ctx.fastmem_entries
        .borrow_mut()
        .push(crate::backend::x64::emit_context::FastmemEntry {
            inst_offset,
            resume_offset,
            bitsize,
            is_write: false,
            is_exclusive: true,
            ordered: false,
            vaddr_reg: vaddr_idx,
            value_reg: result_idx,
            marker: (ctx.location, inst_ref.0),
            recompile: ctx.config.memory.recompile_on_exclusive_fastmem_failure,
        });

    // monitor.exclusive_values[pid] = result (low qword; upper qword left
    // untouched — A32 LDREX{,H,B} write at most 4 bytes; LDREXD writes 8).
    ra.asm.mov(tmp, value_ptr as u64 as i64).unwrap();
    ra.asm.mov(qword_ptr(RegExp::from(tmp)), result).unwrap();

    // Release monitor lock.
    if locked {
        emit_exclusive_unlock(ctx, &mut *ra.asm, tmp, tmp2.cvt32().unwrap());
    }

    ra.define_value(inst_ref, result);
}

fn emit_a32_exclusive_write(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    bitsize: usize,
) {
    // Inline fast path — see `emit_a32_exclusive_write_inline` doc comment.
    if should_fastmem(ctx, inst_ref)
        && ctx.config.global_monitor.is_some()
        && std::env::var_os("RUZU_NO_EXCLUSIVE_INLINE").is_none()
    {
        emit_a32_exclusive_write_inline(ctx, ra, inst_ref, inst, bitsize);
        return;
    }

    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    // args[3] is the AccType immediate. STLEX uses Ordered.
    let ordered = is_ordered(args[3].value.get_acc_type());
    // args[0] = location descriptor (upper), args[1] = vaddr, args[2] = value, args[3] = acc_type
    // ArgCallback: position 0 = None (context), position 1 = vaddr, position 2 = value
    let (first, rest) = args.split_at_mut(2);
    ra.host_call(
        Some(inst_ref),
        &mut [None, Some(&mut first[1]), Some(&mut rest[0]), None],
    );

    let callback = match bitsize {
        8 => &ctx.config.callbacks.exclusive_write_8,
        16 => &ctx.config.callbacks.exclusive_write_16,
        32 => &ctx.config.callbacks.exclusive_write_32,
        64 => &ctx.config.callbacks.exclusive_write_64,
        _ => unreachable!(),
    };
    callback.emit_call_simple(&mut *ra.asm).unwrap();
    if ordered {
        // Drain the store buffer after the exclusive write. Mirrors upstream
        // `EmitExclusiveWriteMemory<bitsize>` ordered path in
        // `emit_x64_memory.cpp.inc:307-309`.
        ra.asm.mfence().unwrap();
    }

    // Clear exclusive_state
    let excl_offset = A32JitState::offset_of_exclusive_state();
    ra.asm
        .mov(dword_ptr(RegExp::from(R15) + excl_offset as i32), 0i32)
        .unwrap();
}

/// Inline STREX/STLEX emit — upstream-faithful port of
/// `EmitExclusiveWriteMemoryInline`.
///
/// Layout (with `pid` = `ctx.config.memory.processor_id`):
///
/// 1. Force RAX as scratch (cmpxchg's implicit expected-value register).
/// 2. Take monitor spin lock.
/// 3. status = 1 (assume failure).
/// 4. If `JitState.exclusive_state == 0` → status stays 1, jump to end.
/// 5. If `monitor.exclusive_addresses[pid] != vaddr` → status stays 1,
///    jump to end.
/// 6. Clear other processors' reservations matching `vaddr` (so a
///    concurrent STREX on another core sees its reservation invalidated).
/// 7. `JitState.exclusive_state = 0` (we're consuming the reservation).
/// 8. Load the saved exclusive value into RAX from
///    `monitor.exclusive_values[pid]` (low qword for bitsize <= 64).
/// 9. Inline fastmem `lock cmpxchg [r13+vaddr], value`. If the memory
///    still holds RAX (= saved value), atomically replace with `value`
///    and ZF=1; otherwise ZF=0.
/// 10. status = ZF inverted (`setnz status`) — 0 on success, 1 on failure.
/// 11. Release monitor spin lock.
/// 12. end: result is status (defined to the IR inst).
fn emit_a32_exclusive_write_inline(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    bitsize: usize,
) {
    use crate::backend::x64::emit_exclusive_memory::{
        emit_exclusive_lock, emit_exclusive_test_and_clear, emit_exclusive_unlock,
        exclusive_address_ptr, exclusive_value_ptr,
    };
    use crate::backend::x64::hostloc::HOST_RAX;

    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let pid = ctx.config.memory.processor_id;
    let addr_ptr = exclusive_address_ptr(ctx, pid).expect("global_monitor checked Some by caller");
    let value_ptr = exclusive_value_ptr(ctx, pid).expect("global_monitor checked Some by caller");

    // Force-reserve RAX up front; cmpxchg implicitly uses RAX as its
    // "expected" value, and we'll also use RAX as the spin-lock scratch
    // (eax in upstream). Holding it here prevents reg_alloc from giving
    // RAX to vaddr / value / status / tmp.
    let _rax_reservation = ra.scratch_gpr_at(HOST_RAX);

    let value = ra.use_gpr(&mut args[2]);
    let vaddr = ra.use_gpr(&mut args[1]);
    let status = ra.scratch_gpr();
    let tmp = ra.scratch_gpr();

    let end = ra.asm.create_label();

    // Take monitor spin lock (clobbers RAX as scratch, which is fine since
    // we restore it below before cmpxchg).
    let locked = emit_exclusive_lock(ctx, &mut *ra.asm, tmp, _rax_reservation.cvt32().unwrap());

    // status = 1 (failure by default).
    ra.asm.mov(status.cvt32().unwrap(), 1u32).unwrap();

    // If exclusive_state == 0 → jump to end (no reservation to consume).
    let excl_state_off = A32JitState::offset_of_exclusive_state();
    ra.asm
        .cmp(dword_ptr(RegExp::from(R15) + excl_state_off as i32), 0i32)
        .unwrap();
    ra.asm.je(&end, rxbyak::JmpType::Near).unwrap();

    // If monitor.exclusive_addresses[pid] != vaddr → jump to end.
    ra.asm.mov(tmp, addr_ptr as u64 as i64).unwrap();
    ra.asm.cmp(qword_ptr(RegExp::from(tmp)), vaddr).unwrap();
    ra.asm.jne(&end, rxbyak::JmpType::Near).unwrap();

    // Clear OTHER processors' reservations matching vaddr.
    emit_exclusive_test_and_clear(ctx, &mut *ra.asm, vaddr, tmp, _rax_reservation);

    // Clear our exclusive_state (we're consuming the reservation).
    ra.asm
        .mov(dword_ptr(RegExp::from(R15) + excl_state_off as i32), 0i32)
        .unwrap();

    // Load saved exclusive value into RAX from monitor.exclusive_values[pid].
    ra.asm.mov(tmp, value_ptr as u64 as i64).unwrap();
    ra.asm
        .mov(_rax_reservation, qword_ptr(RegExp::from(tmp)))
        .unwrap();

    // Inline fastmem `lock cmpxchg [r13+vaddr], value` (`mem = value` if
    // `mem == RAX`, ZF=1; else ZF=0 and `RAX = mem`).
    let addr_expr = RegExp::from(rxbyak::R13) + vaddr;
    let inst_offset = ra.asm.size();
    ra.asm.lock().unwrap();
    match bitsize {
        8 => {
            ra.asm
                .cmpxchg(byte_ptr(addr_expr), value.cvt8().unwrap())
                .unwrap();
        }
        16 => {
            ra.asm
                .cmpxchg(word_ptr(addr_expr), value.cvt16().unwrap())
                .unwrap();
        }
        32 => {
            ra.asm
                .cmpxchg(dword_ptr(addr_expr), value.cvt32().unwrap())
                .unwrap();
        }
        64 => {
            ra.asm.cmpxchg(qword_ptr(addr_expr), value).unwrap();
        }
        _ => unreachable!("emit_a32_exclusive_write_inline: bitsize {}", bitsize),
    }
    let resume_offset = ra.asm.size();
    let vaddr_idx = vaddr.get_idx();
    let value_idx = value.get_idx();
    ctx.fastmem_entries
        .borrow_mut()
        .push(crate::backend::x64::emit_context::FastmemEntry {
            inst_offset,
            resume_offset,
            bitsize,
            is_write: true,
            is_exclusive: true,
            ordered: false,
            vaddr_reg: vaddr_idx,
            value_reg: value_idx,
            marker: (ctx.location, inst_ref.0),
            recompile: ctx.config.memory.recompile_on_exclusive_fastmem_failure,
        });

    // status = !ZF (1 on cmpxchg failure / ZF=0, 0 on success / ZF=1).
    ra.asm.setnz(status.cvt8().unwrap()).unwrap();
    ra.asm
        .movzx(status.cvt32().unwrap(), status.cvt8().unwrap())
        .unwrap();

    // BUG FIX: `end:` must come BEFORE the unlock so that the early-exit
    // `je`/`jne` branches above (which target `end`) still release the spin
    // lock. Mirrors upstream `emit_x64_memory.cpp.inc:533-535` exactly:
    //
    //     code.L(*end);
    //     EmitExclusiveUnlock(code, conf, tmp, eax);
    //
    // The earlier version put `unlock` before `bind(end)` — every failed
    // reservation check jumped past the unlock, leaving the global monitor
    // ~30× fewer SVCs when write-inline was on.
    ra.asm.bind(&end).unwrap();

    // Release monitor spin lock.
    if locked {
        emit_exclusive_unlock(ctx, &mut *ra.asm, tmp, _rax_reservation.cvt32().unwrap());
    }

    ra.define_value(inst_ref, status);
}

pub fn emit_a32_exclusive_read_memory_8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_exclusive_read(ctx, ra, inst_ref, inst, 8);
}
pub fn emit_a32_exclusive_read_memory_16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_exclusive_read(ctx, ra, inst_ref, inst, 16);
}
pub fn emit_a32_exclusive_read_memory_32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_exclusive_read(ctx, ra, inst_ref, inst, 32);
}
pub fn emit_a32_exclusive_read_memory_64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_exclusive_read(ctx, ra, inst_ref, inst, 64);
}

pub fn emit_a32_exclusive_write_memory_8(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_exclusive_write(ctx, ra, inst_ref, inst, 8);
}
pub fn emit_a32_exclusive_write_memory_16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_exclusive_write(ctx, ra, inst_ref, inst, 16);
}
pub fn emit_a32_exclusive_write_memory_32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_exclusive_write(ctx, ra, inst_ref, inst, 32);
}
pub fn emit_a32_exclusive_write_memory_64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_a32_exclusive_write(ctx, ra, inst_ref, inst, 64);
}

/// A32ClearExclusive: clear exclusive monitor
pub fn emit_a32_clear_exclusive(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    _inst_ref: InstRef,
    _inst: &Inst,
) {
    let excl_offset = A32JitState::offset_of_exclusive_state();
    ra.asm
        .mov(dword_ptr(RegExp::from(R15) + excl_offset as i32), 0i32)
        .unwrap();
}

// ---------------------------------------------------------------------------
// Coprocessor operations — CP15 (system control) with TPIDR support
// ---------------------------------------------------------------------------

/// Unpack coproc_info u64 into its fields.
/// Layout: [0]=coproc_no, [1]=two, [2]=opc1, [3]=CRn, [4]=CRm, [6]=opc2
fn unpack_coproc_info(info: u64) -> (u8, u8, u8, u8, u8, u8) {
    let coproc_no = (info & 0xFF) as u8;
    let two = ((info >> 8) & 0xFF) as u8;
    let opc1 = ((info >> 16) & 0xFF) as u8;
    let crn = ((info >> 24) & 0xFF) as u8;
    let crm = ((info >> 32) & 0xFF) as u8;
    let opc2 = ((info >> 48) & 0xFF) as u8;
    (coproc_no, two, opc1, crn, crm, opc2)
}

pub fn emit_a32_coproc_internal_operation(
    _ctx: &EmitContext,
    _ra: &mut RegAlloc,
    _inst_ref: InstRef,
    inst: &Inst,
) {
    // CDP: coprocessor data processing. For CP15, most operations are no-ops
    // (memory barriers are handled by separate DMB/DSB/ISB instructions).
    let info = inst.args[0].get_coproc_info();
    let (coproc_no, _, _, crn, _, _) = unpack_coproc_info(info);

    if coproc_no == 15 {
        match crn {
            7 => {
                // CP15 C7: cache/barrier operations — no-op on x86 (strong memory model)
            }
            _ => {
                // Other CDP to CP15 — no-op
            }
        }
    }
    // Non-CP15 CDP: silently ignore (better than crashing)
}

pub fn emit_a32_coproc_send_one_word(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    _inst_ref: InstRef,
    inst: &Inst,
) {
    // MCR: write CPU register to coprocessor register.
    let info = inst.args[0].get_coproc_info();
    let (coproc_no, two, opc1, crn, crm, opc2) = unpack_coproc_info(info);

    if coproc_no != 15 {
        return;
    }

    if two == 0 && opc1 == 0 && crn == 7 && crm == 5 && opc2 == 4 {
        // CP15_FLUSH_PREFETCH_BUFFER: dummy write, ignore the source value.
        return;
    }

    if two == 0 && opc1 == 0 && crn == 7 && crm == 10 {
        match opc2 {
            // CP15_DATA_SYNC_BARRIER
            4 => {
                ra.asm.mfence().unwrap();
                ra.asm.lfence().unwrap();
                return;
            }
            // CP15_DATA_MEMORY_BARRIER
            5 => {
                ra.asm.mfence().unwrap();
                return;
            }
            _ => {}
        }
    }

    if two == 0 && opc1 == 0 && crn == 13 && crm == 0 && opc2 == 2 {
        // CP15_THREAD_UPRW
        let mut args = ra.get_argument_info(_inst_ref, &inst.args, inst.num_args());
        let offset = A32JitState::offset_of_cp15_uprw();
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
}

pub fn emit_a32_coproc_send_two_words(
    _ctx: &EmitContext,
    _ra: &mut RegAlloc,
    _inst_ref: InstRef,
    _inst: &Inst,
) {
    // MCRR: write two CPU registers to coprocessor — stub (no-op)
}

pub fn emit_a32_coproc_get_one_word(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    // MRC: read from coprocessor register into CPU register.
    let info = inst.args[0].get_coproc_info();
    let (coproc_no, _, _opc1, crn, crm, opc2) = unpack_coproc_info(info);

    let result = ra.scratch_gpr();

    if coproc_no == 15 {
        match (crn, crm, opc2) {
            // MRC p15, 0, Rt, c13, c0, 2 — read TPIDR_UPRW
            (13, 0, 2) => {
                let offset = A32JitState::offset_of_cp15_uprw();
                ra.asm
                    .mov(
                        result.cvt32().unwrap(),
                        dword_ptr(RegExp::from(R15) + offset as i32),
                    )
                    .unwrap();
            }
            // MRC p15, 0, Rt, c13, c0, 3 — read TPIDR_URO
            (13, 0, 3) => {
                let offset = A32JitState::offset_of_cp15_uro();
                ra.asm
                    .mov(
                        result.cvt32().unwrap(),
                        dword_ptr(RegExp::from(R15) + offset as i32),
                    )
                    .unwrap();
            }
            _ => {
                // Other MRC from CP15 — return 0
                ra.asm
                    .xor_(result.cvt32().unwrap(), result.cvt32().unwrap())
                    .unwrap();
            }
        }
    } else {
        // Non-CP15 MRC — return 0
        ra.asm
            .xor_(result.cvt32().unwrap(), result.cvt32().unwrap())
            .unwrap();
    }

    ra.define_value(inst_ref, result);
}

pub fn emit_a32_coproc_get_two_words(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    // MRRC: read two words (64-bit value) from coprocessor.
    let info = inst.args[0].get_coproc_info();
    let (coproc_no, _, opc, _crn, crm, _opc2) = unpack_coproc_info(info);

    if coproc_no == 15 && opc == 0 && crm == 14 {
        // MRRC p15, 0, Rt, Rt2, c14 — CNTPCT (Physical Count Timer).
        // Upstream (zuyu DynarmicCP15::CompileGetTwoWords) returns a Callback
        // that invokes CoreTiming::GetClockTicks() at every call; matching that,
        // we emit a host call to the get_cntpct callback which returns the live
        // tick count. Reading a JitState field would return a stale value since
        // the host only writes `cntpct` sporadically (not per-block).
        ra.host_call(Some(inst_ref), &mut [None, None, None, None]);
        ctx.config
            .callbacks
            .get_cntpct
            .emit_call_simple(&mut *ra.asm)
            .unwrap();
    } else {
        // Other MRRC — return 0.
        let result = ra.scratch_gpr();
        ra.asm
            .xor_(result.cvt32().unwrap(), result.cvt32().unwrap())
            .unwrap();
        ra.define_value(inst_ref, result);
    }
}

pub fn emit_a32_coproc_load_words(
    _ctx: &EmitContext,
    _ra: &mut RegAlloc,
    _inst_ref: InstRef,
    _inst: &Inst,
) {
    // LDC: load to coprocessor from memory — stub (no-op)
}

pub fn emit_a32_coproc_store_words(
    _ctx: &EmitContext,
    _ra: &mut RegAlloc,
    _inst_ref: InstRef,
    _inst: &Inst,
) {
    // STC: store from coprocessor to memory — stub (no-op)
}

#[cfg(test)]
mod tests {
    use super::{a32_bx_upper_without_t, emit_a32_coproc_send_one_word};
    use crate::backend::x64::callback::Callback;
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
        ctx.arch = ArchConfig::A32;
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
            | ((opc2 as u64) << 48)
    }

    fn emit_cp15_send_one_word(info: u64) -> Vec<u8> {
        let config = dummy_emit_config();
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
    fn cp15_legacy_memory_barriers_match_host_fences() {
        let dsb = emit_cp15_send_one_word(coproc_info(15, false, 0, 7, 10, 4));
        let mut expected_dsb = rxbyak::CodeAssembler::new(4096).unwrap();
        expected_dsb.mfence().unwrap();
        expected_dsb.lfence().unwrap();
        assert_eq!(dsb, expected_dsb.code());

        let dmb = emit_cp15_send_one_word(coproc_info(15, false, 0, 7, 10, 5));
        let mut expected_dmb = rxbyak::CodeAssembler::new(4096).unwrap();
        expected_dmb.mfence().unwrap();
        assert_eq!(dmb, expected_dmb.code());

        assert!(emit_cp15_send_one_word(coproc_info(15, true, 0, 7, 10, 4)).is_empty());
        assert!(emit_cp15_send_one_word(coproc_info(15, false, 1, 7, 10, 5)).is_empty());
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

        let mut asm = rxbyak::CodeAssembler::new(4096).unwrap();
        let mut gpr_order = ANY_GPR.to_vec();
        gpr_order.retain(|&loc| loc != HOST_R13);
        let mut ra = RegAlloc::new(&mut asm, gpr_order, ANY_XMM.to_vec(), inst_info);

        let mut ctx = EmitContext::new(location, &config);
        ctx.arch = ArchConfig::A32;
        ctx.fastmem_available = true;
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
        ctx.arch = ArchConfig::A32;
        ctx.block = Some(&block);
        ctx.end_location = Some(block.end_location());

        emit_block(&ctx, &mut ra, &block);
    }
}
