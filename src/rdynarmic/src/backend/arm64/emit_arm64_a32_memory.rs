//! ARM64 A32 memory emission wrappers.
//!
//! Upstream owner: `backend/arm64/emit_arm64_a32_memory.cpp`.

use crate::backend::arm64::abi::XSTATE;
use crate::backend::arm64::block_of_code::BlockOfCode;
use crate::backend::arm64::emit_arm64_memory::{
    emit_exclusive_read_memory, emit_exclusive_write_memory, emit_read_memory, emit_write_memory,
};
use crate::backend::arm64::emit_context::EmitContext;
use crate::backend::arm64::inst;
use crate::backend::arm64::jit_state::A32JitState;
use crate::ir::value::InstRef;

const WZR: u8 = 31;

pub fn emit_a32_clear_exclusive(code: &mut BlockOfCode) -> Result<(), String> {
    code.write_u32(inst::str_w_unsigned(
        WZR,
        XSTATE,
        core::mem::offset_of!(A32JitState, exclusive_state) as u32,
    ))?;
    Ok(())
}

pub fn emit_a32_read_memory<const BITSIZE: usize>(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_read_memory::<BITSIZE>(code, ctx, inst_ref)
}

pub fn emit_a32_exclusive_read_memory<const BITSIZE: usize>(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_exclusive_read_memory::<BITSIZE>(code, ctx, inst_ref)
}

pub fn emit_a32_write_memory<const BITSIZE: usize>(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_write_memory::<BITSIZE>(code, ctx, inst_ref)
}

pub fn emit_a32_exclusive_write_memory<const BITSIZE: usize>(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_exclusive_write_memory::<BITSIZE>(code, ctx, inst_ref)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::arm64::emit_arm64::{EmitConfig, EmittedBlockInfo, LinkTarget, Relocation};
    use crate::backend::arm64::fastmem::FastmemManager;
    use crate::backend::arm64::fpsr_manager::FpsrManager;
    use crate::backend::arm64::reg_alloc::RegAlloc;
    use crate::interface::a32::config::{
        Exception as A32Exception, UserCallbacks as A32UserCallbacks, UserConfig as A32UserConfig,
    };
    use crate::interface::optimization_flags::OptimizationFlag;
    use crate::ir::acc_type::AccType;
    use crate::ir::block::Block;
    use crate::ir::inst::Inst;
    use crate::ir::location::{A32LocationDescriptor, LocationDescriptor};
    use crate::ir::opcode::Opcode;
    use crate::ir::terminal::Terminal;
    use crate::ir::value::Value;

    struct DummyCallbacks;

    impl A32UserCallbacks for DummyCallbacks {
        fn memory_read_code(&self, _vaddr: u32) -> Option<u32> {
            None
        }

        fn memory_read_8(&self, _vaddr: u32) -> u8 {
            0
        }

        fn memory_read_16(&self, _vaddr: u32) -> u16 {
            0
        }

        fn memory_read_32(&self, _vaddr: u32) -> u32 {
            0
        }

        fn memory_read_64(&self, _vaddr: u32) -> u64 {
            0
        }

        fn memory_write_8(&mut self, _vaddr: u32, _value: u8) {}
        fn memory_write_16(&mut self, _vaddr: u32, _value: u16) {}
        fn memory_write_32(&mut self, _vaddr: u32, _value: u32) {}
        fn memory_write_64(&mut self, _vaddr: u32, _value: u64) {}

        fn call_svc(&mut self, _svc_num: u32) {}
        fn exception_raised(&mut self, _pc: u32, _exception: A32Exception) {}
        fn add_ticks(&mut self, _ticks: u64) {}

        fn get_ticks_remaining(&self) -> u64 {
            0
        }
    }

    fn config() -> EmitConfig {
        let mut config = A32UserConfig::new(Box::new(DummyCallbacks));
        config.enable_cycle_counting = false;
        config.code_cache_size = 0;
        config.optimizations = OptimizationFlag::NO_OPTIMIZATIONS;
        config.check_halt_on_memory_access = true;
        EmitConfig::from_a32_config(&config)
    }

    fn block_with_inst(opcode: Opcode, args: &[Value]) -> Block {
        let location = A32LocationDescriptor::at(0x4000).to_location();
        let mut block = Block::new(location);
        block.push_inst(Inst::new(opcode, args));
        block.terminal = Terminal::ReturnToDispatch;
        block
    }

    fn context_emit(
        block: &mut Block,
        code: &mut BlockOfCode,
        emitted_block_info: &mut EmittedBlockInfo,
        config: &EmitConfig,
        emit: impl FnOnce(&mut BlockOfCode, &mut EmitContext<'_>, InstRef) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut reg_alloc = RegAlloc::default();
        let mut fpsr = FpsrManager::new(config.state_fpsr_offset);
        let mut fastmem = FastmemManager::default();
        let mut ctx = EmitContext {
            block,
            reg_alloc: &mut reg_alloc,
            conf: config,
            emitted_block_info,
            fpsr: &mut fpsr,
            fastmem: &mut fastmem,
            deferred_emits: Vec::new(),
        };
        emit(code, &mut ctx, InstRef(0))
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

    fn read_instruction(code: &BlockOfCode, offset: usize) -> u32 {
        unsafe {
            code.code_base_ptr()
                .add(offset)
                .cast::<u32>()
                .read_unaligned()
        }
    }

    #[test]
    fn clear_exclusive_stores_wzr_to_a32_exclusive_state() {
        let mut code = BlockOfCode::with_size(4096).unwrap();

        emit_a32_clear_exclusive(&mut code).unwrap();

        assert_eq!(
            read_instruction(&code, 0),
            inst::str_w_unsigned(
                WZR,
                XSTATE,
                core::mem::offset_of!(A32JitState, exclusive_state) as u32
            )
        );
    }

    #[test]
    fn a32_read_memory_wrapper_uses_common_callback_only_emitter() {
        let config = config();
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut info = empty_block_info(&code);
        let mut block = block_with_inst(
            Opcode::A32ReadMemory32,
            &[
                Value::ImmU64(LocationDescriptor::new(0x4000).value()),
                Value::ImmU32(0x1234),
                Value::ImmAccType(AccType::Normal),
            ],
        );

        context_emit(
            &mut block,
            &mut code,
            &mut info,
            &config,
            |code, ctx, inst| emit_a32_read_memory::<32>(code, ctx, inst),
        )
        .unwrap();

        assert_eq!(
            info.relocations,
            vec![Relocation {
                code_offset: 4,
                target: LinkTarget::ReadMemory32,
            }]
        );
        assert_eq!(read_instruction(&code, 0), inst::movz_x(1, 0x1234, 0));
        assert_eq!(read_instruction(&code, 4), inst::nop());
    }
}
