// SPDX-FileCopyrightText: Copyright 2020 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/arm/dynarmic/arm_dynarmic_32.h and arm_dynarmic_32.cpp
//! ARM32 dynarmic backend.

use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::arm::arm_interface::{
    matching_watchpoint, Architecture, ArmInterface, ArmInterfaceBase, DebugWatchpoint,
    DebugWatchpointType, HaltReason, KProcess, KThread, SharedWatchpointArray, ThreadContext,
    WatchpointArray,
};
use crate::hle::kernel::k_process::SharedProcessMemory;
use crate::memory::memory::Memory;
use common::page_table::PageInfo;
use common::settings_enums::CpuAccuracy;

use rdynarmic::interface::a32::config::{
    empty_coprocessors, Exception as A32Exception, UserCallbacks as A32UserCallbacks,
    UserConfig as A32UserConfig,
};
use rdynarmic::interface::optimization_flags::OptimizationFlag;

use super::dynarmic_cp15::DynarmicCP15;

// Eden indexes `PageEntryData` records (32 bytes); ruzu's split page-table
// storage exposes the contiguous `PageInfo` buffer directly. Keep the same
// log2-stride contract while deriving it from the actual Rust entry layout.
const PAGE_TABLE_LOG2_STRIDE: usize = std::mem::size_of::<PageInfo>().trailing_zeros() as usize;
const _: () = assert!(
    1usize << PAGE_TABLE_LOG2_STRIDE == std::mem::size_of::<PageInfo>(),
    "PageInfo size must be a power of two"
);

static A32_TRACE_AFTER_WATCH_ARMED: AtomicBool = AtomicBool::new(false);

/// Debug hook used to start the bounded A32 single-step tracer from a precise
/// HLE observation point (for example immediately after a guest sleep).
pub fn arm_trace_after_watch() {
    A32_TRACE_AFTER_WATCH_ARMED.store(true, Ordering::Relaxed);
}

/// Translate rdynarmic's HaltReason to core's HaltReason.
///
/// Same mapping as in arm_dynarmic_64.rs.
fn translate_halt_reason(hr: rdynarmic::halt_reason::HaltReason) -> HaltReason {
    let mut result = HaltReason::empty();

    if hr.contains(rdynarmic::halt_reason::HaltReason::STEP) {
        result |= HaltReason::STEP_THREAD;
    }
    if hr.contains(rdynarmic::halt_reason::HaltReason::MEMORY_ABORT) {
        result |= HaltReason::DATA_ABORT;
    }
    if hr.contains(rdynarmic::halt_reason::HaltReason::SVC) {
        result |= HaltReason::SUPERVISOR_CALL;
    }
    if hr.contains(rdynarmic::halt_reason::HaltReason::BREAKPOINT) {
        result |= HaltReason::INSTRUCTION_BREAKPOINT;
    }
    if hr.contains(rdynarmic::halt_reason::HaltReason::EXCEPTION_RAISED) {
        result |= HaltReason::PREFETCH_ABORT;
    }
    if hr.contains(rdynarmic::halt_reason::HaltReason::EXTERNAL_HALT) {
        result |= HaltReason::BREAK_LOOP;
    }

    result
}

fn optimization_flags_from_mask(mask: u32) -> OptimizationFlag {
    let mut flags = OptimizationFlag::NO_OPTIMIZATIONS;

    if mask & OptimizationFlag::BLOCK_LINKING.bits() != 0 {
        flags |= OptimizationFlag::BLOCK_LINKING;
    }
    if mask & OptimizationFlag::RETURN_STACK_BUFFER.bits() != 0 {
        flags |= OptimizationFlag::RETURN_STACK_BUFFER;
    }
    if mask & OptimizationFlag::FAST_DISPATCH.bits() != 0 {
        flags |= OptimizationFlag::FAST_DISPATCH;
    }
    if mask & OptimizationFlag::GET_SET_ELIMINATION.bits() != 0 {
        flags |= OptimizationFlag::GET_SET_ELIMINATION;
    }
    if mask & OptimizationFlag::CONST_PROP.bits() != 0 {
        flags |= OptimizationFlag::CONST_PROP;
    }
    if mask & OptimizationFlag::MISC_IR_OPT.bits() != 0 {
        flags |= OptimizationFlag::MISC_IR_OPT;
    }
    if mask & OptimizationFlag::UNSAFE_UNFUSE_FMA.bits() != 0 {
        flags |= OptimizationFlag::UNSAFE_UNFUSE_FMA;
    }
    if mask & OptimizationFlag::UNSAFE_REDUCED_ERROR_FP.bits() != 0 {
        flags |= OptimizationFlag::UNSAFE_REDUCED_ERROR_FP;
    }
    if mask & OptimizationFlag::UNSAFE_INACCURATE_NAN.bits() != 0 {
        flags |= OptimizationFlag::UNSAFE_INACCURATE_NAN;
    }
    if mask & OptimizationFlag::UNSAFE_IGNORE_STANDARD_FPCR_VALUE.bits() != 0 {
        flags |= OptimizationFlag::UNSAFE_IGNORE_STANDARD_FPCR_VALUE;
    }
    if mask & OptimizationFlag::UNSAFE_IGNORE_GLOBAL_MONITOR.bits() != 0 {
        flags |= OptimizationFlag::UNSAFE_IGNORE_GLOBAL_MONITOR;
    }

    flags
}

fn upstream_optimization_config_from_settings(
    settings: &common::settings::Values,
) -> (OptimizationFlag, bool) {
    if *settings.cpu_debug_mode.get_value() {
        let mut flags = optimization_flags_from_mask(0x3F);
        if !*settings.cpuopt_block_linking.get_value() {
            flags = flags & !OptimizationFlag::BLOCK_LINKING;
        }
        if !*settings.cpuopt_return_stack_buffer.get_value() {
            flags = flags & !OptimizationFlag::RETURN_STACK_BUFFER;
        }
        if !*settings.cpuopt_fast_dispatcher.get_value() {
            flags = flags & !OptimizationFlag::FAST_DISPATCH;
        }
        if !*settings.cpuopt_context_elimination.get_value() {
            flags = flags & !OptimizationFlag::GET_SET_ELIMINATION;
        }
        if !*settings.cpuopt_const_prop.get_value() {
            flags = flags & !OptimizationFlag::CONST_PROP;
        }
        if !*settings.cpuopt_misc_ir.get_value() {
            flags = flags & !OptimizationFlag::MISC_IR_OPT;
        }
        return (flags, false);
    }

    let mut flags = optimization_flags_from_mask(0x3F);
    let mut unsafe_optimizations = false;

    match *settings.cpu_accuracy.get_value() {
        CpuAccuracy::Unsafe => {
            unsafe_optimizations = true;
            if *settings.cpuopt_unsafe_unfuse_fma.get_value() {
                flags |= OptimizationFlag::UNSAFE_UNFUSE_FMA;
            }
            if *settings.cpuopt_unsafe_reduce_fp_error.get_value() {
                flags |= OptimizationFlag::UNSAFE_REDUCED_ERROR_FP;
            }
            if *settings.cpuopt_unsafe_ignore_standard_fpcr.get_value() {
                flags |= OptimizationFlag::UNSAFE_IGNORE_STANDARD_FPCR_VALUE;
            }
            if *settings.cpuopt_unsafe_inaccurate_nan.get_value() {
                flags |= OptimizationFlag::UNSAFE_INACCURATE_NAN;
            }
            if *settings.cpuopt_unsafe_ignore_global_monitor.get_value() {
                flags |= OptimizationFlag::UNSAFE_IGNORE_GLOBAL_MONITOR;
            }
        }
        CpuAccuracy::Auto => {
            unsafe_optimizations = true;
            flags |= OptimizationFlag::UNSAFE_UNFUSE_FMA;
            flags |= OptimizationFlag::UNSAFE_IGNORE_STANDARD_FPCR_VALUE;
            flags |= OptimizationFlag::UNSAFE_INACCURATE_NAN;
        }
        CpuAccuracy::Paranoid => {
            flags = OptimizationFlag::NO_OPTIMIZATIONS;
            unsafe_optimizations = false;
        }
        CpuAccuracy::Accurate => {}
    }

    (flags, unsafe_optimizations)
}

fn parse_trace_hex_env(name: &str) -> Option<u32> {
    let value = std::env::var(name).ok()?;
    let trimmed = value.trim();
    let trimmed = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    u32::from_str_radix(trimmed, 16).ok()
}

fn parse_trace_u32_env(name: &str) -> Option<u32> {
    std::env::var(name).ok()?.trim().parse().ok()
}

/// JIT callbacks for ARM32.
///
/// Memory watchpoint helper. Reads `RUZU_WATCH_ADDR` (comma-separated hex u64
/// addresses, optionally suffixed `:size` for range; default size = 8 bytes).
/// On every write that overlaps any watched range, logs PC + value to stderr.
///
/// Example: `RUZU_WATCH_ADDR=0xE88960,0xEF4F28:16,0x41800230:4`.
///
/// Lookup is gated on a `OnceLock` to avoid re-parsing per access.
fn watched_ranges() -> &'static [(u64, u64)] {
    use std::sync::OnceLock;
    static RANGES: OnceLock<Vec<(u64, u64)>> = OnceLock::new();
    RANGES.get_or_init(|| {
        let raw = std::env::var("RUZU_WATCH_ADDR").unwrap_or_default();
        let mut out = Vec::new();
        for tok in raw.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
            let (addr_s, size) = match tok.split_once(':') {
                Some((a, s)) => (a, s.parse::<u64>().unwrap_or(8)),
                None => (tok, 8u64),
            };
            let addr = if let Some(stripped) = addr_s
                .strip_prefix("0x")
                .or_else(|| addr_s.strip_prefix("0X"))
            {
                u64::from_str_radix(stripped, 16).unwrap_or(0)
            } else {
                addr_s.parse::<u64>().unwrap_or(0)
            };
            if addr != 0 {
                out.push((addr, addr.saturating_add(size)));
            }
        }
        out
    })
}

