// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/arm/dynarmic/arm_dynarmic_64.h and arm_dynarmic_64.cpp
//! ARM64 dynarmic backend.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::arm::arm_interface::{
    matching_watchpoint, Architecture, ArmInterface, ArmInterfaceBase, DebugWatchpoint,
    DebugWatchpointType, HaltReason, KProcess, KThread, SharedWatchpointArray, ThreadContext,
    WatchpointArray,
};
use crate::hle::kernel::k_process::SharedProcessMemory;
use crate::memory::memory::Memory;
use common::page_table::PageInfo;
use common::settings_enums::CpuAccuracy;

use rdynarmic::backend::x64::jit_state::A64JitState;
use rdynarmic::jit_config::{JitConfig, OptimizationFlag, UserCallbacks};

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

fn upstream_optimization_config(
    address_space_bits: usize,
    accuracy: CpuAccuracy,
    unsafe_unfuse_fma: bool,
    unsafe_reduce_fp_error: bool,
    unsafe_inaccurate_nan: bool,
    unsafe_fastmem_check: bool,
    unsafe_ignore_global_monitor: bool,
) -> (OptimizationFlag, bool, usize) {
    let mut flags = OptimizationFlag::ALL_SAFE_OPTIMIZATIONS;
    let mut unsafe_optimizations = false;
    let mut fastmem_address_space_bits = address_space_bits;

    match accuracy {
        CpuAccuracy::Unsafe => {
            unsafe_optimizations = true;
            if unsafe_unfuse_fma {
                flags |= OptimizationFlag::UNSAFE_UNFUSE_FMA;
            }
            if unsafe_reduce_fp_error {
                flags |= OptimizationFlag::UNSAFE_REDUCED_ERROR_FP;
            }
            if unsafe_inaccurate_nan {
                flags |= OptimizationFlag::UNSAFE_INACCURATE_NAN;
            }
            if unsafe_fastmem_check {
                fastmem_address_space_bits = 64;
            }
            if unsafe_ignore_global_monitor {
                flags |= OptimizationFlag::UNSAFE_IGNORE_GLOBAL_MONITOR;
            }
        }
        CpuAccuracy::Auto => {
            unsafe_optimizations = true;
            flags |= OptimizationFlag::UNSAFE_UNFUSE_FMA;
            fastmem_address_space_bits = 64;
        }
        CpuAccuracy::Paranoid => {
            flags = OptimizationFlag::NO_OPTIMIZATIONS;
        }
        CpuAccuracy::Accurate => {}
    }

    (flags, unsafe_optimizations, fastmem_address_space_bits)
}

fn parse_optimization_mask_env(name: &str) -> Option<u32> {
    std::env::var(name).ok().and_then(|value| {
        let trimmed = value.trim();
        let digits = trimmed
            .strip_prefix("0x")
            .or_else(|| trimmed.strip_prefix("0X"))
            .unwrap_or(trimmed);
        u32::from_str_radix(digits, 16)
            .ok()
            .or_else(|| trimmed.parse::<u32>().ok())
    })
}

fn parse_watch_ranges(raw: &str) -> Vec<(u64, u64)> {
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
}

fn watched_ranges_64() -> &'static [(u64, u64)] {
    use std::sync::OnceLock;
    static RANGES: OnceLock<Vec<(u64, u64)>> = OnceLock::new();
    RANGES.get_or_init(|| {
        let raw = std::env::var("RUZU_WATCH_ADDR").unwrap_or_default();
        parse_watch_ranges(&raw)
    })
}

fn parse_u64_env(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|value| {
        let trimmed = value.trim();
        let digits = trimmed
            .strip_prefix("0x")
            .or_else(|| trimmed.strip_prefix("0X"))
            .unwrap_or(trimmed);
        u64::from_str_radix(digits, 16)
            .ok()
            .or_else(|| trimmed.parse::<u64>().ok())
    })
}

#[derive(Clone, Copy)]
struct TraceMaskedAddress {
    target: u64,
    mask: u64,
}

#[derive(Clone, Copy)]
enum TraceWriteAddress {
    Any,
    Exact(u64),
    Page(u64),
}

impl TraceWriteAddress {
    #[inline(always)]
    fn matches(self, vaddr: u64) -> bool {
        match self {
            Self::Any => true,
            Self::Exact(target) => vaddr == target,
            Self::Page(target) => (vaddr & !0xFFFu64) == (target & !0xFFFu64),
        }
    }

    #[inline(always)]
    fn matches_span(self, vaddr: u64, size: u64) -> bool {
        match self {
            Self::Any => true,
            Self::Exact(target) => vaddr <= target && target < vaddr.saturating_add(size),
            Self::Page(target) => {
                (vaddr & !0xFFFu64) == (target & !0xFFFu64)
                    || ((vaddr.saturating_add(size.saturating_sub(1))) & !0xFFFu64)
                        == (target & !0xFFFu64)
            }
        }
    }
}

fn parse_trace_write_address(raw: &str) -> Option<TraceWriteAddress> {
    let (addr_part, page_match) = if let Some(stripped) = raw.strip_suffix(":page") {
        (stripped, true)
    } else {
        (raw, false)
    };
    if addr_part.trim() == "*" {
        return Some(TraceWriteAddress::Any);
    }
    let target = u64::from_str_radix(addr_part.trim().trim_start_matches("0x"), 16).ok()?;
    Some(if page_match {
        TraceWriteAddress::Page(target)
    } else {
        TraceWriteAddress::Exact(target)
    })
}

fn snapshot_vaddr() -> Option<u64> {
    static VALUE: OnceLock<Option<u64>> = OnceLock::new();
    *VALUE.get_or_init(|| parse_u64_env("RUZU_SNAPSHOT_VADDR").filter(|addr| *addr != 0))
}

fn trace_r64_at_vaddr() -> Option<TraceMaskedAddress> {
    static SPEC: OnceLock<Option<TraceMaskedAddress>> = OnceLock::new();
    *SPEC.get_or_init(|| {
        let spec = std::env::var("RUZU_TRACE_R64_AT_VADDR").ok()?;
        let mut parts = spec.splitn(2, '/');
        let addr_str = parts.next().unwrap_or("");
        let mask_str = parts.next().unwrap_or("0xFFFFFFFFFFFFFFFF");
        let target = u64::from_str_radix(addr_str.trim_start_matches("0x"), 16).ok()?;
        let mask = u64::from_str_radix(mask_str.trim_start_matches("0x"), 16).unwrap_or(!0);
        Some(TraceMaskedAddress { target, mask })
    })
}

fn trace_r64_value() -> Option<u64> {
    static VALUE: OnceLock<Option<u64>> = OnceLock::new();
    *VALUE.get_or_init(|| parse_u64_env("RUZU_TRACE_R64_VALUE"))
}

fn trace_a64_invalid_data_read_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_A64_INVALID_DATA_READ").is_some())
}

fn dump_on_invalid_spec() -> Option<(u64, usize)> {
    static SPEC: OnceLock<Option<(u64, usize)>> = OnceLock::new();
    *SPEC.get_or_init(|| {
        let spec = std::env::var("RUZU_DUMP_ON_INVALID").ok()?;
        let mut parts = spec.splitn(2, ':');
        let addr = parts
            .next()
            .and_then(|s| u64::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
            .unwrap_or(0);
        let size = parts
            .next()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .unwrap_or(64)
            .min(128);
        (addr != 0).then_some((addr, size))
    })
}

fn dump_x22_plus_16_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_DUMP_X22_PLUS_16").is_some())
}

fn dump_invalid_x19_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_DUMP_INVALID_X19").is_some())
}

fn trace_r128_pc_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_R128_PC").is_some())
}

fn trace_w_at_vaddr() -> Option<u64> {
    static VALUE: OnceLock<Option<u64>> = OnceLock::new();
    *VALUE.get_or_init(|| parse_u64_env("RUZU_TRACE_W_AT_VADDR"))
}

fn trace_w8_heap_tag_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_W8_HEAP_TAG").is_some())
}

fn trace_count_w64_by_core_at_vaddr() -> Option<u64> {
    static VALUE: OnceLock<Option<u64>> = OnceLock::new();
    *VALUE.get_or_init(|| parse_u64_env("RUZU_COUNT_W64_BY_CORE_AT_VADDR"))
}

fn diverge_page() -> Option<u64> {
    static VALUE: OnceLock<Option<u64>> = OnceLock::new();
    *VALUE.get_or_init(|| parse_u64_env("RUZU_DIVERGE_PAGE").map(|addr| addr & !0xFFFu64))
}

fn trace_w64_at_vaddr() -> Option<TraceWriteAddress> {
    static SPEC: OnceLock<Option<TraceWriteAddress>> = OnceLock::new();
    *SPEC.get_or_init(|| {
        std::env::var("RUZU_TRACE_W64_AT_VADDR")
            .ok()
            .and_then(|raw| parse_trace_write_address(&raw))
    })
}

fn trace_w64_filter_oor_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_W64_FILTER_OOR").is_some())
}

fn trace_w64_value() -> Option<u64> {
    static VALUE: OnceLock<Option<u64>> = OnceLock::new();
    *VALUE.get_or_init(|| parse_u64_env("RUZU_TRACE_W64_VALUE"))
}

fn trace_w32_value() -> Option<u32> {
    static VALUE: OnceLock<Option<u32>> = OnceLock::new();
    *VALUE.get_or_init(|| parse_u64_env("RUZU_TRACE_W32_VALUE").map(|value| value as u32))
}

fn dump_v_index() -> usize {
    static VALUE: OnceLock<usize> = OnceLock::new();
    *VALUE.get_or_init(|| {
        std::env::var("RUZU_DUMP_V_INDEX")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(31)
    })
}

fn trace_cas32_pc_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_CAS32_PC").is_some())
}

fn trace_exclusive32_all_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_EXCLUSIVE32_ALL").is_some())
}

fn trace_exclusive32_addr() -> Option<u64> {
    static VALUE: OnceLock<Option<u64>> = OnceLock::new();
    *VALUE.get_or_init(|| parse_u64_env("RUZU_TRACE_EXCLUSIVE32_ADDR"))
}

fn trace_a64_null_fetch_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_A64_NULL_FETCH").is_some())
}

fn trace_a64_access_pc() -> Option<u64> {
    use std::sync::OnceLock;
    static PC: OnceLock<Option<u64>> = OnceLock::new();
    *PC.get_or_init(|| parse_u64_env("RUZU_TRACE_A64_ACCESS_PC"))
}

/// Parse `RUZU_DUMP_VEC_AT_PC=PC[,PC,...]` (hex). On every memory read,
/// if the JIT PC matches one of the listed PCs we dump V17/V18 (full
/// 128-bit) plus X3/X5 to stderr. Used to inspect strchr-style
/// vectorized scan state at loop boundaries (LD1 at 0x80E3C6A4 /
/// 0x80E3C6F0 — V17/V18/X5 carry over from the previous iteration's
/// ADDP/UMOV chain since the load only writes V1/V2).
fn dump_vec_at_pcs() -> &'static [u64] {
    use std::sync::OnceLock;
    static PCS: OnceLock<Vec<u64>> = OnceLock::new();
    PCS.get_or_init(|| {
        let Ok(raw) = std::env::var("RUZU_DUMP_VEC_AT_PC") else {
            return Vec::new();
        };
        raw.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .filter_map(|s| {
                let s = s
                    .strip_prefix("0x")
                    .or_else(|| s.strip_prefix("0X"))
                    .unwrap_or(s);
                u64::from_str_radix(s, 16).ok()
            })
            .collect()
    })
    .as_slice()
}

#[inline(always)]
fn dump_vec_at_pc(cb: &DynarmicCallbacks64) {
    let pcs = dump_vec_at_pcs();
    if pcs.is_empty() {
        return;
    }
    let Some(pc_ptr) = cb.jit_pc_ptr else {
        return;
    };
    let jit_state_ptr =
        unsafe { (pc_ptr as *const u8).sub(A64JitState::offset_of_pc()) as *const A64JitState };
    let jit_state = unsafe { &*jit_state_ptr };
    if !pcs.contains(&jit_state.pc) {
        return;
    }
    // V<i> = (vec[2i], vec[2i+1]) — low/high 64 bits.
    let vlo = |i: usize| jit_state.vec[2 * i];
    let vhi = |i: usize| jit_state.vec[2 * i + 1];
    let x3 = jit_state.reg[3];
    let x5 = jit_state.reg[5];
    eprintln!(
        "[VEC_DUMP] pc=0x{:016X} v3=0x{:016X}{:016X} v4=0x{:016X}{:016X} v7=0x{:016X}{:016X} v16=0x{:016X}{:016X} v17=0x{:016X}{:016X} v18=0x{:016X}{:016X} x3=0x{:016X} x5=0x{:016X}",
        jit_state.pc,
        vhi(3), vlo(3),
        vhi(4), vlo(4),
        vhi(7), vlo(7),
        vhi(16), vlo(16),
        vhi(17), vlo(17),
        vhi(18), vlo(18),
        x3, x5
    );
}

fn trace_a64_access_budget() -> &'static AtomicU32 {
    use std::sync::OnceLock;
    static BUDGET: OnceLock<AtomicU32> = OnceLock::new();
    BUDGET.get_or_init(|| {
        AtomicU32::new(
            parse_u64_env("RUZU_TRACE_A64_ACCESS_LIMIT")
                .unwrap_or(128)
                .min(u32::MAX as u64) as u32,
        )
    })
}

fn trace_a64_svc_regs_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_A64_SVC_REGS").is_some())
}

#[inline(always)]
fn trace_a64_access_64(cb: &DynarmicCallbacks64, kind: &str, vaddr: u64, size: u64, value: u128) {
    let Some(target_pc) = trace_a64_access_pc() else {
        return;
    };
    let (pc, lr, x0, x1, x4, x5, x6, x7, x8, x19, x20, x21, x22, x25) = cb.watch_context();
    if pc != target_pc {
        return;
    }

    let budget = trace_a64_access_budget();
    let remaining = budget.load(Ordering::Relaxed);
    if remaining == 0 {
        return;
    }
    budget.fetch_sub(1, Ordering::Relaxed);

    eprintln!(
        "[A64_TRACE_ACCESS_{kind}] pc=0x{pc:016X} lr=0x{lr:016X} x0=0x{x0:016X} x1=0x{x1:016X} x4=0x{x4:016X} x5=0x{x5:016X} x6=0x{x6:016X} x7=0x{x7:016X} x8=0x{x8:016X} x19=0x{x19:016X} x20=0x{x20:016X} x21=0x{x21:016X} x22=0x{x22:016X} x25=0x{x25:016X} vaddr=0x{vaddr:016X} size={size} value=0x{value:032X}"
    );
}

