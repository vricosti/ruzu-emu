use std::cell::RefCell;

use crate::backend::x64::a32_jitstate::A32JitState;
use crate::backend::x64::a64_jitstate::A64JitState;
use crate::backend::x64::callback::Callback;
use crate::backend::x64::exception_handler::FastmemPatchTable;
use crate::backend::x64::host_feature::HostFeature;
use crate::backend::x64::patch_info::PatchEntry;
use crate::common::fp::fpcr::Fpcr;
use crate::interface::a32::config::Coprocessors;
use crate::interface::optimization_flags::OptimizationFlag;
use crate::ir::location::{A32LocationDescriptor, A64LocationDescriptor, LocationDescriptor};

pub use crate::backend::common::emit_context::MemoryEmitConfig;

/// Context passed to a deferred-emit closure when the post-block "abort
/// handler" code is generated.
///
/// Matches upstream's pattern in `emit_x64_memory.cpp.inc` where each
/// fastmem (or page-table) memory access pushes a closure onto
/// `ctx.deferred_emits`. After the main block is emitted, those closures
/// run in order and emit the abort/fallback handlers at the end of the
/// block, then bind the `end` label so the fast path falls through.
pub struct DeferredEmitCtx<'a> {
    /// Mutable reference to the assembler so the closure can emit code
    /// (bind labels, call wrapped fallback, jump to end).
    pub asm: &'a mut rxbyak::CodeAssembler,
    /// Mutable reference to the per-emitter fastmem patch table so the
    /// closure can record `inst_rip → FastmemPatchInfo`.
    pub fastmem_patches: &'a mut FastmemPatchTable,
    /// Absolute base address of the code buffer at drain time.
    /// Closures use this to convert code-buffer offsets they captured
    /// during main emission into the absolute RIPs the SIGSEGV handler
    /// needs in the patch table.
    pub code_base: u64,
}

/// One deferred-emit closure. Drained after the main block is emitted.
pub type DeferredEmit = Box<dyn FnOnce(&mut DeferredEmitCtx<'_>)>;

/// Info about a fastmem instruction recorded during emission.
#[derive(Clone, Debug)]
pub struct FastmemEntry {
    /// Code offset of the fastmem mov instruction (relative to code base).
    pub inst_offset: usize,
    /// Code offset right after the fastmem instruction (resume point).
    pub resume_offset: usize,
    /// Bit size of the memory access (8, 16, 32, 64).
    pub bitsize: usize,
    /// Whether this is a write (true) or read (false).
    pub is_write: bool,
    /// Whether the access belongs to an inline exclusive sequence. Faulting
    /// exclusive writes must call the raw compare-and-store callback because
    /// the generated code already holds the monitor lock.
    pub is_exclusive: bool,
    /// Whether this access is `AccType::Ordered` (LDA / STL / LDAEX / STLEX).
    /// When true, the slow-path stub emits `mfence` around the callback
    /// (before for reads, after for writes), matching upstream
    /// `GenFastmemFallbacks` in `a32_emit_x64_memory.cpp:60-96`.
    pub ordered: bool,
    /// Register index holding the virtual address (0=RAX, 1=RCX, etc.).
    pub vaddr_reg: u8,
    /// Register index for the value (result for reads, source for writes).
    pub value_reg: u8,
    /// Identifies this memory microinstruction within its source block.
    ///
    /// Matches upstream `DoNotFastmemMarker` and is used to disable only this
    /// access when its direct fastmem instruction faults.
    pub marker: crate::backend::x64::exception_handler::DoNotFastmemMarker,
    /// Fault policy selected for this ordinary or exclusive access.
    pub recompile: bool,
}

/// Architecture-specific configuration for terminal emission.
///
/// Provides the correct JitState field offsets and location descriptor
/// interpretation for A32 vs A64 blocks. This avoids hardcoding A64 offsets
/// in the shared `emit_terminal.rs` code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchConfig {
    A64,
    A32,
}