/// Trace targets parsed from `RUZU_TRACE_W_AT_VADDR=0xADDR[,0xADDR2,...]`.
/// Each target matches any write whose [vaddr, vaddr+size) range overlaps the
/// 4-byte word at the target address. On match, logs core/pc/lr + value. This
/// mirrors `RUZU_TRACE_W_AT_VADDR` in `arm_dynarmic_64.rs` but uses the A32
/// PC/LR layout (reg[14]=LR is the u32 BEFORE reg[15]=PC in JitState).
fn trace_write_targets() -> &'static [u64] {
    use std::sync::OnceLock;
    static TARGETS: OnceLock<Vec<u64>> = OnceLock::new();
    TARGETS.get_or_init(|| {
        std::env::var("RUZU_TRACE_W_AT_VADDR")
            .ok()
            .map(|raw| {
                raw.split(',')
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .filter_map(|tok| u64::from_str_radix(tok.trim_start_matches("0x"), 16).ok())
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// Trace values parsed from `RUZU_TRACE_W_VALUE=0xVALUE[,0xVALUE2,...]`.
/// Used with `RUZU_NO_FASTMEM=1` when the interesting guest pointer is known
/// but the destination address is not.
fn trace_write_values() -> &'static [u128] {
    use std::sync::OnceLock;
    static VALUES: OnceLock<Vec<u128>> = OnceLock::new();
    VALUES.get_or_init(|| {
        std::env::var("RUZU_TRACE_W_VALUE")
            .ok()
            .map(|raw| {
                raw.split(',')
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .filter_map(|tok| u128::from_str_radix(tok.trim_start_matches("0x"), 16).ok())
                    .collect()
            })
            .unwrap_or_default()
    })
}

#[inline(always)]
fn maybe_trace_w_at_vaddr(cb: &DynarmicCallbacks32, vaddr: u64, size: u64, value: u128) {
    let targets = trace_write_targets();
    if targets.is_empty() {
        return;
    }
    let end = vaddr.saturating_add(size);
    let hits = targets.iter().any(|t| vaddr <= *t && *t < end);
    if !hits {
        return;
    }
    let pc_ptr = cb.jit_pc_ptr;
    let pc = pc_ptr.map(|p| unsafe { p.read_volatile() }).unwrap_or(0);
    let lr = pc_ptr
        .map(|p| unsafe { p.offset(-1).read_volatile() })
        .unwrap_or(0);
    let core = cb.parent.load(std::sync::atomic::Ordering::Relaxed);
    let core_id = if core.is_null() {
        -1i32
    } else {
        unsafe { (*core).core_index() as i32 }
    };
    let t = crate::hle::kernel::trace_format::elapsed_secs();
    let tid = crate::hle::kernel::kernel::get_current_thread_id_fast().unwrap_or(0);
    if common::trace::is_enabled(common::trace::cat::WATCH_WRITE) {
        common::trace::emit_raw(
            common::trace::cat::WATCH_WRITE,
            &[
                core_id as u32 as u64,
                tid,
                pc as u64,
                lr as u64,
                vaddr,
                size,
                value as u64,
                (value >> 64) as u64,
            ],
        );
    }
    if std::env::var_os("RUZU_TRACE_W_AT_REGS").is_some() {
        eprintln!(
            "[{:>10.6}] [W{}_AT] core={} tid={} pc=0x{:08X} lr=0x{:08X} vaddr=0x{:08X} value=0x{:0width$X}",
            t,
            size * 8,
            core_id,
            tid,
            pc,
            lr,
            vaddr as u32,
            value,
            width = (size as usize) * 2
        );
    }
    if std::env::var_os("RUZU_TRACE_W_AT_REGS").is_some() {
        if let Some(p) = pc_ptr {
            let mut regs = [0u32; 16];
            for (i, reg) in regs.iter_mut().enumerate() {
                *reg = unsafe { p.offset((i as isize) - 15).read_volatile() };
            }
            eprintln!(
                "[{:>10.6}] [W{}_AT_REGS] r0=0x{:08X} r1=0x{:08X} r2=0x{:08X} r3=0x{:08X} r4=0x{:08X} r5=0x{:08X} r6=0x{:08X} r7=0x{:08X} r8=0x{:08X} r9=0x{:08X} r10=0x{:08X} r11=0x{:08X} r12=0x{:08X} sp=0x{:08X} lr=0x{:08X} pc=0x{:08X}",
                t,
                size * 8,
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
                regs[12],
                regs[13],
                regs[14],
                regs[15],
            );
            if std::env::var_os("RUZU_TRACE_W_AT_DUMP_REG_PTRS").is_some() {
                let mem = cb.mem();
                for &(name, addr) in &[
                    ("r0", regs[0]),
                    ("r1", regs[1]),
                    ("r2", regs[2]),
                    ("r3", regs[3]),
                    ("r4", regs[4]),
                    ("r5", regs[5]),
                    ("r6", regs[6]),
                    ("r7", regs[7]),
                    ("r8", regs[8]),
                    ("r10", regs[10]),
                    ("sp", regs[13]),
                ] {
                    if addr < 0x1000 {
                        continue;
                    }
                    let mut words = [0u32; 8];
                    for (i, word) in words.iter_mut().enumerate() {
                        *word = mem.read_32(addr as u64 + (i as u64 * 4));
                    }
                    eprintln!(
                        "[{:>10.6}] [W{}_AT_PTR] {}=0x{:08X}: {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X}",
                        t,
                        size * 8,
                        name,
                        addr,
                        words[0],
                        words[1],
                        words[2],
                        words[3],
                        words[4],
                        words[5],
                        words[6],
                        words[7],
                    );
                }
            }
            if let Ok(raw) = std::env::var("RUZU_TRACE_W_AT_EXTRA_PTRS") {
                let mem = cb.mem();
                for token in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    let Some((name, addr_s)) = token.split_once('=') else {
                        continue;
                    };
                    let addr_s = addr_s
                        .strip_prefix("0x")
                        .or_else(|| addr_s.strip_prefix("0X"))
                        .unwrap_or(addr_s);
                    let Ok(addr) = u32::from_str_radix(addr_s, 16) else {
                        continue;
                    };
                    if addr < 0x1000 {
                        continue;
                    }
                    let mut words = [0u32; 16];
                    for (i, word) in words.iter_mut().enumerate() {
                        *word = mem.read_32(addr as u64 + (i as u64 * 4));
                    }
                    eprintln!(
                        "[{:>10.6}] [W{}_AT_EXTRA] {}=0x{:08X}: {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X}",
                        t,
                        size * 8,
                        name,
                        addr,
                        words[0],
                        words[1],
                        words[2],
                        words[3],
                        words[4],
                        words[5],
                        words[6],
                        words[7],
                        words[8],
                        words[9],
                        words[10],
                        words[11],
                        words[12],
                        words[13],
                        words[14],
                        words[15],
                    );
                }
            }
        }
    }
}

#[inline(always)]
fn watch_write(cb: &DynarmicCallbacks32, vaddr: u64, size: u64, value: u128) {
    maybe_trace_w_at_vaddr(cb, vaddr, size, value);
    let value_targets = trace_write_values();
    if !value_targets.is_empty() && value_targets.iter().any(|target| *target == value) {
        let pc_ptr = cb.jit_pc_ptr;
        let pc = pc_ptr.map(|p| unsafe { p.read_volatile() }).unwrap_or(0);
        let lr = pc_ptr
            .map(|p| unsafe { p.offset(-1).read_volatile() })
            .unwrap_or(0);
        let core = cb.parent.load(std::sync::atomic::Ordering::Relaxed);
        let core_id = if core.is_null() {
            -1i32
        } else {
            unsafe { (*core).core_index() as i32 }
        };
        let tid = crate::hle::kernel::kernel::get_current_thread_id_fast().unwrap_or(0);
        let value_lo = value as u64;
        let value_hi = (value >> 64) as u64;
        common::trace::emit_raw(
            common::trace::cat::WATCH_WRITE,
            &[
                core_id as u32 as u64,
                tid,
                pc as u64,
                lr as u64,
                vaddr,
                size,
                value_lo,
                value_hi,
            ],
        );
    }
    // Same filter semantics as watch_read: either filter alone is enough;
    // when both set, both must match. Lets us log writes from a PC window
    // regardless of address (useful for tracing helper function writes).
    let ranges = watched_ranges();
    let pc_range = watched_pc_range();
    let pc_trace = rdynarmic::jit::PC_TRACE_ACTIVE.load(std::sync::atomic::Ordering::Relaxed);
    let has_addr_filter = !ranges.is_empty();
    let has_pc_filter = pc_range.is_some();
    if !has_addr_filter && !has_pc_filter && !pc_trace {
        return;
    }
    if has_addr_filter {
        let end = vaddr.saturating_add(size);
        let hits = ranges.iter().any(|(s, e)| vaddr < *e && end > *s);
        if !hits && !pc_trace {
            return;
        }
    }
    if std::env::var_os("RUZU_A32_TRACE_AFTER_WATCH").is_some() {
        let value_matches = std::env::var("RUZU_A32_TRACE_AFTER_WATCH_VALUE")
            .ok()
            .and_then(|raw| u128::from_str_radix(raw.trim_start_matches("0x"), 16).ok())
            .map(|expected| expected == value)
            .unwrap_or(true);
        let addr_matches = std::env::var("RUZU_A32_TRACE_AFTER_WATCH_ADDR")
            .ok()
            .and_then(|raw| u64::from_str_radix(raw.trim_start_matches("0x"), 16).ok())
            .map(|expected| vaddr <= expected && expected < vaddr.saturating_add(size))
            .unwrap_or(true);
        let mut pc_matches = true;
        let mut r2_matches = true;
        let mut r7_matches = true;
        if let Some(p) = cb.jit_pc_ptr {
            let pc = unsafe { p.read_volatile() };
            pc_matches = std::env::var("RUZU_A32_TRACE_AFTER_WATCH_PC")
                .ok()
                .and_then(|raw| u32::from_str_radix(raw.trim_start_matches("0x"), 16).ok())
                .map(|expected| expected == pc)
                .unwrap_or(true);
            let r2 = unsafe { p.offset(-13).read_volatile() };
            r2_matches = std::env::var("RUZU_A32_TRACE_AFTER_WATCH_R2")
                .ok()
                .and_then(|raw| u32::from_str_radix(raw.trim_start_matches("0x"), 16).ok())
                .map(|expected| expected == r2)
                .unwrap_or(true);
            let r7 = unsafe { p.offset(-8).read_volatile() };
            r7_matches = std::env::var("RUZU_A32_TRACE_AFTER_WATCH_R7")
                .ok()
                .and_then(|raw| u32::from_str_radix(raw.trim_start_matches("0x"), 16).ok())
                .map(|expected| expected == r7)
                .unwrap_or(true);
        }
        if value_matches && addr_matches && pc_matches && r2_matches && r7_matches {
            A32_TRACE_AFTER_WATCH_ARMED.store(true, Ordering::Relaxed);
            if std::env::var_os("RUZU_A32_TRACE_HALT_AFTER_WATCH").is_some() {
                eprintln!(
                    "[A32TRACE] armed after watch write vaddr=0x{:08X} size={} value=0x{:X}",
                    vaddr as u32, size, value
                );
                cb.halt_execution(rdynarmic::halt_reason::HaltReason::USER_DEFINED2);
            }
        }
    }
    let pc_ptr = cb.jit_pc_ptr;
    let pc = pc_ptr.map(|p| unsafe { p.read_volatile() }).unwrap_or(0);
    if let Some((pc_lo, pc_hi)) = pc_range {
        let pc_u64 = pc as u64;
        if pc_u64 < pc_lo || pc_u64 >= pc_hi {
            if !pc_trace {
                return;
            }
        }
    }
    // reg[14] (LR) sits 1 u32 before reg[15] (PC) in A32JitState's contiguous
    // [u32; 16] array.
    let lr = pc_ptr
        .map(|p| unsafe { p.offset(-1).read_volatile() })
        .unwrap_or(0);
    // Include core_index so we can distinguish writes by different JIT
    // instances (one per physical core). Useful when PC_TRACE_ACTIVE is on
    // to see if a non-main core is writing during the window.
    let core = cb.parent.load(std::sync::atomic::Ordering::Relaxed);
    let core_id = if core.is_null() {
        -1i32
    } else {
        unsafe { (*core).core_index() as i32 }
    };
    let tid = crate::hle::kernel::kernel::get_current_thread_id_fast().unwrap_or(0);
    let value_lo = value as u64;
    let value_hi = (value >> 64) as u64;
    common::trace::emit_raw(
        common::trace::cat::WATCH_WRITE,
        &[
            core_id as u32 as u64,
            tid,
            pc as u64,
            lr as u64,
            vaddr,
            size,
            value_lo,
            value_hi,
        ],
    );
    maybe_dump_code_once(cb);
    maybe_dump_instance_at_pc(cb, pc_ptr, pc);
}

/// `RUZU_TRACE_UNMAPPED_WRITE=1` — annotate every JIT-emitted memory write
/// whose target address is not currently mapped in guest memory with
/// `tid`, guest `PC`, `LR`, and the bad `vaddr`/`size`/`value`. Pairs with
/// the existing `Unmapped Write{N} @ 0x...` log emitted from
/// `Memory::write_raw`; that one only tells us the address, this one tells
/// corruption back to a specific call site in guest code.
#[inline(always)]
fn trace_unmapped_write(cb: &DynarmicCallbacks32, vaddr: u64, size: u64, value: u128) {
    if !common::trace::is_enabled(common::trace::cat::UNMAPPED_WRITE) {
        return;
    }
    static TARGET_ADDR: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    let target_addr = TARGET_ADDR.get_or_init(|| {
        std::env::var("RUZU_TRACE_UNMAPPED_WRITE_ADDR")
            .ok()
            .and_then(|raw| u64::from_str_radix(raw.trim().trim_start_matches("0x"), 16).ok())
    });
    if let Some(target_addr) = target_addr {
        if vaddr != *target_addr {
            return;
        }
    }
    static EMIT_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    static EMIT_LIMIT: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    let emit_limit = EMIT_LIMIT.get_or_init(|| {
        std::env::var("RUZU_TRACE_UNMAPPED_WRITE_LIMIT")
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok())
    });
    let emit_index = EMIT_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let Some(limit) = emit_limit {
        if emit_index >= *limit {
            return;
        }
    }
    // Cheap pre-check: skip the trace path entirely when the address IS
    // mapped — the unmapped-write log only fires on the slow path inside
    // `Memory::write_raw`. We mirror the same validity probe here so the
    // trace only emits on actual unmapped writes.
    let mapped = if let Some(ref cm) = cb.core_memory {
        cm.lock()
            .unwrap()
            .is_valid_virtual_address_range(vaddr, size)
    } else {
        cb.memory
            .read()
            .unwrap()
            .is_valid_range(vaddr, size as usize)
    };
    if mapped {
        return;
    }
    let pc_ptr = cb.jit_pc_ptr;
    let pc = pc_ptr.map(|p| unsafe { p.read_volatile() }).unwrap_or(0);
    let lr = pc_ptr
        .map(|p| unsafe { p.offset(-1).read_volatile() })
        .unwrap_or(0);
    let tid = crate::hle::kernel::kernel::get_current_thread_id_fast().unwrap_or(0);
    let value_lo = value as u64;
    let value_hi = (value >> 64) as u64;
    common::trace::emit_raw(
        common::trace::cat::UNMAPPED_WRITE,
        &[tid, pc as u64, lr as u64, vaddr, size, value_lo, value_hi],
    );
    // Dump full GPRs + the struct backing memory at r6 (which holds the
    // This is the same idea as RUZU_DUMP_INSTANCE_AT_PC but hooked at
    // unmapped-write time so we get the registers AT the faulting
    // instruction. Bounded to 5 hits to keep log compact.
    static DUMP_HITS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = DUMP_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if n >= 5 {
        return;
    }
    let Some(p) = pc_ptr else { return };
    let mut r = [0u32; 16];
    for i in 0..16 {
        let off = (i as isize) - 15;
        r[i] = unsafe { p.offset(off).read_volatile() };
    }
    eprintln!(
        "[UNMAPPED_WRITE_REGS] hit#{} r0=0x{:08X} r1=0x{:08X} r2=0x{:08X} r3=0x{:08X} r4=0x{:08X} r5=0x{:08X} r6=0x{:08X} r7=0x{:08X} r8=0x{:08X} r9=0x{:08X} r10=0x{:08X} r11=0x{:08X} r12=0x{:08X} sp=0x{:08X} lr=0x{:08X}",
        n,
        r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7],
        r[8], r[9], r[10], r[11], r[12], r[13], r[14]
    );
    // Dump first 64 bytes of struct at r6 (which holds count at +0xC and
    let mem = cb.mem();
    if r[6] >= 0x1000 {
        let mut hex = String::new();
        for i in 0..64u64 {
            use std::fmt::Write as _;
            let _ = write!(hex, "{:02x}", mem.read_8(r[6] as u64 + i));
        }
        eprintln!(
            "[UNMAPPED_WRITE_STRUCT] hit#{} r6=0x{:08X} +0..63={}",
            n, r[6], hex
        );
    }
    // Dump stack words to walk LR chain — caller-of-caller etc. Width is
    // 64 words (256 bytes) so it covers the prologue + locals area of a
    // function ~3 frames up (e.g. matrix-init → wrapper → outer caller
    // with `push {r4-r11, lr} + sub sp,sp,#0x4C` = 0x90 bytes from inner
    // sp to outer saved-LR).
    let sp = r[13];
    if sp >= 0x1000 {
        let mut words = String::new();
        for i in 0..64u64 {
            use std::fmt::Write as _;
            let w = mem.read_32(sp as u64 + i * 4);
            let _ = write!(words, "{:08x} ", w);
        }
        eprintln!(
            "[UNMAPPED_WRITE_STACK] hit#{} sp=0x{:08X} +0..255={}",
            n,
            sp,
            words.trim_end()
        );
    }
}

#[inline(always)]
fn watch_read(cb: &DynarmicCallbacks32, vaddr: u64, size: u64, value: u128) {
    let ranges = watched_ranges();
    let pc_range = watched_pc_range();
    // PC_TRACE_ACTIVE gates a full-stream log during the SVC-window set by
    // RUZU_TRACE_PC_WINDOW. When true, every guest memory read is logged
    // (addr, value, pc) without needing an explicit RUZU_WATCH_ADDR.
    let pc_trace = rdynarmic::jit::PC_TRACE_ACTIVE.load(std::sync::atomic::Ordering::Relaxed);
    let has_addr_filter = !ranges.is_empty();
    let has_pc_filter = pc_range.is_some();
    if !has_addr_filter && !has_pc_filter && !pc_trace {
        return;
    }
    if has_addr_filter {
        let end = vaddr.saturating_add(size);
        let hits = ranges.iter().any(|(s, e)| vaddr < *e && end > *s);
        if !hits && !pc_trace {
            return;
        }
    }
    let pc_ptr = cb.jit_pc_ptr;
    let pc = pc_ptr.map(|p| unsafe { p.read_volatile() }).unwrap_or(0);
    if let Some((pc_lo, pc_hi)) = pc_range {
        let pc_u64 = pc as u64;
        if pc_u64 < pc_lo || pc_u64 >= pc_hi {
            if !pc_trace {
                return;
            }
        }
    }
    let lr = pc_ptr
        .map(|p| unsafe { p.offset(-1).read_volatile() })
        .unwrap_or(0);
    let tid = crate::hle::kernel::kernel::get_current_thread_id_fast().unwrap_or(0);
    let core = cb.parent.load(std::sync::atomic::Ordering::Relaxed);
    let core_id: i32 = if core.is_null() {
        -1
    } else {
        unsafe { (*core).core_index() as i32 }
    };
    let value_lo = value as u64;
    let value_hi = (value >> 64) as u64;
    common::trace::emit_raw(
        common::trace::cat::WATCH_READ,
        &[
            core_id as u32 as u64,
            tid,
            pc as u64,
            lr as u64,
            vaddr,
            size,
            value_lo,
            value_hi,
        ],
    );
    maybe_dump_code_once(cb);
    maybe_dump_stack_once(cb, pc_ptr);
    maybe_dump_instance_at_pc(cb, pc_ptr, pc);
}