#[inline(always)]
fn watch_read_64(cb: &DynarmicCallbacks64, vaddr: u64, size: u64, value: u128) {
    let ranges = watched_ranges_64();
    if ranges.is_empty() {
        return;
    }
    let end = vaddr.saturating_add(size);
    if !ranges.iter().any(|(s, e)| vaddr < *e && end > *s) {
        return;
    }
    let (pc, lr, _, _, _, _, _, _, _, _, _, _, _, _) = cb.watch_context();
    common::trace::emit_raw(
        common::trace::cat::WATCH_READ,
        &[
            cb.core_index as u64,
            0,
            pc,
            lr,
            vaddr,
            size,
            value as u64,
            (value >> 64) as u64,
        ],
    );
}

#[inline(always)]
fn trace_unmapped_read_64(cb: &DynarmicCallbacks64, vaddr: u64, size: u64) {
    if !common::trace::is_enabled(common::trace::cat::UNMAPPED_READ) {
        return;
    }
    let valid = if let Some(ref cm) = cb.core_memory {
        cm.lock()
            .unwrap()
            .is_valid_virtual_address_range(vaddr, size)
    } else {
        cb.memory
            .read()
            .unwrap()
            .is_valid_range(vaddr, size as usize)
    };
    if valid {
        return;
    }
    let (pc, lr, x0, _, _, _, _, _, x8, x19, x20, x21, x22, x25) = cb.watch_context();
    let x9 = if let Some(pc_ptr) = cb.jit_pc_ptr {
        let jit_state_ptr =
            unsafe { (pc_ptr as *const u8).sub(A64JitState::offset_of_pc()) as *const A64JitState };
        unsafe { (&*jit_state_ptr).reg[9] }
    } else {
        0
    };
    common::trace::emit_raw(
        common::trace::cat::UNMAPPED_READ,
        &[
            cb.core_index as u64,
            pc,
            lr,
            vaddr,
            size,
            x0,
            x8,
            x9,
            x19,
            x20,
            x21,
            x22,
            x25,
        ],
    );
    if common::trace::is_enabled(common::trace::cat::A64_EXCEPTION_CTX) {
        let dump_base = x19.wrapping_add(0x50);
        let qwords = if let Some(ref cm) = cb.core_memory {
            let mem = cm.lock().unwrap();
            if mem.is_valid_virtual_address_range(dump_base, 0x20) {
                Some([
                    mem.read_64(dump_base),
                    mem.read_64(dump_base + 8),
                    mem.read_64(dump_base + 16),
                    mem.read_64(dump_base + 24),
                ])
            } else {
                None
            }
        } else {
            let mem = cb.memory.read().unwrap();
            if mem.is_valid_range(dump_base, 0x20) {
                Some([
                    mem.read_64(dump_base),
                    mem.read_64(dump_base + 8),
                    mem.read_64(dump_base + 16),
                    mem.read_64(dump_base + 24),
                ])
            } else {
                None
            }
        };
        if let Some(qwords) = qwords {
            common::trace::emit_raw(
                common::trace::cat::A64_EXCEPTION_CTX,
                &[1, dump_base, qwords[0], qwords[1], qwords[2], qwords[3]],
            );
        }
    }
}

#[inline(always)]
fn watch_write_64(cb: &DynarmicCallbacks64, vaddr: u64, size: u64, value: u128) {
    let ranges = watched_ranges_64();
    if ranges.is_empty() {
        return;
    }
    let end = vaddr.saturating_add(size);
    if !ranges.iter().any(|(s, e)| vaddr < *e && end > *s) {
        return;
    }
    let (pc, lr, _, _, _, _, _, _, _, _, _, _, _, _) = cb.watch_context();
    common::trace::emit_raw(
        common::trace::cat::WATCH_WRITE,
        &[
            cb.core_index as u64,
            0,
            pc,
            lr,
            vaddr,
            size,
            value as u64,
            (value >> 64) as u64,
        ],
    );
}

#[inline(always)]
fn trace_unmapped_write_64(cb: &DynarmicCallbacks64, vaddr: u64, size: u64, value: u128) {
    if !common::trace::is_enabled(common::trace::cat::UNMAPPED_WRITE) {
        return;
    }

    let valid = if let Some(ref cm) = cb.core_memory {
        cm.lock()
            .unwrap()
            .is_valid_virtual_address_range(vaddr, size)
    } else {
        cb.memory
            .read()
            .unwrap()
            .is_valid_range(vaddr, size as usize)
    };
    if valid {
        return;
    }

    static TRACE_UNMAPPED_WRITE_ADDR: OnceLock<Option<u64>> = OnceLock::new();
    if let Some(target) =
        *TRACE_UNMAPPED_WRITE_ADDR.get_or_init(|| parse_u64_env("RUZU_TRACE_UNMAPPED_WRITE_ADDR"))
    {
        let end = vaddr.saturating_add(size);
        if !(vaddr <= target && target < end) {
            return;
        }
    }

    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::OnceLock;
    static LIMIT: OnceLock<u32> = OnceLock::new();
    static SHOWN: AtomicU32 = AtomicU32::new(0);
    let limit = *LIMIT.get_or_init(|| {
        std::env::var("RUZU_TRACE_UNMAPPED_WRITE_LIMIT")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(64)
    });
    if SHOWN.fetch_add(1, Ordering::Relaxed) >= limit {
        return;
    }

    let tid = crate::hle::kernel::kernel::get_current_thread_id_fast().unwrap_or(0);
    let (pc, lr, _, _, _, _, _, _, _, _, _, _, _, _) = cb.watch_context();
    common::trace::emit_raw(
        common::trace::cat::UNMAPPED_WRITE,
        &[tid, pc, lr, vaddr, size, value as u64, (value >> 64) as u64],
    );
}

/// Translate rdynarmic's HaltReason to core's HaltReason.
///
/// rdynarmic uses its own compact halt reason bits:
///   STEP=1, SVC=2, BREAKPOINT=4, EXCEPTION_RAISED=8,
///   CACHE_INVALIDATION=16, EXTERNAL_HALT=32
///
/// Corresponds to upstream `Core::TranslateHaltReason`.
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
    if hr.contains(rdynarmic::halt_reason::HaltReason::PREFETCH_ABORT) {
        result |= HaltReason::PREFETCH_ABORT;
    }
    if hr.contains(rdynarmic::halt_reason::HaltReason::EXTERNAL_HALT) {
        result |= HaltReason::BREAK_LOOP;
    }

    result
}

/// Per-emulator-core atomic counters for `RUZU_COUNT_W64_BY_CORE_AT_VADDR`.
/// 16 slots covers any reasonable core count. Indexed by `core_index`.
static W64_BY_CORE_COUNTERS: [std::sync::atomic::AtomicU64; 16] = [
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
];

/// Print the per-core W64 counter snapshot. Async-signal-safe (uses
/// `eprintln!` which acquires stdout lock — fine outside the signal handler).
/// Call from a SIGUSR2 handler to dump cumulative counts.
pub fn dump_w64_by_core_counters() {
    eprintln!("{}", w64_by_core_summary_string());
}

