//! x64 implementation of the public A64 JIT interface.
//!
//! Upstream owner: `dynarmic/backend/x64/a64_interface.cpp`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::backend::x64::a64_emit_x64::A64EmitX64;
use crate::backend::x64::a64_jitstate::A64JitState;
use crate::backend::x64::block_of_code::{RunCodeCallbacks, RunCodeFn};
use crate::backend::x64::callback::ArgCallback;
use crate::backend::x64::emit_context::{EmitCallbacks, EmitConfig, RawExclusiveWriteCallbacks};
use crate::common::llvm_disassemble::disassemble_x64;
use crate::frontend::a64::translate::TranslationOptions;
use crate::interface::a64::config::{
    DataCacheOperation as A64DataCacheOperation, Exception as A64Exception,
    InstructionCacheOperation as A64InstructionCacheOperation, UserCallbacks as A64UserCallbacks,
    UserConfig as A64UserConfig, Vector,
};
use crate::interface::halt_reason::HaltReason;
use crate::ir::location::LocationDescriptor;
use crate::jit::{
    block_count_counters, block_count_range, block_trace_range, record_first_pc,
    watch_write_target, PC_TRACE_ACTIVE,
};

const MINIMUM_REMAINING_CODE_SIZE: usize = 1024 * 1024;

/// Public ARM64 JIT compiler.
///
/// This is the main entry point for consumers (e.g., ruzu). Create one
/// per CPU core, configure callbacks, then call `run()` or `step()`.
pub(crate) struct A64Jit {
    pub(crate) inner: Box<JitInner>,
}

/// Internal JIT state. Box'd for stable heap pointer used by callback trampolines.
pub(crate) struct JitInner {
    pub(crate) jit_state: A64JitState,
    pub(crate) exclusive_value: [u64; 2],
    pub(crate) emitter: Option<A64EmitX64>,
    pub(crate) callbacks: Box<dyn A64UserCallbacks>,
    pub(crate) run_code_fn: Option<RunCodeFn>,
    pub(crate) is_executing: bool,
    pub(crate) global_monitor: Option<*mut crate::interface::exclusive_monitor::ExclusiveMonitor>,
    pub(crate) processor_id: usize,
    pub(crate) invalidate_entire_cache: bool,
    pub(crate) invalid_cache_ranges: Vec<(u64, u64)>,
    pub(crate) invalidation_mutex: Mutex<()>,
}

impl JitInner {
    fn perform_requested_cache_invalidation(&mut self, halt_reason: HaltReason) {
        if !halt_reason.contains(HaltReason::CACHE_INVALIDATION) {
            return;
        }

        let _lock = self
            .invalidation_mutex
            .lock()
            .expect("A64 cache invalidation mutex poisoned");
        let halt = unsafe { &*(&self.jit_state.halt_reason as *const u32 as *const AtomicU32) };
        halt.fetch_and(!HaltReason::CACHE_INVALIDATION.bits(), Ordering::Release);

        if !self.invalidate_entire_cache && self.invalid_cache_ranges.is_empty() {
            return;
        }

        self.jit_state.reset_rsb();
        let emitter = self.emitter.as_mut().expect("A64 emitter is initialized");
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
            .expect("A64 emitter is initialized")
            .lookup_cached_block(location)
        {
            return code_ptr;
        }

        self.emitter
            .as_mut()
            .expect("A64 emitter is initialized")
            .make_writable()
            .expect("making the A64 code cache writable failed");

        if self
            .emitter
            .as_ref()
            .expect("A64 emitter is initialized")
            .code
            .space_remaining()
            < MINIMUM_REMAINING_CODE_SIZE
        {
            self.invalidate_entire_cache = true;
            self.perform_requested_cache_invalidation(HaltReason::CACHE_INVALIDATION);
        }

        let inner_ptr = self as *mut JitInner;
        let read_code = move |vaddr: u64| -> Option<u32> {
            let inner = unsafe { &*inner_ptr };
            inner.callbacks.memory_read_code(vaddr)
        };
        let emitter = self.emitter.as_mut().expect("A64 emitter is initialized");
        let code_ptr = emitter.get_or_compile_block(location, &read_code);
        unsafe {
            emitter
                .get_run_code_fn()
                .expect("making the A64 code cache executable failed");
        }
        code_ptr
    }
}

fn a64_trace_registry() -> &'static Mutex<HashMap<usize, usize>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, usize>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn a64_trace_env_enabled() -> bool {
    std::env::var_os("RUZU_BLOCK_TRACE_PC").is_some()
        || std::env::var_os("RUZU_BLOCK_TRACE_CALLER_AT").is_some()
        || std::env::var_os("RUZU_BLOCK_TRACE_BAD_X19_CALLER_AT").is_some()
        || std::env::var_os("RUZU_BLOCK_TRACE_BAD_X0_LIVE_LR_AT").is_some()
        || std::env::var_os("RUZU_BLOCK_TRACE_BAD_X1_LIVE_LR_AT").is_some()
        || std::env::var_os("RUZU_BLOCK_TRACE_LIVE_LR_AT").is_some()
        || std::env::var_os("RUZU_DUMP_MEM_AT").is_some()
        || std::env::var_os("RUZU_DUMP_VEC_AT").is_some()
        || std::env::var_os("RUZU_DUMP_STRING_AT").is_some()
        || std::env::var_os("RUZU_BLOCK_COUNT_PC").is_some()
        || std::env::var_os("RUZU_FIRST_PCS_PER_CORE").is_some()
        || PC_TRACE_ACTIVE.load(std::sync::atomic::Ordering::Relaxed)
}

