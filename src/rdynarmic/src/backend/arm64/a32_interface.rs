use std::ops::RangeInclusive;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use crate::interface::a32::config::{
    UserCallbacks as A32UserCallbacks, UserConfig as A32UserConfig,
};
use crate::interface::halt_reason::HaltReason;

use super::a32_address_space::{A32AddressSpace, A32CallbackContext};
use super::a32_core::A32Core;
use super::jit_state::{A32ExtRegs, A32JitState};

/// A32 ARM64 backend interface state.
///
/// Upstream owner: `backend/arm64/a32_interface.cpp`.
pub struct A32Interface {
    inner: Box<A32InterfaceInner>,
}

pub struct A32InterfaceInner {
    current_state: A32JitState,
    current_address_space: A32AddressSpace,
    callback_context: Option<A32CallbackContext>,
    core: A32Core,
    halt_reason: AtomicU32,
    invalidation: Mutex<A32Invalidation>,
}

impl Deref for A32Interface {
    type Target = A32InterfaceInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for A32Interface {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

#[derive(Default)]
struct A32Invalidation {
    invalid_cache_ranges: Vec<RangeInclusive<u32>>,
    invalidate_entire_cache: bool,
}

impl A32Interface {
    /// Diagnostic passthrough for host-profile attribution.
    pub fn dump_block_map(&self, out: &mut dyn std::io::Write) -> std::io::Result<()> {
        self.inner.current_address_space.dump_block_map(out)
    }

    pub fn new(config: impl Into<A32UserConfig>) -> Result<Self, String> {
        let config = config.into();
        let current_address_space = A32AddressSpace::new_without_prelude(config)?;
        let core = A32Core::new(current_address_space.config());
        let mut interface = Self {
            inner: Box::new(A32InterfaceInner {
                current_state: A32JitState::new(),
                current_address_space,
                callback_context: None,
                core,
                halt_reason: AtomicU32::new(0),
                invalidation: Mutex::new(A32Invalidation::default()),
            }),
        };
        interface.install_callback_context();
        let callback_context_ptr = interface.callback_context_ptr();
        let callback_fns = A32CallbackContext::callback_fns();
        interface
            .current_address_space
            .emit_prelude_with_dispatcher(callback_context_ptr, callback_fns)?;
        interface.install_callback_trampolines()?;
        interface.install_callback_state_pointers();
        Ok(interface)
    }

    pub fn run(&mut self, is_executing: &mut bool) -> Result<HaltReason, String> {
        assert!(!*is_executing, "Recursive JIT execution not allowed");
        self.perform_requested_cache_invalidation(self.current_halt_reason())?;
        let inner = &mut self.inner;
        *is_executing = true;
        let halt_reason = inner.halt_reason.as_ptr();
        let hr = inner.core.run(
            &mut inner.current_address_space,
            &mut inner.current_state,
            halt_reason,
        );
        match hr {
            Ok(hr) => {
                let invalidation_result = self.perform_requested_cache_invalidation(hr);
                *is_executing = false;
                invalidation_result?;
                Ok(hr)
            }
            Err(err) => {
                *is_executing = false;
                Err(err)
            }
        }
    }

    pub fn step(&mut self, is_executing: &mut bool) -> Result<HaltReason, String> {
        assert!(!*is_executing, "Recursive JIT execution not allowed");
        self.perform_requested_cache_invalidation(self.current_halt_reason())?;
        let inner = &mut self.inner;
        *is_executing = true;
        let halt_reason = inner.halt_reason.as_ptr();
        let hr = inner.core.step(
            &mut inner.current_address_space,
            &mut inner.current_state,
            halt_reason,
        );
        match hr {
            Ok(hr) => {
                let invalidation_result = self.perform_requested_cache_invalidation(hr);
                *is_executing = false;
                invalidation_result?;
                Ok(hr)
            }
            Err(err) => {
                *is_executing = false;
                Err(err)
            }
        }
    }

    pub fn clear_cache(&self) {
        let mut invalidation = self.invalidation.lock().expect("A32 invalidation poisoned");
        invalidation.invalidate_entire_cache = true;
        self.halt_execution(HaltReason::CACHE_INVALIDATION);
    }

    pub fn invalidate_cache_range(&self, start_address: u32, length: usize) {
        let end_address = start_address.wrapping_add(length as u32).wrapping_sub(1);
        let mut invalidation = self.invalidation.lock().expect("A32 invalidation poisoned");
        invalidation
            .invalid_cache_ranges
            .push(start_address..=end_address);
        self.halt_execution(HaltReason::CACHE_INVALIDATION);
    }

    pub fn reset(&mut self) {
        self.current_state = A32JitState::new();
    }

    pub fn halt_execution(&self, hr: HaltReason) {
        self.halt_reason.fetch_or(hr.bits(), Ordering::SeqCst);
    }

    pub fn clear_halt(&self, hr: HaltReason) {
        self.halt_reason.fetch_and(!hr.bits(), Ordering::SeqCst);
    }

    pub fn regs(&self) -> &[u32; 16] {
        &self.current_state.regs
    }

    pub fn regs_mut(&mut self) -> &mut [u32; 16] {
        &mut self.current_state.regs
    }

    pub fn ext_regs(&self) -> &A32ExtRegs {
        &self.current_state.ext_regs
    }

    pub fn ext_regs_mut(&mut self) -> &mut A32ExtRegs {
        &mut self.current_state.ext_regs
    }

    pub fn cpsr(&self) -> u32 {
        self.current_state.cpsr()
    }

    pub fn set_cpsr(&mut self, value: u32) {
        self.current_state.set_cpsr(value);
    }

    pub fn fpscr(&self) -> u32 {
        self.current_state.fpscr()
    }

    pub fn set_fpscr(&mut self, value: u32) {
        self.current_state.set_fpscr(value);
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
        (&self.current_state as *const A32JitState).cast()
    }

    pub fn disassemble(&self) -> String {
        String::new()
    }

    pub fn compile_block_only(&mut self) -> Result<*const u8, String> {
        let location_descriptor = self.current_state.get_location_descriptor();
        Ok(self
            .current_address_space
            .get_or_emit(location_descriptor)?)
    }

    #[cfg(test)]
    pub(crate) fn current_address_space(&self) -> &A32AddressSpace {
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
            .expect("A32 invalidation poisoned");
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
        let pc_ptr = self.current_state.regs.as_ptr().wrapping_add(15);
        let upper_location_descriptor_ptr =
            &self.current_state.upper_location_descriptor as *const u32;
        let callbacks = &mut self.current_address_space.config_mut().callbacks;
        callbacks.set_halt_reason_ptr(halt_ptr);
        callbacks.set_pc_ptr(pc_ptr);
        callbacks.set_upper_location_descriptor_ptr(upper_location_descriptor_ptr);
    }

    fn install_callback_context(&mut self) {
        let inner = &mut self.inner;
        let global_monitor = inner.current_address_space.config().global_monitor;
        let processor_id = inner.current_address_space.config().processor_id;
        let callbacks = {
            let callbacks = &mut inner.current_address_space.config_mut().callbacks;
            callbacks.as_mut() as *mut dyn A32UserCallbacks
        };
        inner.callback_context = Some(A32CallbackContext::new(
            &mut inner.current_state,
            callbacks,
            global_monitor,
            processor_id as usize,
        ));
    }

    fn callback_context_ptr(&mut self) -> *const std::ffi::c_void {
        self.callback_context
            .as_mut()
            .expect("A32 callback context has not been installed")
            as *mut A32CallbackContext as *const std::ffi::c_void
    }

    fn install_callback_trampolines(&mut self) -> Result<(), String> {
        let inner = &mut self.inner;
        let callback_context_ptr = inner
            .callback_context
            .as_mut()
            .expect("A32 callback context has not been installed")
            as *mut A32CallbackContext
            as *const std::ffi::c_void;
        inner.current_address_space.emit_callback_trampolines(
            callback_context_ptr,
            callback_context_ptr,
            A32CallbackContext::callback_fns(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface::a32::config::Exception as A32Exception;
    use crate::interface::optimization_flags::OptimizationFlag;
    use std::sync::Arc;

    #[derive(Default)]
    struct PointerState {
        halt_reason_ptr: usize,
        pc_ptr: usize,
        upper_location_descriptor_ptr: usize,
    }

    struct TestCallbacks {
        pointers: Option<Arc<Mutex<PointerState>>>,
    }

    impl A32UserCallbacks for TestCallbacks {
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

        fn set_upper_location_descriptor_ptr(&mut self, ptr: *const u32) {
            if let Some(pointers) = &self.pointers {
                pointers.lock().unwrap().upper_location_descriptor_ptr = ptr as usize;
            }
        }
    }

    fn config_with_pointers(pointers: Option<Arc<Mutex<PointerState>>>) -> A32UserConfig {
        let mut config = A32UserConfig::new(Box::new(TestCallbacks { pointers }));
        config.enable_cycle_counting = false;
        config.code_cache_size = 4096;
        config.optimizations = OptimizationFlag::NO_OPTIMIZATIONS;
        config
    }

    fn config() -> A32UserConfig {
        config_with_pointers(None)
    }

    #[test]
    fn run_performs_deferred_clear_cache_before_execution() {
        let mut interface = A32Interface::new(config()).unwrap();
        interface.regs_mut()[15] = 0x1000;
        interface.clear_cache();
        interface.halt_execution(HaltReason::EXTERNAL_HALT);

        let mut is_executing = false;
        let result = interface.run(&mut is_executing).unwrap();

        assert!(result.contains(HaltReason::EXTERNAL_HALT));
        assert!(!is_executing);
        assert!(!interface
            .current_halt_reason()
            .contains(HaltReason::CACHE_INVALIDATION));
        assert!(!interface
            .current_halt_reason()
            .contains(HaltReason::EXTERNAL_HALT));
        assert!(
            interface
                .current_address_space()
                .address_space()
                .code()
                .code_size()
                > interface
                    .current_address_space()
                    .address_space()
                    .prelude_info()
                    .end_of_prelude
        );
    }

    #[test]
    fn register_accessors_use_current_state() {
        let mut interface = A32Interface::new(config()).unwrap();
        interface.regs_mut()[0] = 0x1234;
        interface.set_cpsr(0xF000_0010);
        interface.set_fpscr(0x0800_0000);

        assert_eq!(interface.regs()[0], 0x1234);
        assert_eq!(interface.cpsr() & 0xF000_0010, 0xF000_0010);
        assert_eq!(interface.fpscr() & 0x0800_0000, 0x0800_0000);
        assert!(interface.disassemble().is_empty());
    }

    #[test]
    fn invalidation_range_uses_upstream_unsigned_arithmetic() {
        let interface = A32Interface::new(config()).unwrap();
        interface.invalidate_cache_range(5, 0);
        let invalidation = interface.invalidation.lock().unwrap();

        assert_eq!(invalidation.invalid_cache_ranges, vec![5..=4]);
    }

    #[test]
    fn constructor_installs_callback_state_pointers() {
        let pointers = Arc::new(Mutex::new(PointerState::default()));
        let interface = A32Interface::new(config_with_pointers(Some(pointers.clone()))).unwrap();
        let pointers = pointers.lock().unwrap();

        assert_eq!(
            pointers.halt_reason_ptr,
            interface.halt_reason.as_ptr() as *const u32 as usize
        );
        assert_eq!(
            pointers.pc_ptr,
            interface.current_state.regs.as_ptr().wrapping_add(15) as usize
        );
        assert_eq!(
            pointers.upper_location_descriptor_ptr,
            &interface.current_state.upper_location_descriptor as *const u32 as usize
        );
    }

    #[test]
    fn constructor_installs_full_callback_trampolines() {
        let interface = A32Interface::new(config()).unwrap();
        let prelude = interface
            .current_address_space()
            .address_space()
            .prelude_info();

        assert!(prelude.read_memory_8.is_some());
        assert!(prelude.read_memory_16.is_some());
        assert!(prelude.read_memory_32.is_some());
        assert!(prelude.read_memory_64.is_some());
        assert!(prelude.wrapped_read_memory_8.is_some());
        assert!(prelude.wrapped_read_memory_16.is_some());
        assert!(prelude.wrapped_read_memory_32.is_some());
        assert!(prelude.wrapped_read_memory_64.is_some());
        assert!(prelude.exclusive_read_memory_8.is_some());
        assert!(prelude.exclusive_read_memory_16.is_some());
        assert!(prelude.exclusive_read_memory_32.is_some());
        assert!(prelude.exclusive_read_memory_64.is_some());
        assert!(prelude.write_memory_8.is_some());
        assert!(prelude.write_memory_16.is_some());
        assert!(prelude.write_memory_32.is_some());
        assert!(prelude.write_memory_64.is_some());
        assert!(prelude.wrapped_write_memory_8.is_some());
        assert!(prelude.wrapped_write_memory_16.is_some());
        assert!(prelude.wrapped_write_memory_32.is_some());
        assert!(prelude.wrapped_write_memory_64.is_some());
        assert!(prelude.exclusive_write_memory_8.is_some());
        assert!(prelude.exclusive_write_memory_16.is_some());
        assert!(prelude.exclusive_write_memory_32.is_some());
        assert!(prelude.exclusive_write_memory_64.is_some());
        assert!(prelude.call_svc.is_some());
        assert!(prelude.exception_raised.is_some());
        assert!(prelude.isb_raised.is_some());
        assert!(prelude.add_ticks.is_some());
        assert!(prelude.get_ticks_remaining.is_some());
        assert!(interface.callback_context.is_some());
    }
}
