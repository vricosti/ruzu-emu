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

// Temporary compatibility bridge while callers move from the legacy shared
// callback trait to the architecture-owned A32 and A64 interfaces.
impl crate::interface::a32::config::UserCallbacks for dyn UserCallbacks + '_ {
    fn memory_read_code(&self, vaddr: u32) -> Option<u32> {
        UserCallbacks::memory_read_code(self, vaddr as u64)
    }

    fn pre_code_read_hook(
        &self,
        is_thumb: bool,
        pc: u32,
        ir: &mut crate::ir::a32_emitter::A32IREmitter<'_>,
    ) -> bool {
        UserCallbacks::pre_code_read_hook(self, is_thumb, pc, ir)
    }

    fn pre_code_translation_hook(
        &self,
        is_thumb: bool,
        pc: u32,
        ir: &mut crate::ir::a32_emitter::A32IREmitter<'_>,
    ) {
        UserCallbacks::pre_code_translation_hook(self, is_thumb, pc, ir);
    }

    fn get_ticks_for_code(&self, is_thumb: bool, vaddr: u32, instruction: u32) -> u64 {
        UserCallbacks::get_ticks_for_code(self, is_thumb, vaddr, instruction)
    }

    fn memory_read_8(&self, vaddr: u32) -> u8 {
        UserCallbacks::memory_read_8(self, vaddr as u64)
    }

    fn memory_read_16(&self, vaddr: u32) -> u16 {
        UserCallbacks::memory_read_16(self, vaddr as u64)
    }

    fn memory_read_32(&self, vaddr: u32) -> u32 {
        UserCallbacks::memory_read_32(self, vaddr as u64)
    }

    fn memory_read_64(&self, vaddr: u32) -> u64 {
        UserCallbacks::memory_read_64(self, vaddr as u64)
    }

    fn memory_write_8(&mut self, vaddr: u32, value: u8) {
        UserCallbacks::memory_write_8(self, vaddr as u64, value);
    }

    fn memory_write_16(&mut self, vaddr: u32, value: u16) {
        UserCallbacks::memory_write_16(self, vaddr as u64, value);
    }

    fn memory_write_32(&mut self, vaddr: u32, value: u32) {
        UserCallbacks::memory_write_32(self, vaddr as u64, value);
    }

    fn memory_write_64(&mut self, vaddr: u32, value: u64) {
        UserCallbacks::memory_write_64(self, vaddr as u64, value);
    }

    fn memory_write_exclusive_8(&mut self, vaddr: u32, value: u8, expected: u8) -> bool {
        UserCallbacks::exclusive_write_8(self, vaddr as u64, value, expected)
    }

    fn memory_write_exclusive_16(&mut self, vaddr: u32, value: u16, expected: u16) -> bool {
        UserCallbacks::exclusive_write_16(self, vaddr as u64, value, expected)
    }

    fn memory_write_exclusive_32(&mut self, vaddr: u32, value: u32, expected: u32) -> bool {
        UserCallbacks::exclusive_write_32(self, vaddr as u64, value, expected)
    }

    fn memory_write_exclusive_64(&mut self, vaddr: u32, value: u64, expected: u64) -> bool {
        UserCallbacks::exclusive_write_64(self, vaddr as u64, value, expected)
    }

    fn is_read_only_memory(&self, vaddr: u32) -> bool {
        UserCallbacks::is_read_only_memory(self, vaddr)
    }

    fn call_svc(&mut self, swi: u32) {
        UserCallbacks::call_supervisor(self, swi);
    }

    fn exception_raised(&mut self, pc: u32, exception: crate::interface::a32::config::Exception) {
        UserCallbacks::exception_raised(self, pc as u64, exception.as_u32() as u64);
    }

    fn instruction_synchronization_barrier_raised(&mut self) {
        UserCallbacks::instruction_synchronization_barrier_raised(self);
    }

    fn add_ticks(&mut self, ticks: u64) {
        UserCallbacks::add_ticks(self, ticks);
    }

    fn get_ticks_remaining(&self) -> u64 {
        UserCallbacks::get_ticks_remaining(self)
    }

    fn set_halt_reason_ptr(&mut self, ptr: *const u32) {
        UserCallbacks::set_halt_reason_ptr(self, ptr);
    }

    fn set_pc_ptr(&mut self, ptr: *const u32) {
        UserCallbacks::set_pc_ptr(self, ptr);
    }

    fn set_upper_location_descriptor_ptr(&mut self, ptr: *const u32) {
        UserCallbacks::set_upper_location_descriptor_ptr(self, ptr);
    }
}

