use std::collections::HashSet;
use std::ffi::c_void;
use std::ops::RangeInclusive;
use std::path::PathBuf;

use crate::backend::common::a32_callbacks::{self, A32ExclusiveState};
use crate::exclusive_monitor::ExclusiveMonitor;
use crate::frontend::a32::translate::translate;
use crate::ir::block::Block;
use crate::ir::location::{A32LocationDescriptor, LocationDescriptor};
use crate::ir::opt;
use crate::ir::terminal::Terminal;
use crate::jit_config::{JitConfig, OptimizationFlag, UserCallbacks};

use super::address_space::AddressSpace;
use super::emit_arm64::{CodePtr, EmitConfig};
use super::fast_hash::arm64_code_cache_profile_enabled;
use super::jit_state::A32JitState;
use super::prelude::{DispatcherCallback, PreludeIsa, PreludeOptions, TickCallbacks};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockRange32 {
    pub start: u32,
    pub end: u32,
    pub descriptor: LocationDescriptor,
}

/// A32-specific ARM64 address-space owner.
///
/// Upstream owner: `backend/arm64/a32_address_space.h/.cpp`.
pub struct A32AddressSpace {
    address_space: AddressSpace,
    conf: JitConfig,
    block_ranges: Vec<BlockRange32>,
    cp15_uprw: Box<u32>,
    cp15_uro: Box<u32>,
    dispatcher_entries: u64,
    dispatcher_cache_hits: u64,
    dispatcher_compiles: u64,
    terminal_link_block: u64,
    terminal_link_block_fast: u64,
    terminal_pop_rsb_hint: u64,
    terminal_fast_dispatch_hint: u64,
    terminal_return_to_dispatch: u64,
    terminal_other: u64,
    compiled_live_insts: u64,
    profile_code_cache: bool,
}

#[derive(Clone, Copy)]
pub struct A32NormalCallbackFns {
    pub read_memory_8: *const c_void,
    pub read_memory_16: *const c_void,
    pub read_memory_32: *const c_void,
    pub read_memory_64: *const c_void,
    pub write_memory_8: *const c_void,
    pub write_memory_16: *const c_void,
    pub write_memory_32: *const c_void,
    pub write_memory_64: *const c_void,
    pub call_svc: *const c_void,
    pub exception_raised: *const c_void,
    pub isb_raised: *const c_void,
    pub add_ticks: *const c_void,
    pub get_ticks_remaining: *const c_void,
    pub get_cntpct: *const c_void,
}

#[derive(Clone, Copy)]
pub struct A32CallbackFns {
    pub read_memory_8: *const c_void,
    pub read_memory_16: *const c_void,
    pub read_memory_32: *const c_void,
    pub read_memory_64: *const c_void,
    pub exclusive_read_memory_8: *const c_void,
    pub exclusive_read_memory_16: *const c_void,
    pub exclusive_read_memory_32: *const c_void,
    pub exclusive_read_memory_64: *const c_void,
    pub write_memory_8: *const c_void,
    pub write_memory_16: *const c_void,
    pub write_memory_32: *const c_void,
    pub write_memory_64: *const c_void,
    pub exclusive_write_memory_8: *const c_void,
    pub exclusive_write_memory_16: *const c_void,
    pub exclusive_write_memory_32: *const c_void,
    pub exclusive_write_memory_64: *const c_void,
    pub call_svc: *const c_void,
    pub exception_raised: *const c_void,
    pub isb_raised: *const c_void,
    pub add_ticks: *const c_void,
    pub get_ticks_remaining: *const c_void,
    pub get_cntpct: *const c_void,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalKind {
    LinkBlock,
    LinkBlockFast,
    PopRsbHint,
    FastDispatchHint,
    ReturnToDispatch,
    Other,
}

fn classify_terminal(terminal: &Terminal) -> TerminalKind {
    match terminal {
        Terminal::LinkBlock { .. } => TerminalKind::LinkBlock,
        Terminal::LinkBlockFast { .. } => TerminalKind::LinkBlockFast,
        Terminal::PopRSBHint => TerminalKind::PopRsbHint,
        Terminal::FastDispatchHint => TerminalKind::FastDispatchHint,
        Terminal::ReturnToDispatch => TerminalKind::ReturnToDispatch,
        Terminal::If { then_, else_, .. } | Terminal::CheckBit { then_, else_ } => {
            let then_kind = classify_terminal(then_);
            let else_kind = classify_terminal(else_);
            if then_kind == else_kind {
                then_kind
            } else {
                TerminalKind::Other
            }
        }
        Terminal::CheckHalt { else_ } => classify_terminal(else_),
        Terminal::Invalid | Terminal::Interpret { .. } => TerminalKind::Other,
    }
}

/// Stable Rust context for A32 ARM64 prelude callback thunks.
///
/// Upstream stores `const A32::UserConfig& conf` in `A32AddressSpace` and emits
/// trampolines that either call `conf.callbacks` directly or pass `&conf` into
/// exclusive-monitor helpers. Rust cannot devirtualize a trait-object member
/// pointer the same way, so this context is the backend-only equivalent: a
/// stable object with raw pointers to the JIT state, callbacks, global monitor,
/// and processor id.
pub struct A32CallbackContext {
    jit_state: *mut A32JitState,
    callbacks: *mut (dyn UserCallbacks + 'static),
    global_monitor: Option<*mut ExclusiveMonitor>,
    processor_id: usize,
    exclusive_value: [u64; 2],
}

impl A32CallbackContext {
    pub fn new(
        jit_state: *mut A32JitState,
        callbacks: *mut (dyn UserCallbacks + 'static),
        global_monitor: Option<*mut ExclusiveMonitor>,
        processor_id: usize,
    ) -> Self {
        Self {
            jit_state,
            callbacks,
            global_monitor,
            processor_id,
            exclusive_value: [0; 2],
        }
    }

    pub fn callback_fns() -> A32CallbackFns {
        A32CallbackFns {
            read_memory_8: a32_arm64_memory_read_8 as *const () as *const c_void,
            read_memory_16: a32_arm64_memory_read_16 as *const () as *const c_void,
            read_memory_32: a32_arm64_memory_read_32 as *const () as *const c_void,
            read_memory_64: a32_arm64_memory_read_64 as *const () as *const c_void,
            exclusive_read_memory_8: a32_arm64_exclusive_read_8 as *const () as *const c_void,
            exclusive_read_memory_16: a32_arm64_exclusive_read_16 as *const () as *const c_void,
            exclusive_read_memory_32: a32_arm64_exclusive_read_32 as *const () as *const c_void,
            exclusive_read_memory_64: a32_arm64_exclusive_read_64 as *const () as *const c_void,
            write_memory_8: a32_arm64_memory_write_8 as *const () as *const c_void,
            write_memory_16: a32_arm64_memory_write_16 as *const () as *const c_void,
            write_memory_32: a32_arm64_memory_write_32 as *const () as *const c_void,
            write_memory_64: a32_arm64_memory_write_64 as *const () as *const c_void,
            exclusive_write_memory_8: a32_arm64_exclusive_write_8 as *const () as *const c_void,
            exclusive_write_memory_16: a32_arm64_exclusive_write_16 as *const () as *const c_void,
            exclusive_write_memory_32: a32_arm64_exclusive_write_32 as *const () as *const c_void,
            exclusive_write_memory_64: a32_arm64_exclusive_write_64 as *const () as *const c_void,
            call_svc: a32_arm64_call_svc as *const () as *const c_void,
            exception_raised: a32_arm64_exception_raised as *const () as *const c_void,
            isb_raised: a32_arm64_isb_raised as *const () as *const c_void,
            add_ticks: a32_arm64_add_ticks as *const () as *const c_void,
            get_ticks_remaining: a32_arm64_get_ticks_remaining as *const () as *const c_void,
            get_cntpct: a32_arm64_get_cntpct as *const () as *const c_void,
        }
    }

    fn callbacks(&self) -> &dyn UserCallbacks {
        unsafe { &*self.callbacks }
    }

    fn callbacks_mut(&mut self) -> &mut dyn UserCallbacks {
        unsafe { &mut *self.callbacks }
    }
}

impl A32ExclusiveState for A32CallbackContext {
    fn exclusive_state(&self) -> u32 {
        unsafe { (*self.jit_state).exclusive_state }
    }