/// RUZU_BLOCK_COUNT_PC=0xLO-0xHI: increment per-core atomic counter on every
/// block entry where guest PC is in the range. Print summary on Drop or on
/// SIGUSR1. Lighter than the eprintln-based BLOCK64 trace — designed to NOT
/// mask multi-core race timing windows.
fn a64_trace_block_entry(inner: &mut JitInner) {
    // Lightweight per-core counter — no eprintln, no syscall. Designed to
    // measure cross-core block-entry distribution without masking timing
    // windows of the multi-core race.
    if let Some((lo, hi)) = block_count_range() {
        let pc = inner.jit_state.pc as u32;
        if pc >= lo && pc < hi {
            let idx = inner.processor_id.min(15);
            block_count_counters()[idx].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    // Capture first-N cold-entry PCs per core (in-memory, no eprintln).
    // No-op when RUZU_FIRST_PCS_PER_CORE is unset.
    record_first_pc(inner.processor_id, inner.jit_state.pc);

    // PC-window tracer for A64 (mirrors the A32 trampoline). When ruzu's SVC
    // dispatcher activates the window, emit a [TRACE_PC] line per block
    // transition. Logs PC + LR (x30) + SP + the args/locals most useful
    // for STK's random_bytes investigation: x0 (out_ptr), x21 (size),
    // x22 (saved out_ptr), x20 (loop counter), x18 (rounds counter).
    if PC_TRACE_ACTIVE.load(std::sync::atomic::Ordering::Relaxed) {
        let r = &inner.jit_state.reg;
        eprintln!(
            "[TRACE_PC] pc=0x{:016X} lr=0x{:016X} sp=0x{:016X} x0=0x{:016X} x18=0x{:016X} x20=0x{:016X} x21=0x{:016X} x22=0x{:016X}",
            inner.jit_state.pc,
            r[30], inner.jit_state.sp, r[0], r[18], r[20], r[21], r[22]
        );
    }

    if let Some((lo, hi)) = block_trace_range() {
        let pc = inner.jit_state.pc as u32;
        if pc >= lo && pc < hi {
            let r = &inner.jit_state.reg;
            // RUZU_TRACE_FREE_X0_RANGE=0xLO-0xHI — only log block entries
            // where x0 is in the specified range. Used to find the
            // specific free() call whose freed pointer triggers a
            // use-after-free wedge. Lower overhead than logging every
            // block-entry to a wide PC range; preserves the multi-core
            // race timing window better.
            let pass_x0_filter = match std::env::var("RUZU_TRACE_FREE_X0_RANGE") {
                Ok(spec) => {
                    let mut parts = spec.splitn(2, '-');
                    let lo_str = parts.next().unwrap_or("").trim_start_matches("0x");
                    let hi_str = parts.next().unwrap_or("").trim_start_matches("0x");
                    let lo64 = u64::from_str_radix(lo_str, 16).unwrap_or(0);
                    let hi64 = u64::from_str_radix(hi_str, 16).unwrap_or(u64::MAX);
                    r[0] >= lo64 && r[0] < hi64
                }
                Err(_) => true,
            };
            if pass_x0_filter {
                // RUZU_BLOCK_TRACE_INCLUDE_TID=1 — also dump host thread id
                // (gettid()) and emulator core index per block-entry. Used to
                // identify which guest threads/emulator-cores enter the same
                // guest block concurrently (multi-core race investigation).
                let include_tid = std::env::var_os("RUZU_BLOCK_TRACE_INCLUDE_TID").is_some();
                if include_tid {
                    #[cfg(target_os = "linux")]
                    let tid = unsafe { libc::syscall(libc::SYS_gettid) };
                    #[cfg(target_os = "macos")]
                    let tid = unsafe {
                        let mut t: u64 = 0;
                        libc::pthread_threadid_np(libc::pthread_self(), &mut t);
                        t as i64
                    };
                    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                    let tid: i64 = 0;
                    eprintln!(
                        "[BLOCK64] core={} tid={} pc=0x{:016X} lr=0x{:016X} sp=0x{:016X} x0=0x{:016X} x1=0x{:016X} x2=0x{:016X} x3=0x{:016X} x19=0x{:016X} x20=0x{:016X} x21=0x{:016X} x22=0x{:016X} x23=0x{:016X} x30=0x{:016X}",
                        inner.processor_id, tid,
                        inner.jit_state.pc,
                        r[30],
                        inner.jit_state.sp,
                        r[0], r[1], r[2], r[3],
                        r[19], r[20], r[21], r[22], r[23],
                        r[30]
                    );
                } else {
                    eprintln!(
                        "[BLOCK64] pc=0x{:016X} lr=0x{:016X} sp=0x{:016X} x0=0x{:016X} x1=0x{:016X} x2=0x{:016X} x3=0x{:016X} x19=0x{:016X} x20=0x{:016X} x21=0x{:016X} x22=0x{:016X} x23=0x{:016X} x30=0x{:016X}",
                        inner.jit_state.pc,
                        r[30],
                        inner.jit_state.sp,
                        r[0],
                        r[1],
                        r[2],
                        r[3],
                        r[19],
                        r[20],
                        r[21],
                        r[22],
                        r[23],
                        r[30]
                    );
                }
            }
        }
    }

    // Targeted: when entering specific guest PCs, dump saved-LR from stack
    // (= the BL site that called the function we're now in). Gated by
    // RUZU_BLOCK_TRACE_CALLER_AT=0xPC1,0xPC2,...
    if let Ok(env) = std::env::var("RUZU_BLOCK_TRACE_CALLER_AT") {
        let pc64 = inner.jit_state.pc;
        for raw_target in env.split(',') {
            let raw = raw_target.trim().trim_start_matches("0x");
            if let Ok(target) = u64::from_str_radix(raw, 16) {
                if pc64 == target {
                    // For 0x80772178: caller PC is at sp+0x8 (saved x30 of mutex_lock prologue).
                    // Caller's caller (grandparent): for aligned_alloc whose
                    // prologue pushed x29,x30 with #0x40 decrement, the saved
                    // x30 is at sp+0x20 + 0x8 = sp+0x28.
                    let sp = inner.jit_state.sp;
                    let saved_lr = inner.callbacks.memory_read_64(sp + 8);
                    let grandparent_lr = inner.callbacks.memory_read_64(sp + 0x28);
                    eprintln!(
                        "[CALLER_AT] pc=0x{:X} sp=0x{:X} saved_x30@sp+8=0x{:X} grand_x30@sp+0x28=0x{:X} x19=0x{:X}",
                        pc64,
                        sp,
                        saved_lr,
                        grandparent_lr,
                        inner.jit_state.reg[19],
                    );
                }
            }
        }
    }

    // Same stack-walk as RUZU_BLOCK_TRACE_CALLER_AT, but only prints when
    // X19 has STK's shifted-heap-pointer shape (0x00002101...). This keeps
    // the allocator/free trace low-noise enough to avoid timing perturbation.
    if let Ok(env) = std::env::var("RUZU_BLOCK_TRACE_BAD_X19_CALLER_AT") {
        let pc64 = inner.jit_state.pc;
        let x19 = inner.jit_state.reg[19];
        let bad_x19_shape = (x19 >> 40) == 0x21 && ((x19 >> 32) & 0xFF) == 0x01;
        if bad_x19_shape {
            for raw_target in env.split(',') {
                let raw = raw_target.trim().trim_start_matches("0x");
                if let Ok(target) = u64::from_str_radix(raw, 16) {
                    if pc64 == target {
                        let sp = inner.jit_state.sp;
                        let saved_lr = inner.callbacks.memory_read_64(sp + 8);
                        let grandparent_lr = inner.callbacks.memory_read_64(sp + 0x28);
                        let r = &inner.jit_state.reg;
                        eprintln!(
                            "[BAD_X19_CALLER_AT] pc=0x{:X} sp=0x{:X} saved_x30@sp+8=0x{:X} grand_x30@sp+0x28=0x{:X} x0=0x{:X} x1=0x{:X} x2=0x{:X} x3=0x{:X} x4=0x{:X} x5=0x{:X} x19=0x{:X}",
                            pc64, sp, saved_lr, grandparent_lr, r[0], r[1], r[2], r[3], r[4], r[5], x19,
                        );
                    }
                }
            }
        }
    }

    // Function-entry variant for delete/free wrappers: X0 is the pointer
    // argument before wrapper code moves it to X19 and forwards it as X1 to
    // the allocator free path.
    if let Ok(env) = std::env::var("RUZU_BLOCK_TRACE_BAD_X0_LIVE_LR_AT") {
        let pc64 = inner.jit_state.pc;
        let x0 = inner.jit_state.reg[0];
        let bad_x0_shape = (x0 >> 40) == 0x21 && ((x0 >> 32) & 0xFF) == 0x01;
        if bad_x0_shape {
            for raw_target in env.split(',') {
                let raw = raw_target.trim().trim_start_matches("0x");
                if let Ok(target) = u64::from_str_radix(raw, 16) {
                    if pc64 == target {
                        let r = &inner.jit_state.reg;
                        eprintln!(
                            "[BAD_X0_LIVE_LR_AT] pc=0x{:X} sp=0x{:X} x30=0x{:X} x0=0x{:X} x1=0x{:X} x2=0x{:X} x3=0x{:X} x4=0x{:X} x5=0x{:X}",
                            pc64, inner.jit_state.sp, r[30], r[0], r[1], r[2], r[3], r[4], r[5],
                        );
                    }
                }
            }
        }
    }

    // Function-entry variant for free/delete paths: X1 is the pointer
    // argument before the prologue copies it to X19. X30 is still the live
    // caller return address, so no guest stack read is needed.
    if let Ok(env) = std::env::var("RUZU_BLOCK_TRACE_BAD_X1_LIVE_LR_AT") {
        let pc64 = inner.jit_state.pc;
        let x1 = inner.jit_state.reg[1];
        let bad_x1_shape = (x1 >> 40) == 0x21 && ((x1 >> 32) & 0xFF) == 0x01;
        if bad_x1_shape {
            for raw_target in env.split(',') {
                let raw = raw_target.trim().trim_start_matches("0x");
                if let Ok(target) = u64::from_str_radix(raw, 16) {
                    if pc64 == target {
                        let r = &inner.jit_state.reg;
                        eprintln!(
                            "[BAD_X1_LIVE_LR_AT] pc=0x{:X} sp=0x{:X} x30=0x{:X} x0=0x{:X} x1=0x{:X} x2=0x{:X} x3=0x{:X} x4=0x{:X} x5=0x{:X}",
                            pc64, inner.jit_state.sp, r[30], r[0], r[1], r[2], r[3], r[4], r[5],
                        );
                    }
                }
            }
        }
    }

    // Same idea but using LIVE x30 (register), useful when at the function-entry
    // PC where the prologue hasn't yet pushed x30 to stack.
    if let Ok(env) = std::env::var("RUZU_BLOCK_TRACE_LIVE_LR_AT") {
        let pc64 = inner.jit_state.pc;
        for raw_target in env.split(',') {
            let raw = raw_target.trim().trim_start_matches("0x");
            if let Ok(target) = u64::from_str_radix(raw, 16) {
                if pc64 == target {
                    let r = &inner.jit_state.reg;
                    eprintln!(
                        "[LIVE_LR_AT] pc=0x{:X} sp=0x{:X} x30=0x{:X} x0=0x{:X} x1=0x{:X} x2=0x{:X} x19=0x{:X} x20=0x{:X} x21=0x{:X} x22=0x{:X} x23=0x{:X} x24=0x{:X} x25=0x{:X} x26=0x{:X} x27=0x{:X} x28=0x{:X}",
                        pc64,
                        inner.jit_state.sp,
                        r[30],
                        r[0],
                        r[1],
                        r[2],
                        r[19],
                        r[20],
                        r[21],
                        r[22],
                        r[23],
                        r[24],
                        r[25],
                        r[26],
                        r[27],
                        r[28],
                    );
                }
            }
        }
    }

    // Generic memory-dump-at-PC. Format: RUZU_DUMP_MEM_AT=PC:reg:size,PC:reg:size,...
    // PC is hex, reg is x register index 0..30 or `sp`, size is number of BYTES
    // to dump. Reads guest memory at the value of `reg`/SP when the block enters.
    if let Ok(env) = std::env::var("RUZU_DUMP_MEM_AT") {
        let pc64 = inner.jit_state.pc;
        for spec in env.split(',') {
            let parts: Vec<&str> = spec.split(':').collect();
            if parts.len() != 3 {
                continue;
            }
            let pc_target = u64::from_str_radix(parts[0].trim().trim_start_matches("0x"), 16).ok();
            let reg = parts[1].trim();
            let size = parts[2].trim().parse::<usize>().ok();
            if let (Some(target), Some(size)) = (pc_target, size) {
                if pc64 == target && size <= 256 {
                    let addr = if reg.eq_ignore_ascii_case("sp") {
                        inner.jit_state.sp
                    } else if let Ok(reg) = reg.parse::<usize>() {
                        if reg < 31 {
                            inner.jit_state.reg[reg]
                        } else {
                            continue;
                        }
                    } else {
                        continue;
                    };
                    let mut bytes = Vec::with_capacity(size);
                    for off in (0..size).step_by(8) {
                        let v = inner.callbacks.memory_read_64(addr + off as u64);
                        for i in 0..8.min(size - off) {
                            bytes.push(((v >> (i * 8)) & 0xFF) as u8);
                        }
                    }
                    let hex: String = bytes
                        .iter()
                        .map(|b| format!("{:02x}", b))
                        .collect::<Vec<_>>()
                        .join(" ");
                    let ascii: String = bytes
                        .iter()
                        .map(|&b| {
                            if b >= 0x20 && b < 0x7f {
                                b as char
                            } else {
                                '.'
                            }
                        })
                        .collect();
                    eprintln!(
                        "[DUMP_MEM_AT] pc=0x{:X} {}=0x{:X} bytes[{}]: {}  | ascii: {:?}",
                        pc64, reg, addr, size, hex, ascii,
                    );
                }
            }
        }
    }

    // Dump selected A64 vector registers at PC. Format:
    // RUZU_DUMP_VEC_AT=PC:vN/vM/...,PC:vN/...
    if let Ok(env) = std::env::var("RUZU_DUMP_VEC_AT") {
        let pc64 = inner.jit_state.pc;
        for spec in env.split(',') {
            let Some((pc_raw, regs_raw)) = spec.split_once(':') else {
                continue;
            };
            let Some(target) = u64::from_str_radix(pc_raw.trim().trim_start_matches("0x"), 16).ok()
            else {
                continue;
            };
            if pc64 != target {
                continue;
            }
            for reg_raw in regs_raw.split('/') {
                let reg_raw = reg_raw
                    .trim()
                    .trim_start_matches('v')
                    .trim_start_matches('V');
                let Some(index) = reg_raw.parse::<usize>().ok() else {
                    continue;
                };
                if index >= 32 {
                    continue;
                }
                let lo = inner.jit_state.vec[index * 2];
                let hi = inner.jit_state.vec[index * 2 + 1];
                let mut bytes = [0u8; 16];
                bytes[..8].copy_from_slice(&lo.to_le_bytes());
                bytes[8..].copy_from_slice(&hi.to_le_bytes());
                let hex = bytes
                    .iter()
                    .map(|b| format!("{:02x}", b))
                    .collect::<Vec<_>>()
                    .join(" ");
                eprintln!(
                    "[DUMP_VEC_AT] pc=0x{:X} v{} lo=0x{:016X} hi=0x{:016X} bytes={}",
                    pc64, index, lo, hi, hex
                );
            }
        }
    }

    // Dump a guest std::string-like object at PC. Format:
    // RUZU_DUMP_STRING_AT=PC:reg[:max],...
    // `reg` points at an object whose first three qwords are ptr/len/cap.
    if let Ok(env) = std::env::var("RUZU_DUMP_STRING_AT") {
        let pc64 = inner.jit_state.pc;
        for spec in env.split(',') {
            let parts: Vec<&str> = spec.split(':').collect();
            if parts.len() < 2 || parts.len() > 3 {
                continue;
            }
            let pc_target = u64::from_str_radix(parts[0].trim().trim_start_matches("0x"), 16).ok();
            let reg = parts[1].trim().parse::<usize>().ok();
            let max = parts
                .get(2)
                .and_then(|raw| raw.trim().parse::<usize>().ok())
                .unwrap_or(160);
            if let (Some(target), Some(reg)) = (pc_target, reg) {
                if pc64 == target && reg < 31 {
                    let obj = inner.jit_state.reg[reg];
                    let ptr = inner.callbacks.memory_read_64(obj);
                    let len = inner.callbacks.memory_read_64(obj + 8) as usize;
                    let cap = inner.callbacks.memory_read_64(obj + 16);
                    let size = len.min(max).min(512);
                    let mut bytes = Vec::with_capacity(size);
                    for off in (0..size).step_by(8) {
                        let v = inner.callbacks.memory_read_64(ptr + off as u64);
                        for i in 0..8.min(size - off) {
                            bytes.push(((v >> (i * 8)) & 0xFF) as u8);
                        }
                    }
                    let text: String = bytes
                        .iter()
                        .map(|&b| {
                            if b >= 0x20 && b < 0x7f {
                                b as char
                            } else {
                                '.'
                            }
                        })
                        .collect();
                    eprintln!(
                        "[DUMP_STRING_AT] pc=0x{:X} x{}=0x{:X} ptr=0x{:X} len={} cap={} text={:?}",
                        pc64, reg, obj, ptr, len, cap, text,
                    );
                }
            }
        }
    }
}

pub(crate) extern "C" fn a64_block_entry_trace_hook(jit_state_ptr: u64) {
    if !a64_trace_env_enabled() {
        return;
    }
    let Some(inner_ptr) = a64_trace_registry()
        .lock()
        .ok()
        .and_then(|registry| registry.get(&(jit_state_ptr as usize)).copied())
    else {
        return;
    };
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    a64_trace_block_entry(inner);
}

/// ABI-stable two-lane payload used by 128-bit memory-read trampolines.
/// SysV returns it in RAX:RDX; the MSVC wrapper writes it through an explicit
/// pointer matching upstream's stack-buffer path.
#[repr(C)]
pub struct Pair128 {
    pub lo: u64,
    pub hi: u64,
}

const _: () = {
    assert!(core::mem::size_of::<Pair128>() == 16);
    assert!(core::mem::align_of::<Pair128>() == 8);
};

impl A64Jit {
    /// Create a new A64Jit from the given configuration.
    ///
    /// This allocates the code buffer, generates the dispatcher prelude,
    /// and wires up all callback trampolines.
    pub fn new(config: A64UserConfig) -> Result<Self, String> {
        let cache_size = config.code_cache_size as usize;
        let effective_optimizations = config.effective_optimizations();

        // Phase 1: Create boxed JitInner with stable heap address
        let mut inner = Box::new(JitInner {
            jit_state: A64JitState::new(),
            emitter: None,
            exclusive_value: [0; 2],
            callbacks: config.callbacks,
            run_code_fn: None,
            is_executing: false,
            global_monitor: config.global_monitor,
            processor_id: config.processor_id as usize,
            invalidate_entire_cache: false,
            invalid_cache_ranges: Vec::new(),
            invalidation_mutex: Mutex::new(()),
        });
        let halt_ptr = &inner.jit_state.halt_reason as *const u32;
        inner.callbacks.set_halt_reason_ptr(halt_ptr);
        let pc_ptr = &inner.jit_state.pc as *const u64 as *const u32;
        inner.callbacks.set_pc_ptr(pc_ptr);

        // Phase 2: Take stable pointer for callback trampolines
        let inner_ptr = &mut *inner as *mut JitInner as u64;
        let jit_state_ptr = &mut inner.jit_state as *mut A64JitState as usize;
        a64_trace_registry()
            .lock()
            .expect("A64 trace registry poisoned")
            .insert(jit_state_ptr, inner_ptr as usize);

        // Build RunCodeCallbacks (dispatcher-level callbacks)
        let run_callbacks = RunCodeCallbacks {
            lookup_block: Box::new(ArgCallback::new(
                lookup_block_trampoline as usize as u64,
                inner_ptr,
            )),
            add_ticks: Box::new(ArgCallback::new(
                add_ticks_trampoline as usize as u64,
                inner_ptr,
            )),
            get_ticks_remaining: Box::new(ArgCallback::new(
                get_ticks_remaining_trampoline as usize as u64,
                inner_ptr,
            )),
            enable_cycle_counting: config.enable_cycle_counting,
            fastmem_pointer: config.fastmem_pointer.map(|p| p as *const u8),
            page_table_pointer: config
                .page_table
                .map(|pointer| pointer.cast::<u8>() as *const u8),
        };

        // Build EmitCallbacks (block-level callbacks for memory/system ops)
        let emit_callbacks = EmitCallbacks {
            memory_read_8: Box::new(ArgCallback::new(
                memory_read_8_trampoline as usize as u64,
                inner_ptr,
            )),
            memory_read_16: Box::new(ArgCallback::new(
                memory_read_16_trampoline as usize as u64,
                inner_ptr,
            )),
            memory_read_32: Box::new(ArgCallback::new(
                memory_read_32_trampoline as usize as u64,
                inner_ptr,
            )),
            memory_read_64: Box::new(ArgCallback::new(
                memory_read_64_trampoline as usize as u64,
                inner_ptr,
            )),
            memory_read_128: Box::new(ArgCallback::new(
                memory_read_128_trampoline as usize as u64,
                inner_ptr,
            )),
            memory_write_8: Box::new(ArgCallback::new(
                memory_write_8_trampoline as usize as u64,
                inner_ptr,
            )),
            memory_write_16: Box::new(ArgCallback::new(
                memory_write_16_trampoline as usize as u64,
                inner_ptr,
            )),
            memory_write_32: Box::new(ArgCallback::new(
                memory_write_32_trampoline as usize as u64,
                inner_ptr,
            )),
            memory_write_64: Box::new(ArgCallback::new(
                memory_write_64_trampoline as usize as u64,
                inner_ptr,
            )),
            memory_write_128: Box::new(ArgCallback::new(
                memory_write_128_trampoline as usize as u64,
                inner_ptr,
            )),
            call_supervisor: Box::new(ArgCallback::new(
                call_supervisor_trampoline as usize as u64,
                inner_ptr,
            )),
            exception_raised: Box::new(ArgCallback::new(
                exception_raised_trampoline as usize as u64,
                inner_ptr,
            )),
            data_cache_operation: Box::new(ArgCallback::new(
                data_cache_op_trampoline as usize as u64,
                inner_ptr,
            )),
            instruction_cache_operation: Box::new(ArgCallback::new(
                instruction_cache_op_trampoline as usize as u64,
                inner_ptr,
            )),
            instruction_synchronization_barrier: Box::new(ArgCallback::new(
                instruction_synchronization_barrier_trampoline as usize as u64,
                inner_ptr,
            )),
            add_ticks: Box::new(ArgCallback::new(
                add_ticks_trampoline as usize as u64,
                inner_ptr,
            )),
            get_ticks_remaining: Box::new(ArgCallback::new(
                get_ticks_remaining_trampoline as usize as u64,
                inner_ptr,
            )),
            get_cntpct: Box::new(ArgCallback::new(
                get_cntpct_trampoline as usize as u64,
                inner_ptr,
            )),
            exclusive_clear: Box::new(ArgCallback::new(
                exclusive_clear_trampoline as usize as u64,
                inner_ptr,
            )),
            exclusive_read_8: Box::new(ArgCallback::new(
                exclusive_read_8_trampoline as usize as u64,
                inner_ptr,
            )),
            exclusive_read_16: Box::new(ArgCallback::new(
                exclusive_read_16_trampoline as usize as u64,
                inner_ptr,
            )),
            exclusive_read_32: Box::new(ArgCallback::new(
                exclusive_read_32_trampoline as usize as u64,
                inner_ptr,
            )),
            exclusive_read_64: Box::new(ArgCallback::new(
                exclusive_read_64_trampoline as usize as u64,
                inner_ptr,
            )),
            exclusive_read_128: Box::new(ArgCallback::new(
                exclusive_read_128_trampoline as usize as u64,
                inner_ptr,
            )),
            exclusive_write_8: Box::new(ArgCallback::new(
                exclusive_write_8_trampoline as usize as u64,
                inner_ptr,
            )),
            exclusive_write_16: Box::new(ArgCallback::new(
                exclusive_write_16_trampoline as usize as u64,
                inner_ptr,
            )),
            exclusive_write_32: Box::new(ArgCallback::new(
                exclusive_write_32_trampoline as usize as u64,
                inner_ptr,
            )),
            exclusive_write_64: Box::new(ArgCallback::new(
                exclusive_write_64_trampoline as usize as u64,
                inner_ptr,
            )),
            exclusive_write_128: Box::new(ArgCallback::new(
                exclusive_write_128_trampoline as usize as u64,
                inner_ptr,
            )),
        };

        let emit_config = EmitConfig {
            coprocessors: crate::interface::a32::config::empty_coprocessors(),
            callbacks: emit_callbacks,
            raw_exclusive_write_callbacks: Some(RawExclusiveWriteCallbacks {
                write_8: Box::new(ArgCallback::new(
                    raw_exclusive_write_8_trampoline as usize as u64,
                    inner_ptr,
                )),
                write_16: Box::new(ArgCallback::new(
                    raw_exclusive_write_16_trampoline as usize as u64,
                    inner_ptr,
                )),
                write_32: Box::new(ArgCallback::new(
                    raw_exclusive_write_32_trampoline as usize as u64,
                    inner_ptr,
                )),
                write_64: Box::new(ArgCallback::new(
                    raw_exclusive_write_64_trampoline as usize as u64,
                    inner_ptr,
                )),
                write_128: Box::new(ArgCallback::new(
                    raw_exclusive_write_128_trampoline as usize as u64,
                    inner_ptr,
                )),
            }),
            enable_cycle_counting: config.enable_cycle_counting,
            memory: {
                crate::backend::common::emit_context::MemoryEmitConfig {
                    fastmem_address_space_bits: config.fastmem_address_space_bits as usize,
                    silently_mirror_fastmem: config.silently_mirror_fastmem,
                    fastmem_exclusive_access: config.fastmem_exclusive_access,
                    recompile_on_exclusive_fastmem_failure: config
                        .recompile_on_exclusive_fastmem_failure,
                    recompile_on_fastmem_failure: config.recompile_on_fastmem_failure,
                    page_table_present: config.page_table.is_some(),
                    page_table_address_space_bits: config.page_table_address_space_bits as usize,
                    silently_mirror_page_table: config.silently_mirror_page_table,
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
                }
            },
            global_monitor: config.global_monitor,
            tpidrro_el0: config.tpidrro_el0,
            tpidr_el0: config.tpidr_el0,
            cntfrq_el0: config.cntfrq_el0,
            ctr_el0: config.ctr_el0,
            dczid_el0: config.dczid_el0,
            hook_data_cache_operations: config.hook_data_cache_operations,
            hook_isb: config.hook_isb,
        };

        let translation_options = TranslationOptions {
            define_unpredictable_behaviour: config.define_unpredictable_behaviour,
            wall_clock_cntpct: config.wall_clock_cntpct,
            ..TranslationOptions::default()
        };

        // Phase 3: Create the emitter (contains code buffer + dispatcher + cache)
        let mut emitter = A64EmitX64::new(
            emit_config,
            run_callbacks,
            translation_options,
            effective_optimizations,
            cache_size,
        )?;
        // Forward the per-emulator-core index so JIT-emit-time diagnostics
        // (e.g. RUZU_BLOCK_PROLOGUE_COUNT_PC) can address the correct slot
        // in their per-core counter array.
        emitter.processor_id = config.processor_id as usize;

        // Extract run_code function pointer
        let run_code_fn = unsafe { emitter.get_run_code_fn()? };

        inner.emitter = Some(emitter);
        inner.run_code_fn = Some(run_code_fn);

        Ok(A64Jit { inner })
    }

    fn perform_requested_cache_invalidation(&mut self, halt_reason: HaltReason) {
        self.inner.perform_requested_cache_invalidation(halt_reason);
    }

    /// Execute JIT code until a halt reason is triggered.
    ///
    /// Matches upstream: Run() does RSB check then GetCurrentBlock() then RunCode().
    /// No mprotect on the hot path — only on cache miss (compilation).
    pub fn run(&mut self) -> HaltReason {
        assert!(
            !self.inner.is_executing,
            "Recursive JIT execution not allowed"
        );
        let halt_reason = HaltReason::from_bits_truncate(self.read_halt_reason());
        self.perform_requested_cache_invalidation(halt_reason);
        self.inner.is_executing = true;

        let unique_hash = self.inner.jit_state.get_unique_hash();
        let location = LocationDescriptor::new(unique_hash);
        let new_rsb_ptr =
            self.inner.jit_state.rsb_ptr.wrapping_sub(1) as usize & A64JitState::RSB_PTR_MASK;
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

        // Call the dispatcher
        let halt_bits = unsafe { run_fn(&mut self.inner.jit_state as *mut _, code_ptr) };
        self.inner
            .emitter
            .as_mut()
            .expect("A64 emitter is initialized")
            .process_pending_fastmem_recompiles()
            .expect("processing A64 fastmem recompiles failed");

        let halt_reason = HaltReason::from_bits_truncate(halt_bits);
        self.perform_requested_cache_invalidation(halt_reason);
        self.inner.is_executing = false;
        halt_reason
    }

    /// Execute a single instruction (single-step).
    ///
    /// Uses a dedicated step_code entry point that:
    /// - Sets cycle budget to 1
    /// - Atomically sets the STEP bit in halt_reason
    /// - Compiles a single-instruction block (via single_stepping descriptor)
    pub fn step(&mut self) -> HaltReason {
        assert!(
            !self.inner.is_executing,
            "Recursive JIT execution not allowed"
        );
        let halt_reason = HaltReason::from_bits_truncate(self.read_halt_reason());
        self.perform_requested_cache_invalidation(halt_reason);
        self.inner.is_executing = true;

        // Build location with single_stepping=true for 1-instruction block
        let a64_loc = crate::ir::location::A64LocationDescriptor::new(
            self.inner.jit_state.pc,
            self.inner.jit_state.fpcr,
            true,
        );
        let location = a64_loc.to_location();

        let code_ptr = self.inner.get_or_compile_block(location);

        // Get the step_code function pointer
        let step_fn = {
            let emitter = self.inner.emitter.as_mut().unwrap();
            unsafe { emitter.get_step_code_fn().unwrap() }
        };

        // Call the step_code entry (sets STEP atomically, cycles=1)
        let halt_bits = unsafe { step_fn(&mut self.inner.jit_state as *mut _, code_ptr) };
        self.inner
            .emitter
            .as_mut()
            .unwrap()
            .process_pending_fastmem_recompiles()
            .expect("processing A64 fastmem recompiles failed");

        let halt_reason = HaltReason::from_bits_truncate(halt_bits);
        self.perform_requested_cache_invalidation(halt_reason);
        self.inner.is_executing = false;
        halt_reason
    }

    /// Request halt from another thread (or same thread in a callback).
    ///
    /// Thread-safe: uses atomic OR on halt_reason.
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
        &self.inner.jit_state as *const _ as *const u8
    }

    /// Clear specific halt reason bits.
    pub fn clear_halt(&self, reason: HaltReason) {
        let halt_ptr = &self.inner.jit_state.halt_reason as *const u32 as *const AtomicU32;
        let atomic = unsafe { &*halt_ptr };
        atomic.fetch_and(!reason.bits(), Ordering::Release);
    }

    /// Reset CPU state without clearing the compiled-code cache.
    pub fn reset(&mut self) {
        assert!(
            !self.inner.is_executing,
            "Cannot reset while the JIT is executing"
        );
        self.inner.jit_state = A64JitState::new();
        self.inner.exclusive_value = [0; 2];
    }

    // ---- Register accessors ----

    pub fn get_register(&self, index: usize) -> u64 {
        if index == 31 {
            return self.get_sp();
        }

        self.inner.jit_state.reg[index]
    }

    pub fn set_register(&mut self, index: usize, value: u64) {
        if index == 31 {
            self.set_sp(value);
            return;
        }

        self.inner.jit_state.reg[index] = value;
    }

    pub fn get_registers(&self) -> [u64; 31] {
        self.inner.jit_state.reg
    }

    pub fn set_registers(&mut self, value: [u64; 31]) {
        self.inner.jit_state.reg = value;
    }

    pub fn get_pc(&self) -> u64 {
        self.inner.jit_state.pc
    }

    pub fn set_pc(&mut self, value: u64) {
        self.inner.jit_state.pc = value;
    }

    pub fn get_sp(&self) -> u64 {
        self.inner.jit_state.sp
    }

    pub fn set_sp(&mut self, value: u64) {
        self.inner.jit_state.sp = value;
    }

    pub fn get_pstate(&self) -> u32 {
        self.inner.jit_state.get_pstate()
    }

    pub fn set_pstate(&mut self, value: u32) {
        self.inner.jit_state.set_pstate(value);
    }

    pub fn get_vector(&self, index: usize) -> Vector {
        assert!(index < 32, "Vector register index out of range (0-31)");

        let lo = self.inner.jit_state.vec[index * 2];
        let hi = self.inner.jit_state.vec[index * 2 + 1];
        [lo, hi]
    }

    pub fn set_vector(&mut self, index: usize, value: Vector) {
        assert!(index < 32, "Vector register index out of range (0-31)");

        self.inner.jit_state.vec[index * 2] = value[0];
        self.inner.jit_state.vec[index * 2 + 1] = value[1];
    }

    pub fn get_vectors(&self) -> [Vector; 32] {
        std::array::from_fn(|index| self.get_vector(index))
    }

    pub fn set_vectors(&mut self, value: [Vector; 32]) {
        for (index, vector) in value.into_iter().enumerate() {
            self.set_vector(index, vector);
        }
    }

    pub fn get_vector_parts(&self, index: usize) -> (u64, u64) {
        let value = self.get_vector(index);
        (value[0], value[1])
    }

    pub fn set_vector_parts(&mut self, index: usize, lo: u64, hi: u64) {
        self.set_vector(index, [lo, hi]);
    }

    pub fn get_fpcr(&self) -> u32 {
        self.inner.jit_state.get_fpcr()
    }

    pub fn set_fpcr(&mut self, value: u32) {
        self.inner.jit_state.set_fpcr(value);
    }

    pub fn get_fpsr(&self) -> u32 {
        self.inner.jit_state.get_fpsr()
    }

    pub fn set_fpsr(&mut self, value: u32) {
        self.inner.jit_state.set_fpsr(value);
    }

    /// Clear exclusive monitor state.
    /// Matching dynarmic's `Jit::ClearExclusiveState()`.
    /// Called before `run()` to ensure no stale exclusive reservation persists.
    pub fn clear_exclusive_state(&mut self) {
        self.inner.jit_state.exclusive_state = 0;
    }

    /// Invalidate cached blocks in a memory range.
    pub fn invalidate_cache_range(&mut self, addr: u64, size: usize) {
        let _lock = self
            .inner
            .invalidation_mutex
            .lock()
            .expect("A64 cache invalidation mutex poisoned");
        let end = addr.wrapping_add(size as u64).wrapping_sub(1);
        self.inner.invalid_cache_ranges.push((addr, end));
        self.halt_execution(HaltReason::CACHE_INVALIDATION);
    }

    /// Clear all cached blocks.
    pub fn clear_cache(&mut self) {
        let _lock = self
            .inner
            .invalidation_mutex
            .lock()
            .expect("A64 cache invalidation mutex poisoned");
        self.inner.invalidate_entire_cache = true;
        self.halt_execution(HaltReason::CACHE_INVALIDATION);
    }

    pub fn is_executing(&self) -> bool {
        self.inner.is_executing
    }

    pub fn disassemble(&self) -> String {
        let emitter = self
            .inner
            .emitter
            .as_ref()
            .expect("A64 emitter is initialized");
        let begin = emitter.code.code_base_ptr();
        let end = begin.wrapping_add(emitter.code.code_size());
        disassemble_x64(begin, end)
    }
}

impl Drop for A64Jit {
    fn drop(&mut self) {
        let jit_state_ptr = &mut self.inner.jit_state as *mut A64JitState as usize;
        if let Ok(mut registry) = a64_trace_registry().lock() {
            registry.remove(&jit_state_ptr);
        }
    }
}

// ---------------------------------------------------------------------------
// Callback trampolines
// ---------------------------------------------------------------------------
//
// These are `extern "C"` functions called from JIT-generated code via
// ArgCallback. The first argument is always `inner_ptr: u64` (the fixed
// arg set up by ArgCallback), which we cast back to &mut JitInner to
// access the user's UserCallbacks.

/// Dispatcher callback: look up or compile the block at the current PC.
/// Returns the native code pointer in RAX.
extern "C" fn lookup_block_trampoline(inner_ptr: u64) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };

    let location = LocationDescriptor::new(inner.jit_state.get_unique_hash());

    if let Some(code_ptr) = inner
        .emitter
        .as_ref()
        .expect("A64 emitter is initialized")
        .lookup_cached_block(location)
    {
        return code_ptr as u64;
    }

    if std::env::var_os("RUZU_TRACE_A64_COMPILE_PC").is_some() {
        eprintln!(
            "[TRACE_A64_COMPILE_PC] pc=0x{:016X} lr=0x{:016X} sp=0x{:016X}",
            inner.jit_state.pc, inner.jit_state.reg[30], inner.jit_state.sp
        );
    }

    inner.get_or_compile_block(location) as u64
}

