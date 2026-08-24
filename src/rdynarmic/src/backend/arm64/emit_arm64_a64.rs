//! A64 terminal emission for the ARM64 host backend.
//!
//! Upstream owner: `backend/arm64/emit_arm64_a64.cpp`.

use crate::backend::arm64::abi::{XHALT, XSCRATCH0, XSCRATCH1, XSCRATCH2, XSTATE, XTICKS};
use crate::backend::arm64::block_of_code::BlockOfCode;
use crate::backend::arm64::emit_arm64::{
    emit_block_link_relocation, emit_relocation, BlockRelocationType, LinkTarget,
};
use crate::backend::arm64::emit_context::EmitContext;
use crate::backend::arm64::inst;
use crate::backend::arm64::jit_state::A64JitState;
use crate::backend::arm64::label::Label;
use crate::backend::arm64::stack_layout::{RSBEntry, StackLayout, RSB_INDEX_MASK};
use crate::halt_reason::HaltReason;
use crate::ir::cond::Cond;
use crate::ir::location::{A64LocationDescriptor, LocationDescriptor};
use crate::ir::terminal::Terminal;
use crate::ir::value::InstRef;
use crate::jit_config::OptimizationFlag;

const X0: u8 = 0;
const X1: u8 = 1;
const X2: u8 = 2;

pub fn emit_a64_terminal(code: &mut BlockOfCode, ctx: &mut EmitContext<'_>) -> Result<(), String> {
    let location = A64LocationDescriptor::from_location(ctx.block.location);
    emit_a64_terminal_inner(
        code,
        ctx,
        ctx.block.terminal.clone(),
        location.set_single_stepping(false).to_location(),
        location.single_stepping(),
    )
}

pub fn emit_a64_condition_failed_terminal(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
) -> Result<(), String> {
    let location = A64LocationDescriptor::from_location(ctx.block.location);
    let Some(condition_failed_location) = ctx.block.condition_failed_location else {
        return Err("A64 condition-failed terminal requested without location".to_string());
    };
    emit_a64_terminal_inner(
        code,
        ctx,
        Terminal::LinkBlock {
            next: condition_failed_location,
        },
        location.set_single_stepping(false).to_location(),
        location.single_stepping(),
    )
}

pub(crate) fn emit_a64_check_memory_abort(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    current_location: LocationDescriptor,
    end: &mut Label,
) -> Result<(), String> {
    if !ctx.conf.check_halt_on_memory_access {
        return Ok(());
    }

    let current_location = A64LocationDescriptor::from_location(current_location);
    code.write_u32(inst::ldar_x(XSCRATCH0, XHALT))?;
    code.write_u32(inst::tst_x_imm(
        XSCRATCH0,
        HaltReason::MEMORY_ABORT.bits() as u64,
    ))?;
    end.b_cond(code, Cond::EQ)?;
    emit_mov_x_imm(code, XSCRATCH0, current_location.pc())?;
    code.write_u32(inst::str_x_unsigned(
        XSCRATCH0,
        XSTATE,
        core::mem::offset_of!(A64JitState, pc) as u32,
    ))?;
    emit_relocation(code, ctx.emitted_block_info, LinkTarget::ReturnFromRunCode)
}

pub fn emit_a64_call_supervisor(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    ctx.reg_alloc
        .prepare_for_call(code, ctx.fpsr, [None, None, None, None])?;

    if ctx.conf.enable_cycle_counting {
        code.write_u32(inst::ldr_x_unsigned(
            X1,
            31,
            StackLayout::cycles_to_run_offset() as u32,
        ))?;
        code.write_u32(inst::sub_x_reg(X1, X1, XTICKS))?;
        emit_relocation(code, ctx.emitted_block_info, LinkTarget::AddTicks)?;
    }

    emit_mov_w_imm(code, X1, args[0].get_immediate_u32())?;
    emit_relocation(code, ctx.emitted_block_info, LinkTarget::CallSVC)?;

    if ctx.conf.enable_cycle_counting {
        emit_relocation(code, ctx.emitted_block_info, LinkTarget::GetTicksRemaining)?;
        code.write_u32(inst::str_x_unsigned(
            X0,
            31,
            StackLayout::cycles_to_run_offset() as u32,
        ))?;
        code.write_u32(inst::mov_x(XTICKS, X0))?;
    }

    Ok(())
}