/// Capture all 16 GPRs when current PC matches a target.
/// `RUZU_DUMP_INSTANCE_AT_PC=0xPC` enables it. Bounded to 200 hits.
/// Reads r0..r15 from the JIT state via pc_ptr.offset(-15..0).
fn maybe_dump_instance_at_pc(cb: &DynarmicCallbacks32, pc_ptr: Option<*const u32>, pc: u32) {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::OnceLock;
    static TARGET: OnceLock<Option<u32>> = OnceLock::new();
    static HITS: AtomicU32 = AtomicU32::new(0);
    let target = *TARGET.get_or_init(|| {
        std::env::var("RUZU_DUMP_INSTANCE_AT_PC")
            .ok()
            .and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok())
    });
    let Some(t) = target else { return };
    if pc != t {
        return;
    }
    let n = HITS.fetch_add(1, Ordering::Relaxed);
    if n >= 200 {
        return;
    }
    let Some(p) = pc_ptr else { return };
    let mut r = [0u32; 16];
    for i in 0..16 {
        let off = (i as isize) - 15;
        r[i] = unsafe { p.offset(off).read_volatile() };
    }
    let mem = cb.mem();
    let star_r0 = if r[0] != 0 {
        mem.read_32(r[0] as u64)
    } else {
        0
    };
    let star_r1 = if r[1] != 0 {
        mem.read_32(r[1] as u64)
    } else {
        0
    };
    let star_r8 = if r[8] != 0 {
        mem.read_32(r[8] as u64)
    } else {
        0
    };
    eprintln!(
        "[INSTANCE] pc=0x{:08X} hit#{} r0=0x{:08X} r1=0x{:08X} r2=0x{:08X} r3=0x{:08X} r4=0x{:08X} r5=0x{:08X} r6=0x{:08X} r7=0x{:08X} r8=0x{:08X} sb=0x{:08X} sl=0x{:08X} fp=0x{:08X} ip=0x{:08X} sp=0x{:08X} lr=0x{:08X} *r0=0x{:08X} *r1=0x{:08X} *r8=0x{:08X}",
        pc, n,
        r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7],
        r[8], r[9], r[10], r[11], r[12], r[13], r[14],
        star_r0, star_r1, star_r8
    );
    // Dump 32 bytes of struct content at r5 (this), r6 (*this/sub-obj),
    // r1 (poll target page) — captures heap state at the moment of the wedge.
    // Also dump 32 stack words starting at sp to walk the call chain.
    if n < 5 {
        for (label, addr) in [("r5", r[5]), ("r6", r[6]), ("r1", r[1])] {
            if addr == 0 || addr < 0x1000 {
                continue;
            }
            let mut hex = String::new();
            for i in 0..32u64 {
                use std::fmt::Write;
                let _ = write!(hex, "{:02x}", mem.read_8(addr as u64 + i));
            }
            eprintln!(
                "[INSTANCE_MEM] hit#{} {}=0x{:08X} bytes={}",
                n, label, addr, hex
            );
        }
        let sp = r[13];
        if sp != 0 {
            let mut words = String::new();
            for i in 0..32u64 {
                use std::fmt::Write;
                let w = mem.read_32(sp as u64 + i * 4);
                let _ = write!(words, "{:08x} ", w);
            }
            eprintln!(
                "[INSTANCE_STACK] hit#{} sp=0x{:08X} words={}",
                n,
                sp,
                words.trim()
            );
        }
    }
}

/// `RUZU_TRACE_UNMAPPED_GUEST_REGS=1` — dump live A32 JIT registers on the
/// first low-address unmapped guest reads. Unlike the generic Memory hook,
/// this reads Dynarmic's live register array at the memory callback boundary.
fn trace_unmapped_guest_read_regs(cb: &DynarmicCallbacks32, vaddr: u64, size: u64) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_UNMAPPED_GUEST_REGS").is_some())
        || vaddr >= 0x1000
    {
        return;
    }
    let mapped = if let Some(ref cm) = cb.core_memory {
        cm.lock()
            .unwrap()
            .is_valid_virtual_address_range(vaddr, size)
    } else {
        cb.memory
            .read()
            .unwrap()
            .is_valid_range(vaddr, size as usize)
    };
    if mapped {
        return;
    }

    use std::sync::atomic::{AtomicU32, Ordering};
    static HITS: AtomicU32 = AtomicU32::new(0);
    let n = HITS.fetch_add(1, Ordering::Relaxed);
    if n >= 32 {
        return;
    }

    let Some(p) = cb.jit_pc_ptr else { return };
    let mut r = [0u32; 16];
    for (i, out) in r.iter_mut().enumerate() {
        *out = unsafe { p.offset((i as isize) - 15).read_volatile() };
    }
    let core = cb.parent.load(Ordering::Relaxed);
    let core_id = if core.is_null() {
        -1
    } else {
        unsafe { (*core).core_index() as i32 }
    };
    let tid = crate::hle::kernel::kernel::get_current_thread_id_fast().unwrap_or(0);
    eprintln!(
        "[UNMAPPED_GUEST_REGS] #{} tid={} core={} pc=0x{:08X} lr=0x{:08X} vaddr=0x{:X} bits={} r0=0x{:08X} r1=0x{:08X} r2=0x{:08X} r3=0x{:08X} r4=0x{:08X} r5=0x{:08X} r6=0x{:08X} r7=0x{:08X} r8=0x{:08X} sb=0x{:08X} sl=0x{:08X} fp=0x{:08X} ip=0x{:08X} sp=0x{:08X}",
        n,
        tid,
        core_id,
        r[15],
        r[14],
        vaddr,
        size * 8,
        r[0],
        r[1],
        r[2],
        r[3],
        r[4],
        r[5],
        r[6],
        r[7],
        r[8],
        r[9],
        r[10],
        r[11],
        r[12],
        r[13],
    );
}

/// `RUZU_TRACE_A32_EXCLUSIVE=1` — bounded diagnostic trace for A32 LDREX/STREX
/// hot loops at the Dynarmic callback boundary.
fn maybe_trace_a32_exclusive(
    cb: &DynarmicCallbacks32,
    op: &str,
    vaddr: u64,
    value: u32,
    expected: Option<u32>,
    ok: Option<bool>,
) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    static COUNT: AtomicU32 = AtomicU32::new(0);

    if !*ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_A32_EXCLUSIVE").is_some()) {
        return;
    }

    let idx = COUNT.fetch_add(1, Ordering::Relaxed);
    if idx >= 512 {
        return;
    }

    let pc_ptr = cb.jit_pc_ptr;
    let pc = pc_ptr.map(|p| unsafe { p.read_volatile() }).unwrap_or(0);
    let lr = pc_ptr
        .map(|p| unsafe { p.offset(-1).read_volatile() })
        .unwrap_or(0);
    let core = cb.parent.load(Ordering::Relaxed);
    let core_id = if core.is_null() {
        usize::MAX
    } else {
        unsafe { (*core).core_index() }
    };
    let expected = expected
        .map(|v| format!("0x{v:08X}"))
        .unwrap_or_else(|| "-".to_string());
    let ok = ok.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string());

    eprintln!(
        "[A32_EXCL] #{idx} core={core_id} op={op} pc=0x{pc:08X} lr=0x{lr:08X} vaddr=0x{vaddr:08X} value=0x{value:08X} expected={expected} ok={ok}"
    );
}

/// One-shot stack dump on the first watch_read hit. Prints 16 32-bit words
/// starting at SP. Useful for identifying the caller chain when LR has been
/// clobbered by intermediate scratch use.
fn maybe_dump_stack_once(cb: &DynarmicCallbacks32, pc_ptr: Option<*const u32>) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static FIRED: AtomicBool = AtomicBool::new(false);
    if std::env::var_os("RUZU_DUMP_STACK").is_none() {
        return;
    }
    let Some(p) = pc_ptr else { return };
    let sp = unsafe { p.offset(-2).read_volatile() };
    if FIRED.swap(true, Ordering::SeqCst) {
        return;
    }
    let mem = cb.mem();
    let mut hex = String::with_capacity(16 * 9);
    for i in 0..16u64 {
        let w = mem.read_32(sp as u64 + i * 4);
        use std::fmt::Write;
        let _ = write!(hex, "{:08x} ", w);
    }
    eprintln!("[STACK_DUMP] sp=0x{:08X} words={}", sp, hex.trim());
}

/// One-shot guest-code dump triggered when watch_read/watch_write fires.
/// `RUZU_DUMP_CODE=0xPC1:LEN1[,0xPC2:LEN2,...]` reads LENn bytes of guest
/// memory starting at PCn and prints them as hex. Retries until any one
/// region's first 16 bytes are non-zero, then dumps ALL configured regions.
fn maybe_dump_code_once(cb: &DynarmicCallbacks32) {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::OnceLock;
    static FIRED: AtomicBool = AtomicBool::new(false);
    static SPECS: OnceLock<Vec<(u64, u64)>> = OnceLock::new();
    let specs = SPECS.get_or_init(|| {
        let raw = std::env::var("RUZU_DUMP_CODE").unwrap_or_default();
        let mut out = Vec::new();
        for token in raw.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
            if let Some((a, l)) = token.split_once(':') {
                if let (Ok(addr), Ok(len)) = (
                    u64::from_str_radix(a.trim_start_matches("0x"), 16),
                    l.parse::<u64>(),
                ) {
                    out.push((addr, len));
                }
            }
        }
        out
    });
    if specs.is_empty() {
        return;
    }
    if FIRED.load(Ordering::Relaxed) {
        return;
    }
    let mem = cb.mem();
    // Wait until at least ANY region's bytes are populated (some pages may
    // be heap fill/zeroed; pick the first non-zero region as the trigger).
    let any_populated = specs
        .iter()
        .any(|&(a, _)| (0..16u64).any(|i| mem.read_8(a + i) != 0));
    if !any_populated {
        return;
    }
    if FIRED.swap(true, Ordering::SeqCst) {
        return;
    }
    for &(addr, len) in specs {
        let mut hex = String::with_capacity(len as usize * 2);
        for i in 0..len {
            use std::fmt::Write;
            let _ = write!(hex, "{:02x}", mem.read_8(addr + i));
        }
        eprintln!("[CODE_DUMP] addr=0x{:08X} len={} bytes={}", addr, len, hex);
    }
    drop(mem);
    maybe_scan_bl(cb);
    maybe_scan_word(cb);
    maybe_scan_movw_movt(cb);
}

/// One-shot trigger that fires the literal / MOVW-MOVT scans the first time
/// it is called (typically from the first guest SVC). Independent of
/// RUZU_DUMP_CODE — only the per-scanner env vars need to be set.
fn maybe_run_one_shot_scans(cb: &DynarmicCallbacks32) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static FIRED: AtomicBool = AtomicBool::new(false);
    if !FIRED.swap(true, Ordering::SeqCst) {
        // First call: run the one-shot scans (binary-time literal/MOVW
        // searches). These don't depend on the target memory being mapped yet.
        maybe_scan_word(cb);
        maybe_scan_movw_movt(cb);
        maybe_scan_thumb2_movw_movt(cb);
        maybe_scan_bl(cb);
        maybe_scan_state_write(cb);
        // Also try the code-region dump now (binary is loaded by first SVC).
        // This fires independently of memory-watch hits, useful for capturing
        // function-entry / caller-area bytes when no watch is active.
        maybe_dump_code_once(cb);
    }
    // Memory dump fires when the target region is mapped (which happens later
    // than first SVC for shared-mem pages). It self-disables after one
    // successful dump, matching the OnceLock pattern of the scanners.
    maybe_dump_mem_after_n_svcs(cb);
}

/// Dump a guest-memory region as soon as it becomes mapped, then disable.
/// `RUZU_DUMP_MEM_AT_FIRST_SVC=0xADDR:LEN[,...]` polls each region on every
/// SVC entry; once `is_valid_virtual_address_range` returns true, the region
/// is dumped and removed from the polling set. This lets us snapshot
/// kernel-shared pages (hid, audio, etc.) at a specific point in boot.
fn maybe_dump_mem_after_n_svcs(cb: &DynarmicCallbacks32) {
    use std::sync::Mutex;
    use std::sync::OnceLock;
    static SPEC: OnceLock<Option<Mutex<Vec<(u64, u64, u64)>>>> = OnceLock::new();
    let spec = SPEC.get_or_init(|| {
        let raw = std::env::var("RUZU_DUMP_MEM_AT_FIRST_SVC").ok()?;
        let mut out = Vec::new();
        // New format: N:0xADDR:LEN where N = SVC count threshold (defer dump
        // until at least N SVCs have entered this hook). Old format
        // (0xADDR:LEN) remains supported with N=0 (dump as soon as mapped).
        for token in raw.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
            let parts: Vec<&str> = token.split(':').collect();
            let (after_n, addr_str, len_str) = match parts.len() {
                3 => (parts[0].parse::<u64>().unwrap_or(0), parts[1], parts[2]),
                2 => (0u64, parts[0], parts[1]),
                _ => continue,
            };
            let Ok(addr) = u64::from_str_radix(addr_str.trim_start_matches("0x"), 16) else {
                continue;
            };
            let len = u64::from_str_radix(len_str.trim_start_matches("0x"), 16)
                .ok()
                .or_else(|| len_str.parse::<u64>().ok());
            if let Some(len) = len {
                out.push((after_n, addr, len));
            }
        }
        Some(Mutex::new(out))
    });
    let Some(spec) = spec else {
        return;
    };
    let mut pending = spec.lock().unwrap();
    if pending.is_empty() {
        return;
    }
    use std::sync::atomic::AtomicU64;
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    let mem = cb.mem();
    pending.retain(|&(after_n, addr, len)| {
        if n < after_n {
            return true;
        }
        if !mem.is_valid_virtual_address_range(addr, len) {
            return true;
        }
        let mut hex = String::with_capacity(len as usize * 2 + (len as usize / 16));
        for i in 0..len {
            use std::fmt::Write;
            let _ = write!(hex, "{:02x}", mem.read_8(addr + i));
            if i % 16 == 15 {
                hex.push(' ');
            }
        }
        eprintln!(
            "[MEM_DUMP] svc_n={} addr=0x{:08X} len=0x{:X} bytes={}",
            n,
            addr,
            len,
            hex.trim()
        );
        false // remove from pending after dump
    });
}

/// Thumb-2 MOVW T3 / MOVT T1 pair scanner. Mirrors `maybe_scan_movw_movt`
/// but matches Thumb-2 encodings — most Switch game code (nnSdk) compiles
/// to Thumb-2, so the ARM-only scanner misses them.
///
/// Encoding (as 32-bit LE word stored as two LE halfwords):
///   MOVW T3: `1111 0 i 10 0100 imm4 | 0 imm3 Rd imm8`
///            hw0 mask 0xFBF0, expected 0xF240
///   MOVT T1: `1111 0 i 10 1100 imm4 | 0 imm3 Rd imm8`
///            hw0 mask 0xFBF0, expected 0xF2C0
///   imm16  = imm4 << 12 | i << 11 | imm3 << 8 | imm8
///
/// `RUZU_FIND_T2_MOVW_MOVT=0xVALUE:0xRANGE_START:0xRANGE_LEN` prints each hit
/// as `[T2_MOVW_HIT] movw_pc=0x... movt_pc=0x... rd=rN value=0x...`.
fn maybe_scan_thumb2_movw_movt(cb: &DynarmicCallbacks32) {
    use std::sync::OnceLock;
    static SPEC: OnceLock<Option<(u32, u64, u64)>> = OnceLock::new();
    let spec = *SPEC.get_or_init(|| {
        let raw = std::env::var("RUZU_FIND_T2_MOVW_MOVT").ok()?;
        let parts: Vec<&str> = raw.split(':').collect();
        if parts.len() != 3 {
            return None;
        }
        let value = u32::from_str_radix(parts[0].trim_start_matches("0x"), 16).ok()?;
        let start = u64::from_str_radix(parts[1].trim_start_matches("0x"), 16).ok()?;
        let len = u64::from_str_radix(parts[2].trim_start_matches("0x"), 16)
            .ok()
            .or_else(|| parts[2].parse::<u64>().ok())?;
        Some((value, start, len))
    });
    let Some((value, start, len)) = spec else {
        return;
    };
    let target_lo = value & 0xFFFF;
    let target_hi = (value >> 16) & 0xFFFF;
    let mem = cb.mem();
    let end = start + len;
    const MAX_DISTANCE_BYTES: u64 = 32;
    const PAGE_SIZE: u64 = 0x1000;
    let read_t2 = |addr: u64| -> u32 {
        let hw0 = (mem.read_8(addr) as u32) | ((mem.read_8(addr + 1) as u32) << 8);
        let hw1 = (mem.read_8(addr + 2) as u32) | ((mem.read_8(addr + 3) as u32) << 8);
        hw0 | (hw1 << 16)
    };
    let extract_imm16 = |insn: u32| -> u32 {
        let hw0 = insn & 0xFFFF;
        let hw1 = (insn >> 16) & 0xFFFF;
        let imm4 = hw0 & 0xF;
        let i = (hw0 >> 10) & 1;
        let imm3 = (hw1 >> 12) & 7;
        let imm8 = hw1 & 0xFF;
        (imm4 << 12) | (i << 11) | (imm3 << 8) | imm8
    };
    let extract_rd = |insn: u32| -> u8 { (((insn >> 16) >> 8) & 0xF) as u8 };
    let mut pc = start;
    let mut hits = 0u32;
    let mut next_page_check = start;
    let mut current_page_valid = false;
    while pc + 4 <= end {
        // Skip unmapped pages without spamming the kernel log.
        if pc >= next_page_check {
            current_page_valid =
                mem.is_valid_virtual_address_range(pc & !(PAGE_SIZE - 1), PAGE_SIZE);
            next_page_check = (pc & !(PAGE_SIZE - 1)) + PAGE_SIZE;
        }
        if !current_page_valid {
            pc = next_page_check;
            continue;
        }
        let insn = read_t2(pc);
        let hw0 = insn & 0xFFFF;
        // MOVW T3: hw0 & 0xFBF0 == 0xF240
        if (hw0 & 0xFBF0) == 0xF240 && extract_imm16(insn) == target_lo {
            let rd = extract_rd(insn);
            // Look ahead for MOVT T1 to same Rd within MAX_DISTANCE_BYTES (Thumb-2
            // step is 2 bytes — 16-bit insns possible, but T2-MOVW/T1-MOVT are 4 bytes).
            let mut q = pc + 4;
            let q_end = (pc + MAX_DISTANCE_BYTES).min(end);
            while q + 4 <= q_end {
                let qi = read_t2(q);
                let qhw0 = qi & 0xFFFF;
                if (qhw0 & 0xFBF0) == 0xF2C0 {
                    let q_rd = extract_rd(qi);
                    if q_rd == rd {
                        if extract_imm16(qi) == target_hi {
                            eprintln!(
                                "[T2_MOVW_HIT] movw_pc=0x{:08X} movt_pc=0x{:08X} rd=r{} value=0x{:08X}",
                                pc, q, rd, value
                            );
                            hits += 1;
                            if hits > 64 {
                                eprintln!("[T2_MOVW_HIT] (more hits suppressed)");
                                return;
                            }
                        }
                        break;
                    }
                }
                q += 2; // Thumb step
            }
        }
        pc += 2; // Thumb step
    }
    eprintln!(
        "[T2_MOVW_HIT] scan done: {} hits for value 0x{:08X} in [0x{:X}..0x{:X}]",
        hits, value, start, end
    );
}