extern "C" fn add_ticks_trampoline(inner_ptr: u64, ticks: u64) {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    if PC_TRACE_ACTIVE.load(std::sync::atomic::Ordering::Relaxed) {
        static ADD_TRACE_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = ADD_TRACE_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if n < 100 {
            let r = &inner.jit_state.reg;
            eprintln!(
                "[ADDTICKS] ticks={} pc=0x{:016X} lr=0x{:016X} x21=0x{:016X} x22=0x{:016X} x7=0x{:016X} x20=0x{:016X}",
                ticks, inner.jit_state.pc, r[30], r[21], r[22], r[7], r[20]
            );
        }
    }
    inner.callbacks.add_ticks(ticks);
}

extern "C" fn get_ticks_remaining_trampoline(inner_ptr: u64) -> u64 {
    let inner = unsafe { &*(inner_ptr as *const JitInner) };
    // While PC_TRACE_ACTIVE, log live PC + key regs each tick check. Fires
    // even when the JIT is in a tight chained-block loop (which the
    // lookup_block_trampoline misses), since dynarmic checks
    // ticks_remaining periodically inside compiled blocks.
    if PC_TRACE_ACTIVE.load(std::sync::atomic::Ordering::Relaxed) {
        static TICK_TRACE_COUNT: std::sync::atomic::AtomicU32 =
            std::sync::atomic::AtomicU32::new(0);
        // Throttle to first 50 samples to avoid log flood.
        let n = TICK_TRACE_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if n < 50 {
            let r = &inner.jit_state.reg;
            eprintln!(
                "[TICK_PC] pc=0x{:016X} lr=0x{:016X} sp=0x{:016X} x18=0x{:016X} x20=0x{:016X} x21=0x{:016X} x22=0x{:016X} x7=0x{:016X}",
                inner.jit_state.pc,
                r[30], inner.jit_state.sp, r[18], r[20], r[21], r[22], r[7]
            );
        }
    }
    inner.callbacks.get_ticks_remaining()
}