    fn set_exclusive_state(&mut self, value: u32) {
        unsafe {
            (*self.jit_state).exclusive_state = value;
        }
    }

    fn exclusive_value(&self, index: usize) -> u64 {
        self.exclusive_value[index]
    }

    fn set_exclusive_value(&mut self, index: usize, value: u64) {
        self.exclusive_value[index] = value;
    }
}

fn trace_a32_mem_pc(context: &A32CallbackContext, op: &str, vaddr: u64, value: Option<u64>) {
    if std::env::var_os("RUZU_TRACE_A32_MEM_PC").is_none() {
        return;
    }

    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    static COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    static PC_RANGE: std::sync::OnceLock<Option<(u32, u32)>> = std::sync::OnceLock::new();

    let after_ms = std::env::var("RUZU_TRACE_A32_MEM_PC_AFTER_MS")
        .ok()
        .and_then(|raw| raw.parse::<u128>().ok())
        .unwrap_or(0);
    if START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis()
        < after_ms
    {
        return;
    }

    let r = unsafe { &(*context.jit_state).regs };
    if let Some((lo, hi)) = *PC_RANGE.get_or_init(parse_trace_a32_mem_pc_range) {
        let pc = r[15];
        if pc < lo || pc >= hi {
            return;
        }
    }

    let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if n >= 256 {
        return;
    }

    match value {
        Some(value) => eprintln!(
            "[A32_MEM_PC] n={} op={} pc=0x{:08X} lr=0x{:08X} sp=0x{:08X} vaddr=0x{:08X} value=0x{:016X} r0=0x{:08X} r1=0x{:08X} r2=0x{:08X} r3=0x{:08X} r4=0x{:08X} r5=0x{:08X} r6=0x{:08X} r7=0x{:08X}",
            n,
            op,
            r[15],
            r[14],
            r[13],
            vaddr as u32,
            value,
            r[0],
            r[1],
            r[2],
            r[3],
            r[4],
            r[5],
            r[6],
            r[7],
        ),
        None => eprintln!(
            "[A32_MEM_PC] n={} op={} pc=0x{:08X} lr=0x{:08X} sp=0x{:08X} vaddr=0x{:08X} r0=0x{:08X} r1=0x{:08X} r2=0x{:08X} r3=0x{:08X} r4=0x{:08X} r5=0x{:08X} r6=0x{:08X} r7=0x{:08X}",
            n,
            op,
            r[15],
            r[14],
            r[13],
            vaddr as u32,
            r[0],
            r[1],
            r[2],
            r[3],
            r[4],
            r[5],
            r[6],
            r[7],
        ),
    }
}

fn parse_trace_a32_mem_pc_range() -> Option<(u32, u32)> {
    let raw = std::env::var("RUZU_TRACE_A32_MEM_PC_RANGE").ok()?;
    let (lo, hi) = raw.split_once('-')?;
    let parse = |value: &str| -> Option<u32> {
        let value = value.trim();
        let value = value
            .strip_prefix("0x")
            .or_else(|| value.strip_prefix("0X"))
            .unwrap_or(value);
        u32::from_str_radix(value, 16).ok()
    };
    let lo = parse(lo)?;
    let hi = parse(hi)?;
    (lo < hi).then_some((lo, hi))
}

extern "C" fn a32_arm64_memory_read_8(ctx: *mut A32CallbackContext, vaddr: u64) -> u64 {
    let context = unsafe { &mut *ctx };
    trace_a32_mem_pc(context, "r8", vaddr, None);
    a32_callbacks::memory_read_8(context.callbacks(), vaddr)
}

extern "C" fn a32_arm64_memory_read_16(ctx: *mut A32CallbackContext, vaddr: u64) -> u64 {
    let context = unsafe { &mut *ctx };
    trace_a32_mem_pc(context, "r16", vaddr, None);
    a32_callbacks::memory_read_16(context.callbacks(), vaddr)
}

extern "C" fn a32_arm64_memory_read_32(ctx: *mut A32CallbackContext, vaddr: u64) -> u64 {
    let context = unsafe { &mut *ctx };
    trace_a32_mem_pc(context, "r32", vaddr, None);
    a32_callbacks::memory_read_32(context.callbacks(), vaddr)
}

extern "C" fn a32_arm64_memory_read_64(ctx: *mut A32CallbackContext, vaddr: u64) -> u64 {
    let context = unsafe { &mut *ctx };
    trace_a32_mem_pc(context, "r64", vaddr, None);
    a32_callbacks::memory_read_64(context.callbacks(), vaddr)
}

extern "C" fn a32_arm64_memory_write_8(ctx: *mut A32CallbackContext, vaddr: u64, value: u64) {
    let context = unsafe { &mut *ctx };
    trace_a32_mem_pc(context, "w8", vaddr, Some(value));
    a32_callbacks::memory_write_8(context.callbacks_mut(), vaddr, value);
}

extern "C" fn a32_arm64_memory_write_16(ctx: *mut A32CallbackContext, vaddr: u64, value: u64) {
    let context = unsafe { &mut *ctx };
    trace_a32_mem_pc(context, "w16", vaddr, Some(value));
    a32_callbacks::memory_write_16(context.callbacks_mut(), vaddr, value);
}

extern "C" fn a32_arm64_memory_write_32(ctx: *mut A32CallbackContext, vaddr: u64, value: u64) {
    let context = unsafe { &mut *ctx };
    trace_a32_mem_pc(context, "w32", vaddr, Some(value));
    a32_callbacks::memory_write_32(context.callbacks_mut(), vaddr, value);
}

extern "C" fn a32_arm64_memory_write_64(ctx: *mut A32CallbackContext, vaddr: u64, value: u64) {
    let context = unsafe { &mut *ctx };
    trace_a32_mem_pc(context, "w64", vaddr, Some(value));
    a32_callbacks::memory_write_64(context.callbacks_mut(), vaddr, value);
}

extern "C" fn a32_arm64_call_svc(ctx: *mut A32CallbackContext, svc_num: u64) {
    let context = unsafe { &mut *ctx };
    a32_callbacks::call_supervisor(context.callbacks_mut(), svc_num);
}

extern "C" fn a32_arm64_exception_raised(ctx: *mut A32CallbackContext, pc: u64, exception: u64) {
    let context = unsafe { &mut *ctx };
    a32_callbacks::exception_raised(context.callbacks_mut(), pc, exception);
}

extern "C" fn a32_arm64_isb_raised(ctx: *mut A32CallbackContext) {
    let context = unsafe { &mut *ctx };
    context
        .callbacks_mut()
        .instruction_synchronization_barrier_raised();
}

extern "C" fn a32_arm64_add_ticks(ctx: *mut A32CallbackContext, ticks: u64) {
    let context = unsafe { &mut *ctx };
    a32_callbacks::add_ticks(context.callbacks_mut(), ticks);
}

extern "C" fn a32_arm64_get_ticks_remaining(ctx: *mut A32CallbackContext) -> u64 {
    let context = unsafe { &mut *ctx };
    if std::env::var_os("RUZU_TRACE_A32_TICK_PC").is_some() {
        static TRACE_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = TRACE_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if n < 128 {
            let r = unsafe { &(*context.jit_state).regs };
            eprintln!(
                "[A32_TICK_PC] n={} pc=0x{:08X} lr=0x{:08X} sp=0x{:08X} r0=0x{:08X} r1=0x{:08X} r2=0x{:08X} r3=0x{:08X} r4=0x{:08X} r5=0x{:08X} r6=0x{:08X} r7=0x{:08X}",
                n, r[15], r[14], r[13], r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7]
            );
        }
    }
    a32_callbacks::get_ticks_remaining(context.callbacks())
}

extern "C" fn a32_arm64_get_cntpct(ctx: *mut A32CallbackContext) -> u64 {
    let context = unsafe { &mut *ctx };
    a32_callbacks::get_cntpct(context.callbacks())
}

extern "C" fn a32_arm64_exclusive_read_8(ctx: *mut A32CallbackContext, vaddr: u64) -> u64 {
    let context = unsafe { &mut *ctx };
    let global_monitor = context.global_monitor;
    let processor_id = context.processor_id;
    let callbacks = unsafe { &mut *context.callbacks };
    a32_callbacks::exclusive_read_8(context, callbacks, global_monitor, processor_id, vaddr)
}

extern "C" fn a32_arm64_exclusive_read_16(ctx: *mut A32CallbackContext, vaddr: u64) -> u64 {
    let context = unsafe { &mut *ctx };
    let global_monitor = context.global_monitor;
    let processor_id = context.processor_id;
    let callbacks = unsafe { &mut *context.callbacks };
    a32_callbacks::exclusive_read_16(context, callbacks, global_monitor, processor_id, vaddr)
}

extern "C" fn a32_arm64_exclusive_read_32(ctx: *mut A32CallbackContext, vaddr: u64) -> u64 {
    let context = unsafe { &mut *ctx };
    let global_monitor = context.global_monitor;
    let processor_id = context.processor_id;
    let callbacks = unsafe { &mut *context.callbacks };
    a32_callbacks::exclusive_read_32(context, callbacks, global_monitor, processor_id, vaddr)
}

extern "C" fn a32_arm64_exclusive_read_64(ctx: *mut A32CallbackContext, vaddr: u64) -> u64 {
    let context = unsafe { &mut *ctx };
    let global_monitor = context.global_monitor;
    let processor_id = context.processor_id;
    let callbacks = unsafe { &mut *context.callbacks };
    a32_callbacks::exclusive_read_64(context, callbacks, global_monitor, processor_id, vaddr)
}

extern "C" fn a32_arm64_exclusive_write_8(
    ctx: *mut A32CallbackContext,
    vaddr: u64,
    value: u64,
) -> u64 {
    let context = unsafe { &mut *ctx };
    let global_monitor = context.global_monitor;
    let processor_id = context.processor_id;
    if let Some(monitor) = global_monitor {
        let callbacks = context.callbacks_mut();
        return if unsafe {
            (&mut *monitor).do_exclusive_operation(processor_id, vaddr, |expected: u8| {
                callbacks.exclusive_write_8(vaddr, value as u8, expected)
            })
        } {
            0
        } else {
            1
        };
    }
    let expected = context.exclusive_value[0] as u8;
    context
        .callbacks_mut()
        .exclusive_write_8(vaddr, value as u8, expected) as u64
        ^ 1
}

extern "C" fn a32_arm64_exclusive_write_16(
    ctx: *mut A32CallbackContext,
    vaddr: u64,
    value: u64,
) -> u64 {
    let context = unsafe { &mut *ctx };
    let global_monitor = context.global_monitor;
    let processor_id = context.processor_id;
    if let Some(monitor) = global_monitor {
        let callbacks = context.callbacks_mut();
        return if unsafe {
            (&mut *monitor).do_exclusive_operation(processor_id, vaddr, |expected: u16| {
                callbacks.exclusive_write_16(vaddr, value as u16, expected)
            })
        } {
            0
        } else {
            1
        };
    }
    let expected = context.exclusive_value[0] as u16;
    context
        .callbacks_mut()
        .exclusive_write_16(vaddr, value as u16, expected) as u64
        ^ 1
}

extern "C" fn a32_arm64_exclusive_write_32(
    ctx: *mut A32CallbackContext,
    vaddr: u64,
    value: u64,
) -> u64 {
    let context = unsafe { &mut *ctx };
    let global_monitor = context.global_monitor;
    let processor_id = context.processor_id;
    if let Some(monitor) = global_monitor {
        let callbacks = context.callbacks_mut();
        return if unsafe {
            (&mut *monitor).do_exclusive_operation(processor_id, vaddr, |expected: u32| {
                callbacks.exclusive_write_32(vaddr, value as u32, expected)
            })
        } {
            0
        } else {
            1
        };
    }
    let expected = context.exclusive_value[0] as u32;
    context
        .callbacks_mut()
        .exclusive_write_32(vaddr, value as u32, expected) as u64
        ^ 1
}

extern "C" fn a32_arm64_exclusive_write_64(
    ctx: *mut A32CallbackContext,
    vaddr: u64,
    value: u64,
) -> u64 {
    let context = unsafe { &mut *ctx };
    let global_monitor = context.global_monitor;
    let processor_id = context.processor_id;
    if let Some(monitor) = global_monitor {
        let callbacks = context.callbacks_mut();
        return if unsafe {
            (&mut *monitor).do_exclusive_operation(processor_id, vaddr, |expected: u64| {
                callbacks.exclusive_write_64(vaddr, value, expected)
            })
        } {
            0
        } else {
            1
        };
    }
    let expected = context.exclusive_value[0];
    context
        .callbacks_mut()
        .exclusive_write_64(vaddr, value, expected) as u64
        ^ 1
}

impl A32AddressSpace {
    pub fn new(conf: JitConfig) -> Result<Self, String> {
        let mut address_space = Self::new_without_prelude(conf)?;
        emit_prelude(&mut address_space)?;
        Ok(address_space)
    }