impl crate::interface::a64::config::UserCallbacks for dyn UserCallbacks + '_ {
    fn memory_read_code(&self, vaddr: u64) -> Option<u32> {
        UserCallbacks::memory_read_code(self, vaddr)
    }

    fn memory_read_8(&self, vaddr: u64) -> u8 {
        UserCallbacks::memory_read_8(self, vaddr)
    }

    fn memory_read_16(&self, vaddr: u64) -> u16 {
        UserCallbacks::memory_read_16(self, vaddr)
    }

    fn memory_read_32(&self, vaddr: u64) -> u32 {
        UserCallbacks::memory_read_32(self, vaddr)
    }

    fn memory_read_64(&self, vaddr: u64) -> u64 {
        UserCallbacks::memory_read_64(self, vaddr)
    }

    fn memory_read_128(&self, vaddr: u64) -> crate::interface::a64::config::Vector {
        let (lo, hi) = UserCallbacks::memory_read_128(self, vaddr);
        [lo, hi]
    }

    fn memory_write_8(&mut self, vaddr: u64, value: u8) {
        UserCallbacks::memory_write_8(self, vaddr, value);
    }

    fn memory_write_16(&mut self, vaddr: u64, value: u16) {
        UserCallbacks::memory_write_16(self, vaddr, value);
    }

    fn memory_write_32(&mut self, vaddr: u64, value: u32) {
        UserCallbacks::memory_write_32(self, vaddr, value);
    }

    fn memory_write_64(&mut self, vaddr: u64, value: u64) {
        UserCallbacks::memory_write_64(self, vaddr, value);
    }

    fn memory_write_128(&mut self, vaddr: u64, value: crate::interface::a64::config::Vector) {
        UserCallbacks::memory_write_128(self, vaddr, value[0], value[1]);
    }

    fn memory_write_exclusive_8(&mut self, vaddr: u64, value: u8, expected: u8) -> bool {
        UserCallbacks::exclusive_write_8(self, vaddr, value, expected)
    }

    fn memory_write_exclusive_16(&mut self, vaddr: u64, value: u16, expected: u16) -> bool {
        UserCallbacks::exclusive_write_16(self, vaddr, value, expected)
    }

    fn memory_write_exclusive_32(&mut self, vaddr: u64, value: u32, expected: u32) -> bool {
        UserCallbacks::exclusive_write_32(self, vaddr, value, expected)
    }

    fn memory_write_exclusive_64(&mut self, vaddr: u64, value: u64, expected: u64) -> bool {
        UserCallbacks::exclusive_write_64(self, vaddr, value, expected)
    }

    fn memory_write_exclusive_128(
        &mut self,
        vaddr: u64,
        value: crate::interface::a64::config::Vector,
        expected: crate::interface::a64::config::Vector,
    ) -> bool {
        UserCallbacks::exclusive_write_128(
            self,
            vaddr,
            value[0],
            value[1],
            expected[0],
            expected[1],
        )
    }

    fn is_read_only_memory(&self, vaddr: u64) -> bool {
        UserCallbacks::is_read_only_memory(self, vaddr as u32)
    }

    fn call_svc(&mut self, swi: u32) {
        UserCallbacks::call_supervisor(self, swi);
    }

    fn exception_raised(&mut self, pc: u64, exception: crate::interface::a64::config::Exception) {
        UserCallbacks::exception_raised(self, pc, exception as u64);
    }

    fn data_cache_operation_raised(
        &mut self,
        op: crate::interface::a64::config::DataCacheOperation,
        value: u64,
    ) {
        UserCallbacks::data_cache_operation(self, op as u64, value);
    }

    fn instruction_cache_operation_raised(
        &mut self,
        op: crate::interface::a64::config::InstructionCacheOperation,
        value: u64,
    ) {
        UserCallbacks::instruction_cache_operation(self, op as u64, value);
    }

    fn instruction_synchronization_barrier_raised(&mut self) {
        UserCallbacks::instruction_synchronization_barrier_raised(self);
    }

    fn add_ticks(&mut self, ticks: u64) {
        UserCallbacks::add_ticks(self, ticks);
    }

    fn get_ticks_remaining(&self) -> u64 {
        UserCallbacks::get_ticks_remaining(self)
    }

    fn get_cntpct(&self) -> u64 {
        UserCallbacks::get_cntpct(self)
    }
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

    /// Convert the legacy merged configuration at the public compatibility
    /// boundary into Eden's A32-owned configuration.
    pub fn into_a32_user_config(self) -> crate::interface::a32::config::UserConfig {
        use crate::interface::a32::config::UserConfig;

        let page_table = self
            .page_table_pointer
            .map(|pointer| pointer as *mut [*mut u8; UserConfig::NUM_PAGE_TABLE_ENTRIES]);
        let memory = self.memory;
        let callbacks = Box::new(LegacyA32Callbacks(self.callbacks));

        UserConfig {
            callbacks,
            global_monitor: self.global_monitor,
            page_table,
            coprocessors: self.coprocessors,
            fastmem_pointer: self.fastmem_pointer,
            optimizations: self.optimizations,
            code_cache_size: self
                .code_cache_size
                .try_into()
                .expect("A32 code cache size must fit u32"),
            page_table_pointer_mask_bits: memory
                .page_table_pointer_mask_bits
                .try_into()
                .expect("A32 page-table pointer mask must fit i32"),
            page_table_log2_stride: memory.page_table_log2_stride,
            arch_version: self.arch_version,
            processor_id: self
                .processor_id
                .try_into()
                .expect("A32 processor id must fit u8"),
            detect_misaligned_access_via_page_table: memory
                .detect_misaligned_access_via_page_table
                .try_into()
                .expect("A32 misalignment mask must fit u8"),
            unsafe_optimizations: self.unsafe_optimizations,
            absolute_offset_page_table: memory.absolute_offset_page_table,
            only_detect_misalignment_via_page_table_on_page_boundary: memory
                .only_detect_misalignment_via_page_table_on_page_boundary,
            recompile_on_fastmem_failure: memory.recompile_on_fastmem_failure,
            fastmem_exclusive_access: memory.fastmem_exclusive_access,
            recompile_on_exclusive_fastmem_failure: memory.recompile_on_exclusive_fastmem_failure,
            hook_isb: self.hook_isb,
            hook_hint_instructions: self.hook_hint_instructions,
            define_unpredictable_behaviour: self.define_unpredictable_behaviour,
            wall_clock_cntpct: self.wall_clock_cntpct,
            check_halt_on_memory_access: memory.check_halt_on_memory_access,
            enable_cycle_counting: self.enable_cycle_counting,
            always_little_endian: false,
            very_verbose_debugging_output: false,
        }
    }
}

