//! ARM64 A32 coprocessor emission.
//!
//! Upstream owner: `backend/arm64/emit_arm64_a32_coprocessor.cpp`.

use crate::backend::arm64::abi::{XSCRATCH0, XSCRATCH1};
use crate::backend::arm64::block_of_code::BlockOfCode;
use crate::backend::arm64::emit_context::EmitContext;
use crate::backend::arm64::inst;
use crate::backend::arm64::reg_alloc::{Argument, HostLoc, HostLocKind, RegAlloc};
use crate::interface::a32::coprocessor::{
    Callback, CallbackOrAccessOneWord, CallbackOrAccessTwoWords,
};
use crate::interface::a32::coprocessor_util::CoprocReg;
use crate::ir::value::InstRef;

const X0: u8 = 0;

fn emit_coprocessor_exception() -> ! {
    unreachable!("A32 coprocessor operation has no compile-time action")
}

fn call_coproc_callback(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    callback: Callback,
    inst_ref: Option<InstRef>,
    arg0: Option<Argument>,
    arg1: Option<Argument>,
) -> Result<(), String> {
    ctx.reg_alloc
        .prepare_for_call(code, ctx.fpsr, [None, arg0, arg1, None])?;

    if let Some(user_arg) = callback.user_arg {
        emit_mov_x_imm(code, X0, user_arg as usize as u64)?;
    }
    emit_mov_x_imm(code, XSCRATCH0, callback.function as usize as u64)?;
    code.write_u32(inst::blr(XSCRATCH0))?;

    if let Some(inst_ref) = inst_ref {
        ctx.reg_alloc.define_as_register(
            ctx.block,
            inst_ref,
            HostLoc {
                kind: HostLocKind::Gpr,
                index: X0 as usize,
            },
        );
    }
    Ok(())
}

pub fn emit_a32_coproc_internal_operation(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let coproc_info = ctx.block.get(inst_ref).args[0]
        .get_coproc_info()
        .to_le_bytes();
    let coproc_num = coproc_info[0] as usize;
    let two = coproc_info[1] != 0;
    let opc1 = coproc_info[2] as u32;
    let crd = CoprocReg::from_u8(coproc_info[3]);
    let crn = CoprocReg::from_u8(coproc_info[4]);
    let crm = CoprocReg::from_u8(coproc_info[5]);
    let opc2 = coproc_info[6] as u32;

    let Some(coproc) = ctx.conf.coprocessors[coproc_num].clone() else {
        emit_coprocessor_exception();
    };
    let Some(action) = coproc.compile_internal_operation(two, opc1, crd, crn, crm, opc2) else {
        emit_coprocessor_exception();
    };
    call_coproc_callback(code, ctx, action, None, None, None)
}

pub fn emit_a32_coproc_send_one_word(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let coproc_info = ctx.block.get(inst_ref).args[0]
        .get_coproc_info()
        .to_le_bytes();
    let coproc_num = coproc_info[0] as usize;
    let two = coproc_info[1] != 0;
    let opc1 = coproc_info[2] as u32;
    let crn = CoprocReg::from_u8(coproc_info[3]);
    let crm = CoprocReg::from_u8(coproc_info[4]);
    let opc2 = coproc_info[5] as u32;

    let Some(coproc) = ctx.conf.coprocessors[coproc_num].clone() else {
        emit_coprocessor_exception();
    };
    match coproc.compile_send_one_word(two, opc1, crn, crm, opc2) {
        CallbackOrAccessOneWord::CoprocessorException => emit_coprocessor_exception(),
        CallbackOrAccessOneWord::Callback(callback) => {
            call_coproc_callback(code, ctx, callback, None, Some(args[1]), None)?;
        }
        CallbackOrAccessOneWord::Memory(destination_ptr) => {
            let mut value = ctx.reg_alloc.read_w(args[1]);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut value])?;
            let value = value.index().expect("coprocessor source must be realized") as u8;
            emit_mov_x_imm(code, XSCRATCH0, destination_ptr as usize as u64)?;
            code.write_u32(inst::str_w_unsigned(value, XSCRATCH0, 0))?;
        }
    }
    Ok(())
}

