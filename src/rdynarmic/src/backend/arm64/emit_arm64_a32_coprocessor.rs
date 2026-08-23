//! ARM64 A32 coprocessor emission.
//!
//! Upstream owner: `backend/arm64/emit_arm64_a32_coprocessor.cpp`.

use crate::backend::arm64::abi::XSCRATCH0;
use crate::backend::arm64::block_of_code::BlockOfCode;
use crate::backend::arm64::emit_arm64::{emit_relocation, LinkTarget};
use crate::backend::arm64::emit_context::EmitContext;
use crate::backend::arm64::inst;
use crate::backend::arm64::reg_alloc::{HostLoc, HostLocKind, RegAlloc};
use crate::ir::value::InstRef;

const X0: u8 = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CoprocInfo {
    coproc_no: u8,
    two: bool,
    opc1: u8,
    crn: u8,
    crm: u8,
    opc2: u8,
}

fn coproc_info(ctx: &EmitContext<'_>, inst_ref: InstRef) -> CoprocInfo {
    let info = ctx.block.get(inst_ref).args[0].get_coproc_info();
    CoprocInfo {
        coproc_no: (info & 0xff) as u8,
        two: ((info >> 8) & 0xff) != 0,
        opc1: ((info >> 16) & 0xff) as u8,
        crn: ((info >> 24) & 0xff) as u8,
        crm: ((info >> 32) & 0xff) as u8,
        opc2: ((info >> 48) & 0xff) as u8,
    }
}

pub fn emit_a32_coproc_internal_operation(
    _code: &mut BlockOfCode,
    _ctx: &mut EmitContext<'_>,
    _inst_ref: InstRef,
) -> Result<(), String> {
    // Upstream delegates to configured coprocessor objects. The current Rust
    // config has no generic coprocessor registry yet, and the local x64 backend
    // treats CP15 internal/cache operations as no-ops.
    Ok(())
}

pub fn emit_a32_coproc_send_one_word(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    // Match upstream: acquire argument information before dispatching the
    // coprocessor action so even ignored writes consume their IR operands.
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let info = coproc_info(ctx, inst_ref);
    if info.coproc_no != 15 {
        return Ok(());
    }

    if !info.two && info.opc1 == 0 && info.crn == 7 && info.crm == 5 && info.opc2 == 4 {
        // CP15_FLUSH_PREFETCH_BUFFER: dummy write, ignore the source value.
        return Ok(());
    }

    if !info.two && info.opc1 == 0 && info.crn == 7 && info.crm == 10 {
        match info.opc2 {
            // CP15_DATA_SYNC_BARRIER
            4 => {
                code.write_u32(inst::dsb_sy())?;
                return Ok(());
            }
            // CP15_DATA_MEMORY_BARRIER
            5 => {
                code.write_u32(inst::dmb_sy())?;
                return Ok(());
            }
            _ => {}
        }
    }

    if !info.two
        && info.opc1 == 0
        && info.crn == 13
        && info.crm == 0
        && info.opc2 == 2
        && !ctx.conf.a32_cp15_uprw.is_null()
    {
        // CP15_THREAD_UPRW
        let mut value = ctx.reg_alloc.read_w(args[1]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut value])?;
        let value_reg = value.index().expect("CP15 source must be realized") as u8;

        emit_mov_x_imm(code, XSCRATCH0, ctx.conf.a32_cp15_uprw as u64)?;
        code.write_u32(inst::str_w_unsigned(value_reg, XSCRATCH0, 0))?;
    }
    Ok(())
}

pub fn emit_a32_coproc_send_two_words(
    _code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let _args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    // MCRR is currently a no-op in the local Rust A32 backend.
    Ok(())
}