pub fn emit_a64_exception_raised(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    ctx.reg_alloc
        .prepare_for_call(code, ctx.fpsr, [None, None, None, None])?;

    if ctx.conf.enable_cycle_counting {
        code.write_u32(inst::ldr_x_unsigned(
            X1,
            31,
            StackLayout::cycles_to_run_offset() as u32,
        ))?;
        code.write_u32(inst::sub_x_reg(X1, X1, XTICKS))?;
        emit_relocation(code, ctx.emitted_block_info, LinkTarget::AddTicks)?;
    }

    emit_mov_x_imm(code, X1, args[0].get_immediate_u64())?;
    emit_mov_x_imm(code, X2, args[1].get_immediate_u64())?;
    emit_relocation(code, ctx.emitted_block_info, LinkTarget::ExceptionRaised)?;

    if ctx.conf.enable_cycle_counting {
        emit_relocation(code, ctx.emitted_block_info, LinkTarget::GetTicksRemaining)?;
        code.write_u32(inst::str_x_unsigned(
            X0,
            31,
            StackLayout::cycles_to_run_offset() as u32,
        ))?;
        code.write_u32(inst::mov_x(XTICKS, X0))?;
    }

    Ok(())
}

pub fn emit_a64_data_cache_operation_raised(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    ctx.reg_alloc
        .prepare_for_call(code, ctx.fpsr, [None, Some(args[1]), Some(args[2]), None])?;
    emit_relocation(
        code,
        ctx.emitted_block_info,
        LinkTarget::DataCacheOperationRaised,
    )
}

pub fn emit_a64_instruction_cache_operation_raised(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    ctx.reg_alloc
        .prepare_for_call(code, ctx.fpsr, [None, Some(args[0]), Some(args[1]), None])?;
    emit_relocation(
        code,
        ctx.emitted_block_info,
        LinkTarget::InstructionCacheOperationRaised,
    )
}

fn emit_a64_terminal_inner(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    terminal: Terminal,
    _initial_location: LocationDescriptor,
    is_single_step: bool,
) -> Result<(), String> {
    match terminal {
        Terminal::Invalid => Err("Invalid A64 terminal".to_string()),
        Terminal::ReturnToDispatch => {
            emit_relocation(code, ctx.emitted_block_info, LinkTarget::ReturnToDispatcher)
        }
        Terminal::LinkBlock { next } => {
            if ctx.conf.has_optimization(OptimizationFlag::BLOCK_LINKING) && !is_single_step {
                emit_guarded_block_link_relocation(code, ctx, next)?;
            }
            emit_set_pc_and_return_to_dispatcher(code, ctx, next)
        }
        Terminal::LinkBlockFast { next } => {
            if ctx.conf.has_optimization(OptimizationFlag::BLOCK_LINKING) && !is_single_step {
                emit_block_link_relocation(
                    code,
                    ctx.emitted_block_info,
                    next,
                    BlockRelocationType::Branch,
                )?;
            }
            emit_set_pc_and_return_to_dispatcher(code, ctx, next)
        }
        Terminal::PopRSBHint => {
            if ctx
                .conf
                .has_optimization(OptimizationFlag::RETURN_STACK_BUFFER)
                && !is_single_step
            {
                emit_pop_rsb_hint(code)?;
            }
            emit_relocation(code, ctx.emitted_block_info, LinkTarget::ReturnToDispatcher)
        }
        Terminal::FastDispatchHint => {
            emit_relocation(code, ctx.emitted_block_info, LinkTarget::ReturnToDispatcher)
        }
        Terminal::If { cond, then_, else_ } => {
            let emit_cond = ctx.conf.emit_cond;
            let pass_branch_offset = emit_cond(code, ctx, cond)?;
            emit_a64_terminal_inner(code, ctx, *else_, _initial_location, is_single_step)?;
            patch_branch_to_current(code, pass_branch_offset, |pc_offset| {
                inst::b_cond(cond, pc_offset)
            })?;
            emit_a64_terminal_inner(code, ctx, *then_, _initial_location, is_single_step)
        }
        Terminal::CheckBit { then_, else_ } => {
            code.write_u32(inst::ldrb_w_unsigned(
                XSCRATCH0,
                31,
                crate::backend::arm64::stack_layout::StackLayout::check_bit_offset() as u32,
            ))?;
            let fail_branch_offset = code.write_u32(inst::cbz_w(XSCRATCH0, 0))?;
            emit_a64_terminal_inner(code, ctx, *then_, _initial_location, is_single_step)?;
            patch_branch_to_current(code, fail_branch_offset, |pc_offset| {
                inst::cbz_w(XSCRATCH0, pc_offset)
            })?;
            emit_a64_terminal_inner(code, ctx, *else_, _initial_location, is_single_step)
        }
        Terminal::CheckHalt { else_ } => {
            code.write_u32(inst::ldar_w(XSCRATCH0, XHALT))?;
            let fail_branch_offset = code.write_u32(inst::cbnz_w(XSCRATCH0, 0))?;
            emit_a64_terminal_inner(code, ctx, *else_, _initial_location, is_single_step)?;
            patch_branch_to_current(code, fail_branch_offset, |pc_offset| {
                inst::cbnz_w(XSCRATCH0, pc_offset)
            })?;
            emit_relocation(code, ctx.emitted_block_info, LinkTarget::ReturnToDispatcher)
        }
    }
}

