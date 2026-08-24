use crate::interface::a64::config::UserConfig;
use crate::interface::halt_reason::HaltReason;
use crate::ir::location::A64LocationDescriptor;

use super::a64_address_space::A64AddressSpace;
use super::jit_state::A64JitState;

/// A64 ARM64 execution core.
///
/// Upstream owner: `backend/arm64/a64_core.h`.
pub struct A64Core;

impl A64Core {
    pub fn new(_config: &UserConfig) -> Self {
        Self
    }

    pub fn run(
        &mut self,
        process: &mut A64AddressSpace,
        thread_ctx: &mut A64JitState,
        halt_reason: *mut u32,
    ) -> Result<HaltReason, String> {
        let location_descriptor = thread_ctx.get_location_descriptor();
        let entry_point = process.get_or_emit(location_descriptor)?;
        let result = unsafe {
            (process.address_space().prelude_info().run_code)(
                entry_point,
                (thread_ctx as *mut A64JitState).cast(),
                halt_reason,
            )
        };
        Ok(HaltReason::from_bits_truncate(result))
    }

    pub fn step(
        &mut self,
        process: &mut A64AddressSpace,
        thread_ctx: &mut A64JitState,
        halt_reason: *mut u32,
    ) -> Result<HaltReason, String> {
        let location_descriptor =
            A64LocationDescriptor::from_location(thread_ctx.get_location_descriptor())
                .set_single_stepping(true)
                .to_location();
        let entry_point = process.get_or_emit(location_descriptor)?;
        let result = unsafe {
            (process.address_space().prelude_info().step_code)(
                entry_point,
                (thread_ctx as *mut A64JitState).cast(),
                halt_reason,
            )
        };
        Ok(HaltReason::from_bits_truncate(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::arm64::a64_address_space::A64CallbackContext;
    use crate::backend::arm64::emit_arm64::EmittedBlockInfo;
    use crate::interface::a64::config::{
        Exception as A64Exception, UserCallbacks as A64UserCallbacks, Vector as A64Vector,
    };
    use crate::interface::optimization_flags::OptimizationFlag;
    use crate::ir::location::LocationDescriptor;
    use std::collections::HashMap;

    struct TestCallbacks {
        code: HashMap<u64, u32>,
    }

    impl A64UserCallbacks for TestCallbacks {
        fn memory_read_code(&self, vaddr: u64) -> Option<u32> {
            self.code.get(&vaddr).copied()
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

        fn memory_read_128(&self, _vaddr: u64) -> A64Vector {
            [0, 0]
        }

        fn memory_write_8(&mut self, _vaddr: u64, _value: u8) {}
        fn memory_write_16(&mut self, _vaddr: u64, _value: u16) {}
        fn memory_write_32(&mut self, _vaddr: u64, _value: u32) {}
        fn memory_write_64(&mut self, _vaddr: u64, _value: u64) {}
        fn memory_write_128(&mut self, _vaddr: u64, _value: A64Vector) {}

        fn call_svc(&mut self, _svc_num: u32) {}
        fn exception_raised(&mut self, _pc: u64, _exception: A64Exception) {}
        fn add_ticks(&mut self, _ticks: u64) {}

        fn get_ticks_remaining(&self) -> u64 {
            0
        }

        fn get_cntpct(&self) -> u64 {
            0
        }
    }

    fn config() -> UserConfig {
        config_with_code(HashMap::from([(0x1000, 0x1400_0002)]))
    }

    fn config_with_code(code: HashMap<u64, u32>) -> UserConfig {
        let mut config = UserConfig::new(Box::new(TestCallbacks { code }));
        config.enable_cycle_counting = false;
        config.code_cache_size = 4096;
        config.optimizations = OptimizationFlag::NO_OPTIMIZATIONS;
        config
    }

    #[test]
    fn get_or_emit_compiles_current_location_descriptor() {
        let conf = config();
        let mut process = A64AddressSpace::new(conf).unwrap();
        let mut state = A64JitState::new();
        state.pc = 0x1000;
        let expected = state.get_location_descriptor();

        let entry_point = process.get_or_emit(expected).unwrap();

        assert!(!entry_point.is_null());
        assert!(process.address_space().get(expected).is_some());
    }

    #[test]
    fn get_or_emit_compiles_single_stepping_location_descriptor() {
        let conf = config();
        let mut process = A64AddressSpace::new(conf).unwrap();
        let expected = A64LocationDescriptor::new(0x1000, 0x0040_0000, true).to_location();

        let entry_point = process.get_or_emit(expected).unwrap();

        assert!(!entry_point.is_null());
        assert!(process.address_space().get(expected).is_some());
    }

    #[test]
    #[cfg(target_arch = "aarch64")]
    fn run_existing_block_calls_arm64_prelude() {
        let conf = config();
        let mut core = A64Core::new(&conf);
        let mut process = A64AddressSpace::new(conf).unwrap();
        let mut state = A64JitState::new();
        state.pc = 0x1000;
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

    #[test]
    #[cfg(target_arch = "aarch64")]
    fn get_set_elimination_preserves_value_live_across_memory_callback() {
        let mut conf = config_with_code(HashMap::from([
            (0x1000, 0xf940_0261), // ldr x1, [x19]
            (0x1004, 0xaa13_03e0), // mov x0, x19
            (0x1008, 0xd61f_0040), // br x2
        ]));
        conf.optimizations = OptimizationFlag::GET_SET_ELIMINATION;

        let mut core = A64Core::new(&conf);
        let mut process = A64AddressSpace::new(conf).unwrap();
        let mut state = A64JitState::new();
        state.pc = 0x1000;
        state.reg[2] = 0x2000;
        state.reg[19] = 0x18;
        let mut runtime_callbacks = TestCallbacks {
            code: HashMap::new(),
        };
        let a64_callbacks: &mut dyn crate::interface::a64::config::UserCallbacks =
            &mut runtime_callbacks;
        let callbacks_ptr = a64_callbacks as *mut dyn crate::interface::a64::config::UserCallbacks;
        let mut callback_context = A64CallbackContext::new(&mut state, callbacks_ptr, None, 0);
        process
            .emit_callback_trampolines(
                (&mut callback_context as *mut A64CallbackContext).cast(),
                A64CallbackContext::callback_fns(),
            )
            .unwrap();

        let mut halt_reason = 0u32;
        core.run(&mut process, &mut state, &mut halt_reason)
            .unwrap();

        assert_eq!(state.reg[0], 0x18);
        assert_eq!(state.pc, 0x2000);
    }

    #[test]
    #[cfg(target_arch = "aarch64")]
    fn get_set_elimination_restores_spilled_values_after_memory_callback() {
        let mut code = HashMap::new();
        let mut pc = 0x1000;
        for reg in 3..=23 {
            code.insert(pc, 0x9100_0400 | (reg << 5) | reg); // add xN, xN, #1
            pc += 4;
        }
        code.insert(pc, 0xf940_0261); // ldr x1, [x19]
        pc += 4;
        code.insert(pc, 0xaa13_03e0); // mov x0, x19
        pc += 4;
        for reg in 3..=23 {
            code.insert(pc, 0x9100_0400 | (reg << 5) | reg); // add xN, xN, #1
            pc += 4;
        }
        code.insert(pc, 0xd61f_0040); // br x2

        let mut conf = config_with_code(code);
        conf.code_cache_size = 64 * 1024;
        conf.optimizations = OptimizationFlag::GET_SET_ELIMINATION;
        let page_table = vec![0u64; 16];
        conf.page_table = Some(page_table.as_ptr() as *mut *mut std::ffi::c_void);
        conf.page_table_address_space_bits = 16;
        conf.silently_mirror_page_table = true;

        let mut core = A64Core::new(&conf);
        let mut process = A64AddressSpace::new(conf).unwrap();
        let mut state = A64JitState::new();
        state.pc = 0x1000;
        state.reg[2] = 0x2000;
        for reg in 3..=23 {
            state.reg[reg as usize] = reg as u64 * 0x10;
        }
        let mut runtime_callbacks = TestCallbacks {
            code: HashMap::new(),
        };
        let a64_callbacks: &mut dyn crate::interface::a64::config::UserCallbacks =
            &mut runtime_callbacks;
        let callbacks_ptr = a64_callbacks as *mut dyn crate::interface::a64::config::UserCallbacks;
        let mut callback_context = A64CallbackContext::new(&mut state, callbacks_ptr, None, 0);
        process
            .emit_callback_trampolines(
                (&mut callback_context as *mut A64CallbackContext).cast(),
                A64CallbackContext::callback_fns(),
            )
            .unwrap();

        let mut halt_reason = 0u32;
        core.run(&mut process, &mut state, &mut halt_reason)
            .unwrap();

        assert_eq!(state.reg[0], 0x131);
        for reg in 3..=23 {
            assert_eq!(state.reg[reg as usize], reg as u64 * 0x10 + 2);
        }
        assert_eq!(state.pc, 0x2000);
    }
}