/// Scan guest memory for ARM32 `MOVW Rd, #lo; MOVT Rd, #hi` pairs that
/// compute a target 32-bit immediate. The MOVT must hit the same Rd as a
/// preceding MOVW within `MAX_DISTANCE_INSNS` instructions.
///
/// `RUZU_FIND_MOVW_MOVT=0xVALUE:0xRANGE_START:0xRANGE_LEN` prints each hit
/// as `[MOVW_HIT] movw_pc=0x... movt_pc=0x... rd=N value=0x...`.
fn maybe_scan_movw_movt(cb: &DynarmicCallbacks32) {
    use std::sync::OnceLock;
    static SPEC: OnceLock<Option<(u32, u64, u64)>> = OnceLock::new();
    let spec = *SPEC.get_or_init(|| {
        let raw = std::env::var("RUZU_FIND_MOVW_MOVT").ok()?;
        let parts: Vec<&str> = raw.split(':').collect();
        if parts.len() != 3 {
            return None;
        }
        let value = u32::from_str_radix(parts[0].trim_start_matches("0x"), 16).ok()?;
        let start = u64::from_str_radix(parts[1].trim_start_matches("0x"), 16).ok()?;
        let len = u64::from_str_radix(parts[2].trim_start_matches("0x"), 16)
            .ok()
            .or_else(|| parts[2].parse::<u64>().ok())?;
        Some((value, start, len))
    });
    let Some((value, start, len)) = spec else {
        return;
    };
    let target_lo = (value & 0xFFFF) as u32;
    let target_hi = ((value >> 16) & 0xFFFF) as u32;
    let mem = cb.mem();
    let end = start + len;
    const MAX_DISTANCE_BYTES: u64 = 32; // search up to 8 instructions ahead
    let mut pc = start;
    let mut hits = 0;
    while pc + 4 <= end {
        let insn = (mem.read_8(pc) as u32)
            | ((mem.read_8(pc + 1) as u32) << 8)
            | ((mem.read_8(pc + 2) as u32) << 16)
            | ((mem.read_8(pc + 3) as u32) << 24);
        // ARMv7 MOVW: cccc 0011 0000 imm4 Rd imm12
        // Match cond=any, op=0x03000000 mask 0x0FF00000.
        if (insn & 0x0FF00000) == 0x03000000 {
            let imm4 = (insn >> 16) & 0xF;
            let imm12 = insn & 0xFFF;
            let imm16 = (imm4 << 12) | imm12;
            let rd = ((insn >> 12) & 0xF) as u8;
            if imm16 == target_lo {
                // Look ahead for MOVT to same Rd
                let mut q = pc + 4;
                let q_end = (pc + MAX_DISTANCE_BYTES).min(end);
                while q + 4 <= q_end {
                    let qi = (mem.read_8(q) as u32)
                        | ((mem.read_8(q + 1) as u32) << 8)
                        | ((mem.read_8(q + 2) as u32) << 16)
                        | ((mem.read_8(q + 3) as u32) << 24);
                    // ARMv7 MOVT: cccc 0011 0100 imm4 Rd imm12 — op mask 0x03400000.
                    if (qi & 0x0FF00000) == 0x03400000 {
                        let q_rd = ((qi >> 12) & 0xF) as u8;
                        if q_rd == rd {
                            let q_imm16 = (((qi >> 16) & 0xF) << 12) | (qi & 0xFFF);
                            if q_imm16 == target_hi {
                                eprintln!(
                                    "[MOVW_HIT] movw_pc=0x{:08X} movt_pc=0x{:08X} rd=r{} value=0x{:08X}",
                                    pc, q, rd, value
                                );
                                hits += 1;
                                if hits > 64 {
                                    eprintln!("[MOVW_HIT] (more hits suppressed)");
                                    return;
                                }
                            }
                            break; // first MOVT to same Rd ends the search
                        }
                    }
                    q += 4;
                }
            }
        }
        pc += 4;
    }
    eprintln!(
        "[MOVW_HIT] scan done: {} hits for value 0x{:08X} in [0x{:X}..0x{:X}]",
        hits, value, start, end
    );
}

/// Scan guest memory for a 4-byte LE word equal to a target value.
/// `RUZU_FIND_WORD=0xVALUE:0xRANGE_START:0xRANGE_LEN` prints every hit as
/// `[WORD_HIT] addr=0x... val=0x... ctx=<16 bytes around>`. Used to locate
/// vtable slots / function pointer storage that hold a known address.
fn maybe_scan_word(cb: &DynarmicCallbacks32) {
    use std::sync::OnceLock;
    static SPEC: OnceLock<Option<(u32, u64, u64)>> = OnceLock::new();
    let spec = *SPEC.get_or_init(|| {
        let raw = std::env::var("RUZU_FIND_WORD").ok()?;
        let parts: Vec<&str> = raw.split(':').collect();
        if parts.len() != 3 {
            return None;
        }
        let value = u32::from_str_radix(parts[0].trim_start_matches("0x"), 16).ok()?;
        let start = u64::from_str_radix(parts[1].trim_start_matches("0x"), 16).ok()?;
        let len = u64::from_str_radix(parts[2].trim_start_matches("0x"), 16)
            .ok()
            .or_else(|| parts[2].parse::<u64>().ok())?;
        Some((value, start, len))
    });
    let Some((value, start, len)) = spec else {
        return;
    };
    let mem = cb.mem();
    let end = start + len;
    const PAGE_SIZE: u64 = 0x1000;
    let mut addr = start;
    let mut hits = 0;
    let mut next_page_check = start;
    let mut current_page_valid = false;
    while addr + 4 <= end {
        if addr >= next_page_check {
            current_page_valid =
                mem.is_valid_virtual_address_range(addr & !(PAGE_SIZE - 1), PAGE_SIZE);
            next_page_check = (addr & !(PAGE_SIZE - 1)) + PAGE_SIZE;
        }
        if !current_page_valid {
            addr = next_page_check;
            continue;
        }
        let w = mem.read_32(addr);
        if w == value {
            // Print the 16 bytes around the hit (8 before, 8 after) for context
            let ctx_start = addr.saturating_sub(8);
            let mut ctx = String::with_capacity(48);
            for i in 0..16u64 {
                use std::fmt::Write;
                let _ = write!(ctx, "{:02x}", mem.read_8(ctx_start + i));
                if i == 7 {
                    ctx.push('|');
                }
            }
            eprintln!(
                "[WORD_HIT] addr=0x{:08X} val=0x{:08X} ctx=[{}]",
                addr, value, ctx
            );
            hits += 1;
            if hits > 64 {
                eprintln!("[WORD_HIT] (more hits suppressed)");
                break;
            }
        }
        addr += 4;
    }
    eprintln!(
        "[WORD_HIT] scan done: {} hits in [0x{:X}..0x{:X}]",
        hits, start, end
    );
}

/// Scan for ARM `STR Rt, [Rn, #+IMM]` paired with a recent `MOV Rt, #VAL`.
/// `RUZU_FIND_STATE_WRITE=VAL:OFFSET:0xSTART:0xLEN` (decimal VAL/OFFSET).
/// Walks STR-immediate ARM A1 encodings whose imm12==OFFSET and emits the
/// PC plus the most recent assignment of the source reg, if it can be
/// resolved to an immediate within the prior 4 instructions.
/// Useful for locating state-machine writers, e.g. `state = 2` at +0x60.
fn maybe_scan_state_write(cb: &DynarmicCallbacks32) {
    use std::sync::OnceLock;
    // (val, offset, start, len)
    static SPEC: OnceLock<Option<(u32, u32, u64, u64)>> = OnceLock::new();
    let spec = *SPEC.get_or_init(|| {
        let raw = std::env::var("RUZU_FIND_STATE_WRITE").ok()?;
        let parts: Vec<&str> = raw.split(':').collect();
        if parts.len() != 4 {
            return None;
        }
        let parse = |s: &str| {
            let s = s.trim().trim_start_matches("0x");
            u64::from_str_radix(s, 16)
                .ok()
                .or_else(|| s.parse::<u64>().ok())
        };
        let val = parse(parts[0])? as u32;
        let off = parse(parts[1])? as u32;
        let start = parse(parts[2])?;
        let len = parse(parts[3])?;
        Some((val, off, start, len))
    });
    let Some((val, off, start, len)) = spec else {
        return;
    };
    let mem = cb.mem();
    let end = start + len;
    const PAGE_SIZE: u64 = 0x1000;
    let mut addr = start;
    let mut hits = 0;
    let mut next_page_check = start;
    let mut current_page_valid = false;
    while addr + 4 <= end {
        if addr >= next_page_check {
            current_page_valid =
                mem.is_valid_virtual_address_range(addr & !(PAGE_SIZE - 1), PAGE_SIZE);
            next_page_check = (addr & !(PAGE_SIZE - 1)) + PAGE_SIZE;
        }
        if !current_page_valid {
            addr = next_page_check;
            continue;
        }
        let w = mem.read_32(addr);
        // STR (immediate, A1, P=1, U=1, B=0, W=0, L=0): cond 0101_1000 Rn Rt imm12
        // Match cond=AL or any cond. Mask off cond + Rn + Rt.
        let masked = w & 0x0FF00FFF;
        if masked == 0x05800000 | (off & 0xFFF) {
            // Found STR Rt, [Rn, #+OFF]. Now look back for MOV Rt, #VAL.
            let rt = (w >> 12) & 0xF;
            let rn = (w >> 16) & 0xF;
            let mut src_imm: Option<u32> = None;
            let mut src_pc: u64 = 0;
            for back in 1..=6u64 {
                let prior_pc = addr.checked_sub(back * 4).unwrap_or(0);
                let pw = mem.read_32(prior_pc);
                // MOV (immediate, A1): cond 0011_1010_0000_Rd_imm12 (S=0)
                //                or:   cond 0011_1011_0000_Rd_imm12 (S=1, MOVS)
                // Match cond=AL preferred, with Rd == rt.
                let match_mov = (pw & 0x0FFF_F000) == 0x03A00000 | (rt << 12);
                if match_mov {
                    let imm12 = pw & 0xFFF;
                    let rot = (imm12 >> 8) & 0xF;
                    let imm8 = imm12 & 0xFF;
                    let imm = if rot == 0 {
                        imm8
                    } else {
                        ((imm8 >> (2 * rot)) | (imm8 << (32 - 2 * rot))) & 0xFFFFFFFF
                    };
                    src_imm = Some(imm);
                    src_pc = prior_pc;
                    break;
                }
            }
            // Emit if the preceding MOV matched the target value.
            // If we couldn't find a MOV-imm, still emit the STR — useful when
            // the source comes via memory load.
            let label = if src_imm == Some(val) {
                "[STATE_WRITE]"
            } else if src_imm.is_some() {
                "[STATE_WRITE_OTHER]"
            } else {
                "[STATE_WRITE_NOMOV]"
            };
            // Only emit STATE_WRITE_OTHER when the preceding MOV *was* an immediate
            // (so we don't drown the log when reg was loaded from memory).
            if matches!(label, "[STATE_WRITE]" | "[STATE_WRITE_OTHER]") {
                eprintln!(
                    "{} pc=0x{:08X} str=Rt=r{} Rn=r{} mov_pc=0x{:08X} mov_imm={}",
                    label,
                    addr,
                    rt,
                    rn,
                    src_pc,
                    src_imm
                        .map(|v| format!("{}", v))
                        .unwrap_or_else(|| "?".into())
                );
                hits += 1;
                if hits > 256 {
                    eprintln!("{} (more hits suppressed)", label);
                    break;
                }
            }
        }
        addr += 4;
    }
    eprintln!(
        "[STATE_WRITE] scan done: {} hits in [0x{:X}..0x{:X}] val={} off={}",
        hits, start, end, val, off
    );
}