fn emit_pop_rsb_hint(code: &mut BlockOfCode) -> Result<(), String> {
    emit_mov_w_imm(code, XSCRATCH0, A64LocationDescriptor::FPCR_MASK)?;
    code.write_u32(inst::ldr_w_unsigned(
        X0,
        XSTATE,
        core::mem::offset_of!(A64JitState, fpcr) as u32,
    ))?;
    code.write_u32(inst::ldr_x_unsigned(
        X1,
        XSTATE,
        core::mem::offset_of!(A64JitState, pc) as u32,
    ))?;
    code.write_u32(inst::and_w_reg(X0, X0, XSCRATCH0))?;
    code.write_u32(inst::and_x_imm(X1, X1, A64LocationDescriptor::PC_MASK))?;
    code.write_u32(inst::lsl_x_imm(
        X0,
        X0,
        A64LocationDescriptor::FPCR_SHIFT as u8,
    ))?;
    code.write_u32(inst::orr_x(X0, X0, X1))?;

    code.write_u32(inst::ldr_w_unsigned(
        XSCRATCH2,
        31,
        StackLayout::rsb_ptr_offset() as u32,
    ))?;
    code.write_u32(inst::and_w_imm(XSCRATCH2, XSCRATCH2, RSB_INDEX_MASK as u32))?;
    code.write_u32(inst::add_x_reg_sp(X2, 31, XSCRATCH2))?;
    code.write_u32(inst::sub_w_imm(
        XSCRATCH2,
        XSCRATCH2,
        core::mem::size_of::<RSBEntry>() as u32,
    ))?;
    code.write_u32(inst::str_w_unsigned(
        XSCRATCH2,
        31,
        StackLayout::rsb_ptr_offset() as u32,
    ))?;
    code.write_u32(inst::ldp_x_offset(
        XSCRATCH0,
        XSCRATCH1,
        X2,
        StackLayout::rsb_offset() as i32,
    ))?;

    code.write_u32(inst::cmp_x_reg(X0, XSCRATCH0))?;
    let fail_branch_offset = code.write_u32(inst::b_cond(Cond::NE, 0))?;
    code.write_u32(inst::br(XSCRATCH1))?;
    patch_branch_to_current(code, fail_branch_offset, |pc_offset| {
        inst::b_cond(Cond::NE, pc_offset)
    })
}

pub(crate) fn emit_a64_cond(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    cond: Cond,
) -> Result<usize, String> {
    code.write_u32(inst::ldr_w_unsigned(
        XSCRATCH0,
        XSTATE,
        ctx.conf.state_nzcv_offset as u32,
    ))?;
    code.write_u32(inst::msr_nzcv(XSCRATCH0))?;
    code.write_u32(inst::b_cond(cond, 0))
}