/// String form of the per-core W64 counter snapshot. Useful for log files
/// when stderr is being flooded by other emitters.
pub fn w64_by_core_summary_string() -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(256);
    out.push_str("[W64_BY_CORE_SUMMARY] hits per emulator core:");
    let mut any = false;
    for (i, c) in W64_BY_CORE_COUNTERS.iter().enumerate() {
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

/// JIT callbacks for ARM64.
///
/// Corresponds to upstream `DynarmicCallbacks64`.
///
/// Holds a shared reference to guest process memory, matching upstream's
/// `Core::Memory::Memory& m_memory` obtained from `process->GetMemory()`.
struct DynarmicCallbacks64 {
    /// Shared guest memory reference (ProcessMemoryData fallback).
    memory: SharedProcessMemory,
    /// Core::Memory::Memory bridge (reads/writes via PageTable → DeviceMemory).
    /// Matches upstream `Core::Memory::Memory& m_memory`.
    core_memory: Option<Arc<std::sync::Mutex<Memory>>>,
    /// SVC number from last supervisor call, shared with parent ArmDynarmic64.
    /// Upstream: callback writes to m_parent.m_svc via back-reference.
    svc: Arc<AtomicU32>,
    /// Whether wall clock is used (if true, ticking is disabled).
    /// Matches upstream `m_parent.m_uses_wall_clock`.
    uses_wall_clock: bool,
    /// Core timing reference for tick management.
    /// Matches upstream `m_parent.m_system.CoreTiming()`.
    core_timing: Arc<crate::core_timing::CoreTiming>,
    /// Last exception address reported by dynarmic.
    last_exception_address: Arc<AtomicU64>,
    /// Saved guest context for upstream-style `ReturnException(pc, hr)`.
    breakpoint_context: Arc<Mutex<ThreadContext>>,
    /// Shared exclusive monitor backing Dynarmic's global monitor state.
    exclusive_monitor:
        *mut crate::arm::dynarmic::dynarmic_exclusive_monitor::DynarmicExclusiveMonitor,
    /// CPU core index associated with this callback/JIT instance.
    core_index: usize,
    /// Upstream: `const bool m_debugger_enabled`.
    debugger_enabled: bool,
    /// Upstream: `const bool m_check_memory_access`.
    check_memory_access: bool,
    /// Shared view of `ArmInterface::m_watchpoints` used by the moved callback.
    watchpoints: SharedWatchpointArray,
    /// Shared counterpart of `ArmDynarmic64::m_halted_watchpoint`.
    halted_watchpoint: Arc<Mutex<Option<DebugWatchpoint>>>,
    /// Raw pointer to rdynarmic's `halt_reason` field.
    /// Set after JIT creation via `set_halt_reason_ptr()`.
    halt_reason_ptr: Option<*const AtomicU32>,
    /// Raw pointer to `A64JitState::pc` for diagnostic logging.
    /// Set after JIT creation via `set_pc_ptr()`.
    jit_pc_ptr: Option<*const u64>,
}

// Safety: exclusive_monitor points to process-owned state that outlives the callback/JIT.
// Each callback is still used by only one JIT/core.
unsafe impl Send for DynarmicCallbacks64 {}

impl DynarmicCallbacks64 {
    fn new(
        memory: SharedProcessMemory,
        core_memory: Option<Arc<std::sync::Mutex<Memory>>>,
        svc: Arc<AtomicU32>,
        uses_wall_clock: bool,
        core_timing: Arc<crate::core_timing::CoreTiming>,
        last_exception_address: Arc<AtomicU64>,
        breakpoint_context: Arc<Mutex<ThreadContext>>,
        exclusive_monitor: *mut crate::arm::dynarmic::dynarmic_exclusive_monitor::DynarmicExclusiveMonitor,
        core_index: usize,
        debugger_enabled: bool,
        watchpoints: SharedWatchpointArray,
        halted_watchpoint: Arc<Mutex<Option<DebugWatchpoint>>>,
    ) -> Self {
        Self {
            memory,
            core_memory,
            svc,
            uses_wall_clock,
            core_timing,
            last_exception_address,
            breakpoint_context,
            exclusive_monitor,
            core_index,
            debugger_enabled,
            check_memory_access: debugger_enabled
                || !*common::settings::values()
                    .cpuopt_ignore_memory_aborts
                    .get_value(),
            watchpoints,
            halted_watchpoint,
            halt_reason_ptr: None,
            jit_pc_ptr: None,
        }
    }

    /// Rust equivalent of upstream `m_parent.m_jit->HaltExecution(hr)`.
    fn halt_execution(&self, reason: rdynarmic::halt_reason::HaltReason) {
        if let Some(ptr) = self.halt_reason_ptr {
            unsafe { (&*ptr).fetch_or(reason.bits(), Ordering::SeqCst) };
        }
    }

    fn watch_context(
        &self,
    ) -> (
        u64,
        u64,
        u64,
        u64,
        u64,
        u64,
        u64,
        u64,
        u64,
        u64,
        u64,
        u64,
        u64,
        u64,
    ) {
        let Some(pc_ptr) = self.jit_pc_ptr else {
            return (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0);
        };
        let jit_state_ptr =
            unsafe { (pc_ptr as *const u8).sub(A64JitState::offset_of_pc()) as *const A64JitState };
        let jit_state = unsafe { &*jit_state_ptr };
        (
            jit_state.pc,
            jit_state.reg[30],
            jit_state.reg[0],
            jit_state.reg[1],
            jit_state.reg[4],
            jit_state.reg[5],
            jit_state.reg[6],
            jit_state.reg[7],
            jit_state.reg[8],
            jit_state.reg[19],
            jit_state.reg[20],
            jit_state.reg[21],
            jit_state.reg[22],
            jit_state.reg[25],
        )
    }

    fn snapshot_context(&self, pc: u64) {
        let Some(pc_ptr) = self.jit_pc_ptr else {
            return;
        };
        let jit_state_ptr =
            unsafe { (pc_ptr as *const u8).sub(A64JitState::offset_of_pc()) as *const A64JitState };
        let jit_state = unsafe { &*jit_state_ptr };
        let mut ctx = self.breakpoint_context.lock().unwrap();
        *ctx = thread_context_from_jit_state(jit_state, pc);
    }

    fn trace_interpreter_fallback(&self, pc: u64, instr: u32, num_instructions: usize) {
        if !common::trace::is_enabled(common::trace::cat::A64_EXCEPTION_CTX) {
            return;
        }
        let Some(pc_ptr) = self.jit_pc_ptr else {
            return;
        };
        let jit_state_ptr =
            unsafe { (pc_ptr as *const u8).sub(A64JitState::offset_of_pc()) as *const A64JitState };
        let jit_state = unsafe { &*jit_state_ptr };
        common::trace::emit_raw(
            common::trace::cat::A64_EXCEPTION_CTX,
            &[
                4,
                self.core_index as u64,
                pc,
                instr as u64,
                num_instructions as u64,
                jit_state.pc,
                jit_state.reg[30],
                jit_state.sp,
                jit_state.reg[0],
                jit_state.reg[8],
                jit_state.reg[19],
                jit_state.reg[20],
                jit_state.reg[21],
                jit_state.reg[23],
            ],
        );
    }

    /// Matches upstream `DynarmicCallbacks64::CheckMemoryAccess`.
    ///
    /// Upstream behavior: `m_check_memory_access` is only true when
    /// `debugger_enabled || !cpuopt_ignore_memory_aborts`. The default is
    /// `cpuopt_ignore_memory_aborts = true`, so `m_check_memory_access = false`,
    /// meaning this function returns true immediately without checking.
    ///
    /// Memory access validation is a debugger feature, not used in normal play.
    /// The JIT uses page table fastmem for actual memory protection.
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

impl UserCallbacks for DynarmicCallbacks64 {
    fn memory_read_code(&self, vaddr: u64) -> Option<u32> {
        if vaddr == 0
            && trace_a64_null_fetch_enabled()
            && common::trace::is_enabled(common::trace::cat::A64_EXCEPTION_CTX)
        {
            if let Some(pc_ptr) = self.jit_pc_ptr {
                let jit_state_ptr = unsafe {
                    (pc_ptr as *const u8).sub(A64JitState::offset_of_pc()) as *const A64JitState
                };
                let jit_state = unsafe { &*jit_state_ptr };
                common::trace::emit_raw(
                    common::trace::cat::A64_EXCEPTION_CTX,
                    &[
                        5,
                        self.core_index as u64,
                        vaddr,
                        jit_state.pc,
                        jit_state.reg[30],
                        jit_state.sp,
                        jit_state.reg[0],
                        jit_state.reg[1],
                        jit_state.reg[2],
                        jit_state.reg[3],
                        jit_state.reg[4],
                        jit_state.reg[5],
                        jit_state.reg[19],
                        jit_state.reg[20],
                    ],
                );
            } else {
                common::trace::emit_raw(
                    common::trace::cat::A64_EXCEPTION_CTX,
                    &[5, self.core_index as u64, vaddr],
                );
            }
        }
        // Upstream: returns std::nullopt if IsValidVirtualAddressRange fails.
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

    fn memory_read_8(&self, vaddr: u64) -> u8 {
        self.check_memory_access(vaddr, 1, DebugWatchpointType::READ);
        trace_unmapped_read_64(self, vaddr, 1);
        let value = if let Some(ref cm) = self.core_memory {
            cm.lock().unwrap().read_8(vaddr)
        } else {
            self.memory.read().unwrap().read_8(vaddr)
        };
        watch_read_64(self, vaddr, 1, value as u128);
        trace_a64_access_64(self, "READ", vaddr, 1, value as u128);
        value
    }

    fn memory_read_16(&self, vaddr: u64) -> u16 {
        self.check_memory_access(vaddr, 2, DebugWatchpointType::READ);
        trace_unmapped_read_64(self, vaddr, 2);
        let value = if let Some(ref cm) = self.core_memory {
            cm.lock().unwrap().read_16(vaddr)
        } else {
            self.memory.read().unwrap().read_16(vaddr)
        };
        watch_read_64(self, vaddr, 2, value as u128);
        trace_a64_access_64(self, "READ", vaddr, 2, value as u128);
        value
    }

    fn memory_read_32(&self, vaddr: u64) -> u32 {
        self.check_memory_access(vaddr, 4, DebugWatchpointType::READ);
        trace_unmapped_read_64(self, vaddr, 4);
        let value = if let Some(ref cm) = self.core_memory {
            cm.lock().unwrap().read_32(vaddr)
        } else {
            self.memory.read().unwrap().read_32(vaddr)
        };
        watch_read_64(self, vaddr, 4, value as u128);
        trace_a64_access_64(self, "READ", vaddr, 4, value as u128);
        value
    }

    fn memory_read_64(&self, vaddr: u64) -> u64 {
        self.check_memory_access(vaddr, 8, DebugWatchpointType::READ);
        trace_unmapped_read_64(self, vaddr, 8);
        // RUZU_SNAPSHOT_VADDR=0xADDR — on every slow-path Read64, also
        // probe [ADDR] via slow path and log when its value first
        // changes from the initial reading. Used to bisect when a
        // mutating write first corrupts a tracked location, since
        // direct-fastmem stores are invisible to write callbacks.
        if let Some(target) = snapshot_vaddr() {
            use std::sync::atomic::{AtomicU64, Ordering};
            static LAST: AtomicU64 = AtomicU64::new(u64::MAX);
            static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let cur = if let Some(ref cm) = self.core_memory {
                let m = cm.lock().unwrap();
                if m.is_valid_virtual_address_range(target, 8) {
                    m.read_64(target)
                } else {
                    u64::MAX
                }
            } else {
                let m = self.memory.read().unwrap();
                if m.is_valid_range(target, 8) {
                    m.read_64(target)
                } else {
                    u64::MAX
                }
            };
            let prev = LAST.swap(cur, Ordering::Relaxed);
            // Only log on TRANSITION from "valid heap addr" (top 24 bits zero,
            // i.e. value <= 0xFF_FFFF_FFFF) to "out-of-range" (top 24 bits set),
            // OR on the very first time the value crosses out-of-range.
            let prev_oor = prev != u64::MAX && prev > 0xFF_FFFF_FFFFu64;
            let cur_oor = cur != u64::MAX && cur > 0xFF_FFFF_FFFFu64;
            let log_now = prev != cur && prev != u64::MAX && (cur_oor != prev_oor || cur_oor);
            // ALSO compare against fastmem-arena read for this same vaddr.
            let arena_value: u64 = if let Some(ref cm) = self.core_memory {
                let arena = cm.lock().unwrap().fastmem_pointer();
                if !arena.is_null() {
                    unsafe { std::ptr::read_unaligned(arena.add(target as usize) as *const u64) }
                } else {
                    0
                }
            } else {
                0
            };
            let arena_differs_from_slow = arena_value != cur && cur != u64::MAX;
            if log_now || arena_differs_from_slow {
                let n = SHOWN.fetch_add(1, Ordering::Relaxed);
                if n < 1024 {
                    if let Some(pc_ptr) = self.jit_pc_ptr {
                        let jit_state_ptr = unsafe {
                            (pc_ptr as *const u8).sub(A64JitState::offset_of_pc())
                                as *const A64JitState
                        };
                        let s = unsafe { &*jit_state_ptr };
                        eprintln!(
                                "[SNAPSHOT #{}] [0x{:016X}] slow=0x{:016X} arena=0x{:016X} differ={} prev=0x{:016X} (around pc=0x{:016X} lr=0x{:016X} via Read64@0x{:X})",
                                n, target, cur, arena_value, arena_differs_from_slow, prev, s.pc, s.reg[30], vaddr
                            );
                    }
                }
            }
        }
        dump_vec_at_pc(self);
        // RUZU_TRACE_R64_AT_VADDR=0xADDR/0xMASK — log on every Read64 whose
        // (vaddr & MASK) == (ADDR & MASK). Used for catching tagged-pointer
        // / out-of-range accesses (e.g. STK reads at 0x00ff_0000_0000_xxxx).
        if let Some(spec) = trace_r64_at_vaddr() {
            if (vaddr & spec.mask) == (spec.target & spec.mask) {
                use std::sync::atomic::{AtomicU32, Ordering};
                static SHOWN: AtomicU32 = AtomicU32::new(0);
                let n = SHOWN.fetch_add(1, Ordering::Relaxed);
                if n < 16 {
                    if let Some(pc_ptr) = self.jit_pc_ptr {
                        let jit_state_ptr = unsafe {
                            (pc_ptr as *const u8).sub(A64JitState::offset_of_pc())
                                as *const A64JitState
                        };
                        let s = unsafe { &*jit_state_ptr };
                        eprintln!(
                            "[R64_AT_VADDR #{}] vaddr=0x{:016X} pc=0x{:016X} lr=0x{:016X} sp=0x{:016X} \
x0=0x{:016X} x1=0x{:016X} x2=0x{:016X} x19=0x{:016X} x20=0x{:016X} x21=0x{:016X} x22=0x{:016X}",
                            n, vaddr, s.pc, s.reg[30], s.sp,
                            s.reg[0], s.reg[1], s.reg[2],
                            s.reg[19], s.reg[20], s.reg[21], s.reg[22],
                        );
                    }
                }
            }
        }
        // RUZU_TRACE_R64_VALUE=0xVAL — log on Read64 that RETURNS the
        // target value. Inverse of W64_VALUE; used to track where a
        // sentinel value enters guest registers from memory.
        if let Some(target) = trace_r64_value() {
            let value_pre = if let Some(ref cm) = self.core_memory {
                cm.lock().unwrap().read_64(vaddr)
            } else {
                self.memory.read().unwrap().read_64(vaddr)
            };
            if value_pre == target {
                use std::sync::atomic::{AtomicU32, Ordering};
                static SHOWN: AtomicU32 = AtomicU32::new(0);
                let n = SHOWN.fetch_add(1, Ordering::Relaxed);
                if n < 8 {
                    if let Some(pc_ptr) = self.jit_pc_ptr {
                        let jit_state_ptr = unsafe {
                            (pc_ptr as *const u8).sub(A64JitState::offset_of_pc())
                                as *const A64JitState
                        };
                        let s = unsafe { &*jit_state_ptr };
                        eprintln!(
                            "[R64_VALUE #{}] vaddr=0x{:016X} value=0x{:016X} pc=0x{:016X} lr=0x{:016X} sp=0x{:016X} \
x0=0x{:016X} x1=0x{:016X} x2=0x{:016X} x19=0x{:016X} x20=0x{:016X} x21=0x{:016X} x22=0x{:016X}",
                            n, vaddr, value_pre, s.pc, s.reg[30], s.sp,
                            s.reg[0], s.reg[1], s.reg[2],
                            s.reg[19], s.reg[20], s.reg[21], s.reg[22],
                        );
                    }
                }
            }
        }
        if trace_a64_invalid_data_read_enabled() {
            let is_valid = if let Some(ref cm) = self.core_memory {
                cm.lock().unwrap().is_valid_virtual_address_range(vaddr, 8)
            } else {
                self.memory.read().unwrap().is_valid_range(vaddr, 8)
            };
            if !is_valid || vaddr < 0x1000 {
                static INVALID_READ64_SHOWN: AtomicU32 = AtomicU32::new(0);
                let invalid_log_index = INVALID_READ64_SHOWN.fetch_add(1, Ordering::Relaxed);
                if invalid_log_index >= 32 {
                    return 0;
                }
                if let Some(pc_ptr) = self.jit_pc_ptr {
                    let jit_state_ptr = unsafe {
                        (pc_ptr as *const u8).sub(A64JitState::offset_of_pc()) as *const A64JitState
                    };
                    let jit_state = unsafe { &*jit_state_ptr };
                    log::error!(
                        "[A64_INVALID_READ64 #{}] core={} vaddr=0x{:016X} pc=0x{:016X} lr=0x{:016X} sp=0x{:016X} \
x29=0x{:016X} x0=0x{:016X} x1=0x{:016X} x2=0x{:016X} x3=0x{:016X} x4=0x{:016X} x5=0x{:016X} \
x6=0x{:016X} x7=0x{:016X} x8=0x{:016X} x9=0x{:016X} x10=0x{:016X} x11=0x{:016X} x12=0x{:016X} \
x19=0x{:016X} x20=0x{:016X} x21=0x{:016X} x22=0x{:016X} x25=0x{:016X}",
                        invalid_log_index,
                        self.core_index,
                        vaddr,
                        jit_state.pc,
                        jit_state.reg[30],
                        jit_state.sp,
                        jit_state.reg[29],
                        jit_state.reg[0],
                        jit_state.reg[1],
                        jit_state.reg[2],
                        jit_state.reg[3],
                        jit_state.reg[4],
                        jit_state.reg[5],
                        jit_state.reg[6],
                        jit_state.reg[7],
                        jit_state.reg[8],
                        jit_state.reg[9],
                        jit_state.reg[10],
                        jit_state.reg[11],
                        jit_state.reg[12],
                        jit_state.reg[19],
                        jit_state.reg[20],
                        jit_state.reg[21],
                        jit_state.reg[22],
                        jit_state.reg[25],
                    );
                    if let Some((addr, size)) = dump_on_invalid_spec() {
                        use std::sync::atomic::{AtomicU32, Ordering};
                        static DUMP_COUNT: AtomicU32 = AtomicU32::new(0);
                        let n = DUMP_COUNT.fetch_add(1, Ordering::Relaxed);
                        if n < 4 {
                            let mut slow = Vec::with_capacity(size);
                            let mut arena = Vec::with_capacity(size);
                            for off in 0..size {
                                let byte_addr = addr + off as u64;
                                let slow_byte = if let Some(ref cm) = self.core_memory {
                                    let m = cm.lock().unwrap();
                                    if m.is_valid_virtual_address_range(byte_addr, 1) {
                                        m.read_8(byte_addr)
                                    } else {
                                        0xff
                                    }
                                } else {
                                    let m = self.memory.read().unwrap();
                                    if m.is_valid_range(byte_addr, 1) {
                                        m.read_8(byte_addr)
                                    } else {
                                        0xff
                                    }
                                };
                                let arena_byte = if let Some(ref cm) = self.core_memory {
                                    let arena_ptr = cm.lock().unwrap().fastmem_pointer();
                                    if !arena_ptr.is_null() {
                                        unsafe { *arena_ptr.add(byte_addr as usize) }
                                    } else {
                                        0xff
                                    }
                                } else {
                                    0xff
                                };
                                slow.push(slow_byte);
                                arena.push(arena_byte);
                            }
                            let slow_hex = slow
                                .iter()
                                .map(|b| format!("{:02x}", b))
                                .collect::<Vec<_>>()
                                .join(" ");
                            let arena_hex = arena
                                .iter()
                                .map(|b| format!("{:02x}", b))
                                .collect::<Vec<_>>()
                                .join(" ");
                            eprintln!(
                                "[DUMP_ON_INVALID #{}] addr=0x{:016X} size={} slow={} arena={} differ={}",
                                n,
                                addr,
                                size,
                                slow_hex,
                                arena_hex,
                                slow != arena
                            );
                        }
                    }
                    if dump_x22_plus_16_enabled() {
                        // Read [x22+0x10] via the slow path to see what
                        // the stored value really is — for diagnosing the
                        // STK fastmem-coherency wedge where the JIT reads
                        // a corrupt free-list head pointer at this addr.
                        let addr = jit_state.reg[22].wrapping_add(0x10);
                        let valid = if let Some(ref cm) = self.core_memory {
                            cm.lock().unwrap().is_valid_virtual_address_range(addr, 8)
                        } else {
                            self.memory.read().unwrap().is_valid_range(addr, 8)
                        };
                        if valid {
                            let v = if let Some(ref cm) = self.core_memory {
                                cm.lock().unwrap().read_64(addr)
                            } else {
                                self.memory.read().unwrap().read_64(addr)
                            };
                            eprintln!(
                                "[A64_INVALID_READ64_X22_16] [0x{:016X}]=0x{:016X} (slow-path read of free-list head; compare to JIT's x19=0x{:016X})",
                                addr, v, jit_state.reg[19]
                            );
                        }
                    }
                    if dump_invalid_x19_enabled() {
                        let base = jit_state.reg[19];
                        let mut bytes = Vec::with_capacity(96);
                        for i in 0..96u64 {
                            let addr = base + i;
                            let valid = if let Some(ref cm) = self.core_memory {
                                cm.lock().unwrap().is_valid_virtual_address_range(addr, 1)
                            } else {
                                self.memory.read().unwrap().is_valid_range(addr, 1)
                            };
                            if !valid {
                                break;
                            }
                            let b = if let Some(ref cm) = self.core_memory {
                                cm.lock().unwrap().read_8(addr)
                            } else {
                                self.memory.read().unwrap().read_8(addr)
                            };
                            bytes.push(b);
                        }
                        let ascii: String = bytes
                            .iter()
                            .map(|&b| {
                                if (0x20..=0x7e).contains(&b) {
                                    b as char
                                } else {
                                    '.'
                                }
                            })
                            .collect();
                        eprintln!(
                            "[A64_INVALID_READ64_X19_BYTES] addr=0x{base:016X} bytes={bytes:02X?}"
                        );
                        eprintln!("[A64_INVALID_READ64_X19_ASCII] {ascii}");
                    }
                } else {
                    eprintln!(
                        "[A64_INVALID_READ64] vaddr=0x{:016X} jit_state=<unavailable>",
                        vaddr
                    );
                }
            }
        }
        let value = if let Some(ref cm) = self.core_memory {
            cm.lock().unwrap().read_64(vaddr)
        } else {
            self.memory.read().unwrap().read_64(vaddr)
        };
        watch_read_64(self, vaddr, 8, value as u128);
        trace_a64_access_64(self, "READ", vaddr, 8, value as u128);
        value
    }

    fn memory_read_128(&self, vaddr: u64) -> (u64, u64) {
        self.check_memory_access(vaddr, 16, DebugWatchpointType::READ);
        // RUZU_DUMP_VEC_AT_PC=PC[,PC,...] — for vectorized strchr/strpbrk
        // scans, dump V17/V18/X3/X5 at LD1 entry. The reduction state
        // from the previous iteration is still live in V17/V18/X5
        // because LD1 only writes V1/V2.
        dump_vec_at_pc(self);
        // RUZU_TRACE_R128_PC=1 — log the JIT PC + key registers on the
        // FIRST few 128-bit reads. Used to identify the guest code that
        // emits a long sequential vector-load scan (e.g. STK's STK
        // unmapped-hole linear scan starting at 0x815E6000).
        if trace_r128_pc_enabled() {
            use std::sync::atomic::{AtomicU32, Ordering};
            static SHOWN: AtomicU32 = AtomicU32::new(0);
            // Trace reads at the known scan-loop PC 0x80E3B720, but only the
            // first 16 to keep noise low. Drop the per-vaddr filter so we
            // see the very first iteration including its starting x1.
            let pc_match = if let Some(pc_ptr) = self.jit_pc_ptr {
                let p = unsafe {
                    (pc_ptr as *const u8).sub(A64JitState::offset_of_pc()) as *const A64JitState
                };
                let live_pc = unsafe { (*p).pc };
                live_pc == 0x80E3B720
            } else {
                false
            };
            let n = if pc_match {
                SHOWN.fetch_add(1, Ordering::Relaxed)
            } else {
                u32::MAX
            };
            if n < 16 {
                if let Some(pc_ptr) = self.jit_pc_ptr {
                    let jit_state_ptr = unsafe {
                        (pc_ptr as *const u8).sub(A64JitState::offset_of_pc()) as *const A64JitState
                    };
                    let s = unsafe { &*jit_state_ptr };
                    eprintln!(
                        "[R128 #{}] vaddr=0x{:016X} pc=0x{:016X} lr=0x{:016X} sp=0x{:016X} \
x0=0x{:016X} x1=0x{:016X} x2=0x{:016X} x3=0x{:016X} x19=0x{:016X} x20=0x{:016X} x21=0x{:016X} x22=0x{:016X} x29=0x{:016X}",
                        n, vaddr, s.pc, s.reg[30], s.sp,
                        s.reg[0], s.reg[1], s.reg[2], s.reg[3],
                        s.reg[19], s.reg[20], s.reg[21], s.reg[22], s.reg[29],
                    );
                }
            }
        }
        let value = if let Some(ref cm) = self.core_memory {
            let m = cm.lock().unwrap();
            (m.read_64(vaddr), m.read_64(vaddr + 8))
        } else {
            let mem = self.memory.read().unwrap();
            (mem.read_64(vaddr), mem.read_64(vaddr + 8))
        };
        watch_read_64(
            self,
            vaddr,
            16,
            ((value.1 as u128) << 64) | (value.0 as u128),
        );
        trace_a64_access_64(
            self,
            "READ",
            vaddr,
            16,
            ((value.1 as u128) << 64) | (value.0 as u128),
        );
        value
    }

    fn memory_write_8(&mut self, vaddr: u64, value: u8) {
        if !self.check_memory_access(vaddr, 1, DebugWatchpointType::WRITE) {
            return;
        }
        trace_unmapped_write_64(self, vaddr, 1, value as u128);
        if trace_w_at_vaddr().is_some_and(|target| vaddr == target) {
            if let Some(pc_ptr) = self.jit_pc_ptr {
                let s = unsafe {
                    &*((pc_ptr as *const u8).sub(A64JitState::offset_of_pc()) as *const A64JitState)
                };
                eprintln!(
                    "[W8_AT] vaddr=0x{:016X} value=0x{:02X} pc=0x{:016X} lr=0x{:016X}",
                    vaddr, value, s.pc, s.reg[30]
                );
            }
        }
        // RUZU_TRACE_W8_HEAP_TAG=1 — log every W8 write whose vaddr is in
        // the heap range (vaddr >> 32 == 0x21) AND whose value has bit 0
        // set (i.e. could be the byte that taints chunk[+16] with the
        // PINUSE-tag pattern producing x4 = 0x814903E1 at the unlink).
        // Use with RUZU_NO_FASTMEM_W8=1 to route all W8 writes through
        // this callback. Throttled to 200 events.
        if trace_w8_heap_tag_enabled() && (vaddr >> 32) == 0x21 && (value & 1) == 1 {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            if n < 200 {
                if let Some(pc_ptr) = self.jit_pc_ptr {
                    let s = unsafe {
                        &*((pc_ptr as *const u8).sub(A64JitState::offset_of_pc())
                            as *const A64JitState)
                    };
                    eprintln!(
                        "[W8_HEAP_TAG #{}] vaddr=0x{:016X} value=0x{:02X} pc=0x{:016X} lr=0x{:016X}",
                        n, vaddr, value, s.pc, s.reg[30]
                    );
                }
            }
        }
        watch_write_64(self, vaddr, 1, value as u128);
        trace_a64_access_64(self, "WRITE", vaddr, 1, value as u128);
        if let Some(ref cm) = self.core_memory {
            cm.lock().unwrap().write_8(vaddr, value);
        } else {
            self.memory.write().unwrap().write_8(vaddr, value);
        }
    }

    fn memory_write_16(&mut self, vaddr: u64, value: u16) {
        if !self.check_memory_access(vaddr, 2, DebugWatchpointType::WRITE) {
            return;
        }
        trace_unmapped_write_64(self, vaddr, 2, value as u128);
        if trace_w_at_vaddr().is_some_and(|target| vaddr == target || vaddr + 1 == target) {
            if let Some(pc_ptr) = self.jit_pc_ptr {
                let s = unsafe {
                    &*((pc_ptr as *const u8).sub(A64JitState::offset_of_pc()) as *const A64JitState)
                };
                eprintln!(
                    "[W16_AT] vaddr=0x{:016X} value=0x{:04X} pc=0x{:016X} lr=0x{:016X}",
                    vaddr, value, s.pc, s.reg[30]
                );
            }
        }
        watch_write_64(self, vaddr, 2, value as u128);
        trace_a64_access_64(self, "WRITE", vaddr, 2, value as u128);
        if let Some(ref cm) = self.core_memory {
            cm.lock().unwrap().write_16(vaddr, value);
        } else {
            self.memory.write().unwrap().write_16(vaddr, value);
        }
    }

    fn memory_write_32(&mut self, vaddr: u64, value: u32) {
        if !self.check_memory_access(vaddr, 4, DebugWatchpointType::WRITE) {
            return;
        }
        trace_unmapped_write_64(self, vaddr, 4, value as u128);
        if trace_w32_value() == Some(value) {
            static SHOWN: AtomicU32 = AtomicU32::new(0);
            let n = SHOWN.fetch_add(1, Ordering::Relaxed);
            if n < 16 {
                if let Some(pc_ptr) = self.jit_pc_ptr {
                    let state = unsafe {
                        &*((pc_ptr as *const u8).sub(A64JitState::offset_of_pc())
                            as *const A64JitState)
                    };
                    eprintln!(
                        "[W32_VALUE #{n}] vaddr=0x{vaddr:016X} value=0x{value:08X} pc=0x{:016X} lr=0x{:016X}",
                        state.pc, state.reg[30]
                    );
                }
            }
        }
        if trace_w_at_vaddr().is_some_and(|target| vaddr <= target && target < vaddr + 4) {
            if let Some(pc_ptr) = self.jit_pc_ptr {
                let s = unsafe {
                    &*((pc_ptr as *const u8).sub(A64JitState::offset_of_pc()) as *const A64JitState)
                };
                let saved_lr = self.memory_read_64(s.sp + 8);
                let x27 = s.reg[27];
                let x27_table = self.memory_read_64(x27 + 0x20);
                let x27_indices = self.memory_read_64(x27 + 0x28);
                let x27_count = self.memory_read_64(x27 + 0x30) as u32;
                let table0 = self.memory_read_64(x27_table);
                let table1 = self.memory_read_64(x27_table + 8);
                let table0_obj = self.memory_read_64(table0);
                let table1_obj = self.memory_read_64(table1);
                let idx0 = self.memory_read_64(x27_indices) as u32;
                let idx1 = (self.memory_read_64(x27_indices) >> 32) as u32;
                let idx2 = self.memory_read_64(x27_indices + 8) as u32;
                let idx3 = (self.memory_read_64(x27_indices + 8) >> 32) as u32;
                let idx8 = self.memory_read_64(x27_indices + 32) as u32;
                let idx9 = (self.memory_read_64(x27_indices + 32) >> 32) as u32;
                let idx10 = self.memory_read_64(x27_indices + 40) as u32;
                let idx11 = (self.memory_read_64(x27_indices + 40) >> 32) as u32;
                let table_idx10 = self.memory_read_64(x27_table + (idx10 as u64) * 8);
                let table_idx10_obj = self.memory_read_64(table_idx10);
                eprintln!(
                            "[W32_AT] vaddr=0x{:016X} value=0x{:08X} pc=0x{:016X} lr=0x{:016X} saved_lr=0x{:016X} sp=0x{:016X} x0=0x{:016X} x1=0x{:016X} x19=0x{:016X} x20=0x{:016X} x21=0x{:016X} x27=0x{:016X} x28=0x{:016X} x27_table=0x{:016X} x27_indices=0x{:016X} x27_count={} table0=0x{:016X}->0x{:016X} table1=0x{:016X}->0x{:016X} idx0_3=[{},{},{},{}] idx8_11=[{},{},{},{}] table_idx10=0x{:016X}->0x{:016X}",
                            vaddr,
                            value,
                            s.pc,
                            s.reg[30],
                            saved_lr,
                            s.sp,
                            s.reg[0],
                            s.reg[1],
                            s.reg[19],
                            s.reg[20],
                            s.reg[21],
                            x27,
                            s.reg[28],
                            x27_table,
                            x27_indices,
                            x27_count,
                            table0,
                            table0_obj,
                            table1,
                            table1_obj,
                            idx0,
                            idx1,
                            idx2,
                            idx3,
                            idx8,
                            idx9,
                            idx10,
                            idx11,
                            table_idx10,
                            table_idx10_obj
                        );
            }
        }
        watch_write_64(self, vaddr, 4, value as u128);
        trace_a64_access_64(self, "WRITE", vaddr, 4, value as u128);
        if let Some(ref cm) = self.core_memory {
            cm.lock().unwrap().write_32(vaddr, value);
        } else {
            self.memory.write().unwrap().write_32(vaddr, value);
        }
    }

    fn memory_write_64(&mut self, vaddr: u64, value: u64) {
        if !self.check_memory_access(vaddr, 8, DebugWatchpointType::WRITE) {
            return;
        }
        trace_unmapped_write_64(self, vaddr, 8, value as u128);
        // RUZU_COUNT_W64_BY_CORE_AT_VADDR=0xVADDR — atomic per-core counter
        // for slow-path W64 writes targeting the specified vaddr. Used to
        // identify which emulator cores write to a tracked memory location.
        // Lighter than RUZU_TRACE_W64_AT_VADDR (no eprintln). SIGUSR2 dumps
        // via `dump_w64_by_core_counters()` (module-level public fn).
        if trace_count_w64_by_core_at_vaddr().is_some_and(|target| vaddr == target) {
            use std::sync::atomic::Ordering;
            let idx = self.core_index.min(15);
            W64_BY_CORE_COUNTERS[idx].fetch_add(1, Ordering::Relaxed);
        }
        // RUZU_DIVERGE_PAGE=0xPAGE — after the slow-path write, read back
        // the value via the FASTMEM ARENA host pointer (virtual_base +
        // vaddr) and compare. If they differ, the slow-path memory and
        // the fastmem arena are NOT aliased to the same physical page —
        // this is the STK heap-shifted-pointer wedge root cause.
        // Specify a single 4KB page (matches whole page).
        if let Some(page) = diverge_page() {
            if (vaddr & !0xFFFu64) == page {
                use std::sync::atomic::{AtomicU32, Ordering};
                static SHOWN: AtomicU32 = AtomicU32::new(0);
                let n = SHOWN.fetch_add(1, Ordering::Relaxed);
                if n < 64 || n.is_multiple_of(1000) {
                    if let Some(ref cm) = self.core_memory {
                        let cm_guard = cm.lock().unwrap();
                        let arena = cm_guard.fastmem_pointer();
                        // Read back the value via the slow-path Memory API
                        // (which goes through pt.pointers[idx] = backing_base + offset).
                        let slow_readback = cm_guard.read_64(vaddr);
                        drop(cm_guard);
                        if !arena.is_null() {
                            let arena_host_addr = unsafe { arena.add(vaddr as usize) };
                            let arena_value =
                                unsafe { std::ptr::read_unaligned(arena_host_addr as *const u64) };
                            let differ_arena = arena_value != value;
                            let differ_slow = slow_readback != value;
                            if differ_arena || differ_slow || n < 16 {
                                eprintln!(
                                    "[DIVERGE_W64 #{}] vaddr=0x{:016X} wrote=0x{:016X} slow_readback=0x{:016X} fastmem_arena=0x{:016X} arena_host={:p} differ_arena={} differ_slow={}",
                                    n, vaddr, value, slow_readback, arena_value,
                                    arena_host_addr, differ_arena, differ_slow
                                );
                            }
                        }
                    }
                }
            }
        }
        // RUZU_TRACE_W64_AT_VADDR=0xVADDR — log every 64-bit write whose
        // target vaddr matches. Used to find the FIRST write that
        // stores a corrupt value into a tracked location.
        // Special suffix `:page` matches the entire 4KB page containing
        // VADDR (e.g. "0x81490350:page" matches 0x81490000..0x81491000).
        if let Some(spec) = trace_w64_at_vaddr() {
            let matches = spec.matches(vaddr);
            // RUZU_TRACE_W64_FILTER_OOR=1 — also restrict to writes whose
            // VALUE is out-of-range (top 24 bits non-zero, > 0xFF_FFFF_FFFF).
            let oor_filter_ok = if trace_w64_filter_oor_enabled() {
                value > 0xFF_FFFF_FFFFu64
            } else {
                true
            };
            if matches && oor_filter_ok {
                use std::sync::atomic::{AtomicU32, Ordering};
                static SHOWN: AtomicU32 = AtomicU32::new(0);
                let n = SHOWN.fetch_add(1, Ordering::Relaxed);
                // Always log the first 32, then every 1000th.
                if n < 32 || n.is_multiple_of(1000) {
                    if let Some(pc_ptr) = self.jit_pc_ptr {
                        let jit_state_ptr = unsafe {
                            (pc_ptr as *const u8).sub(A64JitState::offset_of_pc())
                                as *const A64JitState
                        };
                        let s = unsafe { &*jit_state_ptr };
                        eprintln!(
                            "[W64_AT_VADDR #{}] vaddr=0x{:016X} value=0x{:016X} pc=0x{:016X} lr=0x{:016X} \
x0=0x{:016X} x1=0x{:016X} x2=0x{:016X} x19=0x{:016X} x20=0x{:016X} x21=0x{:016X} x22=0x{:016X}",
                            n, vaddr, value, s.pc, s.reg[30],
                            s.reg[0], s.reg[1], s.reg[2],
                            s.reg[19], s.reg[20], s.reg[21], s.reg[22],
                        );
                    }
                }
            }
        }
        // RUZU_TRACE_W64_VALUE=0xVAL — fire on every 64-bit write whose
        // value matches VAL. Used to find the source of a sentinel value
        // being placed into a struct field (e.g. STK's mysterious
        // 0x0000FF00FFFF0000 ending up in a refcount-table entry).
        if let Some(target) = trace_w64_value() {
            if value == target {
                use std::sync::atomic::{AtomicU32, Ordering};
                static SHOWN: AtomicU32 = AtomicU32::new(0);
                let n = SHOWN.fetch_add(1, Ordering::Relaxed);
                if n < 32 || n.is_multiple_of(1000) {
                    if let Some(pc_ptr) = self.jit_pc_ptr {
                        let jit_state_ptr = unsafe {
                            (pc_ptr as *const u8).sub(A64JitState::offset_of_pc())
                                as *const A64JitState
                        };
                        let s = unsafe { &*jit_state_ptr };
                        eprintln!(
                            "[W64_VALUE #{}] vaddr=0x{:016X} value=0x{:016X} pc=0x{:016X} lr=0x{:016X} sp=0x{:016X} \
x0=0x{:016X} x1=0x{:016X} x2=0x{:016X} x3=0x{:016X} x19=0x{:016X} x20=0x{:016X} x21=0x{:016X} x22=0x{:016X} x29=0x{:016X}",
                            n, vaddr, value, s.pc, s.reg[30], s.sp,
                            s.reg[0], s.reg[1], s.reg[2], s.reg[3],
                            s.reg[19], s.reg[20], s.reg[21], s.reg[22], s.reg[29],
                        );
                    }
                }
            }
        }
        watch_write_64(self, vaddr, 8, value as u128);
        trace_a64_access_64(self, "WRITE", vaddr, 8, value as u128);
        if let Some(ref cm) = self.core_memory {
            cm.lock().unwrap().write_64(vaddr, value);
        } else {
            self.memory.write().unwrap().write_64(vaddr, value);
        }
        // RUZU_DIVERGE_PAGE post-write check: read back via BOTH slow path
        // and fastmem arena to confirm the write took effect coherently.
        // ALSO: write a sentinel via fastmem-arena directly and read it
        // back via slow-path to check the OTHER coherency direction.
        if let Some(target_page) = diverge_page() {
            if (vaddr & !0xFFFu64) == target_page {
                use std::sync::atomic::{AtomicU32, Ordering};
                static SHOWN_POST: AtomicU32 = AtomicU32::new(0);
                let n = SHOWN_POST.fetch_add(1, Ordering::Relaxed);
                if n < 8 {
                    if let Some(ref cm) = self.core_memory {
                        let cm_guard = cm.lock().unwrap();
                        let arena = cm_guard.fastmem_pointer();
                        let slow = cm_guard.read_64(vaddr);
                        drop(cm_guard);
                        if !arena.is_null() {
                            let arena_value = unsafe {
                                std::ptr::read_unaligned(arena.add(vaddr as usize) as *const u64)
                            };
                            // Write sentinel via fastmem-arena pointer,
                            // then read via slow path. If memory is fully
                            // coherent, slow-path read returns sentinel.
                            let sentinel: u64 = 0xDEADBEEF_CAFEBABE;
                            unsafe {
                                std::ptr::write_unaligned(
                                    arena.add(vaddr as usize) as *mut u64,
                                    sentinel,
                                );
                            }
                            let slow_after_arena_write = self
                                .core_memory
                                .as_ref()
                                .map(|cm2| cm2.lock().unwrap().read_64(vaddr))
                                .unwrap_or(0);
                            // Restore the original value.
                            unsafe {
                                std::ptr::write_unaligned(
                                    arena.add(vaddr as usize) as *mut u64,
                                    value,
                                );
                            }
                            eprintln!(
                                "[POST_WRITE #{}] vaddr=0x{:016X} wrote=0x{:016X} slow_readback=0x{:016X} fastmem_arena=0x{:016X} sentinel_via_slow=0x{:016X} match={} sentinel_match={}",
                                n, vaddr, value, slow, arena_value,
                                slow_after_arena_write,
                                slow == value && arena_value == value,
                                slow_after_arena_write == sentinel,
                            );
                        }
                    }
                }
            }
        }
    }

    fn memory_write_128(&mut self, vaddr: u64, value_lo: u64, value_hi: u64) {
        // Capture xmm1's and xmm15's hardware state at the very first
        // opportunity. xmm15 acts as a "fallback fired" marker (set to
        // all-FFs by the fastmem fallback when RUZU_FALLBACK_MARK_XMM15=1).
        #[cfg(target_arch = "x86_64")]
        let (
            xmm1_lo_at_entry,
            xmm1_hi_at_entry,
            xmm15_lo_at_entry,
            xmm15_hi_at_entry,
            xmm14_lo_at_entry,
            xmm14_hi_at_entry,
            xmm13_lo_at_entry,
            xmm13_hi_at_entry,
            xmm12_lo_at_entry,
            xmm12_hi_at_entry,
        ): (u64, u64, u64, u64, u64, u64, u64, u64, u64, u64);
        #[cfg(target_arch = "x86_64")]
        unsafe {
            std::arch::asm!(
                "movq {l}, xmm1",
                "pextrq {h}, xmm1, 1",
                "movq {l15}, xmm15",
                "pextrq {h15}, xmm15, 1",
                "movq {l14}, xmm14",
                "pextrq {h14}, xmm14, 1",
                "movq {l13}, xmm13",
                "pextrq {h13}, xmm13, 1",
                "movq {l12}, xmm12",
                "pextrq {h12}, xmm12, 1",
                l = lateout(reg) xmm1_lo_at_entry,
                h = lateout(reg) xmm1_hi_at_entry,
                l15 = lateout(reg) xmm15_lo_at_entry,
                h15 = lateout(reg) xmm15_hi_at_entry,
                l14 = lateout(reg) xmm14_lo_at_entry,
                h14 = lateout(reg) xmm14_hi_at_entry,
                l13 = lateout(reg) xmm13_lo_at_entry,
                h13 = lateout(reg) xmm13_hi_at_entry,
                l12 = lateout(reg) xmm12_lo_at_entry,
                h12 = lateout(reg) xmm12_hi_at_entry,
                options(nostack, preserves_flags),
            );
        }
        #[cfg(not(target_arch = "x86_64"))]
        let (
            xmm1_lo_at_entry,
            xmm1_hi_at_entry,
            xmm15_lo_at_entry,
            xmm15_hi_at_entry,
            xmm14_lo_at_entry,
            xmm14_hi_at_entry,
            xmm13_lo_at_entry,
            xmm13_hi_at_entry,
            xmm12_lo_at_entry,
            xmm12_hi_at_entry,
        ) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
        if !self.check_memory_access(vaddr, 16, DebugWatchpointType::WRITE) {
            return;
        }
        // RUZU_TRACE_W64_AT_VADDR — same logic as W64 path; also matches
        // 128-bit writes (which span vaddr..vaddr+16).
        if let Some(spec) = trace_w64_at_vaddr() {
            let matches = spec.matches_span(vaddr, 16);
            // RUZU_TRACE_W64_FILTER_OOR — restrict to writes whose lo OR hi
            // half is out-of-range (top 24 bits set, > 0xFF_FFFF_FFFF).
            let oor_filter_ok = if trace_w64_filter_oor_enabled() {
                value_lo > 0xFF_FFFF_FFFFu64 || value_hi > 0xFF_FFFF_FFFFu64
            } else {
                true
            };
            if matches && oor_filter_ok {
                use std::sync::atomic::{AtomicU32, Ordering};
                static SHOWN_128: AtomicU32 = AtomicU32::new(0);
                let n = SHOWN_128.fetch_add(1, Ordering::Relaxed);
                if n < 32 || n.is_multiple_of(1000) {
                    if let Some(pc_ptr) = self.jit_pc_ptr {
                        let jit_state_ptr = unsafe {
                            (pc_ptr as *const u8).sub(A64JitState::offset_of_pc())
                                as *const A64JitState
                        };
                        let s = unsafe { &*jit_state_ptr };
                        eprintln!(
                            "[W128_AT_VADDR #{}] vaddr=0x{:016X} lo=0x{:016X} hi=0x{:016X} pc=0x{:016X} lr=0x{:016X} \
x19=0x{:016X} x20=0x{:016X} x22=0x{:016X}",
                            n, vaddr, value_lo, value_hi, s.pc, s.reg[30],
                            s.reg[19], s.reg[20], s.reg[22],
                        );
                    }
                }
            }
        }
        // RUZU_TRACE_W64_VALUE also checks 128-bit writes (lo OR hi half).
        if let Some(target) = trace_w64_value() {
            if value_lo == target || value_hi == target {
                use std::sync::atomic::{AtomicU32, Ordering};
                static SHOWN: AtomicU32 = AtomicU32::new(0);
                let n = SHOWN.fetch_add(1, Ordering::Relaxed);
                if n < 8 {
                    if let Some(pc_ptr) = self.jit_pc_ptr {
                        let jit_state_ptr = unsafe {
                            (pc_ptr as *const u8).sub(A64JitState::offset_of_pc())
                                as *const A64JitState
                        };
                        let s = unsafe { &*jit_state_ptr };
                        // Also dump VN_lo/hi from JitState. RUZU_DUMP_V_INDEX=31 (decimal).
                        let v_idx = dump_v_index();
                        let vlo = s.vec.get(2 * v_idx).copied().unwrap_or(0);
                        let vhi = s.vec.get(2 * v_idx + 1).copied().unwrap_or(0);
                        eprintln!(
                            "[W128_VALUE #{}] vaddr=0x{:016X} lo=0x{:016X} hi=0x{:016X} pc=0x{:016X} lr=0x{:016X} \
x0=0x{:016X} x1=0x{:016X} x19=0x{:016X} x20=0x{:016X} x21=0x{:016X} v{}_lo=0x{:016X} v{}_hi=0x{:016X} \
xmm1_lo=0x{:016X} xmm1_hi=0x{:016X} xmm12_lo=0x{:016X} xmm12_hi=0x{:016X} xmm13_lo=0x{:016X} xmm13_hi=0x{:016X} xmm14_lo=0x{:016X} xmm14_hi=0x{:016X} xmm15_lo=0x{:016X} xmm15_hi=0x{:016X}",
                            n, vaddr, value_lo, value_hi, s.pc, s.reg[30],
                            s.reg[0], s.reg[1], s.reg[19], s.reg[20], s.reg[21],
                            v_idx, vlo, v_idx, vhi,
                            xmm1_lo_at_entry, xmm1_hi_at_entry,
                            xmm12_lo_at_entry, xmm12_hi_at_entry,
                            xmm13_lo_at_entry, xmm13_hi_at_entry,
                            xmm14_lo_at_entry, xmm14_hi_at_entry,
                            xmm15_lo_at_entry, xmm15_hi_at_entry,
                        );
                    }
                }
            }
        }
        watch_write_64(
            self,
            vaddr,
            16,
            ((value_hi as u128) << 64) | (value_lo as u128),
        );
        trace_a64_access_64(
            self,
            "WRITE",
            vaddr,
            16,
            ((value_hi as u128) << 64) | (value_lo as u128),
        );
        if let Some(ref cm) = self.core_memory {
            let m = cm.lock().unwrap();
            m.write_64(vaddr, value_lo);
            m.write_64(vaddr + 8, value_hi);
        } else {
            let mut mem = self.memory.write().unwrap();
            mem.write_64(vaddr, value_lo);
            mem.write_64(vaddr + 8, value_hi);
        }
    }

    fn exclusive_read_8(&self, vaddr: u64) -> u8 {
        self.memory_read_8(vaddr)
    }

    fn exclusive_read_16(&self, vaddr: u64) -> u16 {
        self.memory_read_16(vaddr)
    }

    fn exclusive_read_32(&self, vaddr: u64) -> u32 {
        self.memory_read_32(vaddr)
    }

    fn exclusive_read_64(&self, vaddr: u64) -> u64 {
        self.memory_read_64(vaddr)
    }

    fn exclusive_read_128(&self, vaddr: u64) -> (u64, u64) {
        self.memory_read_128(vaddr)
    }

    fn exclusive_write_8(&mut self, vaddr: u64, value: u8, expected: u8) -> bool {
        self.check_memory_access(vaddr, 1, DebugWatchpointType::WRITE)
            && if let Some(ref cm) = self.core_memory {
                cm.lock().unwrap().write_exclusive_8(vaddr, value, expected)
            } else {
                self.memory_write_8(vaddr, value);
                true
            }
    }

    fn exclusive_write_16(&mut self, vaddr: u64, value: u16, expected: u16) -> bool {
        self.check_memory_access(vaddr, 2, DebugWatchpointType::WRITE)
            && if let Some(ref cm) = self.core_memory {
                cm.lock()
                    .unwrap()
                    .write_exclusive_16(vaddr, value, expected)
            } else {
                self.memory_write_16(vaddr, value);
                true
            }
    }

    fn exclusive_write_32(&mut self, vaddr: u64, value: u32, expected: u32) -> bool {
        // RUZU_TRACE_CAS32_PC=1 — log the JIT PC + key registers on the
        // FIRST few exclusive-write-32 calls. Used to identify the guest
        // code that emits a tight CAS spinloop (e.g. STK's atomic CAS on
        // 0x0000ff00ffff0008 right after Connect).
        let trace_exclusive32_target = trace_exclusive32_addr();
        let trace_line = if trace_cas32_pc_enabled()
            || trace_exclusive32_all_enabled()
            || trace_exclusive32_target.is_some()
        {
            use std::sync::atomic::{AtomicU32, Ordering};
            static SHOWN: AtomicU32 = AtomicU32::new(0);
            static TARGET_SHOWN: AtomicU32 = AtomicU32::new(0);
            // Only trace CAS at non-canonical / suspicious addresses
            // (outside the normal libnx mutex region 0x80000000..0x90000000).
            let trace_all = trace_exclusive32_all_enabled();
            let trace_target = trace_exclusive32_target.is_some_and(|target| target == vaddr);
            let suspicious =
                trace_all || trace_target || vaddr < 0x8000_0000 || vaddr >= 0x9000_0000;
            let n = if trace_target {
                TARGET_SHOWN.fetch_add(1, Ordering::Relaxed)
            } else if suspicious {
                SHOWN.fetch_add(1, Ordering::Relaxed)
            } else {
                u32::MAX
            };
            if n < 512 || (trace_target && n % 1000 == 0) {
                if let Some(pc_ptr) = self.jit_pc_ptr {
                    let jit_state_ptr = unsafe {
                        (pc_ptr as *const u8).sub(A64JitState::offset_of_pc()) as *const A64JitState
                    };
                    let s = unsafe { &*jit_state_ptr };
                    Some(format!(
                        "[CAS32 #{}] core={} vaddr=0x{:016X} value=0x{:08X} expected=0x{:08X} pc=0x{:016X} lr=0x{:016X} sp=0x{:016X} \
x0=0x{:016X} x1=0x{:016X} x2=0x{:016X} x3=0x{:016X} x19=0x{:016X} x20=0x{:016X} x21=0x{:016X} x22=0x{:016X} x29=0x{:016X}",
                        n, self.core_index, vaddr, value, expected, s.pc, s.reg[30], s.sp,
                        s.reg[0], s.reg[1], s.reg[2], s.reg[3],
                        s.reg[19], s.reg[20], s.reg[21], s.reg[22], s.reg[29],
                    ))
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        let success = self.check_memory_access(vaddr, 4, DebugWatchpointType::WRITE)
            && if let Some(ref cm) = self.core_memory {
                cm.lock()
                    .unwrap()
                    .write_exclusive_32(vaddr, value, expected)
            } else {
                self.memory_write_32(vaddr, value);
                true
            };
        if let Some(line) = trace_line {
            log::warn!("{line} success={success}");
        }
        if success {
            if let Some(target) = trace_exclusive32_target.filter(|target| *target == vaddr) {
                use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
                static TARGET_LOCK_HELD: AtomicBool = AtomicBool::new(false);
                static TARGET_LOCK_DIAG_COUNT: AtomicU32 = AtomicU32::new(0);
                let is_unlock = value == 0;
                let bad_transition = if is_unlock {
                    TARGET_LOCK_HELD
                        .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
                        .is_err()
                } else {
                    TARGET_LOCK_HELD
                        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                        .is_err()
                };
                if bad_transition {
                    let n = TARGET_LOCK_DIAG_COUNT.fetch_add(1, Ordering::Relaxed);
                    if n < 64 || n % 1000 == 0 {
                        if let Some(pc_ptr) = self.jit_pc_ptr {
                            let jit_state_ptr = unsafe {
                                (pc_ptr as *const u8).sub(A64JitState::offset_of_pc())
                                    as *const A64JitState
                            };
                            let s = unsafe { &*jit_state_ptr };
                            log::warn!(
                                "[CAS32_LOCK_STATE_BAD #{}] core={} target=0x{:016X} op={} value=0x{:08X} expected=0x{:08X} pc=0x{:016X} lr=0x{:016X} sp=0x{:016X} x19=0x{:016X} x20=0x{:016X}",
                                n,
                                self.core_index,
                                target,
                                if is_unlock { "unlock_without_lock" } else { "lock_while_held" },
                                value,
                                expected,
                                s.pc,
                                s.reg[30],
                                s.sp,
                                s.reg[19],
                                s.reg[20],
                            );
                        }
                    }
                }
            }
        }
        success
    }

    fn exclusive_write_64(&mut self, vaddr: u64, value: u64, expected: u64) -> bool {
        self.check_memory_access(vaddr, 8, DebugWatchpointType::WRITE)
            && if let Some(ref cm) = self.core_memory {
                cm.lock()
                    .unwrap()
                    .write_exclusive_64(vaddr, value, expected)
            } else {
                self.memory_write_64(vaddr, value);
                true
            }
    }

    fn exclusive_write_128(
        &mut self,
        vaddr: u64,
        value_lo: u64,
        value_hi: u64,
        expected_lo: u64,
        expected_hi: u64,
    ) -> bool {
        self.check_memory_access(vaddr, 16, DebugWatchpointType::WRITE)
            && if let Some(ref cm) = self.core_memory {
                cm.lock().unwrap().write_exclusive_128(
                    vaddr,
                    value_lo,
                    value_hi,
                    expected_lo,
                    expected_hi,
                )
            } else {
                self.memory_write_128(vaddr, value_lo, value_hi);
                true
            }
    }

    fn exclusive_clear(&mut self) {
        if !self.exclusive_monitor.is_null() {
            unsafe {
                (*self.exclusive_monitor)
                    .get_monitor()
                    .clear_processor(self.core_index)
            };
        }
    }

    fn instruction_cache_operation(&mut self, op: u64, vaddr: u64) {
        // Upstream IC operations:
        // 0 = InvalidateByVAToPoU (IC IVAU) — invalidate cache line at vaddr
        // 1 = InvalidateAllToPoU (IC IALLU) — invalidate entire icache
        match op {
            0 => {
                log::trace!(
                    "IC IVAU @ {:#x} (no-op, cache invalidation handled at JIT level)",
                    vaddr
                );
            }
            1 => {
                log::trace!("IC IALLU (no-op, cache invalidation handled at JIT level)");
            }
            _ => {
                log::warn!(
                    "Unknown instruction_cache_operation op={} vaddr={:#x}",
                    op,
                    vaddr
                );
            }
        }
    }

    fn call_supervisor(&mut self, svc_num: u32) {
        self.svc.store(svc_num, Ordering::Relaxed);
        self.halt_execution(rdynarmic::halt_reason::HaltReason::SVC);
    }

    fn interpreter_fallback(&mut self, pc: u64, num_instructions: usize) {
        self.last_exception_address.store(pc, Ordering::Relaxed);
        self.snapshot_context(pc);
        let instr = if let Some(ref cm) = self.core_memory {
            let m = cm.lock().unwrap();
            if m.is_valid_virtual_address_range(pc, 4) {
                m.read_32(pc)
            } else {
                0
            }
        } else {
            let mem = self.memory.read().unwrap();
            if mem.is_valid_range(pc, 4) {
                mem.read_32(pc)
            } else {
                0
            }
        };
        self.trace_interpreter_fallback(pc, instr, num_instructions);
        log::error!(
            "Unimplemented instruction @ 0x{:X} for {} instructions (instr = {:08X})",
            pc,
            num_instructions,
            instr
        );
        self.halt_execution(rdynarmic::halt_reason::HaltReason::PREFETCH_ABORT);
    }

    fn exception_raised(&mut self, pc: u64, exception: u64) {
        use rdynarmic::frontend::a64::types::Exception;

        match exception {
            x if x == Exception::WaitForInterrupt as u64
                || x == Exception::WaitForEvent as u64
                || x == Exception::SendEvent as u64
                || x == Exception::SendEventLocal as u64
                || x == Exception::Yield as u64 =>
            {
                return;
            }
            x if x == Exception::NoExecuteFault as u64 => {
                self.last_exception_address.store(pc, Ordering::Relaxed);
                self.snapshot_context(pc);
                log::error!(
                    "Cannot execute instruction at unmapped address {:#016x}",
                    pc
                );
                self.halt_execution(rdynarmic::halt_reason::HaltReason::PREFETCH_ABORT);
                return;
            }
            x if x == Exception::Breakpoint as u64 => {
                self.last_exception_address.store(pc, Ordering::Relaxed);
                self.snapshot_context(pc);
                self.halt_execution(rdynarmic::halt_reason::HaltReason::BREAKPOINT);
                return;
            }
            _ => {}
        }

        self.last_exception_address.store(pc, Ordering::Relaxed);
        self.snapshot_context(pc);
        log::error!(
            "DynarmicCallbacks64::exception_raised(pc={:#x}, exception={:#x})",
            pc,
            exception
        );
        // Dump instruction window around exception PC
        if let Some(ref cm) = self.core_memory {
            let m = cm.lock().unwrap();
            let start = pc.saturating_sub(0x10);
            for addr in (start..=pc.saturating_add(0x10)).step_by(4) {
                if !m.is_valid_virtual_address(addr) {
                    log::error!("  [{:#018x}] <unmapped>", addr);
                    continue;
                }
                let insn = m.read_32(addr);
                let marker = if addr == pc { " <EXC>" } else { "" };
                log::error!("  [{:#018x}] {:#010x}{}", addr, insn, marker);
            }
        }
    }

    /// Matches upstream `DynarmicCallbacks64::GetCNTPCT`.
    fn get_cntpct(&self) -> u64 {
        self.core_timing.get_clock_ticks()
    }

    /// Matches upstream `DynarmicCallbacks64::AddTicks`:
    /// Divides ticks by NUM_CPU_CORES (4), passes to CoreTiming::AddTicks.
    fn add_ticks(&mut self, ticks: u64) {
        if self.uses_wall_clock {
            return;
        }
        let amortized_ticks =
            std::cmp::max(ticks / crate::hardware_properties::NUM_CPU_CORES as u64, 1);
        self.core_timing.add_ticks(amortized_ticks);
    }

    /// Matches upstream `DynarmicCallbacks64::GetTicksRemaining`:
    /// Returns max(CoreTiming::GetDowncount(), 0).
    fn get_ticks_remaining(&self) -> u64 {
        if self.uses_wall_clock {
            return u64::MAX;
        }
        std::cmp::max(self.core_timing.get_downcount(), 0) as u64
    }

    fn set_halt_reason_ptr(&mut self, ptr: *const u32) {
        self.halt_reason_ptr = Some(ptr as *const AtomicU32);
    }

    fn set_pc_ptr(&mut self, ptr: *const u32) {
        self.jit_pc_ptr = Some(ptr as *const u64);
    }
}

/// ARM64 Dynarmic JIT backend.
///
/// Corresponds to upstream `Core::ArmDynarmic64`.
pub struct ArmDynarmic64 {
    pub base: ArmInterfaceBase,

    // Upstream holds `System& m_system` for accessing CoreTiming, DebuggerEnabled,
    // Settings, etc. Currently these are passed individually (core_timing, uses_wall_clock)
    // to avoid circular dependency with System which owns the ARM backends.
    // When System stabilizes, this can be replaced with a reference.
    /// SVC callback number, shared with DynarmicCallbacks64.
    /// Upstream: m_svc written by callback via m_parent reference.
    svc: Arc<AtomicU32>,

    /// Watchpoint that caused a halt
    halted_watchpoint: Arc<Mutex<Option<DebugWatchpoint>>>,

    /// Context saved at breakpoint
    breakpoint_context: Arc<Mutex<ThreadContext>>,

    /// The rdynarmic A64 JIT instance
    jit: Option<rdynarmic::A64Jit>,

    /// TPIDRRO_EL0 system register storage.
    /// Upstream stores this in `DynarmicCallbacks64::m_tpidrro_el0` and gives
    /// Dynarmic a stable pointer to it through `config.tpidrro_el0`.
    tpidrro_el0: Box<u64>,

    /// TPIDR_EL0 system register storage.
    /// Upstream stores this in `DynarmicCallbacks64::m_tpidr_el0` and gives
    /// Dynarmic a stable pointer to it through `config.tpidr_el0`.
    tpidr_el0: Box<u64>,

    /// Last exception address reported by dynarmic for the current halt.
    last_exception_address: Arc<AtomicU64>,
}

impl ArmDynarmic64 {
    /// Create a new ARM64 dynarmic backend.
    ///
    /// Corresponds to upstream `ArmDynarmic64::ArmDynarmic64`.
    ///
    /// `shared_memory` corresponds to upstream's `process->GetMemory()` —
    /// the JIT callbacks use it for all guest memory access.
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
        // Fetch fastmem pointer from core_memory BEFORE it gets moved
        // into the callbacks. Mirrors A32's pattern
        // (arm_dynarmic_32.rs:2010-2013). The pointer is the base of a
        // 512 GiB host VM region whose pages mirror guest physical
        // memory; the JIT emits direct `mov [r13+vaddr]` instructions
        // against it for fast guest memory access.
        //
        // `RUZU_NO_FASTMEM=1` env-var disables fastmem entirely (forces
        // the slow callback path for all memory accesses) — useful for
        // debugging fastmem-related issues without rebuilding.
        let no_fastmem = std::env::var_os("RUZU_NO_FASTMEM").is_some();
        let kernel_process =
            unsafe { &*(process as *const _ as *const crate::hle::kernel::k_process::KProcess) };
        let address_space_bits = kernel_process.page_table.get_address_space_width() as usize;
        let mut page_table_pointer: Option<*const u8> = if no_fastmem {
            None
        } else {
            kernel_process
                .page_table
                .get_base()
                .get_impl()
                .map(|page_table| page_table.pointers.data() as *const u8)
                .filter(|p| !p.is_null())
        };

        let mut fastmem_pointer: Option<*mut u8> = if no_fastmem {
            log::warn!(
                "ArmDynarmic64: RUZU_NO_FASTMEM set — fastmem and page-table paths disabled"
            );
            None
        } else {
            kernel_process
                .page_table
                .get_base()
                .get_impl()
                .map(|page_table| page_table.fastmem_arena)
                .filter(|p| !p.is_null())
        };

        // Create JIT callbacks with shared memory reference
        let svc = Arc::new(AtomicU32::new(0));
        let last_exception_address = Arc::new(AtomicU64::new(0));
        let breakpoint_context = Arc::new(Mutex::new(ThreadContext::default()));
        let base = ArmInterfaceBase::new(uses_wall_clock);
        let halted_watchpoint = Arc::new(Mutex::new(None));
        let callbacks = DynarmicCallbacks64::new(
            shared_memory,
            core_memory,
            svc.clone(),
            uses_wall_clock,
            core_timing,
            last_exception_address.clone(),
            breakpoint_context.clone(),
            exclusive_monitor,
            core_index,
            debugger_enabled,
            base.shared_watchpoint_array(),
            Arc::clone(&halted_watchpoint),
        );

        log::warn!(
            "ArmDynarmic64: page_table_pointer={:?} fastmem_pointer={:?} for core {}",
            page_table_pointer.map(|p| p as usize),
            fastmem_pointer.map(|p| p as usize),
            core_index
        );

        // RUZU_PROTECT_PAGE=0xVADDR — mprotect the 4KB fastmem-arena page
        // containing VADDR to PROT_READ-only. Forces every guest write to
        // that page to fault, dispatching through the SIGSEGV handler →
        // fastmem fallback stub → memory_write_64 callback. Used to
        // capture every write to a tracked location (e.g. a free-list
        // head pointer) when direct-fastmem stores are otherwise
        // invisible to the slow-path callback.
        #[cfg(any(unix, windows))]
        if let (Some(arena), Ok(spec)) = (fastmem_pointer, std::env::var("RUZU_PROTECT_PAGE")) {
            if core_index == 0 {
                let target =
                    u64::from_str_radix(spec.trim().trim_start_matches("0x"), 16).unwrap_or(0);
                if target != 0 {
                    let page_size = 4096usize;
                    let page_aligned_va = (target as usize) & !(page_size - 1);
                    let host_addr = unsafe { arena.add(page_aligned_va) };
                    #[cfg(unix)]
                    let rc = unsafe {
                        libc::mprotect(host_addr as *mut libc::c_void, page_size, libc::PROT_READ)
                    };
                    #[cfg(windows)]
                    let rc = unsafe {
                        let mut old_protect = 0;
                        if winapi::um::memoryapi::VirtualProtect(
                            host_addr.cast(),
                            page_size,
                            winapi::um::winnt::PAGE_READONLY,
                            &mut old_protect,
                        ) != 0
                        {
                            0
                        } else {
                            -1
                        }
                    };
                    log::warn!(
                        "RUZU_PROTECT_PAGE: protect(host=0x{:X}, 4KB, read-only) for guest=0x{:X} → rc={}",
                        host_addr as usize,
                        page_aligned_va,
                        rc
                    );
                }
            }
        }

        let settings = common::settings::values();
        let (mut optimizations, mut unsafe_optimizations, fastmem_address_space_bits) =
            if *settings.cpu_debug_mode.get_value() {
                (
                    OptimizationFlag::ALL_SAFE_OPTIMIZATIONS,
                    false,
                    address_space_bits,
                )
            } else {
                upstream_optimization_config(
                    address_space_bits,
                    *settings.cpu_accuracy.get_value(),
                    *settings.cpuopt_unsafe_unfuse_fma.get_value(),
                    *settings.cpuopt_unsafe_reduce_fp_error.get_value(),
                    *settings.cpuopt_unsafe_inaccurate_nan.get_value(),
                    *settings.cpuopt_unsafe_fastmem_check.get_value(),
                    *settings.cpuopt_unsafe_ignore_global_monitor.get_value(),
                )
            };

        let mut fastmem_exclusive_access =
            fastmem_pointer.is_some() && !exclusive_monitor.is_null();
        let mut recompile_on_exclusive_fastmem_failure = true;
        let mut only_detect_misalignment_via_page_table_on_page_boundary = true;
        let mut check_halt_on_memory_access = debugger_enabled;

        if *settings.cpu_debug_mode.get_value() {
            if !*settings.cpuopt_page_tables.get_value() {
                page_table_pointer = None;
            }
            if !*settings.cpuopt_block_linking.get_value() {
                optimizations &= !OptimizationFlag::BLOCK_LINKING;
            }
            if !*settings.cpuopt_return_stack_buffer.get_value() {
                optimizations &= !OptimizationFlag::RETURN_STACK_BUFFER;
            }
            if !*settings.cpuopt_fast_dispatcher.get_value() {
                optimizations &= !OptimizationFlag::FAST_DISPATCH;
            }
            if !*settings.cpuopt_context_elimination.get_value() {
                optimizations &= !OptimizationFlag::GET_SET_ELIMINATION;
            }
            if !*settings.cpuopt_const_prop.get_value() {
                optimizations &= !OptimizationFlag::CONST_PROP;
            }
            if !*settings.cpuopt_misc_ir.get_value() {
                optimizations &= !OptimizationFlag::MISC_IR_OPT;
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
            if !*settings.cpuopt_ignore_memory_aborts.get_value() {
                check_halt_on_memory_access = true;
            }
        }

        if !common::settings::is_fastmem_enabled(&settings) {
            fastmem_pointer = None;
            fastmem_exclusive_access = false;
        }

        if let Some(mask) = parse_optimization_mask_env("RUZU_A64_OPTIMIZATION_MASK") {
            optimizations = optimization_flags_from_mask(mask);
            unsafe_optimizations = mask & !OptimizationFlag::ALL_SAFE_OPTIMIZATIONS.bits() != 0;
        } else if std::env::var("RUZU_A64_NO_OPTIMIZATIONS")
            .ok()
            .is_some_and(|value| value != "0")
        {
            optimizations = OptimizationFlag::NO_OPTIMIZATIONS;
            unsafe_optimizations = false;
        }

        let tpidrro_el0 = Box::new(0u64);
        let mut tpidr_el0 = Box::new(0u64);
        let tpidrro_el0_ptr = (&*tpidrro_el0) as *const u64;
        let tpidr_el0_ptr = (&mut *tpidr_el0) as *mut u64;

        // Configure JIT
        // Upstream: enable_cycle_counting = !uses_wall_clock
        let config = JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(callbacks),
            enable_cycle_counting: !uses_wall_clock,
            code_cache_size: if cfg!(target_arch = "aarch64") {
                128 * 1024 * 1024
            } else {
                512 * 1024 * 1024
            },
            optimizations,
            unsafe_optimizations,
            global_monitor: if exclusive_monitor.is_null() {
                None
            } else {
                Some(unsafe { (*exclusive_monitor).get_monitor() as *mut _ })
            },
            fastmem_pointer,
            page_table_pointer,
            define_unpredictable_behaviour: true,
            arch_version: rdynarmic::interface::a32::arch_version::ArchVersion::V8,
            hook_hint_instructions: false,
            processor_id: core_index as usize,
            wall_clock_cntpct: uses_wall_clock,
            cntfrq_el0: common::wall_clock::CNTFRQ as u32,
            ctr_el0: 0x8444_c004,
            dczid_el0: 4,
            hook_data_cache_operations: false,
            hook_isb: false,
            tpidrro_el0: Some(tpidrro_el0_ptr),
            tpidr_el0: Some(tpidr_el0_ptr),
            // Memory emit options matching upstream zuyu's
            // `ArmDynarmic64::MakeJit` setup
            // (zuyu/src/core/arm/dynarmic/arm_dynarmic_64.cpp:225-248).
            // Both widths come from the process page table, as in Eden's
            // `MakeJit(page_table, page_table.GetAddressSpaceWidth())`.
            // Auto/unsafe-fastmem-check may widen only the fastmem side to
            // 64 bits below, matching the upstream accuracy switch.
            memory: rdynarmic::backend::x64::emit_context::MemoryEmitConfig {
                fastmem_address_space_bits,
                silently_mirror_fastmem: false,
                fastmem_exclusive_access,
                recompile_on_exclusive_fastmem_failure,
                recompile_on_fastmem_failure: true,
                page_table_present: page_table_pointer.is_some(),
                page_table_address_space_bits: address_space_bits,
                silently_mirror_page_table: false,
                absolute_offset_page_table: true,
                page_table_pointer_mask_bits: PageInfo::ATTRIBUTE_BITS as u32,
                detect_misaligned_access_via_page_table: 16 | 32 | 64 | 128,
                only_detect_misalignment_via_page_table_on_page_boundary,
                check_halt_on_memory_access,
                processor_id: core_index as usize,
            },
        };

        // Create the JIT
        let jit = match rdynarmic::A64Jit::new(config) {
            Ok(jit) => {
                log::info!(
                    "ArmDynarmic64: JIT created successfully for core {}",
                    core_index
                );
                Some(jit)
            }
            Err(e) => {
                log::error!(
                    "ArmDynarmic64: Failed to create JIT for core {}: {}",
                    core_index,
                    e
                );
                None
            }
        };

        Self {
            base,
            svc,
            halted_watchpoint,
            breakpoint_context,
            jit,
            tpidrro_el0,
            tpidr_el0,
            last_exception_address,
        }
    }
}

// SAFETY: the owned JIT callbacks hold raw pointers to long-lived objects
// (exclusive monitor and watchpoints) that remain valid for the process lifetime.
// The JIT is single-threaded per core — only one thread runs each ArmDynarmic64.
unsafe impl Send for ArmDynarmic64 {}

impl ArmInterface for ArmDynarmic64 {
    fn run_thread(&mut self, _thread: &mut KThread) -> HaltReason {
        self.last_exception_address.store(0, Ordering::Relaxed);
        let jit = match self.jit.as_mut() {
            Some(jit) => jit,
            None => {
                log::error!("ArmDynarmic64::run_thread: JIT not available");
                return HaltReason::BREAK_LOOP;
            }
        };

        jit.clear_exclusive_state();
        let rdynarmic_hr = jit.run();
        if trace_a64_svc_regs_enabled()
            && rdynarmic_hr.contains(rdynarmic::halt_reason::HaltReason::SVC)
        {
            eprintln!(
                "[A64_SVC_HALT] svc=0x{:x} pc=0x{:016x} lr=0x{:016x} sp=0x{:016x} x0=0x{:016x} x1=0x{:016x} x2=0x{:016x} x3=0x{:016x} x4=0x{:016x} x5=0x{:016x} x6=0x{:016x} x7=0x{:016x} x21=0x{:016x} x22=0x{:016x} x24=0x{:016x} x25=0x{:016x}",
                self.svc.load(Ordering::Relaxed),
                jit.get_pc(),
                jit.get_register(30),
                jit.get_sp(),
                jit.get_register(0),
                jit.get_register(1),
                jit.get_register(2),
                jit.get_register(3),
                jit.get_register(4),
                jit.get_register(5),
                jit.get_register(6),
                jit.get_register(7),
                jit.get_register(21),
                jit.get_register(22),
                jit.get_register(24),
                jit.get_register(25),
            );
        }
        translate_halt_reason(rdynarmic_hr)
    }

    fn step_thread(&mut self, _thread: &mut KThread) -> HaltReason {
        self.last_exception_address.store(0, Ordering::Relaxed);
        let jit = match self.jit.as_mut() {
            Some(jit) => jit,
            None => {
                log::error!("ArmDynarmic64::step_thread: JIT not available");
                return HaltReason::BREAK_LOOP;
            }
        };

        jit.clear_exclusive_state();
        let rdynarmic_hr = jit.step();
        if trace_a64_svc_regs_enabled()
            && rdynarmic_hr.contains(rdynarmic::halt_reason::HaltReason::SVC)
        {
            eprintln!(
                "[A64_SVC_STEP] svc=0x{:x} pc=0x{:016x} lr=0x{:016x} sp=0x{:016x} x0=0x{:016x} x1=0x{:016x} x2=0x{:016x} x3=0x{:016x} x4=0x{:016x} x5=0x{:016x} x6=0x{:016x} x7=0x{:016x} x21=0x{:016x} x22=0x{:016x} x24=0x{:016x} x25=0x{:016x}",
                self.svc.load(Ordering::Relaxed),
                jit.get_pc(),
                jit.get_register(30),
                jit.get_sp(),
                jit.get_register(0),
                jit.get_register(1),
                jit.get_register(2),
                jit.get_register(3),
                jit.get_register(4),
                jit.get_register(5),
                jit.get_register(6),
                jit.get_register(7),
                jit.get_register(21),
                jit.get_register(22),
                jit.get_register(24),
                jit.get_register(25),
            );
        }
        translate_halt_reason(rdynarmic_hr)
    }

    fn clear_instruction_cache(&mut self) {
        if let Some(jit) = self.jit.as_mut() {
            jit.clear_cache();
        }
    }

    fn invalidate_cache_range(&mut self, addr: u64, size: usize) {
        if let Some(jit) = self.jit.as_mut() {
            jit.invalidate_cache_range(addr, size as u64);
        }
    }

    fn get_architecture(&self) -> Architecture {
        Architecture::AArch64
    }

    fn get_context(&self, ctx: &mut ThreadContext) {
        let jit = match self.jit.as_ref() {
            Some(jit) => jit,
            None => {
                log::warn!("ArmDynarmic64::get_context: JIT not available");
                return;
            }
        };

        // Upstream: GPRs[0..29] -> ctx.r[0..29], GPR[29] -> ctx.fp, GPR[30] -> ctx.lr
        for i in 0..29 {
            ctx.r[i] = jit.get_register(i);
        }
        ctx.fp = jit.get_register(29);
        ctx.lr = jit.get_register(30);

        ctx.sp = jit.get_sp();
        ctx.pc = jit.get_pc();
        ctx.pstate = jit.get_pstate();

        // Vector registers
        for i in 0..32 {
            let (lo, hi) = jit.get_vector(i);
            ctx.v[i] = (lo as u128) | ((hi as u128) << 64);
        }

        ctx.fpcr = jit.get_fpcr();
        ctx.fpsr = jit.get_fpsr();
        ctx.tpidr = jit.get_tpidr_el0();
    }

    fn set_context(&mut self, ctx: &ThreadContext) {
        let jit = match self.jit.as_mut() {
            Some(jit) => jit,
            None => {
                log::warn!("ArmDynarmic64::set_context: JIT not available");
                return;
            }
        };

        // Upstream: ctx.r[0..29] -> GPRs[0..29], ctx.fp -> GPR[29], ctx.lr -> GPR[30]
        for i in 0..29 {
            jit.set_register(i, ctx.r[i]);
        }
        jit.set_register(29, ctx.fp);
        jit.set_register(30, ctx.lr);

        jit.set_sp(ctx.sp);
        jit.set_pc(ctx.pc);
        jit.set_pstate(ctx.pstate);

        // Vector registers
        for i in 0..32 {
            let lo = ctx.v[i] as u64;
            let hi = (ctx.v[i] >> 64) as u64;
            jit.set_vector(i, lo, hi);
        }

        jit.set_fpcr(ctx.fpcr);
        jit.set_fpsr(ctx.fpsr);
        *self.tpidr_el0 = ctx.tpidr;
        jit.set_tpidr_el0(ctx.tpidr);
    }

    fn set_tpidrro_el0(&mut self, value: u64) {
        *self.tpidrro_el0 = value;
        if let Some(jit) = self.jit.as_mut() {
            jit.set_tpidrro_el0(value);
        }
    }

    fn set_watchpoint_array(&mut self, watchpoints: *const WatchpointArray) {
        self.base.set_watchpoint_array(watchpoints);
    }

    fn get_tpidrro_el0(&self) -> u64 {
        self.jit
            .as_ref()
            .map(|jit| jit.get_tpidrro_el0())
            .unwrap_or(*self.tpidrro_el0)
    }

    fn get_svc_arguments(&self, args: &mut [u64; 8]) {
        let jit = match self.jit.as_ref() {
            Some(jit) => jit,
            None => {
                log::warn!("ArmDynarmic64::get_svc_arguments: JIT not available");
                return;
            }
        };

        // Upstream: reads j.GetRegister(0..8)
        for i in 0..8 {
            args[i] = jit.get_register(i);
        }
        if trace_a64_svc_regs_enabled() {
            eprintln!(
                "[A64_SVC_ARGS] svc=0x{:x} pc=0x{:016x} lr=0x{:016x} sp=0x{:016x} x0=0x{:016x} x1=0x{:016x} x2=0x{:016x} x3=0x{:016x} x4=0x{:016x} x5=0x{:016x} x6=0x{:016x} x7=0x{:016x} x21=0x{:016x} x22=0x{:016x} x24=0x{:016x} x25=0x{:016x}",
                self.svc.load(Ordering::Relaxed),
                jit.get_pc(),
                jit.get_register(30),
                jit.get_sp(),
                args[0],
                args[1],
                args[2],
                args[3],
                args[4],
                args[5],
                args[6],
                args[7],
                jit.get_register(21),
                jit.get_register(22),
                jit.get_register(24),
                jit.get_register(25),
            );
        }
    }

    fn set_svc_arguments(&mut self, args: &[u64; 8]) {
        let jit = match self.jit.as_mut() {
            Some(jit) => jit,
            None => {
                log::warn!("ArmDynarmic64::set_svc_arguments: JIT not available");
                return;
            }
        };

        // Upstream: writes j.SetRegister(0..8, args[i])
        for i in 0..8 {
            jit.set_register(i, args[i]);
        }
    }

    fn get_svc_number(&self) -> u32 {
        self.svc.load(Ordering::Relaxed)
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
        // Upstream: m_jit->HaltExecution(BreakLoop)
        if let Some(jit) = self.jit.as_ref() {
            jit.halt_execution(rdynarmic::halt_reason::HaltReason::EXTERNAL_HALT);
        }
    }

    fn halted_watchpoint(&self) -> Option<DebugWatchpoint> {
        *self.halted_watchpoint.lock().unwrap()
    }

    fn rewind_breakpoint_instruction(&mut self) {
        // Upstream: this->SetContext(m_breakpoint_context)
        let ctx = self.breakpoint_context.lock().unwrap().clone();
        self.set_context(&ctx);
    }
}

fn thread_context_from_jit_state(jit_state: &A64JitState, pc: u64) -> ThreadContext {
    let mut ctx = ThreadContext::default();
    ctx.r.copy_from_slice(&jit_state.reg[..29]);
    ctx.fp = jit_state.reg[29];
    ctx.lr = jit_state.reg[30];
    ctx.sp = jit_state.sp;
    ctx.pc = pc;
    ctx.pstate = jit_state.get_pstate();
    for i in 0..32 {
        let lo = jit_state.vec[i * 2] as u128;
        let hi = (jit_state.vec[i * 2 + 1] as u128) << 64;
        ctx.v[i] = lo | hi;
    }
    ctx.fpcr = jit_state.get_fpcr();
    ctx.fpsr = jit_state.get_fpsr();
    ctx.tpidr = jit_state.tpidr_el0;
    ctx
}

#[cfg(test)]
mod tests {
    use super::{
        parse_optimization_mask_env, parse_watch_ranges, thread_context_from_jit_state,
        translate_halt_reason, upstream_optimization_config, DynarmicCallbacks64,
    };
    use crate::arm::arm_interface::{
        ArmInterfaceBase, DebugWatchpoint, DebugWatchpointType, HaltReason,
    };
    use crate::core_timing::CoreTiming;
    use crate::hle::kernel::k_process::ProcessMemoryData;
    use crate::hle::kernel::k_typed_address::KProcessAddress;
    use common::settings_enums::CpuAccuracy;
    use rdynarmic::backend::x64::jit_state::A64JitState;
    use rdynarmic::jit_config::{OptimizationFlag, UserCallbacks};
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, RwLock};

    #[test]
    fn parse_watch_ranges_accepts_hex_and_default_size() {
        assert_eq!(
            parse_watch_ranges("0x10,0x20:16"),
            vec![(0x10, 0x18), (0x20, 0x30)]
        );
    }

    #[test]
    fn thread_context_snapshot_matches_jit_state_layout() {
        let mut jit_state = A64JitState::new();
        for i in 0..31 {
            jit_state.reg[i] = 0x1000 + i as u64;
        }
        jit_state.sp = 0x2222;
        jit_state.set_pstate(0xA000_0000);
        jit_state.vec[0] = 0x0123_4567_89AB_CDEF;
        jit_state.vec[1] = 0x0FED_CBA9_7654_3210;
        jit_state.set_fpcr(0x0100_0000);
        jit_state.set_fpsr(0x0800_001F);
        jit_state.tpidr_el0 = 0x3333;

        let ctx = thread_context_from_jit_state(&jit_state, 0x4444);
        assert_eq!(ctx.r[0], 0x1000);
        assert_eq!(ctx.r[28], 0x1000 + 28);
        assert_eq!(ctx.fp, 0x1000 + 29);
        assert_eq!(ctx.lr, 0x1000 + 30);
        assert_eq!(ctx.sp, 0x2222);
        assert_eq!(ctx.pc, 0x4444);
        assert_eq!(ctx.pstate, 0xA000_0000);
        assert_eq!(ctx.v[0], 0x0FED_CBA9_7654_3210_0123_4567_89AB_CDEFu128);
        assert_eq!(ctx.fpcr, jit_state.get_fpcr());
        assert_eq!(ctx.fpsr, jit_state.get_fpsr());
        assert_eq!(ctx.tpidr, 0x3333);
    }

    #[test]
    fn parse_optimization_mask_env_accepts_hex() {
        unsafe {
            std::env::set_var("RUZU_A64_TEST_OPT_MASK", "0x3f");
        }
        assert_eq!(
            parse_optimization_mask_env("RUZU_A64_TEST_OPT_MASK"),
            Some(0x3f)
        );
        unsafe {
            std::env::remove_var("RUZU_A64_TEST_OPT_MASK");
        }
    }

    #[test]
    fn auto_and_paranoid_optimization_configs_match_upstream_a64() {
        let (auto_flags, auto_unsafe, auto_fastmem_bits) =
            upstream_optimization_config(39, CpuAccuracy::Auto, false, false, false, false, false);
        assert!(auto_unsafe);
        assert_eq!(auto_fastmem_bits, 64);
        assert!(auto_flags.contains(OptimizationFlag::UNSAFE_UNFUSE_FMA));
        assert!(!auto_flags.contains(OptimizationFlag::UNSAFE_IGNORE_GLOBAL_MONITOR));
        assert!(!auto_flags.contains(OptimizationFlag::UNSAFE_REDUCED_ERROR_FP));
        assert!(!auto_flags.contains(OptimizationFlag::UNSAFE_INACCURATE_NAN));

        let (paranoid_flags, paranoid_unsafe, paranoid_fastmem_bits) =
            upstream_optimization_config(39, CpuAccuracy::Paranoid, true, true, true, true, true);
        assert_eq!(paranoid_flags, OptimizationFlag::NO_OPTIMIZATIONS);
        assert!(!paranoid_unsafe);
        assert_eq!(paranoid_fastmem_bits, 39);

        let (_, accurate_unsafe, accurate_fastmem_bits) = upstream_optimization_config(
            36,
            CpuAccuracy::Accurate,
            false,
            false,
            false,
            false,
            false,
        );
        assert!(!accurate_unsafe);
        assert_eq!(accurate_fastmem_bits, 36);
    }

    #[test]
    fn translate_halt_reason_uses_upstream_a64_bits() {
        let hr = rdynarmic::halt_reason::HaltReason::STEP
            | rdynarmic::halt_reason::HaltReason::MEMORY_ABORT
            | rdynarmic::halt_reason::HaltReason::SVC
            | rdynarmic::halt_reason::HaltReason::BREAKPOINT
            | rdynarmic::halt_reason::HaltReason::PREFETCH_ABORT
            | rdynarmic::halt_reason::HaltReason::EXTERNAL_HALT;
        let translated = translate_halt_reason(hr);
        assert!(translated.contains(HaltReason::STEP_THREAD));
        assert!(translated.contains(HaltReason::DATA_ABORT));
        assert!(translated.contains(HaltReason::SUPERVISOR_CALL));
        assert!(translated.contains(HaltReason::INSTRUCTION_BREAKPOINT));
        assert!(translated.contains(HaltReason::PREFETCH_ABORT));
        assert!(translated.contains(HaltReason::BREAK_LOOP));
    }

    #[test]
    fn translate_halt_reason_ignores_legacy_exception_raised_bit() {
        let translated =
            translate_halt_reason(rdynarmic::halt_reason::HaltReason::EXCEPTION_RAISED);
        assert!(translated.is_empty());
    }

    #[test]
    fn debugger_memory_check_retains_matching_watchpoint_and_halts() {
        let mut memory = ProcessMemoryData::new();
        memory.base = 0x1000;
        memory.data = vec![0; 0x1000];

        let mut watchpoints =
            [DebugWatchpoint::default(); crate::hardware_properties::NUM_WATCHPOINTS as usize];
        watchpoints[0] = DebugWatchpoint {
            start_address: KProcessAddress::new(0x1100),
            end_address: KProcessAddress::new(0x1110),
            type_: DebugWatchpointType::WRITE,
        };
        let mut interface = ArmInterfaceBase::new(false);
        interface.set_watchpoint_array(&watchpoints);
        let halted_watchpoint = Arc::new(Mutex::new(None));
        let mut callbacks = DynarmicCallbacks64::new(
            Arc::new(RwLock::new(memory)),
            None,
            Arc::new(AtomicU32::new(0)),
            false,
            Arc::new(CoreTiming::new()),
            Arc::new(AtomicU64::new(0)),
            Arc::new(Mutex::new(Default::default())),
            std::ptr::null_mut(),
            0,
            true,
            interface.shared_watchpoint_array(),
            Arc::clone(&halted_watchpoint),
        );
        let halt_reason = AtomicU32::new(0);
        callbacks.set_halt_reason_ptr(halt_reason.as_ptr());

        assert!(!callbacks.check_memory_access(0x1108, 4, DebugWatchpointType::WRITE));
        assert_eq!(
            halted_watchpoint
                .lock()
                .unwrap()
                .as_ref()
                .map(|watchpoint| watchpoint.start_address.get()),
            Some(0x1100)
        );
        assert_ne!(
            halt_reason.load(Ordering::SeqCst)
                & rdynarmic::halt_reason::HaltReason::MEMORY_ABORT.bits(),
            0
        );
    }
}
