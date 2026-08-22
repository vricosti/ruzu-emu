//! Port of zuyu/src/core/memory.h / memory.cpp
//! Status: EN COURS
//! Derniere synchro: 2026-03-17
//!
//! Core::Memory::Memory — bridges KPageTableBase with the dynarmic page table
//! and the DeviceMemory backing store.

#[cfg(target_os = "android")]
use common::heap_tracker::HeapTracker;
use common::host_memory::HostMemory;
use common::page_table::{PageInfo, PageTable, PageType};
use common::scratch_buffer::ScratchBuffer;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};

use crate::core::SystemRef;
use crate::device_memory::{dram_memory_map, DeviceMemory};
use crate::gpu_core::RasterizerDownloadArea;
use crate::gpu_dirty_memory_manager::GpuDirtyMemoryManager;
use crate::hardware_properties;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};

/// Page size constants matching upstream YUZU_PAGEBITS / YUZU_PAGESIZE.
const PAGE_BITS: usize = 12;
const PAGE_SIZE: u64 = 1 << PAGE_BITS;
const PAGE_MASK: u64 = PAGE_SIZE - 1;

static RASTERIZER_MARK_CACHED_LAST_STAGE: AtomicU64 = AtomicU64::new(0);
static RASTERIZER_MARK_CACHED_COUNTS: [AtomicU64; 9] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

fn new_rasterizer_read_areas(
) -> [Mutex<RasterizerDownloadArea>; hardware_properties::NUM_CPU_CORES as usize] {
    std::array::from_fn(|_| Mutex::new(RasterizerDownloadArea::default()))
}