pub fn emit_a32_coproc_send_two_words(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let coproc_info = ctx.block.get(inst_ref).args[0]
        .get_coproc_info()
        .to_le_bytes();
    let coproc_num = coproc_info[0] as usize;
    let two = coproc_info[1] != 0;
    let opc = coproc_info[2] as u32;
    let crm = CoprocReg::from_u8(coproc_info[3]);

    let Some(coproc) = ctx.conf.coprocessors[coproc_num].clone() else {
        emit_coprocessor_exception();
    };
    match coproc.compile_send_two_words(two, opc, crm) {
        CallbackOrAccessTwoWords::CoprocessorException => emit_coprocessor_exception(),
        CallbackOrAccessTwoWords::Callback(callback) => {
            call_coproc_callback(code, ctx, callback, None, Some(args[1]), Some(args[2]))?;
        }
        CallbackOrAccessTwoWords::Memory(destination_ptrs) => {
            let mut value1 = ctx.reg_alloc.read_w(args[1]);
            let mut value2 = ctx.reg_alloc.read_w(args[2]);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut value1, &mut value2])?;
            let value1 = value1.index().expect("coprocessor source must be realized") as u8;
            let value2 = value2.index().expect("coprocessor source must be realized") as u8;
            emit_mov_x_imm(code, XSCRATCH0, destination_ptrs[0] as usize as u64)?;
            emit_mov_x_imm(code, XSCRATCH1, destination_ptrs[1] as usize as u64)?;
            code.write_u32(inst::str_w_unsigned(value1, XSCRATCH0, 0))?;
            code.write_u32(inst::str_w_unsigned(value2, XSCRATCH1, 0))?;
        }
    }
    Ok(())
}

pub fn emit_a32_coproc_get_one_word(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let coproc_info = ctx.block.get(inst_ref).args[0]
        .get_coproc_info()
        .to_le_bytes();
    let coproc_num = coproc_info[0] as usize;
    let two = coproc_info[1] != 0;
    let opc1 = coproc_info[2] as u32;
    let crn = CoprocReg::from_u8(coproc_info[3]);
    let crm = CoprocReg::from_u8(coproc_info[4]);
    let opc2 = coproc_info[5] as u32;

    let Some(coproc) = ctx.conf.coprocessors[coproc_num].clone() else {
        emit_coprocessor_exception();
    };
    match coproc.compile_get_one_word(two, opc1, crn, crm, opc2) {
        CallbackOrAccessOneWord::CoprocessorException => emit_coprocessor_exception(),
        CallbackOrAccessOneWord::Callback(callback) => {
            call_coproc_callback(code, ctx, callback, Some(inst_ref), None, None)?;
        }
        CallbackOrAccessOneWord::Memory(source_ptr) => {
            let mut value = ctx.reg_alloc.write_w(inst_ref);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut value])?;
            let value = value.index().expect("coprocessor destination must be realized") as u8;
            emit_mov_x_imm(code, XSCRATCH0, source_ptr as usize as u64)?;
            code.write_u32(inst::ldr_w_unsigned(value, XSCRATCH0, 0))?;
        }
    }
    Ok(())
}

pub fn emit_a32_coproc_get_two_words(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let coproc_info = ctx.block.get(inst_ref).args[0]
        .get_coproc_info()
        .to_le_bytes();
    let coproc_num = coproc_info[0] as usize;
    let two = coproc_info[1] != 0;
    let opc = coproc_info[2] as u32;
    let crm = CoprocReg::from_u8(coproc_info[3]);

    let Some(coproc) = ctx.conf.coprocessors[coproc_num].clone() else {
        emit_coprocessor_exception();
    };
    match coproc.compile_get_two_words(two, opc, crm) {
        CallbackOrAccessTwoWords::CoprocessorException => emit_coprocessor_exception(),
        CallbackOrAccessTwoWords::Callback(callback) => {
            call_coproc_callback(code, ctx, callback, Some(inst_ref), None, None)?;
        }
        CallbackOrAccessTwoWords::Memory(source_ptrs) => {
            let mut value = ctx.reg_alloc.write_x(inst_ref);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut value])?;
            let value = value.index().expect("coprocessor destination must be realized") as u8;
            emit_mov_x_imm(code, XSCRATCH0, source_ptrs[0] as usize as u64)?;
            emit_mov_x_imm(code, XSCRATCH1, source_ptrs[1] as usize as u64)?;
            code.write_u32(inst::ldr_x_unsigned(value, XSCRATCH0, 0))?;
            code.write_u32(inst::ldr_w_unsigned(XSCRATCH1, XSCRATCH1, 0))?;
            code.write_u32(inst::bfi_x(value, XSCRATCH1, 32, 32))?;
        }
    }
    Ok(())
}