impl ArchConfig {
    /// Offset of the PC field in the JitState struct.
    /// A64: `A64JitState::pc` (qword at ~offset 256).
    /// A32: `A32JitState::reg[15]` (dword at offset 60).
    pub fn pc_offset(self) -> usize {
        match self {
            Self::A64 => A64JitState::offset_of_pc(),
            Self::A32 => A32JitState::reg_offset(15),
        }
    }

    /// Width of the PC value in bytes (8 for A64, 4 for A32).
    pub fn pc_width(self) -> usize {
        match self {
            Self::A64 => 8,
            Self::A32 => 4,
        }
    }

    /// Offset of `halt_reason` in the JitState struct.
    pub fn halt_reason_offset(self) -> usize {
        match self {
            Self::A64 => A64JitState::offset_of_halt_reason(),
            Self::A32 => A32JitState::offset_of_halt_reason(),
        }
    }

    /// Extract the PC value from a generic LocationDescriptor.
    /// A64: sign-extended 56-bit PC from A64LocationDescriptor.
    /// A32: lower 32 bits from A32LocationDescriptor.
    pub fn extract_pc(self, loc: LocationDescriptor) -> u64 {
        match self {
            Self::A64 => A64LocationDescriptor::from_location(loc).pc(),
            Self::A32 => A32LocationDescriptor::from_location(loc).pc() as u64,
        }
    }

    /// Extract the single_stepping flag from a LocationDescriptor.
    pub fn extract_single_stepping(self, loc: LocationDescriptor) -> bool {
        match self {
            Self::A64 => A64LocationDescriptor::from_location(loc).single_stepping(),
            Self::A32 => A32LocationDescriptor::from_location(loc).single_stepping(),
        }
    }

    /// Offset of `upper_location_descriptor` in A32JitState (None for A64).
    pub fn upper_location_descriptor_offset(self) -> Option<usize> {
        match self {
            Self::A64 => None,
            Self::A32 => Some(A32JitState::offset_of_upper_location_descriptor()),
        }
    }

    /// Compute the upper_location_descriptor value for an A32 location.
    /// Returns 0 for A64 (no upper descriptor).
    pub fn extract_upper_location_descriptor(self, loc: LocationDescriptor) -> u32 {
        match self {
            Self::A64 => 0,
            Self::A32 => A32LocationDescriptor::from_location(loc).upper_location_descriptor(),
        }
    }

    /// Offset of `cpsr_nzcv` in the JitState struct.
    pub fn cpsr_nzcv_offset(self) -> usize {
        match self {
            Self::A64 => A64JitState::offset_of_cpsr_nzcv(),
            Self::A32 => A32JitState::offset_of_cpsr_nzcv(),
        }
    }

    pub fn fpsr_exc_offset(self) -> usize {
        match self {
            Self::A64 => A64JitState::offset_of_fpsr_exc(),
            Self::A32 => A32JitState::offset_of_fpsr_exc(),
        }
    }

    pub fn fpsr_qc_offset(self) -> usize {
        match self {
            Self::A64 => A64JitState::offset_of_fpsr_qc(),
            Self::A32 => A32JitState::offset_of_fpsr_qc(),
        }
    }

    pub fn guest_mxcsr_offset(self) -> usize {
        match self {
            Self::A64 => A64JitState::offset_of_guest_mxcsr(),
            Self::A32 => A32JitState::offset_of_guest_mxcsr(),
        }
    }

    pub fn asimd_mxcsr_offset(self) -> usize {
        match self {
            Self::A64 => A64JitState::offset_of_asimd_mxcsr(),
            Self::A32 => A32JitState::offset_of_asimd_mxcsr(),
        }
    }

    pub fn is_a32(self) -> bool {
        matches!(self, Self::A32)
    }
}

/// Descriptor for a compiled block of native code.
pub struct BlockDescriptor {
    /// Offset into the code buffer where the block begins.
    pub entrypoint_offset: usize,
    /// Size of the emitted native code in bytes.
    pub size: usize,
}