    pub(crate) fn new_without_prelude(conf: JitConfig) -> Result<Self, String> {
        let code_cache_size = if conf.code_cache_size == 0 {
            crate::jit_config::JitConfig::DEFAULT_CODE_CACHE_SIZE
        } else {
            conf.code_cache_size
        };

        let address_space = AddressSpace::new(code_cache_size)?;

        Ok(Self {
            address_space,
            conf,
            block_ranges: Vec::new(),
            cp15_uprw: Box::new(0),
            cp15_uro: Box::new(0),
            dispatcher_entries: 0,
            dispatcher_cache_hits: 0,
            dispatcher_compiles: 0,
            terminal_link_block: 0,
            terminal_link_block_fast: 0,
            terminal_pop_rsb_hint: 0,
            terminal_fast_dispatch_hint: 0,
            terminal_return_to_dispatch: 0,
            terminal_other: 0,
            compiled_live_insts: 0,
            profile_code_cache: arm64_code_cache_profile_enabled(),
        })
    }

    pub(crate) fn emit_prelude_with_dispatcher(
        &mut self,
        callback_context_ptr: *const c_void,
        fns: A32CallbackFns,
    ) -> Result<(), String> {
        let ticks = self
            .config()
            .enable_cycle_counting
            .then_some(TickCallbacks {
                this_ptr: callback_context_ptr,
                add_ticks_fn_ptr: fns.add_ticks,
                get_ticks_remaining_fn_ptr: fns.get_ticks_remaining,
            });
        let dispatcher = DispatcherCallback {
            this_ptr: (self as *mut A32AddressSpace).cast::<c_void>(),
            fn_ptr: a32_return_to_dispatcher as *const () as *const c_void,
            ticks,
        };
        self.address_space
            .emit_bootstrap_prelude_with_options(PreludeOptions {
                isa: PreludeIsa::A32,
                dispatcher: Some(dispatcher),
                return_stack_buffer: self
                    .config()
                    .has_optimization(OptimizationFlag::RETURN_STACK_BUFFER),
                page_table_pointer: self.config().page_table_pointer.map_or(0, |p| p as u64),
                fastmem_pointer: self.config().fastmem_pointer.map_or(0, |p| p as u64),
            })
    }

