//! x64 implementation of the public A32 JIT interface.
//!
//! Upstream owner: `dynarmic/backend/x64/a32_interface.cpp`.

use std::sync::atomic::{AtomicU32, Ordering};

use crate::backend::common::a32_callbacks;
use crate::backend::x64::a32_emit_x64::A32EmitX64;
use crate::backend::x64::a32_emit_x64_memory;
use crate::backend::x64::a32_jitstate::A32JitState;
use crate::backend::x64::a64_jitstate::A64JitState;
use crate::backend::x64::block_of_code::{RunCodeCallbacks, RunCodeFn, DEFAULT_CODE_SIZE};
use crate::backend::x64::callback::ArgCallback;
use crate::backend::x64::emit_context::{EmitCallbacks, EmitConfig, RawExclusiveWriteCallbacks};
use crate::common::llvm_disassemble::disassemble_x64;
use crate::frontend::a32::translate::translate_callbacks::UserCallbacksAdapter;
use crate::frontend::a32::translate::TranslationOptions as A32TranslationOptions;
use crate::interface::a32::config::{
    UserCallbacks as A32UserCallbacks, UserConfig as A32UserConfig,
};
use crate::interface::halt_reason::HaltReason;
use crate::ir::location::LocationDescriptor;
use crate::jit::{
    block_count_counters, block_count_range, block_trace_range, watch_write_target, PC_TRACE_ACTIVE,
};

const MINIMUM_REMAINING_CODE_SIZE: usize = 1024 * 1024;

fn maybe_log_a32_watch_write(
    inner: &A32JitInner,
    vaddr: u64,
    width: usize,
    value_lo: u64,
    value_hi: u64,
) {
    let Some((wa, wsize)) = watch_write_target() else {
        return;
    };
    if vaddr.saturating_add(width as u64) <= wa || vaddr >= wa + wsize {
        return;
    }
    let regs = &inner.jit_state.reg;

    if width <= 8 {
        eprintln!(
            "[A32_WATCH_WRITE] pc=0x{:08X} lr=0x{:08X} vaddr=0x{:08X} width={} value=0x{:X} r0=0x{:08X} r1=0x{:08X} r2=0x{:08X} r3=0x{:08X} r4=0x{:08X} r5=0x{:08X} r6=0x{:08X} r7=0x{:08X} r8=0x{:08X} r9=0x{:08X} r10=0x{:08X} r11=0x{:08X}",
            regs[15],
            regs[14],
            vaddr as u32,
            width,
            value_lo,
            regs[0],
            regs[1],
            regs[2],
            regs[3],
            regs[4],
            regs[5],
            regs[6],
            regs[7],
            regs[8],
            regs[9],
            regs[10],
            regs[11],
        );
    } else {
        eprintln!(
            "[A32_WATCH_WRITE] pc=0x{:08X} lr=0x{:08X} vaddr=0x{:08X} width=128 lo=0x{:X} hi=0x{:X} r0=0x{:08X} r1=0x{:08X} r2=0x{:08X} r3=0x{:08X} r4=0x{:08X} r5=0x{:08X} r6=0x{:08X} r7=0x{:08X} r8=0x{:08X} r9=0x{:08X} r10=0x{:08X} r11=0x{:08X}",
            regs[15],
            regs[14],
            vaddr as u32,
            value_lo,
            value_hi,
            regs[0],
            regs[1],
            regs[2],
            regs[3],
            regs[4],
            regs[5],
            regs[6],
            regs[7],
            regs[8],
            regs[9],
            regs[10],
            regs[11],
        );
    }
}

pub(crate) struct A32Jit {
    pub(crate) inner: Box<A32JitInner>,
}

pub(crate) struct A32JitInner {
    pub(crate) jit_state: A32JitState,
    pub(crate) emitter: Option<A32EmitX64>,
    pub(crate) callbacks: Box<dyn A32UserCallbacks>,
    pub(crate) run_code_fn: Option<RunCodeFn>,
    pub(crate) global_monitor: Option<*mut crate::interface::exclusive_monitor::ExclusiveMonitor>,
    pub(crate) processor_id: usize,
    invalidate_entire_cache: bool,
    invalid_cache_ranges: Vec<(u32, u32)>,
    invalidation_mutex: std::sync::Mutex<()>,
}

impl A32JitInner {
    fn perform_requested_cache_invalidation(&mut self, halt_reason: HaltReason) {
        if !halt_reason.contains(HaltReason::CACHE_INVALIDATION) {
            return;
        }

        let _lock = self
            .invalidation_mutex
            .lock()
            .expect("A32 cache invalidation mutex poisoned");
        let halt = unsafe { &*(&self.jit_state.halt_reason as *const u32 as *const AtomicU32) };
        halt.fetch_and(!HaltReason::CACHE_INVALIDATION.bits(), Ordering::Release);

        if !self.invalidate_entire_cache && self.invalid_cache_ranges.is_empty() {
            return;
        }

        self.jit_state.reset_rsb();
        let emitter = self.emitter.as_mut().expect("A32 emitter is initialized");
        if self.invalidate_entire_cache {
            emitter.clear_cache();
        } else {
            let ranges: Vec<_> = self
                .invalid_cache_ranges
                .iter()
                .map(|&(start, end)| start..=end)
                .collect();
            emitter.invalidate_ranges(&ranges);
        }
        self.invalid_cache_ranges.clear();
        self.invalidate_entire_cache = false;
    }