/// Callbacks used by the emitter for operations that require host interaction.
pub struct EmitCallbacks {
    /// Read memory: fn(vaddr: u64) -> value (8/16/32/64-bit, zero-extended in RAX).
    pub memory_read_8: Box<dyn Callback>,
    pub memory_read_16: Box<dyn Callback>,
    pub memory_read_32: Box<dyn Callback>,
    pub memory_read_64: Box<dyn Callback>,
    pub memory_read_128: Box<dyn Callback>,

    /// Write memory: fn(vaddr: u64, value: u64).
    pub memory_write_8: Box<dyn Callback>,
    pub memory_write_16: Box<dyn Callback>,
    pub memory_write_32: Box<dyn Callback>,
    pub memory_write_64: Box<dyn Callback>,
    pub memory_write_128: Box<dyn Callback>,

    /// Called when SVC is executed.
    pub call_supervisor: Box<dyn Callback>,

    /// Called when an exception is raised.
    pub exception_raised: Box<dyn Callback>,

    /// Called for data cache operations.
    pub data_cache_operation: Box<dyn Callback>,

    /// Called for instruction cache operations.
    pub instruction_cache_operation: Box<dyn Callback>,

    /// Called when an instruction synchronization barrier is executed.
    pub instruction_synchronization_barrier: Box<dyn Callback>,

    /// Called to add ticks when returning from JIT.
    pub add_ticks: Box<dyn Callback>,

    /// Called to get remaining tick budget.
    pub get_ticks_remaining: Box<dyn Callback>,

    /// Exclusive memory: clear exclusive monitor.
    pub exclusive_clear: Box<dyn Callback>,

    /// Exclusive read memory: fn(vaddr: u64) -> value.
    pub exclusive_read_8: Box<dyn Callback>,
    pub exclusive_read_16: Box<dyn Callback>,
    pub exclusive_read_32: Box<dyn Callback>,
    pub exclusive_read_64: Box<dyn Callback>,
    pub exclusive_read_128: Box<dyn Callback>,

    /// Get CNTPCT_EL0 (counter-timer physical count): fn() -> u64.
    pub get_cntpct: Box<dyn Callback>,

    /// Exclusive write memory: fn(vaddr: u64, value: u64) -> status (0=success).
    pub exclusive_write_8: Box<dyn Callback>,
    pub exclusive_write_16: Box<dyn Callback>,
    pub exclusive_write_32: Box<dyn Callback>,
    pub exclusive_write_64: Box<dyn Callback>,
    pub exclusive_write_128: Box<dyn Callback>,
}

/// Raw exclusive-store callbacks used by the inline fastmem fallback.
///
/// Unlike `EmitCallbacks::exclusive_write_*`, these callbacks do not inspect
/// `JitState::exclusive_state` and do not enter `ExclusiveMonitor`: the
/// generated inline path has already validated the reservation and holds the
/// monitor lock. This mirrors upstream's direct calls to
/// `UserCallbacks::MemoryWriteExclusive*` from `exclusive_write_fallbacks`.
pub struct RawExclusiveWriteCallbacks {
    pub write_8: Box<dyn Callback>,
    pub write_16: Box<dyn Callback>,
    pub write_32: Box<dyn Callback>,
    pub write_64: Box<dyn Callback>,
    /// The 128-bit callback receives `(vaddr, value_ptr, expected_ptr)` after
    /// the fixed JIT context parameter. The pointer adaptation keeps both
    /// System V and Windows within their four-register ABI limit.
    pub write_128: Box<dyn Callback>,
}

