use std::ops::RangeInclusive;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use crate::halt_reason::HaltReason;
use crate::jit_config::JitConfig;

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
    is_executing: bool,
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

    pub fn new(config: JitConfig) -> Result<Self, String> {
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
                is_executing: false,
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
        inner.is_executing = false;
        let hr = hr?;
        self.perform_requested_cache_invalidation(hr)?;
        Ok(hr)
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
        inner.is_executing = false;
        let hr = hr?;
        self.perform_requested_cache_invalidation(hr)?;
        Ok(hr)
    }

    pub fn clear_cache(&self) {
        let mut invalidation = self.invalidation.lock().expect("A32 invalidation poisoned");
        invalidation.invalidate_entire_cache = true;
        self.halt_execution(HaltReason::CACHE_INVALIDATION);
    }

    pub fn invalidate_cache_range(&self, start_address: u32, length: usize) {
        let end_address = start_address.wrapping_add(length.saturating_sub(1) as u32);
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
        // Diagnostic: with `RUZU_LOG_A32_FPSCR_MODES` set, log each distinct
        // FPSCR mode (upper 16 bits) restored via the host interface.
        static SEEN: std::sync::OnceLock<
            Option<std::sync::Mutex<std::collections::BTreeSet<u32>>>,
        > = std::sync::OnceLock::new();
        if let Some(seen) = SEEN.get_or_init(|| {
            std::env::var_os("RUZU_LOG_A32_FPSCR_MODES")
                .map(|_| std::sync::Mutex::new(std::collections::BTreeSet::new()))
        }) {
            let mode = value & 0xffff_0000;
            if seen.lock().unwrap().insert(mode) {
                eprintln!("[A32_FPSCR_MODES] host set_fpscr mode=0x{mode:08X}");
            }
        }
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

    pub fn is_executing(&self) -> bool {
        self.is_executing
    }

    pub fn compile_block_only(&mut self) -> Result<*const u8, String> {
        let location_descriptor = self.current_state.get_location_descriptor();
        Ok(self
            .current_address_space
            .get_or_emit(location_descriptor)?)
    }

    pub(crate) fn current_address_space(&self) -> &A32AddressSpace {
        &self.current_address_space
    }

    fn perform_requested_cache_invalidation(&mut self, hr: HaltReason) -> Result<(), String> {
        if !hr.contains(HaltReason::CACHE_INVALIDATION) {
            return Ok(());
        }

        self.clear_halt(HaltReason::CACHE_INVALIDATION);

        let (invalidate_entire_cache, invalid_cache_ranges) = {
            let mut invalidation = self.invalidation.lock().expect("A32 invalidation poisoned");
            let invalidate_entire_cache = invalidation.invalidate_entire_cache;
            let invalid_cache_ranges = std::mem::take(&mut invalidation.invalid_cache_ranges);
            invalidation.invalidate_entire_cache = false;
            (invalidate_entire_cache, invalid_cache_ranges)
        };

        if invalidate_entire_cache {
            self.current_address_space
                .address_space_mut()
                .clear_cache()?;
            return Ok(());
        }

        if !invalid_cache_ranges.is_empty() {
            self.current_address_space
                .invalidate_cache_ranges(&invalid_cache_ranges);
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
            callbacks.as_mut() as *mut dyn crate::jit_config::UserCallbacks
        };
        inner.callback_context = Some(A32CallbackContext::new(
            &mut inner.current_state,
            callbacks,
            global_monitor,
            processor_id,
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
    use crate::backend::common::emit_context::MemoryEmitConfig;
    use crate::jit_config::{OptimizationFlag, UserCallbacks};
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

    fn config_with_pointers(pointers: Option<Arc<Mutex<PointerState>>>) -> JitConfig {
        JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(TestCallbacks { pointers }),
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

    fn config() -> JitConfig {
        config_with_pointers(None)
    }

    #[test]
    fn run_performs_deferred_clear_cache_before_execution() {
        let mut interface = A32Interface::new(config()).unwrap();
        interface.regs_mut()[15] = 0x1000;
        interface.clear_cache();
        interface.halt_execution(HaltReason::EXTERNAL_HALT);

        let result = interface.run().unwrap();

        assert!(result.contains(HaltReason::EXTERNAL_HALT));
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
        assert!(prelude.get_cntpct.is_some());
        assert!(interface.callback_context.is_some());
    }
}
