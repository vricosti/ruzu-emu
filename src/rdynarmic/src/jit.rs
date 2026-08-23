use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::backend::common::a32_callbacks;
use crate::backend::x64::a32_emit_x64::A32EmitX64;
use crate::backend::x64::a64_emit_x64::A64EmitX64;
use crate::backend::x64::block_of_code::{RunCodeCallbacks, RunCodeFn, DEFAULT_CODE_SIZE};
use crate::backend::x64::callback::ArgCallback;
use crate::backend::x64::emit_context::{EmitCallbacks, EmitConfig, RawExclusiveWriteCallbacks};
use crate::backend::x64::jit_state::{A32JitState, A64JitState};
use crate::frontend::a32::translate::translate_callbacks::UserCallbacksAdapter;
use crate::frontend::a32::translate::TranslationOptions as A32TranslationOptions;
use crate::frontend::a64::translate::TranslationOptions;
use crate::halt_reason::HaltReason;
use crate::interface::a32::config::{
    UserCallbacks as A32UserCallbacks, UserConfig as A32UserConfig,
};
use crate::ir::location::LocationDescriptor;
#[cfg(test)]
use crate::jit_config::OptimizationFlag;
use crate::jit_config::{JitConfig, UserCallbacks};

/// Public ARM64 JIT compiler.
///
/// This is the main entry point for consumers (e.g., ruzu). Create one
/// per CPU core, configure callbacks, then call `run()` or `step()`.
pub struct A64Jit {
    inner: Box<JitInner>,
    #[cfg(target_arch = "aarch64")]
    arm64: Option<crate::backend::arm64::a64_interface::A64Interface>,
}

/// Internal JIT state. Box'd for stable heap pointer used by callback trampolines.
struct JitInner {
    jit_state: A64JitState,
    emitter: Option<A64EmitX64>,
    callbacks: Box<dyn UserCallbacks>,
    run_code_fn: Option<RunCodeFn>,
    is_executing: bool,
    global_monitor: Option<*mut crate::exclusive_monitor::ExclusiveMonitor>,
    processor_id: usize,
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
pub(crate) fn block_count_range() -> Option<(u32, u32)> {
    use std::sync::OnceLock;
    static RANGE: OnceLock<Option<(u32, u32)>> = OnceLock::new();
    *RANGE.get_or_init(|| {
        let raw = std::env::var("RUZU_BLOCK_COUNT_PC").ok()?;
        let mut parts = raw.splitn(2, '-');
        let lo = u32::from_str_radix(parts.next()?.trim_start_matches("0x"), 16).ok()?;
        let hi = u32::from_str_radix(parts.next()?.trim_start_matches("0x"), 16).ok()?;
        Some((lo, hi))
    })
}

/// Atomic per-core counters keyed by emulator core index (0..=15 supported).
pub(crate) fn block_count_counters() -> &'static [std::sync::atomic::AtomicU64; 16] {
    use std::sync::atomic::AtomicU64;
    use std::sync::OnceLock;
    static COUNTERS: OnceLock<[AtomicU64; 16]> = OnceLock::new();
    COUNTERS.get_or_init(|| {
        [
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
        ]
    })
}

/// `RUZU_BLOCK_PROLOGUE_COUNT_PC=0xLO-0xHI` — bypass FAST_DISPATCH chaining
/// by emitting an inline `lock inc` at every block prologue whose entry PC
/// is in the range. Counter address is fixed per emulator core at JIT-emit
/// time. Reveals which cores are actively executing guest code in a PC
/// window, which slow-path and cold-entry probes cannot.
pub fn block_prologue_count_range() -> Option<(u32, u32)> {
    use std::sync::OnceLock;
    static RANGE: OnceLock<Option<(u32, u32)>> = OnceLock::new();
    *RANGE.get_or_init(|| {
        let raw = std::env::var("RUZU_BLOCK_PROLOGUE_COUNT_PC").ok()?;
        let mut parts = raw.splitn(2, '-');
        let lo = u32::from_str_radix(parts.next()?.trim_start_matches("0x"), 16).ok()?;
        let hi = u32::from_str_radix(parts.next()?.trim_start_matches("0x"), 16).ok()?;
        Some((lo, hi))
    })
}

/// Per-core block-prologue hit counters. Public so the JIT can take a stable
/// `*const AtomicU64` address per slot at emit-time.
pub fn block_prologue_counters() -> &'static [std::sync::atomic::AtomicU64; 16] {
    use std::sync::atomic::AtomicU64;
    use std::sync::OnceLock;
    static COUNTERS: OnceLock<[AtomicU64; 16]> = OnceLock::new();
    COUNTERS.get_or_init(|| {
        [
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
        ]
    })
}

/// `RUZU_BLOCK_PROLOGUE_TOP_PC=0xLO-0xHI` — like
/// `RUZU_BLOCK_PROLOGUE_COUNT_PC`, but keeps one per-core counter per A32 block
/// entry PC. The emitted code increments the PC-specific counter directly, so
/// this remains useful when direct block linking bypasses the dispatcher.
pub fn block_prologue_top_range() -> Option<(u32, u32)> {
    use std::sync::OnceLock;
    static RANGE: OnceLock<Option<(u32, u32)>> = OnceLock::new();
    *RANGE.get_or_init(|| {
        let raw = std::env::var("RUZU_BLOCK_PROLOGUE_TOP_PC").ok()?;
        let mut parts = raw.splitn(2, '-');
        let lo = u32::from_str_radix(parts.next()?.trim_start_matches("0x"), 16).ok()?;
        let hi = u32::from_str_radix(parts.next()?.trim_start_matches("0x"), 16).ok()?;
        Some((lo, hi))
    })
}

fn block_prologue_top_entries() -> &'static Mutex<HashMap<u32, Box<[AtomicU64; 16]>>> {
    static ENTRIES: OnceLock<Mutex<HashMap<u32, Box<[AtomicU64; 16]>>>> = OnceLock::new();
    ENTRIES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Returns a stable counter address for `(pc, core)` when top-PC prologue
/// counting is enabled for the PC range. Called at JIT emit time, never from
/// generated guest code.
pub fn block_prologue_top_counter(pc: u32, core: usize) -> Option<u64> {
    let (lo, hi) = block_prologue_top_range()?;
    if pc < lo || pc >= hi {
        return None;
    }
    let core = core.min(15);
    let mut entries = block_prologue_top_entries().lock().ok()?;
    let counters = entries
        .entry(pc)
        .or_insert_with(|| Box::new(std::array::from_fn(|_| AtomicU64::new(0))));
    Some(&counters[core] as *const AtomicU64 as u64)
}

fn block_prologue_top_limit() -> usize {
    use std::sync::OnceLock;
    static LIMIT: OnceLock<usize> = OnceLock::new();
    *LIMIT.get_or_init(|| {
        std::env::var("RUZU_BLOCK_PROLOGUE_TOP_N")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(32)
            .clamp(1, 512)
    })
}

/// `RUZU_FIRST_PCS_PER_CORE=N` — capture the first N distinct block-entry
/// PCs observed per emulator core, recorded at cold-entry trace time
/// (`a64_trace_block_entry`). No eprintln; written to in-memory atomics
/// so the multi-core race timing window stays intact. Dump via
/// `first_pcs_per_core_summary_string()`.
const FIRST_PCS_MAX: usize = 4096;

fn first_pcs_capacity() -> usize {
    use std::sync::OnceLock;
    static CAP: OnceLock<usize> = OnceLock::new();
    *CAP.get_or_init(|| {
        std::env::var("RUZU_FIRST_PCS_PER_CORE")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0)
            .min(FIRST_PCS_MAX)
    })
}

fn first_pcs_buffers() -> &'static [[std::sync::atomic::AtomicU64; FIRST_PCS_MAX]; 16] {
    use std::sync::atomic::AtomicU64;
    use std::sync::OnceLock;
    // Heap-boxed: [AtomicU64; 4096] * 16 = 512 KB; constructing on the
    // stack first (the natural OnceLock init) blows the default 256 KB
    // worker-thread stack and SIGSEGVs.
    static BUFS: OnceLock<Box<[[AtomicU64; FIRST_PCS_MAX]; 16]>> = OnceLock::new();
    BUFS.get_or_init(|| {
        let mut v: Vec<[AtomicU64; FIRST_PCS_MAX]> = Vec::with_capacity(16);
        for _ in 0..16 {
            v.push(std::array::from_fn(|_| AtomicU64::new(0)));
        }
        let boxed: Box<[[AtomicU64; FIRST_PCS_MAX]; 16]> =
            v.into_boxed_slice().try_into().unwrap_or_else(|_| {
                // Should be impossible — we just pushed exactly 16.
                panic!("first_pcs_buffers: vec→array conversion failed");
            });
        boxed
    })
}

fn first_pcs_lengths() -> &'static [std::sync::atomic::AtomicUsize; 16] {
    use std::sync::atomic::AtomicUsize;
    use std::sync::OnceLock;
    static LENS: OnceLock<[AtomicUsize; 16]> = OnceLock::new();
    LENS.get_or_init(|| std::array::from_fn(|_| AtomicUsize::new(0)))
}

/// Record a block-entry PC for the given core. Skips writing when the new
/// PC equals the most recently captured one (cheap consecutive-dedup) so the
/// fixed-cap buffer doesn't fill up with `0xADDR ×N` from a hot self-cold-
/// entered loop. Not a full dedup — non-adjacent repeats still appear.
fn record_first_pc(core_index: usize, pc: u64) {
    let cap = first_pcs_capacity();
    if cap == 0 {
        return;
    }
    let idx = core_index.min(15);
    let len_atom = &first_pcs_lengths()[idx];
    let bufs = first_pcs_buffers();
    let cur = len_atom.load(std::sync::atomic::Ordering::Relaxed);
    if cur > 0 && cur <= cap {
        let last = bufs[idx][cur - 1].load(std::sync::atomic::Ordering::Relaxed);
        if last == pc {
            return;
        }
    }
    let pos = len_atom.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if pos < cap {
        bufs[idx][pos].store(pc, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Dump the captured first-N-PCs-per-core ring buffers as a string.
pub fn first_pcs_per_core_summary_string() -> String {
    use std::fmt::Write;
    let cap = first_pcs_capacity();
    if cap == 0 {
        return String::new();
    }
    let mut out = String::with_capacity(4096);
    out.push_str("[FIRST_PCS_PER_CORE] (cap=");
    let _ = write!(out, "{}", cap);
    out.push_str(", entries are first-seen, may include duplicates):");
    let lens = first_pcs_lengths();
    let bufs = first_pcs_buffers();
    let mut any = false;
    for i in 0..16 {
        let len = lens[i].load(std::sync::atomic::Ordering::Relaxed).min(cap);
        if len == 0 {
            continue;
        }
        any = true;
        let _ = write!(out, "\n  core={} ({} pcs):", i, len);
        for j in 0..len {
            let pc = bufs[i][j].load(std::sync::atomic::Ordering::Relaxed);
            if j % 8 == 0 {
                out.push_str("\n    ");
            }
            let _ = write!(out, "0x{:08X} ", pc);
        }
    }
    if !any {
        out.push_str("\n  (no entries)");
    }
    out
}

/// String form of the per-core block-prologue counter snapshot. Mirrors
/// `block_count_summary_string()` shape; safe to call from any thread.
pub fn block_prologue_count_summary_string() -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(256);
    if block_prologue_count_range().is_none() {
        return out;
    }
    out.push_str("[BLOCK_PROLOGUE_COUNT_SUMMARY] hits per emulator core in PC range:");
    let mut any = false;
    for (i, c) in block_prologue_counters().iter().enumerate() {
        let v = c.load(std::sync::atomic::Ordering::Relaxed);
        if v > 0 {
            let _ = write!(out, "\n  core={}: {}", i, v);
            any = true;
        }
    }
    if !any {
        out.push_str("\n  (no hits)");
    }
    out
}

/// String form of the hottest A32 block-entry PCs observed by the
/// PC-specific prologue counters.
pub fn block_prologue_top_summary_string() -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(2048);
    if block_prologue_top_range().is_none() {
        return out;
    }

    let Ok(entries) = block_prologue_top_entries().lock() else {
        out.push_str("[BLOCK_PROLOGUE_TOP_SUMMARY] unavailable: lock poisoned");
        return out;
    };

    let mut rows: Vec<(u64, u32, [u64; 16])> = entries
        .iter()
        .map(|(&pc, counters)| {
            let per_core = std::array::from_fn(|index| counters[index].load(Ordering::Relaxed));
            let total = per_core.iter().sum();
            (total, pc, per_core)
        })
        .filter(|(total, _, _)| *total > 0)
        .collect();
    rows.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

    let _ = write!(
        out,
        "[BLOCK_PROLOGUE_TOP_SUMMARY] top {} A32 block PCs:",
        block_prologue_top_limit()
    );
    if rows.is_empty() {
        out.push_str("\n  (no hits)");
        return out;
    }

    for (rank, (total, pc, per_core)) in rows
        .into_iter()
        .take(block_prologue_top_limit())
        .enumerate()
    {
        let _ = write!(out, "\n  #{:02} pc=0x{:08X} total={}", rank + 1, pc, total);
        for (core, value) in per_core.into_iter().enumerate() {
            if value > 0 {
                let _ = write!(out, " c{}={}", core, value);
            }
        }
    }
    out
}

/// Public — prints the counter snapshot. Call from a signal handler or
/// at-exit hook.
pub fn dump_block_count_summary() {
    if block_count_range().is_none() {
        return;
    }
    eprintln!("{}", block_count_summary_string());
}

/// String form of the block-count snapshot, suitable for writing to a
/// log file when stderr is being flooded.
pub fn block_count_summary_string() -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(256);
    if block_count_range().is_none() {
        return out;
    }
    out.push_str("[BLOCK_COUNT_SUMMARY] hits per emulator core in PC range:");
    let counters = block_count_counters();
    let mut any = false;
    for (i, c) in counters.iter().enumerate() {
        let v = c.load(std::sync::atomic::Ordering::Relaxed);
        if v > 0 {
            let _ = write!(out, "\n  core={}: {}", i, v);
            any = true;
        }
    }
    if !any {
        out.push_str("\n  (no hits)");
    }
    out
}

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

// ---------------------------------------------------------------------------
// A32 per-PC GPR-capture hook (low overhead: only emitted for a configured
// target PC at block-compile time; zero per-read cost). Buffered/aggregated —
// never eprintln! per hit. Enabled by RUZU_A32_PC_TRACE=0xPC.
// ---------------------------------------------------------------------------

/// Target PC for the A32 GPR-capture hook, parsed once from RUZU_A32_PC_TRACE.
pub fn a32_pc_trace_target() -> Option<u64> {
    use std::sync::OnceLock;
    static T: OnceLock<Option<u64>> = OnceLock::new();
    *T.get_or_init(|| {
        std::env::var("RUZU_A32_PC_TRACE").ok().and_then(|s| {
            let s = s.trim();
            let s = s
                .strip_prefix("0x")
                .or_else(|| s.strip_prefix("0X"))
                .unwrap_or(s);
            u64::from_str_radix(s, 16).ok()
        })
    })
}

/// Aggregated capture state: per-(r6 value) hit counts + last seen GPR snapshot.
struct A32PcTraceAgg {
    total: u64,
    by_tag: std::collections::HashMap<u64, u64>,
    by_tag_r0_r1: std::collections::HashMap<(u64, u32, u32), u64>,
    by_tag_r2: std::collections::HashMap<(u64, u32), u64>,
    sum_r2_by_tag: std::collections::HashMap<u64, u64>,
    by_tag_abs0: std::collections::HashMap<(u64, u64), u64>,
    by_r6: std::collections::HashMap<u32, u64>,
    by_r0: std::collections::HashMap<u32, u64>,
    by_r0_lr: std::collections::HashMap<(u32, u32), u64>,
    by_filtered_args: std::collections::HashMap<(u32, u32, u32, u32, u32, u32, u32), u64>,
    last: [u32; 16],
}

fn a32_pc_trace_agg() -> &'static std::sync::Mutex<A32PcTraceAgg> {
    use std::sync::OnceLock;
    static AGG: OnceLock<std::sync::Mutex<A32PcTraceAgg>> = OnceLock::new();
    AGG.get_or_init(|| {
        std::sync::Mutex::new(A32PcTraceAgg {
            total: 0,
            by_tag: std::collections::HashMap::new(),
            by_tag_r0_r1: std::collections::HashMap::new(),
            by_tag_r2: std::collections::HashMap::new(),
            sum_r2_by_tag: std::collections::HashMap::new(),
            by_tag_abs0: std::collections::HashMap::new(),
            by_r6: std::collections::HashMap::new(),
            by_r0: std::collections::HashMap::new(),
            by_r0_lr: std::collections::HashMap::new(),
            by_filtered_args: std::collections::HashMap::new(),
            last: [0u32; 16],
        })
    })
}

/// Optional LR filter for A32_PC_TRACE argument aggregation.
///
/// `RUZU_A32_PC_TRACE_LR_FILTER=0xLR` records the most common
/// `(r0, r1, r2, r3, r4, r5, lr)` tuples for calls that return to LR. This is
/// useful for PLT/helper targets such as `__aeabi_ldivmod`, where the caller
/// PC identifies the site and the arguments are the real signal.
fn a32_pc_trace_lr_filter() -> Option<u32> {
    use std::sync::OnceLock;
    static LR: OnceLock<Option<u32>> = OnceLock::new();
    *LR.get_or_init(|| {
        std::env::var("RUZU_A32_PC_TRACE_LR_FILTER")
            .ok()
            .and_then(|s| {
                let s = s.trim();
                let s = s
                    .strip_prefix("0x")
                    .or_else(|| s.strip_prefix("0X"))
                    .unwrap_or(s);
                u32::from_str_radix(s, 16).ok()
            })
    })
}

/// Optional comma-separated IR instruction indices where the A32 PC trace hook
/// should also be emitted after the instruction in the traced block.
pub fn a32_pc_trace_after_insts() -> &'static Vec<usize> {
    use std::sync::OnceLock;
    static INSTS: OnceLock<Vec<usize>> = OnceLock::new();
    INSTS.get_or_init(|| {
        std::env::var("RUZU_A32_PC_TRACE_AFTER_INST")
            .ok()
            .map(|spec| {
                spec.split(',')
                    .filter_map(|raw| raw.trim().parse::<usize>().ok())
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// Optional guest-memory probe spec from RUZU_A32_PC_TRACE_MEM:
/// "BASEREG+OFF[,...]" where BASEREG is r0..r15 (e.g. "r8+0x54,r8+0x40").
/// Read via fastmem_base (passed from the JIT's R13) so we see live object state.
fn a32_pc_trace_mem_probes() -> &'static Vec<(usize, i64)> {
    use std::sync::OnceLock;
    static P: OnceLock<Vec<(usize, i64)>> = OnceLock::new();
    P.get_or_init(|| {
        let mut v = Vec::new();
        if let Ok(spec) = std::env::var("RUZU_A32_PC_TRACE_MEM") {
            for tok in spec.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
                // form: rN+0xOFF or rN-0xOFF
                let (reg_s, sign, off_s) = if let Some(i) = tok.find('+') {
                    (&tok[..i], 1i64, &tok[i + 1..])
                } else if let Some(i) = tok.find('-') {
                    (&tok[..i], -1i64, &tok[i + 1..])
                } else {
                    (tok, 1i64, "0")
                };
                let reg = reg_s.trim().trim_start_matches('r').parse::<usize>().ok();
                let off = {
                    let o = off_s.trim();
                    let o = o
                        .strip_prefix("0x")
                        .or_else(|| o.strip_prefix("0X"))
                        .unwrap_or(o);
                    i64::from_str_radix(o, 16)
                        .ok()
                        .or_else(|| off_s.trim().parse::<i64>().ok())
                };
                if let (Some(reg), Some(off)) = (reg, off) {
                    if reg < 16 {
                        v.push((reg, sign * off));
                    }
                }
            }
        }
        v
    })
}

/// Optional comma-separated absolute guest-memory probes from
/// RUZU_A32_PC_TRACE_ABS_MEM: "ADDR[:SIZE][,...]".
///
/// SIZE currently accepts 4 or 8 bytes and defaults to 8. Values are read
/// directly from fastmem at the same hook point as the GPR snapshot, which lets
/// us distinguish "guest memory changed" from "the emitted load/register state
/// is corrupt" without adding per-SVC dumps.
fn a32_pc_trace_abs_mem_probes() -> &'static Vec<(u64, usize)> {
    use std::sync::OnceLock;
    static P: OnceLock<Vec<(u64, usize)>> = OnceLock::new();
    P.get_or_init(|| {
        let mut v = Vec::new();
        if let Ok(spec) = std::env::var("RUZU_A32_PC_TRACE_ABS_MEM") {
            for tok in spec.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
                let (addr_s, size_s) = tok.split_once(':').unwrap_or((tok, "8"));
                let parse_hex_or_dec = |raw: &str| {
                    let raw = raw.trim();
                    let raw_hex = raw
                        .strip_prefix("0x")
                        .or_else(|| raw.strip_prefix("0X"))
                        .unwrap_or(raw);
                    u64::from_str_radix(raw_hex, 16)
                        .ok()
                        .or_else(|| raw.parse::<u64>().ok())
                };
                let Some(addr) = parse_hex_or_dec(addr_s) else {
                    continue;
                };
                let size = size_s.trim().parse::<usize>().unwrap_or(8);
                if matches!(size, 4 | 8) {
                    v.push((addr & 0xFFFF_FFFF, size));
                }
            }
        }
        v
    })
}

fn a32_pc_trace_first_hits() -> u64 {
    use std::sync::OnceLock;
    static N: OnceLock<u64> = OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("RUZU_A32_PC_TRACE_FIRST_HITS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(1)
    })
}

fn parse_optional_u32_env(name: &str) -> Option<u32> {
    std::env::var(name).ok().and_then(|s| {
        let s = s.trim();
        let s = s
            .strip_prefix("0x")
            .or_else(|| s.strip_prefix("0X"))
            .unwrap_or(s);
        u32::from_str_radix(s, 16)
            .ok()
            .or_else(|| s.parse::<u32>().ok())
    })
}

fn a32_pc_trace_match_r0() -> Option<u32> {
    use std::sync::OnceLock;
    static VALUE: OnceLock<Option<u32>> = OnceLock::new();
    *VALUE.get_or_init(|| parse_optional_u32_env("RUZU_A32_PC_TRACE_MATCH_R0"))
}

fn a32_pc_trace_match_r1() -> Option<u32> {
    use std::sync::OnceLock;
    static VALUE: OnceLock<Option<u32>> = OnceLock::new();
    *VALUE.get_or_init(|| parse_optional_u32_env("RUZU_A32_PC_TRACE_MATCH_R1"))
}

fn a32_pc_trace_match_r2() -> Option<u32> {
    use std::sync::OnceLock;
    static VALUE: OnceLock<Option<u32>> = OnceLock::new();
    *VALUE.get_or_init(|| parse_optional_u32_env("RUZU_A32_PC_TRACE_MATCH_R2"))
}

fn a32_pc_trace_match_r3() -> Option<u32> {
    use std::sync::OnceLock;
    static VALUE: OnceLock<Option<u32>> = OnceLock::new();
    *VALUE.get_or_init(|| parse_optional_u32_env("RUZU_A32_PC_TRACE_MATCH_R3"))
}

fn a32_pc_trace_match_r10() -> Option<u32> {
    use std::sync::OnceLock;
    static VALUE: OnceLock<Option<u32>> = OnceLock::new();
    *VALUE.get_or_init(|| parse_optional_u32_env("RUZU_A32_PC_TRACE_MATCH_R10"))
}

fn a32_pc_trace_match_r11() -> Option<u32> {
    use std::sync::OnceLock;
    static VALUE: OnceLock<Option<u32>> = OnceLock::new();
    *VALUE.get_or_init(|| parse_optional_u32_env("RUZU_A32_PC_TRACE_MATCH_R11"))
}

fn a32_pc_trace_match_log_limit() -> u64 {
    use std::sync::OnceLock;
    static N: OnceLock<u64> = OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("RUZU_A32_PC_TRACE_MATCH_LIMIT")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(64)
    })
}

fn a32_pc_trace_match_count() -> &'static std::sync::atomic::AtomicU64 {
    static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    &COUNT
}

/// Called from JIT-emitted code at block entry for the target PC.
/// `jit_state_ptr` = A32JitState (R15); `fastmem_base` = R13 (guest→host base).
/// Reads the 16 GPRs, tallies r0/r6, and (if RUZU_A32_PC_TRACE_MEM set) reads
/// guest memory at [rN+off] via fastmem_base. Buffered aggregate; no per-hit I/O.
pub(crate) extern "C" fn a32_pc_trace_hook(jit_state_ptr: u64, fastmem_base: u64, tag: u64) {
    if a32_pc_trace_target().is_none() {
        return;
    }
    // The x64 and arm64 backends intentionally mirror their upstream JitState
    // layouts, which are not identical. Read the active host backend layout.
    #[cfg(target_arch = "aarch64")]
    let state =
        unsafe { &*(jit_state_ptr as *const crate::backend::arm64::jit_state::A32JitState) };
    #[cfg(not(target_arch = "aarch64"))]
    let state = unsafe { &*(jit_state_ptr as *const crate::backend::x64::jit_state::A32JitState) };
    #[cfg(target_arch = "aarch64")]
    let regs = &state.regs;
    #[cfg(not(target_arch = "aarch64"))]
    let regs = &state.reg;
    #[cfg(target_arch = "aarch64")]
    let ext_regs = &state.ext_regs.0;
    #[cfg(not(target_arch = "aarch64"))]
    let ext_regs = &state.ext_reg;
    let r0 = regs[0];
    let r1 = regs[1];
    let r6 = regs[6];
    let lr = regs[14];

    let match_r0 = a32_pc_trace_match_r0();
    let match_r1 = a32_pc_trace_match_r1();
    let match_r2 = a32_pc_trace_match_r2();
    let match_r3 = a32_pc_trace_match_r3();
    let match_r10 = a32_pc_trace_match_r10();
    let match_r11 = a32_pc_trace_match_r11();
    let r0_matches = match_r0.is_some_and(|expected| expected == r0);
    let r1_matches = match_r1.is_some_and(|expected| expected == r1);
    let r2_matches = match_r2.is_some_and(|expected| expected == regs[2]);
    let r3_matches = match_r3.is_some_and(|expected| expected == regs[3]);
    let r10_matches = match_r10.is_some_and(|expected| expected == regs[10]);
    let r11_matches = match_r11.is_some_and(|expected| expected == regs[11]);
    if r0_matches || r1_matches || r2_matches || r3_matches || r10_matches || r11_matches {
        let hit = a32_pc_trace_match_count().fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        if hit <= a32_pc_trace_match_log_limit() || hit.is_power_of_two() {
            log::warn!(
                "[A32_PC_MATCH] hit={} tag=0x{:08X} r0=0x{:08X} r1=0x{:08X} r2=0x{:08X} r3=0x{:08X} r4=0x{:08X} r5=0x{:08X} r6=0x{:08X} r7=0x{:08X} r8=0x{:08X} r9=0x{:08X} r10=0x{:08X} r11=0x{:08X} lr=0x{:08X}",
                hit,
                tag as u32,
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
                regs[14],
            );
            log::warn!(
                "[A32_FP_STATE] hit={} tag=0x{:08X} upper=0x{:08X} s0=0x{:08X} s4=0x{:08X} s5=0x{:08X} s10=0x{:08X} s12=0x{:08X} s13=0x{:08X} s15=0x{:08X}",
                hit,
                tag as u32,
                state.upper_location_descriptor,
                ext_regs[0],
                ext_regs[4],
                ext_regs[5],
                ext_regs[10],
                ext_regs[12],
                ext_regs[13],
                ext_regs[15],
            );
        }
    }

    // Read guest-memory probes via fastmem base (zero-extend guest vaddr).
    let probes = a32_pc_trace_mem_probes();
    let mut mem_vals: Vec<(usize, i64, u32)> = Vec::new();
    let abs_probes = a32_pc_trace_abs_mem_probes();
    let mut abs_vals: Vec<(u64, usize, u64)> = Vec::new();
    if fastmem_base != 0 {
        for &(reg, off) in probes.iter() {
            let gaddr = (regs[reg] as i64).wrapping_add(off) as u64 & 0xFFFF_FFFF;
            let host = fastmem_base.wrapping_add(gaddr) as *const u32;
            // SAFETY: fastmem arena is mapped (PROT_NONE for unmapped pages → could
            // fault). Guard against obviously-bad guest addrs.
            let val = if gaddr >= 0x1000 {
                unsafe { host.read_volatile() }
            } else {
                0xDEAD_0000
            };
            mem_vals.push((reg, off, val));
        }
        for &(gaddr, size) in abs_probes.iter() {
            let host = fastmem_base.wrapping_add(gaddr) as *const u8;
            let val = if gaddr >= 0x1000 {
                unsafe {
                    match size {
                        4 => (host as *const u32).read_volatile() as u64,
                        8 => (host as *const u64).read_volatile(),
                        _ => 0xBAD0_0000_0000_0000,
                    }
                }
            } else {
                0xDEAD_0000_0000_0000
            };
            abs_vals.push((gaddr, size, val));
        }
    }

    let mut g = match a32_pc_trace_agg().lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    g.total += 1;
    *g.by_tag.entry(tag).or_insert(0) += 1;
    *g.by_tag_r0_r1.entry((tag, regs[0], regs[1])).or_insert(0) += 1;
    *g.by_tag_r2.entry((tag, regs[2])).or_insert(0) += 1;
    *g.sum_r2_by_tag.entry(tag).or_insert(0) += regs[2] as u64;
    if let Some((_, _, val)) = abs_vals.first() {
        *g.by_tag_abs0.entry((tag, *val)).or_insert(0) += 1;
    }
    *g.by_r6.entry(r6).or_insert(0) += 1;
    *g.by_r0.entry(r0).or_insert(0) += 1;
    *g.by_r0_lr.entry((r0, lr)).or_insert(0) += 1;
    if a32_pc_trace_lr_filter().is_some_and(|target_lr| target_lr == lr) {
        *g.by_filtered_args
            .entry((regs[0], regs[1], regs[2], regs[3], regs[4], regs[5], lr))
            .or_insert(0) += 1;
    }
    g.last = *regs;
    if g.total <= a32_pc_trace_first_hits() || g.total % 500 == 0 {
        let mut r6v: Vec<(u32, u64)> = g.by_r6.iter().map(|(k, v)| (*k, *v)).collect();
        r6v.sort_by(|a, b| b.1.cmp(&a.1));
        let top_r6: Vec<String> = r6v
            .iter()
            .take(5)
            .map(|(k, v)| format!("r6=0x{:X}:{}", k, v))
            .collect();
        let mut r0v: Vec<(u32, u64)> = g.by_r0.iter().map(|(k, v)| (*k, *v)).collect();
        r0v.sort_by(|a, b| b.1.cmp(&a.1));
        let top_r0: Vec<String> = r0v
            .iter()
            .take(5)
            .map(|(k, v)| format!("r0=0x{:X}:{}", k, v))
            .collect();
        let mut r0_lrv: Vec<((u32, u32), u64)> = g.by_r0_lr.iter().map(|(k, v)| (*k, *v)).collect();
        r0_lrv.sort_by(|a, b| b.1.cmp(&a.1));
        let top_r0_lr: Vec<String> = r0_lrv
            .iter()
            .take(6)
            .map(|((r0, lr), v)| format!("r0=0x{:X}/lr=0x{:X}:{}", r0, lr, v))
            .collect();
        let mut argsv: Vec<((u32, u32, u32, u32, u32, u32, u32), u64)> =
            g.by_filtered_args.iter().map(|(k, v)| (*k, *v)).collect();
        argsv.sort_by(|a, b| b.1.cmp(&a.1));
        let top_args: Vec<String> = argsv
            .iter()
            .take(5)
            .map(|((r0, r1, r2, r3, r4, r5, lr), v)| {
                format!(
                    "r0=0x{:X}/r1=0x{:X}/r2=0x{:X}/r3=0x{:X}/r4=0x{:X}/r5=0x{:X}/lr=0x{:X}:{}",
                    r0, r1, r2, r3, r4, r5, lr, v
                )
            })
            .collect();
        let mut tagv: Vec<(u64, u64)> = g.by_tag.iter().map(|(k, v)| (*k, *v)).collect();
        tagv.sort_by(|a, b| a.0.cmp(&b.0));
        let tags: Vec<String> = tagv
            .iter()
            .map(|(tag, v)| {
                if *tag == u64::MAX {
                    format!("entry:{}", v)
                } else {
                    format!("inst{}:{}", tag, v)
                }
            })
            .collect();
        let mut tag_argsv: Vec<((u64, u32, u32), u64)> =
            g.by_tag_r0_r1.iter().map(|(k, v)| (*k, *v)).collect();
        tag_argsv.sort_by(|a, b| b.1.cmp(&a.1));
        let tag_args: Vec<String> = tag_argsv
            .iter()
            .take(10)
            .map(|((tag, r0, r1), v)| {
                let label = if *tag == u64::MAX {
                    "entry".to_string()
                } else {
                    format!("inst{}", tag)
                };
                format!("{}:r0=0x{:X}/r1=0x{:X}:{}", label, r0, r1, v)
            })
            .collect();
        let mut tag_r2v: Vec<((u64, u32), u64)> =
            g.by_tag_r2.iter().map(|(k, v)| (*k, *v)).collect();
        tag_r2v.sort_by(|a, b| b.1.cmp(&a.1));
        let tag_r2: Vec<String> = tag_r2v
            .iter()
            .take(10)
            .map(|((tag, r2), v)| {
                let label = if *tag == u64::MAX {
                    "entry".to_string()
                } else {
                    format!("inst{}", tag)
                };
                format!("{}:r2=0x{:X}:{}", label, r2, v)
            })
            .collect();
        let mut sum_r2v: Vec<(u64, u64)> = g.sum_r2_by_tag.iter().map(|(k, v)| (*k, *v)).collect();
        sum_r2v.sort_by(|a, b| b.1.cmp(&a.1));
        let sum_r2: Vec<String> = sum_r2v
            .iter()
            .take(10)
            .map(|(tag, sum)| {
                let label = if *tag == u64::MAX {
                    "entry".to_string()
                } else {
                    format!("inst{}", tag)
                };
                format!("{}:sum_r2=0x{:X}({})", label, sum, sum)
            })
            .collect();
        let mut tag_abs0v: Vec<((u64, u64), u64)> =
            g.by_tag_abs0.iter().map(|(k, v)| (*k, *v)).collect();
        tag_abs0v.sort_by(|a, b| b.1.cmp(&a.1));
        let tag_abs0: Vec<String> = tag_abs0v
            .iter()
            .take(10)
            .map(|((tag, val), v)| {
                let label = if *tag == u64::MAX {
                    "entry".to_string()
                } else {
                    format!("inst{}", tag)
                };
                format!("{}:abs0=0x{:016X}:{}", label, val, v)
            })
            .collect();
        let mem_str: Vec<String> = mem_vals
            .iter()
            .map(|(reg, off, val)| {
                format!(
                    "[r{}{}{:#x}]=0x{:08X}",
                    reg,
                    if *off < 0 { "-" } else { "+" },
                    off.unsigned_abs(),
                    val
                )
            })
            .collect();
        let abs_mem_str: Vec<String> = abs_vals
            .iter()
            .map(|(addr, size, val)| format!("[0x{:08X}:{}]=0x{:016X}", addr, size, val))
            .collect();
        let l = g.last;
        log::warn!(
            "[A32_PC_TRACE] total={} tags=[{}] top_tag_r0_r1=[{}] top_tag_r2=[{}] sum_r2=[{}] top_tag_abs0=[{}] top_r0=[{}] top_r0_lr=[{}] top_args_lr=[{}] top_r6=[{}] last_tag={} last: r0=0x{:X} r1=0x{:X} r2=0x{:X} r3=0x{:X} r4=0x{:X} r5=0x{:X} r6=0x{:X} r7=0x{:X} r8=0x{:X} r9=0x{:X} r10=0x{:X} r11=0x{:X} lr=0x{:X} s0=0x{:08X} s4=0x{:08X} s5=0x{:08X} s10=0x{:08X} s12=0x{:08X} s13=0x{:08X} s15=0x{:08X} mem=[{}] abs_mem=[{}]",
            g.total, tags.join(" "), tag_args.join(" "), tag_r2.join(" "), sum_r2.join(" "), tag_abs0.join(" "), top_r0.join(" "), top_r0_lr.join(" "), top_args.join(" "), top_r6.join(" "),
            if tag == u64::MAX { "entry".to_string() } else { format!("inst{}", tag) },
            l[0], l[1], l[2], l[3], l[4], l[5], l[6], l[7], l[8], l[9], l[10], l[11], l[14],
            ext_regs[0], ext_regs[4], ext_regs[5], ext_regs[10], ext_regs[12], ext_regs[13], ext_regs[15],
            mem_str.join(" "), abs_mem_str.join(" ")
        );
    }
}