/// Configuration for the A64 emitter.
pub struct EmitConfig {
    /// Configurable A32 coprocessors from `A32::UserConfig`.
    pub coprocessors: Coprocessors,
    /// Callbacks for host-side operations.
    pub callbacks: EmitCallbacks,
    /// Direct user callbacks for faulting inline exclusive stores.
    pub raw_exclusive_write_callbacks: Option<RawExclusiveWriteCallbacks>,
    /// Whether cycle counting is enabled.
    pub enable_cycle_counting: bool,
    /// Memory emission options (fastmem + page-table behavior).
    pub memory: MemoryEmitConfig,
    /// Raw pointer to the shared exclusive monitor (when present).
    ///
    /// Set during JIT construction from `UserConfig::global_monitor`.
    /// The JIT-emitted inline LDREX / STREX sequences use this pointer to
    /// compute absolute addresses for the monitor's lock, the per-processor
    /// reservation address, and the per-processor saved value — matching
    /// upstream's `GetExclusiveMonitorLockPointer` /
    /// `GetExclusiveMonitorAddressPointer` / `GetExclusiveMonitorValuePointer`.
    pub global_monitor: Option<*mut crate::interface::exclusive_monitor::ExclusiveMonitor>,
    /// Stable backing pointers embedded in A64 TPIDR instructions.
    ///
    /// Upstream owner: `A64::UserConfig::{tpidr_el0,tpidrro_el0}`.
    pub tpidrro_el0: Option<*const u64>,
    pub tpidr_el0: Option<*mut u64>,
    /// Counter-timer frequency returned for `MRS CNTFRQ_EL0`.
    /// Upstream `A64::UserConfig::cntfrq_el0`; forwarded from the architecture-owned config.
    pub cntfrq_el0: u32,
    /// Cache-type register returned for `MRS CTR_EL0`.
    pub ctr_el0: u32,
    /// Data-cache zero ID register returned for `MRS DCZID_EL0` and consumed
    /// by the A64 callback-configuration pass.
    pub dczid_el0: u32,
    /// Whether data-cache maintenance operations reach the user callback.
    pub hook_data_cache_operations: bool,
    /// Whether ISB instructions reach the user callback.
    pub hook_isb: bool,
}