// Memory read trampolines
extern "C" fn memory_read_8_trampoline(inner_ptr: u64, vaddr: u64) -> u64 {
    let inner = unsafe { &*(inner_ptr as *const JitInner) };
    inner.callbacks.memory_read_8(vaddr) as u64
}

extern "C" fn memory_read_16_trampoline(inner_ptr: u64, vaddr: u64) -> u64 {
    let inner = unsafe { &*(inner_ptr as *const JitInner) };
    inner.callbacks.memory_read_16(vaddr) as u64
}

extern "C" fn memory_read_32_trampoline(inner_ptr: u64, vaddr: u64) -> u64 {
    let inner = unsafe { &*(inner_ptr as *const JitInner) };
    inner.callbacks.memory_read_32(vaddr) as u64
}

extern "C" fn memory_read_64_trampoline(inner_ptr: u64, vaddr: u64) -> u64 {
    let inner = unsafe { &*(inner_ptr as *const JitInner) };
    inner.callbacks.memory_read_64(vaddr)
}

fn memory_read_128_impl(inner_ptr: u64, vaddr: u64) -> Pair128 {
    let inner = unsafe { &*(inner_ptr as *const JitInner) };
    let [lo, hi] = inner.callbacks.memory_read_128(vaddr);
    Pair128 { lo, hi }
}

