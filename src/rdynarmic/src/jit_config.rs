/// Callbacks provided by the host for JIT execution.
///
/// These are invoked from JIT-generated code (via trampolines) for memory
/// access, system calls, tick counting, and other host interactions.
pub trait UserCallbacks: Send {
    /// Read a 32-bit instruction word from guest memory.
    ///
    /// Upstream defaults this to `MemoryRead32`; users that model executable
    /// permissions may override it and return `None` for unmapped code.
    fn memory_read_code(&self, vaddr: u64) -> Option<u32> {
        Some(self.memory_read_32(vaddr))
    }

    /// Called before an A32 instruction is read during translation.
    /// Returning `false` stops translation and requires the callback to set a
    /// terminal on `ir`.
    ///
    /// Upstream: `A32::UserCallbacks::PreCodeReadHook`.
    fn pre_code_read_hook(
        &self,
        _is_thumb: bool,
        _pc: u32,
        _ir: &mut crate::ir::a32_emitter::A32IREmitter<'_>,
    ) -> bool {
        true
    }

    /// Called after an A32 instruction is read and before it is translated.
    ///
    /// Upstream: `A32::UserCallbacks::PreCodeTranslationHook`.
    fn pre_code_translation_hook(
        &self,
        _is_thumb: bool,
        _pc: u32,
        _ir: &mut crate::ir::a32_emitter::A32IREmitter<'_>,
    ) {
    }

    /// Return the guest tick cost of an A32 instruction.
    ///
    /// Upstream: `A32::UserCallbacks::GetTicksForCode`.
    fn get_ticks_for_code(&self, _is_thumb: bool, _vaddr: u32, _instruction: u32) -> u64 {
        1
    }

    /// Read 8 bits from guest memory.
    fn memory_read_8(&self, vaddr: u64) -> u8;
    /// Read 16 bits from guest memory.
    fn memory_read_16(&self, vaddr: u64) -> u16;
    /// Read 32 bits from guest memory.
    fn memory_read_32(&self, vaddr: u64) -> u32;
    /// Read 64 bits from guest memory.
    fn memory_read_64(&self, vaddr: u64) -> u64;
    /// Read 128 bits from guest memory (low, high).
    fn memory_read_128(&self, vaddr: u64) -> (u64, u64);

    /// Write 8 bits to guest memory.
    fn memory_write_8(&mut self, vaddr: u64, value: u8);
    /// Write 16 bits to guest memory.
    fn memory_write_16(&mut self, vaddr: u64, value: u16);
    /// Write 32 bits to guest memory.
    fn memory_write_32(&mut self, vaddr: u64, value: u32);
    /// Write 64 bits to guest memory.
    fn memory_write_64(&mut self, vaddr: u64, value: u64);
    /// Write 128 bits to guest memory (low, high).
    fn memory_write_128(&mut self, vaddr: u64, value_lo: u64, value_hi: u64);

    /// Exclusive write 8 bits. Returns true if the atomic CAS succeeded.
    /// `expected` is the value read during the preceding exclusive read (LDXR).
    fn exclusive_write_8(&mut self, vaddr: u64, value: u8, expected: u8) -> bool;
    /// Exclusive write 16 bits. Returns true if the atomic CAS succeeded.
    fn exclusive_write_16(&mut self, vaddr: u64, value: u16, expected: u16) -> bool;
    /// Exclusive write 32 bits. Returns true if the atomic CAS succeeded.
    fn exclusive_write_32(&mut self, vaddr: u64, value: u32, expected: u32) -> bool;
    /// Exclusive write 64 bits. Returns true if the atomic CAS succeeded.
    fn exclusive_write_64(&mut self, vaddr: u64, value: u64, expected: u64) -> bool;
    /// Exclusive write 128 bits. Returns true if the atomic CAS succeeded.
    fn exclusive_write_128(
        &mut self,
        vaddr: u64,
        value_lo: u64,
        value_hi: u64,
        expected_lo: u64,
        expected_hi: u64,
    ) -> bool;

    /// Check if a virtual address points to read-only memory.
    ///
    /// Matches upstream `A32::UserCallbacks::IsReadOnlyMemory(VAddr)`.
    /// When true, the A32ConstantMemoryReads optimization pass can fold
    /// memory loads at this address into compile-time constants.
    ///
    /// Default: false (conservative — no constant folding).
    fn is_read_only_memory(&self, _vaddr: u32) -> bool {
        false
    }

    /// Called when SVC #imm is executed.
    fn call_supervisor(&mut self, svc_num: u32);