pub fn emit_a32_coproc_get_one_word(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let info = coproc_info(ctx, inst_ref);
    let mut value = ctx.reg_alloc.write_w(inst_ref);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut value])?;
    let value_reg = value.index().expect("CP15 destination must be realized") as u8;

    match (info.coproc_no, info.crn, info.crm, info.opc2) {
        // MRC p15, 0, Rt, c13, c0, 2: read TPIDR_UPRW.
        (15, 13, 0, 2) if !ctx.conf.a32_cp15_uprw.is_null() => {
            emit_mov_x_imm(code, XSCRATCH0, ctx.conf.a32_cp15_uprw as u64)?;
            code.write_u32(inst::ldr_w_unsigned(value_reg, XSCRATCH0, 0))?;
        }
        // MRC p15, 0, Rt, c13, c0, 3: read TPIDR_URO.
        (15, 13, 0, 3) if !ctx.conf.a32_cp15_uro.is_null() => {
            emit_mov_x_imm(code, XSCRATCH0, ctx.conf.a32_cp15_uro as u64)?;
            code.write_u32(inst::ldr_w_unsigned(value_reg, XSCRATCH0, 0))?;
        }
        _ => {
            code.write_u32(inst::movz_w(value_reg, 0, 0))?;
        }
    }
    Ok(())
}

pub fn emit_a32_coproc_get_two_words(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let info = coproc_info(ctx, inst_ref);

    if info.coproc_no == 15 && !info.two && info.opc1 == 0 && info.crm == 14 {
        ctx.reg_alloc
            .prepare_for_call(code, ctx.fpsr, [None, None, None, None])?;
        emit_relocation(code, ctx.emitted_block_info, LinkTarget::GetCNTPCT)?;
        ctx.reg_alloc.define_as_register(
            ctx.block,
            inst_ref,
            HostLoc {
                kind: HostLocKind::Gpr,
                index: X0 as usize,
            },
        );
        return Ok(());
    }

    let mut value = ctx.reg_alloc.write_x(inst_ref);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut value])?;
    let value_reg = value.index().expect("CP15 destination must be realized") as u8;
    code.write_u32(inst::movz_x(value_reg, 0, 0))?;
    Ok(())
}

pub fn emit_a32_coproc_load_words(
    _code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let _args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    // LDC is currently a no-op in the local Rust A32 backend.
    Ok(())
}

pub fn emit_a32_coproc_store_words(
    _code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let _args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    // STC is currently a no-op in the local Rust A32 backend.
    Ok(())
}