/// Called from env-gated A32 fastmem-write instrumentation when a direct store
/// hits RUZU_TRACE_FASTMEM_W_RANGE. This catches writes that bypass the normal
/// memory_write_* callbacks.
pub(crate) extern "C" fn a32_fastmem_write_trace_hook(
    jit_state_ptr: u64,
    block_pc: u64,
    vaddr: u64,
    bitsize: u64,
    value: u64,
) {
    let regs = unsafe { &*(jit_state_ptr as *const [u32; 16]) };
    static HITS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let hit = HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if hit < 64 || hit.is_power_of_two() {
        log::warn!(
            "[A32_FASTMEM_W_TRACE] hit={} block_pc=0x{:08X} pc=0x{:08X} lr=0x{:08X} vaddr=0x{:08X} bits={} value=0x{:016X} r0=0x{:08X} r1=0x{:08X} r2=0x{:08X} r3=0x{:08X} r4=0x{:08X} r5=0x{:08X} r6=0x{:08X} r7=0x{:08X}",
            hit,
            block_pc as u32,
            regs[15],
            regs[14],
            vaddr as u32,
            bitsize,
            value,
            regs[0],
            regs[1],
            regs[2],
            regs[3],
            regs[4],
            regs[5],
            regs[6],
            regs[7],
        );
    }
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
    pub fn new(config: JitConfig) -> Result<Self, String> {
        #[cfg(target_arch = "aarch64")]
        {
            let arm64 = crate::backend::arm64::a64_interface::A64Interface::new(config)?;
            let inner = Box::new(JitInner {
                jit_state: A64JitState::new(),
                emitter: None,
                callbacks: Box::new(A32DummyCallbacks),
                run_code_fn: None,
                is_executing: false,
                global_monitor: None,
                processor_id: 0,
            });
            return Ok(A64Jit {
                inner,
                arm64: Some(arm64),
            });
        }

        if !cfg!(target_arch = "x86_64") {
            return Err(format!(
                "rdynarmic x64 backend is not executable on host architecture {}",
                std::env::consts::ARCH
            ));
        }

        let cache_size = if config.code_cache_size > 0 {
            config.code_cache_size
        } else {
            DEFAULT_CODE_SIZE
        };
        let effective_optimizations = config.effective_optimizations();

        // Phase 1: Create boxed JitInner with stable heap address
        let mut inner = Box::new(JitInner {
            jit_state: A64JitState::new(),
            emitter: None,
            callbacks: config.callbacks,
            run_code_fn: None,
            is_executing: false,
            global_monitor: config.global_monitor,
            processor_id: config.processor_id,
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
            page_table_pointer: config.page_table_pointer,
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
                let mut m = config.memory.clone();
                // `processor_id` lives at the top level of JitConfig but
                // mirror it into MemoryEmitConfig so the helpers can read
                // it via `ctx.config.memory.processor_id`.
                m.processor_id = config.processor_id;
                m
            },
            global_monitor: config.global_monitor,
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
        emitter.processor_id = config.processor_id;

        // Extract run_code function pointer
        let run_code_fn = unsafe { emitter.get_run_code_fn()? };

        inner.emitter = Some(emitter);
        inner.run_code_fn = Some(run_code_fn);

        Ok(A64Jit {
            inner,
            #[cfg(target_arch = "aarch64")]
            arm64: None,
        })
    }

    /// Execute JIT code until a halt reason is triggered.
    ///
    /// Matches upstream: Run() does RSB check then GetCurrentBlock() then RunCode().
    /// No mprotect on the hot path — only on cache miss (compilation).
    pub fn run(&mut self) -> HaltReason {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_mut() {
            return arm64.run().expect("A64 ARM64 run failed");
        }

        assert!(
            !self.inner.is_executing,
            "Recursive JIT execution not allowed"
        );
        self.inner.is_executing = true;

        let location = LocationDescriptor::new(self.inner.jit_state.get_unique_hash());
        let inner_ptr = &mut *self.inner as *mut JitInner;
        let emitter = self.inner.emitter.as_mut().unwrap();

        // Fast path: block already compiled — no mprotect needed.
        let code_ptr = if let Some(ptr) = emitter.lookup_cached_block(location) {
            ptr
        } else {
            // Slow path: need to compile — toggle W^X protections.
            let read_code = move |vaddr: u64| -> Option<u32> {
                let inner = unsafe { &*inner_ptr };
                inner.callbacks.memory_read_code(vaddr)
            };
            let _ = emitter.make_writable();
            let ptr = emitter.get_or_compile_block(location, &read_code);
            let _ = unsafe { emitter.get_run_code_fn() };
            ptr
        };

        // Use the run_code_fn cached at construction time — no mprotect.
        let run_fn = self.inner.run_code_fn.unwrap();

        // Call the dispatcher
        let halt_bits = unsafe { run_fn(&mut self.inner.jit_state as *mut _, code_ptr) };
        emitter
            .process_pending_fastmem_recompiles()
            .expect("processing A64 fastmem recompiles failed");

        self.inner.is_executing = false;
        HaltReason::from_bits_truncate(halt_bits)
    }

    /// Execute a single instruction (single-step).
    ///
    /// Uses a dedicated step_code entry point that:
    /// - Sets cycle budget to 1
    /// - Atomically sets the STEP bit in halt_reason
    /// - Compiles a single-instruction block (via single_stepping descriptor)
    pub fn step(&mut self) -> HaltReason {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_mut() {
            return arm64.step().expect("A64 ARM64 step failed");
        }

        assert!(
            !self.inner.is_executing,
            "Recursive JIT execution not allowed"
        );
        self.inner.is_executing = true;

        // Build location with single_stepping=true for 1-instruction block
        let a64_loc = crate::ir::location::A64LocationDescriptor::new(
            self.inner.jit_state.pc,
            self.inner.jit_state.fpcr,
            true,
        );
        let location = a64_loc.to_location();

        let inner_ptr = &mut *self.inner as *mut JitInner;

        // Make code writable for compilation
        if let Some(ref mut emitter) = self.inner.emitter {
            let _ = emitter.make_writable();
        }

        let read_code = move |vaddr: u64| -> Option<u32> {
            let inner = unsafe { &*inner_ptr };
            inner.callbacks.memory_read_code(vaddr)
        };

        let code_ptr = self
            .inner
            .emitter
            .as_mut()
            .unwrap()
            .get_or_compile_block(location, &read_code);

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

        self.inner.is_executing = false;
        HaltReason::from_bits_truncate(halt_bits)
    }

    /// Request halt from another thread (or same thread in a callback).
    ///
    /// Thread-safe: uses atomic OR on halt_reason.
    pub fn halt_execution(&self, reason: HaltReason) {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            arm64.halt_execution(reason);
            return;
        }

        let halt_ptr = &self.inner.jit_state.halt_reason as *const u32 as *const AtomicU32;
        let atomic = unsafe { &*halt_ptr };
        atomic.fetch_or(reason.bits(), Ordering::Release);
    }

    /// Read the current halt_reason value (diagnostic).
    pub fn read_halt_reason(&self) -> u32 {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            return arm64.current_halt_reason().bits();
        }

        let halt_ptr = &self.inner.jit_state.halt_reason as *const u32 as *const AtomicU32;
        let atomic = unsafe { &*halt_ptr };
        atomic.load(Ordering::Acquire)
    }

    /// Get the address of halt_reason (diagnostic).
    pub fn halt_reason_ptr(&self) -> *const u32 {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            return arm64.halt_reason_ptr();
        }

        &self.inner.jit_state.halt_reason as *const u32
    }

    /// Get the address of jit_state base (R15 value).
    pub fn jit_state_ptr(&self) -> *const u8 {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            return arm64.jit_state_ptr();
        }

        &self.inner.jit_state as *const _ as *const u8
    }

    /// Clear specific halt reason bits.
    pub fn clear_halt(&self, reason: HaltReason) {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            arm64.clear_halt(reason);
            return;
        }

        let halt_ptr = &self.inner.jit_state.halt_reason as *const u32 as *const AtomicU32;
        let atomic = unsafe { &*halt_ptr };
        atomic.fetch_and(!reason.bits(), Ordering::Release);
    }

    // ---- Register accessors ----

    pub fn get_register(&self, index: usize) -> u64 {
        assert!(index < 31, "Register index out of range (0-30)");
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            return arm64.regs()[index];
        }

        self.inner.jit_state.reg[index]
    }

    pub fn set_register(&mut self, index: usize, value: u64) {
        assert!(index < 31, "Register index out of range (0-30)");
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_mut() {
            arm64.regs_mut()[index] = value;
            return;
        }

        self.inner.jit_state.reg[index] = value;
    }

    pub fn get_pc(&self) -> u64 {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            return arm64.pc();
        }

        self.inner.jit_state.pc
    }

    pub fn set_pc(&mut self, value: u64) {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_mut() {
            arm64.set_pc(value);
            return;
        }

        self.inner.jit_state.pc = value;
    }

    pub fn get_sp(&self) -> u64 {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            return arm64.sp();
        }

        self.inner.jit_state.sp
    }

    pub fn set_sp(&mut self, value: u64) {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_mut() {
            arm64.set_sp(value);
            return;
        }

        self.inner.jit_state.sp = value;
    }

    pub fn get_pstate(&self) -> u32 {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            return arm64.pstate();
        }

        self.inner.jit_state.get_pstate()
    }

    pub fn set_pstate(&mut self, value: u32) {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_mut() {
            arm64.set_pstate(value);
            return;
        }

        self.inner.jit_state.set_pstate(value);
    }

    pub fn get_vector(&self, index: usize) -> (u64, u64) {
        assert!(index < 32, "Vector register index out of range (0-31)");
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            let vec = &arm64.vec_regs().0;
            return (vec[index * 2], vec[index * 2 + 1]);
        }

        let lo = self.inner.jit_state.vec[index * 2];
        let hi = self.inner.jit_state.vec[index * 2 + 1];
        (lo, hi)
    }

    pub fn set_vector(&mut self, index: usize, lo: u64, hi: u64) {
        assert!(index < 32, "Vector register index out of range (0-31)");
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_mut() {
            let vec = &mut arm64.vec_regs_mut().0;
            vec[index * 2] = lo;
            vec[index * 2 + 1] = hi;
            return;
        }

        self.inner.jit_state.vec[index * 2] = lo;
        self.inner.jit_state.vec[index * 2 + 1] = hi;
    }

    pub fn get_fpcr(&self) -> u32 {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            return arm64.fpcr();
        }

        self.inner.jit_state.get_fpcr()
    }

    pub fn set_fpcr(&mut self, value: u32) {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_mut() {
            arm64.set_fpcr(value);
            return;
        }

        self.inner.jit_state.set_fpcr(value);
    }

    pub fn get_fpsr(&self) -> u32 {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            return arm64.fpsr();
        }

        self.inner.jit_state.get_fpsr()
    }

    pub fn set_fpsr(&mut self, value: u32) {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_mut() {
            arm64.set_fpsr(value);
            return;
        }

        self.inner.jit_state.set_fpsr(value);
    }

    pub fn get_tpidr_el0(&self) -> u64 {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            return arm64.tpidr_el0();
        }

        self.inner.jit_state.tpidr_el0
    }

    pub fn set_tpidr_el0(&mut self, value: u64) {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_mut() {
            arm64.set_tpidr_el0(value);
            return;
        }

        self.inner.jit_state.tpidr_el0 = value;
    }

    pub fn get_tpidrro_el0(&self) -> u64 {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            return arm64.tpidrro_el0();
        }

        self.inner.jit_state.tpidrro_el0
    }

    pub fn set_tpidrro_el0(&mut self, value: u64) {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_mut() {
            arm64.set_tpidrro_el0(value);
            return;
        }

        self.inner.jit_state.tpidrro_el0 = value;
    }

    /// Clear exclusive monitor state.
    /// Matching dynarmic's `Jit::ClearExclusiveState()`.
    /// Called before `run()` to ensure no stale exclusive reservation persists.
    pub fn clear_exclusive_state(&mut self) {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_mut() {
            arm64.clear_exclusive_state();
            return;
        }

        self.inner.jit_state.exclusive_state = 0;
    }

    /// Invalidate cached blocks in a memory range.
    pub fn invalidate_cache_range(&mut self, addr: u64, size: u64) {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            arm64.invalidate_cache_range(addr, size as usize);
            return;
        }

        self.inner.jit_state.reset_rsb();
        if let Some(ref mut emitter) = self.inner.emitter {
            emitter.invalidate_range(addr, size);
        }
    }

    /// Clear all cached blocks.
    pub fn clear_cache(&mut self) {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            arm64.clear_cache();
            return;
        }

        self.inner.jit_state.reset_rsb();
        if let Some(ref mut emitter) = self.inner.emitter {
            emitter.clear_cache();
        }
    }
}

impl Drop for A64Jit {
    fn drop(&mut self) {
        #[cfg(target_arch = "aarch64")]
        if self.arm64.is_some() {
            return;
        }

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

    let emitter = inner.emitter.as_mut().unwrap();

    // Fast path: block already compiled — no mprotect needed.
    if let Some(code_ptr) = emitter.lookup_cached_block(location) {
        return code_ptr as u64;
    }

    // Slow path: need to compile — toggle W^X protections.
    let read_code = move |vaddr: u64| -> Option<u32> {
        let inner = unsafe { &*(inner_ptr as *const JitInner) };
        inner.callbacks.memory_read_code(vaddr)
    };

    if std::env::var_os("RUZU_TRACE_A64_COMPILE_PC").is_some() {
        eprintln!(
            "[TRACE_A64_COMPILE_PC] pc=0x{:016X} lr=0x{:016X} sp=0x{:016X}",
            inner.jit_state.pc, inner.jit_state.reg[30], inner.jit_state.sp
        );
    }

    let _ = emitter.make_writable();
    let code_ptr = emitter.get_or_compile_block(location, &read_code);
    let _ = unsafe { emitter.get_run_code_fn() };
    code_ptr as u64
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
    let (lo, hi) = inner.callbacks.memory_read_128(vaddr);
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
fn watch_write_target() -> Option<(u64, u64)> {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Option<(u64, u64)>> = OnceLock::new();
    *CACHE.get_or_init(|| {
        let raw = std::env::var("RUZU_WATCH_WRITE").ok()?;
        let (addr_s, len) = match raw.split_once(':') {
            Some((a, l)) => (a, l.parse::<u64>().ok().unwrap_or(8)),
            None => (raw.as_str(), 8u64),
        };
        let s = addr_s.trim_start_matches("0x");
        let addr = u64::from_str_radix(s, 16).ok()?;
        Some((addr, len))
    })
}

#[inline]
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
    inner.callbacks.memory_write_128(vaddr, value_lo, value_hi);
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
    inner.callbacks.call_supervisor(svc_num as u32);
}

extern "C" fn exception_raised_trampoline(inner_ptr: u64, pc: u64, exception: u64) {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    inner.callbacks.exception_raised(pc, exception);
}

extern "C" fn data_cache_op_trampoline(inner_ptr: u64, op: u64, vaddr: u64) {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    inner.callbacks.data_cache_operation(op, vaddr);
}

extern "C" fn instruction_cache_op_trampoline(inner_ptr: u64, op: u64, vaddr: u64) {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    inner.callbacks.instruction_cache_operation(op, vaddr);
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
    inner.jit_state.exclusive_value[0] = value as u64;
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
    inner.jit_state.exclusive_value[0] = value as u64;
    value as u64
}

extern "C" fn exclusive_read_32_trampoline(inner_ptr: u64, vaddr: u64) -> u64 {
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
    inner.jit_state.exclusive_value[0] = value as u64;
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
    inner.jit_state.exclusive_value[0] = value;
    value
}

fn exclusive_read_128_impl(inner_ptr: u64, vaddr: u64) -> Pair128 {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    inner.jit_state.exclusive_state = 1;
    let (lo, hi) = if let Some(monitor) = inner.global_monitor {
        let callbacks = &mut inner.callbacks;
        let value: [u64; 2] = unsafe {
            (&mut *monitor).read_and_mark(inner.processor_id, vaddr, || {
                let (lo, hi) = callbacks.memory_read_128(vaddr);
                [lo, hi]
            })
        };
        (value[0], value[1])
    } else {
        inner.callbacks.memory_read_128(vaddr)
    };
    inner.jit_state.exclusive_value[0] = lo;
    inner.jit_state.exclusive_value[1] = hi;
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
                callbacks.exclusive_write_8(vaddr, value as u8, expected)
            })
        } {
            0
        } else {
            1
        };
    }
    let expected = inner.jit_state.exclusive_value[0] as u8;
    if inner
        .callbacks
        .exclusive_write_8(vaddr, value as u8, expected)
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
                callbacks.exclusive_write_16(vaddr, value as u16, expected)
            })
        } {
            0
        } else {
            1
        };
    }
    let expected = inner.jit_state.exclusive_value[0] as u16;
    if inner
        .callbacks
        .exclusive_write_16(vaddr, value as u16, expected)
    {
        0
    } else {
        1
    }
}

extern "C" fn exclusive_write_32_trampoline(inner_ptr: u64, vaddr: u64, value: u64) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    if inner.jit_state.exclusive_state == 0 {
        return 1;
    }
    inner.jit_state.exclusive_state = 0;
    if let Some(monitor) = inner.global_monitor {
        let callbacks = &mut inner.callbacks;
        return if unsafe {
            (&mut *monitor).do_exclusive_operation(inner.processor_id, vaddr, |expected: u32| {
                callbacks.exclusive_write_32(vaddr, value as u32, expected)
            })
        } {
            0
        } else {
            1
        };
    }
    let expected = inner.jit_state.exclusive_value[0] as u32;
    if inner
        .callbacks
        .exclusive_write_32(vaddr, value as u32, expected)
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
                callbacks.exclusive_write_64(vaddr, value, expected)
            })
        } {
            0
        } else {
            1
        };
    }
    let expected = inner.jit_state.exclusive_value[0];
    if inner.callbacks.exclusive_write_64(vaddr, value, expected) {
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
                    callbacks.exclusive_write_128(
                        vaddr,
                        value_lo,
                        value_hi,
                        expected[0],
                        expected[1],
                    )
                },
            )
        } {
            0
        } else {
            1
        };
    }
    let expected_lo = inner.jit_state.exclusive_value[0];
    let expected_hi = inner.jit_state.exclusive_value[1];
    if inner
        .callbacks
        .exclusive_write_128(vaddr, value_lo, value_hi, expected_lo, expected_hi)
    {
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
        .exclusive_write_8(vaddr, value as u8, expected as u8) as u64
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
        .exclusive_write_16(vaddr, value as u16, expected as u16) as u64
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
        .exclusive_write_32(vaddr, value as u32, expected as u32) as u64
}

extern "C" fn raw_exclusive_write_64_trampoline(
    inner_ptr: u64,
    vaddr: u64,
    value: u64,
    expected: u64,
) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut JitInner) };
    inner.callbacks.exclusive_write_64(vaddr, value, expected) as u64
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
        .exclusive_write_128(vaddr, value[0], value[1], expected[0], expected[1]) as u64
}

// ===========================================================================
// A32 JIT
// ===========================================================================

/// Public ARM32 JIT compiler.
///
/// Same design as `A64Jit` but uses A32 frontend (ARM/Thumb decoder),
/// A32JitState (16 × u32 GPRs, split CPSR, ext_reg array), and
/// A32EmitX64 compilation pipeline.
pub struct A32Jit {
    inner: Box<A32JitInner>,
    #[cfg(target_arch = "aarch64")]
    arm64: Option<crate::backend::arm64::a32_interface::A32Interface>,
    #[cfg(target_arch = "aarch64")]
    arm64_cntpct: u64,
}

struct A32JitInner {
    jit_state: A32JitState,
    emitter: Option<A32EmitX64>,
    callbacks: Box<dyn A32UserCallbacks>,
    run_code_fn: Option<RunCodeFn>,
    is_executing: bool,
    global_monitor: Option<*mut crate::exclusive_monitor::ExclusiveMonitor>,
    processor_id: usize,
}

#[cfg(target_arch = "aarch64")]
struct A32DummyCallbacks;

#[cfg(target_arch = "aarch64")]
impl A32UserCallbacks for A32DummyCallbacks {
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

    fn memory_write_exclusive_8(&mut self, _vaddr: u32, _value: u8, _expected: u8) -> bool {
        false
    }

    fn memory_write_exclusive_16(&mut self, _vaddr: u32, _value: u16, _expected: u16) -> bool {
        false
    }

    fn memory_write_exclusive_32(&mut self, _vaddr: u32, _value: u32, _expected: u32) -> bool {
        false
    }

    fn memory_write_exclusive_64(&mut self, _vaddr: u32, _value: u64, _expected: u64) -> bool {
        false
    }

    fn call_svc(&mut self, _svc_num: u32) {}

    fn exception_raised(&mut self, _pc: u32, _exception: crate::interface::a32::config::Exception) {
    }

    fn add_ticks(&mut self, _ticks: u64) {}

    fn get_ticks_remaining(&self) -> u64 {
        0
    }
}

#[cfg(target_arch = "aarch64")]
impl UserCallbacks for A32DummyCallbacks {
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
    fn call_supervisor(&mut self, _svc_num: u32) {}
    fn exception_raised(&mut self, _pc: u64, _exception: u64) {}
    fn add_ticks(&mut self, _ticks: u64) {}
    fn get_ticks_remaining(&self) -> u64 {
        0
    }
}

impl A32Jit {
    /// Diagnostic: write the ARM64 backend's emitted-block map
    /// (`host_entry guest_descriptor size` per line) to `path`. No-op on
    /// backends without a native block map.
    pub fn dump_jit_block_map(&self, path: &str) -> std::io::Result<()> {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
            return arm64.dump_block_map(&mut file);
        }
        let _ = path;
        Ok(())
    }

    /// Create a new A32Jit from the given configuration.
    pub fn new(config: impl Into<A32UserConfig>) -> Result<Self, String> {
        let config = config.into();
        #[cfg(target_arch = "aarch64")]
        {
            let arm64 = crate::backend::arm64::a32_interface::A32Interface::new(config)?;
            let inner = Box::new(A32JitInner {
                jit_state: A32JitState::new(),
                emitter: None,
                callbacks: Box::new(A32DummyCallbacks),
                run_code_fn: None,
                is_executing: false,
                global_monitor: None,
                processor_id: 0,
            });
            return Ok(A32Jit {
                inner,
                arm64: Some(arm64),
                arm64_cntpct: 0,
            });
        }

        if !cfg!(target_arch = "x86_64") {
            return Err(format!(
                "rdynarmic x64 backend is not executable on host architecture {}",
                std::env::consts::ARCH
            ));
        }

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
            is_executing: false,
            global_monitor: config.global_monitor,
            processor_id: config.processor_id as usize,
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
                fastmem_exclusive_access: config.fastmem_exclusive_access
                    && config.fastmem_pointer.is_some()
                    && config.global_monitor.is_some(),
                recompile_on_exclusive_fastmem_failure: config
                    .recompile_on_exclusive_fastmem_failure,
                recompile_on_fastmem_failure: config.recompile_on_fastmem_failure,
                page_table_present: config.page_table.is_some(),
                page_table_address_space_bits: 32,
                silently_mirror_page_table: true,
                absolute_offset_page_table: config.absolute_offset_page_table,
                page_table_pointer_mask_bits: config.page_table_pointer_mask_bits as u32,
                detect_misaligned_access_via_page_table: config
                    .detect_misaligned_access_via_page_table
                    as u32,
                only_detect_misalignment_via_page_table_on_page_boundary: config
                    .only_detect_misalignment_via_page_table_on_page_boundary,
                check_halt_on_memory_access: config.check_halt_on_memory_access,
                processor_id: config.processor_id as usize,
            },
            global_monitor: config.global_monitor,
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

        Ok(A32Jit {
            inner,
            #[cfg(target_arch = "aarch64")]
            arm64: None,
            #[cfg(target_arch = "aarch64")]
            arm64_cntpct: 0,
        })
    }

    /// Execute JIT code until a halt reason is triggered.
    pub fn run(&mut self) -> HaltReason {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_mut() {
            return arm64.run().expect("A32 ARM64 run failed");
        }

        assert!(
            !self.inner.is_executing,
            "Recursive JIT execution not allowed"
        );
        self.inner.is_executing = true;

        // Upstream: Run() does RSB check then GetCurrentBlock() then RunCode().
        // GetCurrentBlock() is a cache lookup (no mprotect). Only on cache miss
        // does it compile (with EnableWriting/DisableWriting inside Emit()).
        // RunCode() just calls the stored function pointer — no mprotect ever.
        let location = LocationDescriptor::new(self.inner.jit_state.get_unique_hash());
        let callbacks_ptr = self.inner.callbacks.as_ref() as *const dyn A32UserCallbacks;
        let emitter = self.inner.emitter.as_mut().unwrap();

        // Fast path: block already compiled — no mprotect needed.
        let code_ptr = if let Some(ptr) = emitter.lookup_cached_block(location) {
            ptr
        } else {
            // Slow path: need to compile — toggle W^X protections.
            let callbacks = unsafe { &*callbacks_ptr };
            let translate_callbacks = UserCallbacksAdapter::new(callbacks);
            let is_read_only =
                move |vaddr: u32| -> bool { unsafe { &*callbacks_ptr }.is_read_only_memory(vaddr) };
            let _ = emitter.make_writable();
            let ptr =
                emitter.get_or_compile_block_with_ro(location, &translate_callbacks, &is_read_only);
            // Restore RX after compilation.
            let _ = unsafe { emitter.get_run_code_fn() };
            ptr
        };

        // Use the run_code_fn cached at construction time — no mprotect.
        let run_fn = self.inner.run_code_fn.unwrap();

        let halt_bits = unsafe {
            run_fn(
                &mut self.inner.jit_state as *mut _ as *mut A64JitState,
                code_ptr,
            )
        };
        emitter
            .process_pending_fastmem_recompiles()
            .expect("processing A32 fastmem recompiles failed");

        self.inner.is_executing = false;
        HaltReason::from_bits_truncate(halt_bits)
    }

    /// Execute a single instruction.
    pub fn step(&mut self) -> HaltReason {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_mut() {
            return arm64.step().expect("A32 ARM64 step failed");
        }

        assert!(
            !self.inner.is_executing,
            "Recursive JIT execution not allowed"
        );
        self.inner.is_executing = true;

        let a32_loc = crate::ir::location::A32LocationDescriptor::from_location(
            LocationDescriptor::new(self.inner.jit_state.get_unique_hash()),
        );
        let location = a32_loc.set_single_stepping(true).to_location();

        let callbacks_ptr = self.inner.callbacks.as_ref() as *const dyn A32UserCallbacks;

        if let Some(ref mut emitter) = self.inner.emitter {
            let _ = emitter.make_writable();
        }

        let translate_callbacks = UserCallbacksAdapter::new(unsafe { &*callbacks_ptr });
        let is_read_only =
            move |vaddr: u32| -> bool { unsafe { &*callbacks_ptr }.is_read_only_memory(vaddr) };

        let code_ptr = self
            .inner
            .emitter
            .as_mut()
            .unwrap()
            .get_or_compile_block_with_ro(location, &translate_callbacks, &is_read_only);

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

        self.inner.is_executing = false;
        HaltReason::from_bits_truncate(halt_bits)
    }

    /// Request halt from another thread.
    pub fn halt_execution(&self, reason: HaltReason) {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            arm64.halt_execution(reason);
            return;
        }

        let halt_ptr = &self.inner.jit_state.halt_reason as *const u32 as *const AtomicU32;
        let atomic = unsafe { &*halt_ptr };
        atomic.fetch_or(reason.bits(), Ordering::Release);
    }

    /// Read the current halt_reason value (diagnostic).
    pub fn read_halt_reason(&self) -> u32 {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            return arm64.current_halt_reason().bits();
        }

        let halt_ptr = &self.inner.jit_state.halt_reason as *const u32 as *const AtomicU32;
        let atomic = unsafe { &*halt_ptr };
        atomic.load(Ordering::Acquire)
    }

    /// Get the address of halt_reason (diagnostic).
    pub fn halt_reason_ptr(&self) -> *const u32 {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            return arm64.halt_reason_ptr();
        }

        &self.inner.jit_state.halt_reason as *const u32
    }

    /// Get the address of jit_state base (R15 value).
    pub fn jit_state_ptr(&self) -> *const u8 {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            return arm64.jit_state_ptr();
        }

        &self.inner.jit_state as *const A32JitState as *const u8
    }

    /// Clear specific halt reason bits.
    pub fn clear_halt(&self, reason: HaltReason) {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            arm64.clear_halt(reason);
            return;
        }

        let halt_ptr = &self.inner.jit_state.halt_reason as *const u32 as *const AtomicU32;
        let atomic = unsafe { &*halt_ptr };
        atomic.fetch_and(!reason.bits(), Ordering::Release);
    }

    // ---- Register accessors (R0-R15, u32) ----

    pub fn get_register(&self, index: usize) -> u32 {
        assert!(index < 16, "A32 register index out of range (0-15)");
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            return arm64.regs()[index];
        }

        self.inner.jit_state.reg[index]
    }

    pub fn set_register(&mut self, index: usize, value: u32) {
        assert!(index < 16, "A32 register index out of range (0-15)");
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_mut() {
            arm64.regs_mut()[index] = value;
            return;
        }

        self.inner.jit_state.reg[index] = value;
    }

    pub fn get_pc(&self) -> u32 {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            return arm64.regs()[15];
        }

        self.inner.jit_state.reg[15]
    }

    pub fn set_pc(&mut self, value: u32) {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_mut() {
            arm64.regs_mut()[15] = value;
            return;
        }

        self.inner.jit_state.reg[15] = value;
    }

    pub fn get_cpsr(&self) -> u32 {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            return arm64.cpsr();
        }

        self.inner.jit_state.get_cpsr()
    }

    pub fn set_cpsr(&mut self, value: u32) {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_mut() {
            arm64.set_cpsr(value);
            return;
        }

        // A32JitState::set_cpsr handles both cpsr fields and upper_location_descriptor
        self.inner.jit_state.set_cpsr(value);
    }

    pub fn get_fpscr(&self) -> u32 {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            return arm64.fpscr();
        }

        self.inner.jit_state.get_fpscr()
    }

    pub fn set_fpscr(&mut self, value: u32) {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_mut() {
            arm64.set_fpscr(value);
            return;
        }

        // set_fpscr updates fpsr_nzcv, mode bits, mxcsr, AND upper_location_descriptor
        self.inner.jit_state.set_fpscr(value);
    }

    /// Get extension register (S/D backing store, u32 element).
    pub fn get_ext_reg(&self, index: usize) -> u32 {
        assert!(index < 64, "A32 ext_reg index out of range (0-63)");
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            return arm64.ext_regs().0[index];
        }

        self.inner.jit_state.ext_reg[index]
    }

    /// Set extension register.
    pub fn set_ext_reg(&mut self, index: usize, value: u32) {
        assert!(index < 64, "A32 ext_reg index out of range (0-63)");
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_mut() {
            arm64.ext_regs_mut().0[index] = value;
            return;
        }

        self.inner.jit_state.ext_reg[index] = value;
    }

    /// Get CNTPCT (Physical Count Timer) value.
    pub fn get_cntpct(&self) -> u64 {
        #[cfg(target_arch = "aarch64")]
        if self.arm64.is_some() {
            return self.arm64_cntpct;
        }

        self.inner.jit_state.cntpct
    }

    /// Set CNTPCT (Physical Count Timer) value.
    /// Should be set before `run()` to provide the current tick count.
    /// Read by MRRC p15, 0, Rt, Rt2, c14.
    pub fn set_cntpct(&mut self, value: u64) {
        #[cfg(target_arch = "aarch64")]
        if self.arm64.is_some() {
            self.arm64_cntpct = value;
            return;
        }

        self.inner.jit_state.cntpct = value;
    }

    /// Clear exclusive monitor state.
    /// Matching dynarmic's `Jit::ClearExclusiveState()`.
    /// Called before `run()` to ensure no stale exclusive reservation persists.
    pub fn clear_exclusive_state(&mut self) {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_mut() {
            arm64.clear_exclusive_state();
            return;
        }

        self.inner.jit_state.exclusive_state = 0;
    }

    /// Invalidate cached blocks in a memory range.
    pub fn invalidate_cache_range(&mut self, addr: u64, size: u64) {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            arm64.invalidate_cache_range(addr as u32, size as usize);
            return;
        }

        self.inner.jit_state.reset_rsb();
        if let Some(ref mut emitter) = self.inner.emitter {
            emitter.invalidate_range(addr, size);
        }
    }

    /// Clear all cached blocks.
    pub fn clear_cache(&mut self) {
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_ref() {
            arm64.clear_cache();
            return;
        }

        self.inner.jit_state.reset_rsb();
        if let Some(ref mut emitter) = self.inner.emitter {
            emitter.clear_cache();
        }
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
        #[cfg(target_arch = "aarch64")]
        if let Some(arm64) = self.arm64.as_mut() {
            return arm64
                .compile_block_only()
                .expect("A32 ARM64 compile_block_only failed");
        }

        let a32_loc = crate::ir::location::A32LocationDescriptor::from_location(
            LocationDescriptor::new(self.inner.jit_state.get_unique_hash()),
        );
        let location = a32_loc.to_location();

        let callbacks_ptr = self.inner.callbacks.as_ref() as *const dyn A32UserCallbacks;

        if let Some(ref mut emitter) = self.inner.emitter {
            let _ = emitter.make_writable();
        }

        let translate_callbacks = UserCallbacksAdapter::new(unsafe { &*callbacks_ptr });
        let is_read_only =
            move |vaddr: u32| -> bool { unsafe { &*callbacks_ptr }.is_read_only_memory(vaddr) };

        self.inner
            .emitter
            .as_mut()
            .unwrap()
            .get_or_compile_block_with_ro(location, &translate_callbacks, &is_read_only)
    }
}

// ---------------------------------------------------------------------------
// A32 Callback trampolines
// ---------------------------------------------------------------------------