struct LegacyA32Callbacks(Box<dyn UserCallbacks>);

impl crate::interface::a32::config::UserCallbacks for LegacyA32Callbacks {
    fn memory_read_code(&self, vaddr: u32) -> Option<u32> {
        crate::interface::a32::config::UserCallbacks::memory_read_code(self.0.as_ref(), vaddr)
    }

    fn pre_code_read_hook(
        &self,
        is_thumb: bool,
        pc: u32,
        ir: &mut crate::ir::a32_emitter::A32IREmitter<'_>,
    ) -> bool {
        crate::interface::a32::config::UserCallbacks::pre_code_read_hook(
            self.0.as_ref(),
            is_thumb,
            pc,
            ir,
        )
    }

    fn pre_code_translation_hook(
        &self,
        is_thumb: bool,
        pc: u32,
        ir: &mut crate::ir::a32_emitter::A32IREmitter<'_>,
    ) {
        crate::interface::a32::config::UserCallbacks::pre_code_translation_hook(
            self.0.as_ref(),
            is_thumb,
            pc,
            ir,
        );
    }

    fn get_ticks_for_code(&self, is_thumb: bool, vaddr: u32, instruction: u32) -> u64 {
        crate::interface::a32::config::UserCallbacks::get_ticks_for_code(
            self.0.as_ref(),
            is_thumb,
            vaddr,
            instruction,
        )
    }

    fn memory_read_8(&self, vaddr: u32) -> u8 {
        crate::interface::a32::config::UserCallbacks::memory_read_8(self.0.as_ref(), vaddr)
    }

    fn memory_read_16(&self, vaddr: u32) -> u16 {
        crate::interface::a32::config::UserCallbacks::memory_read_16(self.0.as_ref(), vaddr)
    }