/// Scan guest memory for ARM BL instructions targeting a specific PC or PC range.
/// `RUZU_FIND_BL=0xTARGET:0xRANGE_START:0xRANGE_LEN` (single target)
/// `RUZU_FIND_BL=0xLO-0xHI:0xRANGE_START:0xRANGE_LEN` (range of targets, inclusive)
/// Scans RANGE_LEN bytes from RANGE_START for any 4-byte ARM BL/B/BLX word
/// whose decoded offset reaches TARGET (or any address in [LO..=HI]).
/// Prints all hits as `[BL_HIT] pc=0x... target=0x...`.
fn maybe_scan_bl(cb: &DynarmicCallbacks32) {
    use std::sync::OnceLock;
    // (target_lo, target_hi inclusive, start, len)
    static SPEC: OnceLock<Option<(u64, u64, u64, u64)>> = OnceLock::new();
    let spec = *SPEC.get_or_init(|| {
        let raw = std::env::var("RUZU_FIND_BL").ok()?;
        let parts: Vec<&str> = raw.split(':').collect();
        if parts.len() != 3 {
            return None;
        }
        let parse_hex = |s: &str| -> Option<u64> {
            u64::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok()
        };
        let (target_lo, target_hi) = if let Some((lo, hi)) = parts[0].split_once('-') {
            (parse_hex(lo)?, parse_hex(hi)?)
        } else {
            let t = parse_hex(parts[0])?;
            (t, t)
        };
        let start = parse_hex(parts[1])?;
        let len = parse_hex(parts[2]).or_else(|| parts[2].trim().parse::<u64>().ok())?;
        Some((target_lo, target_hi, start, len))
    });
    let Some((target_lo, target_hi, start, len)) = spec else {
        return;
    };
    let mem = cb.mem();
    let end = start + len;
    let mut pc = start;
    let mut hits = 0;
    // Cap output at 256 hits when a range is in use; 64 for a single target.
    let cap = if target_lo == target_hi { 64 } else { 256 };
    while pc + 4 <= end {
        let w0 = mem.read_8(pc);
        let w1 = mem.read_8(pc + 1);
        let w2 = mem.read_8(pc + 2);
        let w3 = mem.read_8(pc + 3);
        // ARM BL/B with any condition: high nibble == 1010 (B) or 1011 (BL).
        // ARM BLX(imm) is encoded `1111 101H imm24` (top byte 0xFA or 0xFB).
        let is_bl_b = (w3 & 0x0F) == 0x0A || (w3 & 0x0F) == 0x0B;
        let is_blx = w3 == 0xFA || w3 == 0xFB;
        if is_bl_b || is_blx {
            let imm24 = (w0 as u32) | ((w1 as u32) << 8) | ((w2 as u32) << 16);
            let signed = if imm24 & 0x800000 != 0 {
                imm24 as i32 | (-(0x1_000_000_i32))
            } else {
                imm24 as i32
            };
            let computed_target = if is_blx {
                // BLX(imm) ARM->Thumb: target = (PC+8 + sign_extend(imm24:H:'0')) | 1
                let h = (w3 & 0x01) as i64;
                let off = (signed as i64) * 4 + (h * 2);
                ((pc as i64 + 8 + off) as u64) | 1
            } else {
                (pc as i64 + 8 + (signed as i64) * 4) as u64
            };
            // Strip Thumb bit when matching against ARM-aligned targets.
            let match_target = computed_target & !1u64;
            if match_target >= target_lo && match_target <= target_hi {
                let cond = (w3 >> 4) & 0xF;
                let kind_id: u8 = if is_blx {
                    2
                } else if w3 & 0x01 != 0 {
                    1
                } else {
                    0
                };
                if common::trace::is_enabled(common::trace::cat::BL_HIT) {
                    common::trace::emit_raw(
                        common::trace::cat::BL_HIT,
                        &[pc, computed_target, kind_id as u64, cond as u64],
                    );
                }
                hits += 1;
                if hits > cap {
                    eprintln!("[BL_HIT] (more hits suppressed)");
                    break;
                }
            }
        }
        pc += 4;
    }
    eprintln!(
        "[BL_HIT] scan done: {} hits in [0x{:X}..0x{:X}] target=[0x{:X}..0x{:X}]",
        hits, start, end, target_lo, target_hi
    );
}