pub fn emit_a32_coproc_load_words(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let coproc_info = ctx.block.get(inst_ref).args[0]
        .get_coproc_info()
        .to_le_bytes();
    let coproc_num = coproc_info[0] as usize;
    let two = coproc_info[1] != 0;
    let long_transfer = coproc_info[2] != 0;
    let crd = CoprocReg::from_u8(coproc_info[3]);
    let option = (coproc_info[4] != 0).then_some(coproc_info[5]);

    let Some(coproc) = ctx.conf.coprocessors[coproc_num].clone() else {
        emit_coprocessor_exception();
    };
    let Some(action) = coproc.compile_load_words(two, long_transfer, crd, option) else {
        emit_coprocessor_exception();
    };
    call_coproc_callback(code, ctx, action, None, Some(args[1]), None)
}

pub fn emit_a32_coproc_store_words(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let coproc_info = ctx.block.get(inst_ref).args[0]
        .get_coproc_info()
        .to_le_bytes();
    let coproc_num = coproc_info[0] as usize;
    let two = coproc_info[1] != 0;
    let long_transfer = coproc_info[2] != 0;
    let crd = CoprocReg::from_u8(coproc_info[3]);
    let option = (coproc_info[4] != 0).then_some(coproc_info[5]);

    let Some(coproc) = ctx.conf.coprocessors[coproc_num].clone() else {
        emit_coprocessor_exception();
    };
    let Some(action) = coproc.compile_store_words(two, long_transfer, crd, option) else {
        emit_coprocessor_exception();
    };
    call_coproc_callback(code, ctx, action, None, Some(args[1]), None)
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
    use crate::interface::a32::coprocessor::Coprocessor;
    use crate::backend::arm64::emit_arm64::{emit_arm64, EmitConfig, EmittedBlockInfo};
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
    use std::cell::UnsafeCell;
    use std::collections::HashMap;
    use std::sync::Arc;

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

    struct TestCoprocessor {
        value: UnsafeCell<u32>,
    }

    unsafe impl Send for TestCoprocessor {}
    unsafe impl Sync for TestCoprocessor {}

    unsafe extern "C" fn test_callback(
        _user_arg: *mut std::ffi::c_void,
        _arg0: u32,
        _arg1: u32,
    ) -> u64 {
        0x1122_3344_5566_7788
    }

    impl Coprocessor for TestCoprocessor {
        fn compile_internal_operation(
            &self,
            _two: bool,
            _opc1: u32,
            _crd: CoprocReg,
            _crn: CoprocReg,
            _crm: CoprocReg,
            _opc2: u32,
        ) -> Option<Callback> {
            Some(Callback {
                function: test_callback,
                user_arg: None,
            })
        }

        fn compile_send_one_word(
            &self,
            _two: bool,
            _opc1: u32,
            _crn: CoprocReg,
            _crm: CoprocReg,
            _opc2: u32,
        ) -> CallbackOrAccessOneWord {
            CallbackOrAccessOneWord::Memory(self.value.get())
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
            CallbackOrAccessOneWord::Memory(self.value.get())
        }

        fn compile_get_two_words(
            &self,
            _two: bool,
            _opc: u32,
            _crm: CoprocReg,
        ) -> CallbackOrAccessTwoWords {
            CallbackOrAccessTwoWords::Callback(Callback {
                function: test_callback,
                user_arg: None,
            })
        }

        fn compile_load_words(
            &self,
            _two: bool,
            _long_transfer: bool,
            _crd: CoprocReg,
            _option: Option<u8>,
        ) -> Option<Callback> {
            Some(Callback {
                function: test_callback,
                user_arg: None,
            })
        }

        fn compile_store_words(
            &self,
            _two: bool,
            _long_transfer: bool,
            _crd: CoprocReg,
            _option: Option<u8>,
        ) -> Option<Callback> {
            Some(Callback {
                function: test_callback,
                user_arg: None,
            })
        }
    }

    fn config() -> EmitConfig {
        let mut coprocessors = JitConfig::default_coprocessors();
        coprocessors[15] = Some(Arc::new(TestCoprocessor {
            value: UnsafeCell::new(0),
        }));
        let jit_config = JitConfig {
            coprocessors,
            callbacks: Box::new(DummyCallbacks),
            enable_cycle_counting: false,
            code_cache_size: 0,
            optimizations: OptimizationFlag::NO_OPTIMIZATIONS,
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
            | ((opc2 as u64) << 40)
    }

    fn coproc_info_two(cp: u8, opc: u8, crm: u8) -> u64 {
        cp as u64 | ((opc as u64) << 16) | ((crm as u64) << 32)
    }

    #[test]
    fn configured_coprocessor_memory_accesses_are_emitted() {
        let config = config();
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
    fn configured_get_two_words_callback_is_called_directly() {
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

        assert!(info.relocations.is_empty());
        assert_eq!(
            read_instruction(&code, code.code_size() - 4),
            inst::blr(XSCRATCH0)
        );
    }
}