    /// Called when an exception is raised.
    fn exception_raised(&mut self, pc: u64, exception: u64);

    /// Called for data cache operations (DC instructions).
    fn data_cache_operation(&mut self, _op: u64, _vaddr: u64) {}

    /// Called for instruction cache operations (IC instructions).
    fn instruction_cache_operation(&mut self, _op: u64, _vaddr: u64) {}

    /// Called when an instruction synchronization barrier is executed.
    fn instruction_synchronization_barrier_raised(&mut self) {}

    /// Get the emulated counter-timer physical count register (CNTPCT_EL0).
    /// Called from A64 MRS CNTPCT_EL0 instruction.
    /// Default: returns 0 (override for proper timing).
    fn get_cntpct(&self) -> u64 {
        0
    }

    /// Add ticks consumed during this execution slice.
    fn add_ticks(&mut self, ticks: u64);

    /// Get the remaining tick budget.
    fn get_ticks_remaining(&self) -> u64;

    /// Inject a pointer to the jit's halt_reason field.
    ///
    /// Called after jit construction so that callbacks can halt execution
    /// from within `exception_raised()` by atomically OR-ing a HaltReason
    /// into the pointed-to u32. This matches upstream's pattern where
    /// callbacks call `m_parent.m_jit->HaltExecution(hr)`.
    ///
    /// # Safety
    /// The pointer is valid for the lifetime of the jit. Treat as AtomicU32.
    fn set_halt_reason_ptr(&mut self, _ptr: *const u32) {}

    /// Called after JIT creation with a pointer to jit_state.reg[15] (PC).
    /// No performance impact — only stores a pointer, never called during execution.
    /// Allows callbacks to read the approximate PC for diagnostics (e.g. unmapped
    /// memory access logging). Note: during JIT block execution, reg[15] reflects
    /// the last terminal write, not the exact faulting instruction.
    fn set_pc_ptr(&mut self, _ptr: *const u32) {}

    /// Called after JIT creation with a pointer to jit_state.upper_location_descriptor.
    /// Used by A32 hosts that want exact access to the current upper location state
    /// for diagnostics around ARM/Thumb/IT transitions.
    fn set_upper_location_descriptor_ptr(&mut self, _ptr: *const u32) {}
}

// Compatibility re-export while the legacy shared `JitConfig` is split into
// its upstream A32/A64 owners.
pub use crate::interface::optimization_flags::OptimizationFlag;

/// Configuration for creating an A64Jit / A32Jit instance.
pub struct JitConfig {
    /// A32 coprocessors, indexed by the encoded coprocessor number.
    ///
    /// Matches upstream `A32::UserConfig::coprocessors`. A64 ignores this
    /// registry.
    pub coprocessors: crate::interface::a32::config::Coprocessors,
    /// Host callbacks for memory access, system calls, and tick counting.
    pub callbacks: Box<dyn UserCallbacks>,
    /// Whether cycle counting is enabled.
    pub enable_cycle_counting: bool,
    /// Code cache size in bytes (default: 128 MB).
    pub code_cache_size: usize,
    /// Which optimization passes and runtime features are enabled.
    pub optimizations: OptimizationFlag,
    /// Whether unsafe optimizations are permitted.
    pub unsafe_optimizations: bool,
    /// Global exclusive monitor for multi-core LDXR/STXR synchronization.
    /// Upstream: `ExclusiveMonitor* global_monitor` in Dynarmic::A32/A64::UserConfig.
    /// When set, exclusive read/write operations use the monitor for cross-core
    /// tracking instead of the per-JIT exclusive_state field.
    pub global_monitor: Option<*mut crate::exclusive_monitor::ExclusiveMonitor>,

    /// Fastmem pointer: base address of a 4GB host memory region that mirrors
    /// the guest 32-bit address space. When set, the JIT emits direct memory
    /// accesses as `[R13 + guest_addr]` instead of calling callbacks.
    ///
    /// Matches upstream `A32::UserConfig::fastmem_pointer`.
    ///
    /// If a memory access page-faults, the signal handler falls back to callbacks
    /// and optionally recompiles the block without fastmem.
    /// When None, all memory accesses go through callbacks (slow path).
    pub fastmem_pointer: Option<*mut u8>,

    /// Page-table pointer used by the x64 memory emitter. Matches upstream
    /// `A32::UserConfig::page_table` / `A64::UserConfig::page_table`; R14
    /// holds this value while generated code is running.
    pub page_table_pointer: Option<*const u8>,

    /// Whether to define unpredictable behaviour.
    /// Matches upstream `A32::UserConfig::define_unpredictable_behaviour`.
    pub define_unpredictable_behaviour: bool,