    fn get_or_compile_block(&mut self, location: LocationDescriptor) -> *const u8 {
        if let Some(code_ptr) = self
            .emitter
            .as_ref()
            .expect("A32 emitter is initialized")
            .lookup_cached_block(location)
        {
            return code_ptr;
        }

        self.emitter
            .as_mut()
            .expect("A32 emitter is initialized")
            .make_writable()
            .expect("making the A32 code cache writable failed");

        if self
            .emitter
            .as_ref()
            .expect("A32 emitter is initialized")
            .code
            .space_remaining()
            < MINIMUM_REMAINING_CODE_SIZE
        {
            self.invalidate_entire_cache = true;
            self.perform_requested_cache_invalidation(HaltReason::CACHE_INVALIDATION);
        }

        let callbacks_ptr = self.callbacks.as_ref() as *const dyn A32UserCallbacks;
        let translate_callbacks = UserCallbacksAdapter::new(unsafe { &*callbacks_ptr });
        let is_read_only =
            move |vaddr: u32| -> bool { unsafe { &*callbacks_ptr }.is_read_only_memory(vaddr) };
        let emitter = self.emitter.as_mut().expect("A32 emitter is initialized");
        let code_ptr =
            emitter.get_or_compile_block_with_ro(location, &translate_callbacks, &is_read_only);
        unsafe {
            emitter
                .get_run_code_fn()
                .expect("making the A32 code cache executable failed");
        }
        code_ptr
    }
}

impl A32Jit {
    /// Diagnostic extension: the x64 backend has no native block-map writer,
    /// so this host implementation is a no-op.
    pub fn dump_jit_block_map(&self, path: &str) -> std::io::Result<()> {
        let _ = path;
        Ok(())
    }