fn record_rasterizer_mark_cached_stage(stage: usize) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ENABLED
        .get_or_init(|| std::env::var_os("RUZU_PROFILE_RASTERIZER_MARK_CACHED_STALL").is_some())
    {
        return;
    }
    RASTERIZER_MARK_CACHED_LAST_STAGE.store(stage as u64, Ordering::Relaxed);
    if let Some(counter) = RASTERIZER_MARK_CACHED_COUNTS.get(stage) {
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn dump_rasterizer_mark_cached_stall_profile() {
    if RASTERIZER_MARK_CACHED_COUNTS[0].load(Ordering::Relaxed) == 0 {
        return;
    }
    const NAMES: [&str; 9] = [
        "enter",
        "after_guard",
        "after_page_table",
        "after_num_pages",
        "before_page_loop",
        "in_page_loop",
        "after_page_loop",
        "exit",
        "early_return",
    ];
    let last_stage = RASTERIZER_MARK_CACHED_LAST_STAGE.load(Ordering::Relaxed) as usize;
    let last_stage_name = NAMES.get(last_stage).copied().unwrap_or("unknown");
    eprintln!(
        "[RASTERIZER_MARK_CACHED_STALL_PROFILE] last_stage={} ({})",
        last_stage, last_stage_name
    );
    for (index, name) in NAMES.iter().enumerate() {
        eprintln!(
            "[RASTERIZER_MARK_CACHED_STALL_PROFILE]   {:02} {:<24} {}",
            index,
            name,
            RASTERIZER_MARK_CACHED_COUNTS[index].load(Ordering::Relaxed)
        );
    }
}

/// Memory permission for mapping operations.
/// Matches upstream Common::MemoryPermission.
pub use common::host_memory::MemoryPermission;

/// Port of Core::Memory::Memory.
///
/// Manages the mapping between guest virtual addresses, physical addresses
/// (in DeviceMemory), and host pointers (in the PageTable used by dynarmic).
pub struct Memory {
    /// Upstream owner: `Core::System& system`.
    system: SystemRef,
    /// Pointer to the device memory backing store.
    device_memory: *const DeviceMemory,
    /// Pointer to the HostMemory buffer (used for fastmem arena base).
    buffer: *const HostMemory,
    /// On Android: HeapTracker wrapping HostMemory for separate heap fault handling.
    /// Upstream: `std::optional<Common::HeapTracker> heap_tracker` + `HeapTracker* buffer`.
    #[cfg(target_os = "android")]
    heap_tracker: Option<Box<HeapTracker>>,
    /// Current page table (set by SetCurrentPageTable when switching processes).
    current_page_table: *mut PageTable,
    /// Upstream owner: `rasterizer_read_areas[Core::Hardware::NUM_CPU_CORES]`.
    rasterizer_read_areas:
        [Mutex<RasterizerDownloadArea>; hardware_properties::NUM_CPU_CORES as usize],
    /// Upstream owner: `rasterizer_write_areas[Core::Hardware::NUM_CPU_CORES]`.
    rasterizer_write_areas: [Mutex<GpuDirtyState>; hardware_properties::NUM_CPU_CORES as usize],
    /// Upstream owner: `std::span<Core::GPUDirtyMemoryManager> gpu_dirty_managers`.
    gpu_dirty_managers: Vec<Arc<GpuDirtyMemoryManager>>,
    /// Port of upstream `scratch_buffers[Core::Hardware::NUM_CPU_CORES]`.
    smmu_scratch_buffers: [Mutex<ScratchBuffer<u32>>; hardware_properties::NUM_CPU_CORES as usize],
    /// Serializes non-core host threads sharing the last per-core GPU cache
    /// slot. Upstream owner: `std::mutex sys_core_guard`.
    sys_core_guard: Arc<Mutex<()>>,
}

#[derive(Default)]
struct GpuDirtyState {
    last_address: u64,
}

/// Parse a `RUZU_WATCH_BLOCK=ADDR:LEN[,ADDR:LEN...]` spec into byte ranges.
fn parse_block_watch_ranges() -> Vec<(u64, u64)> {
    let raw = match std::env::var("RUZU_WATCH_BLOCK") {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for tok in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let (addr, size) = match tok.split_once(':') {
            Some((a, s)) => (a, s.parse::<u64>().unwrap_or(8)),
            None => (tok, 8),
        };
        let addr_str = addr
            .strip_prefix("0x")
            .or_else(|| addr.strip_prefix("0X"))
            .unwrap_or(addr);
        if let Ok(start) = u64::from_str_radix(addr_str, 16) {
            out.push((start, start.saturating_add(size)));
        }
    }
    out
}

fn check_block_watch(kind: &str, dest_addr: u64, src: &[u8]) {
    use std::sync::OnceLock;
    static RANGES: OnceLock<Vec<(u64, u64)>> = OnceLock::new();
    let ranges = RANGES.get_or_init(parse_block_watch_ranges);
    if ranges.is_empty() {
        return;
    }
    let end = dest_addr.saturating_add(src.len() as u64);
    let Some(&(rs, re)) = ranges.iter().find(|(s, e)| dest_addr < *e && end > *s) else {
        return;
    };
    let bt = std::backtrace::Backtrace::force_capture();
    let dump_len = src.len().min(64);
    eprintln!(
        "[BLOCK_WATCH:{kind}] dest=0x{dest_addr:016X} len={} (range hit 0x{rs:X}..0x{re:X}) bytes[..{dump_len}]={:02x?}\n{bt}",
        src.len(),
        &src[..dump_len]
    );
}

fn trace_unmapped_guest_access(kind: &str, vaddr: u64, bits: usize) {
    if let Ok(after_ms) = std::env::var("RUZU_TRACE_UNMAPPED_GUEST_AFTER_MS") {
        if let Ok(after_ms) = after_ms.parse::<u128>() {
            use std::sync::OnceLock;
            static START: OnceLock<std::time::Instant> = OnceLock::new();
            if START
                .get_or_init(std::time::Instant::now)
                .elapsed()
                .as_millis()
                < after_ms
            {
                return;
            }
        }
    }

    let trace_all = common::env_flag!("RUZU_TRACE_UNMAPPED_GUEST_ALL");
    let trace_suspicious = common::env_flag!("RUZU_TRACE_UNMAPPED_GUEST")
        && (vaddr < 0x1000 || (vaddr >> 32) == 0xffff_ffff);
    if !trace_all && !trace_suspicious {
        return;
    }

    use std::sync::atomic::{AtomicU32, Ordering};
    static SHOWN: AtomicU32 = AtomicU32::new(0);
    let n = SHOWN.fetch_add(1, Ordering::Relaxed);
    if n >= 64 {
        return;
    }

    let tid = crate::hle::kernel::kernel::get_current_thread_id_fast().unwrap_or(0);
    let (core, regs) = crate::hle::kernel::kernel::with_current_thread_fast_mut(|t| {
        (
            t.get_current_core().max(0) as usize,
            [
                t.thread_context.r[0],
                t.thread_context.r[1],
                t.thread_context.r[2],
                t.thread_context.r[3],
                t.thread_context.r[4],
                t.thread_context.r[5],
                t.thread_context.r[6],
                t.thread_context.r[7],
                t.thread_context.fp,
                t.thread_context.sp,
                t.thread_context.lr,
            ],
        )
    })
    .unwrap_or((usize::MAX, [0; 11]));
    let (pc, lr) = if core < crate::hle::kernel::kernel::GUEST_PC.len() {
        (
            crate::hle::kernel::kernel::GUEST_PC[core].load(Ordering::Acquire),
            crate::hle::kernel::kernel::GUEST_LR[core].load(Ordering::Acquire),
        )
    } else {
        (0, 0)
    };
    log::error!(
        "[UNMAPPED_GUEST_{kind}] #{} tid={} core={} pc=0x{:08X} lr=0x{:08X} vaddr=0x{:X} bits={} r0=0x{:08X} r1=0x{:08X} r2=0x{:08X} r3=0x{:08X} r4=0x{:08X} r5=0x{:08X} r6=0x{:08X} r7=0x{:08X} fp=0x{:08X} sp=0x{:08X} ctx_lr=0x{:08X}",
        n,
        tid,
        core,
        pc,
        lr,
        vaddr,
        bits,
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
    );
}

/// RUZU_TRACE_GET_POINTER_PAGE=0xPAGEVADDR — log every get_pointer*
/// call whose returned guest vaddr lies in the same 4 KB page as
/// PAGEVADDR. Includes a backtrace so we can identify the HLE caller
/// that's writing through a host pointer (bypassing write_64/
/// write_block). Throttled to avoid log spam. Used to hunt the STK
/// wedge: corruption at slot 0x814903F8 with no visible writer in the
/// W64 fastmem-direct, W64 slow-path callback, write_block, or
/// memory_write_128 paths.
fn trace_get_pointer_page(kind: &str, vaddr: u64) {
    use std::sync::OnceLock;
    static TARGET_PAGE: OnceLock<Option<u64>> = OnceLock::new();
    let target = TARGET_PAGE.get_or_init(|| {
        std::env::var("RUZU_TRACE_GET_POINTER_PAGE")
            .ok()
            .and_then(|s| u64::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
            .map(|v| v & !0xFFFu64)
    });
    let Some(target_page) = target else { return };
    let vaddr_page = vaddr & !0xFFFu64;
    if vaddr_page != *target_page {
        return;
    }
    use std::sync::atomic::{AtomicU32, Ordering};
    static SHOWN: AtomicU32 = AtomicU32::new(0);
    let n = SHOWN.fetch_add(1, Ordering::Relaxed);
    if n < 40 || n.is_multiple_of(1000) {
        let bt = std::backtrace::Backtrace::force_capture();
        eprintln!("[GET_PTR_PAGE:{kind} #{n}] vaddr=0x{vaddr:016X}\n{bt}");
    }
}

/// Env-gated range tracer used by `write_8`/`write_16`/`write_32` to log every
/// host-side guest-memory write whose vaddr falls in a target range.
///
/// `RUZU_TRACE_MEMORY_W_RANGE="0xSTART:0xEND,..."` — log every write whose
/// `[vaddr, vaddr+size)` intersects any listed `[START, END)` half-open range.
/// Catches host-issued writes from any of the write_N helpers (the kernel /
/// HLE writers, plus the JIT memory-write callback path through write_32).
/// Combine with `RUZU_NO_FASTMEM_W32=1`/`_W16`/`_W8` to also catch the JIT
/// fastmem path (otherwise guest stores via fastmem bypass these helpers).

/// Parse a `0xVADDR`-style diagnostic env var ONCE; hot paths pay a single
/// atomic load afterwards. `std::env::var` takes a process-global lock, so
/// per-access lookups on memory hot paths serialize every thread.
fn cached_hex_env(cell: &'static std::sync::OnceLock<Option<u64>>, key: &str) -> Option<u64> {
    *cell.get_or_init(|| {
        std::env::var(key).ok().and_then(|s| {
            let s = s.trim();
            let s = s
                .strip_prefix("0x")
                .or_else(|| s.strip_prefix("0X"))
                .unwrap_or(s);
            u64::from_str_radix(s, 16).ok()
        })
    })
}

fn maybe_trace_write_in_range(vaddr: u64, size: u64, data: u64) {
    use std::sync::OnceLock;
    static RANGES: OnceLock<Vec<(u64, u64)>> = OnceLock::new();
    let ranges = RANGES.get_or_init(|| {
        let raw = match std::env::var("RUZU_TRACE_MEMORY_W_RANGE") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        raw.split(',')
            .filter_map(|tok| {
                let tok = tok.trim();
                let mut parts = tok.split(':');
                let start =
                    u64::from_str_radix(parts.next()?.trim().trim_start_matches("0x"), 16).ok()?;
                let end =
                    u64::from_str_radix(parts.next()?.trim().trim_start_matches("0x"), 16).ok()?;
                Some((start, end))
            })
            .collect()
    });
    if ranges.is_empty() {
        return;
    }
    let write_end = vaddr + size;
    for &(s, e) in ranges {
        if vaddr < e && s < write_end {
            let bt = std::backtrace::Backtrace::force_capture();
            eprintln!(
                "[MEMORY_W{:01}] vaddr=0x{:016X} data=0x{:0width$X}\n{}",
                size * 8,
                vaddr,
                data,
                bt,
                width = (size as usize) * 2
            );
            break;
        }
    }
}

fn parse_trace_write_values() -> Vec<u64> {
    let Ok(raw) = std::env::var("RUZU_TRACE_MEMORY_W_VALUE") else {
        return Vec::new();
    };
    raw.split(',')
        .filter_map(|tok| {
            let tok = tok.trim();
            let tok = tok
                .strip_prefix("0x")
                .or_else(|| tok.strip_prefix("0X"))
                .unwrap_or(tok);
            u64::from_str_radix(tok, 16).ok()
        })
        .collect()
}

fn maybe_trace_write_value(kind: &str, vaddr: u64, size: u64, data: u64) {
    use std::sync::OnceLock;
    static VALUES: OnceLock<Vec<u64>> = OnceLock::new();
    let values = VALUES.get_or_init(parse_trace_write_values);
    if values.is_empty() {
        return;
    }
    let mask = if size >= 8 {
        u64::MAX
    } else {
        (1u64 << (size * 8)) - 1
    };
    let data = data & mask;
    if !values.iter().any(|&value| (value & mask) == data) {
        return;
    }
    let bt = std::backtrace::Backtrace::force_capture();
    eprintln!(
        "[MEMORY_W_VALUE:{kind}] vaddr=0x{vaddr:016X} size={size} data=0x{data:0width$X}\n{bt}",
        width = (size as usize) * 2
    );
}

fn maybe_trace_write_block_values(kind: &str, dest_addr: u64, src: &[u8]) {
    use std::sync::OnceLock;
    static VALUES: OnceLock<Vec<u64>> = OnceLock::new();
    let values = VALUES.get_or_init(parse_trace_write_values);
    if values.is_empty() || src.len() < 4 {
        return;
    }
    for offset in 0..=src.len() - 4 {
        let value = u32::from_le_bytes(src[offset..offset + 4].try_into().unwrap()) as u64;
        if values.iter().any(|&target| (target & 0xFFFF_FFFF) == value) {
            let bt = std::backtrace::Backtrace::force_capture();
            let vaddr = dest_addr + offset as u64;
            eprintln!(
                "[MEMORY_W_VALUE:{kind}] vaddr=0x{vaddr:016X} size=4 data=0x{value:08X} block_dest=0x{dest_addr:016X} block_len=0x{:X}\n{bt}",
                src.len()
            );
            return;
        }
    }
}

// SAFETY: Memory is used behind Arc<Mutex<>> and all raw pointers are
// to long-lived objects (DeviceMemory, HostMemory, PageTable) that outlive Memory.
unsafe impl Send for Memory {}
unsafe impl Sync for Memory {}

impl Memory {
    /// Create a new Memory instance.
    ///
    /// # Safety
    /// The caller must ensure that `device_memory` and `buffer` outlive this Memory.
    pub unsafe fn new(
        system: SystemRef,
        device_memory: *const DeviceMemory,
        buffer: *const HostMemory,
    ) -> Self {
        Self {
            system,
            device_memory,
            buffer,
            #[cfg(target_os = "android")]
            heap_tracker: None,
            current_page_table: std::ptr::null_mut(),
            rasterizer_read_areas: new_rasterizer_read_areas(),
            rasterizer_write_areas: std::array::from_fn(|_| Mutex::new(GpuDirtyState::default())),
            gpu_dirty_managers: Vec::new(),
            smmu_scratch_buffers: std::array::from_fn(|_| Mutex::new(ScratchBuffer::new())),
            sys_core_guard: Arc::new(Mutex::new(())),
        }
    }

    /// Return the non-owning system reference held by this memory instance.
    ///
    /// `KPageTableBase` owns the same `Core::System&` upstream and uses it
    /// during finalization to tear down fastmem mappings directly through
    /// `DeviceMemory`.
    pub fn system_ref(&self) -> SystemRef {
        self.system
    }

    /// Upstream: `Memory::SetGPUDirtyManagers(std::span<Core::GPUDirtyMemoryManager>)`.
    pub fn set_gpu_dirty_managers(&mut self, managers: Vec<Arc<GpuDirtyMemoryManager>>) {
        self.gpu_dirty_managers = managers;
    }

    /// Get the fastmem arena base pointer (for JIT direct memory access).
    /// Returns null if DeviceMemory buffer is not available.
    pub fn fastmem_pointer(&self) -> *mut u8 {
        if self.buffer.is_null() {
            std::ptr::null_mut()
        } else {
            unsafe { (*self.buffer).virtual_base_pointer() }
        }
    }

    /// Return the host backing base for the process `DeviceMemory`.
    ///
    /// Upstream `Core::DeviceMemoryManager` stores this as `physical_base`
    /// from `DeviceMemory().buffer.BackingBasePointer()`. Host1x needs the
    /// same value to derive raw physical page indices from `GetPointerSilent`
    /// results.
    pub fn device_memory_backing_base(&self) -> Option<usize> {
        if self.device_memory.is_null() {
            None
        } else {
            Some(unsafe { (*self.device_memory).buffer.backing_base_pointer() as usize })
        }
    }

    /// Raw pointer to current page table (for diagnostics).
    pub fn current_page_table_raw(&self) -> *mut PageTable {
        self.current_page_table
    }

    /// Set the current page table and wire up the fastmem arena.
    /// Matches upstream `Memory::Impl::SetCurrentPageTable`.
    pub fn set_current_page_table(&mut self, page_table: *mut PageTable, is_application: bool) {
        self.current_page_table = page_table;
        if !page_table.is_null() && !self.buffer.is_null() {
            let pt = unsafe { &mut *page_table };
            let settings = common::settings::values();
            pt.fastmem_arena = if is_application && common::settings::is_fastmem_enabled(&settings)
            {
                unsafe { (*self.buffer).virtual_base_pointer() }
            } else {
                std::ptr::null_mut()
            };

            // On Android, create a HeapTracker wrapping the HostMemory buffer.
            // Upstream: heap_tracker.emplace(system.DeviceMemory().buffer);
            //           buffer = std::addressof(*heap_tracker);
            #[cfg(target_os = "android")]
            {
                let host_mem = unsafe { &mut *(self.buffer as *mut HostMemory) };
                self.heap_tracker = Some(Box::new(HeapTracker::new(host_mem)));
            }

            // RUZU_POLL_DIVERGE=0xVADDR[,0xVADDR,...] — spawn a background
            // thread that compares the value at each VADDR via two views:
            // (a) the VIRTUAL fastmem-arena pointer (= what JIT-direct sees)
            // (b) the BACKING region pointer (= what slow-path callbacks see).
            // If these differ, the wedge-causing coherency divergence is
            // confirmed. Logs first 64 divergences then every 1000th.
            // Useful only when something else triggers the wedge concurrently
            // (e.g. STK in multi-core mode), since slot 0x81490350 is only
            // mutated by JIT-direct writes.
            if let Ok(spec) = std::env::var("RUZU_POLL_DIVERGE") {
                let vaddrs: Vec<u64> = spec
                    .split(',')
                    .filter_map(|s| u64::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
                    .collect();
                if !vaddrs.is_empty() {
                    let arena = pt.fastmem_arena as usize;
                    let pt_ptr = page_table as usize;
                    std::thread::Builder::new()
                        .name("ruzu-poll-diverge".into())
                        .spawn(move || {
                            use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
                            static FIRES: AtomicU64 = AtomicU64::new(0);
                            static TRAP_FIRED: AtomicBool = AtomicBool::new(false);
                            // Snapshot last seen values to avoid flooding.
                            let mut last_arena = vec![0u64; vaddrs.len()];
                            let mut last_backing = vec![0u64; vaddrs.len()];
                            // RUZU_POLL_DIVERGE_SLEEP_US=N — poll interval in microseconds.
                            // Default 50ms. Set 0 for tight spin loop.
                            let sleep_us: u64 = std::env::var("RUZU_POLL_DIVERGE_SLEEP_US")
                                .ok()
                                .and_then(|s| s.parse::<u64>().ok())
                                .unwrap_or(50_000);
                            // RUZU_POLL_TRAP_CORRUPT=1 — on first detection of the
                            // known STK corruption pattern at any polled vaddr, dump
                            // all thread states (from /proc/self/task) and SIGSTOP the
                            // whole process. The user can then `gdb -p PID` to inspect
                            // all 4 JIT thread CPU contexts without any prior gdb
                            // attachment (which is what perturbs timing and hides the
                            // wedge — the Heisenbug). This is non-perturbing for the
                            // writer: only reads volatile memory; the SIGSTOP fires
                            // after corruption has been committed.
                            //
                            // Custom pattern: RUZU_POLL_TRAP_CORRUPT_MASK=0xMASK and
                            // RUZU_POLL_TRAP_CORRUPT_VALUE=0xVALUE — trip when
                            // (val & mask) == value. Default (no mask/value given):
                            // (val >> 32) & 0xFFFF == 0x2101 && (val >> 48) & 0xFFFF
                            // == 0  (matches the 0x2101A3B140A0-class corruption).
                            let trap_enabled = std::env::var_os("RUZU_POLL_TRAP_CORRUPT")
                                .is_some();
                            let trap_mask: u64 = std::env::var("RUZU_POLL_TRAP_CORRUPT_MASK")
                                .ok()
                                .and_then(|s| {
                                    u64::from_str_radix(s.trim().trim_start_matches("0x"), 16)
                                        .ok()
                                })
                                .unwrap_or(0xFFFF_FFFF_0000_0000);
                            let trap_value: u64 = std::env::var("RUZU_POLL_TRAP_CORRUPT_VALUE")
                                .ok()
                                .and_then(|s| {
                                    u64::from_str_radix(s.trim().trim_start_matches("0x"), 16)
                                        .ok()
                                })
                                .unwrap_or(0x0000_2101_0000_0000);
                            let is_corrupt = move |v: u64| -> bool {
                                trap_enabled && (v & trap_mask) == trap_value
                            };
                            let do_trap = move |label: &str, vaddr: u64, val: u64| {
                                if TRAP_FIRED.swap(true, Ordering::SeqCst) {
                                    return;
                                }
                                eprintln!(
                                    "[POLL_TRAP_CORRUPT] {} vaddr=0x{:016X} val=0x{:016X} mask=0x{:016X} expect=0x{:016X}",
                                    label, vaddr, val, trap_mask, trap_value,
                                );
                                eprintln!("[POLL_TRAP_CORRUPT] dumping /proc/self/task/*/stat …");
                                if let Ok(entries) = std::fs::read_dir("/proc/self/task") {
                                    for ent in entries.flatten() {
                                        let p = ent.path();
                                        let tid = p.file_name()
                                            .and_then(|s| s.to_str())
                                            .unwrap_or("?")
                                            .to_string();
                                        let comm = std::fs::read_to_string(p.join("comm"))
                                            .unwrap_or_else(|_| String::from("?"));
                                        let stat = std::fs::read_to_string(p.join("stat"))
                                            .unwrap_or_else(|_| String::from("?"));
                                        // /proc/.../stat field 30 is `kstkeip` — kernel-recorded
                                        // instruction pointer. Pre-parsed cheap view:
                                        let fields: Vec<&str> = stat.split_whitespace().collect();
                                        let kstkeip = fields.get(29).copied().unwrap_or("?");
                                        let state = fields.get(2).copied().unwrap_or("?");
                                        eprintln!(
                                            "  tid={:>6} state={} kstkeip=0x{} comm={}",
                                            tid, state, kstkeip.trim_start_matches("0"),
                                            comm.trim_end(),
                                        );
                                    }
                                }
                                #[cfg(target_os = "linux")]
                                {
                                    eprintln!(
                                        "[POLL_TRAP_CORRUPT] raising SIGSTOP. Attach gdb to inspect: gdb -p {}",
                                        unsafe { libc::getpid() },
                                    );
                                    unsafe { libc::raise(libc::SIGSTOP) };
                                }
                                #[cfg(not(target_os = "linux"))]
                                eprintln!(
                                    "[POLL_TRAP_CORRUPT] automatic SIGSTOP is unavailable on this platform"
                                );
                            };
                            // SEGV-safety: we DO NOT dereference pt_ptr in the loop. The
                            // PageTable pointer captured at thread spawn can be freed at
                            // any later point (process tear-down / set_current_page_table
                            // rebinding to a different process). Using it across that
                            // boundary is a UAF. Also: the fastmem arena is a 512GB
                            // sparse mmap with PROT_NONE on unmapped pages, so a direct
                            // volatile read at vaddr+arena SEGVs before the page is
                            // mapped. mincore() doesn't help (the page IS in a VMA, just
                            // PROT_NONE). We use process_vm_readv() which copies from
                            // another (or our own) process's address space via the kernel
                            // and returns -EFAULT cleanly on unreadable addresses without
                            // touching the calling thread's signal mask. The arena
                            // (HostMemory virtual base) lives for the program lifetime,
                            // so its pointer never dangles.
                            let _ = pt_ptr; // keep the capture (silences unused warning)
                            // SEGV-safe read: on Linux uses process_vm_readv which returns
                            // EFAULT cleanly for PROT_NONE pages. On macOS falls back to a
                            // direct volatile copy (no equivalent of process_vm_readv).
                            #[cfg(target_os = "linux")]
                            let self_pid = unsafe { libc::getpid() };
                            let try_read_safe = |addr: *const u8, dst: &mut [u8]| -> bool {
                                #[cfg(target_os = "linux")]
                                {
                                    let local_iov = libc::iovec {
                                        iov_base: dst.as_mut_ptr() as *mut libc::c_void,
                                        iov_len: dst.len(),
                                    };
                                    let remote_iov = libc::iovec {
                                        iov_base: addr as *mut libc::c_void,
                                        iov_len: dst.len(),
                                    };
                                    let n = unsafe {
                                        libc::process_vm_readv(
                                            self_pid,
                                            &local_iov as *const _,
                                            1,
                                            &remote_iov as *const _,
                                            1,
                                            0,
                                        )
                                    };
                                    n == dst.len() as isize
                                }
                                #[cfg(not(target_os = "linux"))]
                                {
                                    unsafe {
                                        std::ptr::copy_nonoverlapping(addr, dst.as_mut_ptr(), dst.len());
                                    }
                                    true
                                }
                            };
                            loop {
                                if sleep_us > 0 {
                                    std::thread::sleep(std::time::Duration::from_micros(sleep_us));
                                }
                                for (i, &vaddr) in vaddrs.iter().enumerate() {
                                    let arena_host_addr =
                                        (arena + vaddr as usize) as *const u64;
                                    // Three SEGV-safe reads via process_vm_readv. If
                                    // the page isn't mapped (PROT_NONE) the syscall
                                    // returns EFAULT and we skip silently.
                                    let mut buf = [0u8; 8];
                                    if !try_read_safe(arena_host_addr as *const u8, &mut buf) {
                                        continue;
                                    }
                                    let arena_a = u64::from_le_bytes(buf);
                                    if !try_read_safe(arena_host_addr as *const u8, &mut buf) {
                                        continue;
                                    }
                                    let arena_b = u64::from_le_bytes(buf);
                                    if !try_read_safe(arena_host_addr as *const u8, &mut buf) {
                                        continue;
                                    }
                                    let arena_c = u64::from_le_bytes(buf);
                                    if is_corrupt(arena_c) {
                                        do_trap("arena", vaddr, arena_c);
                                    }
                                    // Keep `backing_*` vars as aliases of `arena_*` so the
                                    // subsequent diagnostic logging keeps working without
                                    // a larger restructure. The dual-view comparison
                                    // (originally arena vs backing) is now arena vs arena
                                    // — still useful for detecting torn writes via stable
                                    // re-reads.
                                    let backing_a = arena_a;
                                    let backing_b = arena_b;
                                    let backing_c = arena_c;
                                    let arena_stable = arena_a == arena_b && arena_b == arena_c;
                                    let backing_stable = backing_a == backing_b && backing_b == backing_c;
                                    let stable_diverge =
                                        arena_stable && backing_stable && arena_c != backing_c;
                                    let arena_changed = arena_c != last_arena[i];
                                    let backing_changed = backing_c != last_backing[i];
                                    // RUZU_POLL_DIVERGE_LOG_CHANGES=1 — log every change in the
                                    // polled vaddr's value (even if arena==backing). Useful for
                                    // tracking when a specific slot gets the corrupt pattern.
                                    if common::env_flag!("RUZU_POLL_DIVERGE_LOG_CHANGES")
                                        && (arena_changed || backing_changed)
                                    {
                                        let n = FIRES.fetch_add(1, Ordering::Relaxed);
                                        if n < 200 {
                                            eprintln!(
                                                "[POLL_CHANGE #{}] vaddr=0x{:016X} arena: 0x{:016X} → 0x{:016X}  backing: 0x{:016X} → 0x{:016X}",
                                                n, vaddr,
                                                last_arena[i], arena_c,
                                                last_backing[i], backing_c,
                                            );
                                        }
                                    }
                                    if stable_diverge {
                                        let n = FIRES.fetch_add(1, Ordering::Relaxed);
                                        if n < 64 || n % 1000 == 0 {
                                            eprintln!(
                                                "[POLL_DIVERGE_STABLE #{}] vaddr=0x{:016X} arena_host={:p} arena=0x{:016X} backing=0x{:016X}",
                                                n, vaddr, arena_host_addr,
                                                arena_c, backing_c,
                                            );
                                        }
                                    } else if arena_c != backing_c
                                        && FIRES.load(Ordering::Relaxed) < 4
                                    {
                                        eprintln!(
                                            "[POLL_DIVERGE_RACE] vaddr=0x{:016X} arena=[{:016X},{:016X},{:016X}] backing=[{:016X},{:016X},{:016X}] (race — not stable)",
                                            vaddr, arena_a, arena_b, arena_c,
                                            backing_a, backing_b, backing_c,
                                        );
                                    }
                                    last_arena[i] = arena_c;
                                    last_backing[i] = backing_c;
                                    let _ = (arena_changed, backing_changed);
                                }
                            }
                        })
                        .ok();
                }
            }
        }
    }

    /// Temporarily select a page table for slow-path memory copies without
    /// changing its process-owned fastmem policy.
    pub(crate) fn set_current_page_table_raw(&mut self, page_table: *mut PageTable) {
        self.current_page_table = page_table;
    }

    /// Map a physical memory region into the guest virtual address space.
    ///
    /// Matches upstream `Memory::Impl::MapMemoryRegion`:
    /// - Updates PageTable entries (pointers, backing_addr, blocks) per page
    /// - Maps into fastmem arena if available
    ///
    /// # Arguments
    /// * `page_table` - The page table to update
    /// * `base` - Guest virtual address (page-aligned)
    /// * `size` - Size in bytes (page-aligned)
    /// * `target` - Physical address (≥ DramMemoryMap::Base)
    /// * `perms` - Memory permissions
    /// * `separate_heap` - Whether this is a separate heap mapping
    pub fn map_memory_region(
        &self,
        page_table: &mut PageTable,
        base: u64,
        size: u64,
        target: u64,
        perms: MemoryPermission,
        separate_heap: bool,
    ) {
        debug_assert!(
            (size & PAGE_MASK) == 0,
            "non-page aligned size: {:#x}",
            size
        );
        debug_assert!(
            (base & PAGE_MASK) == 0,
            "non-page aligned base: {:#x}",
            base
        );
        debug_assert!(
            target >= dram_memory_map::BASE,
            "Out of bounds target: {:#x}",
            target
        );

        self.map_pages(
            page_table,
            base / PAGE_SIZE,
            size / PAGE_SIZE,
            target,
            PageType::Memory,
        );

        // RUZU_TRACE_MAP_REGION=0xPAGE — log when a map_memory_region call
        // covers the specified 4KB page. Used to verify fastmem-arena
        // mapping for the mstate region (STK heap-shifted-pointer wedge).
        if let Ok(spec) = std::env::var("RUZU_TRACE_MAP_REGION") {
            if let Ok(target_page) = u64::from_str_radix(spec.trim().trim_start_matches("0x"), 16) {
                let page_aligned = target_page & !(PAGE_SIZE - 1);
                if base <= page_aligned && page_aligned < base + size {
                    eprintln!(
                        "[MAP_REGION] base=0x{:016X} size=0x{:X} target=0x{:016X} target-DRAM=0x{:X} fastmem_arena={:?} separate_heap={}",
                        base,
                        size,
                        target,
                        target.wrapping_sub(dram_memory_map::BASE),
                        page_table.fastmem_arena,
                        separate_heap,
                    );
                }
            }
        }
        // RUZU_TRACE_MAP_HOST_OFFSET=0xOFFSET — log every map covering the
        // specified memfd host_offset. Detects aliasing where multiple
        // guest VAs map to the same memfd page (which would corrupt one
        // when writing to the other).
        if let Ok(spec) = std::env::var("RUZU_TRACE_MAP_HOST_OFFSET") {
            if let Ok(target_offset) = u64::from_str_radix(spec.trim().trim_start_matches("0x"), 16)
            {
                let host_offset = target.wrapping_sub(dram_memory_map::BASE);
                let page_aligned = target_offset & !(PAGE_SIZE - 1);
                if host_offset <= page_aligned && page_aligned < host_offset + size {
                    eprintln!(
                        "[MAP_HOST_OFFSET] vaddr_base=0x{:016X} size=0x{:X} target=0x{:016X} host_offset=0x{:X}",
                        base, size, target, host_offset,
                    );
                }
            }
        }

        if !page_table.fastmem_arena.is_null() {
            // Upstream: buffer->Map(base, target - DramBase, size, perms, separate_heap)
            // On Android, buffer is HeapTracker*; elsewhere it is HostMemory*.
            #[cfg(target_os = "android")]
            if let Some(ref heap_tracker) = self.heap_tracker {
                heap_tracker.map(
                    base as usize,
                    (target - dram_memory_map::BASE) as usize,
                    size as usize,
                    perms,
                    separate_heap,
                );
            }
            #[cfg(not(target_os = "android"))]
            unsafe {
                (*self.buffer).map(
                    base as usize,
                    (target - dram_memory_map::BASE) as usize,
                    size as usize,
                    perms,
                    separate_heap,
                );
            }
        }
    }

    /// Unmap a region of the guest virtual address space.
    ///
    /// Matches upstream `Memory::Impl::UnmapRegion`.
    pub fn unmap_region(
        &self,
        page_table: &mut PageTable,
        base: u64,
        size: u64,
        separate_heap: bool,
    ) {
        debug_assert!((size & PAGE_MASK) == 0);
        debug_assert!((base & PAGE_MASK) == 0);

        self.map_pages(
            page_table,
            base / PAGE_SIZE,
            size / PAGE_SIZE,
            0,
            PageType::Unmapped,
        );

        if !page_table.fastmem_arena.is_null() {
            #[cfg(target_os = "android")]
            if let Some(ref heap_tracker) = self.heap_tracker {
                heap_tracker.unmap(base as usize, size as usize, separate_heap);
            }
            #[cfg(not(target_os = "android"))]
            unsafe {
                (*self.buffer).unmap(base as usize, size as usize, separate_heap);
            }
        }
    }

    /// Change protection on a region of the guest virtual address space.
    ///
    /// Matches upstream `Memory::Impl::ProtectRegion`.
    pub fn protect_region(
        &self,
        page_table: &mut PageTable,
        vaddr: u64,
        size: u64,
        perms: MemoryPermission,
    ) {
        debug_assert!((size & PAGE_MASK) == 0);
        debug_assert!((vaddr & PAGE_MASK) == 0);

        if page_table.fastmem_arena.is_null() {
            return;
        }

        let mut protect_bytes: u64 = 0;
        let mut protect_begin: u64 = 0;

        let mut addr = vaddr;
        while addr < vaddr + size {
            let page_idx = (addr >> PAGE_BITS) as usize;
            let page_type = if page_idx < page_table.pointers.size() {
                page_table.pointers[page_idx].page_type()
            } else {
                PageType::Unmapped
            };

            match page_type {
                PageType::RasterizerCachedMemory => {
                    if protect_bytes > 0 {
                        self.protect_buffer(protect_begin as usize, protect_bytes as usize, perms);
                        protect_bytes = 0;
                    }
                }
                _ => {
                    if protect_bytes == 0 {
                        protect_begin = addr;
                    }
                    protect_bytes += PAGE_SIZE;
                }
            }

            addr += PAGE_SIZE;
        }

        if protect_bytes > 0 {
            self.protect_buffer(protect_begin as usize, protect_bytes as usize, perms);
        }
    }

    // =========================================================================
    // Read/Write via PageTable pointers
    // Matches upstream Core::Memory::Memory::Read/Write methods.
    // =========================================================================

    /// Get a host pointer for a guest virtual address (fast path).
    /// Matches upstream `Memory::Impl::GetPointerImpl`.
    ///
    /// Returns null if the page is unmapped.
    /// Route protect calls through HeapTracker on Android, HostMemory otherwise.
    fn protect_buffer(&self, offset: usize, size: usize, perms: MemoryPermission) {
        #[cfg(target_os = "android")]
        if let Some(ref heap_tracker) = self.heap_tracker {
            heap_tracker.protect(offset, size, perms);
            return;
        }
        unsafe {
            (*self.buffer).protect(offset, size, perms);
        }
    }

    #[inline]
    fn get_pointer_impl(&self, vaddr: u64) -> *mut u8 {
        // AARCH64 masks the upper 16 bits of all memory accesses.
        let vaddr = vaddr & 0xffff_ffff_ffff;

        if self.current_page_table.is_null() {
            return std::ptr::null_mut();
        }
        let pt = unsafe { &*self.current_page_table };
        let page_idx = (vaddr >> PAGE_BITS) as usize;
        if page_idx >= pt.pointers.size() {
            return std::ptr::null_mut();
        }

        let raw = pt.pointers[page_idx].raw_value();
        let pointer = PageInfo::extract_pointer(raw);
        if pointer != 0 {
            // Upstream stores a biased host pointer and reconstructs with
            // unchecked unsigned addition: `pointer + vaddr`.
            return pointer.wrapping_add(vaddr as usize) as *mut u8;
        }

        // Slow path: check page type
        match PageInfo::extract_type(raw) {
            PageType::Unmapped => std::ptr::null_mut(),
            PageType::Memory => {
                // Upstream: ASSERT_MSG(false, "Mapped memory page without a pointer")
                debug_assert!(
                    false,
                    "Mapped memory page without a pointer @ {:#018x}",
                    vaddr
                );
                std::ptr::null_mut()
            }
            PageType::DebugMemory => self.get_pointer_from_debug_memory(vaddr),
            PageType::RasterizerCachedMemory => {
                self.get_pointer_from_rasterizer_cached_memory(vaddr)
            }
        }
    }

    /// Get pointer from debug memory (slow path).
    /// Matches upstream `Memory::Impl::GetPointerFromDebugMemory`.
    fn get_pointer_from_debug_memory(&self, vaddr: u64) -> *mut u8 {
        if self.current_page_table.is_null() {
            return std::ptr::null_mut();
        }
        let pt = unsafe { &*self.current_page_table };
        let page_idx = (vaddr >> PAGE_BITS) as usize;
        if page_idx >= pt.backing_addr.size() {
            return std::ptr::null_mut();
        }
        let backing = pt.backing_addr[page_idx] as usize;
        if backing == 0 {
            return std::ptr::null_mut();
        }
        let phys_addr = (backing as u64).wrapping_add(vaddr);
        if phys_addr < dram_memory_map::BASE {
            return std::ptr::null_mut();
        }
        unsafe { (*self.device_memory).get_pointer(phys_addr) }
    }

    /// Get pointer from rasterizer cached memory (slow path).
    /// Matches upstream `Memory::Impl::GetPointerFromRasterizerCachedMemory`.
    fn get_pointer_from_rasterizer_cached_memory(&self, vaddr: u64) -> *mut u8 {
        // For now, same as debug memory (rasterizer cache not yet implemented).
        self.get_pointer_from_debug_memory(vaddr)
    }

    /// Mark or unmark a process virtual-address range for debugger memory
    /// callbacks. Port of Eden `Memory::Impl::MarkRegionDebug`.
    pub fn mark_region_debug(&self, vaddr: u64, size: u64, debug: bool) {
        if vaddr == 0
            || self.current_page_table.is_null()
            || usize::try_from(size)
                .map(|size| !self.address_space_contains(vaddr, size))
                .unwrap_or(true)
        {
            return;
        }

        let page_table = unsafe { &*self.current_page_table };
        if !page_table.fastmem_arena.is_null() {
            let permission = if debug {
                MemoryPermission::empty()
            } else {
                MemoryPermission::READ_WRITE
            };
            self.protect_buffer(vaddr as usize, size as usize, permission);
        }

        let num_pages = ((vaddr.wrapping_add(size).wrapping_sub(1)) >> PAGE_BITS)
            .wrapping_sub(vaddr >> PAGE_BITS)
            .wrapping_add(1);
        let mut current_vaddr = vaddr;
        for _ in 0..num_pages {
            let page_index = (current_vaddr >> PAGE_BITS) as usize;
            let entry = &page_table.pointers[page_index];
            match (debug, PageInfo::extract_type(entry.raw_value())) {
                (true, PageType::Unmapped) => {
                    debug_assert!(false, "Attempted to mark unmapped pages as debug");
                }
                (true, PageType::Memory) => entry.store(0, PageType::DebugMemory),
                (true, PageType::RasterizerCachedMemory | PageType::DebugMemory)
                | (false, PageType::RasterizerCachedMemory | PageType::Memory) => {}
                (false, PageType::Unmapped) => {
                    debug_assert!(false, "Attempted to mark unmapped pages as non-debug");
                }
                (false, PageType::DebugMemory) => {
                    let page = current_vaddr & !PAGE_MASK;
                    let pointer = self.get_pointer_from_debug_memory(page);
                    let encoded = (pointer as usize).wrapping_sub(page as usize);
                    entry.store(encoded, PageType::Memory);
                }
            }
            current_vaddr = current_vaddr.wrapping_add(PAGE_SIZE);
        }
    }

    /// Mark a CPU virtual-address range as cached (or no longer cached) by the
    /// rasterizer. Used by the GPU device-memory manager when shader/buffer/
    /// texture caches register or invalidate regions.
    ///
    /// Port of upstream `Memory::Impl::RasterizerMarkRegionCached`
    /// (`core/memory.cpp:793-844`). Walks each CPU page in the range and
    /// transitions its `PageType`:
    /// - `Memory`/`DebugMemory` → `RasterizerCachedMemory` when `cached`.
    /// - `RasterizerCachedMemory` → `Memory` when uncached (pointer recovered
    ///   via `get_pointer_from_rasterizer_cached_memory`, which uses the
    ///   per-page `backing_addr` table that survives the type transition).
    /// - `Unmapped` pages skipped (matches upstream — a process need not map
    ///   the GPU-cached region into its own AS, e.g. VRAM-only buffers).
    ///
    /// The fastmem arena is reprotected before the page-type transition,
    /// matching upstream's `Settings::values.use_reactive_flushing` policy.
    pub fn rasterizer_mark_region_cached(&self, vaddr: u64, size: u64, cached: bool) {
        record_rasterizer_mark_cached_stage(0);
        if vaddr == 0
            || size == 0
            || self.current_page_table.is_null()
            || usize::try_from(size)
                .map(|size| !self.address_space_contains(vaddr, size))
                .unwrap_or(true)
        {
            record_rasterizer_mark_cached_stage(8);
            return;
        }
        record_rasterizer_mark_cached_stage(1);
        let pt = unsafe { &*self.current_page_table };
        record_rasterizer_mark_cached_stage(2);

        if !pt.fastmem_arena.is_null() {
            let mut perm = MemoryPermission::empty();
            if !*common::settings::values().use_reactive_flushing.get_value() || !cached {
                perm |= MemoryPermission::READ;
            }
            if !cached {
                perm |= MemoryPermission::WRITE;
            }
            self.protect_buffer(vaddr as usize, size as usize, perm);
        }

        // Upstream computes `num_pages` as
        //   ((vaddr + size - 1) >> PAGEBITS) - (vaddr >> PAGEBITS) + 1
        // so single-byte writes still touch one page, and a write straddling
        // a page boundary touches two pages — even when `size < PAGE_SIZE`.
        let num_pages = ((vaddr + size - 1) >> PAGE_BITS) - (vaddr >> PAGE_BITS) + 1;
        record_rasterizer_mark_cached_stage(3);
        let mut current_vaddr = vaddr;
        record_rasterizer_mark_cached_stage(4);
        for _ in 0..num_pages {
            record_rasterizer_mark_cached_stage(5);
            let page_idx = (current_vaddr >> PAGE_BITS) as usize;
            if page_idx < pt.pointers.size() {
                let entry = &pt.pointers[page_idx];
                let ptype = PageInfo::extract_type(entry.raw_value());
                if cached {
                    match ptype {
                        PageType::Memory | PageType::DebugMemory => {
                            // Switch to RasterizerCachedMemory. Pointer is
                            // stored as 0; readers go through the slow path
                            // (`get_pointer_from_rasterizer_cached_memory`).
                            entry.store(0, PageType::RasterizerCachedMemory);
                        }
                        // Unmapped → skip (no CPU backing to track).
                        // RasterizerCachedMemory → already cached, common
                        // when multiple GPU regions map the same CPU page.
                        _ => {}
                    }
                } else {
                    if ptype == PageType::RasterizerCachedMemory {
                        let pointer = self.get_pointer_from_rasterizer_cached_memory(current_vaddr);
                        if !pointer.is_null() {
                            // Encode pointer as `ptr - vaddr` so the fastmem
                            // path can recover the host address with one
                            // addition (matches the PageInfo layout used by
                            // `map_pages`).
                            let encoded = (pointer as usize).wrapping_sub(current_vaddr as usize);
                            entry.store(encoded, PageType::Memory);
                        } else {
                            // The backing VMA may already have been removed
                            // while its page table was being updated.
                            entry.store(0, PageType::Unmapped);
                        }
                    }
                }
            }
            current_vaddr += PAGE_SIZE as u64;
        }
        record_rasterizer_mark_cached_stage(6);
        record_rasterizer_mark_cached_stage(7);
    }

    fn handle_rasterizer_write(&self, vaddr: u64, size: usize) {
        if self.system.is_null() {
            return;
        }

        let host_ptr = self.get_pointer_impl(vaddr);
        let Some(gpu) = self.system.get().gpu_core() else {
            return;
        };
        let core = self.current_host_thread_cache_index();
        let sys_core = hardware_properties::NUM_CPU_CORES as usize - 1;
        let _sys_core_guard = (core == sys_core).then(|| self.sys_core_guard.lock().unwrap());

        let mut write = |device_addr: u64| {
            let subaddress = device_addr >> PAGE_BITS;
            let mut write_area = self.rasterizer_write_areas[core].lock().unwrap();
            let mut do_collection = write_area.last_address == subaddress;

            if !do_collection {
                do_collection =
                    device_addr != 0 && size != 0 && gpu.on_cpu_write(device_addr, size as u64);
                if !do_collection {
                    return;
                }
                write_area.last_address = subaddress;
            }
            drop(write_area);

            if let Some(manager) = self.gpu_dirty_managers.get(core) {
                manager.collect(device_addr, size);
            }
        };

        if let Some(host1x) = self.system.get().host1x_core() {
            let mut scratch = self.smmu_scratch_buffers[core].lock().unwrap();
            host1x.smmu_apply_op_on_host_pointer(host_ptr as usize, &mut scratch, &mut write);
        }
    }

    fn current_host_thread_cache_index(&self) -> usize {
        if self.system.is_null() {
            return 0;
        }

        self.system
            .get()
            .current_host_thread_id()
            .min(hardware_properties::NUM_CPU_CORES as usize - 1)
    }

    fn handle_rasterizer_download(&self, vaddr: u64, size: usize) {
        if self.system.is_null() {
            return;
        }

        let host_ptr = self.get_pointer_impl(vaddr);
        let Some(gpu) = self.system.get().gpu_core() else {
            return;
        };

        let core = self.current_host_thread_cache_index();
        let mut download = |device_addr: u64| {
            let end_address = device_addr.wrapping_add(size as u64);
            {
                let current_area = self.rasterizer_read_areas[core].lock().unwrap();
                if current_area.start_address <= device_addr
                    && end_address <= current_area.end_address
                {
                    return;
                }
            }

            let new_area = gpu.on_cpu_read(device_addr, size as u64);
            *self.rasterizer_read_areas[core].lock().unwrap() = new_area;
        };

        if let Some(host1x) = self.system.get().host1x_core() {
            let mut scratch = self.smmu_scratch_buffers[core].lock().unwrap();
            host1x.smmu_apply_op_on_host_pointer(host_ptr as usize, &mut scratch, &mut download);
        }
    }

    fn handle_rasterizer_download_for_read_range(&self, start_addr: u64, size: usize) {
        let mut remaining = size;
        let mut vaddr = start_addr;
        while remaining > 0 {
            let page_offset = (vaddr & PAGE_MASK) as usize;
            let copy_amount = ((PAGE_SIZE as usize) - page_offset).min(remaining);
            if self.page_type_at(vaddr) == Some(PageType::RasterizerCachedMemory) {
                self.handle_rasterizer_download(vaddr, copy_amount);
            }
            vaddr += copy_amount as u64;
            remaining -= copy_amount;
        }
    }

    fn page_type_at(&self, vaddr: u64) -> Option<PageType> {
        if self.current_page_table.is_null() {
            return None;
        }
        let pt = unsafe { &*self.current_page_table };
        let page_idx = (vaddr >> PAGE_BITS) as usize;
        if page_idx >= pt.pointers.size() {
            return None;
        }
        Some(PageInfo::extract_type(pt.pointers[page_idx].raw_value()))
    }

    fn page_debug_at(&self, vaddr: u64) -> Option<(PageType, usize, u64, Option<u64>)> {
        if self.current_page_table.is_null() {
            return None;
        }
        let pt = unsafe { &*self.current_page_table };
        let page_idx = (vaddr >> PAGE_BITS) as usize;
        if page_idx >= pt.pointers.size() || page_idx >= pt.backing_addr.size() {
            return None;
        }
        let raw = pt.pointers[page_idx].raw_value();
        let pointer = PageInfo::extract_pointer(raw);
        let ptype = PageInfo::extract_type(raw);
        let backing = pt.backing_addr[page_idx];
        let phys = if backing == 0 {
            None
        } else {
            Some(backing.wrapping_add(vaddr))
        };
        Some((ptype, pointer, backing, phys))
    }

    fn perform_cache_operation<F>(
        &self,
        dest_addr: u64,
        size: usize,
        mut on_rasterizer: F,
    ) -> ResultCode
    where
        F: FnMut(u64, usize),
    {
        let mut remaining = size;
        let mut vaddr = dest_addr;

        while remaining > 0 {
            let page_offset = (vaddr & PAGE_MASK) as usize;
            let block_size = ((PAGE_SIZE as usize) - page_offset).min(remaining);

            if self.get_pointer_impl(vaddr).is_null() {
                // Upstream zuyu's cache helpers currently succeed without
                // checking page mappings. Preserve that behaviour for guest
                // cache-maintenance SVCs while still letting mapped
                // rasterizer-cached pages trigger the Rust-side coherency hook.
                vaddr += block_size as u64;
                remaining -= block_size;
                continue;
            }

            if self.page_type_at(vaddr) == Some(PageType::RasterizerCachedMemory) {
                on_rasterizer(vaddr, block_size);
            }

            vaddr += block_size as u64;
            remaining -= block_size;
        }

        RESULT_SUCCESS
    }

    /// Invalidates a range of bytes within the current process address space.
    ///
    /// Matches upstream `Memory::InvalidateDataCache`: rasterizer-cached ranges
    /// are downloaded from host GPU memory to guest memory.
    pub fn invalidate_data_cache(&self, dest_addr: u64, size: usize) -> ResultCode {
        self.perform_cache_operation(dest_addr, size, |current_vaddr, block_size| {
            self.handle_rasterizer_download(current_vaddr, block_size);
        })
    }

    /// Stores a range of bytes within the current process address space.
    ///
    /// Matches upstream `Memory::StoreDataCache`: CPU flush -> GPU invalidate.
    pub fn store_data_cache(&self, dest_addr: u64, size: usize) -> ResultCode {
        self.perform_cache_operation(dest_addr, size, |current_vaddr, block_size| {
            self.handle_rasterizer_write(current_vaddr, block_size);
        })
    }

    /// Flushes a range of bytes within the current process address space.
    ///
    /// Matches upstream `Memory::FlushDataCache`: CPU flush -> GPU invalidate.
    pub fn flush_data_cache(&self, dest_addr: u64, size: usize) -> ResultCode {
        self.perform_cache_operation(dest_addr, size, |current_vaddr, block_size| {
            self.handle_rasterizer_write(current_vaddr, block_size);
        })
    }

    /// Two-phase variant of `flush_data_cache` for guest cache-maintenance
    /// SVCs.
    ///
    /// Upstream `PerformCacheOperation` runs without any memory-wide lock, so
    /// its per-page `HandleRasterizerWrite` calls can contend with the GPU
    /// thread freely. The Rust `Memory` sits behind a `Mutex`, and holding
    /// that mutex across rasterizer notifications serializes every other
    /// guest-memory access in the emulator (and inverts lock order against
    /// the texture-cache mutex held by the GPU thread during draws).
    ///
    /// Phase 1 (this method, called under the memory lock) only walks the
    /// page table: it merges contiguous `RasterizerCachedMemory` pages into
    /// ranges and resolves their device addresses. Phase 2
    /// (`RasterizerWriteBatch::apply`, called after the caller has dropped
    /// the memory lock) performs the actual rasterizer notifications.
    pub fn collect_rasterizer_write_ranges(
        &self,
        dest_addr: u64,
        size: usize,
    ) -> RasterizerWriteBatch {
        let mut ranges: Vec<(u64, usize)> = Vec::new();
        let mut remaining = size;
        let mut vaddr = dest_addr;
        let core = self.current_host_thread_cache_index();
        let mut scratch = self.smmu_scratch_buffers[core].lock().unwrap();

        while remaining > 0 {
            let page_offset = (vaddr & PAGE_MASK) as usize;
            let block_size = ((PAGE_SIZE as usize) - page_offset).min(remaining);

            let host_ptr = self.get_pointer_impl(vaddr);
            if !host_ptr.is_null()
                && self.page_type_at(vaddr) == Some(PageType::RasterizerCachedMemory)
            {
                if let Some(host1x) = self.system.get().host1x_core() {
                    host1x.smmu_apply_op_on_host_pointer(
                        host_ptr as usize,
                        &mut scratch,
                        &mut |device_addr| match ranges.last_mut() {
                            // Merge device-contiguous blocks so phase 2 issues one
                            // rasterizer notification per range instead of one per
                            // 4 KiB page.
                            Some((last_device_addr, last_size))
                                if *last_device_addr + *last_size as u64 == device_addr =>
                            {
                                *last_size += block_size;
                            }
                            _ => ranges.push((device_addr, block_size)),
                        },
                    );
                }
            }

            vaddr += block_size as u64;
            remaining -= block_size;
        }

        let sys_core = hardware_properties::NUM_CPU_CORES as usize - 1;
        RasterizerWriteBatch {
            system: self.system,
            dirty_manager: self.gpu_dirty_managers.get(core).cloned(),
            sys_core_guard: (core == sys_core).then(|| Arc::clone(&self.sys_core_guard)),
            ranges,
        }
    }

    /// Get a host pointer for a guest virtual address.
    /// Matches upstream `Memory::GetPointer`.
    pub fn get_pointer(&self, vaddr: u64) -> *mut u8 {
        // RUZU_TRACE_GET_POINTER_PAGE=0xPAGEVADDR — log every get_pointer
        // call returning a pointer within the same 4 KB page as PAGEVADDR.
        // Used to find HLE callers that bypass write_64/write_block.
        trace_get_pointer_page("get_pointer", vaddr);
        let ptr = self.get_pointer_impl(vaddr);
        if ptr.is_null() {
            log::error!("Unmapped GetPointer @ {:#018x}", vaddr);
        }
        ptr
    }

    /// Get a host pointer without logging on unmapped addresses.
    /// Matches upstream `Memory::GetPointerSilent`.
    pub fn get_pointer_silent(&self, vaddr: u64) -> *mut u8 {
        trace_get_pointer_page("get_pointer_silent", vaddr);
        self.get_pointer_impl(vaddr)
    }

    /// Read a value of type T from guest virtual address.
    /// Matches upstream `Memory::Impl::Read<T>`.
    #[inline]
    unsafe fn read_raw<T: Copy + Default>(&self, vaddr: u64) -> T {
        let ptr = self.get_pointer_impl(vaddr);
        if ptr.is_null() {
            // RUZU_TRACE_UNMAPPED_BT=1 — capture host backtrace on the first
            // few unmapped reads so we can identify the calling subsystem
            // (HLE service code path, JIT trampoline, etc.). Throttled to
            // 5 entries to avoid log spam.
            if common::env_flag!("RUZU_TRACE_UNMAPPED_BT") {
                use std::sync::atomic::{AtomicU32, Ordering};
                static SHOWN: AtomicU32 = AtomicU32::new(0);
                let n = SHOWN.fetch_add(1, Ordering::Relaxed);
                if n < 5 {
                    let bt = std::backtrace::Backtrace::force_capture();
                    eprintln!(
                        "[UNMAPPED_BT #{}] vaddr=0x{:016X} size={}\n{}",
                        n,
                        vaddr,
                        std::mem::size_of::<T>() * 8,
                        bt
                    );
                }
            }
            if common::env_flag!("RUZU_TRACE_UNMAPPED_GUEST")
                && (vaddr < 0x1000 || (vaddr >> 32) == 0xffff_ffff)
            {
                use std::sync::atomic::{AtomicU32, Ordering};
                static SHOWN: AtomicU32 = AtomicU32::new(0);
                let n = SHOWN.fetch_add(1, Ordering::Relaxed);
                if n < 64 {
                    let tid = crate::hle::kernel::kernel::get_current_thread_id_fast().unwrap_or(0);
                    let (core, regs) =
                        crate::hle::kernel::kernel::with_current_thread_fast_mut(|t| {
                            (
                                t.get_current_core().max(0) as usize,
                                [
                                    t.thread_context.r[0],
                                    t.thread_context.r[1],
                                    t.thread_context.r[2],
                                    t.thread_context.r[3],
                                    t.thread_context.r[4],
                                    t.thread_context.r[5],
                                    t.thread_context.r[6],
                                    t.thread_context.r[7],
                                    t.thread_context.fp,
                                    t.thread_context.sp,
                                    t.thread_context.lr,
                                ],
                            )
                        })
                        .unwrap_or((usize::MAX, [0; 11]));
                    let (pc, lr) = if core < crate::hle::kernel::kernel::GUEST_PC.len() {
                        (
                            crate::hle::kernel::kernel::GUEST_PC[core].load(Ordering::Acquire),
                            crate::hle::kernel::kernel::GUEST_LR[core].load(Ordering::Acquire),
                        )
                    } else {
                        (0, 0)
                    };
                    log::error!(
                        "[UNMAPPED_GUEST_READ] #{} tid={} core={} pc=0x{:08X} lr=0x{:08X} vaddr=0x{:X} bits={} r0=0x{:08X} r1=0x{:08X} r2=0x{:08X} r3=0x{:08X} r4=0x{:08X} r5=0x{:08X} r6=0x{:08X} r7=0x{:08X} fp=0x{:08X} sp=0x{:08X} ctx_lr=0x{:08X}",
                        n,
                        tid,
                        core,
                        pc,
                        lr,
                        vaddr,
                        std::mem::size_of::<T>() * 8,
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
                    );
                }
            }
            log::error!(
                "Unmapped Read{} @ {:#018x}",
                std::mem::size_of::<T>() * 8,
                vaddr
            );
            return T::default();
        }
        if self.page_type_at(vaddr) == Some(PageType::RasterizerCachedMemory) {
            self.handle_rasterizer_download(vaddr, std::mem::size_of::<T>());
        }
        std::ptr::read_unaligned(ptr as *const T)
    }

    /// Write a value of type T to guest virtual address.
    /// Matches upstream `Memory::Impl::Write<T>`.
    #[inline]
    unsafe fn write_raw<T: Copy>(&self, vaddr: u64, data: T) {
        // `RUZU_TRACE_RAW_WRITE_AT=0xVADDR` — log a backtrace whenever the
        // [vaddr, vaddr+sizeof(T)) range covers the target. `write_raw`
        // is the lowest-level guest-memory writer; all of `write_8/16/
        // 32/64`, `write_32_no_rasterizer`, `write_block_no_rasterizer`
        // and `write_block` ultimately funnel through here, so this
        // catches every Rust-side write regardless of the public entry
        // point.
        //
        // The target is parsed once via `OnceLock` so the hot path is
        // just an atomic load + range check; when unset, the cost is
        // one extra branch.
        {
            use std::sync::OnceLock;
            static TARGET: OnceLock<Option<u64>> = OnceLock::new();
            let target = *TARGET.get_or_init(|| {
                std::env::var("RUZU_TRACE_RAW_WRITE_AT")
                    .ok()
                    .and_then(|s| u64::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
            });
            if let Some(target) = target {
                let size = std::mem::size_of::<T>() as u64;
                if vaddr <= target && target < vaddr + size {
                    let bt = std::backtrace::Backtrace::force_capture();
                    // For sizes ≤ 8, we can read back the bytes from the
                    // input data via raw memory copy to format them.
                    let mut buf = [0u8; 8];
                    let n = (size as usize).min(8);
                    std::ptr::copy_nonoverlapping(
                        &data as *const T as *const u8,
                        buf.as_mut_ptr(),
                        n,
                    );
                    let mut hex = String::new();
                    for b in &buf[..n] {
                        use std::fmt::Write;
                        let _ = write!(hex, "{:02x}", b);
                    }
                    eprintln!(
                        "[RAW_WRITE_AT] vaddr=0x{:016X} size={} bytes={}\n{}",
                        vaddr, size, hex, bt
                    );
                }
            }
        }

        // `RUZU_TRACE_RAW_WRITE_VALUE=0xVALUE` — log a backtrace whenever
        // `write_raw` is called with a 4-byte value equal to VALUE,
        // regardless of address. Used to hunt for a "magic" sentinel
        // where it lands. Hot-path cost: 1 atomic load + 1 size check
        // + 1 value compare when enabled, 1 atomic load when not.
        {
            use std::sync::OnceLock;
            static TARGET_VALUE: OnceLock<Option<u32>> = OnceLock::new();
            let target_value = *TARGET_VALUE.get_or_init(|| {
                std::env::var("RUZU_TRACE_RAW_WRITE_VALUE")
                    .ok()
                    .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
            });
            if let Some(target_value) = target_value {
                let size = std::mem::size_of::<T>();
                if size == 4 {
                    let val_u32 =
                        unsafe { std::ptr::read_unaligned(&data as *const T as *const u32) };
                    if val_u32 == target_value {
                        let bt = std::backtrace::Backtrace::force_capture();
                        eprintln!(
                            "[RAW_WRITE_VALUE] vaddr=0x{:016X} size={} value=0x{:08X}\n{}",
                            vaddr, size, val_u32, bt
                        );
                    }
                }
            }
        }

        let ptr = self.get_pointer_impl(vaddr);
        if ptr.is_null() {
            trace_unmapped_guest_access("WRITE", vaddr, std::mem::size_of::<T>() * 8);
            log::error!(
                "Unmapped Write{} @ {:#018x}",
                std::mem::size_of::<T>() * 8,
                vaddr
            );
            return;
        }
        std::ptr::write_unaligned(ptr as *mut T, data);
    }

    /// Read a u8. Matches upstream `Memory::Read8`.
    pub fn read_8(&self, vaddr: u64) -> u8 {
        unsafe { self.read_raw::<u8>(vaddr) }
    }

    /// Read a u16 (LE). Matches upstream `Memory::Read16`.
    pub fn read_16(&self, vaddr: u64) -> u16 {
        if (vaddr & 1) == 0 {
            unsafe { self.read_raw::<u16>(vaddr) }
        } else {
            let a = self.read_8(vaddr) as u16;
            let b = self.read_8(vaddr + 1) as u16;
            (b << 8) | a
        }
    }

    /// Read a u32 (LE). Matches upstream `Memory::Read32`.
    pub fn read_32(&self, vaddr: u64) -> u32 {
        if (vaddr & 3) == 0 {
            unsafe { self.read_raw::<u32>(vaddr) }
        } else {
            let a = self.read_16(vaddr) as u32;
            let b = self.read_16(vaddr + 2) as u32;
            (b << 16) | a
        }
    }

    /// Read a u64 (LE). Matches upstream `Memory::Read64`.
    pub fn read_64(&self, vaddr: u64) -> u64 {
        if (vaddr & 7) == 0 {
            unsafe { self.read_raw::<u64>(vaddr) }
        } else {
            let a = self.read_32(vaddr) as u64;
            let b = self.read_32(vaddr + 4) as u64;
            (b << 32) | a
        }
    }

    /// Write a u8. Matches upstream `Memory::Write8`.
    pub fn write_8(&self, vaddr: u64, data: u8) {
        maybe_trace_write_in_range(vaddr, 1, data as u64);
        maybe_trace_write_value("write_8", vaddr, 1, data as u64);
        if self.page_type_at(vaddr) == Some(PageType::RasterizerCachedMemory) {
            self.handle_rasterizer_write(vaddr, std::mem::size_of::<u8>());
        }
        unsafe { self.write_raw::<u8>(vaddr, data) }
    }

    /// Write a u16 (LE). Matches upstream `Memory::Write16`.
    pub fn write_16(&self, vaddr: u64, data: u16) {
        maybe_trace_write_in_range(vaddr, 2, data as u64);
        maybe_trace_write_value("write_16", vaddr, 2, data as u64);
        if (vaddr & 1) == 0 {
            if self.page_type_at(vaddr) == Some(PageType::RasterizerCachedMemory) {
                self.handle_rasterizer_write(vaddr, std::mem::size_of::<u16>());
            }
            unsafe { self.write_raw::<u16>(vaddr, data) }
        } else {
            self.write_8(vaddr, data as u8);
            self.write_8(vaddr + 1, (data >> 8) as u8);
        }
    }

    /// Write a u32 (LE). Matches upstream `Memory::Write32`.
    pub fn write_32(&self, vaddr: u64, data: u32) {
        maybe_trace_write_in_range(vaddr, 4, data as u64);
        maybe_trace_write_value("write_32", vaddr, 4, data as u64);
        // `RUZU_TRACE_MEMORY_W32_AT_VADDR=0xVADDR` — log every Rust-side
        // `Memory::write_32` call whose vaddr matches. Counterpart to the
        // existing `RUZU_TRACE_MEMORY_W64_AT_VADDR`. Catches HLE / kernel
        // writes (like `write_to_user` in k_condition_variable) that
        // bypass the JIT memory_write_32 callback. Pair with
        // `RUZU_NO_FASTMEM_W32=1` to see ALL writes (guest + kernel).
        {
            static W32_TARGET: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
            if let Some(target) = cached_hex_env(&W32_TARGET, "RUZU_TRACE_MEMORY_W32_AT_VADDR") {
                if vaddr <= target && target < vaddr + 4 {
                    let bt = std::backtrace::Backtrace::force_capture();
                    eprintln!(
                        "[MEMORY_W32] vaddr=0x{:016X} data=0x{:08X}\n{}",
                        vaddr, data, bt
                    );
                }
            }
        }
        if (vaddr & 3) == 0 {
            if self.page_type_at(vaddr) == Some(PageType::RasterizerCachedMemory) {
                self.handle_rasterizer_write(vaddr, std::mem::size_of::<u32>());
            }
            unsafe { self.write_raw::<u32>(vaddr, data) }
        } else {
            self.write_16(vaddr, data as u16);
            self.write_16(vaddr + 2, (data >> 16) as u16);
        }
    }

    /// Write a u32 (LE) without notifying the rasterizer.
    ///
    /// Kernel synchronization helpers use this for guest mutex/CV words while
    /// holding the global scheduler lock. Calling the rasterizer from there can
    /// invert Rust host locks (`Memory` -> shader cache) against the GPU thread
    /// (shader cache -> `Memory`). Guest/JIT writes must keep using `write_32`.
    pub fn write_32_no_rasterizer(&self, vaddr: u64, data: u32) {
        maybe_trace_write_value("write_32_no_rasterizer", vaddr, 4, data as u64);
        if (vaddr & 3) == 0 {
            unsafe { self.write_raw::<u32>(vaddr, data) }
        } else {
            unsafe {
                self.write_raw::<u8>(vaddr, data as u8);
                self.write_raw::<u8>(vaddr + 1, (data >> 8) as u8);
                self.write_raw::<u8>(vaddr + 2, (data >> 16) as u8);
                self.write_raw::<u8>(vaddr + 3, (data >> 24) as u8);
            }
        }
    }

    /// Write a byte block without notifying the rasterizer.
    ///
    /// Used for host-side HLE/service writes where ruzu already holds the global
    /// `Mutex<Memory>`. Guest/JIT writes must keep using `write_block`.
    pub fn write_block_no_rasterizer(&self, dest_addr: u64, src: &[u8]) -> bool {
        maybe_trace_write_block_values("write_block_no_rasterizer", dest_addr, src);
        // `RUZU_TRACE_WRITE_BLOCK_AT=0xVADDR` — log every HLE-side
        // `write_block_no_rasterizer` whose [dest, dest+len) range covers
        // VADDR. Used to attribute non-fastmem writes (HLE WriteBuffer,
        // etc.) that bypass both JIT memory callbacks and the per-u32
        // `RUZU_TRACE_MEMORY_W32_AT_VADDR` hook in `write_32`.
        {
            static WB_TARGET: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
            if let Some(target) = cached_hex_env(&WB_TARGET, "RUZU_TRACE_WRITE_BLOCK_AT") {
                if dest_addr <= target && target < dest_addr + src.len() as u64 {
                    let bt = std::backtrace::Backtrace::force_capture();
                    let off = (target - dest_addr) as usize;
                    let len = src.len();
                    let preview_end = (off + 16).min(len);
                    let mut preview = String::new();
                    for &b in &src[off..preview_end] {
                        use std::fmt::Write;
                        let _ = write!(preview, "{:02x}", b);
                    }
                    eprintln!(
                        "[WRITE_BLOCK_AT] dest=0x{:016X} len={:#x} off=0x{:X} preview={}\n{}",
                        dest_addr, len, off, preview, bt
                    );
                }
            }
        }
        let size = src.len();
        if size == 0 {
            return true;
        }

        if !self.address_space_contains(dest_addr, size) {
            log::error!("Unmapped WriteBlock @ {:#018x} size={:#x}", dest_addr, size);
            return false;
        }

        let mut remaining = size;
        let mut offset = 0usize;
        let mut vaddr = dest_addr;
        let mut user_accessible = true;

        while remaining > 0 {
            let page_offset = (vaddr & PAGE_MASK) as usize;
            let copy_amount = ((PAGE_SIZE as usize) - page_offset).min(remaining);

            let ptr = self.get_pointer_impl(vaddr);
            if ptr.is_null() {
                log::error!("Unmapped WriteBlock @ {:#018x}", vaddr);
                user_accessible = false;
            } else {
                unsafe {
                    std::ptr::copy_nonoverlapping(src[offset..].as_ptr(), ptr, copy_amount);
                }
            }

            vaddr += copy_amount as u64;
            offset += copy_amount;
            remaining -= copy_amount;
        }
        user_accessible
    }

    /// SEGV-safe variant for HLE IPC output buffers.
    ///
    /// Upstream HLE writes call `Memory::WriteBlock`, so they still notify the
    /// rasterizer before copying. IPC out-buffers can overlap protected GPU
    /// cached pages while a service thread copies a large file chunk; using
    /// `process_vm_writev` turns a still-inaccessible host page into `EFAULT`
    /// instead of taking the emulator down with SIGSEGV.
    pub fn write_block_checked(&self, dest_addr: u64, src: &[u8]) -> bool {
        maybe_trace_write_block_values("write_block_checked", dest_addr, src);
        {
            static WB_TARGET: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
            if let Some(target) = cached_hex_env(&WB_TARGET, "RUZU_TRACE_WRITE_BLOCK_AT") {
                if dest_addr <= target && target < dest_addr + src.len() as u64 {
                    let bt = std::backtrace::Backtrace::force_capture();
                    let off = (target - dest_addr) as usize;
                    let len = src.len();
                    let preview_end = (off + 16).min(len);
                    let mut preview = String::new();
                    for &b in &src[off..preview_end] {
                        use std::fmt::Write;
                        let _ = write!(preview, "{:02x}", b);
                    }
                    eprintln!(
                        "[WRITE_BLOCK_CHECKED_AT] dest=0x{:016X} len={:#x} off=0x{:X} preview={}\n{}",
                        dest_addr, len, off, preview, bt
                    );
                }
            }
        }
        let size = src.len();
        if size == 0 {
            return true;
        }

        if !self.address_space_contains(dest_addr, size) {
            log::error!(
                "Unmapped checked WriteBlock @ {:#018x} size={:#x}",
                dest_addr,
                size
            );
            return false;
        }

        #[cfg(target_os = "linux")]
        let self_pid = unsafe { libc::getpid() };
        let mut remaining = size;
        let mut offset = 0usize;
        let mut vaddr = dest_addr;
        let mut user_accessible = true;

        while remaining > 0 {
            let page_offset = (vaddr & PAGE_MASK) as usize;
            let copy_amount = ((PAGE_SIZE as usize) - page_offset).min(remaining);

            let ptr = self.get_pointer_impl(vaddr);
            if ptr.is_null() {
                log::error!("Unmapped checked WriteBlock @ {:#018x}", vaddr);
                user_accessible = false;
            } else {
                if self.page_type_at(vaddr) == Some(PageType::RasterizerCachedMemory) {
                    self.handle_rasterizer_write(vaddr, copy_amount);
                }
                // On Linux: SEGV-safe write via process_vm_writev (EFAULT on PROT_NONE).
                // On macOS: no equivalent; fall back to direct copy.
                #[cfg(target_os = "linux")]
                let written = {
                    let local_iov = libc::iovec {
                        iov_base: src[offset..].as_ptr() as *mut libc::c_void,
                        iov_len: copy_amount,
                    };
                    let remote_iov = libc::iovec {
                        iov_base: ptr as *mut libc::c_void,
                        iov_len: copy_amount,
                    };
                    let mut w = unsafe {
                        libc::process_vm_writev(
                            self_pid,
                            &local_iov as *const _,
                            1,
                            &remote_iov as *const _,
                            1,
                            0,
                        )
                    };
                    if w != copy_amount as isize && self.invalidate_separate_heap(ptr as *const u8)
                    {
                        w = unsafe {
                            libc::process_vm_writev(
                                self_pid,
                                &local_iov as *const _,
                                1,
                                &remote_iov as *const _,
                                1,
                                0,
                            )
                        };
                    }
                    w
                };
                #[cfg(not(target_os = "linux"))]
                let written = {
                    unsafe {
                        std::ptr::copy_nonoverlapping(src[offset..].as_ptr(), ptr, copy_amount);
                    }
                    copy_amount as isize
                };
                if written != copy_amount as isize {
                    use std::sync::atomic::{AtomicU32, Ordering};
                    static CHECKED_WRITE_FAILURES: AtomicU32 = AtomicU32::new(0);
                    let failure = CHECKED_WRITE_FAILURES.fetch_add(1, Ordering::Relaxed);
                    if failure < 16 || failure.is_power_of_two() {
                        if let Some((ptype, pointer, backing, phys)) = self.page_debug_at(vaddr) {
                            let buffer = unsafe { &*self.buffer };
                            let backing_base = buffer.backing_base_pointer() as usize;
                            let backing_size = buffer.backing_size();
                            let ptr_offset = (ptr as usize).wrapping_sub(backing_base);
                            let errno = std::io::Error::last_os_error();
                            log::error!(
                                "checked WriteBlock page debug @ {:#018x}: type={:?} pointer={:#x} backing={:#x} phys={:?} ptr={:#x} backing_base={:#x} ptr_offset={:#x} backing_size={:#x} errno={}",
                                vaddr,
                                ptype,
                                pointer,
                                backing,
                                phys,
                                ptr as usize,
                                backing_base,
                                ptr_offset,
                                backing_size,
                                errno,
                            );
                        }
                    }
                    log::error!(
                        "checked WriteBlock failed @ {:#018x} size={:#x} written={}",
                        vaddr,
                        copy_amount,
                        written
                    );
                    user_accessible = false;
                }
            }

            vaddr += copy_amount as u64;
            offset += copy_amount;
            remaining -= copy_amount;
        }
        user_accessible
    }

    /// Write a u64 (LE). Matches upstream `Memory::Write64`.
    pub fn write_64(&self, vaddr: u64, data: u64) {
        maybe_trace_write_value("write_64", vaddr, 8, data);
        // RUZU_TRACE_MEMORY_W64_AT_VADDR=0xVADDR — log every call into
        // Memory::write_64 with vaddr matching. Catches any Rust-side
        // write (HLE, kernel, etc.) that bypasses the JIT callback.
        {
            static W64_TARGET: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
            let spec_target = cached_hex_env(&W64_TARGET, "RUZU_TRACE_MEMORY_W64_AT_VADDR");
            if let Some(target) = spec_target {
                if vaddr == target {
                    let bt = std::backtrace::Backtrace::force_capture();
                    eprintln!(
                        "[MEMORY_W64] vaddr=0x{:016X} data=0x{:016X}\n{}",
                        vaddr, data, bt
                    );
                }
            }
        }
        if (vaddr & 7) == 0 {
            if self.page_type_at(vaddr) == Some(PageType::RasterizerCachedMemory) {
                self.handle_rasterizer_write(vaddr, std::mem::size_of::<u64>());
            }
            unsafe { self.write_raw::<u64>(vaddr, data) }
        } else {
            self.write_32(vaddr, data as u32);
            self.write_32(vaddr + 4, (data >> 32) as u32);
        }
    }

    /// Check if an address range is within the current address space.
    /// Matches upstream `AddressSpaceContains`.
    fn address_space_contains(&self, addr: u64, size: usize) -> bool {
        if self.current_page_table.is_null() {
            return false;
        }
        let pt = unsafe { &*self.current_page_table };
        let max_addr = 1u64 << pt.current_address_space_width_in_bits;
        let end = addr.checked_add(size as u64);
        match end {
            Some(e) => e >= addr && e <= max_addr,
            None => false,
        }
    }

    /// Read a block of data from guest memory.
    /// Matches upstream `Memory::ReadBlock` (via WalkBlock pattern).
    pub fn read_block(&self, src_addr: u64, dest: &mut [u8]) -> bool {
        let size = dest.len();
        let trace_read_ptr = {
            static RB_TARGET: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
            *RB_TARGET.get_or_init(|| {
                std::env::var("RUZU_TRACE_READ_BLOCK_PTR")
                    .ok()
                    .and_then(|raw| {
                        let raw = raw.trim();
                        let digits = raw
                            .strip_prefix("0x")
                            .or_else(|| raw.strip_prefix("0X"))
                            .unwrap_or(raw);
                        u64::from_str_radix(digits, 16)
                            .ok()
                            .or_else(|| raw.parse::<u64>().ok())
                    })
            })
        };

        // Upstream: AddressSpaceContains check before walking pages.
        if !self.address_space_contains(src_addr, size) {
            log::error!("Unmapped ReadBlock @ {:#018x} size={:#x}", src_addr, size);
            dest.fill(0);
            return false;
        }

        let mut remaining = size;
        let mut offset = 0usize;
        let mut vaddr = src_addr;
        let mut user_accessible = true;

        while remaining > 0 {
            let page_offset = (vaddr & PAGE_MASK) as usize;
            let copy_amount = ((PAGE_SIZE as usize) - page_offset).min(remaining);

            let ptr = self.get_pointer_impl(vaddr);
            if ptr.is_null() {
                log::error!("Unmapped ReadBlock @ {:#018x}", vaddr);
                // Zero destination for unmapped pages, matching upstream.
                dest[offset..offset + copy_amount].fill(0);
                user_accessible = false;
            } else {
                if trace_read_ptr
                    .is_some_and(|target| vaddr <= target && target < vaddr + copy_amount as u64)
                {
                    log::info!(
                        "[READ_BLOCK_PTR] src=0x{:X} page_vaddr=0x{:X} ptr={:p} copy=0x{:X} size=0x{:X}",
                        src_addr,
                        vaddr,
                        ptr,
                        copy_amount,
                        size,
                    );
                }
                if self.page_type_at(vaddr) == Some(PageType::RasterizerCachedMemory) {
                    self.handle_rasterizer_download(vaddr, copy_amount);
                }
                unsafe {
                    std::ptr::copy_nonoverlapping(ptr, dest[offset..].as_mut_ptr(), copy_amount);
                }
            }

            vaddr += copy_amount as u64;
            offset += copy_amount;
            remaining -= copy_amount;
        }
        user_accessible
    }

    /// SEGV-safe variant for host-side HLE consumers that read guest buffers.
    ///
    /// The normal `read_block` path copies from raw translated host pointers,
    /// matching upstream's fast path. Audio decode can observe transiently stale
    /// or protected host pages while consuming guest wave buffers; use
    /// `process_vm_readv` here so an inaccessible host page becomes `EFAULT`
    /// instead of a process-wide SIGSEGV.
    pub fn read_block_checked(&self, src_addr: u64, dest: &mut [u8]) -> bool {
        self.read_block_checked_impl(src_addr, dest, true)
    }

    /// Same as `read_block_checked`, but suppresses diagnostics for callers
    /// with an expected fallback path.
    pub fn read_block_checked_quiet(&self, src_addr: u64, dest: &mut [u8]) -> bool {
        self.read_block_checked_impl(src_addr, dest, false)
    }

    fn read_block_checked_impl(&self, src_addr: u64, dest: &mut [u8], log_errors: bool) -> bool {
        let size = dest.len();
        if size == 0 {
            return true;
        }

        if !self.address_space_contains(src_addr, size) {
            if log_errors {
                log::error!(
                    "Unmapped checked ReadBlock @ {:#018x} size={:#x}",
                    src_addr,
                    size
                );
            }
            dest.fill(0);
            return false;
        }

        #[cfg(target_os = "linux")]
        let self_pid = unsafe { libc::getpid() };

        // SEGV-safe read helper: uses process_vm_readv on Linux (returns EFAULT
        // for PROT_NONE pages), falls back to direct copy on macOS.
        let vm_read = |src_ptr: *const u8, dst: &mut [u8]| -> isize {
            #[cfg(target_os = "linux")]
            {
                let local_iov = libc::iovec {
                    iov_base: dst.as_mut_ptr() as *mut libc::c_void,
                    iov_len: dst.len(),
                };
                let remote_iov = libc::iovec {
                    iov_base: src_ptr as *mut libc::c_void,
                    iov_len: dst.len(),
                };
                unsafe {
                    libc::process_vm_readv(
                        self_pid,
                        &local_iov as *const _,
                        1,
                        &remote_iov as *const _,
                        1,
                        0,
                    )
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                unsafe { std::ptr::copy_nonoverlapping(src_ptr, dst.as_mut_ptr(), dst.len()) };
                dst.len() as isize
            }
        };

        let first_ptr = self.get_pointer_impl(src_addr);
        if !first_ptr.is_null() {
            self.handle_rasterizer_download_for_read_range(src_addr, size);
            let read = vm_read(first_ptr as *const u8, dest);
            if read == size as isize {
                return true;
            }
            if log_errors {
                log::error!(
                    "checked ReadBlock fast path failed @ {:#018x} size={:#x} read={}",
                    src_addr,
                    size,
                    read
                );
            }
        }

        let mut remaining = size;
        let mut offset = 0usize;
        let mut vaddr = src_addr;
        let mut user_accessible = true;

        while remaining > 0 {
            let page_offset = (vaddr & PAGE_MASK) as usize;
            let copy_amount = ((PAGE_SIZE as usize) - page_offset).min(remaining);

            let ptr = self.get_pointer_impl(vaddr);
            if ptr.is_null() {
                if log_errors {
                    log::error!("Unmapped checked ReadBlock @ {:#018x}", vaddr);
                }
                dest[offset..offset + copy_amount].fill(0);
                user_accessible = false;
            } else {
                if self.page_type_at(vaddr) == Some(PageType::RasterizerCachedMemory) {
                    self.handle_rasterizer_download(vaddr, copy_amount);
                }
                let read = vm_read(ptr as *const u8, &mut dest[offset..offset + copy_amount]);
                if read != copy_amount as isize {
                    if log_errors {
                        log::error!(
                            "checked ReadBlock failed @ {:#018x} size={:#x} read={}",
                            vaddr,
                            copy_amount,
                            read
                        );
                    }
                    dest[offset..offset + copy_amount].fill(0);
                    user_accessible = false;
                }
            }

            vaddr += copy_amount as u64;
            offset += copy_amount;
            remaining -= copy_amount;
        }
        user_accessible
    }

    /// Write a block of data to guest memory.
    /// Matches upstream `Memory::WriteBlock` (via WalkBlock pattern).
    pub fn write_block(&self, dest_addr: u64, src: &[u8]) -> bool {
        maybe_trace_write_block_values("write_block", dest_addr, src);
        let size = src.len();

        // RUZU_WATCH_BLOCK=START:LEN — emit a backtrace + first 64 bytes of
        // the source whenever a block write touches [START, START+LEN). Used
        // to find HLE-side writers of guest memory that the JIT
        // memory_write_NN watch doesn't see.
        if common::env_flag!("RUZU_WATCH_BLOCK") {
            check_block_watch("write_block", dest_addr, src);
        }

        // Upstream: AddressSpaceContains check before walking pages.
        if !self.address_space_contains(dest_addr, size) {
            log::error!("Unmapped WriteBlock @ {:#018x} size={:#x}", dest_addr, size);
            if common::env_flag!("RUZU_TRACE_UNMAPPED_BT") {
                use std::sync::atomic::{AtomicU32, Ordering};
                static SHOWN: AtomicU32 = AtomicU32::new(0);
                let n = SHOWN.fetch_add(1, Ordering::Relaxed);
                if n < 5 {
                    let bt = std::backtrace::Backtrace::force_capture();
                    eprintln!(
                        "[UNMAPPED_WB_BT #{}] dest=0x{:016X} size=0x{:X}\n{}",
                        n, dest_addr, size, bt
                    );
                }
            }
            return false;
        }

        let mut remaining = size;
        let mut offset = 0usize;
        let mut vaddr = dest_addr;
        let mut user_accessible = true;

        while remaining > 0 {
            let page_offset = (vaddr & PAGE_MASK) as usize;
            let copy_amount = ((PAGE_SIZE as usize) - page_offset).min(remaining);
            let ptr = self.get_pointer_impl(vaddr);
            if ptr.is_null() {
                log::error!("Unmapped WriteBlock @ {:#018x}", vaddr);
                user_accessible = false;
            } else {
                if self.page_type_at(vaddr) == Some(PageType::RasterizerCachedMemory) {
                    self.handle_rasterizer_write(vaddr, copy_amount);
                }
                unsafe {
                    std::ptr::copy_nonoverlapping(src[offset..].as_ptr(), ptr, copy_amount);
                }
            }
            vaddr += copy_amount as u64;
            offset += copy_amount;
            remaining -= copy_amount;
        }
        user_accessible
    }

    /// Zero a block of guest memory.
    /// Matches upstream `Memory::ZeroBlock` (via WalkBlock pattern).
    ///
    /// For pages backed by HostMemory, batches contiguous physical runs and
    /// calls `HostMemory::clear_backing_region`, which uses `madvise(MADV_REMOVE)`
    /// on Linux — a single syscall reclaims the backing pages lazily instead of
    /// memset'ing every byte. This matches upstream's
    /// `DeviceMemory().buffer.ClearBackingRegion` path and avoids the O(size)
    /// Zero a region of physical memory (DeviceMemory backing buffer).
    /// Used by KPageTableBase pool-allocation callers that need to clear
    /// pages before mapping them into virtual address space (so the VA
    /// mapping doesn't yet exist for `zero_block`'s VA path).
    ///
    /// Upstream calls `ClearBackingRegion(m_system, block.GetAddress(),
    /// block.GetSize(), m_heap_fill_value)` per block in the page group.
    /// `m_heap_fill_value` is typically zero in the standard build profile;
    /// upstream uses non-zero values only when developer poisoning is on.
    pub fn zero_phys_block(&self, phys_addr: u64, size: usize) {
        if size == 0 {
            return;
        }
        let buffer = unsafe { &*self.buffer };
        let backing_size = buffer.backing_size();
        // Translate phys → host pointer (DeviceMemory is identity-mapped
        // from `dram_memory_map::BASE` into the host backing buffer).
        let dram_base = crate::device_memory::dram_memory_map::BASE;
        if phys_addr < dram_base {
            log::error!("zero_phys_block: phys {:#x} below DRAM base", phys_addr);
            return;
        }
        let host_offset = (phys_addr - dram_base) as usize;
        if host_offset
            .checked_add(size)
            .map(|end| end > backing_size)
            .unwrap_or(true)
        {
            log::error!(
                "zero_phys_block: phys {:#x}+{:#x} out of backing (backing_size={:#x})",
                phys_addr,
                size,
                backing_size
            );
            return;
        }
        buffer.clear_backing_region(host_offset, size, 0);
    }

    /// Read a block from physical DeviceMemory backing.
    ///
    /// Used by kernel page-table helpers after they have already performed the
    /// upstream virtual-address state checks and page-table traversal.
    pub fn read_phys_block(&self, phys_addr: u64, dest: &mut [u8]) -> bool {
        let size = dest.len();
        if size == 0 {
            return true;
        }

        let buffer = unsafe { &*self.buffer };
        let backing_base = buffer.backing_base_pointer() as usize;
        let backing_size = buffer.backing_size();
        let dram_base = crate::device_memory::dram_memory_map::BASE;
        if phys_addr < dram_base {
            log::error!("read_phys_block: phys {:#x} below DRAM base", phys_addr);
            dest.fill(0);
            return false;
        }
        let host_offset = (phys_addr - dram_base) as usize;
        if host_offset
            .checked_add(size)
            .map(|end| end > backing_size)
            .unwrap_or(true)
        {
            log::error!(
                "read_phys_block: phys {:#x}+{:#x} out of backing (backing_size={:#x})",
                phys_addr,
                size,
                backing_size
            );
            dest.fill(0);
            return false;
        }

        unsafe {
            std::ptr::copy_nonoverlapping(
                (backing_base + host_offset) as *const u8,
                dest.as_mut_ptr(),
                size,
            );
        }
        true
    }

    /// Write a block to physical DeviceMemory backing.
    ///
    /// Used by kernel page-table helpers after upstream-equivalent destination
    /// state/traversal validation has selected a linear-mapped physical run.
    pub fn write_phys_block(&self, phys_addr: u64, src: &[u8]) -> bool {
        let size = src.len();
        if size == 0 {
            return true;
        }

        let buffer = unsafe { &*self.buffer };
        let backing_base = buffer.backing_base_pointer() as usize;
        let backing_size = buffer.backing_size();
        let dram_base = crate::device_memory::dram_memory_map::BASE;
        if phys_addr < dram_base {
            log::error!("write_phys_block: phys {:#x} below DRAM base", phys_addr);
            return false;
        }
        let host_offset = (phys_addr - dram_base) as usize;
        if host_offset
            .checked_add(size)
            .map(|end| end > backing_size)
            .unwrap_or(true)
        {
            log::error!(
                "write_phys_block: phys {:#x}+{:#x} out of backing (backing_size={:#x})",
                phys_addr,
                size,
                backing_size
            );
            return false;
        }

        unsafe {
            std::ptr::copy_nonoverlapping(
                src.as_ptr(),
                (backing_base + host_offset) as *mut u8,
                size,
            );
        }
        true
    }

    /// Copy between two physical DeviceMemory backing ranges.
    ///
    /// Mirrors upstream `std::memcpy(GetHeapVirtualPointer(dst),
    /// GetHeapVirtualPointer(src), size)` after the kernel page-table owner has
    /// validated both physical runs.
    pub fn copy_phys_to_phys(&self, dst_phys_addr: u64, src_phys_addr: u64, size: usize) -> bool {
        if size == 0 {
            return true;
        }

        let buffer = unsafe { &*self.buffer };
        let backing_base = buffer.backing_base_pointer() as usize;
        let backing_size = buffer.backing_size();
        let dram_base = crate::device_memory::dram_memory_map::BASE;

        let Some(dst_offset) = dst_phys_addr
            .checked_sub(dram_base)
            .map(|offset| offset as usize)
        else {
            log::error!(
                "copy_phys_to_phys: dst phys {:#x} below DRAM base",
                dst_phys_addr
            );
            return false;
        };
        let Some(src_offset) = src_phys_addr
            .checked_sub(dram_base)
            .map(|offset| offset as usize)
        else {
            log::error!(
                "copy_phys_to_phys: src phys {:#x} below DRAM base",
                src_phys_addr
            );
            return false;
        };

        let dst_in_range = dst_offset
            .checked_add(size)
            .map(|end| end <= backing_size)
            .unwrap_or(false);
        let src_in_range = src_offset
            .checked_add(size)
            .map(|end| end <= backing_size)
            .unwrap_or(false);
        if !dst_in_range || !src_in_range {
            log::error!(
                "copy_phys_to_phys: dst {:#x} src {:#x} size {:#x} out of backing (backing_size={:#x})",
                dst_phys_addr,
                src_phys_addr,
                size,
                backing_size
            );
            return false;
        }

        unsafe {
            std::ptr::copy(
                (backing_base + src_offset) as *const u8,
                (backing_base + dst_offset) as *mut u8,
                size,
            );
        }
        true
    }

    /// Copy from physical DeviceMemory backing into guest virtual memory.
    ///
    /// This is the Rust owner-local counterpart to upstream callers that pass
    /// `GetLinearMappedVirtualPointer(...)` into `Memory::WriteBlock(...)`.
    pub fn copy_phys_to_guest(&self, dest_addr: u64, phys_addr: u64, size: usize) -> bool {
        if size == 0 {
            return true;
        }

        let buffer = unsafe { &*self.buffer };
        let backing_base = buffer.backing_base_pointer() as usize;
        let backing_size = buffer.backing_size();
        let dram_base = crate::device_memory::dram_memory_map::BASE;
        if phys_addr < dram_base {
            log::error!("copy_phys_to_guest: phys {:#x} below DRAM base", phys_addr);
            return false;
        }
        let host_offset = (phys_addr - dram_base) as usize;
        if host_offset
            .checked_add(size)
            .map(|end| end > backing_size)
            .unwrap_or(true)
        {
            log::error!(
                "copy_phys_to_guest: phys {:#x}+{:#x} out of backing (backing_size={:#x})",
                phys_addr,
                size,
                backing_size
            );
            return false;
        }

        let src =
            unsafe { std::slice::from_raw_parts((backing_base + host_offset) as *const u8, size) };
        self.write_block(dest_addr, src)
    }

    /// Copy from guest virtual memory into physical DeviceMemory backing.
    ///
    /// This mirrors upstream callers that pass
    /// `GetLinearMappedVirtualPointer(...)` as the destination buffer to
    /// `Memory::ReadBlock(...)`.
    pub fn copy_guest_to_phys(&self, phys_addr: u64, src_addr: u64, size: usize) -> bool {
        if size == 0 {
            return true;
        }

        let buffer = unsafe { &*self.buffer };
        let backing_base = buffer.backing_base_pointer() as usize;
        let backing_size = buffer.backing_size();
        let dram_base = crate::device_memory::dram_memory_map::BASE;
        if phys_addr < dram_base {
            log::error!("copy_guest_to_phys: phys {:#x} below DRAM base", phys_addr);
            return false;
        }
        let host_offset = (phys_addr - dram_base) as usize;
        if host_offset
            .checked_add(size)
            .map(|end| end > backing_size)
            .unwrap_or(true)
        {
            log::error!(
                "copy_guest_to_phys: phys {:#x}+{:#x} out of backing (backing_size={:#x})",
                phys_addr,
                size,
                backing_size
            );
            return false;
        }

        let dest = unsafe {
            std::slice::from_raw_parts_mut((backing_base + host_offset) as *mut u8, size)
        };
        self.read_block(src_addr, dest)
    }

    pub fn zero_block(&self, dest_addr: u64, size: usize) -> bool {
        if !self.address_space_contains(dest_addr, size) {
            log::error!("Unmapped ZeroBlock @ {:#018x} size={:#x}", dest_addr, size);
            return false;
        }

        let buffer = unsafe { &*self.buffer };
        let backing_base = buffer.backing_base_pointer() as usize;
        let backing_size = buffer.backing_size();

        let mut remaining = size;
        let mut vaddr = dest_addr;
        let mut user_accessible = true;

        while remaining > 0 {
            let page_offset = (vaddr & PAGE_MASK) as usize;
            let first_chunk = ((PAGE_SIZE as usize) - page_offset).min(remaining);

            let first_ptr = self.get_pointer_impl(vaddr);
            if first_ptr.is_null() {
                log::error!("Unmapped ZeroBlock @ {:#018x}", vaddr);
                user_accessible = false;
                vaddr += first_chunk as u64;
                remaining -= first_chunk;
                continue;
            }

            // Extend the run as long as following pages are contiguous in host
            // memory (ptr advances exactly by the chunk size each step).
            let mut run = first_chunk;
            let mut cur_vaddr = vaddr + first_chunk as u64;
            let mut expected_ptr = first_ptr as usize + first_chunk;
            while run < remaining {
                let chunk = (PAGE_SIZE as usize).min(remaining - run);
                let p = self.get_pointer_impl(cur_vaddr);
                if p.is_null() || (p as usize) != expected_ptr {
                    break;
                }
                run += chunk;
                cur_vaddr += chunk as u64;
                expected_ptr += chunk;
            }

            self.handle_rasterizer_write(vaddr, run);

            let phys_off = (first_ptr as usize).wrapping_sub(backing_base);
            if phys_off < backing_size && phys_off + run <= backing_size {
                buffer.clear_backing_region(phys_off, run, 0);
            } else {
                // Not in the HostMemory backing buffer (debug/rasterizer paths):
                // fall back to memset.
                unsafe {
                    std::ptr::write_bytes(first_ptr, 0, run);
                }
            }

            vaddr += run as u64;
            remaining -= run;
        }
        user_accessible
    }

    /// Copy a block within guest memory.
    /// Matches upstream `Memory::CopyBlock`.
    pub fn copy_block(&self, dest_addr: u64, src_addr: u64, size: usize) -> bool {
        let mut buf = vec![0u8; size];
        self.read_block(src_addr, &mut buf);
        self.write_block(dest_addr, &buf)
    }

    /// Check if a virtual address is valid (mapped).
    /// Matches upstream `Memory::IsValidVirtualAddress`.
    pub fn is_valid_virtual_address(&self, vaddr: u64) -> bool {
        if self.current_page_table.is_null() {
            return false;
        }
        let pt = unsafe { &*self.current_page_table };
        let page = (vaddr >> PAGE_BITS) as usize;
        if page >= pt.pointers.size() {
            return false;
        }
        let (pointer, ptype) = pt.pointers[page].pointer_type();
        pointer != 0 || ptype == PageType::RasterizerCachedMemory || ptype == PageType::DebugMemory
    }

    /// Check if a virtual address range is valid (all pages mapped).
    /// Matches upstream `Memory::IsValidVirtualAddressRange`.
    pub fn is_valid_virtual_address_range(&self, base: u64, size: u64) -> bool {
        let end = base + size;
        let mut page = base & !(PAGE_MASK);
        while page < end {
            if !self.is_valid_virtual_address(page) {
                return false;
            }
            page += PAGE_SIZE;
        }
        true
    }

    // =========================================================================
    // Exclusive Write (atomic CAS) via PageTable pointers
    // Matches upstream Core::Memory::Memory::WriteExclusive* methods.
    // =========================================================================

    /// Exclusive write u8 with atomic CAS.
    /// Matches upstream `Memory::WriteExclusive8`.
    pub fn write_exclusive_8(&self, vaddr: u64, value: u8, expected: u8) -> bool {
        let ptr = self.get_pointer_impl(vaddr);
        if ptr.is_null() {
            trace_unmapped_guest_access("EXCLUSIVE_WRITE", vaddr, 8);
            log::error!("Unmapped WriteExclusive8 @ {:#018x}", vaddr);
            return true;
        }
        unsafe {
            let atomic = &*(ptr as *const std::sync::atomic::AtomicU8);
            atomic
                .compare_exchange(
                    expected,
                    value,
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                )
                .is_ok()
        }
    }

    /// Exclusive write u16 with atomic CAS.
    /// Matches upstream `Memory::WriteExclusive16`.
    pub fn write_exclusive_16(&self, vaddr: u64, value: u16, expected: u16) -> bool {
        let ptr = self.get_pointer_impl(vaddr);
        if ptr.is_null() {
            trace_unmapped_guest_access("EXCLUSIVE_WRITE", vaddr, 16);
            log::error!("Unmapped WriteExclusive16 @ {:#018x}", vaddr);
            return true;
        }
        unsafe {
            let atomic = &*(ptr as *const std::sync::atomic::AtomicU16);
            atomic
                .compare_exchange(
                    expected,
                    value,
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                )
                .is_ok()
        }
    }

    /// Exclusive write u32 with atomic CAS.
    /// Matches upstream `Memory::WriteExclusive32`.
    pub fn write_exclusive_32(&self, vaddr: u64, value: u32, expected: u32) -> bool {
        let ptr = self.get_pointer_impl(vaddr);
        if ptr.is_null() {
            trace_unmapped_guest_access("EXCLUSIVE_WRITE", vaddr, 32);
            log::error!("Unmapped WriteExclusive32 @ {:#018x}", vaddr);
            return true;
        }
        unsafe {
            let atomic = &*(ptr as *const std::sync::atomic::AtomicU32);
            atomic
                .compare_exchange(
                    expected,
                    value,
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                )
                .is_ok()
        }
    }

    /// Exclusive write u64 with atomic CAS.
    /// Matches upstream `Memory::WriteExclusive64`.
    pub fn write_exclusive_64(&self, vaddr: u64, value: u64, expected: u64) -> bool {
        let ptr = self.get_pointer_impl(vaddr);
        if ptr.is_null() {
            trace_unmapped_guest_access("EXCLUSIVE_WRITE", vaddr, 64);
            log::error!("Unmapped WriteExclusive64 @ {:#018x}", vaddr);
            return true;
        }
        unsafe {
            let atomic = &*(ptr as *const std::sync::atomic::AtomicU64);
            atomic
                .compare_exchange(
                    expected,
                    value,
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                )
                .is_ok()
        }
    }

    /// Exclusive write 128-bit. Matches upstream `Memory::WriteExclusive128`
    /// which calls `Common::AtomicCompareAndSwap` (= `_InterlockedCompareExchange128`
    /// on MSVC, `__sync_bool_compare_and_swap` on GCC/Clang for `__int128`).
    ///
    /// On x86_64 hosts this lowers to a single `lock cmpxchg16b` — atomic
    /// against any concurrent 8/16-byte load or store at the same 16-byte
    /// boundary. The previous two-step 64-bit CAS implementation was a
    /// structural divergence from upstream: it could observe or produce
    /// torn writes when another core read/wrote the same 16-byte slot
    /// between the two halves.
    #[cfg(target_arch = "x86_64")]
    pub fn write_exclusive_128(
        &self,
        vaddr: u64,
        value_lo: u64,
        value_hi: u64,
        expected_lo: u64,
        expected_hi: u64,
    ) -> bool {
        let ptr = self.get_pointer_impl(vaddr);
        if ptr.is_null() {
            trace_unmapped_guest_access("EXCLUSIVE_WRITE", vaddr, 128);
            log::error!("Unmapped WriteExclusive128 @ {:#018x}", vaddr);
            return true;
        }
        // 16-byte alignment is a hardware requirement for cmpxchg16b.
        // dynarmic only emits STXP for 16-byte-aligned vaddrs per ARM ARM.
        unsafe { cmpxchg16b(ptr, value_lo, value_hi, expected_lo, expected_hi) }
    }

    /// Non-x86_64 fallback: two 64-bit CAS (NOT atomic across the boundary).
    /// Matches the previous behavior on platforms without `cmpxchg16b`.
    #[cfg(not(target_arch = "x86_64"))]
    pub fn write_exclusive_128(
        &self,
        vaddr: u64,
        value_lo: u64,
        value_hi: u64,
        expected_lo: u64,
        expected_hi: u64,
    ) -> bool {
        let lo_ok = self.write_exclusive_64(vaddr, value_lo, expected_lo);
        if !lo_ok {
            return false;
        }
        self.write_exclusive_64(vaddr + 8, value_hi, expected_hi)
    }
}

/// Atomic 128-bit compare-and-swap on a 16-byte aligned pointer.
///
/// Returns `true` if `[ptr..ptr+16] == expected_lo:expected_hi` and the
/// 16-byte value was swapped to `value_lo:value_hi`. Returns `false`
/// otherwise (and memory is unchanged).
///
/// # Safety
/// - `ptr` must point to a 16-byte aligned, writable u128 location.
/// - Concurrent accesses to the same location must use compatible atomic
///   operations (lock cmpxchg16b, lock dec, etc.) to be sequentially consistent.
#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn cmpxchg16b(
    ptr: *mut u8,
    value_lo: u64,
    value_hi: u64,
    expected_lo: u64,
    expected_hi: u64,
) -> bool {
    debug_assert!(
        (ptr as usize) & 0xF == 0,
        "cmpxchg16b: misaligned ptr {:p}",
        ptr
    );
    let success: u8;
    std::arch::asm!(
        "xchg rbx, {rbx_save}",
        "lock cmpxchg16b [{ptr}]",
        "setz {success}",
        "mov rbx, {rbx_save}",
        ptr = in(reg) ptr,
        rbx_save = inout(reg) value_lo => _,
        inout("rax") expected_lo => _,
        inout("rdx") expected_hi => _,
        in("rcx") value_hi,
        success = lateout(reg_byte) success,
        options(nostack),
    );
    success != 0
}

/// Deferred rasterizer notifications produced by
/// `Memory::collect_rasterizer_write_ranges` (phase 1, under the memory
/// lock). `apply` is phase 2 and must be called after the memory lock has
/// been released, mirroring upstream's lock-free `PerformCacheOperation`.
pub struct RasterizerWriteBatch {
    system: SystemRef,
    dirty_manager: Option<Arc<GpuDirtyMemoryManager>>,
    /// Upstream's `sys_core_guard`, retained by deferred system-slot writes.
    sys_core_guard: Option<Arc<Mutex<()>>>,
    /// Merged `(device_addr, size)` ranges of rasterizer-cached pages.
    ranges: Vec<(u64, usize)>,
}

impl RasterizerWriteBatch {
    /// Notify the rasterizer for every collected range. Same per-range logic
    /// as `Memory::handle_rasterizer_write`, without touching `Memory` state.
    pub fn apply(self) {
        let _sys_core_guard = self
            .sys_core_guard
            .as_ref()
            .map(|guard| guard.lock().unwrap());
        for (device_addr, size) in self.ranges {
            let do_collection = self.system.is_null()
                || self
                    .system
                    .get()
                    .gpu_core()
                    .map(|gpu| gpu.on_cpu_write(device_addr, size as u64))
                    .unwrap_or(false);
            if !do_collection {
                continue;
            }
            if let Some(manager) = &self.dirty_manager {
                manager.collect(device_addr, size);
            }
        }
    }
}

#[cfg(test)]
mod rasterizer_write_batch_tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn system_slot_batch_waits_for_sys_core_guard() {
        let manager = Arc::new(GpuDirtyMemoryManager::new());
        let sys_core_guard = Arc::new(Mutex::new(()));
        let held_guard = sys_core_guard.lock().unwrap();
        let batch = RasterizerWriteBatch {
            system: SystemRef::null(),
            dirty_manager: Some(Arc::clone(&manager)),
            sys_core_guard: Some(Arc::clone(&sys_core_guard)),
            ranges: vec![(0x4000, 64)],
        };
        let (done_tx, done_rx) = mpsc::channel();

        let worker = std::thread::spawn(move || {
            batch.apply();
            done_tx.send(()).unwrap();
        });

        assert!(
            done_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "system-slot batch bypassed sys_core_guard"
        );
        drop(held_guard);
        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("system-slot batch did not resume after guard release");
        worker.join().unwrap();

        let mut ranges = Vec::new();
        manager.gather(&mut |address, size| ranges.push((address, size)));
        assert_eq!(ranges, vec![(0x4000, 64)]);
    }
}

#[cfg(test)]
mod cmpxchg16b_tests {
    #[cfg(target_arch = "x86_64")]
    use super::cmpxchg16b;

    #[cfg(target_arch = "x86_64")]
    #[repr(align(16))]
    struct Aligned16([u64; 2]);

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn success_on_match() {
        let mut slot = Aligned16([0xDEADBEEF_CAFEBABE, 0x0123456789ABCDEF]);
        let ok = unsafe {
            cmpxchg16b(
                slot.0.as_mut_ptr() as *mut u8,
                0x1111_2222_3333_4444,
                0x5555_6666_7777_8888,
                0xDEADBEEF_CAFEBABE,
                0x0123456789ABCDEF,
            )
        };
        assert!(ok);
        assert_eq!(slot.0[0], 0x1111_2222_3333_4444);
        assert_eq!(slot.0[1], 0x5555_6666_7777_8888);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn failure_on_mismatch_leaves_memory_unchanged() {
        let mut slot = Aligned16([0x1111_1111_1111_1111, 0x2222_2222_2222_2222]);
        let ok = unsafe {
            cmpxchg16b(
                slot.0.as_mut_ptr() as *mut u8,
                0xAAAA,
                0xBBBB,
                0xDEAD, // wrong expected
                0xBEEF,
            )
        };
        assert!(!ok);
        assert_eq!(slot.0[0], 0x1111_1111_1111_1111);
        assert_eq!(slot.0[1], 0x2222_2222_2222_2222);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn failure_on_hi_mismatch_only() {
        // The atomicity contract specifically requires that mismatching
        // EITHER half causes the swap to fail. The previous two-step
        // 64-bit-CAS implementation would partially succeed (lo swapped,
        // hi failed → memory left with new_lo + old_hi). True
        // cmpxchg16b leaves BOTH halves unchanged.
        let mut slot = Aligned16([0xAAAA, 0xBBBB]);
        let ok = unsafe {
            cmpxchg16b(
                slot.0.as_mut_ptr() as *mut u8,
                0x1111,
                0x2222,
                0xAAAA, // matches lo
                0xCCCC, // does NOT match hi
            )
        };
        assert!(!ok);
        assert_eq!(slot.0[0], 0xAAAA);
        assert_eq!(slot.0[1], 0xBBBB);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn round_trip_all_ones_to_all_zeros_and_back() {
        let mut slot = Aligned16([0u64, 0u64]);
        let ok1 = unsafe { cmpxchg16b(slot.0.as_mut_ptr() as *mut u8, u64::MAX, u64::MAX, 0, 0) };
        assert!(ok1);
        assert_eq!(slot.0, [u64::MAX, u64::MAX]);

        let ok2 = unsafe { cmpxchg16b(slot.0.as_mut_ptr() as *mut u8, 0, 0, u64::MAX, u64::MAX) };
        assert!(ok2);
        assert_eq!(slot.0, [0, 0]);
    }
}

#[cfg(test)]
mod process_fastmem_tests {
    use super::*;
    use common::host_memory::MemoryPermission;

    fn memory_for_device(device_memory: &DeviceMemory) -> Memory {
        unsafe {
            Memory::new(
                SystemRef::null(),
                device_memory as *const _,
                &device_memory.buffer,
            )
        }
    }

    #[test]
    fn only_application_page_tables_receive_the_fastmem_arena() {
        let device_memory = DeviceMemory::with_size(0x20_000);
        let mut application_memory = memory_for_device(&device_memory);
        let mut applet_memory = memory_for_device(&device_memory);
        let mut application_page_table = PageTable::new();
        let mut applet_page_table = PageTable::new();
        application_page_table.resize(32, PAGE_BITS);
        applet_page_table.resize(32, PAGE_BITS);

        application_memory.set_current_page_table(&mut application_page_table, true);
        applet_memory.set_current_page_table(&mut applet_page_table, false);

        let settings = common::settings::values();
        if common::settings::is_fastmem_enabled(&settings) {
            assert_eq!(
                application_page_table.fastmem_arena,
                device_memory.buffer.virtual_base_pointer()
            );
        } else {
            assert!(application_page_table.fastmem_arena.is_null());
        }
        assert!(applet_page_table.fastmem_arena.is_null());
    }

    #[test]
    fn debugger_region_switches_memory_pages_to_slow_callbacks_and_back() {
        const VADDR: u64 = 0x4000;
        const DEVICE_OFFSET: u64 = 0x2000;

        let device_memory = DeviceMemory::with_size(0x20_000);
        let mut memory = memory_for_device(&device_memory);
        let mut page_table = PageTable::new();
        page_table.resize(32, PAGE_BITS);
        memory.set_current_page_table(&mut page_table, true);
        memory.map_memory_region(
            &mut page_table,
            VADDR,
            PAGE_SIZE,
            dram_memory_map::BASE + DEVICE_OFFSET,
            MemoryPermission::READ_WRITE,
            false,
        );

        memory.mark_region_debug(VADDR, PAGE_SIZE, true);
        let page = (VADDR >> PAGE_BITS) as usize;
        assert_eq!(
            PageInfo::extract_type(page_table.pointers[page].raw_value()),
            PageType::DebugMemory
        );

        memory.mark_region_debug(VADDR, PAGE_SIZE, false);
        assert_eq!(
            PageInfo::extract_type(page_table.pointers[page].raw_value()),
            PageType::Memory
        );
        assert_ne!(
            PageInfo::extract_pointer(page_table.pointers[page].raw_value()),
            0
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn applet_mapping_does_not_replace_the_application_fastmem_alias() {
        const VADDR: u64 = 0x4000;
        const APPLICATION_OFFSET: usize = 0x2000;
        const APPLET_OFFSET: usize = 0x6000;
        const APPLICATION_VALUE: u64 = 0x1122_3344_5566_7788;
        const APPLET_VALUE: u64 = 0x8877_6655_4433_2211;

        let device_memory = DeviceMemory::with_size(0x20_000);
        let mut application_memory = memory_for_device(&device_memory);
        let mut applet_memory = memory_for_device(&device_memory);
        let mut application_page_table = PageTable::new();
        let mut applet_page_table = PageTable::new();
        application_page_table.resize(32, PAGE_BITS);
        applet_page_table.resize(32, PAGE_BITS);

        unsafe {
            device_memory
                .buffer
                .backing_base_pointer()
                .add(APPLICATION_OFFSET)
                .cast::<u64>()
                .write(APPLICATION_VALUE);
            device_memory
                .buffer
                .backing_base_pointer()
                .add(APPLET_OFFSET)
                .cast::<u64>()
                .write(APPLET_VALUE);
        }

        application_memory.set_current_page_table(&mut application_page_table, true);
        if application_page_table.fastmem_arena.is_null() {
            return;
        }
        application_memory.map_memory_region(
            &mut application_page_table,
            VADDR,
            PAGE_SIZE,
            dram_memory_map::BASE + APPLICATION_OFFSET as u64,
            MemoryPermission::READ_WRITE,
            false,
        );
        let fastmem_value = || unsafe {
            application_page_table
                .fastmem_arena
                .add(VADDR as usize)
                .cast::<u64>()
                .read()
        };
        assert_eq!(fastmem_value(), APPLICATION_VALUE);

        applet_memory.set_current_page_table(&mut applet_page_table, false);
        applet_memory.map_memory_region(
            &mut applet_page_table,
            VADDR,
            PAGE_SIZE,
            dram_memory_map::BASE + APPLET_OFFSET as u64,
            MemoryPermission::READ_WRITE,
            false,
        );

        assert_eq!(fastmem_value(), APPLICATION_VALUE);
        assert_eq!(applet_memory.read_64(VADDR), APPLET_VALUE);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod zero_phys_block_tests {
    use super::{dram_memory_map, DeviceMemory, Memory};
    use crate::core::{System, SystemRef};

    #[test]
    fn zero_phys_block_discards_resident_backing_pages() {
        const OFFSET: usize = 0x4000;
        const SIZE: usize = 0x4000;

        let system = System::new_for_test();
        let device_memory = Box::new(DeviceMemory::with_size(0x20_000));
        let memory = unsafe {
            Memory::new(
                SystemRef::from_ref(&system),
                &*device_memory,
                &device_memory.buffer,
            )
        };
        let pointer = unsafe { device_memory.buffer.backing_base_pointer().add(OFFSET) };
        unsafe {
            std::ptr::write_bytes(pointer, 0xA5, SIZE);
        }

        let mut residency = [0u8; SIZE / 0x1000];
        let before = unsafe {
            libc::mincore(
                pointer.cast(),
                SIZE,
                residency.as_mut_ptr().cast::<libc::c_uchar>(),
            )
        };
        assert_eq!(before, 0);
        assert!(residency.iter().all(|entry| entry & 1 != 0));

        memory.zero_phys_block(dram_memory_map::BASE + OFFSET as u64, SIZE);

        residency.fill(0);
        let after = unsafe {
            libc::mincore(
                pointer.cast(),
                SIZE,
                residency.as_mut_ptr().cast::<libc::c_uchar>(),
            )
        };
        assert_eq!(after, 0);
        assert!(residency.iter().all(|entry| entry & 1 == 0));
        let bytes = unsafe { std::slice::from_raw_parts(pointer, SIZE) };
        assert!(bytes.iter().all(|byte| *byte == 0));
    }
}

#[cfg(test)]
mod rasterizer_download_tests {
    use std::sync::{Arc, Mutex};

    use common::page_table::{PageInfo, PageTable, PageType};

    use super::{dram_memory_map, DeviceMemory, Memory, PAGE_BITS, PAGE_SIZE};
    use crate::core::{System, SystemRef};
    use crate::gpu_core::{
        FramebufferConfig, GpuChannelHandle, GpuCommandList, GpuCoreInterface,
        GpuMemoryManagerHandle, RasterizerDownloadArea,
    };
    use crate::hle::service::nvdrv::nvdata::NvFence;
    use crate::host1x_core::{Host1xChannelType, Host1xCoreInterface};

    struct FakeGpuMemoryManagerHandle;

    impl GpuMemoryManagerHandle for FakeGpuMemoryManagerHandle {
        fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
            self
        }

        fn map(
            &self,
            _gpu_addr: u64,
            _device_addr: u64,
            _size: u64,
            _kind: u32,
            _is_big_pages: bool,
        ) {
        }

        fn map_sparse(&self, _gpu_addr: u64, _size: u64, _is_big_pages: bool) {}

        fn unmap(&self, _gpu_addr: u64, _size: u64) {}
    }

    struct FakeGpuChannelHandle;

    impl GpuChannelHandle for FakeGpuChannelHandle {
        fn bind_memory_manager(&self, _memory_manager: Arc<dyn GpuMemoryManagerHandle>) {}

        fn init_channel(&self, _program_id: u64) {}

        fn bind_id(&self) -> i32 {
            0
        }
    }

    struct FakeGpuCore {
        reads: Arc<Mutex<Vec<(u64, u64)>>>,
        writes: Arc<Mutex<Vec<(u64, u64)>>>,
        download_size: u64,
    }

    impl GpuCoreInterface for FakeGpuCore {
        fn as_any(&self) -> &(dyn std::any::Any + Send) {
            self
        }

        fn allocate_channel_handle(&self) -> Arc<dyn GpuChannelHandle> {
            Arc::new(FakeGpuChannelHandle)
        }

        fn allocate_memory_manager_handle(
            &self,
            _address_space_bits: u64,
            _split_address: u64,
            _big_page_bits: u64,
            _page_bits: u64,
        ) -> Arc<dyn GpuMemoryManagerHandle> {
            Arc::new(FakeGpuMemoryManagerHandle)
        }

        fn init_address_space(&self, _memory_manager: Arc<dyn GpuMemoryManagerHandle>) {}

        fn push_gpu_entries(&self, _channel_id: i32, _entries: GpuCommandList) {}

        fn request_composite(&self, _layers: Vec<FramebufferConfig>, _fences: Vec<NvFence>) {}

        fn wait_for_composite(&self) {}

        fn on_cpu_write(&self, addr: u64, size: u64) -> bool {
            self.writes.lock().unwrap().push((addr, size));
            true
        }

        fn on_cpu_read(&self, addr: u64, size: u64) -> RasterizerDownloadArea {
            self.reads.lock().unwrap().push((addr, size));
            RasterizerDownloadArea {
                start_address: addr,
                end_address: addr.wrapping_add(self.download_size),
                preemptive: false,
            }
        }

        fn flush_region(&self, _addr: u64, _size: u64) {}
    }

    struct FakeHost1xCore {
        applied_host_ptrs: Arc<Mutex<Vec<usize>>>,
        aliases: Vec<u64>,
    }

    impl Host1xCoreInterface for FakeHost1xCore {
        fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
            self
        }

        fn get_host_syncpoint_value(&self, _id: u32) -> u32 {
            0
        }

        fn wait_host(&self, _id: u32, _expected_value: u32) {}

        fn register_guest_action(
            &self,
            _id: u32,
            _expected_value: u32,
            _action: Box<dyn FnOnce() + Send>,
        ) -> Option<u64> {
            None
        }

        fn register_host_action(
            &self,
            _id: u32,
            _expected_value: u32,
            _action: Box<dyn FnOnce() + Send>,
        ) -> Option<u64> {
            None
        }

        fn deregister_host_action(&self, _id: u32, _handle: u64) {}

        fn smmu_allocate(&self, _size: usize) -> u64 {
            0
        }

        fn smmu_register_process(&self, _memory: Option<Arc<Mutex<Memory>>>) -> u32 {
            0
        }

        fn smmu_unregister_process(&self, _asid: u32) {}

        fn smmu_free(&self, _d_address: u64, _size: usize) {}

        fn smmu_map(
            &self,
            _d_address: u64,
            _virtual_address: u64,
            _size: usize,
            _asid: u32,
            _track: bool,
        ) {
        }

        fn smmu_track_continuity(&self, _d_address: u64, _size: usize) {}

        fn smmu_track_continuity_registered(
            &self,
            _d_address: u64,
            _virtual_address: u64,
            _size: usize,
            _asid: u32,
        ) {
        }

        fn smmu_unmap(&self, _d_address: u64, _size: usize) {}

        fn smmu_lookup(&self, _d_address: u64) -> usize {
            0
        }

        fn smmu_apply_op_on_host_pointer(
            &self,
            host_ptr: usize,
            _scratch: &mut common::scratch_buffer::ScratchBuffer<u32>,
            operation: &mut dyn FnMut(u64),
        ) -> usize {
            self.applied_host_ptrs.lock().unwrap().push(host_ptr);
            for &alias in &self.aliases {
                operation(alias);
            }
            self.aliases.len()
        }

        fn bind_device_memory_invalidator(&self, _callback: Box<dyn Fn(u64, usize) + Send + Sync>) {
        }

        fn bind_device_memory_flusher(&self, _callback: Box<dyn Fn(u64, usize) + Send + Sync>) {}

        fn host1x_gmmu_map_low(&self, _d_address: u64, _size: usize) -> u32 {
            0
        }

        fn host1x_gmmu_unmap_low(&self, _gpu_address: u32, _size: usize) {}

        fn start_device(&self, _fd: i32, _channel_type: Host1xChannelType, _syncpt: u32) {}

        fn stop_device(&self, _fd: i32, _channel_type: Host1xChannelType) {}

        fn push_entries(&self, _fd: i32, _entries: Vec<u32>) {}
    }

    fn make_rasterizer_cached_memory(
        system: &System,
    ) -> (Box<DeviceMemory>, Box<PageTable>, Memory, u64, u64) {
        let device_memory = Box::new(DeviceMemory::with_size(0x20_000));
        let mut page_table = Box::new(PageTable::new());
        page_table.resize(16, PAGE_BITS);
        let vaddr = 0x4000u64;
        let device_addr = dram_memory_map::BASE + 0x2000;
        let host_ptr = (device_memory.buffer.backing_base_pointer() as usize)
            .wrapping_add((device_addr - dram_memory_map::BASE) as usize);
        page_table.map_pages(
            (vaddr >> PAGE_BITS) as usize,
            1,
            device_addr,
            PageType::RasterizerCachedMemory,
            host_ptr,
        );

        let mut memory = unsafe {
            Memory::new(
                SystemRef::from_ref(system),
                &*device_memory,
                &device_memory.buffer,
            )
        };
        memory.set_current_page_table(&mut *page_table, true);

        (device_memory, page_table, memory, vaddr, device_addr)
    }

    #[test]
    fn rasterizer_download_skips_reads_covered_by_current_core_area() {
        let reads = Arc::new(Mutex::new(Vec::new()));
        let applied_host_ptrs = Arc::new(Mutex::new(Vec::new()));
        let mut system = System::new_for_test();
        system.set_gpu_core(Box::new(FakeGpuCore {
            reads: reads.clone(),
            writes: Arc::new(Mutex::new(Vec::new())),
            download_size: 0x100,
        }));

        let (_device_memory, _page_table, memory, vaddr, device_addr) =
            make_rasterizer_cached_memory(&system);
        system.set_host1x_core(Box::new(FakeHost1xCore {
            applied_host_ptrs,
            aliases: vec![device_addr + 0x20],
        }));

        memory.handle_rasterizer_download(vaddr + 0x20, 4);
        memory.handle_rasterizer_download(vaddr + 0x40, 4);

        let reads = reads.lock().unwrap();
        assert_eq!(&*reads, &[(device_addr + 0x20, 4)]);
    }

    #[test]
    fn rasterizer_download_uses_host1x_pointer_alias_when_available() {
        let reads = Arc::new(Mutex::new(Vec::new()));
        let applied_host_ptrs = Arc::new(Mutex::new(Vec::new()));
        let host1x_device_addr = 0x1234_5020;
        let mut system = System::new_for_test();
        system.set_gpu_core(Box::new(FakeGpuCore {
            reads: reads.clone(),
            writes: Arc::new(Mutex::new(Vec::new())),
            download_size: 0x80,
        }));
        system.set_host1x_core(Box::new(FakeHost1xCore {
            applied_host_ptrs: applied_host_ptrs.clone(),
            aliases: vec![host1x_device_addr],
        }));

        let (_device_memory, _page_table, memory, vaddr, backing_device_addr) =
            make_rasterizer_cached_memory(&system);

        memory.handle_rasterizer_download(vaddr + 0x20, 4);

        assert_eq!(applied_host_ptrs.lock().unwrap().len(), 1);
        let reads = reads.lock().unwrap();
        assert_eq!(&*reads, &[(host1x_device_addr, 4)]);
        assert_ne!(reads[0].0, backing_device_addr + 0x20);
    }

    #[test]
    fn rasterizer_download_does_not_fallback_to_process_physical_address() {
        let reads = Arc::new(Mutex::new(Vec::new()));
        let applied_host_ptrs = Arc::new(Mutex::new(Vec::new()));
        let mut system = System::new_for_test();
        system.set_gpu_core(Box::new(FakeGpuCore {
            reads: reads.clone(),
            writes: Arc::new(Mutex::new(Vec::new())),
            download_size: 0x80,
        }));
        system.set_host1x_core(Box::new(FakeHost1xCore {
            applied_host_ptrs: applied_host_ptrs.clone(),
            aliases: Vec::new(),
        }));

        let (_device_memory, _page_table, memory, vaddr, _device_addr) =
            make_rasterizer_cached_memory(&system);
        memory.handle_rasterizer_download(vaddr + 0x20, 4);

        assert_eq!(applied_host_ptrs.lock().unwrap().len(), 1);
        assert!(reads.lock().unwrap().is_empty());
    }

    #[test]
    fn rasterizer_write_uses_only_host1x_pointer_aliases() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let applied_host_ptrs = Arc::new(Mutex::new(Vec::new()));
        let host1x_device_addr = 0x1234_5020;
        let mut system = System::new_for_test();
        system.set_gpu_core(Box::new(FakeGpuCore {
            reads: Arc::new(Mutex::new(Vec::new())),
            writes: writes.clone(),
            download_size: 0x80,
        }));
        system.set_host1x_core(Box::new(FakeHost1xCore {
            applied_host_ptrs: applied_host_ptrs.clone(),
            aliases: vec![host1x_device_addr],
        }));

        let (_device_memory, _page_table, memory, vaddr, backing_device_addr) =
            make_rasterizer_cached_memory(&system);
        memory.handle_rasterizer_write(vaddr + 0x20, 4);

        assert_eq!(applied_host_ptrs.lock().unwrap().len(), 1);
        assert_eq!(&*writes.lock().unwrap(), &[(host1x_device_addr, 4)]);
        assert_ne!(host1x_device_addr, backing_device_addr + 0x20);
    }

    #[test]
    fn rasterizer_write_does_not_fallback_to_process_physical_address() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let applied_host_ptrs = Arc::new(Mutex::new(Vec::new()));
        let mut system = System::new_for_test();
        system.set_gpu_core(Box::new(FakeGpuCore {
            reads: Arc::new(Mutex::new(Vec::new())),
            writes: writes.clone(),
            download_size: 0x80,
        }));
        system.set_host1x_core(Box::new(FakeHost1xCore {
            applied_host_ptrs: applied_host_ptrs.clone(),
            aliases: Vec::new(),
        }));

        let (_device_memory, _page_table, memory, vaddr, _device_addr) =
            make_rasterizer_cached_memory(&system);
        memory.handle_rasterizer_write(vaddr + 0x20, 4);

        assert_eq!(applied_host_ptrs.lock().unwrap().len(), 1);
        assert!(writes.lock().unwrap().is_empty());
    }

    #[test]
    fn rasterizer_write_rejects_zero_device_alias_after_page_change() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let mut system = System::new_for_test();
        system.set_gpu_core(Box::new(FakeGpuCore {
            reads: Arc::new(Mutex::new(Vec::new())),
            writes: writes.clone(),
            download_size: 0x80,
        }));
        system.set_host1x_core(Box::new(FakeHost1xCore {
            applied_host_ptrs: Arc::new(Mutex::new(Vec::new())),
            aliases: vec![0x1000, 0],
        }));

        let (_device_memory, _page_table, memory, vaddr, _device_addr) =
            make_rasterizer_cached_memory(&system);
        memory.handle_rasterizer_write(vaddr + 0x20, 4);

        assert_eq!(&*writes.lock().unwrap(), &[(0x1000, 4)]);
    }

    #[test]
    fn rasterizer_uncache_without_backing_restores_unmapped_page() {
        let system = System::new_for_test();
        let (_device_memory, mut page_table, memory, vaddr, _device_addr) =
            make_rasterizer_cached_memory(&system);
        let page = (vaddr >> PAGE_BITS) as usize;
        page_table.backing_addr[page] = 0;

        memory.rasterizer_mark_region_cached(vaddr, PAGE_SIZE, false);

        assert_eq!(
            PageInfo::extract_type(page_table.pointers[page].raw_value()),
            PageType::Unmapped
        );
    }

    #[test]
    fn rasterizer_cache_rejects_ranges_outside_address_space() {
        let system = System::new_for_test();
        let (_device_memory, page_table, memory, _vaddr, _device_addr) =
            make_rasterizer_cached_memory(&system);
        let max_addr = 1u64 << page_table.current_address_space_width_in_bits;
        let last_page = ((max_addr >> PAGE_BITS) - 1) as usize;
        page_table.pointers[last_page].store(0x1000, PageType::Memory);

        memory.rasterizer_mark_region_cached(
            max_addr - PAGE_SIZE as u64,
            PAGE_SIZE as u64 + 1,
            true,
        );

        assert_eq!(
            PageInfo::extract_type(page_table.pointers[last_page].raw_value()),
            PageType::Memory
        );
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn separate_heap_fault_handling_is_android_only() {
        let system = System::new_for_test();
        let (_device_memory, _page_table, memory, vaddr, _device_addr) =
            make_rasterizer_cached_memory(&system);

        assert!(!memory.invalidate_separate_heap(vaddr as *const u8));
    }
}

impl Memory {
    /// Invalidate a separate heap fault address.
    ///
    /// Upstream: `Memory::InvalidateSeparateHeap(void* fault_address)` (memory.cpp:1104).
    /// On Android, delegates to `HeapTracker::DeferredMapSeparateHeap(fault_address)`.
    /// On other platforms, returns false.
    pub fn invalidate_separate_heap(&self, fault_address: *const u8) -> bool {
        #[cfg(target_os = "android")]
        {
            if let Some(ref heap_tracker) = self.heap_tracker {
                return heap_tracker.deferred_map_separate_heap(fault_address);
            }
            false
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = fault_address;
            false
        }
    }

    /// Internal: update page table entries for a range of pages.
    ///
    /// Matches upstream `Memory::Impl::MapPages`.
    fn map_pages(
        &self,
        page_table: &mut PageTable,
        base_page: u64,
        num_pages: u64,
        mut target: u64,
        page_type: PageType,
    ) {
        let end = base_page + num_pages;
        debug_assert!(
            (end as usize) <= page_table.pointers.size(),
            "out of range mapping at {:#x}",
            base_page * PAGE_SIZE
        );

        if target == 0 {
            debug_assert!(
                page_type != PageType::Memory,
                "Mapping memory page without a pointer @ {:#x}",
                base_page * PAGE_SIZE
            );

            let mut page = base_page as usize;
            while page < end as usize {
                page_table.pointers[page].store(0usize, page_type);
                page_table.backing_addr[page] = 0u64;
                page_table.blocks[page] = 0u64;
                page += 1;
            }
        } else {
            let orig_base = base_page;
            let mut page = base_page as usize;
            while page < end as usize {
                // Compute host pointer: DeviceMemory base + physical offset - virtual page offset.
                // The result is intended to be used as host_ptr + page*PAGE_SIZE, so the per-iteration
                // delta is a constant — debug builds otherwise hit "subtract with overflow" when the
                // virtual page index is numerically larger than the physical offset (release-mode
                // wraparound is the intended behavior; restore it explicitly).
                let host_ptr = unsafe {
                    let dm = &*self.device_memory;
                    (dm.buffer.backing_base_pointer() as usize)
                        .wrapping_add((target - dram_memory_map::BASE) as usize)
                        .wrapping_sub(page << PAGE_BITS)
                };
                let backing = (target as usize).wrapping_sub(page << PAGE_BITS);

                page_table.pointers[page].store(host_ptr, page_type);
                page_table.backing_addr[page] = backing as u64;
                page_table.blocks[page] = orig_base << (PAGE_BITS as u64);

                page += 1;
                target += PAGE_SIZE;
            }
        }
    }
}
