use std::ops::RangeInclusive;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use crate::interface::a64::config::{UserCallbacks, UserConfig, Vector};
use crate::interface::halt_reason::HaltReason;

use super::a64_address_space::{A64AddressSpace, A64CallbackContext};
use super::a64_core::A64Core;
use super::jit_state::{A64JitState, A64VecRegs};

/// A64 ARM64 backend interface state.
///
/// Upstream owner: `backend/arm64/a64_interface.cpp`.
pub struct A64Interface {
    inner: Box<A64InterfaceInner>,
}

pub struct A64InterfaceInner {
    current_state: A64JitState,
    current_address_space: A64AddressSpace,
    callback_context: Option<A64CallbackContext>,
    core: A64Core,
    halt_reason: AtomicU32,
    invalidation: Mutex<A64Invalidation>,
    is_executing: bool,
}

impl Deref for A64Interface {
    type Target = A64InterfaceInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for A64Interface {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

#[derive(Default)]
struct A64Invalidation {
    invalid_cache_ranges: Vec<RangeInclusive<u64>>,
    invalidate_entire_cache: bool,
}

impl A64Interface {
    pub fn new(config: impl Into<UserConfig>) -> Result<Self, String> {
        let config = config.into();
        let current_address_space = A64AddressSpace::new(config)?;
        let core = A64Core::new(current_address_space.config());
        let mut interface = Self {
            inner: Box::new(A64InterfaceInner {
                current_state: A64JitState::new(),
                current_address_space,
                callback_context: None,
                core,
                halt_reason: AtomicU32::new(0),
                invalidation: Mutex::new(A64Invalidation::default()),
                is_executing: false,
            }),
        };
        interface.install_callback_trampolines()?;
        interface.install_callback_state_pointers();
        Ok(interface)
    }

    pub fn run(&mut self) -> Result<HaltReason, String> {
        assert!(!self.is_executing, "Recursive JIT execution not allowed");
        self.perform_requested_cache_invalidation(self.current_halt_reason())?;
        let inner = &mut self.inner;
        inner.is_executing = true;
        let halt_reason = inner.halt_reason.as_ptr();
        let hr = inner.core.run(
            &mut inner.current_address_space,
            &mut inner.current_state,
            halt_reason,
        );
        match hr {
            Ok(hr) => {
                let invalidation_result = self.perform_requested_cache_invalidation(hr);
                self.is_executing = false;
                invalidation_result?;
                Ok(hr)
            }
            Err(err) => {
                self.is_executing = false;
                Err(err)
            }
        }
    }

    pub fn step(&mut self) -> Result<HaltReason, String> {
        assert!(!self.is_executing, "Recursive JIT execution not allowed");
        self.perform_requested_cache_invalidation(self.current_halt_reason())?;
        let inner = &mut self.inner;
        inner.is_executing = true;
        let halt_reason = inner.halt_reason.as_ptr();
        let hr = inner.core.step(
            &mut inner.current_address_space,
            &mut inner.current_state,
            halt_reason,
        );
        match hr {
            Ok(hr) => {
                let invalidation_result = self.perform_requested_cache_invalidation(hr);
                self.is_executing = false;
                invalidation_result?;
                Ok(hr)
            }
            Err(err) => {
                self.is_executing = false;
                Err(err)
            }
        }
    }

    pub fn clear_cache(&self) {
        let mut invalidation = self.invalidation.lock().expect("A64 invalidation poisoned");
        invalidation.invalidate_entire_cache = true;
        self.halt_execution(HaltReason::CACHE_INVALIDATION);
    }

    pub fn invalidate_cache_range(&self, start_address: u64, length: usize) {
        let end_address = start_address.wrapping_add(length as u64).wrapping_sub(1);
        let mut invalidation = self.invalidation.lock().expect("A64 invalidation poisoned");
        invalidation
            .invalid_cache_ranges
            .push(start_address..=end_address);
        self.halt_execution(HaltReason::CACHE_INVALIDATION);
    }

    pub fn reset(&mut self) {
        self.current_state = A64JitState::new();
    }

    pub fn halt_execution(&self, hr: HaltReason) {
        self.halt_reason.fetch_or(hr.bits(), Ordering::SeqCst);
    }

    pub fn clear_halt(&self, hr: HaltReason) {
        self.halt_reason.fetch_and(!hr.bits(), Ordering::SeqCst);
    }

    pub fn pc(&self) -> u64 {
        self.current_state.pc
    }

    pub fn set_pc(&mut self, value: u64) {
        self.current_state.pc = value;
    }

    pub fn sp(&self) -> u64 {
        self.current_state.sp
    }

    pub fn set_sp(&mut self, value: u64) {
        self.current_state.sp = value;
    }

    pub fn regs(&self) -> &[u64; 31] {
        &self.current_state.reg
    }

    pub fn regs_mut(&mut self) -> &mut [u64; 31] {
        &mut self.current_state.reg
    }

    pub fn get_register(&self, index: usize) -> u64 {
        self.regs()[index]
    }

    pub fn set_register(&mut self, index: usize, value: u64) {
        self.regs_mut()[index] = value;
    }

    pub fn get_registers(&self) -> [u64; 31] {
        *self.regs()
    }

    pub fn set_registers(&mut self, value: [u64; 31]) {
        *self.regs_mut() = value;
    }

    pub fn vec_regs(&self) -> &A64VecRegs {
        &self.current_state.vec
    }

    pub fn vec_regs_mut(&mut self) -> &mut A64VecRegs {
        &mut self.current_state.vec
    }

    pub fn get_vector(&self, index: usize) -> Vector {
        let vec = &self.vec_regs().0;
        [vec[index * 2], vec[index * 2 + 1]]
    }

    pub fn set_vector(&mut self, index: usize, value: Vector) {
        let vec = &mut self.vec_regs_mut().0;
        vec[index * 2] = value[0];
        vec[index * 2 + 1] = value[1];
    }

    pub fn get_vectors(&self) -> [Vector; 32] {
        std::array::from_fn(|index| self.get_vector(index))
    }

    pub fn set_vectors(&mut self, value: [Vector; 32]) {
        for (index, vector) in value.into_iter().enumerate() {
            self.set_vector(index, vector);
        }
    }

    pub fn fpcr(&self) -> u32 {
        self.current_state.fpcr
    }

    pub fn set_fpcr(&mut self, value: u32) {
        self.current_state.fpcr = value;
    }

    pub fn fpsr(&self) -> u32 {
        self.current_state.fpsr
    }

    pub fn set_fpsr(&mut self, value: u32) {
        self.current_state.fpsr = value;
    }

    pub fn pstate(&self) -> u32 {
        self.current_state.cpsr_nzcv
    }

    pub fn set_pstate(&mut self, value: u32) {
        self.current_state.cpsr_nzcv = value;
    }

    pub fn clear_exclusive_state(&mut self) {
        self.current_state.exclusive_state = 0;
    }

    pub fn current_halt_reason(&self) -> HaltReason {
        HaltReason::from_bits_truncate(self.halt_reason.load(Ordering::SeqCst))
    }

    pub fn halt_reason_ptr(&self) -> *const u32 {
        self.halt_reason.as_ptr() as *const u32
    }

    pub fn jit_state_ptr(&self) -> *const u8 {
        &self.current_state as *const A64JitState as *const u8
    }

    pub fn is_executing(&self) -> bool {
        self.is_executing
    }

    pub fn disassemble(&self) -> String {
        String::new()
    }

    #[cfg(test)]
    pub(crate) fn current_address_space(&self) -> &A64AddressSpace {
        &self.current_address_space
    }

    fn perform_requested_cache_invalidation(&mut self, hr: HaltReason) -> Result<(), String> {
        if !hr.contains(HaltReason::CACHE_INVALIDATION) {
            return Ok(());
        }

        let inner = &mut self.inner;
        let mut invalidation = inner
            .invalidation
            .lock()
            .expect("A64 invalidation poisoned");
        inner
            .halt_reason
            .fetch_and(!HaltReason::CACHE_INVALIDATION.bits(), Ordering::SeqCst);

        if invalidation.invalidate_entire_cache {
            inner
                .current_address_space
                .address_space_mut()
                .clear_cache()?;
            invalidation.invalidate_entire_cache = false;
            invalidation.invalid_cache_ranges.clear();
            return Ok(());
        }

        if !invalidation.invalid_cache_ranges.is_empty() {
            inner
                .current_address_space
                .invalidate_cache_ranges(&invalidation.invalid_cache_ranges);
            invalidation.invalid_cache_ranges.clear();
        }

        Ok(())
    }

    fn install_callback_state_pointers(&mut self) {
        let halt_ptr = self.halt_reason.as_ptr() as *const u32;
        let pc_ptr = &self.current_state.pc as *const u64 as *const u32;
        let callbacks = &mut self.current_address_space.config_mut().callbacks;
        callbacks.set_halt_reason_ptr(halt_ptr);
        callbacks.set_pc_ptr(pc_ptr);
    }

    fn install_callback_trampolines(&mut self) -> Result<(), String> {
        let inner = &mut self.inner;
        let global_monitor = inner.current_address_space.config().global_monitor;
        let processor_id = inner.current_address_space.config().processor_id;
        let callbacks = {
            let callbacks = &mut inner.current_address_space.config_mut().callbacks;
            callbacks.as_mut() as *mut dyn UserCallbacks
        };
        inner.callback_context = Some(A64CallbackContext::new(
            &mut inner.current_state,
            callbacks,
            global_monitor,
            processor_id as usize,
        ));
        let callback_context_ptr = inner
            .callback_context
            .as_mut()
            .expect("A64 callback context was just installed")
            as *mut A64CallbackContext
            as *const std::ffi::c_void;
        inner
            .current_address_space
            .emit_callback_trampolines(callback_context_ptr, A64CallbackContext::callback_fns())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface::a64::config::{Exception as A64Exception, Vector as A64Vector};
    use crate::interface::optimization_flags::OptimizationFlag;
    use std::sync::Arc;

    #[derive(Default)]
    struct PointerState {
        halt_reason_ptr: usize,
        pc_ptr: usize,
    }

    struct TestCallbacks {
        pointers: Option<Arc<Mutex<PointerState>>>,
    }

    impl UserCallbacks for TestCallbacks {
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

        fn set_halt_reason_ptr(&mut self, ptr: *const u32) {
            if let Some(pointers) = &self.pointers {
                pointers.lock().unwrap().halt_reason_ptr = ptr as usize;
            }
        }

        fn set_pc_ptr(&mut self, ptr: *const u32) {
            if let Some(pointers) = &self.pointers {
                pointers.lock().unwrap().pc_ptr = ptr as usize;
            }
        }
    }

    fn config_with_pointers(pointers: Option<Arc<Mutex<PointerState>>>) -> UserConfig {
        let mut config = UserConfig::new(Box::new(TestCallbacks { pointers }));
        config.enable_cycle_counting = false;
        config.code_cache_size = 4096;
        config.optimizations = OptimizationFlag::NO_OPTIMIZATIONS;
        config
    }

    fn config() -> UserConfig {
        config_with_pointers(None)
    }

    #[test]
    fn run_performs_deferred_clear_cache_before_execution() {
        let mut interface = A64Interface::new(config()).unwrap();
        interface.set_pc(0x1000);
        interface.clear_cache();
        interface.halt_execution(HaltReason::EXTERNAL_HALT);

        let result = interface.run().unwrap();

        assert!(result.contains(HaltReason::EXTERNAL_HALT));
        assert!(!interface
            .current_halt_reason()
            .contains(HaltReason::CACHE_INVALIDATION));
    }

    #[test]
    fn register_accessors_use_current_state() {
        let mut interface = A64Interface::new(config()).unwrap();
        interface.set_pc(0x1000);
        interface.set_sp(0x2000);
        interface.set_register(0, 0x1234);
        interface.set_registers(std::array::from_fn(|index| index as u64));
        interface.set_vector(0, [0x1234, 0x5678]);
        interface.set_vectors(std::array::from_fn(|index| [index as u64, !(index as u64)]));
        interface.set_fpcr(0x0040_0000);
        interface.set_fpsr(0x0800_0000);
        interface.set_pstate(0xF000_0000);

        assert_eq!(interface.pc(), 0x1000);
        assert_eq!(interface.sp(), 0x2000);
        assert_eq!(interface.get_register(0), 0);
        assert_eq!(
            interface.get_registers(),
            std::array::from_fn(|index| index as u64)
        );
        assert_eq!(interface.get_vector(0), [0, u64::MAX]);
        assert_eq!(
            interface.get_vectors(),
            std::array::from_fn(|index| [index as u64, !(index as u64)])
        );
        assert_eq!(interface.fpcr(), 0x0040_0000);
        assert_eq!(interface.fpsr(), 0x0800_0000);
        assert_eq!(interface.pstate(), 0xF000_0000);
        assert!(interface.disassemble().is_empty());
    }

    #[test]
    fn invalidation_range_uses_upstream_unsigned_arithmetic() {
        let interface = A64Interface::new(config()).unwrap();
        interface.invalidate_cache_range(5, 0);
        let invalidation = interface.invalidation.lock().unwrap();

        assert_eq!(invalidation.invalid_cache_ranges, vec![5..=4]);
    }

    #[test]
    fn constructor_installs_callback_state_pointers() {
        let pointers = Arc::new(Mutex::new(PointerState::default()));
        let interface = A64Interface::new(config_with_pointers(Some(pointers.clone()))).unwrap();
        let pointers = pointers.lock().unwrap();

        assert_eq!(
            pointers.halt_reason_ptr,
            interface.halt_reason.as_ptr() as *const u32 as usize
        );
        assert_eq!(
            pointers.pc_ptr,
            &interface.current_state.pc as *const u64 as *const u32 as usize
        );
    }

    #[test]
    fn constructor_installs_a64_callback_trampolines() {
        let interface = A64Interface::new(config()).unwrap();
        let prelude = interface
            .current_address_space()
            .address_space()
            .prelude_info();

        assert!(prelude.read_memory_8.is_some());
        assert!(prelude.read_memory_16.is_some());
        assert!(prelude.read_memory_32.is_some());
        assert!(prelude.read_memory_64.is_some());
        assert!(prelude.read_memory_128.is_some());
        assert!(prelude.wrapped_read_memory_8.is_some());
        assert!(prelude.wrapped_read_memory_16.is_some());
        assert!(prelude.wrapped_read_memory_32.is_some());
        assert!(prelude.wrapped_read_memory_64.is_some());
        assert!(prelude.wrapped_read_memory_128.is_some());
        assert!(prelude.exclusive_read_memory_8.is_some());
        assert!(prelude.exclusive_read_memory_16.is_some());
        assert!(prelude.exclusive_read_memory_32.is_some());
        assert!(prelude.exclusive_read_memory_64.is_some());
        assert!(prelude.exclusive_read_memory_128.is_some());
        assert!(prelude.write_memory_8.is_some());
        assert!(prelude.write_memory_16.is_some());
        assert!(prelude.write_memory_32.is_some());
        assert!(prelude.write_memory_64.is_some());
        assert!(prelude.write_memory_128.is_some());
        assert!(prelude.wrapped_write_memory_8.is_some());
        assert!(prelude.wrapped_write_memory_16.is_some());
        assert!(prelude.wrapped_write_memory_32.is_some());
        assert!(prelude.wrapped_write_memory_64.is_some());
        assert!(prelude.wrapped_write_memory_128.is_some());
        assert!(prelude.exclusive_write_memory_8.is_some());
        assert!(prelude.exclusive_write_memory_16.is_some());
        assert!(prelude.exclusive_write_memory_32.is_some());
        assert!(prelude.exclusive_write_memory_64.is_some());
        assert!(prelude.exclusive_write_memory_128.is_some());
        assert!(prelude.call_svc.is_some());
        assert!(prelude.exception_raised.is_some());
        assert!(prelude.isb_raised.is_some());
        assert!(prelude.ic_raised.is_some());
        assert!(prelude.dc_raised.is_some());
        assert!(prelude.get_cntpct.is_some());
        assert!(prelude.add_ticks.is_some());
        assert!(prelude.get_ticks_remaining.is_some());
    }
}