fn emit_guarded_block_link_relocation(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    next: LocationDescriptor,
) -> Result<(), String> {
    let branch_offset = if ctx.conf.enable_cycle_counting {
        code.write_u32(inst::cmp_x_imm(XTICKS, 0))?;
        code.write_u32(inst::b_cond(Cond::LE, 0))?
    } else {
        code.write_u32(inst::ldar_w(XSCRATCH0, XHALT))?;
        code.write_u32(inst::cbnz_w(XSCRATCH0, 0))?
    };
    emit_block_link_relocation(
        code,
        ctx.emitted_block_info,
        next,
        BlockRelocationType::Branch,
    )?;

    let fail_offset = code.code_size();
    let pc_offset = i32::try_from(fail_offset as isize - branch_offset as isize)
        .map_err(|_| "A64 LinkBlock guard branch offset overflow".to_string())?;
    let patched_guard = if ctx.conf.enable_cycle_counting {
        inst::b_cond(Cond::LE, pc_offset)
    } else {
        inst::cbnz_w(XSCRATCH0, pc_offset)
    };
    code.patch_u32(branch_offset, patched_guard)?;
    Ok(())
}

fn patch_branch_to_current(
    code: &mut BlockOfCode,
    branch_offset: usize,
    encode: impl FnOnce(i32) -> u32,
) -> Result<(), String> {
    let target_offset = code.code_size();
    let pc_offset = i32::try_from(target_offset as isize - branch_offset as isize)
        .map_err(|_| "A64 terminal branch offset overflow".to_string())?;
    code.patch_u32(branch_offset, encode(pc_offset))
}

fn emit_set_pc_and_return_to_dispatcher(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    next: LocationDescriptor,
) -> Result<(), String> {
    let next = A64LocationDescriptor::from_location(next);
    emit_mov_x_imm(code, XSCRATCH0, next.pc())?;
    code.write_u32(inst::str_x_unsigned(
        XSCRATCH0,
        XSTATE,
        core::mem::offset_of!(A64JitState, pc) as u32,
    ))?;
    emit_relocation(code, ctx.emitted_block_info, LinkTarget::ReturnToDispatcher)
}

fn emit_mov_x_imm(code: &mut BlockOfCode, reg: u8, imm: u64) -> Result<(), String> {
    code.write_u32(inst::movz_x(reg, (imm & 0xffff) as u16, 0))?;
    for shift in [16, 32, 48] {
        let part = ((imm >> shift) & 0xffff) as u16;
        if part != 0 {
            code.write_u32(inst::movk_x(reg, part, shift as u8))?;
        }
    }
    Ok(())
}