    pub fn address_space(&self) -> &AddressSpace {
        &self.address_space
    }

    pub fn config(&self) -> &JitConfig {
        &self.conf
    }

    pub(crate) fn config_mut(&mut self) -> &mut JitConfig {
        &mut self.conf
    }

    pub(crate) fn address_space_mut(&mut self) -> &mut AddressSpace {
        &mut self.address_space
    }

    /// Diagnostic passthrough for host-profile attribution.
    pub fn dump_block_map(&self, out: &mut dyn std::io::Write) -> std::io::Result<()> {
        self.address_space.dump_block_map(out)
    }

    pub fn get_or_emit(&mut self, descriptor: LocationDescriptor) -> Result<CodePtr, String> {
        if let Some((lo, hi)) = crate::jit::block_count_range() {
            let pc = A32LocationDescriptor::from_location(descriptor).pc();
            if pc >= lo && pc < hi {
                let idx = self.conf.processor_id.min(15);
                crate::jit::block_count_counters()[idx]
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }

        let profile = self.profile_code_cache;
        if profile {
            self.dispatcher_entries = self.dispatcher_entries.saturating_add(1);
        }
        if let Some(block_entry) = self.address_space.get(descriptor) {
            if profile {
                self.dispatcher_cache_hits = self.dispatcher_cache_hits.saturating_add(1);
                self.maybe_log_dispatcher_profile(descriptor, false);
            }
            return Ok(block_entry);
        }

        if profile {
            self.dispatcher_compiles = self.dispatcher_compiles.saturating_add(1);
        }
        log_a32_fpscr_mode_if_requested(descriptor);
        let block = self.generate_ir(descriptor);
        let block_for_ranges = block.clone();
        if profile {
            self.record_block_profile(&block_for_ranges);
        }
        let config = self.get_emit_config();
        let block_info = self.address_space.emit(block, config)?;
        dump_arm64_block_if_requested(&block_for_ranges, &block_info);
        self.register_new_basic_block(&block_for_ranges);
        if profile {
            self.maybe_log_dispatcher_profile(descriptor, true);
        }
        Ok(block_info.entry_point)
    }

    fn record_block_profile(&mut self, block: &Block) {
        self.compiled_live_insts = self
            .compiled_live_insts
            .saturating_add(block.live_inst_count() as u64);
        match classify_terminal(&block.terminal) {
            TerminalKind::LinkBlock => {
                self.terminal_link_block = self.terminal_link_block.saturating_add(1);
            }
            TerminalKind::LinkBlockFast => {
                self.terminal_link_block_fast = self.terminal_link_block_fast.saturating_add(1);
            }
            TerminalKind::PopRsbHint => {
                self.terminal_pop_rsb_hint = self.terminal_pop_rsb_hint.saturating_add(1);
            }
            TerminalKind::FastDispatchHint => {
                self.terminal_fast_dispatch_hint =
                    self.terminal_fast_dispatch_hint.saturating_add(1);
            }
            TerminalKind::ReturnToDispatch => {
                self.terminal_return_to_dispatch =
                    self.terminal_return_to_dispatch.saturating_add(1);
            }
            TerminalKind::Other => {
                self.terminal_other = self.terminal_other.saturating_add(1);
            }
        }
    }

    fn maybe_log_dispatcher_profile(&self, descriptor: LocationDescriptor, compiled: bool) {
        if !self.profile_code_cache {
            return;
        }

        let count = self.dispatcher_entries;
        let compile_checkpoint = compiled
            && (self.dispatcher_compiles <= 16 || self.dispatcher_compiles.is_power_of_two());
        if count <= 16 || count.is_power_of_two() || compile_checkpoint {
            log::info!(
                "[A32_ARM64_DISPATCH] entries={} hits={} compiles={} hit_rate={:.2}% compiled={} descriptor={} terminals(link={} fast_link={} rsb={} fast_dispatch={} return_dispatch={} other={}) avg_live_inst={:.2}",
                self.dispatcher_entries,
                self.dispatcher_cache_hits,
                self.dispatcher_compiles,
                if self.dispatcher_entries == 0 {
                    0.0
                } else {
                    (self.dispatcher_cache_hits as f64 * 100.0) / self.dispatcher_entries as f64
                },
                compiled,
                descriptor,
                self.terminal_link_block,
                self.terminal_link_block_fast,
                self.terminal_pop_rsb_hint,
                self.terminal_fast_dispatch_hint,
                self.terminal_return_to_dispatch,
                self.terminal_other,
                if self.dispatcher_compiles == 0 {
                    0.0
                } else {
                    self.compiled_live_insts as f64 / self.dispatcher_compiles as f64
                },
            );
        }
    }

    pub fn generate_ir(&self, descriptor: LocationDescriptor) -> Block {
        let a32_descriptor = A32LocationDescriptor::from_location(descriptor);
        let read_code = |vaddr: u32| self.conf.callbacks.memory_read_code(vaddr as u64);
        let mut block = translate(a32_descriptor, &read_code);
        dump_a32_ir_if_requested("pre-opt", &block);

        if self
            .conf
            .has_optimization(OptimizationFlag::GET_SET_ELIMINATION)
        {
            opt::a32_get_set_elimination(&mut block);
            block.recompute_use_counts();
            opt::dead_code_elimination(&mut block);
        }
        if self.conf.has_optimization(OptimizationFlag::CONST_PROP) {
            let is_read_only = |vaddr: u32| self.conf.callbacks.is_read_only_memory(vaddr);
            opt::a32_constant_memory_reads(&mut block, &read_code, &is_read_only);
            opt::constant_propagation(&mut block);
            block.recompute_use_counts();
            opt::dead_code_elimination(&mut block);
        }
        opt::identity_removal(&mut block);
        block.recompute_use_counts();
        block.rebuild_pseudo_op_links();
        #[cfg(debug_assertions)]
        opt::verification_pass(&block);
        dump_a32_ir_if_requested("post-opt", &block);

        block
    }

    pub fn get_emit_config(&self) -> EmitConfig {
        let mut config = EmitConfig::from_a32_config(&self.conf);
        config.a32_cp15_uprw = self.cp15_uprw.as_ref() as *const u32 as *mut u32;
        config.a32_cp15_uro = self.cp15_uro.as_ref() as *const u32 as *mut u32;
        config
    }

    pub fn cp15_uprw(&self) -> u32 {
        *self.cp15_uprw
    }

    pub fn set_cp15_uprw(&mut self, value: u32) {
        *self.cp15_uprw = value;
    }

    pub fn cp15_uro(&self) -> u32 {
        *self.cp15_uro
    }

    pub fn set_cp15_uro(&mut self, value: u32) {
        *self.cp15_uro = value;
    }

    pub fn emit_normal_callback_trampolines(
        &mut self,
        this_ptr: *const c_void,
        fns: A32NormalCallbackFns,
    ) -> Result<(), String> {
        let read_memory_8 = self
            .address_space
            .emit_call_trampoline(this_ptr, fns.read_memory_8)?;
        let read_memory_16 = self
            .address_space
            .emit_call_trampoline(this_ptr, fns.read_memory_16)?;
        let read_memory_32 = self
            .address_space
            .emit_call_trampoline(this_ptr, fns.read_memory_32)?;
        let read_memory_64 = self
            .address_space
            .emit_call_trampoline(this_ptr, fns.read_memory_64)?;
        let write_memory_8 = self
            .address_space
            .emit_call_trampoline(this_ptr, fns.write_memory_8)?;
        let write_memory_16 = self
            .address_space
            .emit_call_trampoline(this_ptr, fns.write_memory_16)?;
        let write_memory_32 = self
            .address_space
            .emit_call_trampoline(this_ptr, fns.write_memory_32)?;
        let write_memory_64 = self
            .address_space
            .emit_call_trampoline(this_ptr, fns.write_memory_64)?;
        let call_svc = self
            .address_space
            .emit_call_trampoline(this_ptr, fns.call_svc)?;
        let exception_raised = self
            .address_space
            .emit_call_trampoline(this_ptr, fns.exception_raised)?;
        let isb_raised = self
            .address_space
            .emit_call_trampoline(this_ptr, fns.isb_raised)?;
        let add_ticks = self
            .address_space
            .emit_call_trampoline(this_ptr, fns.add_ticks)?;
        let get_ticks_remaining = self
            .address_space
            .emit_call_trampoline(this_ptr, fns.get_ticks_remaining)?;
        let get_cntpct = self
            .address_space
            .emit_call_trampoline(this_ptr, fns.get_cntpct)?;

        let prelude_info = self.address_space.prelude_info_mut();
        prelude_info.read_memory_8 = Some(read_memory_8);
        prelude_info.read_memory_16 = Some(read_memory_16);
        prelude_info.read_memory_32 = Some(read_memory_32);
        prelude_info.read_memory_64 = Some(read_memory_64);
        prelude_info.write_memory_8 = Some(write_memory_8);
        prelude_info.write_memory_16 = Some(write_memory_16);
        prelude_info.write_memory_32 = Some(write_memory_32);
        prelude_info.write_memory_64 = Some(write_memory_64);
        prelude_info.call_svc = Some(call_svc);
        prelude_info.exception_raised = Some(exception_raised);
        prelude_info.isb_raised = Some(isb_raised);
        prelude_info.add_ticks = Some(add_ticks);
        prelude_info.get_ticks_remaining = Some(get_ticks_remaining);
        prelude_info.get_cntpct = Some(get_cntpct);
        Ok(())
    }

    pub fn emit_callback_trampolines(
        &mut self,
        callbacks_this_ptr: *const c_void,
        exclusive_context_ptr: *const c_void,
        fns: A32CallbackFns,
    ) -> Result<(), String> {
        let read_memory_8 = self
            .address_space
            .emit_call_trampoline(callbacks_this_ptr, fns.read_memory_8)?;
        let read_memory_16 = self
            .address_space
            .emit_call_trampoline(callbacks_this_ptr, fns.read_memory_16)?;
        let read_memory_32 = self
            .address_space
            .emit_call_trampoline(callbacks_this_ptr, fns.read_memory_32)?;
        let read_memory_64 = self
            .address_space
            .emit_call_trampoline(callbacks_this_ptr, fns.read_memory_64)?;

        let wrapped_read_memory_8 = self
            .address_space
            .emit_wrapped_read_call_trampoline(callbacks_this_ptr, fns.read_memory_8)?;
        let wrapped_read_memory_16 = self
            .address_space
            .emit_wrapped_read_call_trampoline(callbacks_this_ptr, fns.read_memory_16)?;
        let wrapped_read_memory_32 = self
            .address_space
            .emit_wrapped_read_call_trampoline(callbacks_this_ptr, fns.read_memory_32)?;
        let wrapped_read_memory_64 = self
            .address_space
            .emit_wrapped_read_call_trampoline(callbacks_this_ptr, fns.read_memory_64)?;

        let exclusive_read_memory_8 = self
            .address_space
            .emit_call_trampoline(exclusive_context_ptr, fns.exclusive_read_memory_8)?;
        let exclusive_read_memory_16 = self
            .address_space
            .emit_call_trampoline(exclusive_context_ptr, fns.exclusive_read_memory_16)?;
        let exclusive_read_memory_32 = self
            .address_space
            .emit_call_trampoline(exclusive_context_ptr, fns.exclusive_read_memory_32)?;
        let exclusive_read_memory_64 = self
            .address_space
            .emit_call_trampoline(exclusive_context_ptr, fns.exclusive_read_memory_64)?;

        let write_memory_8 = self
            .address_space
            .emit_call_trampoline(callbacks_this_ptr, fns.write_memory_8)?;
        let write_memory_16 = self
            .address_space
            .emit_call_trampoline(callbacks_this_ptr, fns.write_memory_16)?;
        let write_memory_32 = self
            .address_space
            .emit_call_trampoline(callbacks_this_ptr, fns.write_memory_32)?;
        let write_memory_64 = self
            .address_space
            .emit_call_trampoline(callbacks_this_ptr, fns.write_memory_64)?;

        let wrapped_write_memory_8 = self
            .address_space
            .emit_wrapped_write_call_trampoline(callbacks_this_ptr, fns.write_memory_8)?;
        let wrapped_write_memory_16 = self
            .address_space
            .emit_wrapped_write_call_trampoline(callbacks_this_ptr, fns.write_memory_16)?;
        let wrapped_write_memory_32 = self
            .address_space
            .emit_wrapped_write_call_trampoline(callbacks_this_ptr, fns.write_memory_32)?;
        let wrapped_write_memory_64 = self
            .address_space
            .emit_wrapped_write_call_trampoline(callbacks_this_ptr, fns.write_memory_64)?;

        let exclusive_write_memory_8 = self
            .address_space
            .emit_call_trampoline(exclusive_context_ptr, fns.exclusive_write_memory_8)?;
        let exclusive_write_memory_16 = self
            .address_space
            .emit_call_trampoline(exclusive_context_ptr, fns.exclusive_write_memory_16)?;
        let exclusive_write_memory_32 = self
            .address_space
            .emit_call_trampoline(exclusive_context_ptr, fns.exclusive_write_memory_32)?;
        let exclusive_write_memory_64 = self
            .address_space
            .emit_call_trampoline(exclusive_context_ptr, fns.exclusive_write_memory_64)?;

        let call_svc = self
            .address_space
            .emit_call_trampoline(callbacks_this_ptr, fns.call_svc)?;
        let exception_raised = self
            .address_space
            .emit_call_trampoline(callbacks_this_ptr, fns.exception_raised)?;
        let isb_raised = self
            .address_space
            .emit_call_trampoline(callbacks_this_ptr, fns.isb_raised)?;
        let add_ticks = self
            .address_space
            .emit_call_trampoline(callbacks_this_ptr, fns.add_ticks)?;
        let get_ticks_remaining = self
            .address_space
            .emit_call_trampoline(callbacks_this_ptr, fns.get_ticks_remaining)?;
        let get_cntpct = self
            .address_space
            .emit_call_trampoline(callbacks_this_ptr, fns.get_cntpct)?;

        let prelude_info = self.address_space.prelude_info_mut();
        prelude_info.read_memory_8 = Some(read_memory_8);
        prelude_info.read_memory_16 = Some(read_memory_16);
        prelude_info.read_memory_32 = Some(read_memory_32);
        prelude_info.read_memory_64 = Some(read_memory_64);
        prelude_info.wrapped_read_memory_8 = Some(wrapped_read_memory_8);
        prelude_info.wrapped_read_memory_16 = Some(wrapped_read_memory_16);
        prelude_info.wrapped_read_memory_32 = Some(wrapped_read_memory_32);
        prelude_info.wrapped_read_memory_64 = Some(wrapped_read_memory_64);
        prelude_info.exclusive_read_memory_8 = Some(exclusive_read_memory_8);
        prelude_info.exclusive_read_memory_16 = Some(exclusive_read_memory_16);
        prelude_info.exclusive_read_memory_32 = Some(exclusive_read_memory_32);
        prelude_info.exclusive_read_memory_64 = Some(exclusive_read_memory_64);
        prelude_info.write_memory_8 = Some(write_memory_8);
        prelude_info.write_memory_16 = Some(write_memory_16);
        prelude_info.write_memory_32 = Some(write_memory_32);
        prelude_info.write_memory_64 = Some(write_memory_64);
        prelude_info.wrapped_write_memory_8 = Some(wrapped_write_memory_8);
        prelude_info.wrapped_write_memory_16 = Some(wrapped_write_memory_16);
        prelude_info.wrapped_write_memory_32 = Some(wrapped_write_memory_32);
        prelude_info.wrapped_write_memory_64 = Some(wrapped_write_memory_64);
        prelude_info.exclusive_write_memory_8 = Some(exclusive_write_memory_8);
        prelude_info.exclusive_write_memory_16 = Some(exclusive_write_memory_16);
        prelude_info.exclusive_write_memory_32 = Some(exclusive_write_memory_32);
        prelude_info.exclusive_write_memory_64 = Some(exclusive_write_memory_64);
        prelude_info.call_svc = Some(call_svc);
        prelude_info.exception_raised = Some(exception_raised);
        prelude_info.isb_raised = Some(isb_raised);
        prelude_info.add_ticks = Some(add_ticks);
        prelude_info.get_ticks_remaining = Some(get_ticks_remaining);
        prelude_info.get_cntpct = Some(get_cntpct);
        Ok(())
    }

    pub fn register_new_basic_block(&mut self, block: &Block) {
        let descriptor = A32LocationDescriptor::from_location(block.location);
        let end_location = A32LocationDescriptor::from_location(block.end_location());
        self.block_ranges.push(BlockRange32 {
            start: descriptor.pc(),
            end: end_location.pc().wrapping_sub(1),
            descriptor: block.location,
        });
    }

    pub fn invalidate_cache_ranges(&mut self, ranges: &[RangeInclusive<u32>]) {
        let mut descriptors = HashSet::new();
        self.block_ranges.retain(|block_range| {
            let overlaps = ranges
                .iter()
                .any(|range| ranges_overlap_u32(block_range.start, block_range.end, range));
            if overlaps {
                descriptors.insert(block_range.descriptor);
            }
            !overlaps
        });
        self.address_space.invalidate_basic_blocks(&descriptors);
    }

    pub fn block_ranges(&self) -> &[BlockRange32] {
        &self.block_ranges
    }
}

/// Diagnostic: with `RUZU_LOG_A32_FPSCR_MODES` set, log each distinct FPSCR
/// mode (upper-16 location-descriptor bits) seen at block-compile time, with
/// the first PC that compiled under it.
fn log_a32_fpscr_mode_if_requested(descriptor: LocationDescriptor) {
    static SEEN: std::sync::OnceLock<Option<std::sync::Mutex<std::collections::BTreeSet<u32>>>> =
        std::sync::OnceLock::new();
    let Some(seen) = SEEN.get_or_init(|| {
        std::env::var_os("RUZU_LOG_A32_FPSCR_MODES")
            .map(|_| std::sync::Mutex::new(std::collections::BTreeSet::new()))
    }) else {
        return;
    };
    let a32 = A32LocationDescriptor::from_location(descriptor);
    let mode = a32.fpscr().value();
    if seen.lock().unwrap().insert(mode) {
        eprintln!(
            "[A32_FPSCR_MODES] new mode=0x{mode:08X} first_pc=0x{:08X}",
            a32.pc()
        );
    }
}

fn dump_arm64_block_if_requested(block: &Block, block_info: &super::emit_arm64::EmittedBlockInfo) {
    let Some((lo, hi)) = dump_arm64_block_range() else {
        return;
    };
    let pc = A32LocationDescriptor::from_location(block.location).pc();
    if pc < lo || pc >= hi {
        return;
    }

    let dir = std::env::var_os("RUZU_DUMP_ARM64_BLOCK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/ruzu-arm64-blocks"));
    if let Err(err) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "[A32_ARM64_BLOCK_DUMP] failed to create {}: {err}",
            dir.display()
        );
        return;
    }