    /// A32 guest architecture version selected for translation.
    /// Matches upstream `A32::UserConfig::arch_version`.
    pub arch_version: crate::interface::a32::arch_version::ArchVersion,

    /// Whether A32 hint instructions raise their corresponding exceptions.
    /// Matches upstream `A32::UserConfig::hook_hint_instructions`.
    pub hook_hint_instructions: bool,

    /// Processor ID for multi-core tracking.
    /// Matches upstream `A32::UserConfig::processor_id`.
    pub processor_id: usize,

    /// Whether to use wall-clock for CNTPCT reads.
    /// Matches upstream `A32::UserConfig::wall_clock_cntpct`.
    pub wall_clock_cntpct: bool,

    /// Counter-timer frequency register. The value of the register is not
    /// interpreted by the JIT; it is returned verbatim for `MRS CNTFRQ_EL0`.
    ///
    /// Matches upstream `A64::UserConfig::cntfrq_el0` (default 600000000).
    /// Emulators must set this to the guest hardware frequency (e.g. the
    /// Switch's 19'200'000 Hz), as yuzu does in `arm_dynarmic_64.cpp`.
    pub cntfrq_el0: u32,

    /// A64 cache-type register returned for `MRS CTR_EL0`.
    /// Matches upstream `A64::UserConfig::ctr_el0`.
    pub ctr_el0: u32,

    /// A64 data-cache zero ID register returned for `MRS DCZID_EL0` and used
    /// to determine the block size lowered for `DC ZVA`.
    /// Matches upstream `A64::UserConfig::dczid_el0`.
    pub dczid_el0: u32,

    /// Whether A64 data-cache maintenance instructions invoke the user
    /// callback. When false, `DC ZVA` is lowered to zeroing stores and other
    /// data-cache operations are discarded.
    /// Matches upstream `A64::UserConfig::hook_data_cache_operations`.
    pub hook_data_cache_operations: bool,

    /// Whether ISB instructions invoke the user callback.
    /// Matches upstream A32/A64 `UserConfig::hook_isb`.
    pub hook_isb: bool,

    /// A64 TPIDRRO_EL0 backing storage.
    ///
    /// Matches upstream `A64::UserConfig::tpidrro_el0`. Generated A64 code
    /// reads through this pointer so the host can update TLS state without
    /// recompiling guest blocks.
    pub tpidrro_el0: Option<*const u64>,

    /// A64 TPIDR_EL0 backing storage.
    ///
    /// Matches upstream `A64::UserConfig::tpidr_el0`. Generated A64 code
    /// reads and writes through this pointer.
    pub tpidr_el0: Option<*mut u64>,

    /// A64 memory-emission options (fastmem AS bits, page-table layout,
    /// recompile-on-fault, misalignment detection, etc.). Defaults to the
    /// "no page table, 64-bit fastmem with mirroring" simplest case;
    /// callers (e.g. ruzu's `arm_dynarmic_64.rs`) override to match the
    /// guest AS layout.
    ///
    /// Mirrors the corresponding fields on upstream
    /// `Dynarmic::A64::UserConfig`. Forwarded into `EmitConfig.memory`
    /// at JIT construction time.
    pub memory: crate::backend::common::emit_context::MemoryEmitConfig,
}

impl JitConfig {
    /// Default code cache size: 128 MB.
    pub const DEFAULT_CODE_CACHE_SIZE: usize = 128 * 1024 * 1024;

    pub fn default_coprocessors() -> crate::interface::a32::config::Coprocessors {
        crate::interface::a32::config::empty_coprocessors()
    }

    /// Check whether a specific optimization flag is active.
    ///
    /// Unsafe flags are masked out unless `unsafe_optimizations` is true,
    /// matching dynarmic's `HasOptimization()`.
    pub fn has_optimization(&self, flag: OptimizationFlag) -> bool {
        let mut f = flag;
        if !self.unsafe_optimizations {
            f = f & OptimizationFlag::ALL_SAFE_OPTIMIZATIONS;
        }
        (f & self.optimizations) != OptimizationFlag::NO_OPTIMIZATIONS
    }

    /// Optimization mask visible to backend emitters.
    ///
    /// This is the mask-level equivalent of upstream `HasOptimization`: unsafe
    /// bits never reach an emitter unless the caller explicitly opted in.
    pub fn effective_optimizations(&self) -> OptimizationFlag {
        if self.unsafe_optimizations {
            self.optimizations
        } else {
            self.optimizations & OptimizationFlag::ALL_SAFE_OPTIMIZATIONS
        }
    }
}