#[cfg(not(target_os = "windows"))]
extern "C" fn memory_read_128_trampoline(inner_ptr: u64, vaddr: u64) -> Pair128 {
    memory_read_128_impl(inner_ptr, vaddr)
}

#[cfg(target_os = "windows")]
extern "C" fn memory_read_128_trampoline(inner_ptr: u64, vaddr: u64, ret_ptr: *mut Pair128) {
    unsafe { ret_ptr.write(memory_read_128_impl(inner_ptr, vaddr)) };
}

// Memory write trampolines

/// Cached watch target parsed from `RUZU_WATCH_WRITE=0xADDR[:LEN]` (LEN
/// defaults to 8). When set, every guest memory write that touches
/// `[addr, addr+len)` logs the JIT PC, LR (X30) and a few key callee-
/// saved registers to stderr. Useful for finding the writer of a static
/// that ends up in an unexpected state. Effective only with
/// `RUZU_NO_FASTMEM=1` — the fastmem fast path bypasses these
/// trampolines entirely.
fn maybe_log_watch_write(inner: &JitInner, vaddr: u64, width: usize, value_lo: u64, value_hi: u64) {
    let Some((wa, wsize)) = watch_write_target() else {
        return;
    };
    if vaddr.saturating_add(width as u64) <= wa || vaddr >= wa + wsize {
        return;
    }
    let pc = inner.jit_state.pc;
    let lr = inner.jit_state.reg[30];
    let x19 = inner.jit_state.reg[19];
    let x20 = inner.jit_state.reg[20];
    let x21 = inner.jit_state.reg[21];
    if width <= 8 {
        eprintln!(
            "[WATCH_WRITE] pc=0x{:08X} lr=0x{:08X} x19=0x{:X} x20=0x{:X} x21=0x{:X} vaddr=0x{:08X} width={} value=0x{:X}",
            pc, lr, x19, x20, x21, vaddr, width, value_lo
        );
    } else {
        eprintln!(
            "[WATCH_WRITE] pc=0x{:08X} lr=0x{:08X} x19=0x{:X} x20=0x{:X} x21=0x{:X} vaddr=0x{:08X} width=128 lo=0x{:X} hi=0x{:X}",
            pc, lr, x19, x20, x21, vaddr, value_lo, value_hi
        );
        // Dump V0..V31 so we can identify which source register held the
        // value (when the store is a vector STR Q), and check whether
        // neighbours (likely set by the same setup code) are zeros, all-
        // ones, or something else.
        for i in 0..32 {
            let vlo = inner.jit_state.vec[i * 2];
            let vhi = inner.jit_state.vec[i * 2 + 1];
            let marker = if vlo == value_lo && vhi == value_hi {
                " <-- match"
            } else {
                ""
            };
            eprintln!(
                "[WATCH_WRITE]   V{:<2}=0x{:016X}_{:016X}{}",
                i, vhi, vlo, marker
            );
        }
        // Also dump X0..X30 — for an STP Xn,Xm,[Xb,#imm] the value comes
        // from a GPR pair, so two adjacent X registers should hold
        // value_lo and value_hi.
        for i in 0..31 {
            let xv = inner.jit_state.reg[i];
            let marker = if xv == value_lo {
                " <-- lo match"
            } else if xv == value_hi {
                " <-- hi match"
            } else {
                ""
            };
            eprintln!("[WATCH_WRITE]   X{:<2}=0x{:016X}{}", i, xv, marker);
        }
    }
}