/// Env-gated block-entry logger. Reads `RUZU_BLOCK_TRACE_PC=0xLO-0xHI`
/// once; for every block lookup whose target PC falls in that range, logs
/// `[BLOCK] pc=... lr=... r0=... r4=...` to stderr. Zero cost when unset.
fn block_trace_range() -> Option<(u32, u32)> {
    use std::sync::OnceLock;
    static RANGE: OnceLock<Option<(u32, u32)>> = OnceLock::new();
    *RANGE.get_or_init(|| {
        let raw = std::env::var("RUZU_BLOCK_TRACE_PC").ok()?;
        let (a, b) = raw.split_once('-')?;
        let parse = |s: &str| -> Option<u32> {
            let s = s.trim();
            let stripped = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"));
            match stripped {
                Some(hex) => u32::from_str_radix(hex, 16).ok(),
                None => s.parse::<u32>().ok(),
            }
        };
        let lo = parse(a)?;
        let hi = parse(b)?;
        if hi <= lo {
            return None;
        }
        Some((lo, hi))
    })
}

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
pub static PC_TRACE_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

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

    let callbacks_ptr = inner.callbacks.as_ref() as *const dyn A32UserCallbacks;
    let emitter = inner.emitter.as_mut().unwrap();

    // Fast path: block already compiled — no mprotect needed.
    if let Some(code_ptr) = emitter.lookup_cached_block(location) {
        return code_ptr as u64;
    }

    // Slow path: compile new block
    let translate_callbacks = UserCallbacksAdapter::new(unsafe { &*callbacks_ptr });
    let is_read_only =
        move |vaddr: u32| -> bool { unsafe { &*callbacks_ptr }.is_read_only_memory(vaddr) };

    let _ = emitter.make_writable();
    let code_ptr =
        emitter.get_or_compile_block_with_ro(location, &translate_callbacks, &is_read_only);
    let _ = unsafe { emitter.get_run_code_fn() };
    code_ptr as u64
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
    a32_callbacks::exclusive_clear(&mut inner.jit_state);
}
extern "C" fn a32_exclusive_read_8_trampoline(inner_ptr: u64, vaddr: u64) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    a32_callbacks::exclusive_read_8(
        &mut inner.jit_state,
        inner.callbacks.as_mut(),
        inner.global_monitor,
        inner.processor_id,
        vaddr,
    )
}
extern "C" fn a32_exclusive_read_16_trampoline(inner_ptr: u64, vaddr: u64) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    a32_callbacks::exclusive_read_16(
        &mut inner.jit_state,
        inner.callbacks.as_mut(),
        inner.global_monitor,
        inner.processor_id,
        vaddr,
    )
}
extern "C" fn a32_exclusive_read_32_trampoline(inner_ptr: u64, vaddr: u64) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    a32_callbacks::exclusive_read_32(
        &mut inner.jit_state,
        inner.callbacks.as_mut(),
        inner.global_monitor,
        inner.processor_id,
        vaddr,
    )
}
extern "C" fn a32_exclusive_read_64_trampoline(inner_ptr: u64, vaddr: u64) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    a32_callbacks::exclusive_read_64(
        &mut inner.jit_state,
        inner.callbacks.as_mut(),
        inner.global_monitor,
        inner.processor_id,
        vaddr,
    )
}
extern "C" fn a32_exclusive_write_8_trampoline(inner_ptr: u64, vaddr: u64, value: u64) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    a32_callbacks::exclusive_write_8(
        &mut inner.jit_state,
        inner.callbacks.as_mut(),
        inner.global_monitor,
        inner.processor_id,
        vaddr,
        value,
    )
}
extern "C" fn a32_exclusive_write_16_trampoline(inner_ptr: u64, vaddr: u64, value: u64) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    a32_callbacks::exclusive_write_16(
        &mut inner.jit_state,
        inner.callbacks.as_mut(),
        inner.global_monitor,
        inner.processor_id,
        vaddr,
        value,
    )
}
extern "C" fn a32_exclusive_write_32_trampoline(inner_ptr: u64, vaddr: u64, value: u64) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    maybe_log_a32_watch_write(inner, vaddr, 4, value, 0);
    a32_callbacks::exclusive_write_32(
        &mut inner.jit_state,
        inner.callbacks.as_mut(),
        inner.global_monitor,
        inner.processor_id,
        vaddr,
        value,
    )
}
extern "C" fn a32_exclusive_write_64_trampoline(inner_ptr: u64, vaddr: u64, value: u64) -> u64 {
    let inner = unsafe { &mut *(inner_ptr as *mut A32JitInner) };
    a32_callbacks::exclusive_write_64(
        &mut inner.jit_state,
        inner.callbacks.as_mut(),
        inner.global_monitor,
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;

    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    struct TestFastmemMapping {
        ptr: *mut libc::c_void,
        len: usize,
    }

    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    impl TestFastmemMapping {
        fn new(len: usize) -> Self {
            let ptr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    len,
                    libc::PROT_NONE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            assert_ne!(ptr, libc::MAP_FAILED);
            Self { ptr, len }
        }

        fn map_u32(&self, offset: usize, value: u32) {
            assert_eq!(
                unsafe {
                    libc::mprotect(
                        self.ptr.cast::<u8>().add(offset).cast(),
                        0x1000,
                        libc::PROT_READ | libc::PROT_WRITE,
                    )
                },
                0
            );
            unsafe {
                self.ptr.cast::<u8>().add(offset).cast::<u32>().write(value);
            }
        }
    }

    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    impl Drop for TestFastmemMapping {
        fn drop(&mut self) {
            unsafe {
                libc::munmap(self.ptr, self.len);
            }
        }
    }

    /// Mock callbacks for testing.
    struct MockCallbacks {
        memory: Arc<Mutex<Vec<u8>>>,
        base_addr: u64,
        ticks_remaining: u64,
        ticks_added: u64,
        cntpct: u64,
        last_svc: Option<u32>,
        svc_sink: Option<Arc<AtomicU32>>,
        isb_sink: Option<Arc<AtomicU32>>,
        data_cache_sink: Option<Arc<Mutex<Vec<(u64, u64)>>>>,
        instruction_cache_sink: Option<Arc<Mutex<Vec<(u64, u64)>>>>,
        memory_read_address_sink: Option<Arc<AtomicU64>>,
        memory_read_64_count: Option<Arc<AtomicU64>>,
        halt_reason_ptr: Option<usize>,
    }

    impl MockCallbacks {
        fn new(base_addr: u64, code: &[u32]) -> Self {
            let mut memory = vec![0u8; 0x10000];
            for (i, &word) in code.iter().enumerate() {
                let offset = i * 4;
                let bytes = word.to_le_bytes();
                memory[offset..offset + 4].copy_from_slice(&bytes);
            }
            Self {
                memory: Arc::new(Mutex::new(memory)),
                base_addr,
                ticks_remaining: 1000,
                ticks_added: 0,
                cntpct: 0,
                last_svc: None,
                svc_sink: None,
                isb_sink: None,
                data_cache_sink: None,
                instruction_cache_sink: None,
                memory_read_address_sink: None,
                memory_read_64_count: None,
                halt_reason_ptr: None,
            }
        }

        fn from_memory(base_addr: u64, memory: Vec<u8>) -> Self {
            Self {
                memory: Arc::new(Mutex::new(memory)),
                base_addr,
                ticks_remaining: 1000,
                ticks_added: 0,
                cntpct: 0,
                last_svc: None,
                svc_sink: None,
                isb_sink: None,
                data_cache_sink: None,
                instruction_cache_sink: None,
                memory_read_address_sink: None,
                memory_read_64_count: None,
                halt_reason_ptr: None,
            }
        }

        fn from_shared_memory(base_addr: u64, memory: Arc<Mutex<Vec<u8>>>) -> Self {
            Self {
                memory,
                base_addr,
                ticks_remaining: 1000,
                ticks_added: 0,
                cntpct: 0,
                last_svc: None,
                svc_sink: None,
                isb_sink: None,
                data_cache_sink: None,
                instruction_cache_sink: None,
                memory_read_address_sink: None,
                memory_read_64_count: None,
                halt_reason_ptr: None,
            }
        }

        #[cfg(target_arch = "aarch64")]
        fn with_cntpct(base_addr: u64, code: &[u32], cntpct: u64) -> Self {
            let mut callbacks = Self::new(base_addr, code);
            callbacks.cntpct = cntpct;
            callbacks
        }

        fn with_svc_sink(base_addr: u64, code: &[u32], svc_sink: Arc<AtomicU32>) -> Self {
            let mut memory = vec![0u8; 0x10000];
            for (i, &word) in code.iter().enumerate() {
                let offset = i * 4;
                let bytes = word.to_le_bytes();
                memory[offset..offset + 4].copy_from_slice(&bytes);
            }
            Self {
                memory: Arc::new(Mutex::new(memory)),
                base_addr,
                ticks_remaining: 1000,
                ticks_added: 0,
                cntpct: 0,
                last_svc: None,
                svc_sink: Some(svc_sink),
                isb_sink: None,
                data_cache_sink: None,
                instruction_cache_sink: None,
                memory_read_address_sink: None,
                memory_read_64_count: None,
                halt_reason_ptr: None,
            }
        }

        #[cfg(target_arch = "aarch64")]
        fn with_memory_read_address_sink(mut self, sink: Arc<AtomicU64>) -> Self {
            self.memory_read_address_sink = Some(sink);
            self
        }

        fn with_memory_read_64_count(base_addr: u64, code: &[u32], count: Arc<AtomicU64>) -> Self {
            let mut callbacks = Self::new(base_addr, code);
            callbacks.memory_read_64_count = Some(count);
            callbacks
        }

        fn with_isb_sink(base_addr: u64, code: &[u32], isb_sink: Arc<AtomicU32>) -> Self {
            let mut callbacks = Self::new(base_addr, code);
            callbacks.isb_sink = Some(isb_sink);
            callbacks
        }

        fn with_cache_operation_sinks(
            base_addr: u64,
            code: &[u32],
            data_cache_sink: Arc<Mutex<Vec<(u64, u64)>>>,
            instruction_cache_sink: Arc<Mutex<Vec<(u64, u64)>>>,
        ) -> Self {
            let mut callbacks = Self::new(base_addr, code);
            callbacks.data_cache_sink = Some(data_cache_sink);
            callbacks.instruction_cache_sink = Some(instruction_cache_sink);
            callbacks
        }
    }

    impl UserCallbacks for MockCallbacks {
        fn memory_read_code(&self, vaddr: u64) -> Option<u32> {
            let offset = vaddr.wrapping_sub(self.base_addr) as usize;
            let memory = self.memory.lock().expect("mock memory poisoned");
            if offset + 4 <= memory.len() {
                Some(u32::from_le_bytes([
                    memory[offset],
                    memory[offset + 1],
                    memory[offset + 2],
                    memory[offset + 3],
                ]))
            } else {
                None
            }
        }

        fn memory_read_8(&self, vaddr: u64) -> u8 {
            let offset = vaddr.wrapping_sub(self.base_addr) as usize;
            let memory = self.memory.lock().expect("mock memory poisoned");
            memory.get(offset).copied().unwrap_or(0)
        }
        fn memory_read_16(&self, vaddr: u64) -> u16 {
            let offset = vaddr.wrapping_sub(self.base_addr) as usize;
            let memory = self.memory.lock().expect("mock memory poisoned");
            if offset + 2 <= memory.len() {
                u16::from_le_bytes([memory[offset], memory[offset + 1]])
            } else {
                0
            }
        }
        fn memory_read_32(&self, vaddr: u64) -> u32 {
            let offset = vaddr.wrapping_sub(self.base_addr) as usize;
            let memory = self.memory.lock().expect("mock memory poisoned");
            if offset + 4 <= memory.len() {
                u32::from_le_bytes(memory[offset..offset + 4].try_into().unwrap())
            } else {
                0
            }
        }
        fn memory_read_64(&self, vaddr: u64) -> u64 {
            if let Some(sink) = &self.memory_read_address_sink {
                sink.store(vaddr, Ordering::Relaxed);
            }
            if let Some(count) = &self.memory_read_64_count {
                count.fetch_add(1, Ordering::Relaxed);
            }
            let offset = vaddr.wrapping_sub(self.base_addr) as usize;
            let memory = self.memory.lock().expect("mock memory poisoned");
            if offset + 8 <= memory.len() {
                u64::from_le_bytes(memory[offset..offset + 8].try_into().unwrap())
            } else {
                0
            }
        }
        fn memory_read_128(&self, vaddr: u64) -> (u64, u64) {
            (self.memory_read_64(vaddr), self.memory_read_64(vaddr + 8))
        }

        fn memory_write_8(&mut self, vaddr: u64, value: u8) {
            let offset = vaddr.wrapping_sub(self.base_addr) as usize;
            let mut memory = self.memory.lock().expect("mock memory poisoned");
            if offset < memory.len() {
                memory[offset] = value;
            }
        }
        fn memory_write_16(&mut self, vaddr: u64, value: u16) {
            let offset = vaddr.wrapping_sub(self.base_addr) as usize;
            let mut memory = self.memory.lock().expect("mock memory poisoned");
            if offset + 2 <= memory.len() {
                memory[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
            }
        }
        fn memory_write_32(&mut self, vaddr: u64, value: u32) {
            let offset = vaddr.wrapping_sub(self.base_addr) as usize;
            let mut memory = self.memory.lock().expect("mock memory poisoned");
            if offset + 4 <= memory.len() {
                memory[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            }
        }
        fn memory_write_64(&mut self, vaddr: u64, value: u64) {
            let offset = vaddr.wrapping_sub(self.base_addr) as usize;
            let mut memory = self.memory.lock().expect("mock memory poisoned");
            if offset + 8 <= memory.len() {
                memory[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
            }
        }
        fn memory_write_128(&mut self, vaddr: u64, lo: u64, hi: u64) {
            self.memory_write_64(vaddr, lo);
            self.memory_write_64(vaddr + 8, hi);
        }

        fn exclusive_write_8(&mut self, vaddr: u64, value: u8, _expected: u8) -> bool {
            self.memory_write_8(vaddr, value);
            true
        }
        fn exclusive_write_16(&mut self, vaddr: u64, value: u16, _expected: u16) -> bool {
            self.memory_write_16(vaddr, value);
            true
        }
        fn exclusive_write_32(&mut self, vaddr: u64, value: u32, _expected: u32) -> bool {
            self.memory_write_32(vaddr, value);
            true
        }
        fn exclusive_write_64(&mut self, vaddr: u64, value: u64, _expected: u64) -> bool {
            self.memory_write_64(vaddr, value);
            true
        }
        fn exclusive_write_128(
            &mut self,
            vaddr: u64,
            lo: u64,
            hi: u64,
            _expected_lo: u64,
            _expected_hi: u64,
        ) -> bool {
            self.memory_write_128(vaddr, lo, hi);
            true
        }
        fn call_supervisor(&mut self, svc_num: u32) {
            self.last_svc = Some(svc_num);
            if let Some(ref sink) = self.svc_sink {
                sink.store(svc_num, Ordering::Relaxed);
            }
            if let Some(ptr) = self.halt_reason_ptr {
                unsafe {
                    (&*(ptr as *const AtomicU32))
                        .fetch_or(HaltReason::SVC.bits(), Ordering::SeqCst);
                }
            }
        }
        fn exception_raised(&mut self, _pc: u64, _exception: u64) {}

        fn instruction_synchronization_barrier_raised(&mut self) {
            if let Some(ref sink) = self.isb_sink {
                sink.fetch_add(1, Ordering::Relaxed);
            }
        }

        fn data_cache_operation(&mut self, op: u64, vaddr: u64) {
            if let Some(ref sink) = self.data_cache_sink {
                sink.lock()
                    .expect("data-cache sink poisoned")
                    .push((op, vaddr));
            }
        }

        fn instruction_cache_operation(&mut self, op: u64, vaddr: u64) {
            if let Some(ref sink) = self.instruction_cache_sink {
                sink.lock()
                    .expect("instruction-cache sink poisoned")
                    .push((op, vaddr));
            }
        }

        fn get_cntpct(&self) -> u64 {
            self.cntpct
        }

        fn add_ticks(&mut self, ticks: u64) {
            self.ticks_added += ticks;
        }
        fn get_ticks_remaining(&self) -> u64 {
            self.ticks_remaining
        }

        fn set_halt_reason_ptr(&mut self, ptr: *const u32) {
            self.halt_reason_ptr = Some(ptr as usize);
        }
    }

    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    #[test]
    fn a32_fastmem_fault_recompiles_only_faulting_access() {
        let code: [u32; 3] = [
            0xE590_1000, // ldr r1, [r0]
            0xE593_2000, // ldr r2, [r3]
            0xEF00_0000, // svc #0
        ];
        let mut memory = vec![0u8; 0x10_000];
        for (index, instruction) in code.iter().enumerate() {
            memory[index * 4..index * 4 + 4].copy_from_slice(&instruction.to_le_bytes());
        }
        // The callback deliberately disagrees with fastmem for the first
        // address, so the second run proves that only the faulting LDR was
        // disabled.
        memory[0x1000..0x1004].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        memory[0x3000..0x3004].copy_from_slice(&0x1234_5678u32.to_le_bytes());

        let mapping = TestFastmemMapping::new(0x10_000);
        mapping.map_u32(0x2000, 0xCAFE_BABE);

        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::from_memory(0x1000, memory)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::NO_OPTIMIZATIONS,
            unsafe_optimizations: false,
            global_monitor: None,
            fastmem_pointer: Some(mapping.ptr.cast()),
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        jit.set_register(0, 0x2000);
        jit.set_register(3, 0x4000);
        jit.set_register(15, 0x1000);
        let location = LocationDescriptor::new(jit.inner.jit_state.get_unique_hash());

        let first_halt = jit.run();
        assert!(first_halt.contains(HaltReason::SVC));
        assert_eq!(jit.get_register(1), 0xCAFE_BABE);
        assert_eq!(jit.get_register(2), 0x1234_5678);
        {
            let emitter = jit.inner.emitter.as_ref().unwrap();
            assert_eq!(emitter.do_not_fastmem.len(), 1);
            assert!(!emitter.cache.contains(&location));
        }

        jit.clear_halt(HaltReason::SVC);
        jit.set_register(15, 0x1000);
        let second_halt = jit.run();
        assert!(second_halt.contains(HaltReason::SVC));
        assert_eq!(jit.get_register(1), 0xCAFE_BABE);
        assert_eq!(jit.get_register(2), 0x1234_5678);
        {
            let emitter = jit.inner.emitter.as_ref().unwrap();
            assert_eq!(emitter.do_not_fastmem.len(), 1);
            assert!(emitter.cache.contains(&location));
        }

        drop(jit);
        drop(mapping);
    }

    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    #[test]
    fn a32_fastmem_takes_precedence_over_page_table() {
        let code: [u32; 2] = [
            0xE590_1000, // ldr r1, [r0]
            0xEF00_0000, // svc #0
        ];
        let mut callback_memory = vec![0u8; 0x10_000];
        for (index, instruction) in code.iter().enumerate() {
            callback_memory[index * 4..index * 4 + 4].copy_from_slice(&instruction.to_le_bytes());
        }
        callback_memory[0x2000..0x2004].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());

        let mapping = TestFastmemMapping::new(0x10_000);
        mapping.map_u32(0x2000, 0xCAFE_BABE);
        let page_table = vec![0usize; 1 << (16 - 12)];

        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::from_memory(0, callback_memory)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::NO_OPTIMIZATIONS,
            unsafe_optimizations: false,
            global_monitor: None,
            fastmem_pointer: Some(mapping.ptr.cast()),
            page_table_pointer: Some(page_table.as_ptr().cast()),
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig {
                fastmem_address_space_bits: 16,
                silently_mirror_fastmem: true,
                page_table_present: true,
                page_table_address_space_bits: 16,
                silently_mirror_page_table: true,
                absolute_offset_page_table: true,
                ..Default::default()
            },
        };
        let mut jit = A32Jit::new(config).expect("A32 JIT");
        jit.set_register(0, 0x2000);
        jit.set_register(15, 0);

        let halt = jit.run();

        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(jit.get_register(1), 0xCAFE_BABE);

        drop(jit);
        drop(page_table);
        drop(mapping);
    }

    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    #[test]
    fn a32_fastmem_fault_recompiles_to_page_table() {
        let code: [u32; 2] = [
            0xE590_1000, // ldr r1, [r0]
            0xEF00_0000, // svc #0
        ];
        let mut callback_memory = vec![0u8; 0x10_000];
        for (index, instruction) in code.iter().enumerate() {
            callback_memory[index * 4..index * 4 + 4].copy_from_slice(&instruction.to_le_bytes());
        }
        callback_memory[0x3000..0x3004].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());

        let mapping = TestFastmemMapping::new(0x10_000);
        let mut page_memory = vec![0u8; 0x1000];
        page_memory[0..4].copy_from_slice(&0xCAFE_BABEu32.to_le_bytes());
        let mut page_table = vec![0usize; 1 << (16 - 12)];
        page_table[3] = (page_memory.as_ptr() as usize).wrapping_sub(0x3000);

        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::from_memory(0, callback_memory)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::NO_OPTIMIZATIONS,
            unsafe_optimizations: false,
            global_monitor: None,
            fastmem_pointer: Some(mapping.ptr.cast()),
            page_table_pointer: Some(page_table.as_ptr().cast()),
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig {
                fastmem_address_space_bits: 16,
                silently_mirror_fastmem: true,
                page_table_present: true,
                page_table_address_space_bits: 16,
                silently_mirror_page_table: true,
                absolute_offset_page_table: true,
                ..Default::default()
            },
        };
        let mut jit = A32Jit::new(config).expect("A32 JIT");
        jit.set_register(0, 0x3000);
        jit.set_register(15, 0);

        assert!(jit.run().contains(HaltReason::SVC));
        assert_eq!(jit.get_register(1), 0xDEAD_BEEF);

        jit.clear_halt(HaltReason::SVC);
        jit.set_register(15, 0);
        assert!(jit.run().contains(HaltReason::SVC));
        assert_eq!(jit.get_register(1), 0xCAFE_BABE);

        drop(jit);
        drop(page_table);
        drop(page_memory);
        drop(mapping);
    }

    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    #[test]
    fn a64_fastmem_fault_recompiles_only_faulting_access() {
        let code: [u32; 3] = [
            0xB940_0001, // ldr w1, [x0]
            0xB940_0062, // ldr w2, [x3]
            0xD400_0001, // svc #0
        ];
        let mut memory = vec![0u8; 0x10_000];
        for (index, instruction) in code.iter().enumerate() {
            memory[index * 4..index * 4 + 4].copy_from_slice(&instruction.to_le_bytes());
        }
        memory[0x1000..0x1004].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        memory[0x3000..0x3004].copy_from_slice(&0x1234_5678u32.to_le_bytes());

        let mapping = TestFastmemMapping::new(0x10_000);
        mapping.map_u32(0x2000, 0xCAFE_BABE);

        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::from_memory(0x1000, memory)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::NO_OPTIMIZATIONS,
            unsafe_optimizations: false,
            global_monitor: None,
            fastmem_pointer: Some(mapping.ptr.cast()),
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_register(0, 0x2000);
        jit.set_register(3, 0x4000);
        jit.set_pc(0x1000);
        let location = LocationDescriptor::new(jit.inner.jit_state.get_unique_hash());

        let first_halt = jit.run();
        assert!(first_halt.contains(HaltReason::SVC));
        assert_eq!(jit.get_register(1), 0xCAFE_BABE);
        assert_eq!(jit.get_register(2), 0x1234_5678);
        {
            let emitter = jit.inner.emitter.as_ref().unwrap();
            assert_eq!(emitter.do_not_fastmem.len(), 1);
            assert!(!emitter.cache.contains(&location));
        }

        jit.clear_halt(HaltReason::SVC);
        jit.set_pc(0x1000);
        let second_halt = jit.run();
        assert!(second_halt.contains(HaltReason::SVC));
        assert_eq!(jit.get_register(1), 0xCAFE_BABE);
        assert_eq!(jit.get_register(2), 0x1234_5678);
        {
            let emitter = jit.inner.emitter.as_ref().unwrap();
            assert_eq!(emitter.do_not_fastmem.len(), 1);
            assert!(emitter.cache.contains(&location));
        }

        drop(jit);
        drop(mapping);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn a32_public_jit_uses_arm64_interface_on_aarch64() {
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, &[0xe1a0_0000])),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };

        let mut jit = A32Jit::new(config).expect("A32 ARM64 public JIT");
        assert!(!jit.jit_state_ptr().is_null());
        assert!(!jit.halt_reason_ptr().is_null());

        jit.set_register(0, 0x1234_5678);
        jit.set_register(15, 0x1000);
        assert_eq!(jit.get_register(0), 0x1234_5678);
        assert_eq!(jit.get_pc(), 0x1000);

        jit.set_cpsr(0x6000_01d3);
        assert_eq!(jit.get_cpsr() & 0xf100_01ff, 0x6000_01d3);
        jit.set_fpscr(0xf800_009f);
        assert_eq!(jit.get_fpscr() & 0xf800_009f, 0xf800_009f);

        jit.set_ext_reg(7, 0xaabb_ccdd);
        assert_eq!(jit.get_ext_reg(7), 0xaabb_ccdd);

        jit.set_cntpct(0x1234_5678_9abc_def0);
        assert_eq!(jit.get_cntpct(), 0x1234_5678_9abc_def0);

        jit.halt_execution(HaltReason::USER_DEFINED2);
        assert_ne!(jit.read_halt_reason() & HaltReason::USER_DEFINED2.bits(), 0);
        jit.clear_halt(HaltReason::USER_DEFINED2);
        assert_eq!(jit.read_halt_reason() & HaltReason::USER_DEFINED2.bits(), 0);

        jit.clear_exclusive_state();
        jit.invalidate_cache_range(0x1000, 4);
        jit.clear_cache();
    }

    fn test_a64_inner(
        callbacks: MockCallbacks,
        global_monitor: Option<*mut crate::exclusive_monitor::ExclusiveMonitor>,
        processor_id: usize,
    ) -> Box<JitInner> {
        Box::new(JitInner {
            jit_state: A64JitState::new(),
            emitter: None,
            callbacks: Box::new(callbacks),
            run_code_fn: None,
            is_executing: false,
            global_monitor,
            processor_id,
        })
    }

    #[test]
    fn test_a64_exclusive_trampolines_use_global_monitor_cross_core() {
        let mut monitor = crate::exclusive_monitor::ExclusiveMonitor::new(2);
        let monitor_ptr = &mut monitor as *mut _;
        let mut core0 = test_a64_inner(
            MockCallbacks::from_memory(0x1000, vec![0; 0x100]),
            Some(monitor_ptr),
            0,
        );
        let mut core1 = test_a64_inner(
            MockCallbacks::from_memory(0x1000, vec![0; 0x100]),
            Some(monitor_ptr),
            1,
        );
        let core0_ptr = &mut *core0 as *mut JitInner as u64;
        let core1_ptr = &mut *core1 as *mut JitInner as u64;

        assert_eq!(exclusive_read_32_trampoline(core0_ptr, 0x1000), 0);
        assert_eq!(exclusive_read_32_trampoline(core1_ptr, 0x1000), 0);
        assert_eq!(exclusive_write_32_trampoline(core1_ptr, 0x1000, 0x1111), 0);
        assert_eq!(
            exclusive_write_32_trampoline(core0_ptr, 0x1000, 0x2222),
            1,
            "core1's successful STXR must invalidate core0's reservation"
        );
    }

    #[test]
    fn test_a64_exclusive_write_without_reservation_fails() {
        let mut monitor = crate::exclusive_monitor::ExclusiveMonitor::new(1);
        let mut inner = test_a64_inner(
            MockCallbacks::from_memory(0x1000, vec![0; 0x100]),
            Some(&mut monitor as *mut _),
            0,
        );
        let inner_ptr = &mut *inner as *mut JitInner as u64;

        assert_eq!(
            exclusive_write_32_trampoline(inner_ptr, 0x1000, 0x1234),
            1,
            "STXR without a prior LDXR must fail"
        );
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn test_a64_mrs_cntpct_writes_callback_value() {
        // MRS X1, CNTPCT_EL0; SVC #0
        let code: [u32; 2] = [0xD53B_E021, 0xD400_0001];
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::with_cntpct(
                0x1000,
                &code,
                0x1234_5678_9ABC_DEF0,
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).expect("A64 JIT");
        jit.set_pc(0x1000);
        jit.run();

        assert_eq!(jit.get_register(1), 0x1234_5678_9ABC_DEF0);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn test_a64_extr_register_32_and_64() {
        // EXTR W2, W0, W1, #8; EXTR X3, X0, X1, #16; SVC #0
        let code: [u32; 3] = [
            crate::backend::arm64::inst::extr_w(2, 0, 1, 8),
            crate::backend::arm64::inst::extr_x(3, 0, 1, 16),
            0xD400_0001,
        ];
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, &code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).expect("A64 JIT");
        jit.set_pc(0x1000);
        jit.set_register(0, 0x1122_3344_5566_7788);
        jit.set_register(1, 0x99AA_BBCC_DDEE_FF00);
        jit.run();

        assert_eq!(jit.get_register(2), 0x88DD_EEFF);
        assert_eq!(jit.get_register(3), 0x7788_99AA_BBCC_DDEE);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn test_a64_byte_reverse_word_half_and_dual() {
        // REV W1, W0; REV16 W2, W0; REV X3, X0; SVC #0
        let code: [u32; 4] = [
            crate::backend::arm64::inst::rev_w(1, 0),
            crate::backend::arm64::inst::rev16_w(2, 0),
            crate::backend::arm64::inst::rev_x(3, 0),
            0xD400_0001,
        ];
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, &code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).expect("A64 JIT");
        jit.set_pc(0x1000);
        jit.set_register(0, 0x1122_3344_5566_7788);
        jit.run();

        assert_eq!(jit.get_register(1), 0x8877_6655);
        assert_eq!(jit.get_register(2), 0x6655_8877);
        assert_eq!(jit.get_register(3), 0x8877_6655_4433_2211);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn test_a64_zero_extend_long_to_quad_from_movi_d() {
        // MOVI D28, #0; SVC #0. This frontend path materializes a U64 immediate
        // and emits ZeroExtendLongToQuad for the scalar SIMD destination.
        let code: [u32; 2] = [0x2F00_E41C, 0xD400_0001];
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, &code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).expect("A64 JIT");
        jit.set_pc(0x1000);
        jit.set_vector(28, 0xFFFF_FFFF_FFFF_FFFF, 0xFFFF_FFFF_FFFF_FFFF);
        jit.run();

        assert_eq!(jit.get_vector(28), (0, 0));
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn test_a64_sbfm_uses_replicate_bit_32_and_64() {
        fn sbfm_w(rd: u8, rn: u8, immr: u8, imms: u8) -> u32 {
            0x1300_0000
                | ((immr as u32) << 16)
                | ((imms as u32) << 10)
                | ((rn as u32) << 5)
                | rd as u32
        }

        fn sbfm_x(rd: u8, rn: u8, immr: u8, imms: u8) -> u32 {
            0x9340_0000
                | ((immr as u32) << 16)
                | ((imms as u32) << 10)
                | ((rn as u32) << 5)
                | rd as u32
        }

        // SBFM W1, W0, #4, #11; SBFM X2, X0, #4, #11; SVC #0
        let code: [u32; 3] = [sbfm_w(1, 0, 4, 11), sbfm_x(2, 0, 4, 11), 0xD400_0001];
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, &code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).expect("A64 JIT");
        jit.set_pc(0x1000);
        jit.set_register(0, 0x0000_0000_0000_0F80);
        jit.run();

        assert_eq!(jit.get_register(1), 0xFFFF_FFF8);
        assert_eq!(jit.get_register(2), 0xFFFF_FFFF_FFFF_FFF8);
    }

    #[test]
    fn test_a64_barriers_respect_isb_hook() {
        // DSB SY; DMB SY; ISB SY; SVC #0
        let code: [u32; 4] = [0xD503_3F9F, 0xD503_3FBF, 0xD503_3FDF, 0xD400_0001];
        let isb_count = Arc::new(AtomicU32::new(0));
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::with_isb_sink(
                0x1000,
                &code,
                Arc::clone(&isb_count),
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            hook_isb: true,
            tpidrro_el0: None,
            tpidr_el0: None,
            memory: crate::backend::x64::emit_context::MemoryEmitConfig {
                ..Default::default()
            },
        };
        let mut jit = A64Jit::new(config).expect("A64 JIT");
        jit.set_pc(0x1000);

        let mut halt = jit.run();
        if halt.is_empty() {
            halt = jit.run();
        }

        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(isb_count.load(Ordering::Relaxed), 1);

        let isb_count = Arc::new(AtomicU32::new(0));
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::with_isb_sink(
                0x1000,
                &code,
                Arc::clone(&isb_count),
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).expect("A64 JIT");
        jit.set_pc(0x1000);
        let mut halt = jit.run();
        if halt.is_empty() {
            halt = jit.run();
        }

        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(isb_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_a64_cache_maintenance_callbacks_match_upstream() {
        // IC IVAU, X0; DC IVAC, X1; SVC #0.
        let code: [u32; 3] = [0xD50B_7520, 0xD508_7621, 0xD400_0001];
        let data_cache = Arc::new(Mutex::new(Vec::new()));
        let instruction_cache = Arc::new(Mutex::new(Vec::new()));
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::with_cache_operation_sinks(
                0x1000,
                &code,
                Arc::clone(&data_cache),
                Arc::clone(&instruction_cache),
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            dczid_el0: 7,
            hook_data_cache_operations: true,
            hook_isb: false,
            tpidrro_el0: None,
            tpidr_el0: None,
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).expect("A64 JIT");
        jit.set_pc(0x1000);
        jit.set_register(0, 0xCAFE_D00D);
        jit.set_register(1, 0xCAFE_BABE);

        let mut halt = jit.run();
        if halt.is_empty() {
            halt = jit.run();
        }

        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(
            *instruction_cache
                .lock()
                .expect("instruction-cache sink poisoned"),
            vec![(
                crate::interface::a64::config::InstructionCacheOperation::InvalidateByVaToPoU
                    as u64,
                0xCAFE_D00D,
            )]
        );
        assert_eq!(
            *data_cache.lock().expect("data-cache sink poisoned"),
            vec![(
                crate::interface::a64::config::DataCacheOperation::InvalidateByVaToPoC as u64,
                0xCAFE_BABE,
            )]
        );
    }

    #[test]
    fn test_a64_cache_system_registers_use_user_config() {
        // MRS X0, CTR_EL0; MRS X1, DCZID_EL0; SVC #0.
        let code: [u32; 3] = [0xD53B_0020, 0xD53B_00E1, 0xD400_0001];
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, &code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            ctr_el0: 0x1234_5678,
            dczid_el0: 0x0000_0017,
            hook_data_cache_operations: false,
            hook_isb: false,
            tpidrro_el0: None,
            tpidr_el0: None,
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).expect("A64 JIT");
        jit.set_pc(0x1000);

        let mut halt = jit.run();
        if halt.is_empty() {
            halt = jit.run();
        }

        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(jit.get_register(0), 0x1234_5678);
        assert_eq!(jit.get_register(1), 0x17);
    }

    #[test]
    fn test_a64_unhooked_dc_zva_uses_configured_block_size() {
        // DC ZVA, X0; SVC #0.
        let code: [u32; 2] = [0xD50B_7420, 0xD400_0001];
        let memory = Arc::new(Mutex::new(vec![0xAA; 0x3000]));
        {
            let mut bytes = memory.lock().expect("mock memory poisoned");
            for (index, word) in code.iter().enumerate() {
                bytes[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
            }
        }
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::from_shared_memory(
                0x1000,
                Arc::clone(&memory),
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            dczid_el0: 0,
            hook_data_cache_operations: false,
            hook_isb: false,
            tpidrro_el0: None,
            tpidr_el0: None,
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).expect("A64 JIT");
        jit.set_pc(0x1000);
        jit.set_register(0, 0x2000);

        let mut halt = jit.run();
        if halt.is_empty() {
            halt = jit.run();
        }

        assert!(halt.contains(HaltReason::SVC));
        let bytes = memory.lock().expect("mock memory poisoned");
        assert_eq!(&bytes[0x1000..0x1004], &[0, 0, 0, 0]);
        assert_eq!(bytes[0x1004], 0xAA);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn test_a64_ldaxr_stlxr_writes_status_and_memory() {
        // mov x1, #0x2000; mov w3, #0x1234; ldaxr w0, [x1];
        // stlxr w2, w3, [x1]; svc #0
        let code: [u32; 5] = [
            0xD284_0001,
            0x5282_4683,
            0x885F_FC20,
            0x8802_FC23,
            0xD400_0001,
        ];
        let mut memory = vec![0; 0x3000];
        for (index, word) in code.iter().enumerate() {
            let offset = 0x1000 + index * 4;
            memory[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        }
        memory[0x2000..0x2004].copy_from_slice(&0u32.to_le_bytes());
        let memory = Arc::new(Mutex::new(memory));

        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::from_shared_memory(0, memory.clone())),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).expect("A64 JIT");
        jit.set_pc(0x1000);
        jit.run();

        let memory_value =
            u32::from_le_bytes(memory.lock().unwrap()[0x2000..0x2004].try_into().unwrap());
        assert_eq!(jit.get_register(0), 0, "LDAXR must read the old value");
        assert_eq!(memory_value, 0x1234, "STLXR must write the new value");
        assert_eq!(
            jit.get_register(2),
            0,
            "STLXR status must be zero on success"
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a64_fastmem_ldaxr_stlxr_writes_status_and_memory() {
        let code: [u32; 8] = [
            0xD284_0001, // mov x1, #0x2000
            0x5282_4683, // mov w3, #0x1234
            0x885F_FC20, // ldaxr w0, [x1]
            0x8802_FC23, // stlxr w2, w3, [x1]
            0xD284_0206, // mov x6, #0x2010
            0xC87F_14C4, // ldxp x4, x5, [x6]
            0xC829_20C7, // stxp w9, x7, x8, [x6]
            0xD400_0001, // svc #0
        ];
        let mut memory = vec![0; 0x3000];
        for (index, word) in code.iter().enumerate() {
            let offset = 0x1000 + index * 4;
            memory[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        }
        let old_lo = 0x1122_3344_5566_7788u64;
        let old_hi = 0x99AA_BBCC_DDEE_FF00u64;
        memory[0x2010..0x2018].copy_from_slice(&old_lo.to_le_bytes());
        memory[0x2018..0x2020].copy_from_slice(&old_hi.to_le_bytes());
        let fastmem_pointer = memory.as_mut_ptr();
        let memory = Arc::new(Mutex::new(memory));
        let mut monitor = Box::new(crate::exclusive_monitor::ExclusiveMonitor::new(1));
        let monitor_ptr = Some(&mut *monitor as *mut _);

        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::from_shared_memory(0, memory.clone())),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::NO_OPTIMIZATIONS,
            unsafe_optimizations: false,
            global_monitor: monitor_ptr,
            fastmem_pointer: Some(fastmem_pointer),
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig {
                fastmem_address_space_bits: 16,
                silently_mirror_fastmem: true,
                fastmem_exclusive_access: true,
                recompile_on_exclusive_fastmem_failure: true,
                recompile_on_fastmem_failure: true,
                page_table_present: false,
                page_table_address_space_bits: 16,
                silently_mirror_page_table: true,
                absolute_offset_page_table: true,
                page_table_pointer_mask_bits: 0,
                detect_misaligned_access_via_page_table: 0,
                only_detect_misalignment_via_page_table_on_page_boundary: false,
                check_halt_on_memory_access: false,
                processor_id: 0,
            },
        };
        let mut jit = A64Jit::new(config).expect("A64 JIT");
        jit.set_pc(0x1000);
        let new_lo = 0x0123_4567_89AB_CDEFu64;
        let new_hi = 0xFEDC_BA98_7654_3210u64;
        jit.set_register(7, new_lo);
        jit.set_register(8, new_hi);
        let halt = jit.run();

        assert!(halt.contains(HaltReason::SVC));
        let memory_value =
            u32::from_le_bytes(memory.lock().unwrap()[0x2000..0x2004].try_into().unwrap());
        assert_eq!(jit.get_register(0), 0);
        assert_eq!(memory_value, 0x1234);
        assert_eq!(jit.get_register(2), 0);
        assert_eq!(jit.get_register(4), old_lo);
        assert_eq!(jit.get_register(5), old_hi);
        assert_eq!(jit.get_register(9), 0);
        let memory = memory.lock().unwrap();
        assert_eq!(
            u64::from_le_bytes(memory[0x2010..0x2018].try_into().unwrap()),
            new_lo
        );
        assert_eq!(
            u64::from_le_bytes(memory[0x2018..0x2020].try_into().unwrap()),
            new_hi
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a32_jit_preserves_disabled_exclusive_fastmem_policy() {
        let mut memory = vec![0; 0x2000];
        memory[0x1000..0x1004].copy_from_slice(&0xEF00_0000u32.to_le_bytes()); // svc #0
        let fastmem_pointer = memory.as_mut_ptr();
        let memory = Arc::new(Mutex::new(memory));
        let mut monitor = Box::new(crate::exclusive_monitor::ExclusiveMonitor::new(1));
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::from_shared_memory(0, memory)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::NO_OPTIMIZATIONS,
            unsafe_optimizations: false,
            global_monitor: Some(&mut *monitor as *mut _),
            fastmem_pointer: Some(fastmem_pointer),
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig {
                fastmem_exclusive_access: false,
                ..Default::default()
            },
        };

        let jit = A32Jit::new(config).expect("A32 JIT");
        let emitter = jit.inner.emitter.as_ref().expect("x64 emitter");

        assert!(!emitter.emit_config.memory.fastmem_exclusive_access);
    }

    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    #[test]
    fn test_a64_exclusive_fastmem_fault_uses_raw_fallback_without_deadlock() {
        let code: [u32; 8] = [
            0xD284_0001, // mov x1, #0x2000
            0x5282_4683, // mov w3, #0x1234
            0x885F_FC20, // ldaxr w0, [x1]
            0x8802_FC23, // stlxr w2, w3, [x1]
            0xD284_0206, // mov x6, #0x2010
            0xC87F_14C4, // ldxp x4, x5, [x6]
            0xC829_20C7, // stxp w9, x7, x8, [x6]
            0xD400_0001, // svc #0
        ];
        let mut memory = vec![0; 0x3000];
        for (index, word) in code.iter().enumerate() {
            let offset = 0x1000 + index * 4;
            memory[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        }
        let old_lo = 0x1122_3344_5566_7788u64;
        let old_hi = 0x99AA_BBCC_DDEE_FF00u64;
        memory[0x2010..0x2018].copy_from_slice(&old_lo.to_le_bytes());
        memory[0x2018..0x2020].copy_from_slice(&old_hi.to_le_bytes());
        let memory = Arc::new(Mutex::new(memory));
        let fastmem = TestFastmemMapping::new(0x1_0000);
        let mut monitor = Box::new(crate::exclusive_monitor::ExclusiveMonitor::new(1));

        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::from_shared_memory(0, memory.clone())),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::NO_OPTIMIZATIONS,
            unsafe_optimizations: false,
            global_monitor: Some(&mut *monitor as *mut _),
            fastmem_pointer: Some(fastmem.ptr.cast()),
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig {
                fastmem_address_space_bits: 16,
                silently_mirror_fastmem: true,
                fastmem_exclusive_access: true,
                recompile_on_exclusive_fastmem_failure: true,
                recompile_on_fastmem_failure: true,
                page_table_present: false,
                page_table_address_space_bits: 16,
                silently_mirror_page_table: true,
                absolute_offset_page_table: true,
                page_table_pointer_mask_bits: 0,
                detect_misaligned_access_via_page_table: 0,
                only_detect_misalignment_via_page_table_on_page_boundary: false,
                check_halt_on_memory_access: false,
                processor_id: 0,
            },
        };
        let mut jit = A64Jit::new(config).expect("A64 JIT");
        jit.set_pc(0x1000);
        let new_lo = 0x0123_4567_89AB_CDEFu64;
        let new_hi = 0xFEDC_BA98_7654_3210u64;
        jit.set_register(7, new_lo);
        jit.set_register(8, new_hi);
        let halt = jit.run();

        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(jit.get_register(2), 0);
        let memory_value =
            u32::from_le_bytes(memory.lock().unwrap()[0x2000..0x2004].try_into().unwrap());
        assert_eq!(memory_value, 0x1234);
        assert_eq!(jit.get_register(4), old_lo);
        assert_eq!(jit.get_register(5), old_hi);
        assert_eq!(jit.get_register(9), 0);
        let memory = memory.lock().unwrap();
        assert_eq!(
            u64::from_le_bytes(memory[0x2010..0x2018].try_into().unwrap()),
            new_lo
        );
        assert_eq!(
            u64::from_le_bytes(memory[0x2018..0x2020].try_into().unwrap()),
            new_hi
        );
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_ldxp_uses_exclusive_read_128_pair_return() {
        // LDXP X0, X1, [X2] ; SVC #0
        //
        // Regression guard for the A64 exclusive_read_128 trampoline ABI:
        // emit_exclusive_read(bitsize=128) expects the callback return in the
        // SysV RAX:RDX pair, not through a hidden ret_ptr. This test executes
        // the real LDXP path and verifies both loaded registers.
        let code: &[u32] = &[0xC87F_0440, 0xD400_0001];
        let lo = 0x1122_3344_5566_7788u64;
        let hi = 0x99AA_BBCC_DDEE_FF00u64;

        let mut memory = vec![0u8; 0x2000];
        for (i, &word) in code.iter().enumerate() {
            let offset = i * 4;
            memory[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        }
        memory[0x1000..0x1008].copy_from_slice(&lo.to_le_bytes());
        memory[0x1008..0x1010].copy_from_slice(&hi.to_le_bytes());

        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::from_memory(0x1000, memory)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(0x1000);
        jit.set_register(2, 0x2000);

        let _ = jit.run();

        assert_eq!(jit.get_register(0), lo);
        assert_eq!(jit.get_register(1), hi);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a64_ldr_q_uses_host_128_bit_return_abi() {
        let code: &[u32] = &[
            0x3DC0_0020, // LDR Q0, [X1]
            0xD400_0001, // SVC #0
        ];
        let lo = 0x0123_4567_89AB_CDEFu64;
        let hi = 0xFEDC_BA98_7654_3210u64;
        let mut memory = vec![0u8; 0x1000];
        for (index, word) in code.iter().copied().enumerate() {
            memory[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        memory[0x100..0x108].copy_from_slice(&lo.to_le_bytes());
        memory[0x108..0x110].copy_from_slice(&hi.to_le_bytes());
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::from_memory(0x1000, memory)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).expect("A64 JIT");
        jit.set_pc(0x1000);
        jit.set_register(1, 0x1100);

        let halt = jit.run();

        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(jit.get_vector(0), (lo, hi));
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a64_str_q_uses_host_128_bit_argument_abi() {
        let code: &[u32] = &[
            0x3D80_0020, // STR Q0, [X1]
            0xD400_0001, // SVC #0
        ];
        let lo = 0x0123_4567_89AB_CDEFu64;
        let hi = 0xFEDC_BA98_7654_3210u64;
        let memory = Arc::new(Mutex::new(vec![0u8; 0x2000]));
        for (index, word) in code.iter().copied().enumerate() {
            memory.lock().unwrap()[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::from_shared_memory(0x1000, memory.clone())),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).expect("A64 JIT");
        jit.set_pc(0x1000);
        jit.set_register(1, 0x1100);
        jit.set_vector(0, lo, hi);

        let halt = jit.run();

        assert!(halt.contains(HaltReason::SVC));
        let memory = memory.lock().unwrap();
        assert_eq!(
            u64::from_le_bytes(memory[0x100..0x108].try_into().unwrap()),
            lo
        );
        assert_eq!(
            u64::from_le_bytes(memory[0x108..0x110].try_into().unwrap()),
            hi
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a64_stxp_uses_host_128_bit_argument_abi() {
        let code: &[u32] = &[
            0xC87F_14C4, // LDXP X4, X5, [X6]
            0xC829_20C7, // STXP W9, X7, X8, [X6]
            0xD400_0001, // SVC #0
        ];
        let old_lo = 0x1122_3344_5566_7788u64;
        let old_hi = 0x99AA_BBCC_DDEE_FF00u64;
        let new_lo = 0x0123_4567_89AB_CDEFu64;
        let new_hi = 0xFEDC_BA98_7654_3210u64;
        let mut contents = vec![0u8; 0x2000];
        for (index, word) in code.iter().copied().enumerate() {
            contents[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        contents[0x100..0x108].copy_from_slice(&old_lo.to_le_bytes());
        contents[0x108..0x110].copy_from_slice(&old_hi.to_le_bytes());
        let memory = Arc::new(Mutex::new(contents));
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::from_shared_memory(0x1000, memory.clone())),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).expect("A64 JIT");
        jit.set_pc(0x1000);
        jit.set_register(6, 0x1100);
        jit.set_register(7, new_lo);
        jit.set_register(8, new_hi);

        let halt = jit.run();

        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(jit.get_register(4), old_lo);
        assert_eq!(jit.get_register(5), old_hi);
        assert_eq!(jit.get_register(9), 0);
        let memory = memory.lock().unwrap();
        assert_eq!(
            u64::from_le_bytes(memory[0x100..0x108].try_into().unwrap()),
            new_lo
        );
        assert_eq!(
            u64::from_le_bytes(memory[0x108..0x110].try_into().unwrap()),
            new_hi
        );
    }

    fn run_a64_alu(code: &[u32], setup: impl FnOnce(&mut A64Jit)) -> A64Jit {
        run_a64_alu_with_optimizations(code, OptimizationFlag::NO_OPTIMIZATIONS, setup)
    }

    fn run_a64_alu_with_optimizations(
        code: &[u32],
        optimizations: OptimizationFlag,
        setup: impl FnOnce(&mut A64Jit),
    ) -> A64Jit {
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(0x1000);
        setup(&mut jit);
        let _ = jit.run();
        jit
    }

    #[test]
    fn test_a64_crc32x_then_crc32b_matches_iso_crc32() {
        let crc32x = run_a64_alu(&[0x9ACE_4D0E, 0xD400_0001], |jit| {
            jit.set_register(8, u32::MAX as u64);
            jit.set_register(14, u64::from_le_bytes(*b"set_arra"));
        });
        assert_eq!(crc32x.get_register(14), 0xB82F_A488);

        let crc32b = run_a64_alu(&[0x1ACB_41CB, 0xD400_0001], |jit| {
            jit.set_register(11, b'y' as u64);
            jit.set_register(14, 0xB82F_A488);
        });
        assert_eq!(crc32b.get_register(11), 0xCA02_ED2E);

        let code = &[
            0x9ACE_4D0E, // crc32x w14, w8, x14
            0x1ACB_41CB, // crc32b w11, w14, w11
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |jit| {
            jit.set_register(8, u32::MAX as u64);
            jit.set_register(11, b'y' as u64);
            jit.set_register(14, u64::from_le_bytes(*b"set_arra"));
        });

        assert_eq!(jit.get_register(11), 0xCA02_ED2E);
    }

    fn run_a64_until_svc(jit: &mut A64Jit) -> HaltReason {
        for _ in 0..8 {
            let halt = jit.run();
            if halt.contains(HaltReason::SVC) {
                return halt;
            }
            assert!(
                halt.is_empty(),
                "unexpected halt={halt:?} pc=0x{:x}",
                jit.get_pc()
            );
        }
        panic!("SVC was not reached; pc=0x{:x}", jit.get_pc());
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a64_fcvtn_single_to_half_updates_underflow_status() {
        let code = [
            0x0E21_6800, // FCVTN V0.4H, V0.4S
            0xD400_0001, // SVC #0
        ];
        let input = (0x3280_0000u64 << 32) | 0x3280_0000;
        let jit = run_a64_alu(&code, |jit| {
            jit.set_vector(0, input, input);
            jit.set_fpsr(0);
        });

        assert_eq!(jit.get_vector(0), (0, 0));
        assert_eq!(jit.get_fpsr() & 0x18, 0x18);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a64_fcvtl_half_to_single_updates_nan_status() {
        let code = [
            0x0E21_7800, // FCVTL V0.4S, V0.4H
            0xD400_0001, // SVC #0
        ];
        let jit = run_a64_alu(&code, |jit| {
            jit.set_vector(0, 0x0001_7c01_c000_3c00, 0);
            jit.set_fpcr(1 << 25);
            jit.set_fpsr(0);
        });

        assert_eq!(
            jit.get_vector(0),
            (0xc000_0000_3f80_0000, 0x3380_0000_7fc0_0000)
        );
        assert_eq!(jit.get_fpsr() & 1, 1);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a64_fdiv_default_nan_mode_canonicalizes_result() {
        let code = [
            0x1E20_19CB, // FDIV S11, S14, S0
            0xD400_0001, // SVC #0
        ];
        let jit = run_a64_alu(&code, |jit| {
            jit.set_vector(14, 0, 0);
            jit.set_vector(0, 0, 0);
            jit.set_fpcr(1 << 25);
            jit.set_fpsr(0);
        });

        assert_eq!(jit.get_vector(11).0 as u32, 0x7fc0_0000);
        assert_eq!(jit.get_fpsr() & 1, 1);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a64_fcvtzu_fraction_sets_inexact_status() {
        let code = [
            0x1E39_000A, // FCVTZU W10, S0
            0xD400_0001, // SVC #0
        ];
        let jit = run_a64_alu(&code, |jit| {
            jit.set_vector(0, 1.5f32.to_bits() as u64, 0);
            jit.set_fpsr(0);
        });

        assert_eq!(jit.get_register(10), 1);
        assert_eq!(jit.get_fpsr() & (1 << 4), 1 << 4);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a64_fcvtzu_fixed_point_optimized_register_allocation() {
        let code = [
            0x1E19_FC20, // FCVTZU W0, S1, #1
            0xD400_0001, // SVC #0
        ];
        let jit = run_a64_alu_with_optimizations(
            &code,
            OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
            |jit| {
                jit.set_vector(1, 1.5f32.to_bits() as u64, 0);
                jit.set_fpsr(0);
            },
        );

        assert_eq!(jit.get_register(0), 3);
        assert_eq!(jit.get_fpsr() & (1 << 4), 0);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a64_fcvtzu_double_to_u32_saturates_without_invalid_operation() {
        let code = [
            0x1E79_0008, // FCVTZU W8, D0
            0xD400_0001, // SVC #0
        ];
        let initial_fpsr = 0x0800_001c;
        let jit = run_a64_alu(&code, |jit| {
            jit.set_vector(0, 0x43cd_9a29_e4a6_d831, 0);
            jit.set_fpsr(initial_fpsr);
        });

        assert_eq!(jit.get_register(8), u32::MAX as u64);
        assert_eq!(jit.get_fpsr(), initial_fpsr);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a64_scalar_frecpx_uses_fpcr_and_updates_fpsr() {
        let code = [
            0x5EA1_F820, // FRECPX S0, S1
            0xD400_0001, // SVC #0
        ];
        let jit = run_a64_alu(&code, |jit| {
            jit.set_vector(1, 0x7f80_0001, u64::MAX);
            jit.set_fpcr(1 << 25);
            jit.set_fpsr(1 << 1);
        });

        assert_eq!(jit.get_vector(0), (0x7fc0_0000, 0));
        assert_eq!(jit.get_fpsr(), (1 << 1) | 1);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a64_scalar_frecps_uses_upstream_native_result() {
        let code = [
            0x5E22_FC20, // FRECPS S0, S1, S2
            0xD400_0001, // SVC #0
        ];
        let jit = run_a64_alu(&code, |jit| {
            jit.set_vector(1, 4.0f32.to_bits() as u64, u64::MAX);
            jit.set_vector(2, 0.25f32.to_bits() as u64, u64::MAX);
            jit.set_fpsr(0);
        });

        assert_eq!(jit.get_vector(0), (1.0f32.to_bits() as u64, 0));
        assert_eq!(jit.get_fpsr(), 0);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a64_scalar_frecps_exception_uses_reference_fallback() {
        let code = [
            0x5E22_FC20, // FRECPS S0, S1, S2
            0xD400_0001, // SVC #0
        ];
        let jit = run_a64_alu(&code, |jit| {
            jit.set_vector(1, 0.0f32.to_bits() as u64, u64::MAX);
            jit.set_vector(2, f32::INFINITY.to_bits() as u64, u64::MAX);
            jit.set_fpsr(0);
        });

        assert_eq!(jit.get_vector(0), (2.0f32.to_bits() as u64, 0));
        // The native FMA is attempted before its NaN redirects execution to
        // the architectural helper, so Eden retains IOC in MXCSR.
        assert_eq!(jit.get_fpsr(), 1);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a64_scalar_frsqrts_uses_fused_common_fp_path() {
        let code = [
            0x5EA2_FC20, // FRSQRTS S0, S1, S2
            0xD400_0001, // SVC #0
        ];
        let jit = run_a64_alu(&code, |jit| {
            jit.set_vector(1, 4.0f32.to_bits() as u64, u64::MAX);
            jit.set_vector(2, 0.5f32.to_bits() as u64, u64::MAX);
            jit.set_fpsr(0);
        });

        assert_eq!(jit.get_vector(0), (0.5f32.to_bits() as u64, 0));
        assert_eq!(jit.get_fpsr(), 0);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a64_scalar_frsqrts_near_infinity_matches_upstream_fallback() {
        let code = [
            0x5EB8_FCAD, // FRSQRTS S13, S5, S24
            0xD400_0001, // SVC #0
        ];
        let jit = run_a64_alu(&code, |jit| {
            jit.set_vector(5, 0xFC6A_0206, 0);
            jit.set_vector(24, 0xFC6A_0206, 0);
            jit.set_fpcr(0x0040_0000);
            jit.set_fpsr(0);
        });

        assert_eq!(jit.get_vector(13), (0xFF7F_FFFF, 0));
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a64_vector_frecps_auto_uses_upstream_native_result() {
        let code = [
            0x4E22_FC20, // FRECPS V0.4S, V1.4S, V2.4S
            0xD400_0001, // SVC #0
        ];
        let auto = OptimizationFlag::ALL_SAFE_OPTIMIZATIONS
            | OptimizationFlag::UNSAFE_UNFUSE_FMA
            | OptimizationFlag::UNSAFE_IGNORE_STANDARD_FPCR_VALUE
            | OptimizationFlag::UNSAFE_INACCURATE_NAN;
        let jit = run_a64_alu_with_optimizations(&code, auto, |jit| {
            let v1_low = (4.0f32.to_bits() as u64) | ((8.0f32.to_bits() as u64) << 32);
            let v1_high = (16.0f32.to_bits() as u64) | ((32.0f32.to_bits() as u64) << 32);
            let v2_low = (0.25f32.to_bits() as u64) | ((0.125f32.to_bits() as u64) << 32);
            let v2_high = (0.0625f32.to_bits() as u64) | ((0.03125f32.to_bits() as u64) << 32);
            jit.set_vector(1, v1_low, v1_high);
            jit.set_vector(2, v2_low, v2_high);
            jit.set_fpsr(0);
        });

        let one_pair = (1.0f32.to_bits() as u64) | ((1.0f32.to_bits() as u64) << 32);
        assert_eq!(jit.get_vector(0), (one_pair, one_pair));
        assert_eq!(jit.get_fpsr(), 0);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a64_vector_frsqrts_auto_uses_upstream_native_result() {
        let code = [
            0x4EA2_FC20, // FRSQRTS V0.4S, V1.4S, V2.4S
            0xD400_0001, // SVC #0
        ];
        let auto = OptimizationFlag::ALL_SAFE_OPTIMIZATIONS
            | OptimizationFlag::UNSAFE_UNFUSE_FMA
            | OptimizationFlag::UNSAFE_IGNORE_STANDARD_FPCR_VALUE
            | OptimizationFlag::UNSAFE_INACCURATE_NAN;
        let jit = run_a64_alu_with_optimizations(&code, auto, |jit| {
            let v1_low = (4.0f32.to_bits() as u64) | ((8.0f32.to_bits() as u64) << 32);
            let v1_high = (16.0f32.to_bits() as u64) | ((32.0f32.to_bits() as u64) << 32);
            let v2_low = (0.5f32.to_bits() as u64) | ((0.25f32.to_bits() as u64) << 32);
            let v2_high = (0.125f32.to_bits() as u64) | ((0.0625f32.to_bits() as u64) << 32);
            jit.set_vector(1, v1_low, v1_high);
            jit.set_vector(2, v2_low, v2_high);
            jit.set_fpsr(0);
        });

        let half_pair = (0.5f32.to_bits() as u64) | ((0.5f32.to_bits() as u64) << 32);
        assert_eq!(jit.get_vector(0), (half_pair, half_pair));
        assert_eq!(jit.get_fpsr(), 0);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a64_vector_frsqrts_exception_uses_reference_fallback() {
        let code = [
            0x4EA2_FC20, // FRSQRTS V0.4S, V1.4S, V2.4S
            0xD400_0001, // SVC #0
        ];
        let jit = run_a64_alu(&code, |jit| {
            let v1_low = (0.0f32.to_bits() as u64) | ((4.0f32.to_bits() as u64) << 32);
            let v1_high = (8.0f32.to_bits() as u64) | ((16.0f32.to_bits() as u64) << 32);
            let v2_low = (f32::INFINITY.to_bits() as u64) | ((0.5f32.to_bits() as u64) << 32);
            let v2_high = (0.25f32.to_bits() as u64) | ((0.125f32.to_bits() as u64) << 32);
            jit.set_vector(1, v1_low, v1_high);
            jit.set_vector(2, v2_low, v2_high);
            jit.set_fpsr(0);
        });

        let low = (1.5f32.to_bits() as u64) | ((0.5f32.to_bits() as u64) << 32);
        let high = (0.5f32.to_bits() as u64) | ((0.5f32.to_bits() as u64) << 32);
        assert_eq!(jit.get_vector(0), (low, high));
        // The speculative native FMA raises IOC for 0 * infinity before the
        // vector is redirected to the reference fallback. Upstream retains
        // that sticky host exception in FPSR.
        assert_eq!(jit.get_fpsr(), 1);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a64_fadd_vector_alias_default_nan() {
        let code = [
            0x0E24_D4A4, // FADD V4.2S, V5.2S, V4.2S
            0xD400_0001, // SVC #0
        ];
        let jit = run_a64_alu(&code, |jit| {
            let v4 = (4.0f32.to_bits() as u64) | ((0xffd4_e6ddu64) << 32);
            let v5 = (1.0f32.to_bits() as u64) | ((2.0f32.to_bits() as u64) << 32);
            jit.set_vector(4, v4, 0);
            jit.set_vector(5, v5, 0);
            jit.set_fpcr(1 << 25);
            jit.set_fpsr(0);
        });

        assert_eq!(jit.get_vector(4), (0x7fc0_0000_40a0_0000, 0));
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a64_fmax_flushes_denormal_without_setting_idc() {
        let code = [
            0x1E20_4863, // FMAX S3, S3, S0
            0xD400_0001, // SVC #0
        ];
        let jit = run_a64_alu(&code, |jit| {
            jit.set_vector(3, 1, 0);
            jit.set_vector(0, 0, 0);
            jit.set_fpcr(1 << 24);
            jit.set_fpsr(0);
        });

        assert_eq!(jit.get_vector(3).0 as u32, 0);
        assert_eq!(jit.get_fpsr() & (1 << 7), 0);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a64_fcvt_single_to_double_default_nan() {
        let code = [
            0x1E22_C120, // FCVT D0, S9
            0xD400_0001, // SVC #0
        ];
        let jit = run_a64_alu(&code, |jit| {
            jit.set_vector(9, 0x7ff0_62c7, 0);
            jit.set_fpcr(1 << 25);
            jit.set_fpsr(0);
        });

        assert_eq!(jit.get_vector(0).0, 0x7ff8_0000_0000_0000);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn test_a64_virtual_tail_call_preserves_x0_across_indirect_branch() {
        let code: &[u32] = &[
            0xAA13_03E0, // mov x0, x19
            0xA941_53F3, // ldp x19, x20, [sp, #0x10]
            0xA8C2_7BFD, // ldp x29, x30, [sp], #0x20
            0xF940_0C42, // ldr x2, [x2, #0x18]
            0xAA02_03F0, // mov x16, x2
            0xD61F_0200, // br x16
            0xD503_201F, // nop
            0xD503_201F, // nop
            0xF900_0060, // str x0, [x3]
            0xD400_0001, // svc #0
        ];
        let mut memory = vec![0u8; 0x4000];
        for (i, word) in code.iter().enumerate() {
            memory[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        memory[0x1018..0x1020].copy_from_slice(&0x1020u64.to_le_bytes());

        let shared_memory = Arc::new(Mutex::new(memory));
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::from_shared_memory(
                0x1000,
                Arc::clone(&shared_memory),
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::BLOCK_LINKING
                | OptimizationFlag::RETURN_STACK_BUFFER
                | OptimizationFlag::FAST_DISPATCH,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(0x1000);
        jit.set_sp(0x3000);
        jit.set_register(2, 0x2000);
        jit.set_register(3, 0x4000);
        jit.set_register(19, 0x1234_5678_9ABC_DEF0);

        let halt = run_a64_until_svc(&mut jit);

        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(
            u64::from_le_bytes(
                shared_memory.lock().unwrap()[0x3000..0x3008]
                    .try_into()
                    .unwrap()
            ),
            0x1234_5678_9ABC_DEF0
        );
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_variable_w_shifts_preserve_32_bit_operands_in_mixed_block() {
        let code: &[u32] = &[
            0x1AC1_20E2, // lsl  w2, w7, w1
            0x1100_0421, // add  w1, w1, #1
            0x6A05_009F, // tst  w4, w5
            0x1AC1_28C4, // asr  w4, w6, w1
            0x2A02_0062, // orr  w2, w3, w2
            0x1A83_1043, // csel w3, w2, w3, ne
            0xD400_0001, // svc  #0
        ];
        let jit = run_a64_alu(code, |j| {
            j.set_register(1, 5);
            j.set_register(3, 16);
            j.set_register(4, 64);
            j.set_register(5, 64);
            j.set_register(6, 64);
            j.set_register(7, 1);
        });

        assert_eq!(jit.get_register(1), 6);
        assert_eq!(jit.get_register(2), 48);
        assert_eq!(jit.get_register(3), 48);
        assert_eq!(jit.get_register(4), 1);
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_movn_w_zero_extends_into_x_register() {
        // MOVN W11, #0; SVC #0. A 32-bit GPR write clears the upper half of X11.
        let code: &[u32] = &[0x1280_000B, 0xD400_0001];
        let jit = run_a64_alu(code, |_| {});

        assert_eq!(jit.get_register(11), 0x0000_0000_FFFF_FFFF);
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_variable_w_shift_with_destination_reusing_count_register() {
        let code: &[u32] = &[
            0x5280_0027, // mov w7, #1
            0x5100_0668, // sub w8, w19, #1
            0x5100_0A66, // sub w6, w19, #2
            0x1AC8_20E8, // lsl w8, w7, w8
            0x5100_0508, // sub w8, w8, #1
            0x1AC6_20E6, // lsl w6, w7, w6
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| j.set_register(19, 8));

        assert_eq!(jit.get_register(8), 127);
        assert_eq!(jit.get_register(6), 64);
    }

    const NZCV_C: u32 = 1 << 29;
    const NZCV_Z: u32 = 1 << 30;

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_adc_uses_carry_in() {
        // ADC X0, X1, X2 ; SVC #0
        let code: &[u32] = &[0x9A02_0020, 0xD400_0001];
        // C=1: X0 = 10 + 20 + 1 = 31
        let jit = run_a64_alu(code, |j| {
            j.set_register(1, 10);
            j.set_register(2, 20);
            j.set_pstate(NZCV_C);
        });
        assert_eq!(jit.get_register(0), 31);
        // C=0: X0 = 10 + 20 + 0 = 30
        let jit = run_a64_alu(code, |j| {
            j.set_register(1, 10);
            j.set_register(2, 20);
            j.set_pstate(0);
        });
        assert_eq!(jit.get_register(0), 30);
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_fcvtzs_w_fixed_scales_before_truncating() {
        let code: &[u32] = &[
            0x1E18_E020, // FCVTZS W0, S1, #8
            0xD400_0001, // SVC #0
        ];
        let jit = run_a64_alu(code, |j| {
            j.set_vector(1, 1.5f32.to_bits() as u64, 0);
        });
        assert_eq!(jit.get_register(0), 384);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a64_fcvtas_x_from_d_rounds_ties_away_from_zero() {
        let convert = |value: f64| {
            let code: &[u32] = &[
                0x9E64_03C2, // FCVTAS X2, D30
                0xD400_0001, // SVC #0
            ];
            let jit = run_a64_alu(code, |jit| {
                jit.set_vector(30, value.to_bits(), 0);
            });
            (jit.get_register(2), jit.get_fpsr())
        };

        let (result, fpsr) = convert(0.911);
        assert_eq!(result, 1);
        assert_ne!(fpsr & (1 << 4), 0, "inexact conversion must set IXC");
        assert_eq!(convert(-0.911).0, (-1i64) as u64);
        assert_eq!(convert(0.5).0, 1);
        assert_eq!(convert(-0.5).0, (-1i64) as u64);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a64_fp_to_signed_x_native_rounding_and_saturation() {
        let convert = |instruction: u32, value: f64| {
            let code = [instruction, 0xD400_0001];
            let jit = run_a64_alu(&code, |jit| {
                jit.set_vector(30, value.to_bits(), 0);
            });
            jit.get_register(2)
        };

        assert_eq!(convert(0x9E68_03C2, 0.1), 1); // FCVTPS X2, D30
        assert_eq!(convert(0x9E68_03C2, -0.1), 0);
        assert_eq!(convert(0x9E78_03C2, 9.75), 9); // FCVTZS X2, D30
        assert_eq!(convert(0x9E78_03C2, f64::NAN), 0);
        assert_eq!(convert(0x9E78_03C2, f64::INFINITY), i64::MAX as u64);
        assert_eq!(convert(0x9E78_03C2, f64::NEG_INFINITY), i64::MIN as u64);
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_sbc_uses_carry_in() {
        // SBC X0, X1, X2 ; SVC #0  — SubWithCarry(X1, X2, C) = X1 + ~X2 + C
        let code: &[u32] = &[0xDA02_0020, 0xD400_0001];
        // C=1 → plain subtract: 100 - 30 = 70
        let jit = run_a64_alu(code, |j| {
            j.set_register(1, 100);
            j.set_register(2, 30);
            j.set_pstate(NZCV_C);
        });
        assert_eq!(jit.get_register(0), 70);
        // C=0 → 100 - 30 - 1 = 69
        let jit = run_a64_alu(code, |j| {
            j.set_register(1, 100);
            j.set_register(2, 30);
            j.set_pstate(0);
        });
        assert_eq!(jit.get_register(0), 69);
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_adcs_sets_carry_and_zero() {
        // ADCS X0, X1, X2 ; SVC #0 — should set C on unsigned overflow, Z when result 0.
        let code: &[u32] = &[0xBA02_0020, 0xD400_0001];
        // 0xFFFF_FFFF_FFFF_FFFF + 1 + 0 = 0 with carry out, Z=1, C=1
        let jit = run_a64_alu(code, |j| {
            j.set_register(1, u64::MAX);
            j.set_register(2, 1);
            j.set_pstate(0);
        });
        assert_eq!(jit.get_register(0), 0);
        let nzcv = jit.get_pstate() & 0xF000_0000;
        assert_ne!(nzcv & (1 << 30), 0, "Z flag must be set");
        assert_ne!(nzcv & (1 << 29), 0, "C flag must be set on carry out");
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_adcs_32bit_zero_extends() {
        // ADCS W0, W1, W2 (sf=0) ; SVC #0 — 32-bit result must zero-extend, not
        // sign-extend, into X0. Regression guard for upper-32-all-ones address bugs.
        let code: &[u32] = &[0x3A02_0020, 0xD400_0001];
        // W1 = 0xFFFF_FFFF, W2 = 1, C=0 → 0 (32-bit), upper 32 bits of X0 must be 0.
        let jit = run_a64_alu(code, |j| {
            j.set_register(1, 0x0000_0000_FFFF_FFFF);
            j.set_register(2, 1);
            j.set_pstate(0);
        });
        assert_eq!(jit.get_register(0), 0);
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_fcsel_s_uses_scalar_xmm_operands_as_gprs() {
        // FCSEL S0, S1, S2, EQ ; SVC #0
        //
        // A64GetS is typed as a scalar integer in the IR but is physically
        // backed by an XMM register on x64. ConditionalSelect32 must select
        // the low 32-bit scalar lane, not ask regalloc for a full 128-bit GPR.
        let code: &[u32] = &[0x1E22_0C20, 0xD400_0001];
        let then_bits = 1.25f32.to_bits() as u64;
        let else_bits = (-2.5f32).to_bits() as u64;

        let jit = run_a64_alu(code, |j| {
            j.set_vector(1, 0xAAAA_AAAA_0000_0000 | then_bits, u64::MAX);
            j.set_vector(2, 0xBBBB_BBBB_0000_0000 | else_bits, u64::MAX);
            j.set_pstate(NZCV_Z);
        });
        assert_eq!(jit.get_vector(0), (then_bits, 0));

        let jit = run_a64_alu(code, |j| {
            j.set_vector(1, 0xAAAA_AAAA_0000_0000 | then_bits, u64::MAX);
            j.set_vector(2, 0xBBBB_BBBB_0000_0000 | else_bits, u64::MAX);
            j.set_pstate(0);
        });
        assert_eq!(jit.get_vector(0), (else_bits, 0));
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_fcmpe_cset_cmp_fcsel_clamps_negative_scalar_to_zero() {
        let code: &[u32] = &[
            0x1E20_22F8, // fcmpe s23, #0.0
            0x1A9F_D7E0, // cset w0, gt
            0x0F00_0419, // movi v25.2s, #0
            0x7100_001F, // cmp w0, #0
            0x1E39_1EF7, // fcsel s23, s23, s25, ne
            0xD400_0001, // svc #0
        ];

        let positive = 0.25f32.to_bits() as u64;
        let jit = run_a64_alu(code, |j| {
            j.set_vector(23, positive, u64::MAX);
            j.set_vector(25, u64::MAX, u64::MAX);
        });
        assert_eq!(jit.get_vector(23), (positive, 0));

        let jit = run_a64_alu(code, |j| {
            j.set_vector(23, (-0.25f32).to_bits() as u64, u64::MAX);
            j.set_vector(25, u64::MAX, u64::MAX);
        });
        assert_eq!(jit.get_vector(23), (0, 0));
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_str_d_from_rev64_2s_writes_low_64_bits() {
        // LDR D0, [X0] ; REV64 V0.2S, V0.2S ; STR D0, [X1] ; LDR X2, [X1] ; SVC #0
        //
        // A guest can build a 64-bit GPU class value through a vector path and
        // store it with STR Dn. The scalar memory write must extract the low
        // 64 bits from XMM, preserving both dwords after the REV64 swap.
        let code: &[u32] = &[
            0xFD40_0000,
            0x0EA0_0800,
            0xFD00_0020,
            0xF940_0022,
            0xD400_0001,
        ];
        let mut memory = vec![0u8; 0x3000];
        for (i, &word) in code.iter().enumerate() {
            let offset = i * 4;
            memory[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        }
        memory[0x1000..0x1008].copy_from_slice(&0x0000_B0B5_0000_A140u64.to_le_bytes());

        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::from_memory(0x1000, memory)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(0x1000);
        jit.set_register(0, 0x2000);
        jit.set_register(1, 0x2010);

        let halt = jit.run();

        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(jit.get_register(2), 0x0000_A140_0000_B0B5);
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_rev32_8h_reverses_halfwords_within_words() {
        // REV32 V0.8H, V1.8H ; SVC #0
        let code: &[u32] = &[0x6E60_0820, 0xD400_0001];
        let jit = run_a64_alu(code, |j| {
            j.set_vector(1, 0x0004_0003_0002_0001, 0x0008_0007_0006_0005);
        });
        assert_eq!(
            jit.get_vector(0),
            (0x0003_0004_0001_0002, 0x0007_0008_0005_0006)
        );
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_vector_reverse_group_family_matches_upstream_sequences() {
        let code: &[u32] = &[
            0x4E20_1A00, // REV16 V0.16B, V16.16B
            0x6E20_0A01, // REV32 V1.16B, V16.16B
            0x6E60_0A02, // REV32 V2.8H, V16.8H
            0x4E20_0A03, // REV64 V3.16B, V16.16B
            0x4E60_0A04, // REV64 V4.8H, V16.8H
            0x4EA0_0A05, // REV64 V5.4S, V16.4S
            0xD400_0001, // SVC #0
        ];
        let input = core::array::from_fn::<_, 16, _>(|index| index as u8);
        let pack = |bytes: [u8; 16]| {
            (
                u64::from_le_bytes(bytes[..8].try_into().unwrap()),
                u64::from_le_bytes(bytes[8..].try_into().unwrap()),
            )
        };
        let jit = run_a64_alu(code, |j| {
            let (lo, hi) = pack(input);
            j.set_vector(16, lo, hi);
        });

        assert_eq!(
            jit.get_vector(0),
            pack([1, 0, 3, 2, 5, 4, 7, 6, 9, 8, 11, 10, 13, 12, 15, 14])
        );
        assert_eq!(
            jit.get_vector(1),
            pack([3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8, 15, 14, 13, 12])
        );
        assert_eq!(
            jit.get_vector(2),
            pack([2, 3, 0, 1, 6, 7, 4, 5, 10, 11, 8, 9, 14, 15, 12, 13])
        );
        assert_eq!(
            jit.get_vector(3),
            pack([7, 6, 5, 4, 3, 2, 1, 0, 15, 14, 13, 12, 11, 10, 9, 8])
        );
        assert_eq!(
            jit.get_vector(4),
            pack([6, 7, 4, 5, 2, 3, 0, 1, 14, 15, 12, 13, 10, 11, 8, 9])
        );
        assert_eq!(
            jit.get_vector(5),
            pack([4, 5, 6, 7, 0, 1, 2, 3, 12, 13, 14, 15, 8, 9, 10, 11])
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a64_trn1_trn2_all_element_sizes_use_native_x64_sequences() {
        let code: &[u32] = &[
            0x4E11_2A00, // TRN1 V0.16B, V16.16B, V17.16B
            0x4E11_6A01, // TRN2 V1.16B, V16.16B, V17.16B
            0x4E51_2A02, // TRN1 V2.8H, V16.8H, V17.8H
            0x4E51_6A03, // TRN2 V3.8H, V16.8H, V17.8H
            0x4E91_2A04, // TRN1 V4.4S, V16.4S, V17.4S
            0x4E91_6A05, // TRN2 V5.4S, V16.4S, V17.4S
            0x4ED1_2A06, // TRN1 V6.2D, V16.2D, V17.2D
            0x4ED1_6A07, // TRN2 V7.2D, V16.2D, V17.2D
            0xD400_0001, // SVC #0
        ];
        let a = core::array::from_fn::<_, 16, _>(|index| index as u8);
        let b = core::array::from_fn::<_, 16, _>(|index| 0x80 + index as u8);
        let pack = |bytes: &[u8; 16]| {
            (
                u64::from_le_bytes(bytes[..8].try_into().unwrap()),
                u64::from_le_bytes(bytes[8..].try_into().unwrap()),
            )
        };
        let expected = |lane_bytes: usize, part: usize| {
            let mut result = [0u8; 16];
            let lane_count = 16 / lane_bytes;
            for pair in 0..lane_count / 2 {
                let source_lane = pair * 2 + part;
                let source_start = source_lane * lane_bytes;
                let result_a_start = pair * 2 * lane_bytes;
                let result_b_start = result_a_start + lane_bytes;
                result[result_a_start..result_a_start + lane_bytes]
                    .copy_from_slice(&a[source_start..source_start + lane_bytes]);
                result[result_b_start..result_b_start + lane_bytes]
                    .copy_from_slice(&b[source_start..source_start + lane_bytes]);
            }
            pack(&result)
        };

        let jit = run_a64_alu(code, |jit| {
            let (a_lo, a_hi) = pack(&a);
            let (b_lo, b_hi) = pack(&b);
            jit.set_vector(16, a_lo, a_hi);
            jit.set_vector(17, b_lo, b_hi);
        });

        for (register, lane_bytes, part) in [
            (0, 1, 0),
            (1, 1, 1),
            (2, 2, 0),
            (3, 2, 1),
            (4, 4, 0),
            (5, 4, 1),
            (6, 8, 0),
            (7, 8, 1),
        ] {
            assert_eq!(
                jit.get_vector(register),
                expected(lane_bytes, part),
                "TRN{} with {}-bit elements",
                part + 1,
                lane_bytes * 8
            );
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn test_a64_frintn_8h_uses_software_fallback() {
        // FRINTN V0.8H, V1.8H ; SVC #0. Upstream deliberately uses the
        // software FPRoundInt<u16> fallback instead of host FP16 instructions.
        let code: &[u32] = &[0x4E79_8820, 0xD400_0001];
        let jit = run_a64_alu(code, |j| {
            j.set_vector(1, 0xc100_be00_4100_3e00, 0x8000_0000_bc00_3c00);
        });
        assert_eq!(
            jit.get_vector(0),
            (0xc000_c000_4000_4000, 0x8000_0000_bc00_3c00)
        );
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_fmul_vector_4s_dispatches_and_computes() {
        // FMUL V0.4S, V1.4S, V2.4S ; SVC #0 — regression for the previously
        // undispatched A64 vector-FP family (AnimH hit 0x6E22DC21,
        // FMUL V1.4S, ... which fell through to the interpreter / PrefetchAbort).
        let code: &[u32] = &[0x6E22_DC20, 0xD400_0001];
        let f = |x: f32| x.to_bits() as u64;
        // V1 = [1,2,3,4], V2 = [5,6,7,8] → V0 = [5,12,21,32]
        let mut jit = {
            let config = JitConfig {
                coprocessors: JitConfig::default_coprocessors(),
                callbacks: Box::new(MockCallbacks::new(0x1000, code)),
                enable_cycle_counting: false,
                code_cache_size: 4 * 1024 * 1024,
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
                memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
            };
            A64Jit::new(config).unwrap()
        };
        jit.set_pc(0x1000);
        jit.set_vector(1, (f(2.0) << 32) | f(1.0), (f(4.0) << 32) | f(3.0));
        jit.set_vector(2, (f(6.0) << 32) | f(5.0), (f(8.0) << 32) | f(7.0));
        let _ = jit.run();
        assert_eq!(
            jit.get_vector(0),
            ((f(12.0) << 32) | f(5.0), (f(32.0) << 32) | f(21.0)),
            "FMUL V0.4S must be lanewise product"
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a64_fp_pairwise_min_max_matches_arm_nan_and_signed_zero_rules() {
        let code: &[u32] = &[
            0x2E22_F420, // FMAXP V0.2S, V1.2S, V2.2S
            0x2EA2_F423, // FMINP V3.2S, V1.2S, V2.2S
            0x2E22_C424, // FMAXNMP V4.2S, V1.2S, V2.2S
            0x2EA2_C425, // FMINNMP V5.2S, V1.2S, V2.2S
            0xD400_0001, // SVC #0
        ];
        let qnan = 0x7FC5_4321u64;
        let one = 1.0f32.to_bits() as u64;
        let jit = run_a64_alu(code, |j| {
            j.set_vector(1, (one << 32) | qnan, 0);
            j.set_vector(2, 0x0000_0000_8000_0000, 0);
        });

        assert_eq!(jit.get_vector(0), (qnan, 0));
        assert_eq!(jit.get_vector(3), (0x8000_0000_0000_0000 | qnan, 0));
        assert_eq!(jit.get_vector(4), (one, 0));
        assert_eq!(jit.get_vector(5), (0x8000_0000_0000_0000 | one, 0));
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a64_fp_vector_min_max_matches_arm_nan_and_signed_zero_rules() {
        let code: &[u32] = &[
            0x4E22_F420, // FMAX V0.4S, V1.4S, V2.4S
            0x4EA2_F423, // FMIN V3.4S, V1.4S, V2.4S
            0x4E22_C424, // FMAXNM V4.4S, V1.4S, V2.4S
            0x4EA2_C425, // FMINNM V5.4S, V1.4S, V2.4S
            0xD400_0001, // SVC #0
        ];
        let qnan = 0x7FC5_4321u64;
        let snan = 0x7F81_2345u64;
        let quiet_snan = 0x7FC1_2345u64;
        let one = 1.0f32.to_bits() as u64;
        let two = 2.0f32.to_bits() as u64;
        let minus_three = (-3.0f32).to_bits() as u64;
        let jit = run_a64_alu(code, |j| {
            j.set_vector(1, qnan | (snan << 32), 0x8000_0000 | (two << 32));
            j.set_vector(2, one | (qnan << 32), minus_three << 32);
        });

        assert_eq!(jit.get_vector(0), (qnan | (quiet_snan << 32), two << 32));
        assert_eq!(
            jit.get_vector(3),
            (qnan | (quiet_snan << 32), 0x8000_0000 | (minus_three << 32))
        );
        assert_eq!(jit.get_vector(4), (one | (quiet_snan << 32), two << 32));
        assert_eq!(
            jit.get_vector(5),
            (one | (quiet_snan << 32), 0x8000_0000 | (minus_three << 32))
        );
        assert_ne!(jit.get_fpsr() & 1, 0, "signaling NaN must set FPSR.IOC");
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_jit_creation() {
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, &[0xD4000001])),
            enable_cycle_counting: true,
            code_cache_size: 4 * 1024 * 1024, // 4 MB for tests
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let jit = A64Jit::new(config);
        assert!(jit.is_ok(), "JIT creation failed: {:?}", jit.err());
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_jit_register_accessors() {
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, &[])),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).unwrap();

        jit.set_pc(0x1000);
        assert_eq!(jit.get_pc(), 0x1000);

        jit.set_sp(0x7FFF_0000);
        assert_eq!(jit.get_sp(), 0x7FFF_0000);

        jit.set_register(0, 42);
        assert_eq!(jit.get_register(0), 42);

        jit.set_register(30, 0xDEAD);
        assert_eq!(jit.get_register(30), 0xDEAD);

        jit.set_vector(0, 0x1111, 0x2222);
        assert_eq!(jit.get_vector(0), (0x1111, 0x2222));

        jit.set_tpidr_el0(0xABCD);
        assert_eq!(jit.get_tpidr_el0(), 0xABCD);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_halt_execution() {
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, &[])),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let jit = A64Jit::new(config).unwrap();

        jit.halt_execution(HaltReason::EXTERNAL_HALT);
        // Read back via the jit_state directly
        let halt = HaltReason::from_bits_truncate(jit.inner.jit_state.halt_reason);
        assert!(halt.contains(HaltReason::EXTERNAL_HALT));

        jit.clear_halt(HaltReason::EXTERNAL_HALT);
        let halt = HaltReason::from_bits_truncate(jit.inner.jit_state.halt_reason);
        assert!(!halt.contains(HaltReason::EXTERNAL_HALT));
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn external_halt_stops_a_running_direct_link_loop() {
        use std::sync::mpsc;
        use std::thread;
        use std::time::{Duration, Instant};

        let read_count = Arc::new(AtomicU64::new(0));
        let runner_count = Arc::clone(&read_count);
        let (halt_ptr_tx, halt_ptr_rx) = mpsc::sync_channel(0);
        let (result_tx, result_rx) = mpsc::sync_channel(0);

        let runner = thread::spawn(move || {
            let code = [
                0xF940_0020, // ldr x0, [x1]
                0x17FF_FFFF, // b 0x1000
            ];
            let config = JitConfig {
                coprocessors: JitConfig::default_coprocessors(),
                callbacks: Box::new(MockCallbacks::with_memory_read_64_count(
                    0x1000,
                    &code,
                    runner_count,
                )),
                enable_cycle_counting: false,
                code_cache_size: 4 * 1024 * 1024,
                optimizations: OptimizationFlag::BLOCK_LINKING,
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
                memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
            };
            let mut jit = A64Jit::new(config).expect("A64 JIT");
            jit.set_pc(0x1000);
            jit.set_register(1, 0x1100);
            halt_ptr_tx
                .send(jit.halt_reason_ptr() as usize)
                .expect("halt pointer receiver");

            let halt = jit.run();
            result_tx.send(halt).expect("halt result receiver");
        });

        let halt_ptr = halt_ptr_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("JIT construction timed out");
        let started_deadline = Instant::now() + Duration::from_secs(2);
        while read_count.load(Ordering::Acquire) < 100 {
            assert!(
                Instant::now() < started_deadline,
                "JIT did not enter the guest loop"
            );
            thread::yield_now();
        }

        let halt_reason = unsafe { &*(halt_ptr as *const AtomicU32) };
        halt_reason.fetch_or(HaltReason::EXTERNAL_HALT.bits(), Ordering::Release);

        let halt = result_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("running JIT did not observe EXTERNAL_HALT");
        assert!(halt.contains(HaltReason::EXTERNAL_HALT));
        runner.join().expect("JIT runner panicked");
    }

    #[test]
    fn test_a64_svc_reports_immediate_and_halts() {
        let svc_sink = Arc::new(AtomicU32::new(u32::MAX));
        let code: &[u32] = &[
            0xD400_04C1, // svc #0x26
        ];
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::with_svc_sink(0x1000, code, svc_sink.clone())),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(0x1000);

        let halt = jit.run();

        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(svc_sink.load(Ordering::Relaxed), 0x26);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn test_a64_stp_preindex_sp_writes_stack_and_writeback() {
        let svc_sink = Arc::new(AtomicU32::new(u32::MAX));
        let mut memory = vec![0u8; 0x3000];
        let code: &[u32] = &[
            0xA9BE_7BFD, // stp x29, x30, [sp, #-0x20]!
            0xD400_0001, // svc #0
        ];
        for (index, word) in code.iter().copied().enumerate() {
            let offset = 0x1000 + index * 4;
            memory[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        }
        let shared_memory = Arc::new(Mutex::new(memory));
        let callbacks = MockCallbacks::from_shared_memory(0, shared_memory.clone());
        let mut callbacks = callbacks;
        callbacks.svc_sink = Some(svc_sink.clone());
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(callbacks),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(0x1000);
        jit.set_sp(0x2000);
        jit.set_register(29, 0x1111_2222_3333_4444);
        jit.set_register(30, 0x5555_6666_7777_8888);

        let halt = jit.run();

        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(svc_sink.load(Ordering::Relaxed), 0);
        assert_eq!(jit.get_sp(), 0x1fe0);
        let memory = shared_memory.lock().unwrap();
        let fp = u64::from_le_bytes(memory[0x1fe0..0x1fe8].try_into().unwrap());
        let lr = u64::from_le_bytes(memory[0x1fe8..0x1ff0].try_into().unwrap());
        assert_eq!(fp, 0x1111_2222_3333_4444);
        assert_eq!(lr, 0x5555_6666_7777_8888);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn test_a64_fastmem_stp_preindex_sp_writes_stack_and_writeback() {
        let svc_sink = Arc::new(AtomicU32::new(u32::MAX));
        let mut memory = vec![0u8; 0x5000];
        let code: &[u32] = &[
            0xA9BE_7BFD, // stp x29, x30, [sp, #-0x20]!
            0xD400_0001, // svc #0
        ];
        for (index, word) in code.iter().copied().enumerate() {
            let offset = 0x1000 + index * 4;
            memory[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        }
        let shared_memory = Arc::new(Mutex::new(memory));
        let fastmem_pointer = {
            let mut memory = shared_memory.lock().unwrap();
            memory.as_mut_ptr()
        };
        let callbacks = MockCallbacks::from_shared_memory(0, shared_memory.clone());
        let mut callbacks = callbacks;
        callbacks.svc_sink = Some(svc_sink.clone());
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(callbacks),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::NO_OPTIMIZATIONS,
            unsafe_optimizations: false,
            global_monitor: None,
            fastmem_pointer: Some(fastmem_pointer),
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig {
                fastmem_address_space_bits: 39,
                silently_mirror_fastmem: false,
                fastmem_exclusive_access: false,
                recompile_on_exclusive_fastmem_failure: true,
                recompile_on_fastmem_failure: true,
                page_table_present: false,
                page_table_address_space_bits: 39,
                silently_mirror_page_table: false,
                absolute_offset_page_table: true,
                page_table_pointer_mask_bits: 0,
                detect_misaligned_access_via_page_table: 0,
                only_detect_misalignment_via_page_table_on_page_boundary: false,
                check_halt_on_memory_access: false,
                processor_id: 0,
            },
        };
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(0x1000);
        jit.set_sp(0x2000);
        jit.set_register(29, 0x1111_2222_3333_4444);
        jit.set_register(30, 0x5555_6666_7777_8888);

        let halt = jit.run();

        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(svc_sink.load(Ordering::Relaxed), 0);
        assert_eq!(jit.get_sp(), 0x1fe0);
        let memory = shared_memory.lock().unwrap();
        let fp = u64::from_le_bytes(memory[0x1fe0..0x1fe8].try_into().unwrap());
        let lr = u64::from_le_bytes(memory[0x1fe8..0x1ff0].try_into().unwrap());
        assert_eq!(fp, 0x1111_2222_3333_4444);
        assert_eq!(lr, 0x5555_6666_7777_8888);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn test_a64_svc_result_is_visible_to_cbnz_after_resume() {
        let svc_sink = Arc::new(AtomicU32::new(u32::MAX));
        let mut memory = vec![0u8; 0x3000];
        let code: &[u32] = &[
            0x9400_0008, // bl 0x1020
            0x3500_0080, // cbnz w0, 0x1014
            0x5280_2460, // mov w0, #0x123
            0xD400_0041, // svc #2
            0xD420_0000, // brk #0
            0x5280_8AC0, // mov w0, #0x456
            0xD400_0061, // svc #3
            0xD420_0000, // brk #0
            0xD400_0021, // svc #1
            0xD65F_03C0, // ret
        ];
        for (index, word) in code.iter().copied().enumerate() {
            let offset = 0x1000 + index * 4;
            memory[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        }
        let shared_memory = Arc::new(Mutex::new(memory));
        let callbacks = MockCallbacks::from_shared_memory(0, shared_memory);
        let mut callbacks = callbacks;
        callbacks.svc_sink = Some(svc_sink.clone());
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(callbacks),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(0x1000);

        let mut run_until_svc = |jit: &mut A64Jit| {
            for _ in 0..8 {
                let halt = jit.run();
                if halt.contains(HaltReason::SVC) {
                    return halt;
                }
                assert!(
                    halt.is_empty(),
                    "unexpected halt={halt:?} pc=0x{:x}",
                    jit.get_pc()
                );
            }
            panic!("SVC was not reached; pc=0x{:x}", jit.get_pc());
        };

        let first_halt = run_until_svc(&mut jit);
        assert!(
            first_halt.contains(HaltReason::SVC),
            "first halt={first_halt:?} pc=0x{:x} svc={}",
            jit.get_pc(),
            svc_sink.load(Ordering::Relaxed)
        );
        assert_eq!(svc_sink.load(Ordering::Relaxed), 1);
        jit.set_register(0, 0);
        jit.clear_halt(HaltReason::SVC);

        let second_halt = run_until_svc(&mut jit);
        assert!(second_halt.contains(HaltReason::SVC));
        assert_eq!(svc_sink.load(Ordering::Relaxed), 2);
        assert_eq!(jit.get_register(0), 0x123);
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_svc_preserves_x19_across_host_callback() {
        let code: &[u32] = &[
            0xD296_4A13, // movz x19, #0xb250
            0xF2A0_C593, // movk x19, #0x062c, lsl #16
            0xF2C0_0433, // movk x19, #0x0021, lsl #32
            0xD400_0001, // svc #0
        ];
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(0x1000);

        let halt = jit.run();

        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(jit.get_register(19), 0x0000_0021_062C_B250);
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_tbl_executes_table_lookup() {
        let code: &[u32] = &[
            0x4E02_2003, // tbl v3.16b, {v0.16b, v1.16b}, v2.16b
            0xD400_0001, // svc #0
        ];
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };

        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(0x1000);
        jit.set_vector(0, 0x0706_0504_0302_0100, 0x0F0E_0D0C_0B0A_0908);
        jit.set_vector(1, 0x1716_1514_1312_1110, 0x1F1E_1D1C_1B1A_1918);
        jit.set_vector(2, 0x211F_2011_100F_0100, 0x0302_0100_FF20_1E10);

        let halt = jit.run();
        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(
            jit.get_vector(3),
            (0x001F_0011_100F_0100, 0x0302_0100_0000_1E10)
        );
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_tbx_preserves_default_for_out_of_range_indices() {
        let code: &[u32] = &[
            0x4E02_3003, // tbx v3.16b, {v0.16b, v1.16b}, v2.16b
            0xD400_0001, // svc #0
        ];
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };

        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(0x1000);
        jit.set_vector(0, 0x0706_0504_0302_0100, 0x0F0E_0D0C_0B0A_0908);
        jit.set_vector(1, 0x1716_1514_1312_1110, 0x1F1E_1D1C_1B1A_1918);
        jit.set_vector(2, 0x211F_2011_100F_0100, 0x0302_0100_FF20_1E10);
        jit.set_vector(3, 0xA7A6_A5A4_A3A2_A1A0, 0xAFAE_ADAC_ABAA_A9A8);

        let halt = jit.run();
        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(
            jit.get_vector(3),
            (0xA71F_A511_100F_0100, 0x0302_0100_ABAA_1E10)
        );
    }

    /// Set the architectural Z flag and run b.ne. b.ne should not branch
    /// because Z=1 means equal.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_bne_with_preset_cpsr_nzcv_z_set() {
        let code: &[u32] = &[
            0x54000041, // b.ne pc+8 (target = 0x1008)
            0xD4000001, // 0x1004: svc #0 (fall-through if b.ne not taken)
            0xD4000021, // 0x1008: svc #1 (b.ne target)
        ];
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(0x1000);
        jit.set_pstate(NZCV_Z);
        let halt = run_a64_until_svc(&mut jit);
        assert!(halt.contains(HaltReason::SVC));

        let config2 = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit2 = A64Jit::new(config2).unwrap();
        jit2.set_pc(0x1000);
        jit2.set_pstate(0);
        let halt = run_a64_until_svc(&mut jit2);
        assert!(halt.contains(HaltReason::SVC));

        assert_eq!(
            jit.get_pc(),
            0x1008,
            "b.ne with Z=1 should NOT branch (fall through to svc #0)"
        );
        assert_eq!(
            jit2.get_pc(),
            0x100C,
            "b.ne with Z=0 should branch (to svc #1)"
        );
    }

    /// Sanity test: CBZ with x1=0 should branch (jump). x1!=0 should fall through.
    /// CBZ doesn't go through cpsr_nzcv — it reads the register directly. If this
    /// works but the cmp+b.ne test fails, the bug is in cpsr_nzcv read path.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_cbz_when_zero_branches() {
        // 0x1000: cbz x1, pc+8   (encoding: sf=1, opc=0, imm19=2, Rt=1)
        //   b400003F? No wait. cbz Rt, label:  sf 0110100 imm19 Rt
        //   sf=1, op=0 (CBZ), imm19=2 (target=pc+8), Rt=1
        //   Word: 1_0110100_0000000000000000010_00001 = 0xB4000041
        // 0x1004: svc #0  (fall-through if x1!=0)
        // 0x1008: svc #1  (cbz target if x1==0)
        let code: &[u32] = &[
            0xB4000041, // cbz x1, pc+8
            0xD4000001, // svc #0
            0xD4000021, // svc #1
        ];
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        // Test 1: x1 == 0, should TAKE branch → svc #1 → PC=0x100C
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(0x1000);
        jit.set_register(1, 0);
        let halt = run_a64_until_svc(&mut jit);
        assert!(halt.contains(HaltReason::SVC));

        let config2 = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        // Test 2: x1 != 0, should NOT branch → svc #0 → PC=0x1008
        let mut jit2 = A64Jit::new(config2).unwrap();
        jit2.set_pc(0x1000);
        jit2.set_register(1, 1);
        let halt = run_a64_until_svc(&mut jit2);
        assert!(halt.contains(HaltReason::SVC));

        assert_eq!(jit.get_pc(), 0x100C, "CBZ x1=0 should branch");
        assert_eq!(jit2.get_pc(), 0x1008, "CBZ x1!=0 should fall through");
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_rbit_w() {
        let code: &[u32] = &[
            0x5AC0_0020, // rbit w0, w1
            0xD400_0001, // svc #0
        ];
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(0x1000);
        jit.set_register(1, 7);

        let halt = jit.run();

        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(
            jit.get_register(0),
            0xE000_0000,
            "rbit(7) must reverse to 0xE0000000"
        );
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_rbit_x() {
        let code: &[u32] = &[
            0xDAC0_0020, // rbit x0, x1
            0xD400_0001, // svc #0
        ];
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(0x1000);
        jit.set_register(1, 7);

        let halt = jit.run();

        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(
            jit.get_register(0),
            0xE000_0000_0000_0000,
            "rbit(7) must reverse to 0xE000000000000000",
        );
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_extr_uses_rn_as_high_rm_as_low() {
        let code: &[u32] = &[
            0x93D8_FAD6, // extr x22, x22, x24, #0x3e
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            j.set_register(22, 0x0123_4567_89AB_CDEF);
            j.set_register(24, 0x8000_0000_0000_0003);
        });

        assert_eq!(jit.get_register(22), 0x048D_159E_26AF_37BE);
    }

    /// `cmp x1, x2` where x1==x2 must set Z=1 so the following `b.ne`
    /// does not take its branch.
    #[test]
    fn test_a64_cmp_w_eq_then_bne_falls_through() {
        // Code at PC=0x1000:
        //   0x1000: cmp w1, w2          (0x6B02003F = SUBS WZR, W1, W2)
        //   0x1004: b.ne pc+8           (0x54000041 = b.ne to 0x100C)
        //   0x1008: svc #0              (0xD4000001) — fall-through (b.ne not taken): GOOD
        //   0x100C: svc #1              (0xD4000021) — b.ne taken: BAD
        // Verify: ARM64 b.ne pc+8 encoding. Imm19 sign-extended * 4 + PC = target.
        // For target = pc+8, offset = 8, imm19 = 8/4 = 2. Cond NE = 1.
        // Encoding: 0_1010100_imm19_0_cond = 0x54000040 | (imm19 << 5) | cond
        // imm19 = 2 → bits 5..23 = 0b0_0000_0000_0000_0000_010 = 0x40
        // cond = 1 → bits 0..3 = 0b0001
        // So: 0x54000040 | 0x40 | 1 = 0x54000041 (matches above)
        let code: &[u32] = &[
            0xEB02003F, // cmp x1, x2  (Rm=2 — uses BOTH x1 and x2)
            0x54000041, // b.ne pc+8  (should NOT branch when Z=1)
            0xD4000001, // svc #0   (success — b.ne fell through)
            0xD4000021, // svc #1   (failure — b.ne taken when it shouldn't have)
        ];

        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(0x1000);
        jit.set_register(1, 0x30444F4D); // STK MOD0 magic value
        jit.set_register(2, 0x30444F4D);

        let halt = run_a64_until_svc(&mut jit);
        assert!(
            halt.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            halt
        );

        // Now run again with x1 != x2 — b.ne SHOULD take the branch.
        let mut jit2 = A64Jit::new(JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        })
        .unwrap();
        jit2.set_pc(0x1000);
        jit2.set_register(1, 0x30444F4D);
        jit2.set_register(2, 0x99999999); // different
        let halt = run_a64_until_svc(&mut jit2);
        assert!(halt.contains(HaltReason::SVC));

        // Final PC tells us which SVC we hit. If b.ne falls through (correct), we
        // executed the SVC at 0x1008 and the JIT advances PC past it.
        assert_eq!(
            jit.get_pc(),
            0x1008 + 4,
            "cmp w1,w2 with equal values should set Z=1; b.ne should NOT branch. \
             Expected SVC at 0x1008 (PC after = 0x100C); got PC=0x{:X}",
            jit.get_pc()
        );
        assert_eq!(
            jit2.get_pc(),
            0x100C + 4,
            "cmp x1,x2 with different values should set Z=0; b.ne should branch"
        );
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_ccmp_immediate_nzcv_controls_conditional_branch() {
        // Exercise both CCMP paths: compare registers when NE is true, or use
        // the immediate NZCV value when NE is false.
        //   cmp  x2, #0
        //   ccmp w3, w4, #2, ne
        //   b.cc target
        let code: &[u32] = &[
            0xF100_005F,
            0x7A44_1062,
            0x5400_0063,
            0x5280_0000,
            0xD400_0001,
            0x5280_0020,
            0xD400_0001,
        ];

        let run = |optimizations: OptimizationFlag,
                   next: u64,
                   processed: u64,
                   expected_count: u64,
                   expected_result: u64| {
            let config = JitConfig {
                coprocessors: JitConfig::default_coprocessors(),
                callbacks: Box::new(MockCallbacks::new(0x1000, code)),
                enable_cycle_counting: false,
                code_cache_size: 4 * 1024 * 1024,
                optimizations,
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
                memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
            };
            let mut jit = A64Jit::new(config).unwrap();
            jit.set_pc(0x1000);
            {
                jit.set_register(2, next);
                jit.set_register(3, processed);
                jit.set_register(4, expected_count);
            }
            let halt = run_a64_until_svc(&mut jit);
            assert!(halt.contains(HaltReason::SVC));
            assert_eq!(jit.get_register(0), expected_result);
        };

        for optimizations in [
            OptimizationFlag::NO_OPTIMIZATIONS,
            OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
        ] {
            // Null next pointer makes CCMP use immediate NZCV=2 (C=1), so CC
            // is false and execution reaches the first SVC.
            run(optimizations, 0, 1, 2, 0);
            // A next pointer and 1 < 2 produce C=0, so CC is true and execution
            // branches to the second SVC.
            run(optimizations, 1, 1, 2, 1);
        }
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_sub_w_then_tst_x_bne_observes_zero_extended_result() {
        let code: &[u32] = &[
            0x5100_0400, // sub w0, w0, #1
            0xF240_041F, // tst x0, #3
            0x5400_0041, // b.ne bad
            0xD400_0001, // svc #0
            0xD400_0021, // bad: svc #1
        ];
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(0x1000);
        jit.set_register(0, 1);

        let halt = run_a64_until_svc(&mut jit);

        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(jit.get_register(0), 0);
        assert_eq!(jit.get_pc(), 0x1010, "b.ne must not branch when x0 is zero");
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn test_a64_return_stack_buffer_wraps_across_repeated_calls() {
        let code: &[u32] = &[
            0xD280_0000, // mov x0, #0
            0xD280_0201, // mov x1, #16
            0x9400_0004, // loop: bl func
            0xF100_0421, // subs x1, x1, #1
            0x54FF_FFC1, // b.ne loop
            0xD400_0001, // svc #0
            0x9100_0400, // func: add x0, x0, #1
            0xD65F_03C0, // ret
        ];
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(0x1000);

        let halt = run_a64_until_svc(&mut jit);

        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(jit.get_register(0), 16);
        assert_eq!(jit.get_register(1), 0);
        assert_eq!(jit.get_pc(), 0x1018);
    }

    #[test]
    fn test_a32_bfc_preserves_low_bits() {
        let decoded = crate::frontend::a32::decoder::decode_arm(0xE7DF_0E1F);
        assert_eq!(decoded.id, crate::frontend::a32::decoder::ArmInstId::BFC);

        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, &[0xE7DF_0E1F])),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();

        jit.set_pc(0x1000);
        jit.set_register(0, 0x69);

        let halt = jit.step();

        assert!(halt.contains(HaltReason::STEP));
        assert_eq!(jit.get_pc(), 0x1004);
        assert_eq!(jit.get_register(0), 0x69);
    }

    #[test]
    fn test_a32_clrex_advances_pc_and_clears_exclusive_state() {
        let decoded = crate::frontend::a32::decoder::decode_arm(0xF57F_F01F);
        assert_eq!(decoded.id, crate::frontend::a32::decoder::ArmInstId::CLREX);

        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, &[0xF57F_F01F])),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();

        jit.set_pc(0x1000);
        #[cfg(target_arch = "aarch64")]
        unsafe {
            let state = jit
                .jit_state_ptr()
                .cast_mut()
                .cast::<crate::backend::arm64::jit_state::A32JitState>();
            (*state).exclusive_state = 1;
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            jit.inner.jit_state.exclusive_state = 1;
        }

        let halt = jit.step();

        assert!(halt.contains(HaltReason::STEP));
        assert_eq!(jit.get_pc(), 0x1004);
        #[cfg(target_arch = "aarch64")]
        unsafe {
            let state = jit
                .jit_state_ptr()
                .cast::<crate::backend::arm64::jit_state::A32JitState>();
            assert_eq!((*state).exclusive_state, 0);
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            assert_eq!(jit.inner.jit_state.exclusive_state, 0);
        }
    }

    #[test]
    fn test_a32_ubfx_uses_low_nibble_source_register() {
        let decoded = crate::frontend::a32::decoder::decode_arm(0xE7E3_52D1);
        assert_eq!(decoded.id, crate::frontend::a32::decoder::ArmInstId::UBFX);

        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, &[0xE7E3_52D1])),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();

        jit.set_pc(0x1000);
        jit.set_register(1, 0x20);
        jit.set_register(3, 0);

        let halt = jit.step();

        assert!(halt.contains(HaltReason::STEP));
        assert_eq!(jit.get_pc(), 0x1004);
        assert_eq!(jit.get_register(5), 1);
    }

    #[test]
    fn test_a32_cmp_bne_loop_exits_when_equal() {
        // ADD r7, r7, #1  = E2877001
        // CMP r4, r7      = E1540007
        // BNE -12 (to ADD) = 1AFFFFFC
        // MOV r0, #42     = E3A0002A
        // SVC #0          = EF000000
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(
                0x1000,
                &[
                    0xE2877001, // ADD r7, r7, #1
                    0xE1540007, // CMP r4, r7
                    0x1AFFFFFC, // BNE back to ADD
                    0xE3A0002A, // MOV r0, #42
                    0xEF000000, // SVC #0
                ],
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        jit.set_register(4, 5); // r4 = limit
        jit.set_register(7, 3); // r7 = start counter
        jit.set_register(15, 0x1000);
        jit.set_cpsr(0x10); // USR mode

        let hr = jit.run();

        assert!(
            hr.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            hr
        );
        assert_eq!(jit.get_register(7), 5, "r7 should be 5 (loop limit)");
        assert_eq!(jit.get_register(0), 42, "r0 should be 42 (post-loop MOV)");
    }

    #[test]
    fn test_a32_scalar_saturation_results_and_q_flag() {
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(
                0x1000,
                &[
                    0xE6A7_2011, // SSAT r2, #8, r1
                    0xE6E8_3010, // USAT r3, #8, r0
                    0xE102_4051, // QADD r4, r1, r2
                    0xE6A7_5F36, // SSAT16 r5, #8, r6
                    0xE6E8_7F38, // USAT16 r7, #8, r8
                    0xEF00_0000, // SVC #0
                ],
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        jit.set_register(0, u32::MAX);
        jit.set_register(1, i32::MAX as u32);
        jit.set_register(6, 0x0100_ff00);
        jit.set_register(8, 0x0100_ffff);
        jit.set_register(15, 0x1000);
        jit.set_cpsr(0x10);

        let halt = jit.run();

        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(jit.get_register(2), 127);
        assert_eq!(jit.get_register(3), 0);
        assert_eq!(jit.get_register(4), i32::MAX as u32);
        assert_eq!(jit.get_register(5), 0x007f_ff80);
        assert_eq!(jit.get_register(7), 0x00ff_0000);
        assert_ne!(jit.get_cpsr() & (1 << 27), 0, "CPSR.Q must be sticky");
    }

    #[test]
    fn test_a32_cmp_bne_loop_with_all_optimizations() {
        // Same loop but with all optimizations + block linking enabled
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(
                0x1000,
                &[
                    0xE2877001, // ADD r7, r7, #1
                    0xE1540007, // CMP r4, r7
                    0x1AFFFFFC, // BNE back to ADD
                    0xE3A0002A, // MOV r0, #42
                    0xEF000000, // SVC #0
                ],
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        jit.set_register(4, 5);
        jit.set_register(7, 3);
        jit.set_register(15, 0x1000);
        jit.set_cpsr(0x10);

        let hr = jit.run();

        assert!(
            hr.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            hr
        );
        assert_eq!(jit.get_register(7), 5, "r7 should be 5 (loop limit)");
        assert_eq!(jit.get_register(0), 42, "r0 should be 42 (post-loop MOV)");
    }

    #[test]
    fn test_a32_cmp_bne_with_vfp_no_getset_elim() {
        // Same VFP+CMP loop but WITHOUT GetSetElimination
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(
                0x1000,
                &[
                    0xE2877001, // ADD r7, r7, #1
                    0xEEB48AC0, // VCMPE.F32 s16, s0
                    0xEEF1FA10, // VMRS APSR_nzcv, FPSCR
                    0xE1540007, // CMP r4, r7
                    0x1AFFFFFA, // BNE back
                    0xE3A0002A, // MOV r0, #42
                    0xEF000000, // SVC #0
                ],
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS
                & !OptimizationFlag::GET_SET_ELIMINATION,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        jit.set_register(4, 5);
        jit.set_register(7, 3);
        jit.set_register(15, 0x1000);
        jit.set_cpsr(0x10);

        let hr = jit.run();

        assert!(
            hr.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            hr
        );
        assert_eq!(jit.get_register(7), 5, "r7 should be 5");
        assert_eq!(jit.get_register(0), 42, "r0 should be 42");
    }

    #[test]
    fn test_a32_cmp_bne_with_vfp_all_opts() {
        // Same loop WITH all optimizations including GetSetElimination
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(
                0x1000,
                &[
                    0xE2877001, // ADD r7, r7, #1
                    0xEEB48AC0, // VCMPE.F32 s16, s0
                    0xEEF1FA10, // VMRS APSR_nzcv, FPSCR
                    0xE1540007, // CMP r4, r7
                    0x1AFFFFFA, // BNE back
                    0xE3A0002A, // MOV r0, #42
                    0xEF000000, // SVC #0
                ],
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        jit.set_register(4, 5);
        jit.set_register(7, 3);
        jit.set_register(15, 0x1000);
        jit.set_cpsr(0x10);

        let hr = jit.run();

        assert!(
            hr.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            hr
        );
        assert_eq!(jit.get_register(7), 5, "r7 should be 5");
        assert_eq!(jit.get_register(0), 42, "r0 should be 42");
    }

    #[test]
    fn test_vfp_cmp_bne_gse_only() {
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(
                0x1000,
                &[
                    0xE2877001, 0xEEB48AC0, 0xEEF1FA10, 0xE1540007, 0x1AFFFFFA, 0xE3A0002A,
                    0xEF000000,
                ],
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::GET_SET_ELIMINATION,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        jit.set_register(4, 5);
        jit.set_register(7, 3);
        jit.set_register(15, 0x1000);
        jit.set_cpsr(0x10);
        let hr = jit.run();
        assert!(hr.contains(HaltReason::SVC));
        assert_eq!(jit.get_register(7), 5);
    }

    #[test]
    fn test_vfp_cmp_bne_gse_only_single_step_progress() {
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(
                0x1000,
                &[
                    0xE2877001, 0xEEB48AC0, 0xEEF1FA10, 0xE1540007, 0x1AFFFFFA, 0xE3A0002A,
                    0xEF000000,
                ],
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::GET_SET_ELIMINATION,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        jit.set_register(4, 5);
        jit.set_register(7, 3);
        jit.set_register(15, 0x1000);
        jit.set_cpsr(0x10);

        let mut pcs = Vec::new();
        let mut r7s = Vec::new();
        let mut halts = Vec::new();
        for _ in 0..12 {
            pcs.push(jit.get_register(15));
            r7s.push(jit.get_register(7));
            let hr = jit.step();
            halts.push(hr);
            if hr.contains(HaltReason::SVC) {
                break;
            }
            assert!(
                hr.contains(HaltReason::STEP),
                "expected step halt, got {:?}",
                hr
            );
        }

        eprintln!("pcs={pcs:08X?} r7s={r7s:?} halts={halts:?}");

        assert_eq!(pcs[0], 0x1000);
        assert_eq!(r7s[0], 3);
        assert_eq!(pcs[1], 0x1004);
        assert_eq!(r7s[1], 4);
        assert_eq!(pcs[4], 0x1010);
        assert_eq!(pcs[5], 0x1000, "first BNE should branch back");
        assert_eq!(r7s[5], 4, "r7 must survive across the loop backedge");
        assert_eq!(
            pcs[10], 0x1014,
            "second BNE should fall through after CMP sets Z"
        );
    }

    #[test]
    fn test_vfp_cmp_bne_gse_only_with_cycle_counting() {
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(
                0x1000,
                &[
                    0xE2877001, 0xEEB48AC0, 0xEEF1FA10, 0xE1540007, 0x1AFFFFFA, 0xE3A0002A,
                    0xEF000000,
                ],
            )),
            enable_cycle_counting: true,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::GET_SET_ELIMINATION,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        jit.set_register(4, 5);
        jit.set_register(7, 3);
        jit.set_register(15, 0x1000);
        jit.set_cpsr(0x10);

        let hr = jit.run();
        assert!(
            hr.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            hr
        );
        assert_eq!(jit.get_register(7), 5);
        assert_eq!(jit.get_register(0), 42);
    }

    #[test]
    fn test_vfp_vmrs_apsr_then_bne_uses_equal_flags() {
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(
                0x1000,
                &[
                    0xEEB48AC0, // VCMPE.F32 s16, s0  (0.0 == 0.0 => Z=1, C=1)
                    0xEEF1FA10, // VMRS APSR_nzcv, FPSCR
                    0x1A000000, // BNE +0  (skip next MOV only if Z == 0)
                    0xE3A0002A, // MOV r0, #42
                    0xEF000000, // SVC #0
                ],
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        jit.set_register(15, 0x1000);
        jit.set_cpsr(0x10);

        let hr = jit.run();

        assert!(
            hr.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            hr
        );
        assert_eq!(
            jit.get_register(0),
            42,
            "BNE must not branch when VMRS copied Z=1 from FPSCR"
        );
    }

    /// Directly test the helper used by emit_fp_single_to_fixed_u32 to see
    /// if the bug is in the helper itself or in arg passing from emitted code.
    #[test]
    fn test_fp_single_to_fixed_u32_helper_direct() {
        let bits = 1920.0f32.to_bits() as u64;
        let mut fpsr = 0;
        let got = crate::backend::x64::fp_helpers::fp_single_to_fixed_u32(bits, 0, 0, &mut fpsr);
        assert_eq!(
            got, 0x780,
            "helper for 1920.0f32 should return 0x780, got 0x{:X}",
            got
        );
    }

    /// showed rdynarmic converting 1920.0f32 to u32 as 0xFFFFFFFF instead of
    /// float should truncate cleanly; saturation must only kick in for inputs
    /// >= 2^32 or for NaN/negative.
    #[test]
    fn test_vcvt_u32_f32_small_positive_float() {
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(
                0x1000,
                &[
                    0xEEBC0AC9, // VCVT.U32.F32 s0, s18  (round-toward-zero form)
                    0xEE100A10, // VMOV r0, s0
                    0xEF000000, // SVC #0
                ],
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        // s18 = 1920.0f32; bit pattern 0x44F00000.
        jit.set_ext_reg(18, 1920.0f32.to_bits());
        jit.set_register(15, 0x1000);
        jit.set_cpsr(0x10);

        let hr = jit.run();

        assert!(
            hr.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            hr
        );
        assert_eq!(
            jit.get_register(0),
            0x780,
            "VCVT.U32.F32 of 1920.0f32 should be 0x780, got 0x{:X}",
            jit.get_register(0)
        );
    }

    #[test]
    fn test_vcvt_u32_f64_does_not_truncate_to_u16() {
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(
                0x1000,
                &[
                    0xEEBC0BC8, // VCVT.U32.F64 s0, d8
                    0xEE100A10, // VMOV r0, s0
                    0xEF000000, // SVC #0
                ],
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        let value = 1_000_000.0f64.to_bits();
        jit.set_ext_reg(16, value as u32);
        jit.set_ext_reg(17, (value >> 32) as u32);
        jit.set_register(15, 0x1000);
        jit.set_cpsr(0x10);

        let hr = jit.run();

        assert!(
            hr.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            hr
        );
        assert_eq!(jit.get_register(0), 1_000_000);
    }

    #[test]
    fn test_vldr_f64_literal_loads_expected_double() {
        let base = 0x1000u64;
        let mut memory = vec![0u8; 0x1400];
        let code = [
            0xEDDF0BE8u32, // VLDR d16, [pc, #928]
            0xEF000000u32, // SVC #0
        ];
        for (i, &word) in code.iter().enumerate() {
            let offset = i * 4;
            memory[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        }

        let literal_addr = (0x13A8u64 - base) as usize;
        let literal = 268_435_456.0f64.to_bits();
        memory[literal_addr..literal_addr + 8].copy_from_slice(&literal.to_le_bytes());

        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::from_memory(base, memory)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        jit.set_register(15, base as u32);
        jit.set_cpsr(0x10);

        let hr = jit.run();

        assert!(
            hr.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            hr
        );
        let got = (jit.get_ext_reg(33) as u64) << 32 | jit.get_ext_reg(32) as u64;
        assert_eq!(got, literal);
    }

    #[test]
    fn test_vmul_f64_with_literal_operand_stays_finite() {
        let base = 0x1000u64;
        let mut memory = vec![0u8; 0x1400];
        let code = [
            0xEDDF0BE8u32, // VLDR d16, [pc, #928]
            0xEE288B20u32, // VMUL.F64 d8, d8, d16
            0xEF000000u32, // SVC #0
        ];
        for (i, &word) in code.iter().enumerate() {
            let offset = i * 4;
            memory[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        }

        let literal_addr = (0x13A8u64 - base) as usize;
        let literal = 268_435_456.0f64.to_bits();
        memory[literal_addr..literal_addr + 8].copy_from_slice(&literal.to_le_bytes());

        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::from_memory(base, memory)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        let d8 = 1.56328125f64.to_bits();
        jit.set_ext_reg(16, d8 as u32);
        jit.set_ext_reg(17, (d8 >> 32) as u32);
        jit.set_register(15, base as u32);
        jit.set_cpsr(0x10);

        let hr = jit.run();

        assert!(
            hr.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            hr
        );
        let got_bits = (jit.get_ext_reg(17) as u64) << 32 | jit.get_ext_reg(16) as u64;
        let got = f64::from_bits(got_bits);
        let expected = 1.56328125f64 * 268_435_456.0f64;
        assert!(
            got.is_finite(),
            "vmul produced non-finite result: {got_bits:#018X}"
        );
        assert_eq!(got, expected);
    }

    #[test]
    fn test_vcvt_vsub_vmul_tail_stays_finite() {
        let base = 0x1000u64;
        let mut memory = vec![0u8; 0x1400];
        let code = [
            0xEDDF0B9Au32, // VLDR d16, [pc, #616]
            0xEEBC0BC8u32, // VCVT.U32.F64 s0, d8
            0xEEF81B40u32, // VCVT.F64.U32 d17, s0
            0xEE781B61u32, // VSUB.F64 d17, d8, d17
            0xEE218BA0u32, // VMUL.F64 d8, d17, d16
            0xEF000000u32, // SVC #0
        ];
        for (i, &word) in code.iter().enumerate() {
            let offset = i * 4;
            memory[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        }

        let literal_addr = (0x1270u64 - base) as usize;
        let literal = 1_000_000_000.0f64.to_bits();
        memory[literal_addr..literal_addr + 8].copy_from_slice(&literal.to_le_bytes());

        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::from_memory(base, memory)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        let d8 = (1.56328125f64 * 268_435_456.0f64).to_bits();
        jit.set_ext_reg(16, d8 as u32);
        jit.set_ext_reg(17, (d8 >> 32) as u32);
        jit.set_register(15, base as u32);
        jit.set_cpsr(0x10);

        let hr = jit.run();

        assert!(
            hr.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            hr
        );
        let got_bits = (jit.get_ext_reg(17) as u64) << 32 | jit.get_ext_reg(16) as u64;
        let got = f64::from_bits(got_bits);
        let expected = 199_999_988.07907104f64;
        assert!(
            got.is_finite(),
            "tail produced non-finite result: {got_bits:#018X}"
        );
        assert!(
            (got - expected).abs() < 1e-6,
            "got {got}, expected {expected}"
        );
    }

    #[test]
    fn test_vcvt_vsub_vmul_loop_reaches_zero_with_backedge() {
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(
                0x1000,
                &[
                    0xEEBC0BC8, // VCVT.U32.F64 s0, d8
                    0xEEF81B40, // VCVT.F64.U32 d17, s0
                    0xEE781B61, // VSUB.F64 d17, d8, d17
                    0xEE218BA0, // VMUL.F64 d8, d17, d16
                    0xEEB58BC0, // VCMPE.F64 d8, #0.0
                    0xEEF1FA10, // VMRS APSR_nzcv, FPSCR
                    0x1AFFFFF8, // BNE back to 0x1000
                    0xEF000000, // SVC #0
                ],
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        let d8 = (1.56328125f64 * 268_435_456.0f64).to_bits();
        let d16 = 1_000_000_000.0f64.to_bits();
        jit.set_ext_reg(16, d8 as u32);
        jit.set_ext_reg(17, (d8 >> 32) as u32);
        jit.set_ext_reg(32, d16 as u32);
        jit.set_ext_reg(33, (d16 >> 32) as u32);
        jit.set_register(15, 0x1000);
        jit.set_cpsr(0x10);

        let hr = jit.run();

        assert!(
            hr.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            hr
        );
        let got_bits = (jit.get_ext_reg(17) as u64) << 32 | jit.get_ext_reg(16) as u64;
        let got = f64::from_bits(got_bits);
        assert_eq!(got, 0.0, "loop should converge exactly to zero");
    }

    #[test]
    fn test_vcvt_vsub_vmul_loop_single_step_converges_to_zero() {
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(
                0x1000,
                &[
                    0xEEBC0BC8, // VCVT.U32.F64 s0, d8
                    0xEEF81B40, // VCVT.F64.U32 d17, s0
                    0xEE781B61, // VSUB.F64 d17, d8, d17
                    0xEE218BA0, // VMUL.F64 d8, d17, d16
                    0xEEB58BC0, // VCMPE.F64 d8, #0.0
                    0xEEF1FA10, // VMRS APSR_nzcv, FPSCR
                    0x1AFFFFF8, // BNE back to 0x1000
                    0xEF000000, // SVC #0
                ],
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        let d8 = (1.56328125f64 * 268_435_456.0f64).to_bits();
        let d16 = 1_000_000_000.0f64.to_bits();
        jit.set_ext_reg(16, d8 as u32);
        jit.set_ext_reg(17, (d8 >> 32) as u32);
        jit.set_ext_reg(32, d16 as u32);
        jit.set_ext_reg(33, (d16 >> 32) as u32);
        jit.set_register(15, 0x1000);
        jit.set_cpsr(0x10);

        let mut saw_svc = false;
        for _ in 0..64 {
            let hr = jit.step();
            if hr.contains(HaltReason::SVC) {
                saw_svc = true;
                break;
            }
        }

        assert!(saw_svc, "single-step loop did not reach SVC");
        let got_bits = (jit.get_ext_reg(17) as u64) << 32 | jit.get_ext_reg(16) as u64;
        let got = f64::from_bits(got_bits);
        assert_eq!(got, 0.0, "single-step loop should converge exactly to zero");
    }

    fn run_vcvt_vsub_vmul_loop_with_opts(optimizations: OptimizationFlag) -> (HaltReason, f64) {
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(
                0x1000,
                &[
                    0xEEBC0BC8, // VCVT.U32.F64 s0, d8
                    0xEEF81B40, // VCVT.F64.U32 d17, s0
                    0xEE781B61, // VSUB.F64 d17, d8, d17
                    0xEE218BA0, // VMUL.F64 d8, d17, d16
                    0xEEB58BC0, // VCMPE.F64 d8, #0.0
                    0xEEF1FA10, // VMRS APSR_nzcv, FPSCR
                    0x1AFFFFF8, // BNE back to 0x1000
                    0xEF000000, // SVC #0
                ],
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        let d8 = (1.56328125f64 * 268_435_456.0f64).to_bits();
        let d16 = 1_000_000_000.0f64.to_bits();
        jit.set_ext_reg(16, d8 as u32);
        jit.set_ext_reg(17, (d8 >> 32) as u32);
        jit.set_ext_reg(32, d16 as u32);
        jit.set_ext_reg(33, (d16 >> 32) as u32);
        jit.set_register(15, 0x1000);
        jit.set_cpsr(0x10);

        let hr = jit.run();
        let got_bits = (jit.get_ext_reg(17) as u64) << 32 | jit.get_ext_reg(16) as u64;
        (hr, f64::from_bits(got_bits))
    }

    #[test]
    fn test_vcvt_vsub_vmul_loop_no_optimizations() {
        let (hr, got) = run_vcvt_vsub_vmul_loop_with_opts(OptimizationFlag::NO_OPTIMIZATIONS);
        assert!(
            hr.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            hr
        );
        assert_eq!(got, 0.0, "no-opt loop should converge exactly to zero");
    }

    #[test]
    fn test_vcvt_vsub_vmul_loop_block_linking_only() {
        let (hr, got) = run_vcvt_vsub_vmul_loop_with_opts(OptimizationFlag::BLOCK_LINKING);
        assert!(
            hr.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            hr
        );
        assert_eq!(
            got, 0.0,
            "block-linking-only loop should converge exactly to zero"
        );
    }

    #[test]
    fn test_vfp_f64_vmrs_apsr_then_bne_uses_equal_flags() {
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(
                0x1000,
                &[
                    0xEEB58BC0, // VCMPE.F64 d8, #0.0
                    0xEEF1FA10, // VMRS APSR_nzcv, FPSCR
                    0x1A000000, // BNE +0
                    0xE3A0002A, // MOV r0, #42
                    0xEF000000, // SVC #0
                ],
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        jit.set_ext_reg(16, 0);
        jit.set_ext_reg(17, 0);
        jit.set_register(15, 0x1000);
        jit.set_cpsr(0x10);

        let hr = jit.run();

        assert!(
            hr.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            hr
        );
        assert_eq!(
            jit.get_register(0),
            42,
            "BNE must not branch when F64 compare sets Z=1"
        );
    }

    #[test]
    fn test_vfp_f64_vmrs_apsr_then_bne_branches_on_nonzero() {
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(
                0x1000,
                &[
                    0xEEB58BC0, // VCMPE.F64 d8, #0.0
                    0xEEF1FA10, // VMRS APSR_nzcv, FPSCR
                    0x1A000001, // BNE +1
                    0xE3A0002A, // MOV r0, #42
                    0xEF000000, // SVC #0
                    0xE3A00063, // MOV r0, #99
                    0xEF000000, // SVC #0
                ],
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        let d8 = 1.0f64.to_bits();
        jit.set_ext_reg(16, d8 as u32);
        jit.set_ext_reg(17, (d8 >> 32) as u32);
        jit.set_register(15, 0x1000);
        jit.set_cpsr(0x10);

        let hr = jit.run();

        assert!(
            hr.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            hr
        );
        assert_eq!(
            jit.get_register(0),
            99,
            "BNE must branch when F64 compare sets Z=0"
        );
    }

    #[test]
    fn test_vcvt_vsub_vmul_five_unrolled_iterations_reach_zero() {
        let mut code = Vec::new();
        for _ in 0..5 {
            code.extend_from_slice(&[
                0xEEBC0BC8, // VCVT.U32.F64 s0, d8
                0xEEF81B40, // VCVT.F64.U32 d17, s0
                0xEE781B61, // VSUB.F64 d17, d8, d17
                0xEE218BA0, // VMUL.F64 d8, d17, d16
            ]);
        }
        code.extend_from_slice(&[
            0xEEB58BC0, // VCMPE.F64 d8, #0.0
            0xEEF1FA10, // VMRS APSR_nzcv, FPSCR
            0x1A000001, // BNE +1
            0xE3A0002A, // MOV r0, #42
            0xEF000000, // SVC #0
            0xE3A00063, // MOV r0, #99
            0xEF000000, // SVC #0
        ]);

        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, &code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        let d8 = (1.56328125f64 * 268_435_456.0f64).to_bits();
        let d16 = 1_000_000_000.0f64.to_bits();
        jit.set_ext_reg(16, d8 as u32);
        jit.set_ext_reg(17, (d8 >> 32) as u32);
        jit.set_ext_reg(32, d16 as u32);
        jit.set_ext_reg(33, (d16 >> 32) as u32);
        jit.set_register(15, 0x1000);
        jit.set_cpsr(0x10);

        let hr = jit.run();

        assert!(
            hr.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            hr
        );
        let got_bits = (jit.get_ext_reg(17) as u64) << 32 | jit.get_ext_reg(16) as u64;
        let got = f64::from_bits(got_bits);
        assert_eq!(
            jit.get_register(0),
            42,
            "unrolled F64 tail should reach zero and not branch; d8={got} ({got_bits:#018X})"
        );
        assert_eq!(
            got, 0.0,
            "unrolled F64 tail should end at 0.0, got {got} ({got_bits:#018X})"
        );
    }

    fn run_unrolled_vcvt_vsub_vmul_iterations(iterations: usize) -> f64 {
        let mut code = Vec::new();
        for _ in 0..iterations {
            code.extend_from_slice(&[
                0xEEBC0BC8, // VCVT.U32.F64 s0, d8
                0xEEF81B40, // VCVT.F64.U32 d17, s0
                0xEE781B61, // VSUB.F64 d17, d8, d17
                0xEE218BA0, // VMUL.F64 d8, d17, d16
            ]);
        }
        code.push(0xEF000000); // SVC #0

        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, &code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        let d8 = (1.56328125f64 * 268_435_456.0f64).to_bits();
        let d16 = 1_000_000_000.0f64.to_bits();
        jit.set_ext_reg(16, d8 as u32);
        jit.set_ext_reg(17, (d8 >> 32) as u32);
        jit.set_ext_reg(32, d16 as u32);
        jit.set_ext_reg(33, (d16 >> 32) as u32);
        jit.set_register(15, 0x1000);
        jit.set_cpsr(0x10);

        let hr = jit.run();
        assert!(
            hr.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            hr
        );
        let got_bits = (jit.get_ext_reg(17) as u64) << 32 | jit.get_ext_reg(16) as u64;
        f64::from_bits(got_bits)
    }

    #[test]
    fn test_vcvt_vsub_vmul_unrolled_progression_matches_expected_values() {
        let expected = [
            199_999_988.07907104f64,
            79_071_044.921875f64,
            921_875_000.0f64,
            0.0f64,
            0.0f64,
        ];
        for (iterations, expected_value) in (1usize..=5).zip(expected) {
            let got = run_unrolled_vcvt_vsub_vmul_iterations(iterations);
            assert!(
                (got - expected_value).abs() < 1e-6,
                "iteration {iterations}: got {got}, expected {expected_value}"
            );
        }
    }

    #[test]
    fn test_a32_set_cpsr_nzc_empty_marker_backend_path() {
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(
                0x1000,
                &[
                    0xE3A01001, // MOV  r1, #1
                    0xE1B000A1, // MOVS r0, r1, LSR #1  => result=0, carry=1
                    0xE2A22000, // ADC  r2, r2, #0      => consumes carry only
                    0xEF000000, // SVC #0
                ],
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::GET_SET_ELIMINATION,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        jit.set_register(2, 41);
        jit.set_register(15, 0x1000);
        jit.set_cpsr(0x10);

        let hr = jit.run();

        assert!(
            hr.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            hr
        );
        assert_eq!(
            jit.get_register(2),
            42,
            "ADC must consume carry after GSE rewrites dead NZ"
        );
    }

    #[test]
    fn test_a32_cmp_addhs_subhs_division_tail_with_all_optimizations() {
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(
                0x1000,
                &[
                    0xE1500101, // CMP   r0, r1, LSL #2
                    0x22833004, // ADDHS r3, r3, #4
                    0x20400101, // SUBHS r0, r0, r1, LSL #2
                    0xE1500081, // CMP   r0, r1, LSL #1
                    0x22833002, // ADDHS r3, r3, #2
                    0x20400081, // SUBHS r0, r0, r1, LSL #1
                    0xE1500001, // CMP   r0, r1
                    0x22833001, // ADDHS r3, r3, #1
                    0x20400001, // SUBHS r0, r0, r1
                    0xE1A04000, // MOV   r4, r0   ; remainder
                    0xE1A00003, // MOV   r0, r3   ; quotient
                    0xEF000000, // SVC   #0
                ],
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        jit.set_register(0, 13); // dividend
        jit.set_register(1, 3); // divisor
        jit.set_register(3, 0); // quotient accumulator
        jit.set_register(15, 0x1000);
        jit.set_cpsr(0x10);

        let hr = jit.run();

        assert!(
            hr.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            hr
        );
        assert_eq!(jit.get_register(0), 4, "quotient must be 4");
        assert_eq!(jit.get_register(4), 1, "remainder must be 1");
    }

    fn run_a32_rtld_udivmod(dividend: u32, divisor: u32) -> (u32, u32) {
        // This includes the CLZ-based computed `bx ip` dispatch, which is the
        // part most likely to differ from the simpler hand-written tail test.
        let mut code = vec![
            0xE3510001, 0x3A000073, 0x0A00006F, 0xE1500001, 0x3A00006A, 0xE16FCF10, 0xE16F3F11,
            0xE043300C, 0xE28FCD06, 0xE04CC103, 0xE04CC183, 0xE3A03000, 0xE12FFF1C, 0xE1500F81,
            0x22833102, 0x20400F81, 0xE1500F01, 0x22833101, 0x20400F01, 0xE1500E81, 0x22833202,
            0x20400E81, 0xE1500E01, 0x22833201, 0x20400E01, 0xE1500D81, 0x22833302, 0x20400D81,
            0xE1500D01, 0x22833301, 0x20400D01, 0xE1500C81, 0x22833402, 0x20400C81, 0xE1500C01,
            0x22833401, 0x20400C01, 0xE1500B81, 0x22833502, 0x20400B81, 0xE1500B01, 0x22833501,
            0x20400B01, 0xE1500A81, 0x22833602, 0x20400A81, 0xE1500A01, 0x22833601, 0x20400A01,
            0xE1500981, 0x22833702, 0x20400981, 0xE1500901, 0x22833701, 0x20400901, 0xE1500881,
            0x22833802, 0x20400881, 0xE1500801, 0x22833801, 0x20400801, 0xE1500781, 0x22833902,
            0x20400781, 0xE1500701, 0x22833901, 0x20400701, 0xE1500681, 0x22833A02, 0x20400681,
            0xE1500601, 0x22833A01, 0x20400601, 0xE1500581, 0x22833B02, 0x20400581, 0xE1500501,
            0x22833B01, 0x20400501, 0xE1500481, 0x22833C02, 0x20400481, 0xE1500401, 0x22833C01,
            0x20400401, 0xE1500381, 0x22833080, 0x20400381, 0xE1500301, 0x22833040, 0x20400301,
            0xE1500281, 0x22833020, 0x20400281, 0xE1500201, 0x22833010, 0x20400201, 0xE1500181,
            0x22833008, 0x20400181, 0xE1500101, 0x22833004, 0x20400101, 0xE1500081, 0x22833002,
            0x20400081, 0xE1500001, 0x22833001, 0x20400001, 0xE5820000, 0xE1A00003, 0xE12FFF1E,
            0xE5820000, 0xE3A00000, 0xE12FFF1E, 0xE3A03000, 0xE5823000, 0xE12FFF1E, 0xE3A00000,
            0xEAFFFFFF, 0xE12FFF1E,
        ];
        let base = 0x1000u64;
        let svc_addr = base + (code.len() as u64) * 4;
        code.push(0xEF00_0000); // SVC #0

        let callbacks = MockCallbacks::new(base, &code);
        let memory = callbacks.memory.clone();
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(callbacks),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        let remainder_addr = base + 0x800;
        jit.set_register(0, dividend);
        jit.set_register(1, divisor);
        jit.set_register(2, remainder_addr as u32);
        jit.set_register(15, base as u32);
        jit.set_register(14, svc_addr as u32);
        jit.set_cpsr(0x10);

        let hr = jit.run();

        assert!(
            hr.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            hr
        );
        let quotient = jit.get_register(0);
        let remainder_off = (remainder_addr - base) as usize;
        let memory = memory.lock().expect("mock memory poisoned");
        let remainder =
            u32::from_le_bytes(memory[remainder_off..remainder_off + 4].try_into().unwrap());
        (quotient, remainder)
    }

    fn run_a32_rtld_lookup_in_main(symbol_addr: u32) -> u32 {
        const RTLD_BASE: u32 = 0x0020_0000;
        const MAIN_BASE: u32 = 0x0020_6000;
        const RTLD_MODULE_OBJ: u32 = 0x0020_51F8;
        const MAIN_MODULE_OBJ: u32 = 0x00ED_F000;
        const RTLD_BUCKETS: u32 = 0x0020_4198;
        const RTLD_CHAINS: u32 = 0x0020_41DC;
        const RTLD_STRTAB: u32 = 0x0020_44D8;
        const RTLD_SYMTAB: u32 = 0x0020_4288;
        const RTLD_NBUCKET: u32 = 0x0000_0011;
        const MAIN_BUCKETS: u32 = 0x00D4_3EF8;
        const MAIN_CHAINS: u32 = 0x00D4_471C;
        const MAIN_STRTAB: u32 = 0x00D4_8C38;
        const MAIN_SYMTAB: u32 = 0x00D4_55A8;
        const MAIN_NBUCKET: u32 = 0x0000_0209;
        const LOOKUP_IN_MODULE: u32 = 0x0020_0998;

        let rtld = std::fs::read("/home/vricosti/Dev/emulators/ruzu/1/0x00200000_rtld.bin")
            .expect("rtld dump must exist");
        let main = std::fs::read("/home/vricosti/Dev/emulators/ruzu/1/0x00206000_main.bin")
            .expect("main dump must exist");

        let mem_len = (MAIN_BASE - RTLD_BASE) as usize + main.len() + 0x1000;
        let mut memory = vec![0u8; mem_len];
        memory[..rtld.len()].copy_from_slice(&rtld);
        let main_off = (MAIN_BASE - RTLD_BASE) as usize;
        memory[main_off..main_off + main.len()].copy_from_slice(&main);

        let write32 = |mem: &mut [u8], addr: u32, value: u32| {
            let off = (addr - RTLD_BASE) as usize;
            mem[off..off + 4].copy_from_slice(&value.to_le_bytes());
        };

        // Seed the runtime-built rtld/main module objects with the live values captured from ruzu.
        write32(&mut memory, RTLD_MODULE_OBJ + 0x10, RTLD_BASE);
        write32(&mut memory, RTLD_MODULE_OBJ + 0x28, RTLD_BUCKETS);
        write32(&mut memory, RTLD_MODULE_OBJ + 0x2C, RTLD_CHAINS);
        write32(&mut memory, RTLD_MODULE_OBJ + 0x30, RTLD_STRTAB);
        write32(&mut memory, RTLD_MODULE_OBJ + 0x34, RTLD_SYMTAB);
        write32(&mut memory, RTLD_MODULE_OBJ + 0x54, RTLD_NBUCKET);
        write32(&mut memory, MAIN_MODULE_OBJ + 0x10, MAIN_BASE);
        write32(&mut memory, MAIN_MODULE_OBJ + 0x28, MAIN_BUCKETS);
        write32(&mut memory, MAIN_MODULE_OBJ + 0x2C, MAIN_CHAINS);
        write32(&mut memory, MAIN_MODULE_OBJ + 0x30, MAIN_STRTAB);
        write32(&mut memory, MAIN_MODULE_OBJ + 0x34, MAIN_SYMTAB);
        write32(&mut memory, MAIN_MODULE_OBJ + 0x54, MAIN_NBUCKET);

        let svc_addr = RTLD_BASE as u64 + memory.len() as u64 - 4;
        let svc_off = (svc_addr as u32 - RTLD_BASE) as usize;
        memory[svc_off..svc_off + 4].copy_from_slice(&0xEF00_0000u32.to_le_bytes());

        let memory = Arc::new(Mutex::new(memory));
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::from_shared_memory(
                RTLD_BASE as u64,
                memory.clone(),
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        jit.set_register(0, MAIN_MODULE_OBJ);
        jit.set_register(1, symbol_addr);
        jit.set_register(13, svc_addr as u32 - 0x100);
        jit.set_register(14, svc_addr as u32);
        jit.set_register(15, LOOKUP_IN_MODULE);
        jit.set_cpsr(0x10);

        let hr = jit.run();

        assert!(
            hr.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            hr
        );
        jit.get_register(0)
    }

    fn run_a32_rtld_global_lookup(symbol_addr: u32) -> u32 {
        const RTLD_BASE: u32 = 0x0020_0000;
        const LIST_HEAD: u32 = 0x0020_51E8;
        const RTLD_MODULE_OBJ: u32 = 0x0020_51F8;
        const MAIN_MODULE_OBJ: u32 = 0x00ED_F000;
        const GLOBAL_LOOKUP: u32 = 0x0020_08D0;

        let rtld = std::fs::read("/home/vricosti/Dev/emulators/ruzu/1/0x00200000_rtld.bin")
            .expect("rtld dump must exist");
        let main = std::fs::read("/home/vricosti/Dev/emulators/ruzu/1/0x00206000_main.bin")
            .expect("main dump must exist");

        let mut memory = vec![0u8; (0x0020_6000u32 - RTLD_BASE) as usize + main.len() + 0x1000];
        memory[..rtld.len()].copy_from_slice(&rtld);
        let main_off = (0x0020_6000u32 - RTLD_BASE) as usize;
        memory[main_off..main_off + main.len()].copy_from_slice(&main);

        let write32 = |mem: &mut [u8], addr: u32, value: u32| {
            let off = (addr - RTLD_BASE) as usize;
            mem[off..off + 4].copy_from_slice(&value.to_le_bytes());
        };

        // Minimal live module list: head -> rtld -> main -> head.
        write32(&mut memory, LIST_HEAD + 0x0, MAIN_MODULE_OBJ);
        write32(&mut memory, LIST_HEAD + 0x4, RTLD_MODULE_OBJ);
        write32(&mut memory, RTLD_MODULE_OBJ + 0x0, LIST_HEAD);
        write32(&mut memory, RTLD_MODULE_OBJ + 0x4, MAIN_MODULE_OBJ);
        write32(&mut memory, MAIN_MODULE_OBJ + 0x0, RTLD_MODULE_OBJ);
        write32(&mut memory, MAIN_MODULE_OBJ + 0x4, LIST_HEAD);

        // Reuse the live hash-table fields needed by 0x200998 for both modules.
        write32(&mut memory, RTLD_MODULE_OBJ + 0x10, 0x0020_0000);
        write32(&mut memory, RTLD_MODULE_OBJ + 0x28, 0x0020_4198);
        write32(&mut memory, RTLD_MODULE_OBJ + 0x2C, 0x0020_41DC);
        write32(&mut memory, RTLD_MODULE_OBJ + 0x30, 0x0020_44D8);
        write32(&mut memory, RTLD_MODULE_OBJ + 0x34, 0x0020_4288);
        write32(&mut memory, RTLD_MODULE_OBJ + 0x54, 0x0000_0011);
        write32(&mut memory, MAIN_MODULE_OBJ + 0x10, 0x0020_6000);
        write32(&mut memory, MAIN_MODULE_OBJ + 0x28, 0x00D4_3EF8);
        write32(&mut memory, MAIN_MODULE_OBJ + 0x2C, 0x00D4_471C);
        write32(&mut memory, MAIN_MODULE_OBJ + 0x30, 0x00D4_8C38);
        write32(&mut memory, MAIN_MODULE_OBJ + 0x34, 0x00D4_55A8);
        write32(&mut memory, MAIN_MODULE_OBJ + 0x54, 0x0000_0209);

        let svc_addr = RTLD_BASE as u64 + memory.len() as u64 - 4;
        let svc_off = (svc_addr as u32 - RTLD_BASE) as usize;
        memory[svc_off..svc_off + 4].copy_from_slice(&0xEF00_0000u32.to_le_bytes());

        let memory = Arc::new(Mutex::new(memory));
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::from_shared_memory(
                RTLD_BASE as u64,
                memory.clone(),
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        jit.set_register(0, symbol_addr);
        jit.set_register(13, svc_addr as u32 - 0x100);
        jit.set_register(14, svc_addr as u32);
        jit.set_register(15, GLOBAL_LOOKUP);
        jit.set_cpsr(0x10);

        let hr = jit.run();

        assert!(
            hr.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            hr
        );
        jit.get_register(0)
    }

    fn run_a32_rtld_symbol_resolve(
        symbol_offset: u32,
        flags: u32,
        optimizations: OptimizationFlag,
    ) -> (u32, u32) {
        const RTLD_BASE: u32 = 0x0020_0000;
        const LIST_HEAD: u32 = 0x0020_51E8;
        const RTLD_MODULE_OBJ: u32 = 0x0020_51F8;
        const MAIN_MODULE_OBJ: u32 = 0x00ED_F000;
        const RESOLVE_SYMBOL: u32 = 0x0020_0F18;
        const OUT_ADDR: u32 = 0x0020_5800;

        let rtld = std::fs::read("/home/vricosti/Dev/emulators/ruzu/1/0x00200000_rtld.bin")
            .expect("rtld dump must exist");
        let main = std::fs::read("/home/vricosti/Dev/emulators/ruzu/1/0x00206000_main.bin")
            .expect("main dump must exist");

        let mut memory = vec![0u8; (0x0020_6000u32 - RTLD_BASE) as usize + main.len() + 0x4000];
        memory[..rtld.len()].copy_from_slice(&rtld);
        let main_off = (0x0020_6000u32 - RTLD_BASE) as usize;
        memory[main_off..main_off + main.len()].copy_from_slice(&main);

        let write32 = |mem: &mut [u8], addr: u32, value: u32| {
            let off = (addr - RTLD_BASE) as usize;
            mem[off..off + 4].copy_from_slice(&value.to_le_bytes());
        };

        write32(&mut memory, LIST_HEAD + 0x0, 0x022C_8000);
        write32(&mut memory, LIST_HEAD + 0x4, RTLD_MODULE_OBJ);
        write32(&mut memory, RTLD_MODULE_OBJ + 0x0, LIST_HEAD);
        write32(&mut memory, RTLD_MODULE_OBJ + 0x4, MAIN_MODULE_OBJ);
        write32(&mut memory, MAIN_MODULE_OBJ + 0x0, RTLD_MODULE_OBJ);
        write32(&mut memory, MAIN_MODULE_OBJ + 0x4, 0x0168_47D0);

        write32(&mut memory, RTLD_MODULE_OBJ + 0x10, 0x0020_0000);
        write32(&mut memory, RTLD_MODULE_OBJ + 0x28, 0x0020_4198);
        write32(&mut memory, RTLD_MODULE_OBJ + 0x2C, 0x0020_41DC);
        write32(&mut memory, RTLD_MODULE_OBJ + 0x30, 0x0020_44D8);
        write32(&mut memory, RTLD_MODULE_OBJ + 0x34, 0x0020_4288);
        write32(&mut memory, RTLD_MODULE_OBJ + 0x54, 0x0000_0011);
        write32(&mut memory, MAIN_MODULE_OBJ + 0x10, 0x0020_6000);
        write32(&mut memory, MAIN_MODULE_OBJ + 0x28, 0x00D4_3EF8);
        write32(&mut memory, MAIN_MODULE_OBJ + 0x2C, 0x00D4_471C);
        write32(&mut memory, MAIN_MODULE_OBJ + 0x30, 0x00D4_8C38);
        write32(&mut memory, MAIN_MODULE_OBJ + 0x34, 0x00D4_55A8);
        write32(&mut memory, MAIN_MODULE_OBJ + 0x54, 0x0000_0209);
        write32(&mut memory, OUT_ADDR, 0);

        let svc_addr = RTLD_BASE as u64 + memory.len() as u64 - 4;
        let svc_off = (svc_addr as u32 - RTLD_BASE) as usize;
        memory[svc_off..svc_off + 4].copy_from_slice(&0xEF00_0000u32.to_le_bytes());

        let memory = Arc::new(Mutex::new(memory));
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::from_shared_memory(
                RTLD_BASE as u64,
                memory.clone(),
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        let sp = svc_addr as u32 - 0x200;
        jit.set_register(0, RTLD_MODULE_OBJ);
        jit.set_register(1, OUT_ADDR);
        jit.set_register(2, symbol_offset);
        jit.set_register(13, sp);
        jit.set_register(14, svc_addr as u32);
        jit.set_register(15, RESOLVE_SYMBOL);
        jit.set_cpsr(0x10);
        {
            let mut memory = memory.lock().expect("mock memory poisoned");
            let off = (sp + 0x24 - RTLD_BASE) as usize;
            memory[off..off + 4].copy_from_slice(&flags.to_le_bytes());
        }

        let hr = jit.run();

        assert!(
            hr.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            hr
        );
        let result = jit.get_register(0);
        let memory = memory.lock().expect("mock memory poisoned");
        let out_off = (OUT_ADDR - RTLD_BASE) as usize;
        let resolved = u32::from_le_bytes(memory[out_off..out_off + 4].try_into().unwrap());
        (result, resolved)
    }

    #[test]
    fn test_a32_rtld_udivmod_dispatch_with_all_optimizations() {
        for (dividend, divisor) in [
            (13, 3),
            (0x0015_1875, 0x2011),
            (0x0CDD_61E4, 0x69),
            (0x0E73_EA73, 0x65),
            (0x0FA9_DE90, 0x69),
        ] {
            let (q, r) = run_a32_rtld_udivmod(dividend, divisor);
            assert_eq!(
                q,
                dividend / divisor,
                "quotient mismatch for {:#x}/{:#x}",
                dividend,
                divisor
            );
            assert_eq!(
                r,
                dividend % divisor,
                "remainder mismatch for {:#x}/{:#x}",
                dividend,
                divisor
            );
        }
    }

    // The four rtld tests below replay real RTLD memory captured from a live
    // emulators/ruzu/1/ that are not checked into the repo, so they are
    // #[ignore]d by default. Run with `cargo test -- --ignored` on a machine
    // that has the dumps.
    #[test]
    #[ignore = "requires local rtld/main .bin dumps not checked into the repo"]
    fn test_a32_rtld_lookup_in_main_finds_nn_detail_init_libc0() {
        let result = run_a32_rtld_lookup_in_main(0x0020_4502);
        assert_eq!(result, 0x00D4_8B58);
    }

    #[test]
    #[ignore = "requires local rtld/main .bin dumps not checked into the repo"]
    fn test_a32_rtld_global_lookup_finds_nn_detail_init_libc0() {
        let result = run_a32_rtld_global_lookup(0x0020_4502);
        assert_eq!(result, 0x0020_629C);
    }

    #[test]
    #[ignore = "requires local rtld/main .bin dumps not checked into the repo"]
    fn test_a32_rtld_resolve_symbol_finds_nn_detail_init_libc0() {
        let (result, resolved) =
            run_a32_rtld_symbol_resolve(0x2A, 0, OptimizationFlag::ALL_SAFE_OPTIMIZATIONS);
        assert_eq!(result, 1);
        assert_eq!(resolved, 0x0020_629C);
    }

    #[test]
    #[ignore = "requires local rtld/main .bin dumps not checked into the repo"]
    fn test_a32_rtld_resolve_symbol_finds_nn_detail_init_libc0_no_opts() {
        let (result, resolved) =
            run_a32_rtld_symbol_resolve(0x2A, 0, OptimizationFlag::NO_OPTIMIZATIONS);
        assert_eq!(result, 1);
        assert_eq!(resolved, 0x0020_629C);
    }

    #[test]
    fn test_a32_tst_imm_beq_takes_zero_path() {
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(
                0x1000,
                &[
                    0xE3A08000, // mov r8, #0
                    0xE3180C03, // tst r8, #0x300
                    0x0A000001, // beq +1
                    0xE3A00000, // mov r0, #0
                    0xEF000000, // svc 0
                    0xE3A00001, // mov r0, #1
                    0xEF000000, // svc 0
                ],
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        jit.set_register(15, 0x1000);
        jit.set_register(14, 0x2000);
        jit.set_cpsr(0x10);

        let hr = jit.run();
        assert!(
            hr.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            hr
        );
        assert_eq!(jit.get_register(0), 1);
    }

    #[test]
    fn test_thumb32_tst_imm_beq_uses_result_nz() {
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(
                0x1000,
                &[
                    0xF011_2100, // movs r1, #0; first half of tst.w r1, #0x55
                    0xD001_0F55, // second half of tst.w; beq +1
                    0xDF00_2000, // movs r0, #0; svc 0
                    0xDF00_2001, // movs r0, #1; svc 0
                ],
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        jit.set_register(15, 0x1000);
        jit.set_register(14, 0x2000);
        jit.set_cpsr(0x30);

        let hr = jit.run();
        assert!(
            hr.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            hr
        );
        assert_eq!(jit.get_register(0), 1);
    }

    #[test]
    fn test_vfp_cmp_bne_const_prop_only() {
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(
                0x1000,
                &[
                    0xE2877001, 0xEEB48AC0, 0xEEF1FA10, 0xE1540007, 0x1AFFFFFA, 0xE3A0002A,
                    0xEF000000,
                ],
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::CONST_PROP,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        jit.set_register(4, 5);
        jit.set_register(7, 3);
        jit.set_register(15, 0x1000);
        jit.set_cpsr(0x10);
        let hr = jit.run();
        assert!(hr.contains(HaltReason::SVC));
        assert_eq!(jit.get_register(7), 5);
    }

    #[test]
    fn test_vfp_cmp_bne_block_linking_only() {
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(
                0x1000,
                &[
                    0xE2877001, 0xEEB48AC0, 0xEEF1FA10, 0xE1540007, 0x1AFFFFFA, 0xE3A0002A,
                    0xEF000000,
                ],
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::BLOCK_LINKING,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).unwrap();
        jit.set_register(4, 5);
        jit.set_register(7, 3);
        jit.set_register(15, 0x1000);
        jit.set_cpsr(0x10);
        let hr = jit.run();
        assert!(hr.contains(HaltReason::SVC));
        assert_eq!(jit.get_register(7), 5);
    }

    /// Regression test for the AArch64 newlib `strchr` position-extract bug
    /// at NRO 0x80E3C714 (STK skins/music wedge).
    ///
    /// Runs the exact instruction sequence from the strchr inner loop +
    /// position-extract block, with strchr(s=path, c=':') applied to a
    /// 32-byte-aligned 31-character path that ends with a NUL terminator at
    /// byte 31. Target ':' is NOT in the path. Expected return: x3 (lower 64
    /// bits of v17 after the position-extract) = 0x8000_0000_0000_0000, which
    /// rbit/clz/tst-then-csel translates into strchr returning NULL.
    ///
    /// Two coordinated fixes (both upstream-faithful) were required:
    /// 1. `Block::inst_real_return_type` chases through Identity arg chains
    ///    to recover the real underlying type (mirrors upstream
    ///    `Inst::GetType()` microinstruction.cpp:624-628). Used by
    ///    `inst_info` build in `a64_emit_x64.rs::compile()` so spill reloads
    ///    of Identity-aliased 128-bit vectors use `movaps`, not `movsd`.
    /// 2. `floating_point_conversion_integer.rs::fmov_float_gen` now uses
    ///    `vector_get_element` to extract the proper-typed scalar (mirrors
    ///    upstream `Vpart_scalar` impl.cpp:202-210), instead of feeding the
    ///    full U128 from `v_scalar_read` into a U64-typed `set_x`.
    ///
    /// BIF uses upstream-faithful XOR-chain form `r = Vd ^ ((Vd ^ Vn) & ~Vm)`.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_strchr_position_extract_block() {
        // Position-extract block — same opcodes as NRO 0x80E3C6F4..0x80E3C72C.
        // Inputs are pre-loaded into v0/v1/v2/v7/v16; the JIT runs cmeq+cmhs+
        // orr+umaxp (loop body) then BIF/AND/ADDP/ADDP and reads back x3.
        let code: &[u32] = &[
            0x6E208C25, // cmeq v5.16b, v1.16b, v0.16b
            0x6E208C46, // cmeq v6.16b, v2.16b, v0.16b
            0x6E213CA3, // cmhs v3.16b, v5.16b, v1.16b
            0x6E223CC4, // cmhs v4.16b, v6.16b, v2.16b
            0x6EE71CA3, // bif  v3.16b, v5.16b, v7.16b
            0x6EE71CC4, // bif  v4.16b, v6.16b, v7.16b
            0x4E301C71, // and  v17.16b, v3.16b, v16.16b
            0x4E301C92, // and  v18.16b, v4.16b, v16.16b
            0x4E32BE31, // addp v17.16b, v17.16b, v18.16b
            0x4E32BE31, // addp v17.16b, v17.16b, v18.16b
            0x4E083E23, // mov  x3, v17.d[0]
            0xD4000001, // svc  #0
        ];

        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(0x1000);

        // v0 = ':' broadcast (target byte for strchr).
        let target: u8 = 0x3A;
        let v0_lane: u64 = u64::from_le_bytes([
            target, target, target, target, target, target, target, target,
        ]);
        jit.set_vector(0, v0_lane, v0_lane);

        // v1 = first 16 bytes of "//share/supertuxkart/data/skins\0":
        // "//share/supertux" (no NUL, no ':')
        let v1_lo = u64::from_le_bytes(*b"//share/");
        let v1_hi = u64::from_le_bytes(*b"supertux");
        jit.set_vector(1, v1_lo, v1_hi);

        // v2 = next 16 bytes: "kart/data/skins" + NUL at byte 31 (= byte 15 of v2).
        let v2_lo = u64::from_le_bytes(*b"kart/dat");
        let v2_hi = u64::from_le_bytes(*b"a/skins\0");
        jit.set_vector(2, v2_lo, v2_hi);

        // v7 = (v16 + v16) per u32 lane — see prologue at 0x80E3C69C.
        // v16 broadcasts 0xC0300C03 → bytes per lane [0x03,0x0C,0x30,0xC0].
        // v7 = v16 + v16 → 0x80601806 → bytes [0x06,0x18,0x60,0x80].
        let v16_lane: u32 = 0xC030_0C03;
        let v7_lane: u32 = v16_lane.wrapping_add(v16_lane); // 0x8060_1806
        let v16_64 = (v16_lane as u64) | ((v16_lane as u64) << 32);
        let v7_64 = (v7_lane as u64) | ((v7_lane as u64) << 32);
        jit.set_vector(7, v7_64, v7_64);
        jit.set_vector(16, v16_64, v16_64);

        let halt = jit.run();
        assert!(
            halt.contains(HaltReason::SVC),
            "expected SVC halt, got {:?}",
            halt
        );
        let x3 = jit.get_register(3);
        assert_eq!(
            x3, 0x8000_0000_0000_0000,
            "strchr position-extract block must yield x3=0x8000000000000000 \
             so the subsequent rbit/clz/tst/csel returns NULL for ':' not \
             present in path. Got 0x{:016X}.",
            x3
        );
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_orn_v3_16b_matches_bitwise_or_not() {
        let n = (0x00FF_00FF_F0F0_F0F0, 0x1234_5678_9ABC_DEF0);
        let m = (0x0F0F_0F0F_AAAA_5555, 0xFFFF_0000_5555_AAAA);
        let code = [
            0x4EE5_1C63, // orn v3.16b, v3.16b, v5.16b
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(&code, |j| {
            j.set_vector(3, n.0, n.1);
            j.set_vector(5, m.0, m.1);
        });
        assert_eq!(jit.get_vector(3), (n.0 | !m.0, n.1 | !m.1));
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_orn_v3_8b_zeros_upper_half() {
        let n = 0x00FF_00FF_F0F0_F0F0;
        let m = 0x0F0F_0F0F_AAAA_5555;
        let code = [
            0x0EE5_1C63, // orn v3.8b, v3.8b, v5.8b
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(&code, |j| {
            j.set_vector(3, n, u64::MAX);
            j.set_vector(5, m, u64::MAX);
        });
        assert_eq!(jit.get_vector(3), (n | !m, 0));
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_bsl_uses_vd_as_mask_and_selects_vn_else_vm() {
        let code: &[u32] = &[
            0x6E671CA3, // bsl v3.16b, v5.16b, v7.16b
            0xD4000001, // svc #0
        ];
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(0x1000);
        let d_lo = 0x00FF_00FF_F0F0_F0F0;
        let d_hi = 0xFFFF_0000_3333_CCCC;
        let n_lo = 0x1111_2222_3333_4444;
        let n_hi = 0x5555_6666_7777_8888;
        let m_lo = 0xAAAA_BBBB_CCCC_DDDD;
        let m_hi = 0xEEEE_FFFF_9999_0000;
        jit.set_vector(3, d_lo, d_hi);
        jit.set_vector(5, n_lo, n_hi);
        jit.set_vector(7, m_lo, m_hi);

        let halt = jit.run();
        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(
            jit.get_vector(3),
            (m_lo ^ ((m_lo ^ n_lo) & d_lo), m_hi ^ ((m_hi ^ n_hi) & d_hi),)
        );
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_bit_uses_vm_as_true_mask_and_preserves_vd_elsewhere() {
        let code: &[u32] = &[
            0x6EA71CA3, // bit v3.16b, v5.16b, v7.16b
            0xD4000001, // svc #0
        ];
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(0x1000);
        let d_lo = 0x0123_4567_89AB_CDEF;
        let d_hi = 0xFEDC_BA98_7654_3210;
        let n_lo = 0xFFFF_0000_FFFF_0000;
        let n_hi = 0x0000_FFFF_0000_FFFF;
        let m_lo = 0x0F0F_F0F0_AAAA_5555;
        let m_hi = 0x3333_CCCC_55AA_AA55;
        jit.set_vector(3, d_lo, d_hi);
        jit.set_vector(5, n_lo, n_hi);
        jit.set_vector(7, m_lo, m_hi);

        let halt = jit.run();
        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(
            jit.get_vector(3),
            (d_lo ^ ((d_lo ^ n_lo) & m_lo), d_hi ^ ((d_hi ^ n_hi) & m_hi),)
        );
    }

    /// Repro for STK std::bad_alloc wedge: `MOVI v31.4s, #0` should zero
    /// the entire 128-bit V31 register, regardless of any sentinel pattern
    /// pre-loaded into it. STK's allocator hits this pattern (3+ call
    /// sites: 0x80062FD4, 0x8005E63C, 0x800336BC) and a downstream `STP qN,
    /// qN, [mem]` writes the resulting v31. Production STK runs trace the
    /// 16-byte stores carrying `lo=0xFFFFFFFFFFFFFFFF, hi=0x00FFFFFF00FFFFFF`
    /// instead of all-zeros — a 24-bit "max" pattern leaking from somewhere
    /// (or an unrelated v31 carry). This test pre-fills v31 with a
    /// distinguishable sentinel and asserts MOVI clears all 16 bytes.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_movi_v31_4s_zero_clears_all_16_bytes() {
        let code: &[u32] = &[
            0x4F00041F, // movi v31.4s, #0
            0xD4000001, // svc #0
        ];
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(0x1000);
        jit.set_vector(31, 0xDEADBEEFCAFEBABE, 0x1234567890ABCDEF);
        let halt = jit.run();
        assert!(halt.contains(HaltReason::SVC));
        let (lo, hi) = jit.get_vector(31);
        assert_eq!(
            lo, 0,
            "MOVI v31.4s,#0 must zero lower 64; got 0x{:016X}",
            lo
        );
        assert_eq!(
            hi, 0,
            "MOVI v31.4s,#0 must zero upper 64; got 0x{:016X}",
            hi
        );
    }

    /// Mirrors the EXACT STK PC=0x80062FCC pattern: MOVI v31 + 3 STR xzr
    /// (5 in real STK, simplified) + 2× STP q31, q31. STK observed wrong
    /// values lo=0xFFFFFFFFFFFFFFFF, hi=0x00FFFFFF00FFFFFF being written
    /// to memory instead of the expected zeros. This test pre-fills v31
    /// with that exact sentinel before MOVI to verify it survives.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_movi_v31_then_strs_then_stp_q31_pattern() {
        let code: &[u32] = &[
            0x4F00041F, // movi v31.4s, #0
            0xD2880000, // movz x0, #0x4000
            0xD2880801, // movz x1, #0x4040
            0xF900001F, // str xzr, [x0]
            0xF900041F, // str xzr, [x0, #8]
            0xF900081F, // str xzr, [x0, #0x10]
            0xAD017C1F, // stp q31, q31, [x0, #0x20]
            0xAD007C3F, // stp q31, q31, [x1]
            0xD4000001, // svc #0
        ];
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(0x1000);
        // Pre-fill v31 with the EXACT sentinel pattern STK observed.
        jit.set_vector(31, 0xFFFFFFFFFFFFFFFF, 0x00FFFFFF00FFFFFF);
        let halt = jit.run();
        assert!(
            halt.contains(HaltReason::SVC),
            "expected SVC, got {:?}",
            halt
        );
        let (lo, hi) = jit.get_vector(31);
        assert_eq!(
            lo, 0,
            "MOVI v31.4s,#0 must zero lower 64 even after subsequent str+stp; \
             got 0x{:016X}",
            lo
        );
        assert_eq!(
            hi, 0,
            "MOVI v31.4s,#0 must zero upper 64 even after subsequent str+stp; \
             got 0x{:016X}",
            hi
        );
    }

    /// ADDV B0, V0.8B — sums all 8 bytes of V0 (size=00, Q=0) and stores
    /// the truncated 8-bit result in B0 (low byte of V0). Encoding
    /// 0x0E31B800 — STK NRO at pc=0x80B473A0 hits this on init; the
    /// instruction had no translator and fell into the
    /// interpret-fallback no-op, looping at PrefetchAbort forever.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_addv_b0_v0_8b_sums_8_bytes() {
        let code: &[u32] = &[
            0x0E31B800, // addv b0, v0.8b
            0xD4000001, // svc #0
        ];
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(0x1000);
        // V0.8B = bytes [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]
        // sum = 0x24 = 36; truncated to 8 bits → 0x24
        jit.set_vector(0, 0x0807060504030201, 0xFFFFFFFFFFFFFFFF);
        let halt = jit.run();
        assert!(
            halt.contains(HaltReason::SVC),
            "expected SVC, got {:?}",
            halt
        );
        let (lo, hi) = jit.get_vector(0);
        assert_eq!(
            lo & 0xFF,
            0x24,
            "ADDV B0, V0.8B should put sum=0x24 in low byte; got 0x{:016X}",
            lo
        );
        assert_eq!(
            lo & !0xFFu64,
            0,
            "ADDV must zero bits 63:8 of Vd; got 0x{:016X}",
            lo
        );
        assert_eq!(hi, 0, "ADDV must zero upper 64 of Vd; got 0x{:016X}", hi);
    }

    /// UADDLV H0, V0.8B reduces 8 unsigned bytes and stores the long 16-bit
    /// sum in H0.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_uaddlv_h0_v0_8b_sums_unsigned_bytes_long() {
        let code: &[u32] = &[
            0x2E30_3800, // uaddlv h0, v0.8b
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            j.set_vector(0, 0x0807_0605_0403_0201, 0xFFFF_FFFF_FFFF_FFFF);
        });
        let (lo, hi) = jit.get_vector(0);
        assert_eq!(
            lo, 0x24,
            "UADDLV should write 16-bit sum and clear Vd low D"
        );
        assert_eq!(hi, 0, "UADDLV should clear Vd upper D");
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_uaddw_v6_8h_v2_8h_v4_8b_widens_and_adds() {
        let code: &[u32] = &[
            0x2E24_1046, // uaddw v6.8h, v2.8h, v4.8b
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            j.set_vector(2, 0x0067_0066_0065_0064, 0x006B_006A_0069_0068);
            j.set_vector(4, 0x0807_0605_0403_0201, u64::MAX);
            j.set_vector(6, u64::MAX, u64::MAX);
        });
        assert_eq!(
            jit.get_vector(6),
            (0x006B_0069_0067_0065, 0x0073_0071_006F_006D)
        );
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_sqrdmulh_v1_8h_v1_8h_v7_h3_rounds_high_product() {
        let code: &[u32] = &[
            0x4F77_D021, // sqrdmulh v1.8h, v1.8h, v7.h[3]
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            j.set_vector(1, 0x03E8_03E8_03E8_03E8, 0x03E8_03E8_03E8_03E8);
            j.set_vector(7, 0x4000_0000_0000_0000, 0);
        });
        assert_eq!(
            jit.get_vector(1),
            (0x01F4_01F4_01F4_01F4, 0x01F4_01F4_01F4_01F4)
        );
    }

    /// SADDLV H0, V0.8B is the signed counterpart of UADDLV.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_saddlv_h0_v0_8b_sums_signed_bytes_long() {
        let code: &[u32] = &[
            0x0E30_3800, // saddlv h0, v0.8b
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            // Bytes: [-1, -2, 1, 2, 3, 4, 5, 6] => 18 = 0x0012.
            j.set_vector(0, 0x0605_0403_0201_FEFF, 0xFFFF_FFFF_FFFF_FFFF);
        });
        let (lo, hi) = jit.get_vector(0);
        assert_eq!(lo, 0x12, "SADDLV should sign-extend bytes before summing");
        assert_eq!(hi, 0, "SADDLV should clear Vd upper D");
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_clz_v0_2s_counts_each_lane_and_zeros_upper_half() {
        let code: &[u32] = &[
            0x2EA0_4800, // clz v0.2s, v0.2s
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            j.set_vector(0, 0x00F0_0000_0000_0001, u64::MAX);
        });

        assert_eq!(jit.get_vector(0), (0x0000_0008_0000_001F, 0));
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_shll_and_shll2_zero_extend_selected_half_and_shift_by_element_size() {
        let shll = run_a64_alu(&[0x2E61_3800, 0xD400_0001], |j| {
            j.set_vector(0, 0xFFFF_8000_7FFF_0001, u64::MAX);
        });
        assert_eq!(
            shll.get_vector(0),
            (0x7FFF_0000_0001_0000, 0xFFFF_0000_8000_0000)
        );

        let shll2 = run_a64_alu(&[0x6E61_3800, 0xD400_0001], |j| {
            j.set_vector(0, u64::MAX, 0x0005_0004_0003_0002);
        });
        assert_eq!(
            shll2.get_vector(0),
            (0x0003_0000_0002_0000, 0x0005_0000_0004_0000)
        );
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_sqxtn_v16_8b_v15_8h_saturates_and_sets_qc() {
        let code: &[u32] = &[
            0x0E21_49F0, // sqxtn v16.8b, v15.8h
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            // Signed halfwords: [-129, -128, -1, 0, 1, 127, 128, 300].
            j.set_vector(15, 0x0000_FFFF_FF80_FF7F, 0x012C_0080_007F_0001);
            j.set_vector(16, u64::MAX, u64::MAX);
        });
        let (lo, hi) = jit.get_vector(16);
        assert_eq!(lo, 0x7F7F_7F01_00FF_8080);
        assert_eq!(hi, 0, "SQXTN must zero the destination's upper half");
        assert_ne!(jit.get_fpsr() & (1 << 27), 0, "SQXTN must set FPSR.QC");
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_sqxtn_b16_h15_extracts_and_saturates_scalar_halfword() {
        let code: &[u32] = &[
            0x5E21_49F0, // sqxtn b16, h15
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            j.set_vector(15, 0xABCD_EF01_2345_FF7F, u64::MAX);
            j.set_vector(16, u64::MAX, u64::MAX);
        });
        assert_eq!(jit.get_vector(16), (0x80, 0));
        assert_ne!(jit.get_fpsr() & (1 << 27), 0, "SQXTN must set FPSR.QC");
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_smull2_v19_4s_v18_8h_v0_h2_multiplies_high_half() {
        let code: &[u32] = &[
            0x4F60_A253, // smull2 v19.4s, v18.8h, v0.h[2]
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            j.set_vector(18, 0, 0x0007_FFFA_0005_FFFC);
            j.set_vector(0, 0x0000_FFFD_0000_0000, 0);
            j.set_vector(19, u64::MAX, u64::MAX);
        });
        assert_eq!(
            jit.get_vector(19),
            (0xFFFF_FFF1_0000_000C, 0xFFFF_FFEB_0000_0012)
        );
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_sqrshrn_v28_8b_v2_8h_rounds_saturates_and_sets_qc() {
        let code: &[u32] = &[
            0x0F0E_9C5C, // sqrshrn v28.8b, v2.8h, #2
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            // Signed halfwords: [-1000, -6, -5, -4, 2, 3, 4, 1000].
            j.set_vector(2, 0xFFFC_FFFB_FFFA_FC18, 0x03E8_0004_0003_0002);
            j.set_vector(28, u64::MAX, u64::MAX);
        });
        assert_eq!(jit.get_vector(28), (0x7F01_0101_FFFF_FF80, 0));
        assert_ne!(jit.get_fpsr() & (1 << 27), 0, "SQRSHRN must set FPSR.QC");
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_ssra_v28_4s_v29_4s_accumulates_signed_shift() {
        let code: &[u32] = &[
            0x4F34_17BC, // ssra v28.4s, v29.4s, #12
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            j.set_vector(28, 0x0000_0064_0000_0001, 0x2000_0000_1000_0000);
            j.set_vector(29, 0xFFFE_C000_0001_2000, 0x8000_0000_7FFF_FFFF);
        });
        assert_eq!(
            jit.get_vector(28),
            (0x0000_0050_0000_0013, 0x1FF8_0000_1007_FFFF)
        );
    }

    /// USHL V0.2S, V0.2S, V1.2S shifts left for positive counts and right
    /// logically for negative counts.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_ushl_v0_2s_variable_logical_shift() {
        let code: &[u32] = &[
            0x2EA1_4400, // ushl v0.2s, v0.2s, v1.2s
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            j.set_vector(0, 0x8000_0000_0000_0001, 0xFFFF_FFFF_FFFF_FFFF);
            j.set_vector(1, 0xFFFF_FFFF_0000_0003, 0);
        });
        let (lo, hi) = jit.get_vector(0);
        assert_eq!(lo, 0x4000_0000_0000_0008);
        assert_eq!(hi, 0);
    }

    /// SMIN V1.2S, V1.2S, V3.2S computes signed lane-wise minima.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_smin_v1_2s_signed_minimum() {
        let code: &[u32] = &[
            0x0EA3_6C21, // smin v1.2s, v1.2s, v3.2s
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            j.set_vector(1, 0xFFFF_FFF6_0000_0005, 0xFFFF_FFFF_FFFF_FFFF);
            j.set_vector(3, 0x0000_0014_FFFF_FFFE, 0);
        });
        let (lo, hi) = jit.get_vector(1);
        assert_eq!(lo, 0xFFFF_FFF6_FFFF_FFFE);
        assert_eq!(hi, 0);
    }

    /// ADDV H0, V0.4H — sums 4 halfwords (size=01, Q=0). Truncates to 16
    /// bits in Vd. Tests the size=01 path of the ADDV translator.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_addv_h0_v0_4h_sums_4_halves() {
        let code: &[u32] = &[
            0x0E71B800, // addv h0, v0.4h
            0xD4000001, // svc #0
        ];
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(0x1000);
        // V0.4H = halfs [0x1234, 0x5678, 0x9ABC, 0xDEF0]
        // sum_u32 = 0x1234 + 0x5678 + 0x9ABC + 0xDEF0 = 0x1E258;
        // truncated to 16 bits → 0xE258.
        jit.set_vector(0, 0xDEF0_9ABC_5678_1234, 0xCAFEBABE_DEADBEEF);
        let halt = jit.run();
        assert!(halt.contains(HaltReason::SVC));
        let (lo, hi) = jit.get_vector(0);
        assert_eq!(
            lo & 0xFFFF,
            0xE258,
            "ADDV H0, V0.4H sum & 0xFFFF should be 0xE258; got 0x{:016X}",
            lo
        );
        assert_eq!(
            lo & !0xFFFFu64,
            0,
            "ADDV must zero bits 63:16 of Vd; got 0x{:016X}",
            lo
        );
        assert_eq!(hi, 0, "ADDV must zero upper 64 of Vd; got 0x{:016X}", hi);
    }

    /// More aggressive repro: MOVI v31, then a memory write whose host_call
    /// path spills caller-save XMMs, then verify v31 survives the spill.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_movi_v31_survives_host_call_spill() {
        // movi v31.4s, #0
        // mov x0, #0x4000      ; some unmapped addr → write goes to callback
        // str xzr, [x0]        ; 8-byte zero write (forces host_call/spill)
        // svc #0
        let code: &[u32] = &[
            0x4F00041F, // movi v31.4s, #0
            0xD2880000, // movz x0, #0x4000
            0xF900001F, // str xzr, [x0]
            0xD4000001, // svc #0
        ];
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(0x1000);
        // Pre-fill v31 with a distinguishable sentinel that is NOT 0.
        jit.set_vector(31, 0xDEADBEEFCAFEBABE, 0x1234567890ABCDEF);
        let halt = jit.run();
        assert!(halt.contains(HaltReason::SVC));
        let (lo, hi) = jit.get_vector(31);
        assert_eq!(
            lo, 0,
            "MOVI v31.4s,#0 must zero lower 64 bits; got 0x{:016X}",
            lo
        );
        assert_eq!(
            hi, 0,
            "MOVI v31.4s,#0 must zero upper 64 bits; got 0x{:016X}",
            hi
        );
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_uabdl_v5_8h_v4_8b_v7_8b_widens_unsigned_difference() {
        let code: &[u32] = &[
            0x2E27_7085, // uabdl v5.8h, v4.8b, v7.8b
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            j.set_vector(4, 0x8032_C801_64FF_0A00, 0);
            j.set_vector(7, 0x8046_64FF_9600_0305, 0);
            j.set_vector(5, u64::MAX, u64::MAX);
        });
        assert_eq!(
            jit.get_vector(5),
            (0x0032_00FF_0007_0005, 0x0000_0014_0064_00FE)
        );
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_sabdl_v5_8h_v4_8b_v7_8b_uses_signed_difference() {
        let code: &[u32] = &[
            0x0E27_7085, // sabdl v5.8h, v4.8b, v7.8b
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            j.set_vector(4, 0x0000_0000_00FF_7F80, 0);
            j.set_vector(7, 0x0000_0000_FF01_807F, 0);
            j.set_vector(5, u64::MAX, u64::MAX);
        });
        let (lo, hi) = jit.get_vector(5);
        assert_eq!(lo, 0x0001_0002_00FF_00FF);
        assert_eq!(hi, 0);
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_uabd_v28_8h_v28_8h_v6_8h_uses_unsigned_difference() {
        let code: &[u32] = &[
            0x6E66_779C, // uabd v28.8h, v28.8h, v6.8h
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            j.set_vector(28, 0xFFFF_0064_0001_0000, 0x1234_00C8_0032_8000);
            j.set_vector(6, 0x0000_0014_FFFF_0005, 0x4321_0032_0064_7FFF);
        });
        assert_eq!(
            jit.get_vector(28),
            (0xFFFF_0050_FFFE_0005, 0x30ED_0096_0032_0001)
        );
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_saba_v28_8h_v28_8h_v6_8h_accumulates_signed_difference() {
        let code: &[u32] = &[
            0x4E66_7F9C, // saba v28.8h, v28.8h, v6.8h
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            // V28 is both the initial accumulator and the first operand.
            j.set_vector(28, 0x000A_FFFF_007F_FF80, 0xFFFE_0003_8000_7FFF);
            j.set_vector(6, 0xFFEC_0001_FF80_007F, 0x0002_FFFD_7FFF_8000);
        });
        assert_eq!(
            jit.get_vector(28),
            (0x0028_0001_017E_007F, 0x0002_0009_7FFF_7FFE)
        );
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_uhadd_and_urhadd_v8b_distinguish_truncation_from_rounding() {
        let code: &[u32] = &[
            0x2E34_0400, // uhadd v0.8b, v0.8b, v20.8b
            0x2E34_1441, // urhadd v1.8b, v2.8b, v20.8b
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            let operand = 0xFFFE_C864_0302_0100;
            j.set_vector(0, operand, u64::MAX);
            j.set_vector(2, operand, u64::MAX);
            j.set_vector(20, 0xFFFF_64C8_0403_0200, u64::MAX);
        });
        assert_eq!(jit.get_vector(0), (0xFFFE_9696_0302_0100, 0));
        assert_eq!(jit.get_vector(1), (0xFFFF_9696_0403_0200, 0));
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_srhadd_and_urhadd_all_element_sizes() {
        fn vector_pair(bytes: [u8; 16]) -> (u64, u64) {
            (
                u64::from_le_bytes(bytes[..8].try_into().unwrap()),
                u64::from_le_bytes(bytes[8..].try_into().unwrap()),
            )
        }

        let a = [
            0x80, 0x7f, 0xff, 0x00, 0x01, 0xfe, 0x40, 0xc0, 0x34, 0x12, 0x78, 0x56, 0xef, 0xcd,
            0xab, 0x89,
        ];
        let b = [
            0x7f, 0x80, 0x01, 0xff, 0xff, 0x02, 0xc0, 0x40, 0xcc, 0xed, 0x88, 0xa9, 0x11, 0x32,
            0x55, 0x76,
        ];

        let mut signed8 = [0u8; 16];
        let mut unsigned8 = [0u8; 16];
        for lane in 0..16 {
            signed8[lane] =
                (((a[lane] as i8 as i16) + (b[lane] as i8 as i16) + 1) >> 1) as i8 as u8;
            unsigned8[lane] = ((a[lane] as u16 + b[lane] as u16 + 1) >> 1) as u8;
        }

        let mut signed16 = [0u8; 16];
        let mut unsigned16 = [0u8; 16];
        for lane in 0..8 {
            let offset = lane * 2;
            let lhs_signed = i16::from_le_bytes(a[offset..offset + 2].try_into().unwrap());
            let rhs_signed = i16::from_le_bytes(b[offset..offset + 2].try_into().unwrap());
            let lhs_unsigned = u16::from_le_bytes(a[offset..offset + 2].try_into().unwrap());
            let rhs_unsigned = u16::from_le_bytes(b[offset..offset + 2].try_into().unwrap());
            signed16[offset..offset + 2].copy_from_slice(
                &(((lhs_signed as i32 + rhs_signed as i32 + 1) >> 1) as i16).to_le_bytes(),
            );
            unsigned16[offset..offset + 2].copy_from_slice(
                &(((lhs_unsigned as u32 + rhs_unsigned as u32 + 1) >> 1) as u16).to_le_bytes(),
            );
        }

        let mut signed32 = [0u8; 16];
        let mut unsigned32 = [0u8; 16];
        for lane in 0..4 {
            let offset = lane * 4;
            let lhs_signed = i32::from_le_bytes(a[offset..offset + 4].try_into().unwrap());
            let rhs_signed = i32::from_le_bytes(b[offset..offset + 4].try_into().unwrap());
            let lhs_unsigned = u32::from_le_bytes(a[offset..offset + 4].try_into().unwrap());
            let rhs_unsigned = u32::from_le_bytes(b[offset..offset + 4].try_into().unwrap());
            signed32[offset..offset + 4].copy_from_slice(
                &(((lhs_signed as i64 + rhs_signed as i64 + 1) >> 1) as i32).to_le_bytes(),
            );
            unsigned32[offset..offset + 4].copy_from_slice(
                &(((lhs_unsigned as u64 + rhs_unsigned as u64 + 1) >> 1) as u32).to_le_bytes(),
            );
        }

        let code: &[u32] = &[
            0x4E35_1680, // srhadd v0.16b, v20.16b, v21.16b
            0x4E75_1681, // srhadd v1.8h, v20.8h, v21.8h
            0x4EB5_1682, // srhadd v2.4s, v20.4s, v21.4s
            0x6E35_1683, // urhadd v3.16b, v20.16b, v21.16b
            0x6E75_1684, // urhadd v4.8h, v20.8h, v21.8h
            0x6EB5_1685, // urhadd v5.4s, v20.4s, v21.4s
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            let (a_low, a_high) = vector_pair(a);
            let (b_low, b_high) = vector_pair(b);
            j.set_vector(20, a_low, a_high);
            j.set_vector(21, b_low, b_high);
        });

        assert_eq!(jit.get_vector(0), vector_pair(signed8));
        assert_eq!(jit.get_vector(1), vector_pair(signed16));
        assert_eq!(jit.get_vector(2), vector_pair(signed32));
        assert_eq!(jit.get_vector(3), vector_pair(unsigned8));
        assert_eq!(jit.get_vector(4), vector_pair(unsigned16));
        assert_eq!(jit.get_vector(5), vector_pair(unsigned32));
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_addp_full_and_lower_all_element_sizes() {
        fn vector_pair(bytes: [u8; 16]) -> (u64, u64) {
            (
                u64::from_le_bytes(bytes[..8].try_into().unwrap()),
                u64::from_le_bytes(bytes[8..].try_into().unwrap()),
            )
        }

        fn paired_add(a: [u8; 16], b: [u8; 16], element_bytes: usize, lower: bool) -> [u8; 16] {
            let mut result = [0u8; 16];
            let source_bytes = if lower { 8 } else { 16 };
            let pairs_per_source = source_bytes / (element_bytes * 2);
            for (source_index, source) in [a, b].into_iter().enumerate() {
                for pair in 0..pairs_per_source {
                    let input_offset = pair * element_bytes * 2;
                    let output_offset = (source_index * pairs_per_source + pair) * element_bytes;
                    let lhs = u64::from_le_bytes({
                        let mut bytes = [0u8; 8];
                        bytes[..element_bytes]
                            .copy_from_slice(&source[input_offset..input_offset + element_bytes]);
                        bytes
                    });
                    let rhs = u64::from_le_bytes({
                        let mut bytes = [0u8; 8];
                        bytes[..element_bytes].copy_from_slice(
                            &source[input_offset + element_bytes..input_offset + element_bytes * 2],
                        );
                        bytes
                    });
                    let mask = if element_bytes == 8 {
                        u64::MAX
                    } else {
                        (1u64 << (element_bytes * 8)) - 1
                    };
                    result[output_offset..output_offset + element_bytes].copy_from_slice(
                        &(lhs.wrapping_add(rhs) & mask).to_le_bytes()[..element_bytes],
                    );
                }
            }
            result
        }

        let a = [
            0xff, 0x02, 0x7f, 0x81, 0x34, 0x12, 0xcc, 0xed, 0x78, 0x56, 0x88, 0xa9, 0xef, 0xcd,
            0x11, 0x32,
        ];
        let b = [
            0x80, 0x80, 0x01, 0xfe, 0x01, 0x00, 0xff, 0xff, 0xff, 0xff, 0x01, 0x00, 0x40, 0xc0,
            0xc0, 0x40,
        ];
        let code: &[u32] = &[
            0x4E35_BE80, // addp v0.16b, v20.16b, v21.16b
            0x4E75_BE81, // addp v1.8h, v20.8h, v21.8h
            0x4EB5_BE82, // addp v2.4s, v20.4s, v21.4s
            0x4EF5_BE83, // addp v3.2d, v20.2d, v21.2d
            0x0E35_BE84, // addp v4.8b, v20.8b, v21.8b
            0x0E75_BE85, // addp v5.4h, v20.4h, v21.4h
            0x0EB5_BE86, // addp v6.2s, v20.2s, v21.2s
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            let (a_low, a_high) = vector_pair(a);
            let (b_low, b_high) = vector_pair(b);
            j.set_vector(20, a_low, a_high);
            j.set_vector(21, b_low, b_high);
        });

        assert_eq!(jit.get_vector(0), vector_pair(paired_add(a, b, 1, false)));
        assert_eq!(jit.get_vector(1), vector_pair(paired_add(a, b, 2, false)));
        assert_eq!(jit.get_vector(2), vector_pair(paired_add(a, b, 4, false)));
        assert_eq!(jit.get_vector(3), vector_pair(paired_add(a, b, 8, false)));
        assert_eq!(jit.get_vector(4), vector_pair(paired_add(a, b, 1, true)));
        assert_eq!(jit.get_vector(5), vector_pair(paired_add(a, b, 2, true)));
        assert_eq!(jit.get_vector(6), vector_pair(paired_add(a, b, 4, true)));
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_saddlp_and_uaddlp_all_element_sizes() {
        fn vector_pair(bytes: [u8; 16]) -> (u64, u64) {
            (
                u64::from_le_bytes(bytes[..8].try_into().unwrap()),
                u64::from_le_bytes(bytes[8..].try_into().unwrap()),
            )
        }

        fn paired_add_widen(source: [u8; 16], element_bytes: usize, signed: bool) -> [u8; 16] {
            let mut result = [0u8; 16];
            let output_bytes = element_bytes * 2;
            for pair in 0..16 / output_bytes {
                let input_offset = pair * output_bytes;
                let mut lhs_bytes = [0u8; 8];
                let mut rhs_bytes = [0u8; 8];
                lhs_bytes[..element_bytes]
                    .copy_from_slice(&source[input_offset..input_offset + element_bytes]);
                rhs_bytes[..element_bytes].copy_from_slice(
                    &source[input_offset + element_bytes..input_offset + output_bytes],
                );
                let output_offset = pair * output_bytes;
                if signed {
                    let shift = 64 - element_bytes * 8;
                    let lhs = ((u64::from_le_bytes(lhs_bytes) << shift) as i64) >> shift;
                    let rhs = ((u64::from_le_bytes(rhs_bytes) << shift) as i64) >> shift;
                    result[output_offset..output_offset + output_bytes]
                        .copy_from_slice(&lhs.wrapping_add(rhs).to_le_bytes()[..output_bytes]);
                } else {
                    let lhs = u64::from_le_bytes(lhs_bytes);
                    let rhs = u64::from_le_bytes(rhs_bytes);
                    result[output_offset..output_offset + output_bytes]
                        .copy_from_slice(&lhs.wrapping_add(rhs).to_le_bytes()[..output_bytes]);
                }
            }
            result
        }

        let source = [
            0x80, 0x7f, 0xff, 0x02, 0x34, 0x12, 0xcc, 0xed, 0x78, 0x56, 0x88, 0xa9, 0xef, 0xcd,
            0x11, 0x32,
        ];
        let code: &[u32] = &[
            0x4E20_2A80, // saddlp v0.8h, v20.16b
            0x4E60_2A81, // saddlp v1.4s, v20.8h
            0x4EA0_2A82, // saddlp v2.2d, v20.4s
            0x6E20_2A83, // uaddlp v3.8h, v20.16b
            0x6E60_2A84, // uaddlp v4.4s, v20.8h
            0x6EA0_2A85, // uaddlp v5.2d, v20.4s
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            let (low, high) = vector_pair(source);
            j.set_vector(20, low, high);
        });

        assert_eq!(
            jit.get_vector(0),
            vector_pair(paired_add_widen(source, 1, true))
        );
        assert_eq!(
            jit.get_vector(1),
            vector_pair(paired_add_widen(source, 2, true))
        );
        assert_eq!(
            jit.get_vector(2),
            vector_pair(paired_add_widen(source, 4, true))
        );
        assert_eq!(
            jit.get_vector(3),
            vector_pair(paired_add_widen(source, 1, false))
        );
        assert_eq!(
            jit.get_vector(4),
            vector_pair(paired_add_widen(source, 2, false))
        );
        assert_eq!(
            jit.get_vector(5),
            vector_pair(paired_add_widen(source, 4, false))
        );
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_sqadd_v1_8h_saturates_and_sets_qc() {
        let code: &[u32] = &[
            0x4E61_0CE1, // sqadd v1.8h, v7.8h, v1.8h
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            j.set_vector(7, 0x7530_9C40_0002_FFFF, 0x0001_0001_0001_0001);
            j.set_vector(1, 0x2710_D8F0_7FFE_8000, 0x0002_0002_0002_0002);
        });
        assert_eq!(
            jit.get_vector(1),
            (0x7FFF_8000_7FFF_8000, 0x0003_0003_0003_0003)
        );
        assert_ne!(jit.get_fpsr() & (1 << 27), 0, "SQADD must set FPSR.QC");
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_frintn_v30_4s_rounds_to_nearest_tie_even() {
        let code: &[u32] = &[
            0x4E21_8BDE, // frintn v30.4s, v30.4s
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            j.set_vector(30, 0x3FC0_0000_3FB3_3333, 0xBFC0_0000_4020_0000);
        });
        assert_eq!(
            jit.get_vector(30),
            (0x4000_0000_3F80_0000, 0xC000_0000_4000_0000)
        );
    }

    #[cfg(target_arch = "aarch64")]
    fn run_native_a64_simd_instruction(
        instruction: u32,
        mut vectors: [[u64; 2]; 32],
    ) -> [[u64; 2]; 32] {
        use crate::backend::arm64::block_of_code::BlockOfCode;
        use crate::backend::arm64::inst;

        let mut code = BlockOfCode::with_size(4096).expect("native SIMD oracle code cache");
        code.write_u32(inst::sub_sp_imm(128)).unwrap();
        for register in 8..16 {
            code.write_u32(inst::str_q_unsigned_sp(
                register,
                (register as u32 - 8) * 16,
            ))
            .unwrap();
        }
        for register in 0..32 {
            code.write_u32(inst::ldr_q_unsigned(register, 0, register as u32 * 16))
                .unwrap();
        }
        code.write_u32(instruction).unwrap();
        for register in 0..32 {
            code.write_u32(inst::str_q_unsigned(register, 0, register as u32 * 16))
                .unwrap();
        }
        for register in 8..16 {
            code.write_u32(inst::ldr_q_unsigned_sp(
                register,
                (register as u32 - 8) * 16,
            ))
            .unwrap();
        }
        code.write_u32(inst::add_sp_imm(128)).unwrap();
        code.write_u32(inst::ret_lr()).unwrap();
        code.seal();

        let function: unsafe extern "C" fn(*mut [[u64; 2]; 32]) =
            unsafe { std::mem::transmute(code.code_base_ptr()) };
        unsafe { function(&mut vectors) };
        vectors
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn test_a64_simd_translation_matches_native_aarch64_results() {
        let cases = [
            (0x0E21_49F0, 16, 0x7F80_7F7F_807F_8080, 0),
            (
                0x4F60_A253,
                19,
                0xF94D_A970_0D33_E330,
                0x0A3D_6AE8_0526_B338,
            ),
            (
                0x4F70_2913,
                19,
                0xB639_9859_A954_3C58,
                0x96AE_2EB7_189B_E264,
            ),
            (0x4F57_6283, 3, 0x0235_EF05_B8F2_3270, 0x6A6E_7585_632C_C344),
            (0x0F0E_9C5C, 28, 0x7F7F_7F7F_8080_8080, 0),
            (
                0x4F0E_9CDC,
                28,
                0xE787_C790_8A48_7D70,
                0x7F7F_7F7F_8080_8080,
            ),
            (0x0F15_8E42, 2, 0xDEDB_F894_F9AC_8145, 0),
            (0x4F15_8E02, 2, 0xBEBC_C936_D5DD_C5C0, 0xB31E_2458_1C6F_AB87),
            (0x2E27_00E6, 6, 0x018C_01DC_01EC_0110, 0x01EC_01E6_0124_00DA),
            (0x2E27_7085, 5, 0x000E_00BB_0005_0078, 0x00D4_0095_0058_0045),
            (
                0x6E66_779C,
                28,
                0x2E27_27B6_3D31_7F30,
                0x0F6B_3042_3C62_4688,
            ),
            (0x2E34_0400, 0, 0x6B60_C74A_C487_75A0, 0),
            (0x2E24_1046, 6, 0xBF90_C969_D6CE_C5D0, 0x2008_7150_51FE_42FC),
            (0x2E21_2B9C, 28, 0xFFFF_FF00_0000_00FF, 0),
            (0x4F77_D021, 1, 0xF729_03C8_02A7_0324, 0x035D_FA77_F9F4_F9B5),
            (0x0EA0_0800, 0, 0xDAEF_D9B0_3B92_DC1C, 0),
        ];

        for (word, destination, expected_lo, expected_hi) in cases {
            let code = [word, 0xD400_0001];
            let jit = run_a64_alu(&code, |jit| {
                for index in 0..32 {
                    let lo_index = 2 * index;
                    let hi_index = lo_index + 1;
                    let pattern = |i: usize| {
                        0x9E37_79B9_7F4A_7C15u64.wrapping_mul((i + 1) as u64)
                            ^ (0xA5A5_A5A5_A5A5_A5A5u64
                                .wrapping_add((i as u64).wrapping_mul(0x0101_0101_0101_0101)))
                    };
                    jit.set_vector(index, pattern(lo_index), pattern(hi_index));
                }
            });
            assert_eq!(
                jit.get_vector(destination),
                (expected_lo, expected_hi),
                "native AArch64 mismatch for instruction 0x{word:08X}"
            );
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn test_a64_simd_translation_preserves_all_native_register_results() {
        let instructions = [
            0x0E21_49F0,
            0x4F60_A253,
            0x4F70_2913,
            0x4F57_6283,
            0x0F0E_9C5C,
            0x4F0E_9CDC,
            0x0F15_8E42,
            0x4F15_8E02,
            0x2E27_00E6,
            0x2E27_7085,
            0x6E66_779C,
            0x2E34_0400,
            0x2E24_1046,
            0x2E21_2B9C,
            0x4F77_D021,
            0x0EA0_0800,
        ];
        let initial = std::array::from_fn(|index| {
            let pattern = |lane: usize| {
                0x9E37_79B9_7F4A_7C15u64.wrapping_mul((lane + 1) as u64)
                    ^ (0xA5A5_A5A5_A5A5_A5A5u64
                        .wrapping_add((lane as u64).wrapping_mul(0x0101_0101_0101_0101)))
            };
            [pattern(index * 2), pattern(index * 2 + 1)]
        });

        for instruction in instructions {
            let native = run_native_a64_simd_instruction(instruction, initial);
            let code = [instruction, 0xD400_0001];
            let jit = run_a64_alu(&code, |jit| {
                for (index, [lo, hi]) in initial.into_iter().enumerate() {
                    jit.set_vector(index, lo, hi);
                }
            });
            let translated = std::array::from_fn(|index| {
                let (lo, hi) = jit.get_vector(index);
                [lo, hi]
            });
            assert_eq!(
                translated, native,
                "translated register state differs after instruction 0x{instruction:08X}"
            );
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn test_a64_ld4_2s_reads_from_rn_and_deinterleaves_lanes() {
        const NOP: u32 = 0xD503_201F;
        let code = [
            0x0C40_0830, // ld4 {v16.2s-v19.2s}, [x1]
            NOP,
            NOP,
            NOP,
            NOP,
            NOP,
            NOP,
            NOP,
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu_with_optimizations(
            &code,
            OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
            |jit| jit.set_register(1, 0x1000),
        );

        assert_eq!(
            jit.get_vector(16),
            (((NOP as u64) << 32) | code[0] as u64, 0)
        );
        assert_eq!(jit.get_vector(17), (((NOP as u64) << 32) | NOP as u64, 0));
        assert_eq!(jit.get_vector(18), (((NOP as u64) << 32) | NOP as u64, 0));
        assert_eq!(jit.get_vector(19), (((NOP as u64) << 32) | NOP as u64, 0));
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn test_a64_indirect_tail_call_preserves_ld4_address_in_x1() {
        const NOP: u32 = 0xD503_201F;
        let code = [
            0xA9BC_7BFD, // stp x29, x30, [sp, #-0x40]!
            0x9100_03FD, // mov x29, sp
            0xA901_53F3, // stp x19, x20, [sp, #0x10]
            0xA902_5BF5, // stp x21, x22, [sp, #0x20]
            0xAA01_03F6, // mov x22, x1
            0xAA02_03F5, // mov x21, x2
            0xAA00_03F4, // mov x20, x0
            0xAA15_03E1, // mov x1, x21
            0xA941_53F3, // ldp x19, x20, [sp, #0x10]
            0xAA16_03E0, // mov x0, x22
            0xA942_5BF5, // ldp x21, x22, [sp, #0x20]
            0xAA03_03F0, // mov x16, x3
            0xA8C4_7BFD, // ldp x29, x30, [sp], #0x40
            0xD61F_0200, // br x16
            NOP,
            NOP,
            0x0C40_0830, // ld4 {v16.2s-v19.2s}, [x1]
            NOP,
            NOP,
            NOP,
            NOP,
            NOP,
            NOP,
            NOP,
            0xD400_0001, // svc #0
        ];
        let mut jit = run_a64_alu_with_optimizations(
            &code,
            OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
            |jit| {
                jit.set_sp(0x9000);
                jit.set_register(1, 0x2000);
                jit.set_register(2, 0x1040);
                jit.set_register(3, 0x1040);
            },
        );
        run_a64_until_svc(&mut jit);

        assert_eq!(
            jit.get_vector(16),
            (((NOP as u64) << 32) | code[16] as u64, 0)
        );
        assert_eq!(jit.get_vector(17), (((NOP as u64) << 32) | NOP as u64, 0));
        assert_eq!(jit.get_vector(18), (((NOP as u64) << 32) | NOP as u64, 0));
        assert_eq!(jit.get_vector(19), (((NOP as u64) << 32) | NOP as u64, 0));
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn test_a64_direct_call_preserves_x4_argument_between_blocks() {
        let code = [
            0xAA13_03E4, // mov x4, x19
            0x9400_0003, // bl 0x1010
            0xD503_201F, // nop
            0xD503_201F, // nop
            0xAA04_03E0, // mov x0, x4
            0xD400_0001, // svc #0
        ];
        let mut jit = run_a64_alu_with_optimizations(
            &code,
            OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
            |jit| jit.set_register(19, 0x2112_3456_78),
        );
        run_a64_until_svc(&mut jit);

        assert_eq!(jit.get_register(0), 0x2112_3456_78);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn test_a64_direct_call_preserves_adjacent_stack_and_immediate_arguments() {
        let code = [
            0x7949_A660, // ldrh w0, [x19, #0x4d2]
            0x377F_F780, // tbnz w0, #15, 0x1000
            0x3720_0180, // tbnz w0, #4, 0x1038
            0x321C_0004, // orr w4, w0, #0x10
            0x9112_2261, // add x1, x19, #0x488
            0x5280_0023, // mov w3, #1
            0x9101_03E2, // add x2, sp, #0x40
            0xAA13_03E0, // mov x0, x19
            0x7909_A664, // strh w4, [x19, #0x4d2]
            0x9400_0003, // bl 0x1030
            0xD503_201F, // nop
            0xD503_201F, // nop
            0xD400_0001, // svc #0
            0xD400_0001, // svc #0 (target of the second TBNZ, not taken here)
            0xD400_0001, // svc #0 (target of the second TBNZ)
        ];
        let mut jit = run_a64_alu_with_optimizations(
            &code,
            OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
            |jit| {
                jit.set_sp(0x9000);
                jit.set_register(19, 0x2000);
            },
        );
        run_a64_until_svc(&mut jit);

        assert_eq!(jit.get_register(0), 0x2000);
        assert_eq!(jit.get_register(1), 0x2488);
        assert_eq!(jit.get_register(2), 0x9040);
        assert_eq!(jit.get_register(3), 1);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn test_a64_adrp_materializes_guest_page_at_high_pc() {
        const PC: u64 = 0x80e6_59b8;
        let code = [
            0xB000_30A1, // adrp x1, 0x8147a000
            0xD400_0001, // svc #0
        ];
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(PC, &code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(PC);
        run_a64_until_svc(&mut jit);

        assert_eq!(jit.get_register(1), 0x8147_a000);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn test_a64_adrp_address_is_preserved_for_memory_callback() {
        const PC: u64 = 0x80e6_59b8;
        let code = [
            0xB000_30A1, // adrp x1, 0x8147a000
            0xF943_D021, // ldr x1, [x1, #0x7a0]
            0xD400_0001, // svc #0
        ];
        let read_address = Arc::new(AtomicU64::new(u64::MAX));
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(
                MockCallbacks::new(PC, &code)
                    .with_memory_read_address_sink(Arc::clone(&read_address)),
            ),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(PC);
        run_a64_until_svc(&mut jit);

        assert_eq!(read_address.load(Ordering::Relaxed), 0x8147_a7a0);
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_frintx_v22_2d_uses_fpcr_and_sets_inexact() {
        let code: &[u32] = &[
            0x6E61_9AF6, // frintx v22.2d, v23.2d
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            j.set_vector(23, 1.25f64.to_bits(), 2.0f64.to_bits());
        });
        assert_eq!(jit.get_vector(22), (1.0f64.to_bits(), 2.0f64.to_bits()));
        assert_ne!(jit.get_fpsr() & (1 << 4), 0, "FRINTX must set FPSR.IXC");
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_fsqrt_v0_4s_computes_each_lane() {
        let code: &[u32] = &[
            0x6EA1_F820, // fsqrt v0.4s, v1.4s
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            j.set_vector(
                1,
                (4.0f32.to_bits() as u64) << 32 | 1.0f32.to_bits() as u64,
                (16.0f32.to_bits() as u64) << 32 | 9.0f32.to_bits() as u64,
            );
        });
        assert_eq!(
            jit.get_vector(0),
            (
                (2.0f32.to_bits() as u64) << 32 | 1.0f32.to_bits() as u64,
                (4.0f32.to_bits() as u64) << 32 | 3.0f32.to_bits() as u64,
            )
        );
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_scalar_frint_fallbacks_count_input_once_and_update_fpsr() {
        let code: &[u32] = &[
            0x1E27_4000, // frintx s0, s0
            0x1E26_4021, // frinta s1, s1
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            j.set_vector(0, 1.25f32.to_bits() as u64, u64::MAX);
            j.set_vector(1, 2.5f32.to_bits() as u64, u64::MAX);
        });
        assert_eq!(jit.get_vector(0), (1.0f32.to_bits() as u64, 0));
        assert_eq!(jit.get_vector(1), (3.0f32.to_bits() as u64, 0));
        assert_ne!(jit.get_fpsr() & (1 << 4), 0, "FRINTX must set FPSR.IXC");
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_scalar_frintx_d_followed_by_fcvtzs_w_preserves_integer() {
        let code: &[u32] = &[
            0x1E67_4000, // frintx d0, d0
            0x1E78_0013, // fcvtzs w19, d0
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            j.set_vector(0, 7.0f64.to_bits(), u64::MAX);
        });
        assert_eq!(jit.get_register(19), 7);
        assert_eq!(jit.get_vector(0), (7.0f64.to_bits(), 0));
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_unsigned_fp_to_fixed_fallbacks_count_input_once() {
        let code: &[u32] = &[
            0x1E59_FC00, // fcvtzu w0, d0, #1
            0x9E19_FC21, // fcvtzu x1, s1, #1
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |j| {
            j.set_vector(0, 1.5f64.to_bits(), u64::MAX);
            j.set_vector(1, 2.25f32.to_bits() as u64, u64::MAX);
        });
        assert_eq!(jit.get_register(0), 3);
        assert_eq!(jit.get_register(1), 4);
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_fmov_fmul_fmadd_fmla_sequence_preserves_lanes() {
        let code: &[u32] = &[
            0x0F03_F41C, // fmov v28.2s, #0.5
            0x2E3C_DFBD, // fmul v29.2s, v29.2s, v28.2s
            0x1F1C_7BFE, // fmadd s30, s31, s28, s30
            0x0E3C_CC1D, // fmla v29.2s, v0.2s, v28.2s
            0xD400_0001, // svc #0
        ];
        let pack_2s =
            |lane0: f32, lane1: f32| (lane1.to_bits() as u64) << 32 | lane0.to_bits() as u64;
        let jit = run_a64_alu(code, |j| {
            j.set_vector(0, pack_2s(6.0, 8.0), u64::MAX);
            j.set_vector(29, pack_2s(2.0, 4.0), u64::MAX);
            j.set_vector(30, 10.0f32.to_bits() as u64, u64::MAX);
            j.set_vector(31, 12.0f32.to_bits() as u64, u64::MAX);
        });

        assert_eq!(jit.get_vector(28), (pack_2s(0.5, 0.5), 0));
        assert_eq!(jit.get_vector(29), (pack_2s(4.0, 6.0), 0));
        assert_eq!(jit.get_vector(30), (16.0f32.to_bits() as u64, 0));
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_rgb565_conversion_keeps_primary_channels_separate() {
        let code: &[u32] = &[
            0x52A8_4F80, // mov w0, #0x427c0000
            0x1E2C_101F, // fmov s31, #0.5
            0x1E27_F01D, // fmov s29, #31.0
            0x5280_07E4, // mov w4, #63
            0x5280_03E3, // mov w3, #31
            0x1E27_001E, // fmov s30, w0
            0x1F1D_7C42, // fmadd s2, s2, s29, s31
            0x1F1D_7C00, // fmadd s0, s0, s29, s31
            0x1F1E_7C3F, // fmadd s31, s1, s30, s31
            0x1E38_0042, // fcvtzs w2, s2
            0x1E38_0000, // fcvtzs w0, s0
            0x1E38_03E1, // fcvtzs w1, s31
            0x6B04_003F, // cmp w1, w4
            0x1A84_D021, // csel w1, w1, w4, le
            0x0AA1_7C21, // bic w1, w1, w1, asr #31
            0x6B03_005F, // cmp w2, w3
            0x1A83_D042, // csel w2, w2, w3, le
            0x0AA2_7C42, // bic w2, w2, w2, asr #31
            0x6B03_001F, // cmp w0, w3
            0x1A83_D000, // csel w0, w0, w3, le
            0x0AA0_7C00, // bic w0, w0, w0, asr #31
            0x2A01_1441, // orr w1, w2, w1, lsl #5
            0x2A00_2C20, // orr w0, w1, w0, lsl #11
            0xD400_0001, // svc #0
        ];
        let run = |r: f32, g: f32, b: f32| {
            run_a64_alu(code, |j| {
                j.set_vector(0, r.to_bits() as u64, u64::MAX);
                j.set_vector(1, g.to_bits() as u64, u64::MAX);
                j.set_vector(2, b.to_bits() as u64, u64::MAX);
            })
            .get_register(0) as u16
        };

        assert_eq!(run(1.0, 0.0, 0.0), 0xF800);
        assert_eq!(run(0.0, 1.0, 0.0), 0x07E0);
        assert_eq!(run(0.0, 0.0, 1.0), 0x001F);
        assert_eq!(run(1.0, 1.0, 1.0), 0xFFFF);
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_ext_8b_extracts_across_64_bit_operands() {
        let code: &[u32] = &[
            0x2E01_1883, // ext v3.8b, v4.8b, v1.8b, #3
            0xD400_0001, // svc #0
        ];
        let jit = run_a64_alu(code, |jit| {
            jit.set_vector(4, u64::from_le_bytes([0, 1, 2, 3, 4, 5, 6, 7]), u64::MAX);
            jit.set_vector(
                1,
                u64::from_le_bytes([8, 9, 10, 11, 12, 13, 14, 15]),
                u64::MAX,
            );
        });

        assert_eq!(
            jit.get_vector(3),
            (u64::from_le_bytes([3, 4, 5, 6, 7, 8, 9, 10]), 0)
        );
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_ldr_b_then_ucvtf_zeros_stale_vector_bits() {
        const CODE_ADDRESS: usize = 0x1000;
        const BYTE_ADDRESS: usize = 0x8000;
        let code: &[u32] = &[
            0x3D40_026D, // ldr b13, [x19]
            0x7E21_D9AD, // ucvtf s13, s13
            0xD400_0001, // svc #0
        ];
        let mut memory = vec![0u8; 0x10000];
        for (index, word) in code.iter().enumerate() {
            let offset = CODE_ADDRESS + index * 4;
            memory[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        }
        memory[BYTE_ADDRESS] = 127;

        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::from_memory(0, memory)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).expect("A64 JIT");
        jit.set_pc(CODE_ADDRESS as u64);
        jit.set_sp(0xF000);
        jit.set_register(19, BYTE_ADDRESS as u64);
        jit.set_vector(13, u64::MAX, u64::MAX);
        let halt = run_a64_until_svc(&mut jit);

        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(jit.get_vector(13), (127.0f32.to_bits() as u64, 0));
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_principal_component_iteration_matches_diagonal_matrix() {
        const CODE_ADDRESS: usize = 0x1000;
        const MATRIX_ADDRESS: usize = 0x8000;
        let code: &[u32] = &[
            0x1E2E_1000, // fmov s0, #1.0
            0x2D40_781F, // ldp s31, s30, [x0]
            0xD100_83FF, // sub sp, sp, #0x20
            0x5280_0101, // mov w1, #8
            0x2D41_701D, // ldp s29, s28, [x0, #8]
            0x2D42_681B, // ldp s27, s26, [x0, #16]
            0x1E20_4001, // fmov s1, s0
            0x1E20_4002, // fmov s2, s0
            0x1E20_4019, // fmov s25, s0
            0xD503_201F, // nop
            0x1E21_0B98, // fmul s24, s28, s1
            0x1E21_0B63, // fmul s3, s27, s1
            0x1E21_0BD6, // fmul s22, s30, s1
            0x1F00_63D8, // fmadd s24, s30, s0, s24
            0x1F00_0FA3, // fmadd s3, s29, s0, s3
            0x1F00_5BF6, // fmadd s22, s31, s0, s22
            0x1F02_6378, // fmadd s24, s27, s2, s24
            0x1F02_0F43, // fmadd s3, s26, s2, s3
            0x1F02_5BB6, // fmadd s22, s29, s2, s22
            0x1E23_2310, // fcmpe s24, s3
            0x1E38_4C77, // fcsel s23, s3, s24, mi
            0x1E36_22F0, // fcmpe s23, s22
            0x1E36_CEF7, // fcsel s23, s23, s22, gt
            0x7100_0421, // subs w1, w1, #1
            0x1E37_1B37, // fdiv s23, s25, s23
            0x1E36_0AE0, // fmul s0, s23, s22
            0x1E38_0AE1, // fmul s1, s23, s24
            0x1E23_0AE2, // fmul s2, s23, s3
            0x54FF_FDC1, // b.ne loop
            0x9100_83FF, // add sp, sp, #0x20
            0xD400_0001, // svc #0
        ];

        let mut memory = vec![0u8; 0x10000];
        for (index, word) in code.iter().enumerate() {
            let offset = CODE_ADDRESS + index * 4;
            memory[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        }
        let matrix = [1.0f32, 0.0, 0.0, 2.0, 0.0, 4.0];
        for (index, value) in matrix.iter().enumerate() {
            let offset = MATRIX_ADDRESS + index * 4;
            memory[offset..offset + 4].copy_from_slice(&value.to_bits().to_le_bytes());
        }

        let memory = Arc::new(Mutex::new(memory));
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::from_shared_memory(0, Arc::clone(&memory))),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).expect("A64 JIT");
        jit.set_pc(CODE_ADDRESS as u64);
        jit.set_sp(0xF000);
        jit.set_register(0, MATRIX_ADDRESS as u64);
        let halt = run_a64_until_svc(&mut jit);

        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(jit.get_vector(0), ((1.0f32 / 65536.0).to_bits() as u64, 0));
        assert_eq!(jit.get_vector(1), ((1.0f32 / 256.0).to_bits() as u64, 0));
        assert_eq!(jit.get_vector(2), (1.0f32.to_bits() as u64, 0));
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_weighted_covariance_matches_symmetric_points() {
        const CODE_ADDRESS: usize = 0x1000;
        const POINTS_ADDRESS: usize = 0x8000;
        const WEIGHTS_ADDRESS: usize = 0x8100;
        const RESULT_ADDRESS: usize = 0x8200;
        let code: &[u32] = &[
            0x7100_001F, // cmp w0, #0
            0x5400_06ED, // b.le empty
            0x0F00_041F, // movi v31.2s, #0
            0xAA01_03E3, // mov x3, x1
            0xAA02_03E4, // mov x4, x2
            0x8B20_4840, // add x0, x2, w0, uxtw #2
            0x1E20_43FE, // fmov s30, s31
            0x1E20_43FD, // fmov s29, s31
            0x1E20_43E6, // fmov s6, s31
            0xD503_201F, // nop
            0xBC40_4485, // ldr s5, [x4], #4
            0xBD40_0863, // ldr s3, [x3, #8]
            0x2CC1_9062, // ldp s2, s4, [x3], #12
            0xEB04_001F, // cmp x0, x4
            0x1E25_28C6, // fadd s6, s6, s5
            0x1F03_78BE, // fmadd s30, s5, s3, s30
            0x1F04_7CBF, // fmadd s31, s5, s4, s31
            0x1F02_74BD, // fmadd s29, s5, s2, s29
            0x54FF_FF01, // b.ne centroid_loop
            0x0F01_6681, // movi v1.2s, #0x34, lsl #24
            0x1E21_20D0, // fcmpe s6, s1
            0x5400_004C, // b.gt normalize
            0x1400_0006, // b covariance_init
            0x1E2E_1000, // fmov s0, #1.0
            0x1E26_1800, // fdiv s0, s0, s6
            0x1E20_0BBD, // fmul s29, s29, s0
            0x1E20_0BFF, // fmul s31, s31, s0
            0x1E20_0BDE, // fmul s30, s30, s0
            0x0F00_0415, // movi v21.2s, #0
            0x1E20_42B4, // fmov s20, s21
            0x1E20_42B3, // fmov s19, s21
            0x1E20_42B2, // fmov s18, s21
            0x1E20_42B1, // fmov s17, s21
            0x1E20_42BC, // fmov s28, s21
            0xBD40_0839, // ldr s25, [x1, #8]
            0x2D40_683B, // ldp s27, s26, [x1]
            0x9100_3021, // add x1, x1, #12
            0xBC40_4458, // ldr s24, [x2], #4
            0x1E3E_3B39, // fsub s25, s25, s30
            0x1E3D_3B7B, // fsub s27, s27, s29
            0x1E3F_3B5A, // fsub s26, s26, s31
            0xEB02_001F, // cmp x0, x2
            0x1E39_0B16, // fmul s22, s24, s25
            0x1E3A_0B17, // fmul s23, s24, s26
            0x1E3B_0B18, // fmul s24, s24, s27
            0x1F1B_4AD2, // fmadd s18, s22, s27, s18
            0x1F1A_52D4, // fmadd s20, s22, s26, s20
            0x1F19_56D5, // fmadd s21, s22, s25, s21
            0x1F1A_4EF3, // fmadd s19, s23, s26, s19
            0x1F1B_46F1, // fmadd s17, s23, s27, s17
            0x1F1B_731C, // fmadd s28, s24, s27, s28
            0x54FF_FDE1, // b.ne covariance_loop
            0x2D00_451C, // stp s28, s17, [x8]
            0x2D01_4D12, // stp s18, s19, [x8, #8]
            0x2D02_5514, // stp s20, s21, [x8, #16]
            0xD400_0001, // svc #0
            0xA900_7D1F, // stp xzr, xzr, [x8]
            0xF900_091F, // str xzr, [x8, #16]
            0xD400_0001, // svc #0
        ];

        let mut memory = vec![0u8; 0x10000];
        for (index, word) in code.iter().enumerate() {
            let offset = CODE_ADDRESS + index * 4;
            memory[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        }
        let points = [1.0f32, 2.0, 3.0, 3.0, 4.0, 5.0];
        for (index, value) in points.iter().enumerate() {
            let offset = POINTS_ADDRESS + index * 4;
            memory[offset..offset + 4].copy_from_slice(&value.to_bits().to_le_bytes());
        }
        for (index, value) in [1.0f32, 1.0].iter().enumerate() {
            let offset = WEIGHTS_ADDRESS + index * 4;
            memory[offset..offset + 4].copy_from_slice(&value.to_bits().to_le_bytes());
        }

        let memory = Arc::new(Mutex::new(memory));
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::from_shared_memory(0, Arc::clone(&memory))),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).expect("A64 JIT");
        jit.set_pc(CODE_ADDRESS as u64);
        jit.set_sp(0xF000);
        jit.set_register(0, 2);
        jit.set_register(1, POINTS_ADDRESS as u64);
        jit.set_register(2, WEIGHTS_ADDRESS as u64);
        jit.set_register(8, RESULT_ADDRESS as u64);
        let halt = run_a64_until_svc(&mut jit);

        assert!(halt.contains(HaltReason::SVC));
        let memory = memory.lock().expect("mock memory poisoned");
        for index in 0..6 {
            let offset = RESULT_ADDRESS + index * 4;
            let bits = u32::from_le_bytes(
                memory[offset..offset + 4]
                    .try_into()
                    .expect("covariance result"),
            );
            assert_eq!(bits, 2.0f32.to_bits(), "covariance[{index}]");
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a32_vmsr_vmrs_fpscr_uses_host_abi_registers() {
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(
                0x1000,
                &[
                    0xEEE1_0A10, // VMSR FPSCR, r0
                    0xEEF1_1A10, // VMRS r1, FPSCR
                    0xEF00_0000, // SVC #0
                ],
            )),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).expect("A32 JIT");
        let fpscr_mode = 0x03C0_0000;
        jit.set_register(0, fpscr_mode);
        jit.set_register(15, 0x1000);
        jit.set_cpsr(0x10);

        let halt = jit.run();

        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(jit.get_register(1) & 0x07C0_0000, fpscr_mode);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_a32_svc_with_cycle_counting_matches_callback_contract() {
        let svc_sink = Arc::new(AtomicU32::new(u32::MAX));
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::with_svc_sink(
                0x1000,
                &[0xEF00_0026], // SVC #0x26
                svc_sink.clone(),
            )),
            enable_cycle_counting: true,
            code_cache_size: 4 * 1024 * 1024,
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A32Jit::new(config).expect("A32 JIT");
        jit.set_register(15, 0x1000);
        jit.set_cpsr(0x10);

        let halt = jit.run();

        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(svc_sink.load(Ordering::Relaxed), 0x26);
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_a64_array_allocation_size_sequence() {
        let code: &[u32] = &[
            0x5280_2608, // mov w8, #0x130
            0x9B28_7EA9, // smull x9, w21, w8
            0x9BC8_7E68, // umulh x8, x19, x8
            0xEB08_03FF, // cmp xzr, x8
            0xB27D_0128, // orr x8, x9, #8
            0xDA9F_0100, // csinv x0, x8, xzr, eq
            0xD400_0001, // svc #0
        ];

        for count in [1_u64, 2, 0x218] {
            let jit = run_a64_alu(code, |j| {
                j.set_register(19, count);
                j.set_register(21, count);
            });
            assert_eq!(jit.get_register(0), count * 0x130 | 8, "count={count}");
        }
    }

    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    #[test]
    fn test_a64_ordered_fastmem_write_preserves_reused_value() {
        let code: &[u32] = &[
            0x9100_52C8, // add x8, x22, #0x14
            0x889F_FE75, // stlr w21, [x19]
            0x9340_7EB3, // sxtw x19, w21
            0x889F_FD15, // stlr w21, [x8]
            0x5280_2608, // mov w8, #0x130
            0x9B28_7EA9, // smull x9, w21, w8
            0x9BC8_7E68, // umulh x8, x19, x8
            0xEB08_03FF, // cmp xzr, x8
            0xB27D_0128, // orr x8, x9, #8
            0xDA9F_0100, // csinv x0, x8, xzr, eq
            0xD400_0001, // svc #0
        ];
        let mapping = TestFastmemMapping::new(0x10_000);
        mapping.map_u32(0x2000, 0);
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(MockCallbacks::new(0x1000, code)),
            enable_cycle_counting: false,
            code_cache_size: 4 * 1024 * 1024,
            optimizations: OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
            unsafe_optimizations: false,
            global_monitor: None,
            fastmem_pointer: Some(mapping.ptr.cast()),
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
            memory: crate::backend::x64::emit_context::MemoryEmitConfig::default(),
        };
        let mut jit = A64Jit::new(config).unwrap();
        jit.set_pc(0x1000);
        jit.set_register(19, 0x2100);
        jit.set_register(21, 0x60);
        jit.set_register(22, 0x2200);

        let halt = jit.run();

        assert!(halt.contains(HaltReason::SVC));
        assert_eq!(jit.get_register(0), 0x7208);
        drop(jit);
        drop(mapping);
    }
}
