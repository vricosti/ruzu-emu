use crate::halt_reason::HaltReason;
use crate::ir::location::A32LocationDescriptor;
use crate::jit_config::JitConfig;

use super::a32_address_space::A32AddressSpace;
use super::jit_state::A32JitState;

/// A32 ARM64 execution core.
///
/// Upstream owner: `backend/arm64/a32_core.h`.
pub struct A32Core;

impl A32Core {
    pub fn new(_config: &JitConfig) -> Self {
        Self
    }

    pub fn run(
        &mut self,
        process: &mut A32AddressSpace,
        thread_ctx: &mut A32JitState,
        halt_reason: *mut u32,
    ) -> Result<HaltReason, String> {
        let location_descriptor = thread_ctx.get_location_descriptor();
        let entry_point = process.get_or_emit(location_descriptor)?;
        let result = unsafe {
            (process.address_space().prelude_info().run_code)(
                entry_point,
                (thread_ctx as *mut A32JitState).cast(),
                halt_reason,
            )
        };
        Ok(HaltReason::from_bits_truncate(result))
    }

    pub fn step(
        &mut self,
        process: &mut A32AddressSpace,
        thread_ctx: &mut A32JitState,
        halt_reason: *mut u32,
    ) -> Result<HaltReason, String> {
        let location_descriptor =
            A32LocationDescriptor::from_location(thread_ctx.get_location_descriptor())
                .set_single_stepping(true)
                .to_location();
        let entry_point = process.get_or_emit(location_descriptor)?;
        let result = unsafe {
            (process.address_space().prelude_info().step_code)(
                entry_point,
                (thread_ctx as *mut A32JitState).cast(),
                halt_reason,
            )
        };
        Ok(HaltReason::from_bits_truncate(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::arm64::emit_arm64::EmittedBlockInfo;
    use crate::backend::common::emit_context::MemoryEmitConfig;
    use crate::frontend::a32::fpscr::FPSCR;
    use crate::frontend::a32::psr::PSR;
    use crate::ir::location::LocationDescriptor;
    use crate::jit_config::{OptimizationFlag, UserCallbacks};
    use std::collections::HashMap;

    struct TestCallbacks;

    impl UserCallbacks for TestCallbacks {
        fn memory_read_code(&self, _vaddr: u64) -> Option<u32> {
            Some(0xE12F_FF1E) // BX LR: a real terminal instruction for compile-only tests.
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

    fn config() -> JitConfig {
        JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(TestCallbacks),
            enable_cycle_counting: false,
            code_cache_size: 4096,
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
        }
    }

    #[test]
    fn get_or_emit_compiles_current_location_descriptor() {
        let conf = config();
        let mut process = A32AddressSpace::new(conf).unwrap();
        let mut state = A32JitState::new();
        state.regs[15] = 0x1000;
        let expected = state.get_location_descriptor();

        let entry_point = process.get_or_emit(expected).unwrap();

        assert!(!entry_point.is_null());
        assert!(process.address_space().get(expected).is_some());
    }

    #[test]
    fn get_or_emit_compiles_single_stepping_location_descriptor() {
        let conf = config();
        let mut process = A32AddressSpace::new(conf).unwrap();
        let mut state = A32JitState::new();
        state.regs[15] = 0x1000;
        state.set_cpsr(0);
        state.set_fpscr(0);
        let expected = A32LocationDescriptor::new(0x1000, PSR::default(), FPSCR::default(), true)
            .to_location();

        let entry_point = process.get_or_emit(expected).unwrap();

        assert!(!entry_point.is_null());
        assert!(process.address_space().get(expected).is_some());
    }

    #[test]
    #[cfg(target_arch = "aarch64")]
    fn run_existing_block_calls_arm64_prelude() {
        let conf = config();
        let mut core = A32Core::new(&conf);
        let mut process = A32AddressSpace::new(conf).unwrap();
        let mut state = A32JitState::new();
        state.regs[15] = 0x1000;
        let location = state.get_location_descriptor();

        let mut block_code = super::super::block_of_code::BlockOfCode::with_size(4096).unwrap();
        block_code
            .write_u32(super::super::inst::movz_w(0, 0x42, 0))
            .unwrap();
        block_code.write_u32(super::super::inst::ret_lr()).unwrap();
        block_code.seal();

        let block_info = EmittedBlockInfo {
            entry_point: block_code.code_base_ptr(),
            size: 8,
            relocations: vec![],
            block_relocations: crate::backend::arm64::fast_hash::FastHashMap::default(),
            fastmem_patch_info: crate::backend::arm64::fast_hash::FastHashMap::default(),
        };
        process
            .address_space_mut()
            .insert_emitted_block(location, block_info)
            .unwrap();

        let mut halt_reason = 0u32;
        let result = core
            .run(&mut process, &mut state, &mut halt_reason)
            .unwrap();

        assert_eq!(result, HaltReason::empty());
        assert_eq!(halt_reason, 0);
    }
}