#[inline]
extern "C" fn memory_write_8_trampoline(inner_ptr: u64, vaddr: u64, value: u64) {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    maybe_log_watch_write(inner, vaddr, 1, value, 0);
    inner.callbacks.memory_write_8(vaddr, value as u8);
}

extern "C" fn memory_write_16_trampoline(inner_ptr: u64, vaddr: u64, value: u64) {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    maybe_log_watch_write(inner, vaddr, 2, value, 0);
    inner.callbacks.memory_write_16(vaddr, value as u16);
}

extern "C" fn memory_write_32_trampoline(inner_ptr: u64, vaddr: u64, value: u64) {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    maybe_log_watch_write(inner, vaddr, 4, value, 0);
    inner.callbacks.memory_write_32(vaddr, value as u32);
}

extern "C" fn memory_write_64_trampoline(inner_ptr: u64, vaddr: u64, value: u64) {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    maybe_log_watch_write(inner, vaddr, 8, value, 0);
    inner.callbacks.memory_write_64(vaddr, value);
}

fn memory_write_128_impl(inner_ptr: u64, vaddr: u64, value_lo: u64, value_hi: u64) {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    maybe_log_watch_write(inner, vaddr, 16, value_lo, value_hi);
    inner
        .callbacks
        .memory_write_128(vaddr, [value_lo, value_hi]);
}