    let path = dir.join(format!(
        "a32_{pc:08X}_host_{:016X}.bin",
        block_info.entry_point as usize
    ));
    let bytes = unsafe { std::slice::from_raw_parts(block_info.entry_point, block_info.size) };
    match std::fs::write(&path, bytes) {
        Ok(()) => eprintln!(
            "[A32_ARM64_BLOCK_DUMP] pc=0x{pc:08X} host=0x{:016X} size={} path={}",
            block_info.entry_point as usize,
            block_info.size,
            path.display()
        ),
        Err(err) => eprintln!(
            "[A32_ARM64_BLOCK_DUMP] failed to write pc=0x{pc:08X} path={}: {err}",
            path.display()
        ),
    }
}

fn dump_arm64_block_range() -> Option<(u32, u32)> {
    static RANGE: std::sync::OnceLock<Option<(u32, u32)>> = std::sync::OnceLock::new();
    *RANGE.get_or_init(|| {
        let raw = std::env::var("RUZU_DUMP_ARM64_BLOCK_PC").ok()?;
        let (lo, hi) = raw.split_once('-')?;
        let parse = |value: &str| -> Option<u32> {
            let value = value.trim();
            let value = value
                .strip_prefix("0x")
                .or_else(|| value.strip_prefix("0X"))
                .unwrap_or(value);
            u32::from_str_radix(value, 16).ok()
        };
        let lo = parse(lo)?;
        let hi = parse(hi)?;
        (lo < hi).then_some((lo, hi))
    })
}