    fn memory_read_32(&self, vaddr: u32) -> u32 {
        crate::interface::a32::config::UserCallbacks::memory_read_32(self.0.as_ref(), vaddr)
    }

    fn memory_read_64(&self, vaddr: u32) -> u64 {
        crate::interface::a32::config::UserCallbacks::memory_read_64(self.0.as_ref(), vaddr)
    }

    fn memory_write_8(&mut self, vaddr: u32, value: u8) {
        crate::interface::a32::config::UserCallbacks::memory_write_8(self.0.as_mut(), vaddr, value);
    }

    fn memory_write_16(&mut self, vaddr: u32, value: u16) {
        crate::interface::a32::config::UserCallbacks::memory_write_16(
            self.0.as_mut(),
            vaddr,
            value,
        );
    }

    fn memory_write_32(&mut self, vaddr: u32, value: u32) {
        crate::interface::a32::config::UserCallbacks::memory_write_32(
            self.0.as_mut(),
            vaddr,
            value,
        );
    }

    fn memory_write_64(&mut self, vaddr: u32, value: u64) {
        crate::interface::a32::config::UserCallbacks::memory_write_64(
            self.0.as_mut(),
            vaddr,
            value,
        );
    }

    fn memory_write_exclusive_8(&mut self, vaddr: u32, value: u8, expected: u8) -> bool {
        crate::interface::a32::config::UserCallbacks::memory_write_exclusive_8(
            self.0.as_mut(),
            vaddr,
            value,
            expected,
        )
    }

    fn memory_write_exclusive_16(&mut self, vaddr: u32, value: u16, expected: u16) -> bool {
        crate::interface::a32::config::UserCallbacks::memory_write_exclusive_16(
            self.0.as_mut(),
            vaddr,
            value,
            expected,
        )
    }

    fn memory_write_exclusive_32(&mut self, vaddr: u32, value: u32, expected: u32) -> bool {
        crate::interface::a32::config::UserCallbacks::memory_write_exclusive_32(
            self.0.as_mut(),
            vaddr,
            value,
            expected,
        )
    }

    fn memory_write_exclusive_64(&mut self, vaddr: u32, value: u64, expected: u64) -> bool {
        crate::interface::a32::config::UserCallbacks::memory_write_exclusive_64(
            self.0.as_mut(),
            vaddr,
            value,
            expected,
        )
    }

    fn is_read_only_memory(&self, vaddr: u32) -> bool {
        crate::interface::a32::config::UserCallbacks::is_read_only_memory(self.0.as_ref(), vaddr)
    }

    fn call_svc(&mut self, swi: u32) {
        crate::interface::a32::config::UserCallbacks::call_svc(self.0.as_mut(), swi);
    }

    fn exception_raised(&mut self, pc: u32, exception: crate::interface::a32::config::Exception) {
        crate::interface::a32::config::UserCallbacks::exception_raised(
            self.0.as_mut(),
            pc,
            exception,
        );
    }

    fn instruction_synchronization_barrier_raised(&mut self) {
        crate::interface::a32::config::UserCallbacks::instruction_synchronization_barrier_raised(
            self.0.as_mut(),
        );
    }

    fn add_ticks(&mut self, ticks: u64) {
        crate::interface::a32::config::UserCallbacks::add_ticks(self.0.as_mut(), ticks);
    }

    fn get_ticks_remaining(&self) -> u64 {
        crate::interface::a32::config::UserCallbacks::get_ticks_remaining(self.0.as_ref())
    }

    fn set_halt_reason_ptr(&mut self, ptr: *const u32) {
        crate::interface::a32::config::UserCallbacks::set_halt_reason_ptr(self.0.as_mut(), ptr);
    }

    fn set_pc_ptr(&mut self, ptr: *const u32) {
        crate::interface::a32::config::UserCallbacks::set_pc_ptr(self.0.as_mut(), ptr);
    }

    fn set_upper_location_descriptor_ptr(&mut self, ptr: *const u32) {
        crate::interface::a32::config::UserCallbacks::set_upper_location_descriptor_ptr(
            self.0.as_mut(),
            ptr,
        );
    }
}

impl From<JitConfig> for crate::interface::a32::config::UserConfig {
    fn from(config: JitConfig) -> Self {
        config.into_a32_user_config()
    }
}