    /// Create a new A32Jit from the given configuration.
    pub fn new(config: A32UserConfig) -> Result<Self, String> {
        let cache_size = if config.code_cache_size > 0 {
            config.code_cache_size as usize
        } else {
            DEFAULT_CODE_SIZE
        };
        let effective_optimizations = config.effective_optimizations();

        let mut inner = Box::new(A32JitInner {
            jit_state: A32JitState::new(),
            emitter: None,
            callbacks: config.callbacks,
            run_code_fn: None,
            global_monitor: config.global_monitor,
            processor_id: config.processor_id as usize,
            invalidate_entire_cache: false,
            invalid_cache_ranges: Vec::new(),
            invalidation_mutex: std::sync::Mutex::new(()),
        });

        // Wire the halt_reason pointer into callbacks so they can halt execution
        // from within exception_raised(), matching upstream's m_parent.m_jit->HaltExecution().
        let halt_ptr = &inner.jit_state.halt_reason as *const u32;
        inner.callbacks.set_halt_reason_ptr(halt_ptr);
        let pc_ptr = &inner.jit_state.reg[15] as *const u32;
        inner.callbacks.set_pc_ptr(pc_ptr);

        let inner_ptr = &mut *inner as *mut A32JitInner as u64;

        let run_callbacks = RunCodeCallbacks {
            lookup_block: Box::new(ArgCallback::new(
                a32_lookup_block_trampoline as usize as u64,
                inner_ptr,
            )),
            add_ticks: Box::new(ArgCallback::new(
                a32_add_ticks_trampoline as usize as u64,
                inner_ptr,
            )),
            get_ticks_remaining: Box::new(ArgCallback::new(
                a32_get_ticks_remaining_trampoline as usize as u64,
                inner_ptr,
            )),
            enable_cycle_counting: config.enable_cycle_counting,
            fastmem_pointer: config.fastmem_pointer.map(|p| p as *const u8),
            page_table_pointer: config
                .page_table
                .map(|pointer| pointer.cast::<u8>() as *const u8),
        };

        let emit_callbacks = EmitCallbacks {
            memory_read_8: Box::new(ArgCallback::new(
                a32_memory_read_8_trampoline as usize as u64,
                inner_ptr,
            )),
            memory_read_16: Box::new(ArgCallback::new(
                a32_memory_read_16_trampoline as usize as u64,
                inner_ptr,
            )),
            memory_read_32: Box::new(ArgCallback::new(
                a32_memory_read_32_trampoline as usize as u64,
                inner_ptr,
            )),
            memory_read_64: Box::new(ArgCallback::new(
                a32_memory_read_64_trampoline as usize as u64,
                inner_ptr,
            )),
            memory_read_128: Box::new(ArgCallback::new(
                a32_unreachable_read_128_trampoline as usize as u64,
                inner_ptr,
            )),
            memory_write_8: Box::new(ArgCallback::new(
                a32_memory_write_8_trampoline as usize as u64,
                inner_ptr,
            )),
            memory_write_16: Box::new(ArgCallback::new(
                a32_memory_write_16_trampoline as usize as u64,
                inner_ptr,
            )),
            memory_write_32: Box::new(ArgCallback::new(
                a32_memory_write_32_trampoline as usize as u64,
                inner_ptr,
            )),
            memory_write_64: Box::new(ArgCallback::new(
                a32_memory_write_64_trampoline as usize as u64,
                inner_ptr,
            )),
            memory_write_128: Box::new(ArgCallback::new(
                a32_unreachable_write_128_trampoline as usize as u64,
                inner_ptr,
            )),
            call_supervisor: Box::new(ArgCallback::new(
                a32_call_supervisor_trampoline as usize as u64,
                inner_ptr,
            )),
            exception_raised: Box::new(ArgCallback::new(
                a32_exception_raised_trampoline as usize as u64,
                inner_ptr,
            )),
            data_cache_operation: Box::new(ArgCallback::new(
                a32_unreachable_cache_operation_trampoline as usize as u64,
                inner_ptr,
            )),
            instruction_cache_operation: Box::new(ArgCallback::new(
                a32_unreachable_cache_operation_trampoline as usize as u64,
                inner_ptr,
            )),
            instruction_synchronization_barrier: Box::new(ArgCallback::new(
                a32_instruction_synchronization_barrier_trampoline as usize as u64,
                inner_ptr,
            )),
            add_ticks: Box::new(ArgCallback::new(
                a32_add_ticks_trampoline as usize as u64,
                inner_ptr,
            )),
            get_ticks_remaining: Box::new(ArgCallback::new(
                a32_get_ticks_remaining_trampoline as usize as u64,
                inner_ptr,
            )),
            get_cntpct: Box::new(ArgCallback::new(
                a32_unreachable_get_cntpct_trampoline as usize as u64,
                inner_ptr,
            )),
            exclusive_clear: Box::new(ArgCallback::new(
                a32_exclusive_clear_trampoline as usize as u64,
                inner_ptr,
            )),
            exclusive_read_8: Box::new(ArgCallback::new(
                a32_exclusive_read_8_trampoline as usize as u64,
                inner_ptr,
            )),
            exclusive_read_16: Box::new(ArgCallback::new(
                a32_exclusive_read_16_trampoline as usize as u64,
                inner_ptr,
            )),
            exclusive_read_32: Box::new(ArgCallback::new(
                a32_exclusive_read_32_trampoline as usize as u64,
                inner_ptr,
            )),
            exclusive_read_64: Box::new(ArgCallback::new(
                a32_exclusive_read_64_trampoline as usize as u64,
                inner_ptr,
            )),
            exclusive_read_128: Box::new(ArgCallback::new(
                a32_unreachable_read_128_trampoline as usize as u64,
                inner_ptr,
            )),
            exclusive_write_8: Box::new(ArgCallback::new(
                a32_exclusive_write_8_trampoline as usize as u64,
                inner_ptr,
            )),
            exclusive_write_16: Box::new(ArgCallback::new(
                a32_exclusive_write_16_trampoline as usize as u64,
                inner_ptr,
            )),
            exclusive_write_32: Box::new(ArgCallback::new(
                a32_exclusive_write_32_trampoline as usize as u64,
                inner_ptr,
            )),
            exclusive_write_64: Box::new(ArgCallback::new(
                a32_exclusive_write_64_trampoline as usize as u64,
                inner_ptr,
            )),
            exclusive_write_128: Box::new(ArgCallback::new(
                a32_unreachable_write_128_trampoline as usize as u64,
                inner_ptr,
            )),
        };

        let emit_config = EmitConfig {
            coprocessors: config.coprocessors.clone(),
            callbacks: emit_callbacks,
            raw_exclusive_write_callbacks: Some(RawExclusiveWriteCallbacks {
                write_8: Box::new(ArgCallback::new(
                    a32_raw_exclusive_write_8_trampoline as usize as u64,
                    inner_ptr,
                )),
                write_16: Box::new(ArgCallback::new(
                    a32_raw_exclusive_write_16_trampoline as usize as u64,
                    inner_ptr,
                )),
                write_32: Box::new(ArgCallback::new(
                    a32_raw_exclusive_write_32_trampoline as usize as u64,
                    inner_ptr,
                )),
                write_64: Box::new(ArgCallback::new(
                    a32_raw_exclusive_write_64_trampoline as usize as u64,
                    inner_ptr,
                )),
                write_128: Box::new(ArgCallback::new(
                    a32_unreachable_raw_exclusive_write_128_trampoline as usize as u64,
                    inner_ptr,
                )),
            }),
            enable_cycle_counting: config.enable_cycle_counting,
            // A32 memory emission uses the same fastmem/page-table policy as
            // upstream Dynarmic::A32::UserConfig. Preserve the caller-provided
            // settings instead of falling back to default 64-bit mirroring.
            memory: crate::backend::common::emit_context::MemoryEmitConfig {
                fastmem_address_space_bits: 32,
                silently_mirror_fastmem: true,
                fastmem_exclusive_access: config.fastmem_exclusive_access,
                recompile_on_exclusive_fastmem_failure: config
                    .recompile_on_exclusive_fastmem_failure,
                recompile_on_fastmem_failure: config.recompile_on_fastmem_failure,
                page_table_present: config.page_table.is_some(),
                page_table_address_space_bits: 32,
                silently_mirror_page_table: true,
                absolute_offset_page_table: config.absolute_offset_page_table,
                page_table_pointer_mask_bits: config.page_table_pointer_mask_bits as u32,
                page_table_log2_stride: config.page_table_log2_stride,
                detect_misaligned_access_via_page_table: config
                    .detect_misaligned_access_via_page_table
                    as u32,
                only_detect_misalignment_via_page_table_on_page_boundary: config
                    .only_detect_misalignment_via_page_table_on_page_boundary,
                check_halt_on_memory_access: config.check_halt_on_memory_access,
                processor_id: config.processor_id as usize,
            },
            global_monitor: config.global_monitor,
            tpidrro_el0: None,
            tpidr_el0: None,
            // Unused by A32 (CNTFRQ is a CP15 read there), but the shared
            // EmitConfig carries it; forward the configured value anyway.
            cntfrq_el0: 600_000_000,
            ctr_el0: 0x8444_c004,
            dczid_el0: 4,
            hook_data_cache_operations: false,
            hook_isb: config.hook_isb,
        };

        let mut emitter = A32EmitX64::new(
            emit_config,
            run_callbacks,
            effective_optimizations,
            A32TranslationOptions {
                arch_version: config.arch_version,
                define_unpredictable_behaviour: config.define_unpredictable_behaviour,
                hook_hint_instructions: config.hook_hint_instructions,
            },
            cache_size,
        )?;

        let run_code_fn = unsafe { emitter.get_run_code_fn()? };

        inner.emitter = Some(emitter);
        inner.run_code_fn = Some(run_code_fn);

        Ok(A32Jit { inner })
    }

    fn perform_requested_cache_invalidation(&mut self, halt_reason: HaltReason) {
        self.inner.perform_requested_cache_invalidation(halt_reason);
    }