fn dump_a32_ir_if_requested(stage: &str, block: &Block) {
    let pc = A32LocationDescriptor::from_location(block.location).pc();
    if !dump_a32_ir_pcs().contains(&pc) {
        return;
    }

    eprintln!("=== A32 IR dump ({stage}) for block at PC=0x{pc:08X} ===");
    eprintln!("{block}");
}

fn dump_a32_ir_pcs() -> &'static [u32] {
    static PCS: std::sync::OnceLock<Vec<u32>> = std::sync::OnceLock::new();
    PCS.get_or_init(|| {
        std::env::var("RUZU_DUMP_A32_IR_AT_PC")
            .ok()
            .map(|raw| {
                raw.split(',')
                    .filter_map(|part| {
                        let part = part.trim();
                        let part = part
                            .strip_prefix("0x")
                            .or_else(|| part.strip_prefix("0X"))
                            .unwrap_or(part);
                        u32::from_str_radix(part, 16).ok()
                    })
                    .collect()
            })
            .unwrap_or_default()
    })
}

extern "C" fn a32_return_to_dispatcher(
    address_space: *mut c_void,
    thread_ctx: *mut c_void,
) -> CodePtr {
    let result = unsafe {
        let address_space = &mut *(address_space.cast::<A32AddressSpace>());
        let thread_ctx = &mut *(thread_ctx.cast::<A32JitState>());
        address_space.get_or_emit(thread_ctx.get_location_descriptor())
    };

    match result {
        Ok(code_ptr) => code_ptr,
        Err(error) => {
            eprintln!("A32 ARM64 return_to_dispatcher failed: {error}");
            std::process::abort();
        }
    }
}