/// `RUZU_FASTMEM_TRAP_PAGE=0xADDR[,0xADDR2,…]` — mprotect the 4-KiB host
/// page(s) backing the given guest vaddr(s) in the fastmem arena as
/// `PROT_READ`. JIT-emitted writes to those guest pages then take a
/// SIGSEGV, the backend's exception handler patches the faulting MOV out
/// of the fastmem path on first use, and subsequent stores to that
/// instruction go through the slow `write_8/16/32/64` callback — where
/// `RUZU_TRACE_W_AT_VADDR=…` / WATCH_WRITE diagnostics fire. Used to
/// surface stores that would otherwise be invisible to memory callbacks
/// chain, task #112) without paying the global `RUZU_NO_FASTMEM`
/// slowdown.
///
/// Idempotent across cores: the host page is shared between all JIT
/// instances on the same process, so the mprotect on core 0 covers
/// every core. Subsequent cores log "already trapped" and bail out.
#[cfg(any(unix, windows))]
fn maybe_trap_fastmem_page(fastmem_pointer: Option<*mut u8>, core_index: usize) {
    use std::sync::OnceLock;
    static DONE: OnceLock<()> = OnceLock::new();
    let Some(raw) = std::env::var("RUZU_FASTMEM_TRAP_PAGE").ok() else {
        return;
    };
    if DONE.get().is_some() {
        log::warn!(
            "[FASTMEM_TRAP] core={} skipped — already trapped on an earlier core",
            core_index
        );
        return;
    }
    let Some(fastmem_pointer) = fastmem_pointer else {
        log::warn!(
            "[FASTMEM_TRAP] core={} RUZU_FASTMEM_TRAP_PAGE set but fastmem is disabled (RUZU_NO_FASTMEM?); ignoring",
            core_index
        );
        return;
    };
    if fastmem_pointer.is_null() {
        log::warn!(
            "[FASTMEM_TRAP] core={} fastmem_pointer is null; ignoring",
            core_index
        );
        return;
    }
    let _ = DONE.set(());
    const PAGE_SIZE: usize = 0x1000;
    let mut pages: Vec<usize> = Vec::new();
    for token in raw.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let stripped = token.trim_start_matches("0x").trim_start_matches("0X");
        let Ok(addr) = u64::from_str_radix(stripped, 16) else {
            log::warn!("[FASTMEM_TRAP] cannot parse '{}'", token);
            continue;
        };
        let page = (addr as usize) & !(PAGE_SIZE - 1);
        pages.push(page);
    }
    if pages.is_empty() {
        return;
    }
    // The guest page backing `addr` is often NOT yet allocated when the
    // JIT is constructed (heap setup happens after the first SVCs). Our
    // initial mprotect succeeds against the PROT_NONE-mapped fastmem
    // arena, but as soon as the kernel maps the guest page on first
    // touch it RE-applies PROT_READ|PROT_WRITE — undoing the trap. So we
    // spawn a small re-applier thread that keeps the page PROT_READ for
    // a configurable duration (default ~30 s) and bails out after that.
    let fastmem_addr = fastmem_pointer as usize;
    let duration_ms = std::env::var("RUZU_FASTMEM_TRAP_DURATION_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(30_000);
    let interval_ms = std::env::var("RUZU_FASTMEM_TRAP_INTERVAL_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(50);
    log::warn!(
        "[FASTMEM_TRAP] re-apply loop: {} page(s), every {} ms for {} ms",
        pages.len(),
        interval_ms,
        duration_ms
    );
    std::thread::Builder::new()
        .name("ruzu-fastmem-trap".into())
        .spawn(move || {
            let start = std::time::Instant::now();
            let mut tick: u64 = 0;
            while start.elapsed().as_millis() < duration_ms as u128 {
                for &page in &pages {
                    let host_ptr = (fastmem_addr + page) as *mut libc::c_void;
                    #[cfg(unix)]
                    let ret = unsafe { libc::mprotect(host_ptr, PAGE_SIZE, libc::PROT_READ) };
                    #[cfg(windows)]
                    let ret = unsafe {
                        let mut old_protect = 0;
                        if winapi::um::memoryapi::VirtualProtect(
                            host_ptr.cast(),
                            PAGE_SIZE,
                            winapi::um::winnt::PAGE_READONLY,
                            &mut old_protect,
                        ) != 0
                        {
                            0
                        } else {
                            -1
                        }
                    };
                    // Log the FIRST success per page so we know when the
                    // trap "took" (the page is mapped) and every Nth
                    // success after that. EAGAIN-equivalent quiet path:
                    // ret != 0 just means the page isn't yet mapped, we
                    // try again next tick.
                    if ret == 0 && tick % 100 == 0 {
                        log::warn!(
                            "[FASTMEM_TRAP] re-applied guest=0x{:08X} host=0x{:X} tick={}",
                            page,
                            host_ptr as usize,
                            tick
                        );
                    }
                }
                tick += 1;
                std::thread::sleep(std::time::Duration::from_millis(interval_ms));
            }
            log::warn!(
                "[FASTMEM_TRAP] re-apply loop ended after {} ms",
                duration_ms
            );
        })
        .expect("spawn ruzu-fastmem-trap");
}

/// `RUZU_WATCH_VADDR_POLL=0xADDR[,0xADDR2,…]` — spawn a background thread
/// that reads each 4-byte guest vaddr from the fastmem arena every
/// `RUZU_WATCH_VADDR_POLL_INTERVAL_MS` (default 10) and logs the value
/// whenever it changes. Catches writes regardless of access mechanism
/// (fastmem fast path, callback path, HLE-side direct write, …) without
/// requiring instruction-level instrumentation. Trade-off: misses
/// back-to-back writes that change a value twice within one poll cycle,
/// and only sees the FINAL value of a multi-byte store.
///
/// Idempotent across cores. Uses unsafe pointer reads on the fastmem
/// region — for pages that aren't mapped yet, the read will SIGSEGV;
/// to be safe, the poller catches that and skips (the fastmem region
/// is mapped PROT_NONE for unallocated pages, so reading is the same
/// as writing for fault behaviour).
fn maybe_spawn_vaddr_poller(fastmem_pointer: Option<*mut u8>, core_index: usize) {
    use std::sync::OnceLock;
    static DONE: OnceLock<()> = OnceLock::new();
    let Some(raw) = std::env::var("RUZU_WATCH_VADDR_POLL").ok() else {
        return;
    };
    if DONE.get().is_some() {
        return;
    }
    let Some(fastmem_pointer) = fastmem_pointer.filter(|p| !p.is_null()) else {
        log::warn!(
            "[WATCH_POLL] core={} fastmem disabled; cannot poll",
            core_index
        );
        return;
    };
    let _ = DONE.set(());
    let mut addrs: Vec<u64> = Vec::new();
    for token in raw.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let stripped = token.trim_start_matches("0x").trim_start_matches("0X");
        let Ok(addr) = u64::from_str_radix(stripped, 16) else {
            log::warn!("[WATCH_POLL] cannot parse '{}'", token);
            continue;
        };
        addrs.push(addr);
    }
    if addrs.is_empty() {
        return;
    }
    let interval_ms = std::env::var("RUZU_WATCH_VADDR_POLL_INTERVAL_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(10);
    let fastmem_addr = fastmem_pointer as usize;
    log::warn!(
        "[WATCH_POLL] watching {} vaddr(s) every {} ms",
        addrs.len(),
        interval_ms
    );
    std::thread::Builder::new()
        .name("ruzu-vaddr-poll".into())
        .spawn(move || {
            let mut last_values: Vec<Option<u32>> = vec![None; addrs.len()];
            let start = std::time::Instant::now();
            loop {
                for (i, &addr) in addrs.iter().enumerate() {
                    let host_ptr = (fastmem_addr + addr as usize) as *const u32;
                    // SAFETY: We may SIGSEGV if the page isn't mapped. The
                    // host's default SIGSEGV handler will abort — to be
                    // safer we'd need sigsetjmp; for diagnostic use this
                    // is acceptable. The trap re-applier will keep the
                    // page PROT_READ, so reads are fine.
                    let value = unsafe { std::ptr::read_volatile(host_ptr) };
                    let changed = match last_values[i] {
                        None => true,
                        Some(prev) => prev != value,
                    };
                    if changed {
                        let t = start.elapsed().as_secs_f64();
                        log::warn!(
                            "[WATCH_POLL] t={:8.3}s vaddr=0x{:08X} value=0x{:08X} (was {})",
                            t,
                            addr,
                            value,
                            last_values[i]
                                .map(|v| format!("0x{:08X}", v))
                                .unwrap_or_else(|| "—".to_string())
                        );
                        last_values[i] = Some(value);
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(interval_ms));
            }
        })
        .expect("spawn ruzu-vaddr-poll");
}

/// PC-range filter from `RUZU_WATCH_PC=0xLO-0xHI` (inclusive..exclusive).
/// Returns None when unset; pairs with `watch_read` / `watch_write` to
/// limit log output to guest code within a specific PC window.
fn watched_pc_range() -> Option<(u64, u64)> {
    use std::sync::OnceLock;
    static RANGE: OnceLock<Option<(u64, u64)>> = OnceLock::new();
    *RANGE.get_or_init(|| {
        let raw = std::env::var("RUZU_WATCH_PC").ok()?;
        let (a, b) = raw.split_once('-')?;
        let parse = |s: &str| -> Option<u64> {
            let s = s.trim();
            let stripped = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"));
            match stripped {
                Some(hex) => u64::from_str_radix(hex, 16).ok(),
                None => s.parse::<u64>().ok(),
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

fn maybe_trace_mem_callback_8(cb: &DynarmicCallbacks32, is_write: bool, vaddr: u64, value: u8) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ENABLED.get_or_init(|| std::env::var_os("RUZU_A32_MEM_CALLBACK_STATS").is_some()) {
        return;
    }
    let Some((pc_lo, pc_hi)) = watched_pc_range() else {
        return;
    };
    let Some(pc_ptr) = cb.jit_pc_ptr else {
        return;
    };
    let pc = unsafe { pc_ptr.read_volatile() } as u64;
    if pc < pc_lo || pc >= pc_hi {
        return;
    }

    static READS: AtomicU64 = AtomicU64::new(0);
    static WRITES: AtomicU64 = AtomicU64::new(0);
    let counter = if is_write { &WRITES } else { &READS };
    let count = counter.fetch_add(1, Ordering::Relaxed) + 1;
    if count <= 8 || count.is_power_of_two() {
        let lr = unsafe { pc_ptr.offset(-1).read_volatile() };
        eprintln!(
            "[A32_MEM_CB_8] kind={} count={} pc=0x{:08X} lr=0x{:08X} vaddr=0x{:08X} value=0x{:02X}",
            if is_write { "W" } else { "R" },
            count,
            pc as u32,
            lr,
            vaddr as u32,
            value
        );
    }
}

fn a32_callback_diagnostics_enabled() -> bool {
    static ENV_ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENV_ENABLED.get_or_init(|| {
        std::env::var_os("RUZU_TRACE_A32_EXCLUSIVE").is_some()
            || std::env::var_os("RUZU_TRACE_UNMAPPED_GUEST").is_some()
            || std::env::var_os("RUZU_TRACE_UNMAPPED_GUEST_ALL").is_some()
            || std::env::var_os("RUZU_TRACE_W_AT").is_some()
            || std::env::var_os("RUZU_TRACE_W_AT_REGS").is_some()
            || std::env::var_os("RUZU_WATCH_ADDR").is_some()
            || std::env::var_os("RUZU_WATCH_WRITE").is_some()
            || std::env::var_os("RUZU_WATCH_BLOCK").is_some()
            || std::env::var_os("RUZU_WATCH_PC").is_some()
    }) || rdynarmic::jit::PC_TRACE_ACTIVE.load(std::sync::atomic::Ordering::Relaxed)
        || common::trace::is_enabled(common::trace::cat::WATCH_READ)
        || common::trace::is_enabled(common::trace::cat::WATCH_WRITE)
        || common::trace::is_enabled(common::trace::cat::UNMAPPED_WRITE)
        || common::trace::is_enabled(common::trace::cat::STLEX)
}

/// Corresponds to upstream `DynarmicCallbacks32`.
///
/// Upstream fields: `m_parent`, `m_memory`, `m_process`, `m_debugger_enabled`,
/// `m_check_memory_access`. The SVC and timing state is accessed through `m_parent`;
/// the exclusive monitor and core index are owned by the A32 JIT configuration.
///
/// In Rust, `parent` is a raw pointer set post-construction via `set_parent_ptr()`,
/// matching upstream's reference-based `m_parent`. The pointer is null during JIT
/// construction but is set immediately after. All callback methods are only called
/// by the JIT during `run_thread()`, at which point parent is guaranteed to be set.
struct DynarmicCallbacks32 {
    /// Upstream: `ArmDynarmic32& m_parent`.
    /// Shared atomic pointer set post-construction by the parent ArmDynarmic32.
    /// Uses AtomicPtr so the parent can set it after JIT creation without needing
    /// mutable access to the callbacks (which are consumed by the JIT).
    /// Safety: once set, valid for the lifetime of the parent ArmDynarmic32.
    parent: Arc<AtomicPtr<ArmDynarmic32>>,
    /// Upstream: `Core::Memory::Memory& m_memory`.
    /// None in tests where Memory is not wired.
    core_memory: Option<Arc<std::sync::Mutex<Memory>>>,
    /// Upstream: `Kernel::KProcess* m_process`.
    /// Raw pointer to the owning process, used for LogBacktrace.
    /// Safety: valid for the lifetime of the KProcess that owns this JIT.
    process: *const crate::hle::kernel::k_process::KProcess,
    /// Shared guest memory reference (ProcessMemoryData).
    /// Used as fallback when core_memory is None (tests).
    /// Not in upstream, but needed for Rust fallback path.
    memory: SharedProcessMemory,
    /// Upstream: `const bool m_debugger_enabled`.
    debugger_enabled: bool,
    /// Upstream: `const bool m_check_memory_access`.
    check_memory_access: bool,
    /// Shared view of `ArmInterface::m_watchpoints` used by the moved callback.
    watchpoints: SharedWatchpointArray,
    /// Shared counterpart of `ArmDynarmic32::m_halted_watchpoint`.
    halted_watchpoint: Arc<Mutex<Option<DebugWatchpoint>>>,
    /// Raw pointer to jit_state.reg[15] (PC) for diagnostic logging.
    /// Set after jit creation via `set_pc_ptr()`.
    /// Not in upstream, but needed since we don't have debugger.
    jit_pc_ptr: Option<*const u32>,
}

// Safety: The raw pointers (parent, process, jit_pc_ptr) all point to
// objects that are stable for the JIT's lifetime. The JIT is single-threaded per core.
unsafe impl Send for DynarmicCallbacks32 {}

impl DynarmicCallbacks32 {
    fn new(
        memory: SharedProcessMemory,
        core_memory: Option<Arc<std::sync::Mutex<Memory>>>,
        process: *const crate::hle::kernel::k_process::KProcess,
        parent_ptr: Arc<AtomicPtr<ArmDynarmic32>>,
        debugger_enabled: bool,
        watchpoints: SharedWatchpointArray,
        halted_watchpoint: Arc<Mutex<Option<DebugWatchpoint>>>,
    ) -> Self {
        log::info!(
            "DynarmicCallbacks32: core_memory={}",
            if core_memory.is_some() {
                "wired"
            } else {
                "fallback"
            }
        );
        Self {
            parent: parent_ptr,
            core_memory,
            process,
            memory,
            debugger_enabled,
            check_memory_access: debugger_enabled
                || !*common::settings::values()
                    .cpuopt_ignore_memory_aborts
                    .get_value(),
            watchpoints,
            halted_watchpoint,
            jit_pc_ptr: None,
        }
    }

    /// Get a reference to the parent ArmDynarmic32.
    /// All callback methods are only called by the JIT during run_thread(),
    /// at which point parent is guaranteed to be set by ArmDynarmic32::new().
    ///
    /// Corresponds to upstream's `m_parent` reference.
    fn parent(&self) -> &ArmDynarmic32 {
        let ptr = self.parent.load(Ordering::Acquire);
        debug_assert!(
            !ptr.is_null(),
            "DynarmicCallbacks32::parent() called before parent pointer was set"
        );
        unsafe { &*ptr }
    }

    /// Halt the jit execution with the given reason.
    /// This is the Rust equivalent of upstream's `m_parent.m_jit->HaltExecution(hr)`.
    fn halt_execution(&self, reason: rdynarmic::halt_reason::HaltReason) {
        if let Some(jit) = self.parent().jit.as_ref() {
            jit.halt_execution(reason);
        }
    }

    /// Matches upstream `DynarmicCallbacks32::ReturnException`.
    fn return_exception(&self, pc: u32, reason: rdynarmic::halt_reason::HaltReason) {
        let parent = self.parent();
        let mut ctx = ThreadContext::default();
        parent.get_context(&mut ctx);
        ctx.pc = pc as u64;
        ctx.r[15] = pc as u64;
        *parent.breakpoint_context.lock().unwrap() = ctx;
        self.halt_execution(reason);
    }

    /// Matches upstream `DynarmicCallbacks32::CheckMemoryAccess`.
    ///
    /// Upstream behavior: `m_check_memory_access` is only true when
    /// `debugger_enabled || !cpuopt_ignore_memory_aborts`. The default is
    /// `cpuopt_ignore_memory_aborts = true`, so `m_check_memory_access = false`,
    /// meaning this function returns true immediately without checking.
    ///
    /// Memory access validation is a debugger feature, not used in normal play.
    /// The JIT uses page table fastmem for actual memory protection.
    /// Access `m_memory` — returns a lock guard on the Memory bridge.
    /// Matches upstream's `m_memory` reference (Core::Memory::Memory&).
    /// Panics if core_memory is not wired (only happens in tests).
    fn mem(&self) -> std::sync::MutexGuard<'_, Memory> {
        self.core_memory
            .as_ref()
            .expect("core_memory not wired")
            .lock()
            .unwrap()
    }

    /// Matches upstream `DynarmicCallbacks32::CheckMemoryAccess`.
    /// Default: no check (m_check_memory_access = false).
    fn check_memory_access(&self, addr: u64, size: u64, access_type: DebugWatchpointType) -> bool {
        if !self.check_memory_access {
            return true;
        }

        let valid = if let Some(core_memory) = &self.core_memory {
            core_memory
                .lock()
                .unwrap()
                .is_valid_virtual_address_range(addr, size)
        } else {
            self.memory
                .read()
                .unwrap()
                .is_valid_range(addr, size as usize)
        };
        if !valid {
            log::error!("Stopping execution due to unmapped memory access at {addr:#x}");
            self.halt_execution(rdynarmic::halt_reason::HaltReason::PREFETCH_ABORT);
            return false;
        }

        if !self.debugger_enabled {
            return true;
        }

        if let Some(watchpoint) = matching_watchpoint(&self.watchpoints, addr, size, access_type) {
            *self.halted_watchpoint.lock().unwrap() = Some(watchpoint);
            self.halt_execution(rdynarmic::halt_reason::HaltReason::MEMORY_ABORT);
            return false;
        }

        true
    }
}

impl A32UserCallbacks for DynarmicCallbacks32 {
    fn memory_read_code(&self, vaddr: u32) -> Option<u32> {
        let vaddr = vaddr as u64;
        // Upstream returns nullopt when instruction fetch targets an invalid
        // virtual range. Do not use fastmem here: an invalid guest PC must end
        // translation, not turn into a host SIGSEGV while reading code bytes.
        if let Some(ref cm) = self.core_memory {
            let m = cm.lock().unwrap();
            if m.is_valid_virtual_address_range(vaddr, 4) {
                Some(m.read_32(vaddr))
            } else {
                None
            }
        } else {
            let mem = self.memory.read().unwrap();
            if mem.is_valid_range(vaddr, 4) {
                Some(mem.read_32(vaddr))
            } else {
                None
            }
        }
    }

    fn memory_read_8(&self, vaddr: u32) -> u8 {
        let vaddr = vaddr as u64;
        self.check_memory_access(vaddr, 1, DebugWatchpointType::READ);
        trace_unmapped_guest_read_regs(self, vaddr, 1);
        let value = self.mem().read_8(vaddr);
        maybe_trace_mem_callback_8(self, false, vaddr, value);
        value
    }

    fn memory_read_16(&self, vaddr: u32) -> u16 {
        let vaddr = vaddr as u64;
        self.check_memory_access(vaddr, 2, DebugWatchpointType::READ);
        trace_unmapped_guest_read_regs(self, vaddr, 2);
        let v = self.mem().read_16(vaddr);
        watch_read(self, vaddr, 2, v as u128);
        v
    }

    fn memory_read_32(&self, vaddr: u32) -> u32 {
        let vaddr = vaddr as u64;
        self.check_memory_access(vaddr, 4, DebugWatchpointType::READ);
        trace_unmapped_guest_read_regs(self, vaddr, 4);
        let v = self.mem().read_32(vaddr);
        watch_read(self, vaddr, 4, v as u128);
        v
    }

    fn memory_read_64(&self, vaddr: u32) -> u64 {
        let vaddr = vaddr as u64;
        self.check_memory_access(vaddr, 8, DebugWatchpointType::READ);
        trace_unmapped_guest_read_regs(self, vaddr, 8);
        let v = self.mem().read_64(vaddr);
        watch_read(self, vaddr, 8, v as u128);
        v
    }

    fn memory_write_8(&mut self, vaddr: u32, value: u8) {
        let vaddr = vaddr as u64;
        maybe_trace_mem_callback_8(self, true, vaddr, value);
        watch_write(self, vaddr, 1, value as u128);
        if self.check_memory_access(vaddr, 1, DebugWatchpointType::WRITE) {
            trace_unmapped_write(self, vaddr, 1, value as u128);
            self.mem().write_8(vaddr, value);
        }
    }

    fn memory_write_16(&mut self, vaddr: u32, value: u16) {
        let vaddr = vaddr as u64;
        watch_write(self, vaddr, 2, value as u128);
        if self.check_memory_access(vaddr, 2, DebugWatchpointType::WRITE) {
            trace_unmapped_write(self, vaddr, 2, value as u128);
            self.mem().write_16(vaddr, value);
        }
    }

    fn memory_write_32(&mut self, vaddr: u32, value: u32) {
        let vaddr = vaddr as u64;
        watch_write(self, vaddr, 4, value as u128);
        if self.check_memory_access(vaddr, 4, DebugWatchpointType::WRITE) {
            trace_unmapped_write(self, vaddr, 4, value as u128);
            self.mem().write_32(vaddr, value);
        }
    }

    fn memory_write_64(&mut self, vaddr: u32, value: u64) {
        let vaddr = vaddr as u64;
        watch_write(self, vaddr, 8, value as u128);
        if self.check_memory_access(vaddr, 8, DebugWatchpointType::WRITE) {
            trace_unmapped_write(self, vaddr, 8, value as u128);
            self.mem().write_64(vaddr, value);
        }
    }

    fn memory_write_exclusive_8(&mut self, vaddr: u32, value: u8, expected: u8) -> bool {
        let vaddr = vaddr as u64;
        if !self.check_memory_access(vaddr, 1, DebugWatchpointType::WRITE) {
            return false;
        }
        if !a32_callback_diagnostics_enabled() {
            return self.mem().write_exclusive_8(vaddr, value, expected);
        }
        maybe_trace_w_at_vaddr(self, vaddr, 1, value as u128);
        self.mem().write_exclusive_8(vaddr, value, expected)
    }

    fn memory_write_exclusive_16(&mut self, vaddr: u32, value: u16, expected: u16) -> bool {
        let vaddr = vaddr as u64;
        if !self.check_memory_access(vaddr, 2, DebugWatchpointType::WRITE) {
            return false;
        }
        if !a32_callback_diagnostics_enabled() {
            return self.mem().write_exclusive_16(vaddr, value, expected);
        }
        maybe_trace_w_at_vaddr(self, vaddr, 2, value as u128);
        self.mem().write_exclusive_16(vaddr, value, expected)
    }

    fn memory_write_exclusive_32(&mut self, vaddr: u32, value: u32, expected: u32) -> bool {
        let vaddr = vaddr as u64;
        if !self.check_memory_access(vaddr, 4, DebugWatchpointType::WRITE) {
            return false;
        }
        if !a32_callback_diagnostics_enabled() {
            return self.mem().write_exclusive_32(vaddr, value, expected);
        }
        maybe_trace_w_at_vaddr(self, vaddr, 4, value as u128);
        let ok = self.mem().write_exclusive_32(vaddr, value, expected);
        maybe_trace_a32_exclusive(self, "write32", vaddr, value, Some(expected), Some(ok));
        // Same PC-range filter as watch_read / watch_write. Reports STLEX
        // attempts (write_exclusive_32) so we can distinguish "lock never
        // tried" from "lock always fails exclusive-check".
        if common::trace::is_enabled(common::trace::cat::STLEX) {
            let pc = self
                .jit_pc_ptr
                .map(|p| unsafe { p.read_volatile() })
                .unwrap_or(0);
            // Optional pc-range filter via env (kept as opt-in alongside the TOML toggle).
            let should = if let Some((pc_lo, pc_hi)) = watched_pc_range() {
                let pc_u64 = pc as u64;
                pc_u64 >= pc_lo && pc_u64 < pc_hi
            } else {
                true
            };
            if should {
                common::trace::emit_raw(
                    common::trace::cat::STLEX,
                    &[pc as u64, vaddr, value as u64, expected as u64, ok as u64],
                );
            }
        }
        ok
    }

    fn memory_write_exclusive_64(&mut self, vaddr: u32, value: u64, expected: u64) -> bool {
        let vaddr = vaddr as u64;
        if !self.check_memory_access(vaddr, 8, DebugWatchpointType::WRITE) {
            return false;
        }
        if !a32_callback_diagnostics_enabled() {
            return self.mem().write_exclusive_64(vaddr, value, expected);
        }
        maybe_trace_w_at_vaddr(self, vaddr, 8, value as u128);
        self.mem().write_exclusive_64(vaddr, value, expected)
    }

    fn call_svc(&mut self, svc_num: u32) {
        // Upstream: m_parent.m_svc_swi = swi;
        //           m_parent.m_jit->HaltExecution(SupervisorCall);
        self.parent().svc_swi.store(svc_num, Ordering::Relaxed);
        // RUZU_FIND_WORD / RUZU_FIND_MOVW_MOVT / RUZU_FIND_T2_MOVW_MOVT:
        // run literal/MOV pair scans once after the first SVC entry, so the
        // main module is loaded but boot is still early.
        maybe_run_one_shot_scans(self);
        self.halt_execution(rdynarmic::halt_reason::HaltReason::SVC);
    }

    fn exception_raised(&mut self, pc: u32, exception: A32Exception) {
        // Port of upstream ExceptionRaised (arm_dynarmic_32.cpp:92-109).
        //
        // Upstream behavior:
        //   NoExecuteFault: ReturnException(pc, PrefetchAbort) -> halts
        //   default:        if debugger -> ReturnException(pc, InstructionBreakpoint)
        //                   else -> LogBacktrace + LOG_CRITICAL (NO halt, continues)
        //   Hints (SEV/WFI/WFE/Yield): handled by IR as no-ops, never reach here
        match exception {
            A32Exception::NoExecuteFault => {
                log::error!("Cannot execute instruction at unmapped address {:#08x}", pc);
                // Upstream: ReturnException(pc, PrefetchAbort)
                // Store the exception address so the parent can retrieve it.
                self.parent()
                    .last_exception_address
                    .store(pc as u64, Ordering::Relaxed);
                self.halt_execution(rdynarmic::halt_reason::HaltReason::EXCEPTION_RAISED);
            }
            _ => {
                if self.debugger_enabled {
                    self.return_exception(pc, rdynarmic::halt_reason::HaltReason::BREAKPOINT);
                    return;
                }

                let mut ctx = ThreadContext::default();
                self.parent().get_context(&mut ctx);

                let process = unsafe { &*self.process };
                log::error!(
                    "ExceptionRaised(pre-logbacktrace, exception = {}, pc = {:08X}, thumb = {})",
                    exception as i32,
                    pc,
                    self.parent().is_in_thumb_mode()
                );
                self.parent().base.log_backtrace(process, &ctx);

                let code = self.mem().read_32(pc as u64);
                log::error!(
                    "ExceptionRaised(exception = {}, pc = {:08X}, code = {:08X}, thumb = {})",
                    exception as i32,
                    pc,
                    code,
                    self.parent().is_in_thumb_mode()
                );

                // Upstream logs non-NoExecute A32 exceptions but does not
                // return a guest exception unless the debugger is active.
                // Do not halt here; otherwise benign Dynarmic callbacks (for
                // thread as a fake PrefetchAbort.
            }
        }
    }

    /// Matches upstream `DynarmicCallbacks32::AddTicks`:
    /// Divides ticks by NUM_CPU_CORES (4), passes to CoreTiming::AddTicks.
    fn add_ticks(&mut self, ticks: u64) {
        // Upstream: ASSERT_MSG(!m_parent.m_uses_wall_clock, ...)
        if self.parent().base.uses_wall_clock {
            return;
        }
        // Divide by number of CPU cores, minimum 1 tick.
        // Matches upstream: amortized_ticks = max(ticks / NUM_CPU_CORES, 1)
        let amortized_ticks =
            std::cmp::max(ticks / crate::hardware_properties::NUM_CPU_CORES as u64, 1);
        self.parent().core_timing.add_ticks(amortized_ticks);
    }

    /// Matches upstream `DynarmicCallbacks32::GetTicksRemaining`:
    /// Returns max(CoreTiming::GetDowncount(), 0).
    fn get_ticks_remaining(&self) -> u64 {
        // Upstream: ASSERT_MSG(!m_parent.m_uses_wall_clock, ...)
        if self.parent().base.uses_wall_clock {
            return u64::MAX;
        }
        std::cmp::max(self.parent().core_timing.get_downcount(), 0) as u64
    }

    fn set_pc_ptr(&mut self, ptr: *const u32) {
        self.jit_pc_ptr = Some(ptr);
    }
}

/// ARM32 Dynarmic JIT backend.
///
/// Corresponds to upstream `Core::ArmDynarmic32`.
pub struct ArmDynarmic32 {
    pub base: ArmInterfaceBase,

    // Upstream holds `System& m_system` for accessing CoreTiming, DebuggerEnabled,
    // Settings, etc. Currently these are passed individually (core_timing, uses_wall_clock)
    // to avoid circular dependency with System which owns the ARM backends.
    // When System stabilizes, this can be replaced with a reference.
    /// Core index for this CPU.
    /// Upstream: `m_core_index`.
    core_index: usize,

    /// SVC callback number.
    /// Upstream: `m_svc_swi` written by callback via `m_parent` reference.
    svc_swi: Arc<AtomicU32>,

    /// Core timing reference for tick management.
    /// Upstream: accessed via `m_system.CoreTiming()`.
    /// Stored here so callbacks can access it via `parent().core_timing`.
    core_timing: Arc<crate::core_timing::CoreTiming>,

    /// Shared atomic pointer used by callbacks to reach back to this ArmDynarmic32.
    /// The callbacks store a clone of this Arc. After JIT creation, the parent sets
    /// this to point to itself, allowing callbacks to access parent fields.
    parent_ptr: Arc<AtomicPtr<ArmDynarmic32>>,

    /// Watchpoint that caused a halt
    halted_watchpoint: Arc<Mutex<Option<DebugWatchpoint>>>,

    /// Context saved at breakpoint
    breakpoint_context: Arc<Mutex<ThreadContext>>,

    /// The rdynarmic A32 JIT instance
    jit: Option<rdynarmic::A32Jit>,

    /// Upstream: `std::shared_ptr<DynarmicCP15> m_cp15`.
    cp15: Arc<DynarmicCP15>,

    /// Last exception address reported by dynarmic for the current halt.
    last_exception_address: Arc<AtomicU64>,

    /// Cached fastmem pointer for bounded instruction tracing.
    /// Temporary diagnostic state, not an upstream field.
    trace_fastmem_ptr: *const u8,
}

impl ArmDynarmic32 {
    #[inline]
    pub fn core_index(&self) -> usize {
        self.core_index
    }

    /// Create a new ARM32 dynarmic backend.
    ///
    /// Corresponds to upstream `ArmDynarmic32::ArmDynarmic32`.
    pub fn new(
        _system: &dyn std::any::Any,
        uses_wall_clock: bool,
        process: &KProcess,
        exclusive_monitor: *mut crate::arm::dynarmic::dynarmic_exclusive_monitor::DynarmicExclusiveMonitor,
        core_index: usize,
        shared_memory: SharedProcessMemory,
        core_timing: Arc<crate::core_timing::CoreTiming>,
        core_memory: Option<Arc<std::sync::Mutex<Memory>>>,
        debugger_enabled: bool,
    ) -> Self {
        // Get page-table and fastmem pointers from the process memory state.
        // Upstream `ArmDynarmic32::MakeJit(Common::PageTable*)` receives the
        // process page table and then reads `page_table->fastmem_arena`, which
        // is set from `DeviceMemory().buffer.VirtualBasePointer()` by
        // `Memory::SetCurrentPageTable`. In Rust the page-table path remains
        // optional while the fastmem arena base is available directly through
        // the per-process Memory bridge, matching ArmDynarmic64's wiring.
        let (mut page_table_pointer, mut fastmem_pointer): (Option<*const u8>, Option<*mut u8>) =
            if std::env::var_os("RUZU_NO_FASTMEM").is_some() {
                (None, None)
            } else {
                let page_table_pointer = {
                    let kernel_process = unsafe {
                        &*(process as *const _ as *const crate::hle::kernel::k_process::KProcess)
                    };
                    kernel_process
                        .page_table
                        .get_base()
                        .get_impl()
                        .map(|page_table| page_table.pointers.data() as *const u8)
                        .filter(|p| !p.is_null())
                };
                let fastmem_pointer = {
                    let kernel_process = unsafe {
                        &*(process as *const _ as *const crate::hle::kernel::k_process::KProcess)
                    };
                    kernel_process
                        .page_table
                        .get_base()
                        .get_impl()
                        .map(|page_table| page_table.fastmem_arena)
                }
                .filter(|p| !p.is_null());
                (page_table_pointer, fastmem_pointer)
            };

        let svc_swi = Arc::new(AtomicU32::new(0));
        let last_exception_address = Arc::new(AtomicU64::new(0));
        let parent_ptr = Arc::new(AtomicPtr::new(std::ptr::null_mut()));
        let base = ArmInterfaceBase::new(uses_wall_clock);
        let halted_watchpoint = Arc::new(Mutex::new(None));
        let callbacks = DynarmicCallbacks32::new(
            shared_memory,
            core_memory,
            process as *const _ as *const crate::hle::kernel::k_process::KProcess,
            parent_ptr.clone(),
            debugger_enabled,
            base.shared_watchpoint_array(),
            Arc::clone(&halted_watchpoint),
        );
        let cp15 = Arc::new(DynarmicCP15::new(parent_ptr.clone()));

        let settings = common::settings::values();
        let (optimizations, unsafe_optimizations) = if let Some(mask) =
            std::env::var("RUZU_A32_OPTIMIZATION_MASK")
                .ok()
                .and_then(|value| {
                    let trimmed = value.trim();
                    let digits = trimmed
                        .strip_prefix("0x")
                        .or_else(|| trimmed.strip_prefix("0X"))
                        .unwrap_or(trimmed);
                    u32::from_str_radix(digits, 16)
                        .ok()
                        .or_else(|| trimmed.parse::<u32>().ok())
                }) {
            let flags = optimization_flags_from_mask(mask);
            (
                flags,
                (flags.bits() & !OptimizationFlag::ALL_SAFE_OPTIMIZATIONS.bits()) != 0,
            )
        } else if std::env::var("RUZU_A32_NO_OPTIMIZATIONS")
            .ok()
            .is_some_and(|value| value != "0")
        {
            (OptimizationFlag::NO_OPTIMIZATIONS, false)
        } else {
            upstream_optimization_config_from_settings(&settings)
        };

        // Upstream `ArmDynarmic32::MakeJit` uses 128 MiB on ARM64 hosts and
        // 512 MiB elsewhere. Dynarmic's ARM64 backend cannot branch across a
        // larger cache with its current direct-branch layout.
        let code_cache_size = if cfg!(target_arch = "aarch64") {
            128 * 1024 * 1024
        } else {
            512 * 1024 * 1024
        };
        let mut fastmem_exclusive_access = fastmem_pointer.is_some();
        let mut recompile_on_exclusive_fastmem_failure = true;
        let mut only_detect_misalignment_via_page_table_on_page_boundary = true;

        if *settings.cpu_debug_mode.get_value() {
            if !*settings.cpuopt_page_tables.get_value() {
                page_table_pointer = None;
            }
            if !*settings.cpuopt_reduce_misalign_checks.get_value() {
                only_detect_misalignment_via_page_table_on_page_boundary = false;
            }
            if !*settings.cpuopt_fastmem.get_value() {
                fastmem_pointer = None;
                fastmem_exclusive_access = false;
            }
            if !*settings.cpuopt_fastmem_exclusives.get_value() {
                fastmem_exclusive_access = false;
            }
            if !*settings.cpuopt_recompile_exclusives.get_value() {
                recompile_on_exclusive_fastmem_failure = false;
            }
        }
        if !common::settings::is_fastmem_enabled(&settings) {
            fastmem_pointer = None;
            fastmem_exclusive_access = false;
        }
        let check_halt_on_memory_access = debugger_enabled
            || (*settings.cpu_debug_mode.get_value()
                && !*settings.cpuopt_ignore_memory_aborts.get_value());

        let mut coprocessors = empty_coprocessors();
        coprocessors[15] = Some(cp15.clone());
        let config = A32UserConfig {
            callbacks: Box::new(callbacks),
            global_monitor: if exclusive_monitor.is_null() {
                None
            } else {
                Some(unsafe { (*exclusive_monitor).get_monitor() as *mut _ })
            },
            page_table: page_table_pointer
                .map(|pointer| pointer as *mut [*mut u8; A32UserConfig::NUM_PAGE_TABLE_ENTRIES]),
            coprocessors,
            fastmem_pointer,
            optimizations,
            code_cache_size: code_cache_size
                .try_into()
                .expect("A32 code cache size must fit u32"),
            page_table_pointer_mask_bits: PageInfo::ATTRIBUTE_BITS
                .try_into()
                .expect("A32 page-table pointer mask must fit i32"),
            page_table_log2_stride: PAGE_TABLE_LOG2_STRIDE,
            arch_version: rdynarmic::interface::a32::arch_version::ArchVersion::V8,
            processor_id: core_index.try_into().expect("A32 processor id must fit u8"),
            detect_misaligned_access_via_page_table: 16 | 32 | 64 | 128,
            unsafe_optimizations,
            absolute_offset_page_table: true,
            only_detect_misalignment_via_page_table_on_page_boundary,
            recompile_on_fastmem_failure: true,
            fastmem_exclusive_access,
            recompile_on_exclusive_fastmem_failure,
            hook_isb: false,
            hook_hint_instructions: false,
            define_unpredictable_behaviour: true,
            wall_clock_cntpct: uses_wall_clock,
            check_halt_on_memory_access,
            enable_cycle_counting: !uses_wall_clock,
            always_little_endian: false,
            very_verbose_debugging_output: false,
        };

        if std::env::var_os("RUZU_TRACE_A32_JIT_CONFIG").is_some() {
            log::warn!(
                "ArmDynarmic32: page_table_pointer={:?} fastmem_pointer={:?} cycle_counting={} optimizations={:#x} unsafe_optimizations={} code_cache_size={}",
                page_table_pointer.map(|p| p as usize),
                fastmem_pointer.map(|p| p as usize),
                !uses_wall_clock,
                optimizations.bits(),
                unsafe_optimizations,
                code_cache_size
            );
        }

        let jit = match rdynarmic::A32Jit::new(config) {
            Ok(jit) => {
                log::info!(
                    "ArmDynarmic32: JIT created successfully for core {}",
                    core_index
                );
                Some(jit)
            }
            Err(e) => {
                log::error!(
                    "ArmDynarmic32: Failed to create JIT for core {}: {}",
                    core_index,
                    e
                );
                None
            }
        };

        // Expose the fastmem base to audio_core's direct-write tracer so it
        // can translate host pointers back to guest vaddrs (the host base
        // shifts per run due to ASLR; using guest vaddrs keeps env-var
        // configuration stable across runs).
        if let Some(p) = fastmem_pointer {
            common::fastmem_registry::set(p as usize);
        }

        // RUZU_FASTMEM_TRAP_PAGE=0xADDR — mprotect the host page backing the
        // given guest vaddr in the fastmem arena as PROT_READ, so any
        // subsequent JIT-emitted write through fastmem faults. The
        // backend's SIGSEGV handler then patches the faulting MOV and
        // routes the write through the slow callback path, which makes the
        // existing `RUZU_TRACE_W_AT_VADDR=…` / WATCH_WRITE diagnostics
        // fire on stores that would otherwise be invisible. Used to
        // call chain (task #112) without paying the global `NO_FASTMEM`
        // slowdown.
        #[cfg(any(unix, windows))]
        maybe_trap_fastmem_page(fastmem_pointer, core_index);

        // RUZU_WATCH_VADDR_POLL=0xADDR — non-intrusive memory watcher.
        // A background thread reads `[fastmem_pointer + vaddr]` every
        // `RUZU_WATCH_VADDR_POLL_INTERVAL_MS` (default 10) and logs the
        // value when it changes. Catches writes regardless of mechanism
        // (fastmem, callback, HLE direct, etc.) at the cost of missing
        // back-to-back writes that happen within the poll interval.
        // Complements the JIT-side trap when the writer is invisible to
        // the SIGSEGV recovery path.
        maybe_spawn_vaddr_poller(fastmem_pointer, core_index);

        let result = Self {
            base,
            core_index,
            svc_swi,
            core_timing,
            parent_ptr,
            halted_watchpoint,
            breakpoint_context: Arc::new(Mutex::new(ThreadContext::default())),
            jit,
            cp15,
            last_exception_address,
            trace_fastmem_ptr: fastmem_pointer.unwrap_or(std::ptr::null_mut()) as *const u8,
        };

        // NOTE: The parent pointer is NOT set here because `result` will be moved
        // by the caller (e.g. into a Box). The caller MUST call `set_parent_ptr()`
        // after placing the ArmDynarmic32 at its final stable location.
        // Until then, callbacks that access parent() will panic on the debug_assert.
        // This is safe because callbacks are only invoked during run_thread().

        result
    }

    /// Set the parent pointer so callbacks can access this ArmDynarmic32.
    ///
    /// MUST be called after the ArmDynarmic32 is placed at its final stable memory
    /// location (e.g. after Box allocation). The pointer must remain valid for the
    /// lifetime of the JIT. Callbacks will panic if parent() is called before this.
    ///
    /// Matches upstream where `m_parent` is a reference set during construction.
    /// In Rust, we defer because the callbacks are consumed by the JIT before the
    /// parent struct reaches its final location.
    pub fn set_parent_ptr(&mut self) {
        let ptr: *mut ArmDynarmic32 = self;
        self.parent_ptr.store(ptr, Ordering::Release);
    }

    pub(super) fn clock_ticks(&self) -> u64 {
        self.core_timing.get_clock_ticks()
    }

    /// Check if CPU is in Thumb mode.
    ///
    /// Corresponds to upstream `ArmDynarmic32::IsInThumbMode`.
    pub fn is_in_thumb_mode(&self) -> bool {
        if let Some(jit) = self.jit.as_ref() {
            // Thumb bit is bit 5 of CPSR
            (jit.get_cpsr() & 0x20) != 0
        } else {
            log::warn!("ArmDynarmic32::is_in_thumb_mode: JIT not available");
            false
        }
    }

    /// Convert FPSCR to separate FPSR and FPCR values.
    ///
    /// Corresponds to upstream `FpscrToFpsrFpcr`.
    fn fpscr_to_fpsr_fpcr(fpscr: u32) -> (u32, u32) {
        // FPSCR bits [31:27] -> FPSR[31:27]
        // FPSCR bit [7] -> FPSR[7]
        // FPSCR bits [4:0] -> FPSR[4:0]
        let nzcv = fpscr & 0xf800_0000;
        let idc = fpscr & 0x80;
        let fiq = fpscr & 0x1f;
        let fpsr = nzcv | idc | fiq;

        // FPSCR bits [26:15] -> FPCR[26:15]
        // FPSCR bits [12:8] -> FPCR[12:8]
        let round = fpscr & 0x07ff_8000;
        let trap = fpscr & 0x1f00;
        let fpcr = round | trap;

        (fpsr, fpcr)
    }

    /// Convert separate FPSR and FPCR values back to FPSCR.
    ///
    /// Corresponds to upstream `FpsrFpcrToFpscr`.
    fn fpsr_fpcr_to_fpscr(fpsr: u64, fpcr: u64) -> u32 {
        let combined = (fpsr as u32) | (fpcr as u32);
        let (s, c) = Self::fpscr_to_fpsr_fpcr(combined);
        s | c
    }
}

// SAFETY: ArmDynarmic32 holds raw pointers to long-lived process/watchpoint state.
// The JIT is single-threaded per core — only one thread runs each ArmDynarmic32.
unsafe impl Send for ArmDynarmic32 {}

impl ArmInterface for ArmDynarmic32 {
    fn run_thread(&mut self, _thread: &mut KThread) -> HaltReason {
        self.last_exception_address.store(0, Ordering::Relaxed);
        let trace_fastmem_ptr = self.trace_fastmem_ptr;
        let jit = match self.jit.as_mut() {
            Some(jit) => jit,
            None => {
                log::error!("ArmDynarmic32::run_thread: JIT not available");
                return HaltReason::BREAK_LOOP;
            }
        };

        jit.clear_exclusive_state();

        // Cache trace config to avoid parsing env vars on every run_thread call
        static TRACE_CFG: std::sync::OnceLock<(Option<u32>, Option<u32>, u32, u32)> =
            std::sync::OnceLock::new();
        let &(trace_start, trace_end, trace_limit, trace_search_limit) =
            TRACE_CFG.get_or_init(|| {
                (
                    parse_trace_hex_env("RUZU_A32_TRACE_RANGE_START"),
                    parse_trace_hex_env("RUZU_A32_TRACE_RANGE_END"),
                    parse_trace_u32_env("RUZU_A32_TRACE_LIMIT").unwrap_or(0),
                    parse_trace_u32_env("RUZU_A32_TRACE_SEARCH_LIMIT").unwrap_or(0),
                )
            });
        if let (Some(start), Some(end)) = (trace_start, trace_end) {
            let current_pc = jit.get_register(15);
            let trace_only_when_pc_window =
                std::env::var_os("RUZU_A32_TRACE_ONLY_WHEN_PC_WINDOW").is_some();
            let pc_window_active =
                rdynarmic::jit::PC_TRACE_ACTIVE.load(std::sync::atomic::Ordering::Relaxed);
            if trace_only_when_pc_window
                && !pc_window_active
                && !(current_pc >= start && current_pc < end)
            {
                let rdynarmic_hr = jit.run();
                return translate_halt_reason(rdynarmic_hr);
            }
            let trace_after_watch = std::env::var_os("RUZU_A32_TRACE_AFTER_WATCH").is_some();
            let trace_strict_after_watch =
                std::env::var_os("RUZU_A32_TRACE_STRICT_AFTER_WATCH").is_some();
            if trace_after_watch
                && trace_search_limit > 0
                && !A32_TRACE_AFTER_WATCH_ARMED.load(Ordering::Relaxed)
                && (trace_strict_after_watch || !(current_pc >= start && current_pc < end))
            {
                let rdynarmic_hr = jit.run();
                return translate_halt_reason(rdynarmic_hr);
            }
            if trace_limit > 0
                && (current_pc >= start && current_pc < end || trace_search_limit > 0)
            {
                let mut last_hr = rdynarmic::halt_reason::HaltReason::empty();
                let mut entered_range = current_pc >= start && current_pc < end;
                let mut logged_steps = 0u32;
                let total_limit = if entered_range {
                    trace_limit
                } else {
                    trace_search_limit.saturating_add(trace_limit)
                };
                for step in 0..total_limit {
                    let pc = jit.get_register(15);
                    if !entered_range {
                        let quiet_search =
                            std::env::var_os("RUZU_A32_TRACE_SEARCH_QUIET").is_some();
                        if !quiet_search {
                            log::info!(
                                "[A32TRACE] search_step={} pc=0x{:08x} cpsr=0x{:08x} r0=0x{:08x} r1=0x{:08x} r2=0x{:08x} r3=0x{:08x} sp=0x{:08x} lr=0x{:08x}",
                                step,
                                pc,
                                jit.get_cpsr(),
                                jit.get_register(0),
                                jit.get_register(1),
                                jit.get_register(2),
                                jit.get_register(3),
                                jit.get_register(13),
                                jit.get_register(14),
                            );
                        }
                        if pc >= start && pc < end {
                            entered_range = true;
                            log::info!(
                                "[A32TRACE] entered range at search_step={} pc=0x{:08x}",
                                step,
                                pc
                            );
                        } else {
                            last_hr = jit.step();
                            if !last_hr.is_empty()
                                && last_hr != rdynarmic::halt_reason::HaltReason::STEP
                            {
                                if !quiet_search {
                                    log::info!("[A32TRACE] halt while searching: {:?}", last_hr);
                                }
                                break;
                            }
                            if step + 1 >= trace_search_limit {
                                break;
                            }
                            continue;
                        }
                    }
                    if logged_steps >= trace_limit {
                        break;
                    }
                    let cpsr = jit.get_cpsr();
                    let read_code_word = |vaddr: u32| -> u32 {
                        if trace_fastmem_ptr.is_null() {
                            return 0;
                        }
                        unsafe {
                            (trace_fastmem_ptr.add(vaddr as usize) as *const u32).read_unaligned()
                        }
                    };
                    log::info!(
                        "[A32TRACE] step={} search_step={} pc=0x{:08x} cpsr=0x{:08x} op_m1=0x{:08x} op_0=0x{:08x} op_p1=0x{:08x} op_p2=0x{:08x} r0=0x{:08x} r1=0x{:08x} r2=0x{:08x} r3=0x{:08x} r4=0x{:08x} r5=0x{:08x} r6=0x{:08x} r7=0x{:08x} r8=0x{:08x} r9=0x{:08x} r10=0x{:08x} r11=0x{:08x} r12=0x{:08x} sp=0x{:08x} lr=0x{:08x}",
                        logged_steps,
                        step,
                        pc,
                        cpsr,
                        read_code_word(pc.saturating_sub(4)),
                        read_code_word(pc),
                        read_code_word(pc.saturating_add(4)),
                        read_code_word(pc.saturating_add(8)),
                        jit.get_register(0),
                        jit.get_register(1),
                        jit.get_register(2),
                        jit.get_register(3),
                        jit.get_register(4),
                        jit.get_register(5),
                        jit.get_register(6),
                        jit.get_register(7),
                        jit.get_register(8),
                        jit.get_register(9),
                        jit.get_register(10),
                        jit.get_register(11),
                        jit.get_register(12),
                        jit.get_register(13),
                        jit.get_register(14),
                    );
                    logged_steps += 1;
                    last_hr = jit.step();
                    if std::env::var_os("RUZU_A32_TRACE_HALT_AFTER_WATCH").is_some()
                        && last_hr.contains(rdynarmic::halt_reason::HaltReason::USER_DEFINED2)
                    {
                        last_hr = rdynarmic::halt_reason::HaltReason::STEP;
                    }
                    if !last_hr.is_empty() && last_hr != rdynarmic::halt_reason::HaltReason::STEP {
                        log::info!("[A32TRACE] halt={:?}", last_hr);
                        break;
                    }
                }
                return translate_halt_reason(last_hr);
            }
        }

        let rdynarmic_hr = jit.run();
        translate_halt_reason(rdynarmic_hr)
    }

    fn step_thread(&mut self, _thread: &mut KThread) -> HaltReason {
        self.last_exception_address.store(0, Ordering::Relaxed);
        let jit = match self.jit.as_mut() {
            Some(jit) => jit,
            None => {
                log::error!("ArmDynarmic32::step_thread: JIT not available");
                return HaltReason::BREAK_LOOP;
            }
        };

        jit.clear_exclusive_state();
        // Upstream uses m_jit->Step() for single-instruction stepping.
        let rdynarmic_hr = jit.step();
        translate_halt_reason(rdynarmic_hr)
    }

    fn clear_instruction_cache(&mut self) {
        if let Some(jit) = self.jit.as_mut() {
            jit.clear_cache();
        }
    }

    fn invalidate_cache_range(&mut self, addr: u64, size: usize) {
        if let Some(jit) = self.jit.as_mut() {
            // Upstream casts addr to u32 for A32
            jit.invalidate_cache_range(addr, size as u64);
        }
    }

    fn dump_jit_block_map(&mut self, path: &str) {
        if let Some(jit) = self.jit.as_ref() {
            if let Err(err) = jit.dump_jit_block_map(path) {
                log::warn!("ArmDynarmic32: failed to dump JIT block map: {err}");
            }
        }
    }

    fn get_architecture(&self) -> Architecture {
        Architecture::AArch32
    }

    fn get_context(&self, ctx: &mut ThreadContext) {
        let jit = match self.jit.as_ref() {
            Some(jit) => jit,
            None => {
                log::warn!("ArmDynarmic32::get_context: JIT not available");
                return;
            }
        };

        // Upstream maps A32 GPRs to ThreadContext:
        // GPR[0..15] -> ctx.r[0..15], rest zeroed
        for i in 0..16 {
            ctx.r[i] = jit.get_register(i) as u64;
        }
        ctx.fp = jit.get_register(11) as u64;
        // r[15] is PC in A32
        ctx.pc = jit.get_register(15) as u64;
        ctx.sp = jit.get_register(13) as u64;
        ctx.lr = jit.get_register(14) as u64;

        ctx.pstate = jit.get_cpsr();

        // ExtRegs -> Vectors (A32 uses VFP/NEON extension registers)
        // Upstream reads 64 ExtRegs (u32 each) and maps groups of 4 to u128 vectors.
        // ext_reg layout: 64 x u32, where ext_reg[i*4..i*4+4] maps to ctx.v[i].
        for i in 0..16 {
            let e0 = jit.get_ext_reg(i * 4) as u128;
            let e1 = jit.get_ext_reg(i * 4 + 1) as u128;
            let e2 = jit.get_ext_reg(i * 4 + 2) as u128;
            let e3 = jit.get_ext_reg(i * 4 + 3) as u128;
            ctx.v[i] = e0 | (e1 << 32) | (e2 << 64) | (e3 << 96);
        }
        // A32 only has 16 Q-registers (D0-D31 / S0-S63)
        for i in 16..32 {
            ctx.v[i] = 0;
        }

        let (fpsr, fpcr) = Self::fpscr_to_fpsr_fpcr(jit.get_fpscr());
        ctx.fpcr = fpcr;
        ctx.fpsr = fpsr;
        ctx.tpidr = self.cp15.uprw() as u64;
    }

    fn set_context(&mut self, ctx: &ThreadContext) {
        let jit = match self.jit.as_mut() {
            Some(jit) => jit,
            None => {
                log::warn!("ArmDynarmic32::set_context: JIT not available");
                return;
            }
        };

        // Upstream maps ThreadContext back to A32 GPRs.
        // Upstream loops 0..16, reading all 16 GPRs (including R15/PC) from ctx.r[i].
        for i in 0..16 {
            jit.set_register(i, ctx.r[i] as u32);
        }

        jit.set_cpsr(ctx.pstate);

        // Vectors -> ExtRegs
        for i in 0..16 {
            jit.set_ext_reg(i * 4, ctx.v[i] as u32);
            jit.set_ext_reg(i * 4 + 1, (ctx.v[i] >> 32) as u32);
            jit.set_ext_reg(i * 4 + 2, (ctx.v[i] >> 64) as u32);
            jit.set_ext_reg(i * 4 + 3, (ctx.v[i] >> 96) as u32);
        }

        let fpscr = Self::fpsr_fpcr_to_fpscr(ctx.fpsr as u64, ctx.fpcr as u64);
        jit.set_fpscr(fpscr);
        self.cp15.set_uprw(ctx.tpidr as u32);
    }

    fn set_tpidrro_el0(&mut self, value: u64) {
        self.cp15.set_uro(value as u32);
    }

    fn set_watchpoint_array(&mut self, watchpoints: *const WatchpointArray) {
        self.base.set_watchpoint_array(watchpoints);
    }

    fn get_tpidrro_el0(&self) -> u64 {
        self.cp15.uro() as u64
    }

    fn get_svc_arguments(&self, args: &mut [u64; 8]) {
        let jit = match self.jit.as_ref() {
            Some(jit) => jit,
            None => {
                log::warn!("ArmDynarmic32::get_svc_arguments: JIT not available");
                return;
            }
        };

        // Upstream reads GPR[0..8] from JIT
        for i in 0..8 {
            args[i] = jit.get_register(i) as u64;
        }
    }

    fn set_svc_arguments(&mut self, args: &[u64; 8]) {
        let jit = match self.jit.as_mut() {
            Some(jit) => jit,
            None => {
                log::warn!("ArmDynarmic32::set_svc_arguments: JIT not available");
                return;
            }
        };

        // Upstream writes GPR[0..8] to JIT as u32
        for i in 0..8 {
            jit.set_register(i, args[i] as u32);
        }
    }

    fn get_svc_number(&self) -> u32 {
        self.svc_swi.load(Ordering::Relaxed)
    }

    fn get_last_exception_address(&self) -> Option<u64> {
        let address = self.last_exception_address.load(Ordering::Relaxed);
        if address == 0 {
            None
        } else {
            Some(address)
        }
    }

    fn signal_interrupt(&mut self, _thread: &mut KThread) {
        if let Some(jit) = self.jit.as_ref() {
            jit.halt_execution(rdynarmic::halt_reason::HaltReason::EXTERNAL_HALT);
        }
    }

    fn halted_watchpoint(&self) -> Option<DebugWatchpoint> {
        *self.halted_watchpoint.lock().unwrap()
    }

    fn rewind_breakpoint_instruction(&mut self) {
        let ctx = self.breakpoint_context.lock().unwrap().clone();
        self.set_context(&ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::kernel::k_process::ProcessMemoryData;
    use std::sync::RwLock;

    #[test]
    fn callbacks_implement_the_architecture_owned_a32_interface() {
        fn assert_a32_callbacks<T: A32UserCallbacks>() {}

        assert_a32_callbacks::<DynarmicCallbacks32>();
    }

    #[test]
    fn page_table_stride_matches_exposed_page_info_buffer() {
        assert_eq!(
            1usize << PAGE_TABLE_LOG2_STRIDE,
            std::mem::size_of::<PageInfo>()
        );
    }

    #[test]
    fn memory_read_code_returns_none_for_invalid_fetch() {
        let mut backing = ProcessMemoryData::new();
        backing.base = 0x1000;
        backing.data = vec![0x78, 0x56, 0x34, 0x12];
        let callbacks = DynarmicCallbacks32::new(
            Arc::new(RwLock::new(backing)),
            None,
            std::ptr::null(),
            Arc::new(AtomicPtr::new(std::ptr::null_mut())),
            false,
            ArmInterfaceBase::new(false).shared_watchpoint_array(),
            Arc::new(Mutex::new(None)),
        );

        assert_eq!(callbacks.memory_read_code(0x1000), Some(0x12345678));
        assert_eq!(callbacks.memory_read_code(0), None);
    }

    #[test]
    fn translate_halt_reason_includes_memory_abort() {
        assert_eq!(
            translate_halt_reason(rdynarmic::halt_reason::HaltReason::MEMORY_ABORT),
            HaltReason::DATA_ABORT
        );
    }

    #[test]
    fn auto_optimization_config_matches_upstream_a32() {
        let mut settings = common::settings::Values::default();
        settings.cpu_debug_mode.set_value(false);
        settings.cpu_accuracy.set_value(CpuAccuracy::Auto);

        let (flags, unsafe_optimizations) = upstream_optimization_config_from_settings(&settings);
        assert!(unsafe_optimizations);
        assert!(flags.contains(OptimizationFlag::UNSAFE_UNFUSE_FMA));
        assert!(flags.contains(OptimizationFlag::UNSAFE_IGNORE_STANDARD_FPCR_VALUE));
        assert!(flags.contains(OptimizationFlag::UNSAFE_INACCURATE_NAN));
        assert!(!flags.contains(OptimizationFlag::UNSAFE_IGNORE_GLOBAL_MONITOR));
    }
}