#[cfg(not(target_os = "windows"))]
extern "C" fn memory_write_128_trampoline(
    inner_ptr: u64,
    vaddr: u64,
    value_lo: u64,
    value_hi: u64,
) {
    memory_write_128_impl(inner_ptr, vaddr, value_lo, value_hi);
}

#[cfg(target_os = "windows")]
extern "C" fn memory_write_128_trampoline(inner_ptr: u64, vaddr: u64, value: *const Pair128) {
    let value = unsafe { value.read_unaligned() };
    memory_write_128_impl(inner_ptr, vaddr, value.lo, value.hi);
}

// System trampolines
extern "C" fn call_supervisor_trampoline(inner_ptr: u64, svc_num: u64) {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    inner.callbacks.call_svc(svc_num as u32);
}

extern "C" fn exception_raised_trampoline(inner_ptr: u64, pc: u64, exception: u64) {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    inner
        .callbacks
        .exception_raised(pc, A64Exception::from_u32(exception as u32));
}

extern "C" fn data_cache_op_trampoline(inner_ptr: u64, op: u64, vaddr: u64) {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    inner
        .callbacks
        .data_cache_operation_raised(A64DataCacheOperation::from_u32(op as u32), vaddr);
}

extern "C" fn instruction_cache_op_trampoline(inner_ptr: u64, op: u64, vaddr: u64) {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    inner.callbacks.instruction_cache_operation_raised(
        A64InstructionCacheOperation::from_u32(op as u32),
        vaddr,
    );
}

extern "C" fn instruction_synchronization_barrier_trampoline(inner_ptr: u64) {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    inner.callbacks.instruction_synchronization_barrier_raised();
}

extern "C" fn get_cntpct_trampoline(inner_ptr: u64) -> u64 {
    let inner = unsafe { &*(inner_ptr as *const JitInner) };
    inner.callbacks.get_cntpct()
}

// Exclusive memory trampolines
extern "C" fn exclusive_clear_trampoline(inner_ptr: u64) {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    inner.jit_state.exclusive_state = 0;
}

extern "C" fn exclusive_read_8_trampoline(inner_ptr: u64, vaddr: u64) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    inner.jit_state.exclusive_state = 1;
    let value = if let Some(monitor) = inner.global_monitor {
        let callbacks = &mut inner.callbacks;
        unsafe {
            (&mut *monitor)
                .read_and_mark(inner.processor_id, vaddr, || callbacks.memory_read_8(vaddr))
        }
    } else {
        inner.callbacks.memory_read_8(vaddr)
    };
    inner.exclusive_value[0] = value as u64;
    value as u64
}

extern "C" fn exclusive_read_16_trampoline(inner_ptr: u64, vaddr: u64) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    inner.jit_state.exclusive_state = 1;
    let value = if let Some(monitor) = inner.global_monitor {
        let callbacks = &mut inner.callbacks;
        unsafe {
            (&mut *monitor).read_and_mark(inner.processor_id, vaddr, || {
                callbacks.memory_read_16(vaddr)
            })
        }
    } else {
        inner.callbacks.memory_read_16(vaddr)
    };
    inner.exclusive_value[0] = value as u64;
    value as u64
}

pub(crate) extern "C" fn exclusive_read_32_trampoline(inner_ptr: u64, vaddr: u64) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    inner.jit_state.exclusive_state = 1;
    let value = if let Some(monitor) = inner.global_monitor {
        let callbacks = &mut inner.callbacks;
        unsafe {
            (&mut *monitor).read_and_mark(inner.processor_id, vaddr, || {
                callbacks.memory_read_32(vaddr)
            })
        }
    } else {
        inner.callbacks.memory_read_32(vaddr)
    };
    inner.exclusive_value[0] = value as u64;
    value as u64
}