    /// Execute JIT code until a halt reason is triggered.
    pub fn run(&mut self, is_executing: &mut bool) -> HaltReason {
        assert!(!*is_executing, "Recursive JIT execution not allowed");
        let halt_reason = HaltReason::from_bits_truncate(self.read_halt_reason());
        self.perform_requested_cache_invalidation(halt_reason);
        *is_executing = true;

        // Upstream: Run() does RSB check then GetCurrentBlock() then RunCode().
        // GetCurrentBlock() is a cache lookup (no mprotect). Only on cache miss
        // does it compile (with EnableWriting/DisableWriting inside Emit()).
        // RunCode() just calls the stored function pointer — no mprotect ever.
        let unique_hash = self.inner.jit_state.get_unique_hash();
        let location = LocationDescriptor::new(unique_hash);
        let new_rsb_ptr =
            self.inner.jit_state.rsb_ptr.wrapping_sub(1) as usize & A32JitState::RSB_PTR_MASK;
        let rsb_code_ptr =
            if self.inner.jit_state.rsb_location_descriptors[new_rsb_ptr] == unique_hash {
                self.inner.jit_state.rsb_ptr = new_rsb_ptr as u32;
                Some(self.inner.jit_state.rsb_codeptrs[new_rsb_ptr] as *const u8)
            } else {
                None
            };
        // Fast path: block already compiled — no mprotect needed.
        let code_ptr = if let Some(ptr) = rsb_code_ptr {
            ptr
        } else {
            self.inner.get_or_compile_block(location)
        };

        // Use the run_code_fn cached at construction time — no mprotect.
        let run_fn = self.inner.run_code_fn.unwrap();

        let halt_bits = unsafe {
            run_fn(
                &mut self.inner.jit_state as *mut _ as *mut A64JitState,
                code_ptr,
            )
        };
        self.inner
            .emitter
            .as_mut()
            .expect("A32 emitter is initialized")
            .process_pending_fastmem_recompiles()
            .expect("processing A32 fastmem recompiles failed");

        let halt_reason = HaltReason::from_bits_truncate(halt_bits);
        self.perform_requested_cache_invalidation(halt_reason);
        *is_executing = false;
        halt_reason
    }

    /// Execute a single instruction.
    pub fn step(&mut self, is_executing: &mut bool) -> HaltReason {
        assert!(!*is_executing, "Recursive JIT execution not allowed");
        let halt_reason = HaltReason::from_bits_truncate(self.read_halt_reason());
        self.perform_requested_cache_invalidation(halt_reason);
        *is_executing = true;

        let a32_loc = crate::ir::location::A32LocationDescriptor::from_location(
            LocationDescriptor::new(self.inner.jit_state.get_unique_hash()),
        );
        let location = a32_loc.set_single_stepping(true).to_location();

        let code_ptr = self.inner.get_or_compile_block(location);

        let step_fn = {
            let emitter = self.inner.emitter.as_mut().unwrap();
            unsafe { emitter.get_step_code_fn().unwrap() }
        };

        let halt_bits = unsafe {
            step_fn(
                &mut self.inner.jit_state as *mut _ as *mut A64JitState,
                code_ptr,
            )
        };
        self.inner
            .emitter
            .as_mut()
            .unwrap()
            .process_pending_fastmem_recompiles()
            .expect("processing A32 fastmem recompiles failed");

        let halt_reason = HaltReason::from_bits_truncate(halt_bits);
        self.perform_requested_cache_invalidation(halt_reason);
        *is_executing = false;
        halt_reason
    }

    /// Request halt from another thread.
    pub fn halt_execution(&self, reason: HaltReason) {
        let halt_ptr = &self.inner.jit_state.halt_reason as *const u32 as *const AtomicU32;
        let atomic = unsafe { &*halt_ptr };
        atomic.fetch_or(reason.bits(), Ordering::Release);
    }

    /// Read the current halt_reason value (diagnostic).
    pub fn read_halt_reason(&self) -> u32 {
        let halt_ptr = &self.inner.jit_state.halt_reason as *const u32 as *const AtomicU32;
        let atomic = unsafe { &*halt_ptr };
        atomic.load(Ordering::Acquire)
    }

    /// Get the address of halt_reason (diagnostic).
    pub fn halt_reason_ptr(&self) -> *const u32 {
        &self.inner.jit_state.halt_reason as *const u32
    }

    /// Get the address of jit_state base (R15 value).
    pub fn jit_state_ptr(&self) -> *const u8 {
        &self.inner.jit_state as *const A32JitState as *const u8
    }

    /// Clear specific halt reason bits.
    pub fn clear_halt(&self, reason: HaltReason) {
        let halt_ptr = &self.inner.jit_state.halt_reason as *const u32 as *const AtomicU32;
        let atomic = unsafe { &*halt_ptr };
        atomic.fetch_and(!reason.bits(), Ordering::Release);
    }

    /// Reset CPU state without clearing the compiled-code cache.
    pub fn reset(&mut self, is_executing: bool) {
        assert!(!is_executing, "Cannot reset while the JIT is executing");
        self.inner.jit_state = A32JitState::new();
    }

    // ---- Register accessors (R0-R15, u32) ----

    pub fn regs(&self) -> &[u32; 16] {
        &self.inner.jit_state.reg
    }

    pub fn regs_mut(&mut self) -> &mut [u32; 16] {
        &mut self.inner.jit_state.reg
    }

    pub fn ext_regs(&self) -> &[u32; 64] {
        &self.inner.jit_state.ext_reg
    }

    pub fn ext_regs_mut(&mut self) -> &mut [u32; 64] {
        &mut self.inner.jit_state.ext_reg
    }

    pub fn get_register(&self, index: usize) -> u32 {
        assert!(index < 16, "A32 register index out of range (0-15)");

        self.inner.jit_state.reg[index]
    }