fn emit_prelude(address_space: &mut A32AddressSpace) -> Result<(), String> {
    address_space.address_space.emit_bootstrap_prelude()
}

fn ranges_overlap_u32(start: u32, end: u32, range: &RangeInclusive<u32>) -> bool {
    start <= *range.end() && *range.start() <= end
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::common::emit_context::MemoryEmitConfig;
    use crate::frontend::a32::fpscr::FPSCR;
    use crate::frontend::a32::psr::PSR;
    use crate::jit_config::UserCallbacks;

    struct TestCallbacks {
        code: Vec<u32>,
        memory: [u8; 64],
    }

    impl UserCallbacks for TestCallbacks {
        fn memory_read_code(&self, vaddr: u64) -> Option<u32> {
            let index = (vaddr / 4) as usize;
            self.code.get(index).copied()
        }

        fn memory_read_8(&self, _vaddr: u64) -> u8 {
            self.memory[_vaddr as usize]
        }

        fn memory_read_16(&self, _vaddr: u64) -> u16 {
            let offset = _vaddr as usize;
            u16::from_le_bytes(self.memory[offset..offset + 2].try_into().unwrap())
        }

        fn memory_read_32(&self, _vaddr: u64) -> u32 {
            let offset = _vaddr as usize;
            u32::from_le_bytes(self.memory[offset..offset + 4].try_into().unwrap())
        }

        fn memory_read_64(&self, _vaddr: u64) -> u64 {
            let offset = _vaddr as usize;
            u64::from_le_bytes(self.memory[offset..offset + 8].try_into().unwrap())
        }

        fn memory_read_128(&self, _vaddr: u64) -> (u64, u64) {
            (self.memory_read_64(_vaddr), self.memory_read_64(_vaddr + 8))
        }

        fn memory_write_8(&mut self, _vaddr: u64, _value: u8) {
            self.memory[_vaddr as usize] = _value;
        }

        fn memory_write_16(&mut self, _vaddr: u64, _value: u16) {
            let offset = _vaddr as usize;
            self.memory[offset..offset + 2].copy_from_slice(&_value.to_le_bytes());
        }

        fn memory_write_32(&mut self, _vaddr: u64, _value: u32) {
            let offset = _vaddr as usize;
            self.memory[offset..offset + 4].copy_from_slice(&_value.to_le_bytes());
        }

        fn memory_write_64(&mut self, _vaddr: u64, _value: u64) {
            let offset = _vaddr as usize;
            self.memory[offset..offset + 8].copy_from_slice(&_value.to_le_bytes());
        }

        fn memory_write_128(&mut self, _vaddr: u64, _value_lo: u64, _value_hi: u64) {
            self.memory_write_64(_vaddr, _value_lo);
            self.memory_write_64(_vaddr + 8, _value_hi);
        }

        fn exclusive_read_8(&self, _vaddr: u64) -> u8 {
            self.memory_read_8(_vaddr)
        }

        fn exclusive_read_16(&self, _vaddr: u64) -> u16 {
            self.memory_read_16(_vaddr)
        }

        fn exclusive_read_32(&self, _vaddr: u64) -> u32 {
            self.memory_read_32(_vaddr)
        }

        fn exclusive_read_64(&self, _vaddr: u64) -> u64 {
            self.memory_read_64(_vaddr)
        }

        fn exclusive_read_128(&self, _vaddr: u64) -> (u64, u64) {
            self.memory_read_128(_vaddr)
        }

        fn exclusive_write_8(&mut self, _vaddr: u64, _value: u8, _expected: u8) -> bool {
            self.memory_write_8(_vaddr, _value);
            true
        }

        fn exclusive_write_16(&mut self, _vaddr: u64, _value: u16, _expected: u16) -> bool {
            self.memory_write_16(_vaddr, _value);
            true
        }

        fn exclusive_write_32(&mut self, _vaddr: u64, _value: u32, _expected: u32) -> bool {
            self.memory_write_32(_vaddr, _value);
            true
        }

        fn exclusive_write_64(&mut self, _vaddr: u64, _value: u64, _expected: u64) -> bool {
            self.memory_write_64(_vaddr, _value);
            true
        }

        fn exclusive_write_128(
            &mut self,
            _vaddr: u64,
            _value_lo: u64,
            _value_hi: u64,
            _expected_lo: u64,
            _expected_hi: u64,
        ) -> bool {
            self.memory_write_128(_vaddr, _value_lo, _value_hi);
            true
        }

        fn exclusive_clear(&mut self) {}
        fn call_supervisor(&mut self, _svc_num: u32) {}
        fn exception_raised(&mut self, _pc: u64, _exception: u64) {}
        fn add_ticks(&mut self, _ticks: u64) {}

        fn get_ticks_remaining(&self) -> u64 {
            0
        }
    }

    fn config(code: Vec<u32>) -> JitConfig {
        JitConfig {
            callbacks: Box::new(TestCallbacks {
                code,
                memory: [0; 64],
            }),
            enable_cycle_counting: false,
            code_cache_size: 4096,
            optimizations: OptimizationFlag::NO_OPTIMIZATIONS,
            unsafe_optimizations: false,
            global_monitor: None,
            fastmem_pointer: None,
            page_table_pointer: None,
            define_unpredictable_behaviour: false,
            processor_id: 0,
            wall_clock_cntpct: false,
            cntfrq_el0: 600_000_000,
            tpidrro_el0: None,
            tpidr_el0: None,
            memory: MemoryEmitConfig::default(),
        }
    }

    extern "C" fn dummy_callback() {}

    fn dummy_callback_fns() -> A32NormalCallbackFns {
        let ptr = dummy_callback as *const () as *const c_void;
        A32NormalCallbackFns {
            read_memory_8: ptr,
            read_memory_16: ptr,
            read_memory_32: ptr,
            read_memory_64: ptr,
            write_memory_8: ptr,
            write_memory_16: ptr,
            write_memory_32: ptr,
            write_memory_64: ptr,
            call_svc: ptr,
            exception_raised: ptr,
            isb_raised: ptr,
            add_ticks: ptr,
            get_ticks_remaining: ptr,
            get_cntpct: ptr,
        }
    }

    fn dummy_full_callback_fns() -> A32CallbackFns {
        let ptr = dummy_callback as *const () as *const c_void;
        A32CallbackFns {
            read_memory_8: ptr,
            read_memory_16: ptr,
            read_memory_32: ptr,
            read_memory_64: ptr,
            exclusive_read_memory_8: ptr,
            exclusive_read_memory_16: ptr,
            exclusive_read_memory_32: ptr,
            exclusive_read_memory_64: ptr,
            write_memory_8: ptr,
            write_memory_16: ptr,
            write_memory_32: ptr,
            write_memory_64: ptr,
            exclusive_write_memory_8: ptr,
            exclusive_write_memory_16: ptr,
            exclusive_write_memory_32: ptr,
            exclusive_write_memory_64: ptr,
            call_svc: ptr,
            exception_raised: ptr,
            isb_raised: ptr,
            add_ticks: ptr,
            get_ticks_remaining: ptr,
            get_cntpct: ptr,
        }
    }

    #[test]
    fn generate_ir_uses_a32_location_descriptor_and_callbacks() {
        let address_space = A32AddressSpace::new(config(vec![0xe1a0_0000])).unwrap();
        let descriptor =
            A32LocationDescriptor::new(0, PSR::default(), FPSCR::default(), true).to_location();

        let block = address_space.generate_ir(descriptor);

        assert_eq!(block.location, descriptor);
        assert_eq!(
            A32LocationDescriptor::from_location(block.end_location()).pc(),
            4
        );
    }

    #[test]
    fn register_and_invalidate_cache_ranges_track_a32_pc_ranges() {
        let mut address_space = A32AddressSpace::new(config(vec![0xe1a0_0000])).unwrap();
        let descriptor = A32LocationDescriptor::at(0)
            .set_single_stepping(true)
            .to_location();
        let block = address_space.generate_ir(descriptor);

        address_space.register_new_basic_block(&block);
        assert_eq!(address_space.block_ranges().len(), 1);
        assert_eq!(address_space.block_ranges()[0].start, 0);
        assert_eq!(address_space.block_ranges()[0].end, 3);

        address_space.invalidate_cache_ranges(&[2..=2]);
        assert!(address_space.block_ranges().is_empty());
    }

    #[test]
    fn normal_callback_trampolines_populate_prelude_and_extend_cache_base() {
        let mut address_space = A32AddressSpace::new(config(vec![0xe1a0_0000])).unwrap();
        let bootstrap_end = address_space.address_space().prelude_info().end_of_prelude;
        let this_ptr = 0x1234usize as *const c_void;

        address_space
            .emit_normal_callback_trampolines(this_ptr, dummy_callback_fns())
            .unwrap();

        let prelude = address_space.address_space().prelude_info();
        assert_eq!(bootstrap_end, 352);
        assert!(prelude.end_of_prelude > bootstrap_end);
        assert_eq!(prelude.end_of_prelude, 800);
        assert!(prelude.read_memory_8.is_some());
        assert!(prelude.write_memory_64.is_some());
        assert!(prelude.call_svc.is_some());
        assert!(prelude.get_ticks_remaining.is_some());
        assert!(prelude.get_cntpct.is_some());

        let end_of_prelude = prelude.end_of_prelude;
        address_space.address_space.clear_cache().unwrap();
        assert_eq!(
            address_space.address_space().code().code_size(),
            end_of_prelude
        );
    }

    #[test]
    fn callback_trampolines_populate_full_upstream_a32_prelude_subset() {
        let mut address_space = A32AddressSpace::new(config(vec![0xe1a0_0000])).unwrap();
        let callbacks_this_ptr = 0x1234usize as *const c_void;
        let exclusive_context_ptr = 0x5678usize as *const c_void;

        address_space
            .emit_callback_trampolines(
                callbacks_this_ptr,
                exclusive_context_ptr,
                dummy_full_callback_fns(),
            )
            .unwrap();

        let prelude = address_space.address_space().prelude_info();
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

        assert!(prelude.read_memory_128.is_none());
        assert!(prelude.wrapped_read_memory_128.is_none());
        assert!(prelude.exclusive_read_memory_128.is_none());
        assert!(prelude.write_memory_128.is_none());
        assert!(prelude.wrapped_write_memory_128.is_none());
        assert!(prelude.exclusive_write_memory_128.is_none());

        let end_of_prelude = prelude.end_of_prelude;
        assert!(end_of_prelude > 720);
        address_space.address_space.clear_cache().unwrap();
        assert_eq!(
            address_space.address_space().code().code_size(),
            end_of_prelude
        );
    }

    #[test]
    fn callback_context_thunks_forward_memory_and_exclusive_state() {
        type ReadFn = extern "C" fn(*mut A32CallbackContext, u64) -> u64;
        type WriteFn = extern "C" fn(*mut A32CallbackContext, u64, u64);
        type ExclusiveWriteFn = extern "C" fn(*mut A32CallbackContext, u64, u64) -> u64;

        let mut state = A32JitState::new();
        let mut callbacks = TestCallbacks {
            code: vec![],
            memory: [0; 64],
        };
        callbacks.memory_write_32(4, 0x1122_3344);
        let mut context = A32CallbackContext::new(&mut state, &mut callbacks, None, 0);
        let fns = A32CallbackContext::callback_fns();

        let read_32: ReadFn = unsafe { std::mem::transmute(fns.read_memory_32) };
        let write_32: WriteFn = unsafe { std::mem::transmute(fns.write_memory_32) };
        let exclusive_read_32: ReadFn =
            unsafe { std::mem::transmute(fns.exclusive_read_memory_32) };
        let exclusive_write_32: ExclusiveWriteFn =
            unsafe { std::mem::transmute(fns.exclusive_write_memory_32) };

        assert_eq!(read_32(&mut context, 4), 0x1122_3344);
        write_32(&mut context, 8, 0xaabb_ccdd);
        assert_eq!(read_32(&mut context, 8), 0xaabb_ccdd);

        assert_eq!(exclusive_read_32(&mut context, 8), 0xaabb_ccdd);
        assert_eq!(state.exclusive_state, 1);
        state.exclusive_state = 0;
        assert_eq!(exclusive_write_32(&mut context, 8, 0xfeed_face), 0);
        assert_eq!(state.exclusive_state, 0);
        assert_eq!(read_32(&mut context, 8), 0xfeed_face);
    }

    #[test]
    fn callback_context_exclusive_write_uses_global_monitor_after_emitted_clear() {
        type ReadFn = extern "C" fn(*mut A32CallbackContext, u64) -> u64;
        type ExclusiveWriteFn = extern "C" fn(*mut A32CallbackContext, u64, u64) -> u64;

        let mut state = A32JitState::new();
        let mut callbacks = TestCallbacks {
            code: vec![],
            memory: [0; 64],
        };
        callbacks.memory_write_32(8, 0xaabb_ccdd);
        let mut monitor = ExclusiveMonitor::new(1);
        let mut context =
            A32CallbackContext::new(&mut state, &mut callbacks, Some(&mut monitor), 0);
        let fns = A32CallbackContext::callback_fns();

        let read_32: ReadFn = unsafe { std::mem::transmute(fns.read_memory_32) };
        let exclusive_read_32: ReadFn =
            unsafe { std::mem::transmute(fns.exclusive_read_memory_32) };
        let exclusive_write_32: ExclusiveWriteFn =
            unsafe { std::mem::transmute(fns.exclusive_write_memory_32) };

        assert_eq!(exclusive_read_32(&mut context, 8), 0xaabb_ccdd);
        assert_eq!(state.exclusive_state, 1);

        // Upstream `CallbackOnlyEmitExclusiveWriteMemory` checks and clears
        // this byte before branching to the exclusive-write trampoline.
        state.exclusive_state = 0;
        assert_eq!(state.exclusive_state, 0);
        assert_eq!(exclusive_write_32(&mut context, 8, 0xfeed_face), 0);
        assert_eq!(read_32(&mut context, 8), 0xfeed_face);
    }
}