extern "C" fn exclusive_read_64_trampoline(inner_ptr: u64, vaddr: u64) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    inner.jit_state.exclusive_state = 1;
    let value = if let Some(monitor) = inner.global_monitor {
        let callbacks = &mut inner.callbacks;
        unsafe {
            (&mut *monitor).read_and_mark(inner.processor_id, vaddr, || {
                callbacks.memory_read_64(vaddr)
            })
        }
    } else {
        inner.callbacks.memory_read_64(vaddr)
    };
    inner.exclusive_value[0] = value;
    value
}

fn exclusive_read_128_impl(inner_ptr: u64, vaddr: u64) -> Pair128 {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    inner.jit_state.exclusive_state = 1;
    let [lo, hi] = if let Some(monitor) = inner.global_monitor {
        let callbacks = &mut inner.callbacks;
        let value: [u64; 2] = unsafe {
            (&mut *monitor).read_and_mark(inner.processor_id, vaddr, || {
                callbacks.memory_read_128(vaddr)
            })
        };
        value
    } else {
        inner.callbacks.memory_read_128(vaddr)
    };
    inner.exclusive_value[0] = lo;
    inner.exclusive_value[1] = hi;
    Pair128 { lo, hi }
}

#[cfg(not(target_os = "windows"))]
extern "C" fn exclusive_read_128_trampoline(inner_ptr: u64, vaddr: u64) -> Pair128 {
    exclusive_read_128_impl(inner_ptr, vaddr)
}

#[cfg(target_os = "windows")]
extern "C" fn exclusive_read_128_trampoline(inner_ptr: u64, vaddr: u64, ret_ptr: *mut Pair128) {
    unsafe { ret_ptr.write(exclusive_read_128_impl(inner_ptr, vaddr)) };
}

extern "C" fn exclusive_write_8_trampoline(inner_ptr: u64, vaddr: u64, value: u64) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    if inner.jit_state.exclusive_state == 0 {
        return 1;
    }
    inner.jit_state.exclusive_state = 0;
    if let Some(monitor) = inner.global_monitor {
        let callbacks = &mut inner.callbacks;
        return if unsafe {
            (&mut *monitor).do_exclusive_operation(inner.processor_id, vaddr, |expected: u8| {
                callbacks.memory_write_exclusive_8(vaddr, value as u8, expected)
            })
        } {
            0
        } else {
            1
        };
    }
    let expected = inner.exclusive_value[0] as u8;
    if inner
        .callbacks
        .memory_write_exclusive_8(vaddr, value as u8, expected)
    {
        0
    } else {
        1
    }
}

extern "C" fn exclusive_write_16_trampoline(inner_ptr: u64, vaddr: u64, value: u64) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    if inner.jit_state.exclusive_state == 0 {
        return 1;
    }
    inner.jit_state.exclusive_state = 0;
    if let Some(monitor) = inner.global_monitor {
        let callbacks = &mut inner.callbacks;
        return if unsafe {
            (&mut *monitor).do_exclusive_operation(inner.processor_id, vaddr, |expected: u16| {
                callbacks.memory_write_exclusive_16(vaddr, value as u16, expected)
            })
        } {
            0
        } else {
            1
        };
    }
    let expected = inner.exclusive_value[0] as u16;
    if inner
        .callbacks
        .memory_write_exclusive_16(vaddr, value as u16, expected)
    {
        0
    } else {
        1
    }
}

pub(crate) extern "C" fn exclusive_write_32_trampoline(
    inner_ptr: u64,
    vaddr: u64,
    value: u64,
) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    if inner.jit_state.exclusive_state == 0 {
        return 1;
    }
    inner.jit_state.exclusive_state = 0;
    if let Some(monitor) = inner.global_monitor {
        let callbacks = &mut inner.callbacks;
        return if unsafe {
            (&mut *monitor).do_exclusive_operation(inner.processor_id, vaddr, |expected: u32| {
                callbacks.memory_write_exclusive_32(vaddr, value as u32, expected)
            })
        } {
            0
        } else {
            1
        };
    }
    let expected = inner.exclusive_value[0] as u32;
    if inner
        .callbacks
        .memory_write_exclusive_32(vaddr, value as u32, expected)
    {
        0
    } else {
        1
    }
}

extern "C" fn exclusive_write_64_trampoline(inner_ptr: u64, vaddr: u64, value: u64) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    if inner.jit_state.exclusive_state == 0 {
        return 1;
    }
    inner.jit_state.exclusive_state = 0;
    if let Some(monitor) = inner.global_monitor {
        let callbacks = &mut inner.callbacks;
        return if unsafe {
            (&mut *monitor).do_exclusive_operation(inner.processor_id, vaddr, |expected: u64| {
                callbacks.memory_write_exclusive_64(vaddr, value, expected)
            })
        } {
            0
        } else {
            1
        };
    }
    let expected = inner.exclusive_value[0];
    if inner
        .callbacks
        .memory_write_exclusive_64(vaddr, value, expected)
    {
        0
    } else {
        1
    }
}

fn exclusive_write_128_impl(inner_ptr: u64, vaddr: u64, value_lo: u64, value_hi: u64) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    if inner.jit_state.exclusive_state == 0 {
        return 1;
    }
    inner.jit_state.exclusive_state = 0;
    if let Some(monitor) = inner.global_monitor {
        let callbacks = &mut inner.callbacks;
        return if unsafe {
            (&mut *monitor).do_exclusive_operation(
                inner.processor_id,
                vaddr,
                |expected: [u64; 2]| {
                    callbacks.memory_write_exclusive_128(vaddr, [value_lo, value_hi], expected)
                },
            )
        } {
            0
        } else {
            1
        };
    }
    let expected_lo = inner.exclusive_value[0];
    let expected_hi = inner.exclusive_value[1];
    if inner.callbacks.memory_write_exclusive_128(
        vaddr,
        [value_lo, value_hi],
        [expected_lo, expected_hi],
    ) {
        0
    } else {
        1
    }
}

#[cfg(not(target_os = "windows"))]
extern "C" fn exclusive_write_128_trampoline(
    inner_ptr: u64,
    vaddr: u64,
    value_lo: u64,
    value_hi: u64,
) -> u64 {
    exclusive_write_128_impl(inner_ptr, vaddr, value_lo, value_hi)
}

#[cfg(target_os = "windows")]
extern "C" fn exclusive_write_128_trampoline(
    inner_ptr: u64,
    vaddr: u64,
    value: *const Pair128,
) -> u64 {
    let value = unsafe { value.read_unaligned() };
    exclusive_write_128_impl(inner_ptr, vaddr, value.lo, value.hi)
}

extern "C" fn raw_exclusive_write_8_trampoline(
    inner_ptr: u64,
    vaddr: u64,
    value: u64,
    expected: u64,
) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    inner
        .callbacks
        .memory_write_exclusive_8(vaddr, value as u8, expected as u8) as u64
}

extern "C" fn raw_exclusive_write_16_trampoline(
    inner_ptr: u64,
    vaddr: u64,
    value: u64,
    expected: u64,
) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    inner
        .callbacks
        .memory_write_exclusive_16(vaddr, value as u16, expected as u16) as u64
}

extern "C" fn raw_exclusive_write_32_trampoline(
    inner_ptr: u64,
    vaddr: u64,
    value: u64,
    expected: u64,
) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    inner
        .callbacks
        .memory_write_exclusive_32(vaddr, value as u32, expected as u32) as u64
}

extern "C" fn raw_exclusive_write_64_trampoline(
    inner_ptr: u64,
    vaddr: u64,
    value: u64,
    expected: u64,
) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    inner
        .callbacks
        .memory_write_exclusive_64(vaddr, value, expected) as u64
}

extern "C" fn raw_exclusive_write_128_trampoline(
    inner_ptr: u64,
    vaddr: u64,
    value: *const [u64; 2],
    expected: *const [u64; 2],
) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    let value = unsafe { *value };
    let expected = unsafe { *expected };
    inner
        .callbacks
        .memory_write_exclusive_128(vaddr, value, expected) as u64
}

// ===========================================================================
// A32 JIT
// ===========================================================================