/// Per-block emission context.
///
/// Holds the location descriptor for the block being emitted and
/// a reference to the shared emitter configuration.
pub struct EmitContext<'a> {
    /// Location descriptor for the current block.
    pub location: LocationDescriptor,
    /// Emitter configuration and callbacks.
    pub config: &'a EmitConfig,
    /// Copy of the immutable mask owned by `BlockOfCode`.
    ///
    /// Upstream emitters query `BlockOfCode::HasHostFeature` directly. Rust
    /// splits the assembler and constant pool into independent mutable
    /// borrows during emission, so the context carries the same mask by value.
    pub host_features: HostFeature,
    /// Effective optimization mask after applying `unsafe_optimizations`.
    /// Upstream exposes this through virtual `EmitContext::HasOptimization`.
    pub optimizations: OptimizationFlag,
    /// Architecture-specific configuration (A32 vs A64).
    /// Controls PC offset, halt_reason offset, location descriptor parsing.
    pub arch: ArchConfig,
    /// Dispatcher return_from_run_code offsets (4 entries).
    ///
    /// When `Some`, terminals emit `jmp rel32` to these absolute code buffer
    /// offsets instead of inline `ret`. When `None` (e.g., in unit tests),
    /// terminals emit `ret` directly for standalone testing.
    pub dispatcher_offsets: Option<[usize; 4]>,
    /// Base pointer of the code buffer (needed to compute `jmp rel32` targets).
    pub code_base_ptr: *const u8,
    /// Whether this block is being compiled for single-stepping.
    /// When true, block linking and fast dispatch are disabled.
    pub is_single_step: bool,
    /// Whether block linking (direct jumps between blocks) is enabled.
    pub enable_block_linking: bool,
    /// Patch entries collected during emission (populated by terminal emitters).
    /// Uses RefCell so terminal emitters can append through `&EmitContext`.
    pub patch_entries: RefCell<Vec<PatchEntry>>,
    /// Block lookup function for checking if a target is already compiled.
    /// Returns the native code entrypoint if the block is cached.
    pub block_lookup: Option<Box<dyn Fn(LocationDescriptor) -> Option<*const u8> + 'a>>,
    /// Code buffer offset of the PopRSBHint terminal handler (prelude code).
    pub terminal_handler_pop_rsb_hint: Option<usize>,
    /// Code buffer offset of the FastDispatchHint terminal handler (prelude code).
    pub terminal_handler_fast_dispatch_hint: Option<usize>,
    /// Whether RSB optimization is enabled.
    pub enable_rsb: bool,
    /// Whether fast dispatch table is enabled.
    pub enable_fast_dispatch: bool,
    /// Reference to the IR block being emitted.
    /// Used by emit handlers to find associated pseudo-operations
    /// (GetCarryFromOp, GetOverflowFromOp, GetNZCVFromOp) via
    /// block.get_associated_pseudo_operation(), matching upstream's
    /// inst->GetAssociatedPseudoOperation().
    pub block: Option<&'a crate::ir::block::Block>,
    /// End location of the current block (set before emission).
    /// Used by UpdateUpperLocationDescriptor to compute the new upper descriptor.
    pub end_location: Option<LocationDescriptor>,
    /// Whether the current block contains a BXWritePC instruction.
    /// If true, UpdateUpperLocationDescriptor is a no-op (BXWritePC handles it).
    pub has_bx_write_pc: bool,
    /// Whether fastmem is available for this block.
    /// When true, memory accesses emit `[R13 + vaddr]` instead of callbacks.
    pub fastmem_available: bool,
    /// Per-emitter set of memory microinstructions disabled after a fastmem
    /// fault. The emitter owns this set for the lifetime of this context.
    pub do_not_fastmem: Option<
        &'a std::collections::HashSet<crate::backend::x64::exception_handler::DoNotFastmemMarker>,
    >,
    /// Fastmem instruction info collected during emission.
    /// Converted to absolute RIPs and fallback stubs after block emission.
    ///
    /// Used by the existing per-emission A32 fastmem path; the upstream-
    /// faithful A64 path does not populate this and uses `deferred_emits`
    /// instead to record patches.
    pub fastmem_entries: RefCell<Vec<FastmemEntry>>,
    /// Closures to run after the main block has been emitted, to emit
    /// abort/fallback handlers for fastmem and page-table memory accesses.
    ///
    /// Matches upstream `ctx.deferred_emits` in `emit_x64_memory.cpp.inc`.
    /// Each closure binds an `abort` label at the current code position,
    /// calls the wrapped fallback stub, records the patch entry in the
    /// emitter's `fastmem_patch_info`, and jumps to the `end` label that
    /// was placed inline by the main emit path so the fast path skips the
    /// abort handler.
    pub deferred_emits: RefCell<Vec<DeferredEmit>>,
    /// Reference to the per-emitter pre-generated fastmem fallback-stub
    /// table. Set by the A64 emitter when a block compile starts; used
    /// by the memory dispatchers to look up the per-(ordered, bitsize,
    /// vaddr_idx, value_idx) fallback stub offset that corresponds to
    /// the access being emitted.
    ///
    /// Stored as a raw pointer to keep `EmitContext` lifetime-clean
    /// (the table outlives the context — it's owned by `A64EmitX64`).
    /// Cast back to `&FastmemFallbacksTable` at use time.
    pub fastmem_fallbacks: Option<*const ()>,
    /// `RUZU_BLOCK_PROLOGUE_COUNT_PC` per-core counter address. When `Some`,
    /// `emit_block` emits `lock inc qword [counter_addr]` immediately after
    /// the entrypoint offset is captured, so the increment runs on every
    /// block entry (including FAST_DISPATCH-chained entries). `Cell` so the
    /// outer compile path can set it through `&EmitContext`.
    pub prologue_counter_addr: std::cell::Cell<Option<u64>>,
}