    pub fn set_register(&mut self, index: usize, value: u32) {
        assert!(index < 16, "A32 register index out of range (0-15)");

        self.inner.jit_state.reg[index] = value;
    }

    pub fn get_pc(&self) -> u32 {
        self.inner.jit_state.reg[15]
    }

    pub fn set_pc(&mut self, value: u32) {
        self.inner.jit_state.reg[15] = value;
    }

    pub fn get_cpsr(&self) -> u32 {
        self.inner.jit_state.get_cpsr()
    }

    pub fn set_cpsr(&mut self, value: u32) {
        // A32JitState::set_cpsr handles both cpsr fields and upper_location_descriptor
        self.inner.jit_state.set_cpsr(value);
    }

    pub fn get_fpscr(&self) -> u32 {
        self.inner.jit_state.get_fpscr()
    }

    pub fn set_fpscr(&mut self, value: u32) {
        // set_fpscr updates fpsr_nzcv, mode bits, mxcsr, AND upper_location_descriptor
        self.inner.jit_state.set_fpscr(value);
    }

    /// Get extension register (S/D backing store, u32 element).
    pub fn get_ext_reg(&self, index: usize) -> u32 {
        assert!(index < 64, "A32 ext_reg index out of range (0-63)");

        self.inner.jit_state.ext_reg[index]
    }

    /// Set extension register.
    pub fn set_ext_reg(&mut self, index: usize, value: u32) {
        assert!(index < 64, "A32 ext_reg index out of range (0-63)");

        self.inner.jit_state.ext_reg[index] = value;
    }

    /// Clear exclusive monitor state.
    /// Matching dynarmic's `Jit::ClearExclusiveState()`.
    /// Called before `run()` to ensure no stale exclusive reservation persists.
    pub fn clear_exclusive_state(&mut self) {
        self.inner.jit_state.exclusive_state = 0;
    }

    /// Invalidate cached blocks in a memory range.
    pub fn invalidate_cache_range(&mut self, addr: u32, size: usize) {
        let _lock = self
            .inner
            .invalidation_mutex
            .lock()
            .expect("A32 cache invalidation mutex poisoned");
        let end = addr.wrapping_add(size as u32).wrapping_sub(1);
        self.inner.invalid_cache_ranges.push((addr, end));
        self.halt_execution(HaltReason::CACHE_INVALIDATION);
    }

    /// Clear all cached blocks.
    pub fn clear_cache(&mut self) {
        let _lock = self
            .inner
            .invalidation_mutex
            .lock()
            .expect("A32 cache invalidation mutex poisoned");
        self.inner.invalidate_entire_cache = true;
        self.halt_execution(HaltReason::CACHE_INVALIDATION);
    }

    pub fn disassemble(&self) -> String {
        let emitter = self
            .inner
            .emitter
            .as_ref()
            .expect("A32 emitter is initialized");
        let begin = emitter.code.code_base_ptr();
        let end = begin.wrapping_add(emitter.code.code_size());
        disassemble_x64(begin, end)
    }

    /// Force compilation of the block at the current PC (without executing it).
    ///
    /// Returns the entrypoint pointer of the compiled block. Used by the
    /// deterministic JIT microbenchmark (`compile_bench` binary) to isolate
    /// compile cost from execution cost. Not part of normal JIT operation.
    ///
    /// The caller is responsible for setting PC + CPSR via `set_pc` / `set_cpsr`
    /// before invoking this method. Invokes the same `get_or_compile_block_with_ro`
    /// path that `step()` uses.
    pub fn compile_block_only(&mut self) -> *const u8 {
        let a32_loc = crate::ir::location::A32LocationDescriptor::from_location(
            LocationDescriptor::new(self.inner.jit_state.get_unique_hash()),
        );
        let location = a32_loc.to_location();

        self.inner.get_or_compile_block(location)
    }
}

// ---------------------------------------------------------------------------
// A32 Callback trampolines
// ---------------------------------------------------------------------------

/// Env-gated block-entry logger. Reads `RUZU_BLOCK_TRACE_PC=0xLO-0xHI`
/// once; for every block lookup whose target PC falls in that range, logs
/// `[BLOCK] pc=... lr=... r0=... r4=...` to stderr. Zero cost when unset.
fn block_trace_verbose() -> bool {
    use std::sync::OnceLock;
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var("RUZU_BLOCK_TRACE_VERBOSE").is_ok())
}

fn block_trace_code_words() -> usize {
    use std::sync::OnceLock;
    static N: OnceLock<usize> = OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("RUZU_BLOCK_TRACE_CODE")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0)
    })
}

/// `RUZU_TRACK_PC_LR=0xPC,0xLR` — when block trampoline sees this PC+LR pair,
/// log r4 + memory at the offsets in `RUZU_TRACK_OFFSETS` (default `0x1c,0x70`)
/// every iteration. Used to watch how a target struct's fields evolve across
/// loop iterations without paying the fastmem-absorption tax of WATCH_ADDR.
fn track_pc_lr() -> Option<(u32, u32)> {
    use std::sync::OnceLock;
    static SPEC: OnceLock<Option<(u32, u32)>> = OnceLock::new();
    *SPEC.get_or_init(|| {
        let raw = std::env::var("RUZU_TRACK_PC_LR").ok()?;
        let (a, b) = raw.split_once(',')?;
        let parse = |s: &str| -> Option<u32> {
            let s = s.trim();
            let stripped = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"));
            match stripped {
                Some(hex) => u32::from_str_radix(hex, 16).ok(),
                None => s.parse::<u32>().ok(),
            }
        };
        Some((parse(a)?, parse(b)?))
    })
}