fn emit_mov_x_imm(code: &mut BlockOfCode, reg: u8, imm: u64) -> Result<(), String> {
    code.write_u32(inst::movz_x(reg, (imm & 0xffff) as u16, 0))?;
    for shift in [16, 32, 48] {
        let chunk = ((imm >> shift) & 0xffff) as u16;
        if chunk != 0 {
            code.write_u32(inst::movk_x(reg, chunk, shift as u8))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::arm64::emit_arm64::{emit_arm64, EmitConfig, EmittedBlockInfo, Relocation};
    use crate::backend::arm64::fastmem::FastmemManager;
    use crate::backend::arm64::fpsr_manager::FpsrManager;
    use crate::backend::arm64::reg_alloc::RegAlloc;
    use crate::backend::common::emit_context::MemoryEmitConfig;
    use crate::frontend::a32::fpscr::FPSCR;
    use crate::frontend::a32::psr::PSR;
    use crate::frontend::a32::types::Reg;
    use crate::ir::block::Block;
    use crate::ir::inst::Inst;
    use crate::ir::location::A32LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::terminal::Terminal;
    use crate::ir::value::Value;
    use crate::jit_config::{JitConfig, OptimizationFlag, UserCallbacks};
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
        fn exclusive_read_8(&self, _vaddr: u64) -> u8 {
            0
        }
        fn exclusive_read_16(&self, _vaddr: u64) -> u16 {
            0
        }
        fn exclusive_read_32(&self, _vaddr: u64) -> u32 {
            0
        }
        fn exclusive_read_64(&self, _vaddr: u64) -> u64 {
            0
        }
        fn exclusive_read_128(&self, _vaddr: u64) -> (u64, u64) {
            (0, 0)
        }
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
        fn exclusive_clear(&mut self) {}
        fn call_supervisor(&mut self, _svc_num: u32) {}
        fn exception_raised(&mut self, _pc: u64, _exception: u64) {}
        fn add_ticks(&mut self, _ticks: u64) {}
        fn get_ticks_remaining(&self) -> u64 {
            0
        }
    }

    fn config() -> EmitConfig {
        let jit_config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(DummyCallbacks),
            enable_cycle_counting: false,
            code_cache_size: 0,
            optimizations: OptimizationFlag::NO_OPTIMIZATIONS,
            unsafe_optimizations: false,
            global_monitor: None,
            fastmem_pointer: None,
            page_table_pointer: None,
            define_unpredictable_behaviour: false,
            processor_id: 0,
            wall_clock_cntpct: false,
            cntfrq_el0: 600_000_000,
            tpidrro_el0: None,
            tpidr_el0: None,
            memory: MemoryEmitConfig::default(),
        };
        EmitConfig::from_a32_config(&jit_config)
    }

    fn empty_block_info(code: &BlockOfCode) -> EmittedBlockInfo {
        EmittedBlockInfo {
            entry_point: code.code_base_ptr(),
            size: 0,
            relocations: Vec::new(),
            block_relocations: crate::backend::arm64::fast_hash::FastHashMap::default(),
            fastmem_patch_info: crate::backend::arm64::fast_hash::FastHashMap::default(),
        }
    }

    fn block_with_inst(opcode: Opcode, args: &[Value]) -> Block {
        let mut block = Block::new(
            A32LocationDescriptor::new(0x1000, PSR::new(0), FPSCR::new(0), false).to_location(),
        );
        block.push_inst(Inst::new(opcode, args));
        block.terminal = Terminal::ReturnToDispatch;
        block
    }

    fn emit_test(
        block: &mut Block,
        code: &mut BlockOfCode,
        info: &mut EmittedBlockInfo,
        config: &EmitConfig,
        emit: impl FnOnce(&mut BlockOfCode, &mut EmitContext<'_>, InstRef) -> Result<(), String>,
    ) {
        let mut reg_alloc = RegAlloc::default();
        let mut fpsr = FpsrManager::new(config.state_fpsr_offset);
        let mut fastmem = FastmemManager::default();
        let mut ctx = EmitContext {
            block,
            reg_alloc: &mut reg_alloc,
            conf: config,
            emitted_block_info: info,
            fpsr: &mut fpsr,
            fastmem: &mut fastmem,
            deferred_emits: Vec::new(),
        };
        emit(code, &mut ctx, InstRef(0)).unwrap();
    }

    fn read_instruction(code: &BlockOfCode, offset: usize) -> u32 {
        unsafe {
            code.code_base_ptr()
                .add(offset)
                .cast::<u32>()
                .read_unaligned()
        }
    }

    fn test_gpr(index: usize) -> u8 {
        crate::backend::arm64::abi::GPR_ORDER[index] as u8
    }

    fn coproc_info(cp: u8, opc1: u8, crn: u8, crm: u8, opc2: u8) -> u64 {
        cp as u64
            | ((opc1 as u64) << 16)
            | ((crn as u64) << 24)
            | ((crm as u64) << 32)
            | ((opc2 as u64) << 48)
    }

    fn coproc_info_two(cp: u8, opc: u8, crm: u8) -> u64 {
        cp as u64 | ((opc as u64) << 16) | ((crm as u64) << 32)
    }

    #[test]
    fn cp15_tpidr_uprw_write_and_read_use_external_pointer() {
        let mut value = 0u32;
        let mut config = config();
        config.a32_cp15_uprw = &mut value;
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut info = empty_block_info(&code);
        let mut block = block_with_inst(
            Opcode::A32CoprocSendOneWord,
            &[
                Value::ImmCoprocInfo(coproc_info(15, 0, 13, 0, 2)),
                Value::ImmU32(0x1234),
            ],
        );

        emit_test(
            &mut block,
            &mut code,
            &mut info,
            &config,
            |code, ctx, inst| emit_a32_coproc_send_one_word(code, ctx, inst),
        );

        assert_eq!(
            read_instruction(&code, 0),
            inst::movz_x(test_gpr(0), 0x1234, 0)
        );
        assert_eq!(
            read_instruction(&code, code.code_size() - 4),
            inst::str_w_unsigned(test_gpr(0), XSCRATCH0, 0)
        );

        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut info = empty_block_info(&code);
        let mut block = block_with_inst(
            Opcode::A32CoprocGetOneWord,
            &[Value::ImmCoprocInfo(coproc_info(15, 0, 13, 0, 2))],
        );

        emit_test(
            &mut block,
            &mut code,
            &mut info,
            &config,
            |code, ctx, inst| emit_a32_coproc_get_one_word(code, ctx, inst),
        );

        assert_eq!(
            read_instruction(&code, code.code_size() - 4),
            inst::ldr_w_unsigned(test_gpr(0), XSCRATCH0, 0)
        );
    }

    #[test]
    fn cp15_legacy_memory_barriers_match_host_barriers() {
        let config = config();

        for (opc2, expected) in [(4, inst::dsb_sy()), (5, inst::dmb_sy())] {
            let mut code = BlockOfCode::with_size(4096).unwrap();
            let mut info = empty_block_info(&code);
            let mut block = block_with_inst(
                Opcode::A32CoprocSendOneWord,
                &[
                    Value::ImmCoprocInfo(coproc_info(15, 0, 7, 10, opc2)),
                    Value::ImmU32(0),
                ],
            );

            emit_test(
                &mut block,
                &mut code,
                &mut info,
                &config,
                |code, ctx, inst| emit_a32_coproc_send_one_word(code, ctx, inst),
            );

            assert_eq!(read_instruction(&code, 0), expected);
        }

        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut info = empty_block_info(&code);
        let mut block = block_with_inst(
            Opcode::A32CoprocSendOneWord,
            &[
                Value::ImmCoprocInfo(coproc_info(15, 1, 7, 10, 4)),
                Value::ImmU32(0),
            ],
        );

        emit_test(
            &mut block,
            &mut code,
            &mut info,
            &config,
            |code, ctx, inst| emit_a32_coproc_send_one_word(code, ctx, inst),
        );

        assert_eq!(code.code_size(), 0);
    }

    #[test]
    fn ignored_cp15_write_consumes_register_operand() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(
            A32LocationDescriptor::new(0x1000, PSR::new(0), FPSCR::new(0), false).to_location(),
        );
        let value = block.append(Opcode::A32GetRegister, &[Value::ImmA32Reg(Reg::R12)]);
        block.append(
            Opcode::A32CoprocSendOneWord,
            &[
                Value::ImmCoprocInfo(coproc_info(15, 0, 7, 5, 4)),
                Value::Inst(value),
            ],
        );
        block.terminal = Terminal::ReturnToDispatch;

        emit_arm64(&mut code, block, config()).unwrap();
    }

    #[test]
    fn cp15_unknown_get_one_word_returns_zero() {
        let config = config();
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut info = empty_block_info(&code);
        let mut block = block_with_inst(
            Opcode::A32CoprocGetOneWord,
            &[Value::ImmCoprocInfo(coproc_info(15, 0, 1, 0, 0))],
        );

        emit_test(
            &mut block,
            &mut code,
            &mut info,
            &config,
            |code, ctx, inst| emit_a32_coproc_get_one_word(code, ctx, inst),
        );

        assert_eq!(read_instruction(&code, 0), inst::movz_w(test_gpr(0), 0, 0));
    }

    #[test]
    fn cp15_cntpct_get_two_words_uses_get_cntpct_relocation() {
        let config = config();
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut info = empty_block_info(&code);
        let mut block = block_with_inst(
            Opcode::A32CoprocGetTwoWords,
            &[Value::ImmCoprocInfo(coproc_info_two(15, 0, 14))],
        );

        emit_test(
            &mut block,
            &mut code,
            &mut info,
            &config,
            |code, ctx, inst| emit_a32_coproc_get_two_words(code, ctx, inst),
        );

        assert_eq!(
            info.relocations,
            vec![Relocation {
                code_offset: 0,
                target: LinkTarget::GetCNTPCT,
            }]
        );
        assert_eq!(read_instruction(&code, 0), inst::nop());
    }
}