impl<'a> EmitContext<'a> {
    pub fn new(location: LocationDescriptor, config: &'a EmitConfig) -> Self {
        Self {
            location,
            config,
            host_features: crate::backend::x64::block_of_code::get_host_features(),
            optimizations: OptimizationFlag::NO_OPTIMIZATIONS,
            arch: ArchConfig::A64,
            dispatcher_offsets: None,
            code_base_ptr: std::ptr::null(),
            is_single_step: false,
            enable_block_linking: false,
            patch_entries: RefCell::new(Vec::new()),
            block_lookup: None,
            terminal_handler_pop_rsb_hint: None,
            terminal_handler_fast_dispatch_hint: None,
            enable_rsb: false,
            enable_fast_dispatch: false,
            block: None,
            end_location: None,
            has_bx_write_pc: false,
            fastmem_available: false,
            do_not_fastmem: None,
            fastmem_entries: RefCell::new(Vec::new()),
            deferred_emits: RefCell::new(Vec::new()),
            fastmem_fallbacks: None,
            prologue_counter_addr: std::cell::Cell::new(None),
        }
    }

    pub fn with_dispatcher(
        location: LocationDescriptor,
        config: &'a EmitConfig,
        arch: ArchConfig,
        host_features: HostFeature,
        optimizations: OptimizationFlag,
        dispatcher_offsets: [usize; 4],
        code_base_ptr: *const u8,
    ) -> Self {
        Self {
            location,
            config,
            host_features,
            optimizations,
            arch,
            dispatcher_offsets: Some(dispatcher_offsets),
            code_base_ptr,
            is_single_step: arch.extract_single_stepping(location),
            enable_block_linking: false,
            patch_entries: RefCell::new(Vec::new()),
            block_lookup: None,
            terminal_handler_pop_rsb_hint: None,
            terminal_handler_fast_dispatch_hint: None,
            enable_rsb: false,
            enable_fast_dispatch: false,
            block: None,
            end_location: None,
            has_bx_write_pc: false,
            fastmem_available: false,
            do_not_fastmem: None,
            fastmem_entries: RefCell::new(Vec::new()),
            deferred_emits: RefCell::new(Vec::new()),
            fastmem_fallbacks: None,
            prologue_counter_addr: std::cell::Cell::new(None),
        }
    }

    /// Take collected patch entries out of the context.
    pub fn take_patch_entries(&self) -> Vec<PatchEntry> {
        self.patch_entries.borrow_mut().drain(..).collect()
    }

    pub fn has_host_feature(&self, feature: HostFeature) -> bool {
        self.host_features.contains(feature)
    }

    pub fn has_optimization(&self, flag: OptimizationFlag) -> bool {
        self.optimizations.contains(flag)
    }

    pub fn fpcr(&self, fpcr_controlled: bool) -> Fpcr {
        let fpcr = match self.arch {
            ArchConfig::A64 => Fpcr::new(A64LocationDescriptor::from(self.location).fpcr()),
            ArchConfig::A32 => Fpcr::new(
                A32LocationDescriptor::from_location(self.location)
                    .fpscr()
                    .value(),
            ),
        };
        if fpcr_controlled {
            fpcr
        } else {
            fpcr.asimd_standard_value()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_block_descriptor() {
        let desc = BlockDescriptor {
            entrypoint_offset: 0x100,
            size: 64,
        };
        assert_eq!(desc.entrypoint_offset, 0x100);
        assert_eq!(desc.size, 64);
    }

    #[test]
    fn architecture_selects_its_own_saturation_flag_offset() {
        assert_eq!(
            ArchConfig::A64.fpsr_qc_offset(),
            A64JitState::offset_of_fpsr_qc()
        );
        assert_eq!(
            ArchConfig::A32.fpsr_qc_offset(),
            A32JitState::offset_of_fpsr_qc()
        );
        assert_ne!(
            ArchConfig::A64.fpsr_qc_offset(),
            ArchConfig::A32.fpsr_qc_offset()
        );
    }
}