fn track_offsets() -> &'static [u32] {
    use std::sync::OnceLock;
    static OFFS: OnceLock<Vec<u32>> = OnceLock::new();
    OFFS.get_or_init(|| {
        let raw = std::env::var("RUZU_TRACK_OFFSETS").unwrap_or_else(|_| "0x1c,0x70".to_string());
        raw.split(',')
            .filter_map(|tok| {
                let s = tok.trim();
                let stripped = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"));
                match stripped {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => s.parse::<u32>().ok(),
                }
            })
            .collect()
    })
}

/// `RUZU_A32_DUMP_MEM_AT=0xPC:rN:SIZE[,0xPC:rN:SIZE...]` — when an A32 block
/// starts at PC, dump SIZE bytes from the guest address currently held in rN.
/// This is intentionally diagnostic-only and reads through callbacks so it
/// observes guest memory even when generated code uses direct fastmem.
fn a32_dump_mem_specs() -> &'static [(u32, usize, usize)] {
    use std::sync::OnceLock;
    static SPECS: OnceLock<Vec<(u32, usize, usize)>> = OnceLock::new();
    SPECS.get_or_init(|| {
        let raw = match std::env::var("RUZU_A32_DUMP_MEM_AT") {
            Ok(raw) => raw,
            Err(_) => return Vec::new(),
        };
        raw.split(',')
            .filter_map(|spec| {
                let mut parts = spec.split(':');
                let pc = parts.next()?.trim();
                let reg = parts.next()?.trim();
                let size = parts.next()?.trim();
                if parts.next().is_some() {
                    return None;
                }
                let parse_hex = |s: &str| -> Option<u32> {
                    let stripped = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"));
                    match stripped {
                        Some(hex) => u32::from_str_radix(hex, 16).ok(),
                        None => s.parse::<u32>().ok(),
                    }
                };
                let pc = parse_hex(pc)?;
                let reg = reg.strip_prefix('r').unwrap_or(reg);
                let reg = reg.parse::<usize>().ok()?;
                let size = size.parse::<usize>().ok()?;
                if reg >= 16 || size == 0 || size > 256 {
                    return None;
                }
                Some((pc, reg, size))
            })
            .collect()
    })
}