fn emit_mov_w_imm(code: &mut BlockOfCode, reg: u8, imm: u32) -> Result<(), String> {
    code.write_u32(inst::movz_w(reg, (imm & 0xffff) as u16, 0))?;
    let high = ((imm >> 16) & 0xffff) as u16;
    if high != 0 {
        code.write_u32(inst::movk_w(reg, high, 16))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::arm64::emit_arm64::{
        BlockRelocation, EmitConfig, EmittedBlockInfo, Relocation,
    };
    use crate::backend::arm64::fastmem::FastmemManager;
    use crate::backend::arm64::fpsr_manager::FpsrManager;
    use crate::backend::arm64::reg_alloc::RegAlloc;
    use crate::ir::block::Block;
    use crate::ir::opcode::Opcode;
    use crate::ir::value::Value;
    use crate::jit_config::{JitConfig, UserCallbacks};
    use std::collections::HashMap;

    struct DummyCallbacks;

    impl UserCallbacks for DummyCallbacks {
        fn memory_read_code(&self, _vaddr: u64) -> Option<u32> {
            None
        }
        fn memory_read_8(&self, _vaddr: u64) -> u8 {
            0
        }
        fn memory_read_16(&self, _vaddr: u64) -> u16 {
            0
        }
        fn memory_read_32(&self, _vaddr: u64) -> u32 {
            0
        }
        fn memory_read_64(&self, _vaddr: u64) -> u64 {
            0
        }
        fn memory_read_128(&self, _vaddr: u64) -> (u64, u64) {
            (0, 0)
        }
        fn memory_write_8(&mut self, _vaddr: u64, _value: u8) {}
        fn memory_write_16(&mut self, _vaddr: u64, _value: u16) {}
        fn memory_write_32(&mut self, _vaddr: u64, _value: u32) {}
        fn memory_write_64(&mut self, _vaddr: u64, _value: u64) {}
        fn memory_write_128(&mut self, _vaddr: u64, _value_lo: u64, _value_hi: u64) {}
        fn exclusive_write_8(&mut self, _vaddr: u64, _value: u8, _expected: u8) -> bool {
            false
        }
        fn exclusive_write_16(&mut self, _vaddr: u64, _value: u16, _expected: u16) -> bool {
            false
        }
        fn exclusive_write_32(&mut self, _vaddr: u64, _value: u32, _expected: u32) -> bool {
            false
        }
        fn exclusive_write_64(&mut self, _vaddr: u64, _value: u64, _expected: u64) -> bool {
            false
        }
        fn exclusive_write_128(
            &mut self,
            _vaddr: u64,
            _value_lo: u64,
            _value_hi: u64,
            _expected_lo: u64,
            _expected_hi: u64,
        ) -> bool {
            false
        }
        fn call_supervisor(&mut self, _svc_num: u32) {}
        fn exception_raised(&mut self, _pc: u64, _exception: u64) {}
        fn add_ticks(&mut self, _ticks: u64) {}
        fn get_ticks_remaining(&self) -> u64 {
            0
        }
    }

    fn config_with(optimizations: OptimizationFlag, enable_cycle_counting: bool) -> JitConfig {
        JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(DummyCallbacks),
            enable_cycle_counting,
            code_cache_size: 0,
            optimizations,
            unsafe_optimizations: false,
            global_monitor: None,
            fastmem_pointer: None,
            page_table_pointer: None,
            define_unpredictable_behaviour: false,
            arch_version: crate::interface::a32::arch_version::ArchVersion::V8,
            hook_hint_instructions: false,
            processor_id: 0,
            wall_clock_cntpct: false,
            cntfrq_el0: 600_000_000,
            ctr_el0: 0x8444_c004,
            dczid_el0: 4,
            hook_data_cache_operations: false,
            hook_isb: false,
            tpidrro_el0: None,
            tpidr_el0: None,
            memory: Default::default(),
        }
    }

    fn config() -> JitConfig {
        config_with(OptimizationFlag::NO_OPTIMIZATIONS, false)
    }

    fn config_with_memory_abort_check() -> JitConfig {
        let mut config = config();
        config.memory.check_halt_on_memory_access = true;
        config
    }

    fn emitted_words(code: &BlockOfCode) -> Vec<u32> {
        (0..code.code_size() / 4)
            .map(|index| unsafe {
                code.code_base_ptr()
                    .add(index * 4)
                    .cast::<u32>()
                    .read_unaligned()
            })
            .collect()
    }

    fn with_context(
        block: &mut Block,
        code: &mut BlockOfCode,
        f: impl FnOnce(&mut BlockOfCode, &mut EmitContext<'_>),
    ) -> EmittedBlockInfo {
        with_context_config(block, code, config(), f)
    }

    fn with_context_config(
        block: &mut Block,
        code: &mut BlockOfCode,
        config: JitConfig,
        f: impl FnOnce(&mut BlockOfCode, &mut EmitContext<'_>),
    ) -> EmittedBlockInfo {
        let config = config.into_a64_user_config();
        let conf = EmitConfig::from_a64_config(&config);
        let mut reg_alloc = RegAlloc::default();
        let mut info = EmittedBlockInfo {
            entry_point: code.code_base_ptr(),
            size: 0,
            relocations: Vec::new(),
            block_relocations: crate::backend::arm64::fast_hash::FastHashMap::default(),
            fastmem_patch_info: crate::backend::arm64::fast_hash::FastHashMap::default(),
        };
        let mut fpsr = FpsrManager::default();
        let mut fastmem = FastmemManager::default();
        {
            let mut ctx = EmitContext {
                block,
                reg_alloc: &mut reg_alloc,
                conf: &conf,
                emitted_block_info: &mut info,
                fpsr: &mut fpsr,
                fastmem: &mut fastmem,
                deferred_emits: Vec::new(),
            };
            f(code, &mut ctx);
        }
        info
    }

    #[test]
    fn data_cache_callback_uses_operation_and_value_arguments() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, false).to_location());
        let inst_ref = block.append(
            Opcode::A64DataCacheOperationRaised,
            &[
                Value::ImmU64(0x1000),
                Value::ImmU64(7),
                Value::ImmU64(0x1234),
            ],
        );

        let info = with_context(&mut block, &mut code, |code, ctx| {
            emit_a64_data_cache_operation_raised(code, ctx, inst_ref).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::movz_x(X1, 7, 0),
                inst::movz_x(X2, 0x1234, 0),
                inst::nop(),
            ]
        );
        assert_eq!(
            info.relocations,
            vec![Relocation {
                code_offset: 8,
                target: LinkTarget::DataCacheOperationRaised,
            }]
        );
    }

    #[test]
    fn instruction_cache_callback_uses_operation_and_value_arguments() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, false).to_location());
        let inst_ref = block.append(
            Opcode::A64InstructionCacheOperationRaised,
            &[Value::ImmU64(2), Value::ImmU64(0x5678)],
        );

        let info = with_context(&mut block, &mut code, |code, ctx| {
            emit_a64_instruction_cache_operation_raised(code, ctx, inst_ref).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::movz_x(X1, 2, 0),
                inst::movz_x(X2, 0x5678, 0),
                inst::nop(),
            ]
        );
        assert_eq!(
            info.relocations,
            vec![Relocation {
                code_offset: 8,
                target: LinkTarget::InstructionCacheOperationRaised,
            }]
        );
    }

    #[test]
    fn return_to_dispatch_terminal_emits_relocation_placeholder() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, false).to_location());
        block.terminal = Terminal::ReturnToDispatch;

        let info = with_context(&mut block, &mut code, |code, ctx| {
            emit_a64_terminal(code, ctx).unwrap();
        });

        assert_eq!(emitted_words(&code), vec![inst::nop()]);
        assert_eq!(
            info.relocations,
            vec![Relocation {
                code_offset: 0,
                target: LinkTarget::ReturnToDispatcher,
            }]
        );
    }

    #[test]
    fn check_memory_abort_emits_upstream_abort_path_when_enabled() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, false).to_location());
        let current_location = A64LocationDescriptor::new(0x2004, 0, false).to_location();

        let info = with_context_config(
            &mut block,
            &mut code,
            config_with_memory_abort_check(),
            |code, ctx| {
                let mut end = Label::new();
                emit_a64_check_memory_abort(code, ctx, current_location, &mut end).unwrap();
                end.bind(code).unwrap();
            },
        );

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::ldar_x(XSCRATCH0, XHALT),
                inst::tst_x_imm(XSCRATCH0, HaltReason::MEMORY_ABORT.bits() as u64),
                inst::b_cond(Cond::EQ, 16),
                inst::movz_x(XSCRATCH0, 0x2004, 0),
                inst::str_x_unsigned(
                    XSCRATCH0,
                    XSTATE,
                    core::mem::offset_of!(A64JitState, pc) as u32
                ),
                inst::nop(),
            ]
        );
        assert_eq!(info.relocations[0].code_offset, 20);
        assert_eq!(info.relocations[0].target, LinkTarget::ReturnFromRunCode);
    }

    #[test]
    fn check_memory_abort_emits_nothing_when_disabled() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, false).to_location());
        let current_location = A64LocationDescriptor::new(0x2004, 0, false).to_location();

        let info = with_context(&mut block, &mut code, |code, ctx| {
            let mut end = Label::new();
            emit_a64_check_memory_abort(code, ctx, current_location, &mut end).unwrap();
            end.bind(code).unwrap();
        });

        assert_eq!(emitted_words(&code), Vec::<u32>::new());
        assert!(info.relocations.is_empty());
    }

    #[test]
    fn link_block_fast_updates_pc_then_returns_to_dispatcher_without_block_linking() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, true).to_location());
        let next = A64LocationDescriptor::new(0x1234_5678_9abc, 0, false).to_location();
        block.terminal = Terminal::LinkBlockFast { next };

        let info = with_context(&mut block, &mut code, |code, ctx| {
            emit_a64_terminal(code, ctx).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::movz_x(XSCRATCH0, 0x9abc, 0),
                inst::movk_x(XSCRATCH0, 0x5678, 16),
                inst::movk_x(XSCRATCH0, 0x1234, 32),
                inst::str_x_unsigned(
                    XSCRATCH0,
                    XSTATE,
                    core::mem::offset_of!(A64JitState, pc) as u32
                ),
                inst::nop(),
            ]
        );
        assert_eq!(info.relocations[0].code_offset, 16);
        assert_eq!(info.block_relocations.len(), 0);
    }

    #[test]
    fn link_block_updates_pc_then_returns_to_dispatcher_without_block_linking() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, true).to_location());
        let next = A64LocationDescriptor::new(0x2004, 0, false).to_location();
        block.terminal = Terminal::LinkBlock { next };

        let info = with_context(&mut block, &mut code, |code, ctx| {
            emit_a64_terminal(code, ctx).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::movz_x(XSCRATCH0, 0x2004, 0),
                inst::str_x_unsigned(
                    XSCRATCH0,
                    XSTATE,
                    core::mem::offset_of!(A64JitState, pc) as u32
                ),
                inst::nop(),
            ]
        );
        assert_eq!(info.relocations[0].code_offset, 8);
        assert_eq!(info.block_relocations.len(), 0);
    }

    #[test]
    fn link_block_with_block_linking_checks_halt_then_links_or_falls_back() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, false).to_location());
        let next = A64LocationDescriptor::new(0x2004, 0, false).to_location();
        block.terminal = Terminal::LinkBlock { next };

        let info = with_context_config(
            &mut block,
            &mut code,
            config_with(OptimizationFlag::BLOCK_LINKING, false),
            |code, ctx| {
                emit_a64_terminal(code, ctx).unwrap();
            },
        );

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::ldar_w(XSCRATCH0, XHALT),
                inst::cbnz_w(XSCRATCH0, 8),
                inst::nop(),
                inst::movz_x(XSCRATCH0, 0x2004, 0),
                inst::str_x_unsigned(
                    XSCRATCH0,
                    XSTATE,
                    core::mem::offset_of!(A64JitState, pc) as u32
                ),
                inst::nop(),
            ]
        );
        assert_eq!(
            info.block_relocations[&next],
            vec![BlockRelocation {
                code_offset: 8,
                relocation_type: BlockRelocationType::Branch,
            }]
        );
        assert_eq!(info.relocations[0].code_offset, 20);
    }

    #[test]
    fn link_block_with_cycle_counting_checks_ticks_then_links_or_falls_back() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, false).to_location());
        let next = A64LocationDescriptor::new(0x2004, 0, false).to_location();
        block.terminal = Terminal::LinkBlock { next };

        let info = with_context_config(
            &mut block,
            &mut code,
            config_with(OptimizationFlag::BLOCK_LINKING, true),
            |code, ctx| {
                emit_a64_terminal(code, ctx).unwrap();
            },
        );

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::cmp_x_imm(XTICKS, 0),
                inst::b_cond(Cond::LE, 8),
                inst::nop(),
                inst::movz_x(XSCRATCH0, 0x2004, 0),
                inst::str_x_unsigned(
                    XSCRATCH0,
                    XSTATE,
                    core::mem::offset_of!(A64JitState, pc) as u32
                ),
                inst::nop(),
            ]
        );
        assert_eq!(
            info.block_relocations[&next],
            vec![BlockRelocation {
                code_offset: 8,
                relocation_type: BlockRelocationType::Branch,
            }]
        );
        assert_eq!(info.relocations[0].code_offset, 20);
    }

    #[test]
    fn fast_dispatch_hint_returns_to_dispatcher_like_upstream_todo_path() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, false).to_location());
        block.terminal = Terminal::FastDispatchHint;

        let info = with_context(&mut block, &mut code, |code, ctx| {
            emit_a64_terminal(code, ctx).unwrap();
        });

        assert_eq!(emitted_words(&code), vec![inst::nop()]);
        assert_eq!(info.relocations[0].code_offset, 0);
        assert_eq!(info.relocations[0].target, LinkTarget::ReturnToDispatcher);
    }

    #[test]
    fn pop_rsb_hint_with_rsb_optimization_emits_upstream_prediction_path() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, false).to_location());
        block.terminal = Terminal::PopRSBHint;

        let info = with_context_config(
            &mut block,
            &mut code,
            config_with(OptimizationFlag::RETURN_STACK_BUFFER, false),
            |code, ctx| {
                emit_a64_terminal(code, ctx).unwrap();
            },
        );

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::movz_w(XSCRATCH0, 0, 0),
                inst::movk_w(
                    XSCRATCH0,
                    (A64LocationDescriptor::FPCR_MASK >> 16) as u16,
                    16
                ),
                inst::ldr_w_unsigned(X0, XSTATE, core::mem::offset_of!(A64JitState, fpcr) as u32,),
                inst::ldr_x_unsigned(X1, XSTATE, core::mem::offset_of!(A64JitState, pc) as u32,),
                inst::and_w_reg(X0, X0, XSCRATCH0),
                inst::and_x_imm(X1, X1, A64LocationDescriptor::PC_MASK),
                inst::lsl_x_imm(X0, X0, A64LocationDescriptor::FPCR_SHIFT as u8),
                inst::orr_x(X0, X0, X1),
                inst::ldr_w_unsigned(
                    XSCRATCH2,
                    31,
                    crate::backend::arm64::stack_layout::StackLayout::rsb_ptr_offset() as u32,
                ),
                inst::and_w_imm(XSCRATCH2, XSCRATCH2, RSB_INDEX_MASK as u32),
                inst::add_x_reg_sp(X2, 31, XSCRATCH2),
                inst::sub_w_imm(
                    XSCRATCH2,
                    XSCRATCH2,
                    core::mem::size_of::<RSBEntry>() as u32,
                ),
                inst::str_w_unsigned(
                    XSCRATCH2,
                    31,
                    crate::backend::arm64::stack_layout::StackLayout::rsb_ptr_offset() as u32,
                ),
                inst::ldp_x_offset(XSCRATCH0, XSCRATCH1, X2, StackLayout::rsb_offset() as i32),
                inst::cmp_x_reg(X0, XSCRATCH0),
                inst::b_cond(Cond::NE, 8),
                inst::br(XSCRATCH1),
                inst::nop(),
            ]
        );
        assert_eq!(info.relocations[0].code_offset, 68);
        assert_eq!(info.relocations[0].target, LinkTarget::ReturnToDispatcher);
    }

    #[test]
    fn check_halt_branches_to_dispatcher_when_halted() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, false).to_location());
        block.terminal = Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        };

        let info = with_context(&mut block, &mut code, |code, ctx| {
            emit_a64_terminal(code, ctx).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::ldar_w(XSCRATCH0, XHALT),
                inst::cbnz_w(XSCRATCH0, 8),
                inst::nop(),
                inst::nop(),
            ]
        );
        assert_eq!(info.relocations[0].code_offset, 8);
        assert_eq!(info.relocations[1].code_offset, 12);
    }

    #[test]
    fn check_bit_branches_between_then_and_else_terminals() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, false).to_location());
        block.terminal = Terminal::CheckBit {
            then_: Box::new(Terminal::ReturnToDispatch),
            else_: Box::new(Terminal::ReturnToDispatch),
        };

        let info = with_context(&mut block, &mut code, |code, ctx| {
            emit_a64_terminal(code, ctx).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::ldrb_w_unsigned(
                    XSCRATCH0,
                    31,
                    crate::backend::arm64::stack_layout::StackLayout::check_bit_offset() as u32,
                ),
                inst::cbz_w(XSCRATCH0, 8),
                inst::nop(),
                inst::nop(),
            ]
        );
        assert_eq!(info.relocations[0].code_offset, 8);
        assert_eq!(info.relocations[1].code_offset, 12);
    }

    #[test]
    fn if_terminal_restores_nzcv_then_branches_to_then_terminal() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, false).to_location());
        block.terminal = Terminal::If {
            cond: Cond::NE,
            then_: Box::new(Terminal::ReturnToDispatch),
            else_: Box::new(Terminal::ReturnToDispatch),
        };

        let info = with_context(&mut block, &mut code, |code, ctx| {
            emit_a64_terminal(code, ctx).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::ldr_w_unsigned(
                    XSCRATCH0,
                    XSTATE,
                    core::mem::offset_of!(A64JitState, cpsr_nzcv) as u32,
                ),
                inst::msr_nzcv(XSCRATCH0),
                inst::b_cond(Cond::NE, 8),
                inst::nop(),
                inst::nop(),
            ]
        );
        assert_eq!(info.relocations[0].code_offset, 12);
        assert_eq!(info.relocations[1].code_offset, 16);
    }
}