/// Public flag that gates per-block-lookup PC tracing (`[TRACE_PC]` lines).
/// Toggled externally by the ruzu SVC dispatcher to mark a window between two
/// main-thread SVCs. When true, `a32_lookup_block_trampoline` logs PC+LR on
/// every block transition. This is the counterpart to zuyu's
/// `Core::ArmDynarmic32SetPcTraceActive`.
extern "C" fn a32_lookup_block_trampoline(inner_ptr: u64) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };

    // Same low-overhead counter as the A64 path, but for A32 block lookups.
    // This counts dispatcher/block-transition entries only; direct block links
    // intentionally bypass it. Use RUZU_BLOCK_PROLOGUE_COUNT_PC for emitted
    // prologue counts once the ARM64 A32 emitter grows that hook.
    if let Some((lo, hi)) = block_count_range() {
        let pc = inner.jit_state.reg[15];
        if pc >= lo && pc < hi {
            let idx = inner.processor_id.min(15);
            block_count_counters()[idx].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    // PC-window tracer: when ruzu's SVC dispatcher has activated the window,
    // emit a compact [TRACE_PC] line per block transition. Matches zuyu's
    // AddTicks hook gated on `Core::ArmDynarmic32SetPcTraceActive`. The load
    // is Relaxed — losing a sample at the edge is fine.
    // Logs r4..r11 + sp to help pinpoint which block first diverges in
    // a callee-saved register (callers outside can filter to a single reg).
    if PC_TRACE_ACTIVE.load(std::sync::atomic::Ordering::Relaxed) {
        let r = &inner.jit_state.reg;
        eprintln!(
            "[TRACE_PC] pc=0x{:08X} lr=0x{:08X} r4=0x{:08X} r5=0x{:08X} r6=0x{:08X} r7=0x{:08X} r8=0x{:08X} r9=0x{:08X} r10=0x{:08X} r11=0x{:08X} sp=0x{:08X}",
            r[15], r[14], r[4], r[5], r[6], r[7], r[8], r[9], r[10], r[11], r[13]
        );
    }

    // Block-entry tracing: log (pc, lr, r0, r4) when the lookup target PC
    // is in RUZU_BLOCK_TRACE_PC. This fires on every block transition in
    // the JIT, so the env gate must stay cheap.
    // RUZU_TRACK_PC_LR: per-iteration field tracker. When the block trampoline
    // enters with the configured (PC, LR) pair, log r4 plus N memory words at
    // `r4 + RUZU_TRACK_OFFSETS`. Reads go through `memory_read_32` (the slow
    // callback path), so they always see the authoritative guest memory even
    // when fastmem absorbs the JIT-emitted accesses.
    if let Some((target_pc, target_lr)) = track_pc_lr() {
        if inner.jit_state.reg[15] == target_pc && inner.jit_state.reg[14] == target_lr {
            let r4 = inner.jit_state.reg[4];
            let mut buf = format!("[TRACK] r4=0x{:08X}", r4);
            for &off in track_offsets() {
                let addr = r4.wrapping_add(off);
                let v = inner.callbacks.memory_read_32(addr);
                use std::fmt::Write;
                let _ = write!(buf, " *(this+0x{:x})=0x{:08X}", off, v);
            }
            eprintln!("{}", buf);
        }
    }

    let pc_for_mem_dump = inner.jit_state.reg[15];
    for &(target_pc, reg, size) in a32_dump_mem_specs() {
        if pc_for_mem_dump == target_pc {
            let base = inner.jit_state.reg[reg];
            let mut bytes = Vec::with_capacity(size);
            for off in (0..size).step_by(4) {
                let word = inner
                    .callbacks
                    .memory_read_32(base.wrapping_add(off as u32));
                for i in 0..4.min(size - off) {
                    bytes.push(((word >> (i * 8)) & 0xff) as u8);
                }
            }
            let hex = bytes
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<Vec<_>>()
                .join(" ");
            eprintln!(
                "[A32_DUMP_MEM_AT] pc=0x{:08X} r{}=0x{:08X} size={} bytes={}",
                pc_for_mem_dump, reg, base, size, hex
            );
        }
    }

    if let Some((lo, hi)) = block_trace_range() {
        let pc = inner.jit_state.reg[15];
        if pc >= lo && pc < hi {
            if block_trace_verbose() {
                let r = &inner.jit_state.reg;
                eprintln!(
                    "[BLOCK] pc=0x{:08X} cpsr_nzcv=0x{:08X} cpsr_q={}",
                    pc, inner.jit_state.cpsr_nzcv, inner.jit_state.cpsr_q,
                );
                eprintln!(
                    "        r0=0x{:08X}  r1=0x{:08X}  r2=0x{:08X}  r3=0x{:08X}",
                    r[0], r[1], r[2], r[3],
                );
                eprintln!(
                    "        r4=0x{:08X}  r5=0x{:08X}  r6=0x{:08X}  r7=0x{:08X}",
                    r[4], r[5], r[6], r[7],
                );
                eprintln!(
                    "        r8=0x{:08X}  r9=0x{:08X} r10=0x{:08X} r11=0x{:08X}",
                    r[8], r[9], r[10], r[11],
                );
                eprintln!(
                    "       r12=0x{:08X}  sp=0x{:08X}  lr=0x{:08X}  pc=0x{:08X}",
                    r[12], r[13], r[14], r[15],
                );
            } else {
                eprintln!(
                    "[BLOCK] pc=0x{:08X} lr=0x{:08X} r0=0x{:08X} r4=0x{:08X}",
                    pc, inner.jit_state.reg[14], inner.jit_state.reg[0], inner.jit_state.reg[4],
                );
            }
            let n = block_trace_code_words();
            if n > 0 {
                for i in 0..n {
                    let vaddr = pc.wrapping_add((i * 4) as u32);
                    let word = inner.callbacks.memory_read_code(vaddr).unwrap_or(0);
                    eprintln!("        code[0x{:08X}] = 0x{:08X}", vaddr as u32, word);
                }
            }
        }
    }

    let location = LocationDescriptor::new(inner.jit_state.get_unique_hash());
    inner.get_or_compile_block(location) as u64
}

extern "C" fn a32_add_ticks_trampoline(inner_ptr: u64, ticks: u64) {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    a32_callbacks::add_ticks(inner.callbacks.as_mut(), ticks);
}

extern "C" fn a32_get_ticks_remaining_trampoline(inner_ptr: u64) -> u64 {
    let inner = unsafe { &*(inner_ptr as *const A32JitInner) };
    a32_callbacks::get_ticks_remaining(inner.callbacks.as_ref())
}

extern "C" fn a32_memory_read_8_trampoline(inner_ptr: u64, vaddr: u64) -> u64 {
    let inner = unsafe { &*(inner_ptr as *const A32JitInner) };
    a32_callbacks::memory_read_8(inner.callbacks.as_ref(), vaddr)
}
extern "C" fn a32_memory_read_16_trampoline(inner_ptr: u64, vaddr: u64) -> u64 {
    let inner = unsafe { &*(inner_ptr as *const A32JitInner) };
    a32_callbacks::memory_read_16(inner.callbacks.as_ref(), vaddr)
}
extern "C" fn a32_memory_read_32_trampoline(inner_ptr: u64, vaddr: u64) -> u64 {
    let inner = unsafe { &*(inner_ptr as *const A32JitInner) };
    a32_callbacks::memory_read_32(inner.callbacks.as_ref(), vaddr)
}
extern "C" fn a32_memory_read_64_trampoline(inner_ptr: u64, vaddr: u64) -> u64 {
    let inner = unsafe { &*(inner_ptr as *const A32JitInner) };
    a32_callbacks::memory_read_64(inner.callbacks.as_ref(), vaddr)
}
extern "C" fn a32_unreachable_read_128_trampoline(_inner_ptr: u64, _vaddr: u64, _ret_ptr: u64) {
    unreachable!("A32 has no 128-bit memory callback")
}

extern "C" fn a32_memory_write_8_trampoline(inner_ptr: u64, vaddr: u64, value: u64) {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    maybe_log_a32_watch_write(inner, vaddr, 1, value, 0);
    a32_callbacks::memory_write_8(inner.callbacks.as_mut(), vaddr, value);
}
extern "C" fn a32_memory_write_16_trampoline(inner_ptr: u64, vaddr: u64, value: u64) {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    maybe_log_a32_watch_write(inner, vaddr, 2, value, 0);
    a32_callbacks::memory_write_16(inner.callbacks.as_mut(), vaddr, value);
}
extern "C" fn a32_memory_write_32_trampoline(inner_ptr: u64, vaddr: u64, value: u64) {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    maybe_log_a32_watch_write(inner, vaddr, 4, value, 0);
    a32_callbacks::memory_write_32(inner.callbacks.as_mut(), vaddr, value);
}
extern "C" fn a32_memory_write_64_trampoline(inner_ptr: u64, vaddr: u64, value: u64) {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    maybe_log_a32_watch_write(inner, vaddr, 8, value, 0);
    a32_callbacks::memory_write_64(inner.callbacks.as_mut(), vaddr, value);
}
extern "C" fn a32_unreachable_write_128_trampoline(
    _inner_ptr: u64,
    _vaddr: u64,
    _value_lo: u64,
    _value_hi: u64,
) {
    unreachable!("A32 has no 128-bit memory callback")
}

extern "C" fn a32_call_supervisor_trampoline(inner_ptr: u64, svc_num: u64) {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    a32_callbacks::call_supervisor(inner.callbacks.as_mut(), svc_num);
}
extern "C" fn a32_exception_raised_trampoline(inner_ptr: u64, pc: u64, exception: u64) {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    a32_callbacks::exception_raised(inner.callbacks.as_mut(), pc, exception);
}
extern "C" fn a32_unreachable_cache_operation_trampoline(_inner_ptr: u64, _op: u64, _vaddr: u64) {
    unreachable!("A32 has no A64 cache-operation callback")
}

extern "C" fn a32_instruction_synchronization_barrier_trampoline(inner_ptr: u64) {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    inner.callbacks.instruction_synchronization_barrier_raised();
}

extern "C" fn a32_unreachable_get_cntpct_trampoline(_inner_ptr: u64) -> u64 {
    unreachable!("A32 has no GetCNTPCT callback")
}

extern "C" fn a32_exclusive_clear_trampoline(inner_ptr: u64) {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    inner.jit_state.exclusive_state = 0;
}
extern "C" fn a32_exclusive_read_8_trampoline(inner_ptr: u64, vaddr: u64) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    let monitor = inner
        .global_monitor
        .expect("A32 exclusive read requires a global monitor");
    a32_emit_x64_memory::exclusive_read_8(
        inner.callbacks.as_mut(),
        monitor,
        inner.processor_id,
        vaddr,
    )
}
extern "C" fn a32_exclusive_read_16_trampoline(inner_ptr: u64, vaddr: u64) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    let monitor = inner
        .global_monitor
        .expect("A32 exclusive read requires a global monitor");
    a32_emit_x64_memory::exclusive_read_16(
        inner.callbacks.as_mut(),
        monitor,
        inner.processor_id,
        vaddr,
    )
}
extern "C" fn a32_exclusive_read_32_trampoline(inner_ptr: u64, vaddr: u64) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    let monitor = inner
        .global_monitor
        .expect("A32 exclusive read requires a global monitor");
    a32_emit_x64_memory::exclusive_read_32(
        inner.callbacks.as_mut(),
        monitor,
        inner.processor_id,
        vaddr,
    )
}
extern "C" fn a32_exclusive_read_64_trampoline(inner_ptr: u64, vaddr: u64) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    let monitor = inner
        .global_monitor
        .expect("A32 exclusive read requires a global monitor");
    a32_emit_x64_memory::exclusive_read_64(
        inner.callbacks.as_mut(),
        monitor,
        inner.processor_id,
        vaddr,
    )
}
extern "C" fn a32_exclusive_write_8_trampoline(inner_ptr: u64, vaddr: u64, value: u64) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    let monitor = inner
        .global_monitor
        .expect("A32 exclusive write requires a global monitor");
    a32_emit_x64_memory::exclusive_write_8(
        inner.callbacks.as_mut(),
        monitor,
        inner.processor_id,
        vaddr,
        value,
    )
}
extern "C" fn a32_exclusive_write_16_trampoline(inner_ptr: u64, vaddr: u64, value: u64) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    let monitor = inner
        .global_monitor
        .expect("A32 exclusive write requires a global monitor");
    a32_emit_x64_memory::exclusive_write_16(
        inner.callbacks.as_mut(),
        monitor,
        inner.processor_id,
        vaddr,
        value,
    )
}
extern "C" fn a32_exclusive_write_32_trampoline(inner_ptr: u64, vaddr: u64, value: u64) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    maybe_log_a32_watch_write(inner, vaddr, 4, value, 0);
    let monitor = inner
        .global_monitor
        .expect("A32 exclusive write requires a global monitor");
    a32_emit_x64_memory::exclusive_write_32(
        inner.callbacks.as_mut(),
        monitor,
        inner.processor_id,
        vaddr,
        value,
    )
}
extern "C" fn a32_exclusive_write_64_trampoline(inner_ptr: u64, vaddr: u64, value: u64) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    let monitor = inner
        .global_monitor
        .expect("A32 exclusive write requires a global monitor");
    a32_emit_x64_memory::exclusive_write_64(
        inner.callbacks.as_mut(),
        monitor,
        inner.processor_id,
        vaddr,
        value,
    )
}
extern "C" fn a32_raw_exclusive_write_8_trampoline(
    inner_ptr: u64,
    vaddr: u64,
    value: u64,
    expected: u64,
) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    inner
        .callbacks
        .memory_write_exclusive_8(vaddr as u32, value as u8, expected as u8) as u64
}

extern "C" fn a32_raw_exclusive_write_16_trampoline(
    inner_ptr: u64,
    vaddr: u64,
    value: u64,
    expected: u64,
) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    inner
        .callbacks
        .memory_write_exclusive_16(vaddr as u32, value as u16, expected as u16) as u64
}

extern "C" fn a32_raw_exclusive_write_32_trampoline(
    inner_ptr: u64,
    vaddr: u64,
    value: u64,
    expected: u64,
) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    inner
        .callbacks
        .memory_write_exclusive_32(vaddr as u32, value as u32, expected as u32) as u64
}

extern "C" fn a32_raw_exclusive_write_64_trampoline(
    inner_ptr: u64,
    vaddr: u64,
    value: u64,
    expected: u64,
) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    inner
        .callbacks
        .memory_write_exclusive_64(vaddr as u32, value, expected) as u64
}

extern "C" fn a32_unreachable_raw_exclusive_write_128_trampoline(
    _inner_ptr: u64,
    _vaddr: u64,
    _value: *const [u64; 2],
    _expected: *const [u64; 2],
) -> u64 {
    unreachable!("A32 has no 128-bit exclusive-write callback")
}
