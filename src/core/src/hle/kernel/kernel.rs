//! Port of Eden src/core/hle/kernel/kernel.h/.cpp
//! Status: COMPLET (stub — runtime dependencies not yet available)
//! Derniere synchro: 2026-03-11
//!
//! KernelCore: the main kernel class, managing all kernel subsystems
//! including schedulers, physical cores, memory layout, slab heaps,
//! shared memory objects, and the global scheduler context.
//!
//! Full implementation requires KProcess, KThread, KScheduler,
//! KMemoryManager, KMemoryLayout, KHardwareTimer, KHandleTable,
//! KResourceLimit, KWorkerTaskManager, and many other subsystems.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use super::super::service::os::event::Event;
use super::super::service::server_manager::ServerManager;
use super::k_memory_manager::KMemoryManager;
use super::k_port::KPort;
use super::k_process::KProcess;
use super::k_scheduler::KScheduler;
use super::k_shared_memory::{KSharedMemory, MemoryPermission};
use super::k_thread::{KThread, KThreadLock, ThreadState};

use super::global_scheduler_context::GlobalSchedulerContext;
use super::init::init_slab_setup::KSlabResourceCounts;
use super::k_auto_object_container::KAutoObjectWithListContainer;
use super::k_hardware_timer::KHardwareTimer;
use super::k_object_name::KObjectNameGlobalData;
use super::k_process::ProcessLock;
use super::k_thread::SuspendType;
use super::k_worker_task_manager::KWorkerTaskManager;
use super::physical_core::PhysicalCore;
use crate::core_timing::CoreTiming;
use crate::device_memory::DeviceMemory;
use crate::hardware_properties;
use crate::hle::result::ResultCode;

// Thread-local host thread ID.
// Upstream: `static inline thread_local u8 host_thread_id = UINT8_MAX` in KernelCore::Impl.
// Core threads get IDs 0..NUM_CPU_CORES-1. Other host threads get IDs >= NUM_CPU_CORES.
// UINT8_MAX (255) means "not yet registered".
std::thread_local! {
    static HOST_THREAD_ID: std::cell::Cell<u32> = const { std::cell::Cell::new(u32::MAX) };
    static IS_PHANTOM_MODE_FOR_SINGLECORE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Upstream `KernelCore::Impl::SetHostThreadId()`.
///
/// The no-inline boundary is part of upstream's fiber contract: guest fibers
/// can resume on a different host thread, so a TLS access must occur after the
/// context switch instead of being folded into code that ran before it.
#[inline(never)]
fn set_host_thread_id(core_id: usize) -> u32 {
    HOST_THREAD_ID.with(|id| {
        assert_eq!(id.get(), u32::MAX, "host thread already registered");
        let this_id = core_id as u32;
        id.set(this_id);
        this_id
    })
}

/// Upstream `KernelCore::Impl::GetHostThreadId()`.
#[inline(never)]
fn get_host_thread_id() -> u32 {
    HOST_THREAD_ID.with(std::cell::Cell::get)
}

/// Upstream `KernelCore::Impl::IsPhantomModeForSingleCore()`.
#[inline(never)]
fn is_phantom_mode_for_single_core() -> bool {
    IS_PHANTOM_MODE_FOR_SINGLECORE.with(std::cell::Cell::get)
}

/// Upstream `KernelCore::Impl::SetIsPhantomModeForSingleCore()`.
#[inline(never)]
fn set_is_phantom_mode_for_single_core(value: bool) {
    IS_PHANTOM_MODE_FOR_SINGLECORE.with(|mode| mode.set(value));
}

// Global kernel pointer for scheduler callbacks.
// Set during kernel initialization, cleared on shutdown.
// The callbacks (fn pointers) need access to GSC + schedulers but can't capture state.
static KERNEL_PTR: std::sync::atomic::AtomicPtr<KernelCore> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

// Raw pointer to the GSC's `m_scheduler_lock` field. Cached at kernel
// initialization so any site can open a `KScopedSchedulerLock` without
// depending on a per-thread scheduler_lock_ptr cache.
//
// Upstream assumes `KScheduler::GetSchedulerLock(kernel)` is always valid
// once the kernel is constructed. Ruzu's previous scheme cached the pointer
// on `KThread::scheduler_lock_ptr`, which is zero until the thread is
// attached — forcing condvar/arbiter entry points to silently no-op the
// scheduler lock. Cache it on the kernel singleton so the "always valid"
// assumption actually holds.
static SCHEDULER_LOCK_PTR: std::sync::atomic::AtomicPtr<
    super::k_scheduler_lock::KAbstractSchedulerLock,
> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

#[cfg(test)]
std::thread_local! {
    static SCOPED_TEST_KERNEL_PTR: std::cell::Cell<*mut KernelCore> =
        const { std::cell::Cell::new(std::ptr::null_mut()) };
    static SCOPED_TEST_KERNEL_ACTIVE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Deferred `KThread::SetActiveCore()` updates that could not be applied
/// immediately because the target thread mutex was still held.
///
/// Upstream applies active-core migration while still under the scheduler lock.
/// Rust cannot safely block on a `KThreadLock` there, so preserve the
/// migration request here and retry it from later scheduler callbacks until it
/// succeeds. Dropping the migration outright leaves a runnable thread tagged to
/// the wrong core and can strand all guest cores in idle.
static PENDING_ACTIVE_CORE_UPDATES: Mutex<Vec<(u64, i32)>> = Mutex::new(Vec::new());

struct TrackedServerManager {
    manager: Arc<Mutex<ServerManager>>,
    stop_requested: Arc<AtomicBool>,
    wakeup_event: Arc<Event>,
    host_threads: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>>,
    name: String,
}

/// Public accessor for KERNEL_PTR — used by GSC to interrupt cores on thread state changes.
pub fn get_kernel_ref() -> Option<&'static KernelCore> {
    #[cfg(test)]
    if let Some(ptr) = SCOPED_TEST_KERNEL_ACTIVE.with(|active| {
        active
            .get()
            .then(|| SCOPED_TEST_KERNEL_PTR.with(std::cell::Cell::get))
    }) {
        return (!ptr.is_null()).then(|| unsafe { &*ptr });
    }

    let ptr = KERNEL_PTR.load(Ordering::Acquire);
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { &*ptr })
    }
}

/// Mutable accessor for KERNEL_PTR — used by code paths (e.g. KPageTableBase
/// allocation paths) that need `&mut KernelCore` to call `memory_manager_mut()`.
/// In production, the kernel pointer is set once at startup and never
/// reassigned, so the returned reference is valid for the duration of the
/// program.
#[allow(clippy::mut_from_ref)]
pub fn get_kernel_mut() -> Option<&'static mut KernelCore> {
    #[cfg(test)]
    if let Some(ptr) = SCOPED_TEST_KERNEL_ACTIVE.with(|active| {
        active
            .get()
            .then(|| SCOPED_TEST_KERNEL_PTR.with(std::cell::Cell::get))
    }) {
        return (!ptr.is_null()).then(|| unsafe { &mut *ptr });
    }

    let ptr = KERNEL_PTR.load(Ordering::Acquire);
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { &mut *ptr })
    }
}

/// Test-only owner that installs a minimal `KernelCore` in a thread-local
/// override without running full kernel initialization.
///
/// Unit tests for page-table allocation paths need upstream-shaped access to
/// `KernelCore::MemoryManager()`, but `KernelCore::initialize()` starts broad
/// scheduler/core state that is inappropriate for small native tests. The
/// thread-local override prevents parallel native tests from replacing each
/// other's process-global kernel pointer.
#[cfg(test)]
pub struct ScopedKernelForTest {
    kernel: Box<KernelCore>,
    previous: *mut KernelCore,
    previous_active: bool,
}

#[cfg(test)]
impl ScopedKernelForTest {
    pub fn new() -> Self {
        let mut kernel = Box::new(KernelCore::new());
        let ptr = &mut *kernel as *mut KernelCore;
        let previous = SCOPED_TEST_KERNEL_PTR.with(|current| current.replace(ptr));
        let previous_active = SCOPED_TEST_KERNEL_ACTIVE.with(|active| active.replace(true));
        Self {
            kernel,
            previous,
            previous_active,
        }
    }

    pub fn memory_manager_mut(&mut self) -> &mut KMemoryManager {
        self.kernel.memory_manager_mut()
    }

    pub fn kernel_mut(&mut self) -> &mut KernelCore {
        &mut self.kernel
    }
}

#[cfg(test)]
impl Drop for ScopedKernelForTest {
    fn drop(&mut self) {
        SCOPED_TEST_KERNEL_PTR.with(|current| current.set(self.previous));
        SCOPED_TEST_KERNEL_ACTIVE.with(|active| active.set(self.previous_active));
    }
}

/// Returns the kernel's global `KAbstractSchedulerLock`, if the kernel has
/// been initialized. Matches upstream `KScheduler::GetSchedulerLock(kernel)`.
///
/// Safe to call from any site (SVC handlers, HLE threads, hardware timer
/// callbacks). In production, returns `None` only before
/// `KernelCore::initialize()` has run. Minimal scoped test kernels also return
/// `None` because they do not initialize scheduler state.
pub fn scheduler_lock() -> Option<&'static super::k_scheduler_lock::KAbstractSchedulerLock> {
    #[cfg(test)]
    if SCOPED_TEST_KERNEL_ACTIVE.with(std::cell::Cell::get) {
        return None;
    }

    let ptr = SCHEDULER_LOCK_PTR.load(Ordering::Acquire);
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { &*ptr })
    }
}

/// SIGUSR1 flag set by the async-signal-safe handler; polled by the preemption
/// thread so the dump runs outside signal context (where Rust's Mutex is unsafe).
static DUMP_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Per-core SVC-entry tracker.  Each entry is packed as (tid:u32, svc:u32).
/// Updated by `svc_dispatch::call` at entry; cleared at exit.  Used by the
/// SIGUSR1 dumper to identify which thread/svc is currently executing on each
/// core.
pub static SVC_IN_PROGRESS: [std::sync::atomic::AtomicU64;
    hardware_properties::NUM_CPU_CORES as usize] = [
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
];

/// Per-core last known guest PC. Updated by `PhysicalCore::handoff_after_svc`
/// at every SVC return (cheapest hook that already touches the thread context
/// where the PC lives). The value lags the JIT's current PC by however many
/// guest instructions have executed since the last SVC — fine for
/// post-freeze diagnostics where the spin has no SVCs at all and we want the
/// PC at which the spin started.
pub static GUEST_PC: [std::sync::atomic::AtomicU64; hardware_properties::NUM_CPU_CORES as usize] = [
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
];

/// Per-core last known guest LR (link register). When a guest thread calls
/// an nnSdk SVC stub like `svc 0x18; bx lr`, LR points back into the caller
/// — usually the game's code that invoked WaitSynchronization or similar.
/// Essential for identifying the actual hot spot in a spin loop where the
/// game only ever calls one kind of SVC (the SVC address drowns out the
/// real work PC).
pub static GUEST_LR: [std::sync::atomic::AtomicU64; hardware_properties::NUM_CPU_CORES as usize] = [
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
];

/// Per-core last known guest SP. Lets the SIGUSR1 dumper walk a few stack
/// frames above the current SVC/halt — the nnSdk SVC stub caller sits right
/// above and its caller (the game-level function driving the loop) is
/// typically within 1-2 frames up.
pub static GUEST_SP: [std::sync::atomic::AtomicU64; hardware_properties::NUM_CPU_CORES as usize] = [
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
];

/// Per-core snapshot of guest general-purpose registers. AArch32 uses the
/// low words; AArch64 uses x0..x28. Updated alongside GUEST_{PC,LR,SP}.
pub static GUEST_REGS: [[std::sync::atomic::AtomicU64; 29];
    hardware_properties::NUM_CPU_CORES as usize] = {
    const NEW: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    const ROW: [std::sync::atomic::AtomicU64; 29] = [NEW; 29];
    [ROW, ROW, ROW, ROW]
};

/// RUZU_PC_SAMPLE=1 — background guest-PC/LR sampling profiler. Every
/// `RUZU_PC_SAMPLE_INTERVAL_US` (default 200µs) it reads each core's last
/// guest (PC,LR) and increments a histogram keyed by LR (the game code that
/// called the nnSdk SVC stub — the real hot loop, vs the stub PC which drowns
/// it out). Dumped on SIGUSR1 via `dump_pc_sample_hist`. Fills the missing
/// "where is the wedge hot loop" tool: the candidate-PC hooks all turned out
/// cold, so we need to discover the hot LR empirically.
static PC_SAMPLE_HIST: std::sync::Mutex<Option<std::collections::HashMap<(u32, u64), u64>>> =
    std::sync::Mutex::new(None);
static PC_SAMPLE_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn maybe_spawn_pc_sampler() {
    if std::env::var_os("RUZU_PC_SAMPLE").is_none() {
        return;
    }
    if PC_SAMPLE_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let interval_us = std::env::var("RUZU_PC_SAMPLE_INTERVAL_US")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(200);
    *PC_SAMPLE_HIST.lock().unwrap() = Some(std::collections::HashMap::new());
    std::thread::Builder::new()
        .name("ruzu-pc-sample".into())
        .spawn(move || {
            let dur = std::time::Duration::from_micros(interval_us.max(20));
            loop {
                {
                    let mut guard = PC_SAMPLE_HIST.lock().unwrap();
                    if let Some(hist) = guard.as_mut() {
                        for core in 0..GUEST_LR.len() {
                            let lr = GUEST_LR[core].load(Ordering::Acquire);
                            let pc = GUEST_PC[core].load(Ordering::Acquire);
                            if lr == 0 && pc == 0 {
                                continue;
                            }
                            // tid running on this core = last SVC's tid (SVC_IN_PROGRESS
                            // is set on every svc-enter and persists until the next svc
                            // on this core ≈ the current thread). Lets us isolate one
                            // thread's hot loop (e.g. tid=75 Main render thread).
                            let tid = (SVC_IN_PROGRESS[core].load(Ordering::Acquire) >> 32) as u32;
                            // Key by (tid, lr<<32 | pc&0xFFFFFFFF) so we keep the SVC
                            // stub PC and the game caller LR distinguished, per thread.
                            let key = (tid, (lr << 32) | (pc & 0xFFFF_FFFF));
                            *hist.entry(key).or_insert(0) += 1;
                        }
                    }
                }
                std::thread::sleep(dur);
            }
        })
        .ok();
    eprintln!(
        "[PC_SAMPLE] sampler started (interval {}µs); dump on SIGUSR1",
        interval_us
    );
}

pub fn dump_pc_sample_hist() {
    let guard = PC_SAMPLE_HIST.lock().unwrap();
    let Some(hist) = guard.as_ref() else {
        return;
    };
    let total: u64 = hist.values().sum();
    let mut v: Vec<((u32, u64), u64)> = hist.iter().map(|(k, c)| (*k, *c)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    eprintln!(
        "[PC_SAMPLE] === top guest (tid,LR,PC) by sample count (total={}) ===",
        total
    );
    for ((tid, lrpc), count) in v.iter().take(40) {
        let lr = lrpc >> 32;
        let pc = lrpc & 0xFFFF_FFFF;
        let pct = if total > 0 {
            (*count as f64) * 100.0 / total as f64
        } else {
            0.0
        };
        eprintln!(
            "[PC_SAMPLE] tid={} lr=0x{:08X} pc=0x{:08X} count={} ({:.1}%)",
            tid, lr, pc, count, pct
        );
    }
}

#[inline]
pub fn mark_svc_enter(core_id: usize, tid: u64, svc: u32) {
    if core_id >= SVC_IN_PROGRESS.len() {
        return;
    }
    let packed = ((tid & 0xFFFF_FFFF) << 32) | (svc as u64 & 0xFFFF_FFFF);
    SVC_IN_PROGRESS[core_id].store(packed, Ordering::Release);
}

/// Record the guest PC observed after an SVC returns. Called from
/// `PhysicalCore::handoff_after_svc` once `jit.get_context()` has populated
/// the ThreadContext for this core.
#[inline]
pub fn record_guest_pc(core_id: usize, pc: u64) {
    if core_id >= GUEST_PC.len() {
        return;
    }
    GUEST_PC[core_id].store(pc, Ordering::Release);
}

/// Record the guest PC + LR observed after an SVC / Halt. LR typically
/// points into the game code that called the nnSdk SVC stub, which is
/// more diagnostic than PC (which sits in the stub itself).
#[inline]
pub fn record_guest_pc_lr(core_id: usize, pc: u64, lr: u64) {
    if core_id >= GUEST_PC.len() {
        return;
    }
    GUEST_PC[core_id].store(pc, Ordering::Release);
    GUEST_LR[core_id].store(lr, Ordering::Release);
}

/// Record the guest PC + LR + SP observed after an SVC / Halt.
#[inline]
pub fn record_guest_pc_lr_sp(core_id: usize, pc: u64, lr: u64, sp: u64) {
    if core_id >= GUEST_PC.len() {
        return;
    }
    GUEST_PC[core_id].store(pc, Ordering::Release);
    GUEST_LR[core_id].store(lr, Ordering::Release);
    GUEST_SP[core_id].store(sp, Ordering::Release);
}

/// Record the guest PC + LR + SP + r0..r11 observed after an SVC / Halt.
/// Used by the SIGUSR1 dumper to see live register values during a spin
/// loop (e.g., the `r8` object pointer and `r10` target count that drive
/// the loop at game PC 0x015DFE30).
#[inline]
pub fn record_guest_full(core_id: usize, pc: u64, lr: u64, sp: u64, regs: &[u64]) {
    if core_id >= GUEST_PC.len() {
        return;
    }
    GUEST_PC[core_id].store(pc, Ordering::Release);
    GUEST_LR[core_id].store(lr, Ordering::Release);
    GUEST_SP[core_id].store(sp, Ordering::Release);
    for (i, slot) in GUEST_REGS[core_id].iter().enumerate() {
        let v = regs.get(i).copied().unwrap_or(0);
        slot.store(v, Ordering::Release);
    }
}

#[inline]
pub fn mark_svc_exit(core_id: usize) {
    if core_id >= SVC_IN_PROGRESS.len() {
        return;
    }
    SVC_IN_PROGRESS[core_id].store(0, Ordering::Release);
}

#[cfg(unix)]
extern "C" fn sigusr1_handler(_signum: libc::c_int) {
    // Only async-signal-safe code here.
    DUMP_REQUESTED.store(true, Ordering::Relaxed);
}

#[cfg(unix)]
extern "C" fn sigurg_handler(_signum: libc::c_int) {
    #[cfg(not(any(
        all(target_os = "linux", target_env = "gnu"),
        target_os = "macos",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    )))]
    unsafe {
        // Keep the signal handler valid on targets without a supported native
        // backtrace API, but emit only the diagnostic marker.
        let marker = b"[SIGURG] native backtrace unavailable on this platform\n";
        let _ = libc::write(2, marker.as_ptr() as *const _, marker.len());
    }

    #[cfg(any(
        all(target_os = "linux", target_env = "gnu"),
        target_os = "macos",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    ))]
    {
        // Use libc's declarations so Linux/macOS get the c_int ABI while the
        // BSDs get libexecinfo's size_t ABI and link requirement.
        const MAX_FRAMES: usize = 32;
        let mut frames: [*mut libc::c_void; MAX_FRAMES] = [std::ptr::null_mut(); MAX_FRAMES];
        unsafe {
            let marker = b"[SIGURG] --- backtrace ---\n";
            let _ = libc::write(2, marker.as_ptr() as *const _, marker.len());

            #[cfg(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos"))]
            {
                let n = libc::backtrace(frames.as_mut_ptr(), MAX_FRAMES as libc::c_int);
                libc::backtrace_symbols_fd(frames.as_ptr(), n, 2);
            }

            #[cfg(any(target_os = "freebsd", target_os = "netbsd", target_os = "openbsd"))]
            {
                let n = libc::backtrace(frames.as_mut_ptr(), MAX_FRAMES);
                let _ = libc::backtrace_symbols_fd(frames.as_ptr(), n, 2);
            }
        }
    }
}

#[cfg(unix)]
fn install_sigusr1_handler() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = sigusr1_handler as usize;
        libc::sigemptyset(&mut sa.sa_mask);
        sa.sa_flags = libc::SA_RESTART;
        let _ = libc::sigaction(libc::SIGUSR1, &sa, std::ptr::null_mut());

        // Also install SIGURG handler for per-thread backtrace dump.
        let mut sa2: libc::sigaction = std::mem::zeroed();
        sa2.sa_sigaction = sigurg_handler as usize;
        libc::sigemptyset(&mut sa2.sa_mask);
        // Don't set SA_RESTART — we want the blocked futex to return EINTR so
        // the signal handler runs. After the handler the thread returns to the
        // same wait.
        sa2.sa_flags = 0;
        let _ = libc::sigaction(libc::SIGURG, &sa2, std::ptr::null_mut());
    }
    eprintln!(
        "[SIGUSR1] handler installed for pid={}: send `kill -USR1 <pid>` to dump thread state",
        std::process::id(),
    );
    maybe_spawn_pc_sampler();
}

#[cfg(not(unix))]
fn install_sigusr1_handler() {
    maybe_spawn_pc_sampler();
}

/// Dump all per-core and per-thread state to stderr.
/// Called from the preemption thread once DUMP_REQUESTED is set.
/// The preemption thread is a normal host thread (not a fiber) so locking is
/// safe.
fn dump_thread_state(kernel: &KernelCore) {
    eprintln!("=========================================");
    eprintln!("[DUMP] === ruzu kernel thread dump ===");
    dump_pc_sample_hist();
    eprintln!("{}", rdynarmic::jit::block_prologue_count_summary_string());
    eprintln!("{}", rdynarmic::jit::block_prologue_top_summary_string());
    crate::hle::kernel::svc_dispatch::dump_svc_ring_profile();
    crate::hle::kernel::svc_dispatch::dump_svc_summary_profile();
    crate::hle::kernel::svc_dispatch::dump_svc_profile();
    eprintln!(
        "{}",
        crate::hle::kernel::k_condition_variable::cv_stats::summary_string()
    );
    crate::hle::kernel::svc::svc_memory_history::dump("sigusr1_thread_dump");
    crate::hle::service::nvdrv::nvdrv_interface::dump_nvdrv_ioctl_profile();
    crate::hle::service::nvdrv::nvdrv_interface::dump_nvdrv_ioctl_history("sigusr1_thread_dump");
    crate::hle::service::nvnflinger::buffer_queue_core::dump_bqp_wait_profile();
    crate::hle::service::nvnflinger::buffer_queue_producer::dump_bqp_slot_profile();
    crate::hle::service::nvnflinger::hardware_composer::dump_hwc_cache_profile();
    crate::hle::service::nvnflinger::hos_binder_driver::dump_binder_txn_profile();
    crate::hle::service::nvnflinger::diagnostics::dump("sigusr1_thread_dump");
    crate::hle::service::vi::conductor::dump_vsync_profile();
    // Who holds each coarse lock right now + the full observed nesting graph
    // (RUZU_LOCK_ORDER=1).
    common::lock_order::dump_owners();
    common::lock_order::dump_graph();
    common::lock_order::dump_wait_for();

    fn parse_u64_auto(raw: &str) -> Option<u64> {
        let raw = raw.trim();
        let hex = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X"));
        match hex {
            Some(hex) => u64::from_str_radix(hex, 16).ok(),
            None => raw.parse().ok(),
        }
    }

    // Per-core running thread + interrupt flag + in-progress SVC + last guest PC.
    let mut pcs_to_dump: Vec<u64> = Vec::new();
    let mut stacks_to_dump: Vec<(usize, u64)> = Vec::new();
    let mut svc21_messages_to_dump: Vec<(usize, u32, u32, u32, u32)> = Vec::new();
    for core_id in 0..hardware_properties::NUM_CPU_CORES as usize {
        if let Some(core) = kernel.physical_core(core_id) {
            let interrupted = core.is_interrupted();
            let packed = SVC_IN_PROGRESS[core_id].load(Ordering::Acquire);
            let svc_tid = (packed >> 32) as u32;
            let svc_num = (packed & 0xFFFF_FFFF) as u32;
            let last_pc = GUEST_PC[core_id].load(Ordering::Acquire);
            let last_lr = GUEST_LR[core_id].load(Ordering::Acquire);
            let last_sp = GUEST_SP[core_id].load(Ordering::Acquire);
            eprintln!(
                "[DUMP] core={} is_interrupted={} in_svc={{tid={}, imm=0x{:X}}} last_guest_pc=0x{:X} last_guest_lr=0x{:X} last_guest_sp=0x{:X}",
                core_id, interrupted, svc_tid, svc_num, last_pc, last_lr, last_sp,
            );
            let regs: [u64; 29] =
                std::array::from_fn(|i| GUEST_REGS[core_id][i].load(Ordering::Acquire));
            eprintln!(
                "[DUMP]        regs x0-x3:   {:016X} {:016X} {:016X} {:016X}",
                regs[0], regs[1], regs[2], regs[3],
            );
            eprintln!(
                "[DUMP]        regs x4-x7:   {:016X} {:016X} {:016X} {:016X}",
                regs[4], regs[5], regs[6], regs[7],
            );
            eprintln!(
                "[DUMP]        regs x8-x11:  {:016X} {:016X} {:016X} {:016X}",
                regs[8], regs[9], regs[10], regs[11],
            );
            eprintln!(
                "[DUMP]        regs x12-x18: {:016X} {:016X} {:016X} {:016X} {:016X} {:016X} {:016X}",
                regs[12], regs[13], regs[14], regs[15], regs[16], regs[17], regs[18],
            );
            eprintln!(
                "[DUMP]        regs x19-x24: {:016X} {:016X} {:016X} {:016X} {:016X} {:016X}",
                regs[19], regs[20], regs[21], regs[22], regs[23], regs[24],
            );
            eprintln!(
                "[DUMP]        regs x25-x28: {:016X} {:016X} {:016X} {:016X}",
                regs[25], regs[26], regs[27], regs[28],
            );
            if svc_num == 0x21 && regs[1] != 0 && regs[2] != 0 {
                svc21_messages_to_dump.push((
                    core_id,
                    svc_tid,
                    regs[0] as u32,
                    regs[1] as u32,
                    regs[2] as u32,
                ));
            }
            if last_sp != 0 {
                stacks_to_dump.push((core_id, last_sp));
            }
            if last_pc != 0 && !pcs_to_dump.contains(&last_pc) {
                pcs_to_dump.push(last_pc);
            }
            // LR usually points AFTER the `BL <stub>` call in the caller
            // (i.e., into game code). Subtract 4 to get the `BL` itself
            // and its surrounding context — where the spin actually
            // happens.
            if last_lr != 0 && last_lr != last_pc {
                let caller_pc = last_lr.saturating_sub(4);
                if !pcs_to_dump.contains(&caller_pc) {
                    pcs_to_dump.push(caller_pc);
                }
            }
        }
    }
    // Allow operator to inject extra addresses via RUZU_DUMP_ADDRS=0xAAA,0xBBB
    // — the SIGUSR1 dumper will print memory around each. Useful for
    // inspecting known init-write PCs or state-handler targets.
    if let Ok(raw) = std::env::var("RUZU_DUMP_ADDRS") {
        for s in raw.split(',') {
            let s = s.trim().trim_start_matches("0x").trim_start_matches("0X");
            if let Ok(addr) = u64::from_str_radix(s, 16) {
                if addr != 0 && !pcs_to_dump.contains(&addr) {
                    pcs_to_dump.push(addr);
                }
            }
        }
    }
    // RUZU_POKE_ADDR=0x40037000:4:1 (addr:size_bytes:value_hex) — write a
    // test value into guest memory on SIGUSR1. Experimental harness for
    let pokes: Vec<(u64, u32)> = std::env::var("RUZU_POKE_ADDR")
        .ok()
        .iter()
        .flat_map(|raw| {
            raw.split(',')
                .map(|s| s.trim().to_string())
                .collect::<Vec<_>>()
        })
        .filter_map(|tok| {
            let parts: Vec<&str> = tok.split(':').collect();
            if parts.len() != 3 {
                return None;
            }
            let addr = parse_u64_auto(parts[0])?;
            let size = parse_u64_auto(parts[1])?;
            if size != 4 {
                return None;
            }
            let value = parse_u64_auto(parts[2])? as u32;
            Some((addr, value))
        })
        .collect();

    // RUZU_DUMP_REGION=0x22C0000:24576 or 0x22C0000:0x6000 dumps a
    // contiguous u32 range as raw hex (no per-PC context windows) — useful
    // for whole-vtable snapshots.
    let region_dumps: Vec<(u64, u64)> = std::env::var("RUZU_DUMP_REGION")
        .ok()
        .iter()
        .flat_map(|raw| {
            raw.split(',')
                .map(|s| s.trim().to_string())
                .collect::<Vec<_>>()
        })
        .filter_map(|tok| {
            let (a, n) = tok.split_once(':')?;
            let addr = parse_u64_auto(a)?;
            let bytes = parse_u64_auto(n)?;
            Some((addr, bytes))
        })
        .collect();
    // Walk the application process (if any) and dump each thread.
    let system = kernel.system();
    if system.is_null() {
        eprintln!("[DUMP] no system ref — skipping thread walk");
        eprintln!("=========================================");
        DUMP_REQUESTED.store(false, Ordering::Relaxed);
        return;
    }
    let Some(process_arc) = system.get().current_process_arc.as_ref().cloned() else {
        eprintln!("[DUMP] no current process");
        eprintln!("=========================================");
        DUMP_REQUESTED.store(false, Ordering::Relaxed);
        return;
    };

    // RUZU_DUMP_MEM=0xADDR:LEN[,0xADDR:LEN...] — dump guest memory words at the
    // wedge instant (e.g. the barrier-object region to read the never-satisfied
    // join predicate). LEN is in BYTES (decimal or 0xhex).
    if let Ok(spec) = std::env::var("RUZU_DUMP_MEM") {
        for part in spec.split(',') {
            let mut it = part.split(':');
            if let (Some(a), Some(l)) = (it.next(), it.next()) {
                if let (Some(addr), Some(len)) = (parse_u64_auto(a), parse_u64_auto(l)) {
                    let nwords = ((len as usize) / 4).min(64);
                    // Raw fastmem reads can fault when the guest address is
                    // unmapped (or when VirtualBuffer is used without 4K
                    // fastmem). Keep SIGUSR1 dumps safe by default and only
                    // use the raw host pointer when explicitly requested.
                    let fb = if std::env::var_os("RUZU_DUMP_MEM_USE_FASTMEM").is_some() {
                        common::fastmem_registry::base()
                    } else {
                        0
                    };
                    if fb != 0 {
                        let mut w = vec![0u32; nwords];
                        for (i, slot) in w.iter_mut().enumerate() {
                            let host = (fb as u64 + addr + (i as u64) * 4) as *const u32;
                            *slot = unsafe { std::ptr::read_volatile(host) };
                        }
                        eprintln!("[DUMP] MEM 0x{:08X} ({} words) = {:08X?}", addr, nwords, w);
                    } else {
                        match process_arc.try_lock() {
                            Ok(process_guard) => {
                                if let Some(memory) =
                                    process_guard.page_table.get_base().m_memory.as_ref()
                                {
                                    match memory.try_lock() {
                                        Ok(m) => {
                                            let mut w = vec![0u32; nwords];
                                            for (i, slot) in w.iter_mut().enumerate() {
                                                *slot = m.read_32(addr + (i as u64) * 4);
                                            }
                                            eprintln!(
                                                "[DUMP] MEM 0x{:08X} ({} words,page_table) = {:08X?}",
                                                addr, nwords, w
                                            );
                                        }
                                        Err(_) => {
                                            eprintln!(
                                                "[DUMP] MEM 0x{:08X} <guest-memory-lock-busy>",
                                                addr
                                            );
                                        }
                                    }
                                } else {
                                    eprintln!("[DUMP] MEM 0x{:08X} <no-memory-source>", addr);
                                }
                            }
                            Err(_) => {
                                eprintln!("[DUMP] MEM 0x{:08X} <process-lock-busy>", addr);
                            }
                        }
                    }
                }
            }
        }
    }

    let pq_fronts = kernel.global_scheduler_context().and_then(|gsc| {
        gsc.try_lock().ok().map(|gsc| {
            [
                (
                    gsc.get_scheduled_front(0),
                    gsc.m_priority_queue.get_suggested_front(0),
                ),
                (
                    gsc.get_scheduled_front(1),
                    gsc.m_priority_queue.get_suggested_front(1),
                ),
                (
                    gsc.get_scheduled_front(2),
                    gsc.m_priority_queue.get_suggested_front(2),
                ),
                (
                    gsc.get_scheduled_front(3),
                    gsc.m_priority_queue.get_suggested_front(3),
                ),
            ]
        })
    });
    eprintln!(
        "[DUMP] scheduler pq_fronts=(scheduled,suggested) {:?}",
        pq_fronts
    );
    for core_id in 0..crate::hardware_properties::NUM_CPU_CORES as usize {
        let Some(scheduler) = kernel.scheduler(core_id) else {
            eprintln!("[DUMP] scheduler core={} missing", core_id);
            continue;
        };
        match scheduler.try_lock() {
            Ok(scheduler) => {
                let switch_thread_info = |thread: &Option<Weak<KThreadLock>>| {
                    thread.as_ref().and_then(Weak::upgrade).map(|thread| {
                        let guard = thread.lock().unwrap();
                        (
                            guard.get_thread_id(),
                            Arc::as_ptr(&thread) as usize,
                            guard.context_guard_owner.load(Ordering::SeqCst)
                                != super::k_thread::CONTEXT_GUARD_UNOWNED,
                            guard.context_guard_owner.load(Ordering::SeqCst),
                            guard.get_current_core(),
                            guard.get_active_core(),
                        )
                    })
                };
                eprintln!(
                    "[DUMP] scheduler core={} current={:?} highest={:?} prev={:?} needs={} interrupt_task={} idle={} switch_cur={:?} switch_target={:?} switch_from={}",
                    core_id,
                    scheduler.get_scheduler_current_thread_id(),
                    scheduler.state.highest_priority_thread_id,
                    scheduler.state.prev_thread_id,
                    scheduler.needs_scheduling(),
                    scheduler.state.interrupt_task_runnable,
                    scheduler.is_idle(),
                    switch_thread_info(&scheduler.switch_cur_thread),
                    switch_thread_info(&scheduler.switch_highest_priority_thread),
                    scheduler.switch_from_schedule,
                );
            }
            Err(_) => {
                eprintln!(
                    "[DUMP] scheduler core={} <scheduler-lock-contended>",
                    core_id
                );
            }
        }
    }

    // Attempt the process lock with multi-second polling so a briefly-held
    // Mutex passes; if still contended after the timeout, it's a real hold.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut acquired = None;
    let mut poll_count = 0u64;
    while std::time::Instant::now() < deadline {
        match process_arc.try_lock() {
            Ok(guard) => {
                acquired = Some(guard);
                break;
            }
            Err(_) => {
                poll_count += 1;
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }
    let thread_entries: Vec<(u64, std::sync::Arc<super::k_thread::KThreadLock>)> = match acquired {
        Some(mut guard) => {
            eprintln!(
                "[DUMP] process threads: {} (process.lock() acquired after {} polls)",
                guard.thread_list.len(),
                poll_count,
            );
            // RUZU_DUMP_JIT_MAP=prefix — write each core's JIT block map
            // (host entry -> guest descriptor) to `prefix.coreN.map` so host
            // profiler samples inside JIT code can be attributed to guest
            // locations offline.
            if let Ok(prefix) = std::env::var("RUZU_DUMP_JIT_MAP") {
                for core in 0..4 {
                    if let Some(arm) = guard.get_arm_interface_mut(core) {
                        let path = format!("{prefix}.core{core}.map");
                        arm.dump_jit_block_map(&path);
                        eprintln!("[DUMP] JIT block map core {core} -> {path}");
                    }
                }
            }
            for (core_id, tid, handle, message_addr, size) in &svc21_messages_to_dump {
                if let Some(object_id) = guard.handle_table.get_object(*handle) {
                    let Some(client_session) = guard.get_client_session_by_object_id(object_id)
                    else {
                        eprintln!(
                            "[DUMP]   SVC21_HANDLE core={} tid={} handle=0x{:08X} object_id=0x{:X} <not-client-session>",
                            core_id, tid, handle, object_id
                        );
                        continue;
                    };
                    let handle_line = match client_session.try_lock() {
                        Ok(client) => {
                            let parent_id = client.get_parent_id();
                            let server_session = parent_id
                                .and_then(|parent_id| guard.get_session_by_object_id(parent_id))
                                .and_then(|session| {
                                    session
                                        .try_lock()
                                        .ok()
                                        .map(|session| session.get_server_session().clone())
                                });
                            let manager_name = server_session
                                .as_ref()
                                .and_then(|server_session| {
                                    server_session.try_lock().ok().and_then(|server_session| {
                                        server_session.get_manager().cloned()
                                    })
                                })
                                .and_then(|manager| {
                                    manager.lock().ok().and_then(|manager| {
                                        manager
                                            .session_handler()
                                            .map(|handler| handler.service_name().to_string())
                                    })
                                })
                                .unwrap_or_else(|| "<no-manager-handler>".to_string());
                            let server_state = parent_id
                                .and_then(|parent_id| guard.get_session_by_object_id(parent_id))
                                .and_then(|session| {
                                    session
                                        .try_lock()
                                        .ok()
                                        .map(|session| session.get_server_session().clone())
                                })
                                .and_then(|server_session| {
                                    server_session.try_lock().ok().map(|server| {
                                        let server_manager_name = server
                                            .get_manager()
                                            .and_then(|manager| {
                                                manager.lock().ok().and_then(|manager| {
                                                    manager.session_handler().map(|handler| {
                                                        handler.service_name().to_string()
                                                    })
                                                })
                                            })
                                            .unwrap_or_else(|| "<no-server-manager>".to_string());
                                        format!(
                                            "server_manager={} request_list={} current_request={}",
                                            server_manager_name,
                                            server.request_list.len(),
                                            server.current_request.is_some()
                                        )
                                    })
                                })
                                .unwrap_or_else(|| {
                                    "<server-session-lock-busy-or-missing>".to_string()
                                });
                            format!(
                                "parent_id={:?} manager={} {}",
                                parent_id, manager_name, server_state
                            )
                        }
                        Err(_) => "<client-session-lock-busy>".to_string(),
                    };
                    eprintln!(
                        "[DUMP]   SVC21_HANDLE core={} tid={} handle=0x{:08X} object_id=0x{:X} {}",
                        core_id, tid, handle, object_id, handle_line
                    );
                } else {
                    eprintln!(
                        "[DUMP]   SVC21_HANDLE core={} tid={} handle=0x{:08X} <invalid-handle>",
                        core_id, tid, handle
                    );
                }

                let word_count = ((*size as usize).min(0x40) / 4).min(16);
                let words: Option<Vec<u32>> =
                    if let Some(memory) = guard.page_table.get_base().m_memory.as_ref() {
                        match memory.try_lock() {
                            Ok(m) => {
                                let mut w = vec![0u32; word_count];
                                for (i, slot) in w.iter_mut().enumerate() {
                                    *slot = m.read_32(*message_addr as u64 + (i as u64) * 4);
                                }
                                Some(w)
                            }
                            Err(_) => None,
                        }
                    } else {
                        let mem = guard.process_memory.read().unwrap();
                        let bytes = mem.read_block(*message_addr as u64, word_count * 4);
                        if bytes.len() >= word_count * 4 {
                            Some(
                                bytes
                                    .chunks_exact(4)
                                    .take(word_count)
                                    .map(|chunk| {
                                        u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])
                                    })
                                    .collect(),
                            )
                        } else {
                            None
                        }
                    };
                match words {
                    Some(words) => eprintln!(
                        "[DUMP]   SVC21_MSG core={} tid={} handle=0x{:08X} msg=0x{:08X} size=0x{:X} words={:08X?}",
                        core_id, tid, handle, message_addr, size, words
                    ),
                    None => eprintln!(
                        "[DUMP]   SVC21_MSG core={} tid={} handle=0x{:08X} msg=0x{:08X} size=0x{:X} <guest-memory-lock-busy>",
                        core_id, tid, handle, message_addr, size
                    ),
                }
            }
            // RUZU_DUMP_HANDLE=0xNNN[,0xMMM...] — resolve each handle in the
            // main process handle table and identify the kernel object kind
            // (thread/session/event/...) plus thread state when applicable.
            if let Ok(spec) = std::env::var("RUZU_DUMP_HANDLE") {
                for s in spec.split(',') {
                    let s = s.trim().trim_start_matches("0x").trim_start_matches("0X");
                    let Ok(handle) = u32::from_str_radix(s, 16) else {
                        continue;
                    };
                    let Some(object_id) = guard.handle_table.get_object(handle) else {
                        eprintln!("[DUMP]   HANDLE 0x{:08X} <not-in-handle-table>", handle);
                        continue;
                    };
                    let mut kinds: Vec<String> = Vec::new();
                    if let Some(thread) = guard.get_thread_by_object_id(object_id) {
                        let state = match thread.try_lock() {
                            Ok(t) => format!("tid={} state={:?}", t.thread_id, t.thread_state),
                            Err(_) => "<thread-lock-busy>".to_string(),
                        };
                        kinds.push(format!("thread({})", state));
                    }
                    if guard.get_session_by_object_id(object_id).is_some() {
                        kinds.push("session".to_string());
                    }
                    if guard.get_client_session_by_object_id(object_id).is_some() {
                        kinds.push("client_session".to_string());
                    }
                    if guard.get_server_session_by_object_id(object_id).is_some() {
                        kinds.push("server_session".to_string());
                    }
                    if let Some(event) = guard.get_event_by_object_id(object_id) {
                        let state = match event.try_lock() {
                            Ok(e) => format!("readable_event_id={}", e.readable_event_id),
                            Err(_) => "<event-lock-busy>".to_string(),
                        };
                        kinds.push(format!("event({})", state));
                    }
                    if let Some(revent) = guard.get_readable_event_by_object_id(object_id) {
                        let state = match revent.try_lock() {
                            Ok(e) => format!("{}", e.is_signaled()),
                            Err(_) => "<revent-lock-busy>".to_string(),
                        };
                        kinds.push(format!("readable_event(signaled={})", state));
                    }
                    if guard.get_shared_memory_by_object_id(object_id).is_some() {
                        kinds.push("shared_memory".to_string());
                    }
                    if guard.get_transfer_memory_by_object_id(object_id).is_some() {
                        kinds.push("transfer_memory".to_string());
                    }
                    eprintln!(
                        "[DUMP]   HANDLE 0x{:08X} object_id=0x{:X} kinds=[{}]",
                        handle,
                        object_id,
                        kinds.join(", ")
                    );
                }
            }
            // Dump 32 bytes (8 ARM32 insns) around each interesting PC so the
            // SIGUSR1 operator can see exactly what's spinning. ARM32 insns
            // are 4 bytes; Thumb insns are 2 or 4. We print raw little-endian
            // u32s starting 8 bytes before the PC.
            for pc in &pcs_to_dump {
                const WORDS_BEFORE: u64 = 32; // 128 bytes before PC (catches BNE-72 target + loop preamble)
                const WORDS_AFTER: u64 = 16; // 64 bytes after PC
                const TOTAL: usize = (WORDS_BEFORE + WORDS_AFTER + 1) as usize;
                let start = pc.saturating_sub(WORDS_BEFORE * 4);
                // Prefer the page_table's guest memory (virtual addresses
                // mapped by the loader). Fall back to process_memory if
                // not wired.
                let words: Option<Vec<u32>> =
                    if let Some(memory) = guard.page_table.get_base().m_memory.as_ref() {
                        // try_lock, never block: under a wedge the guest memory
                        // Mutex may be held forever by a stuck thread. Blocking
                        // here would hang the SIGUSR1 dumper before it reaches
                        // the per-thread wait-state walk (the CV/owner data).
                        match memory.try_lock() {
                            Ok(m) => {
                                let mut w = vec![0u32; TOTAL];
                                for (i, slot) in w.iter_mut().enumerate() {
                                    *slot = m.read_32(start + (i as u64) * 4);
                                }
                                Some(w)
                            }
                            Err(_) => None,
                        }
                    } else {
                        let mem = guard.process_memory.read().unwrap();
                        let len = (TOTAL as u64) * 4;
                        if !mem.is_valid_range(start, len as usize) {
                            None
                        } else {
                            let mut w = vec![0u32; TOTAL];
                            for (i, slot) in w.iter_mut().enumerate() {
                                *slot = mem.read_32(start + (i as u64) * 4);
                            }
                            Some(w)
                        }
                    };
                match words {
                    Some(w) => {
                        eprint!("[DUMP]   pc=0x{:X} insns:", pc);
                        for (i, insn) in w.iter().enumerate() {
                            if i as u64 == WORDS_BEFORE {
                                eprint!(" [{:08X}]", insn);
                            } else {
                                eprint!(" {:08X}", insn);
                            }
                        }
                        eprintln!();
                    }
                    None => eprintln!("[DUMP]   pc=0x{:X}: memory range not mapped", pc),
                }
            }
            // Per-core guest stack dump — 128 words (512 bytes) starting at SP.
            // The nnSdk SVC wrapper (e.g., `0x1D314F4`) is only one frame up
            // from the SVC stub; its caller's LR sits a few words deeper in
            // the stack. Walking these words reveals the game-level function
            for (core_id, sp) in &stacks_to_dump {
                const STACK_WORDS: usize = 128;
                let mut stack_words: Vec<u32> = Vec::with_capacity(STACK_WORDS);
                if let Some(memory) = guard.page_table.get_base().m_memory.as_ref() {
                    // try_lock, never block (see pcs_to_dump rationale above).
                    match memory.try_lock() {
                        Ok(m) => {
                            for i in 0..STACK_WORDS {
                                let addr = sp + (i as u64) * 4;
                                if !m.is_valid_virtual_address_range(addr, 4) {
                                    break;
                                }
                                stack_words.push(m.read_32(addr));
                            }
                        }
                        Err(_) => {
                            eprintln!(
                                "[DUMP]   core={} sp=0x{:X}: <guest-memory-lock-busy>",
                                core_id, sp
                            );
                            continue;
                        }
                    }
                } else {
                    let mem = guard.process_memory.read().unwrap();
                    for i in 0..STACK_WORDS {
                        let addr = sp + (i as u64) * 4;
                        if !mem.is_valid_range(addr, 4) {
                            break;
                        }
                        stack_words.push(mem.read_32(addr));
                    }
                }
                if stack_words.is_empty() {
                    eprintln!("[DUMP]   core={} sp=0x{:X}: stack not mapped", core_id, sp);
                    continue;
                }
                eprint!("[DUMP]   core={} stack@0x{:X}:", core_id, sp);
                for (i, w) in stack_words.iter().enumerate() {
                    if i > 0 && (i & 7) == 0 {
                        eprint!("\n[DUMP]        +0x{:02X}:", i * 4);
                    }
                    eprint!(" {:08X}", w);
                }
                eprintln!();
            }
            // RUZU_DUMP_REGION raw u32 dumps — 8 words per line.
            for (start, bytes) in &region_dumps {
                let nwords = (bytes / 4) as usize;
                eprintln!(
                    "[DUMP] === REGION 0x{:X}..0x{:X} ({} u32 words) ===",
                    start,
                    start + bytes,
                    nwords
                );
                let mut all_words: Vec<u32> = Vec::with_capacity(nwords);
                if let Some(memory) = guard.page_table.get_base().m_memory.as_ref() {
                    let m = memory.lock().unwrap();
                    for i in 0..nwords {
                        all_words.push(m.read_32(start + (i as u64) * 4));
                    }
                } else {
                    let mem = guard.process_memory.read().unwrap();
                    if !mem.is_valid_range(*start, *bytes as usize) {
                        eprintln!("[DUMP]   region not mapped");
                        continue;
                    }
                    for i in 0..nwords {
                        all_words.push(mem.read_32(start + (i as u64) * 4));
                    }
                }
                for chunk_idx in 0..nwords.div_ceil(8) {
                    let off = chunk_idx * 8;
                    eprint!("[DUMP] 0x{:08X}:", start + (off as u64) * 4);
                    for w in &all_words[off..(off + 8).min(nwords)] {
                        eprint!(" {:08X}", w);
                    }
                    eprintln!();
                }
            }
            // RUZU_POKE_ADDR — experimental write into guest memory to test
            // whether a missing HLE signal is the root cause of a spin.
            for (addr, value) in &pokes {
                if let Some(memory) = guard.page_table.get_base().m_memory.as_ref() {
                    let m = memory.lock().unwrap();
                    let old = m.read_32(*addr);
                    m.write_32(*addr, *value);
                    let readback = m.read_32(*addr);
                    eprintln!(
                        "[POKE] addr=0x{:X} old=0x{:08X} wrote=0x{:08X} readback=0x{:08X}",
                        addr, old, value, readback
                    );
                } else {
                    eprintln!("[POKE] addr=0x{:X}: no page_table memory — skipping", addr);
                }
            }
            guard
                .thread_list
                .iter()
                .filter_map(|tid| {
                    guard
                        .get_thread_by_thread_id(*tid)
                        .map(|thread| (*tid, thread))
                })
                .collect()
        }
        None => {
            eprintln!(
                "[DUMP] process.lock() is CONTENDED after 3s poll ({} tries) — \
                 Mutex held continuously by some thread.",
                poll_count,
            );
            // Send SIGURG to every CPUCore_* and HLE:* host thread. Each will
            // invoke the SIGURG handler (libc::backtrace -> fd 2) which prints
            // its Rust stack. glibc backtrace is async-signal-safe.
            eprintln!("[DUMP] Triggering SIGURG backtrace on every worker thread...");
            #[cfg(target_os = "linux")]
            if let Ok(entries) = std::fs::read_dir("/proc/self/task") {
                for ent in entries.flatten() {
                    let Ok(tid_str) = ent.file_name().into_string() else {
                        continue;
                    };
                    let comm = std::fs::read_to_string(format!("/proc/self/task/{}/comm", tid_str))
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    if comm.starts_with("CPUCore_")
                        || comm.starts_with("HLE:")
                        || comm == "CoreTiming"
                    {
                        let tid: i32 = tid_str.parse().unwrap_or(-1);
                        if tid > 0 {
                            eprintln!("[DUMP] SIGURG -> host_tid={} comm={}", tid, comm);
                            unsafe {
                                libc::syscall(
                                    libc::SYS_tgkill,
                                    std::process::id() as i32,
                                    tid,
                                    libc::SIGURG,
                                );
                            }
                            // Sleep briefly so each thread's output doesn't
                            // interleave chaotically.
                            std::thread::sleep(std::time::Duration::from_millis(30));
                        }
                    }
                }
            }
            eprintln!("[DUMP] Host threads currently blocked in futex:");
            #[cfg(target_os = "linux")]
            if let Ok(entries) = std::fs::read_dir("/proc/self/task") {
                for ent in entries.flatten() {
                    let Ok(tid_str) = ent.file_name().into_string() else {
                        continue;
                    };
                    let comm = std::fs::read_to_string(format!("/proc/self/task/{}/comm", tid_str))
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    // Only interesting threads.
                    if !(comm.starts_with("CPUCore_")
                        || comm == "CoreTiming"
                        || comm.starts_with("DSP_")
                        || comm.starts_with("HLE:")
                        || comm == "ruzu-cmd")
                    {
                        continue;
                    }
                    let wchan =
                        std::fs::read_to_string(format!("/proc/self/task/{}/wchan", tid_str))
                            .unwrap_or_default()
                            .trim()
                            .to_string();
                    let state =
                        std::fs::read_to_string(format!("/proc/self/task/{}/stat", tid_str))
                            .ok()
                            .and_then(|s| s.split_whitespace().nth(2).map(|x| x.to_string()))
                            .unwrap_or_default();
                    let stack =
                        std::fs::read_to_string(format!("/proc/self/task/{}/stack", tid_str))
                            .unwrap_or_else(|_| "<stack unavailable>".into());
                    eprintln!(
                        "[DUMP]   host_tid={} comm={} state={} wchan={}",
                        tid_str, comm, state, wchan
                    );
                    for line in stack.lines().take(6) {
                        eprintln!("[DUMP]     {}", line.trim());
                    }
                }
            }
            eprintln!("=========================================");
            DUMP_REQUESTED.store(false, Ordering::Relaxed);
            return;
        }
    };

    // Walk each thread. Thread Arcs were captured while holding the process lock
    // above, so the dump does not reacquire process.lock() per-thread and hide
    // useful state behind false `<process-lock-contended>` rows.
    for (tid, thread_arc) in thread_entries {
        let try_result = thread_arc.try_lock();
        if let Ok(t) = try_result {
            let state = t.get_state();
            let thread_type = t.thread_type;
            let priority = t.get_priority();
            let current_core = t.get_current_core();
            let active_core = t.get_active_core();
            let affinity = t.physical_affinity_mask.get_affinity_mask();
            let wait_reason = t.get_wait_reason_for_debugging();
            let addr_key = t.get_address_key();
            let addr_key_val = t.get_address_key_value();
            let cv_key = t.get_condition_variable_key();
            let waiting_lock = t.get_waiting_lock_info().is_some();
            let lock_owner_tid = t.get_lock_owner_thread_id();
            let pc = t.thread_context.pc as u32;
            let lr = t.thread_context.lr as u32;
            let sp = t.thread_context.sp as u32;
            eprintln!(
                "[DUMP]   tid={} type={:?} state={:?} prio={} core={} active_core={} wait={:?} \
                 affinity=0x{:X} \
                 addr_key=0x{:X} addr_key_val=0x{:X} cv_key=0x{:X} \
                 waiting_lock={} lock_owner_tid={:?} pc=0x{:08X} lr=0x{:08X} sp=0x{:08X}",
                tid,
                thread_type,
                state,
                priority,
                current_core,
                active_core,
                wait_reason,
                affinity,
                addr_key.get(),
                addr_key_val,
                cv_key,
                waiting_lock,
                lock_owner_tid,
                pc,
                lr,
                sp,
            );
            // Context-guard attribution: a leaked (still-locked) guard on a
            // RUNNABLE thread wedges the switch fiber's try_lock spin.
            {
                let ctx_owner = t.context_guard_owner.load(Ordering::SeqCst);
                let ctx_locked = ctx_owner != super::k_thread::CONTEXT_GUARD_UNOWNED;
                let trace = t.context_guard_trace.lock();
                if ctx_locked
                    || ctx_owner != super::k_thread::CONTEXT_GUARD_UNOWNED
                    || trace.last_lock.is_some()
                {
                    eprintln!(
                        "[DUMP]        tid={} ctx_guard locked={} owner={} last_lock={:?} last_unlock={:?}",
                        tid,
                        ctx_locked,
                        ctx_owner,
                        trace.last_lock,
                        trace.last_unlock,
                    );
                }
            }
            // AArch32 GPRs r0-r12 (the join loop's subtask-array base is r7).
            let r = &t.thread_context.r;
            eprintln!(
                "[DUMP]        tid={} r0-r12: {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X}",
                tid,
                r[0] as u32, r[1] as u32, r[2] as u32, r[3] as u32,
                r[4] as u32, r[5] as u32, r[6] as u32, r[7] as u32,
                r[8] as u32, r[9] as u32, r[10] as u32, r[11] as u32, r[12] as u32,
            );
        } else {
            eprintln!(
                "[DUMP]   tid={} <thread-lock-contended — held by someone>",
                tid,
            );
        }
    }
    // RUZU_DUMP_HOST_BT=1 — send SIGURG to every host worker thread so each
    // prints its native Rust backtrace (libc::backtrace -> fd 2). Unlike the
    // process-lock-contended branch this runs unconditionally, so we can see
    // the Rust stack of a host thread blocked INSIDE a synchronous HLE IPC
    // handler (the Sig-A SendSyncRequest wedge: guest tid RUNNABLE-but-frozen
    // in SVC 0x21 while its host fiber is stuck in the handler).
    if std::env::var_os("RUZU_DUMP_HOST_BT").is_some() {
        eprintln!("[DUMP] RUZU_DUMP_HOST_BT: SIGURG backtrace on every worker thread...");
        #[cfg(target_os = "linux")]
        if let Ok(entries) = std::fs::read_dir("/proc/self/task") {
            for ent in entries.flatten() {
                let Ok(tid_str) = ent.file_name().into_string() else {
                    continue;
                };
                let comm = std::fs::read_to_string(format!("/proc/self/task/{}/comm", tid_str))
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if comm.starts_with("CPUCore_")
                    || comm.starts_with("HLE:")
                    || comm.starts_with("DSP_")
                    || comm == "CoreTiming"
                    || comm == "GPU"
                    || comm == "VSyncThread"
                    || comm.starts_with("AudioRender")
                {
                    let host_tid: i32 = tid_str.parse().unwrap_or(-1);
                    if host_tid > 0 {
                        eprintln!("[DUMP] SIGURG -> host_tid={} comm={}", host_tid, comm);
                        unsafe {
                            libc::syscall(
                                libc::SYS_tgkill,
                                std::process::id() as i32,
                                host_tid,
                                libc::SIGURG,
                            );
                        }
                        std::thread::sleep(std::time::Duration::from_millis(40));
                    }
                }
            }
        }
    }
    eprintln!("=========================================");
    DUMP_REQUESTED.store(false, Ordering::Relaxed);
}

/// Real scheduler callbacks that access the kernel via KERNEL_PTR.
/// Wired to the scheduler lock during kernel initialization.
static SCHEDULER_CALLBACKS: super::k_scheduler_lock::SchedulerCallbacks =
    super::k_scheduler_lock::SchedulerCallbacks {
        disable_scheduling: real_disable_scheduling,
        enable_scheduling: real_enable_scheduling,
        update_highest_priority_threads: real_update_highest_priority_threads,
    };

fn real_disable_scheduling() {
    apply_pending_active_core_updates();

    with_current_thread_fast_mut(|thread| {
        debug_assert!(thread.get_disable_dispatch_count() >= 0);
        thread.disable_dispatch();
    });
}

fn real_enable_scheduling(cores_needing_scheduling: u64) {
    apply_pending_active_core_updates();

    let kernel_ptr = KERNEL_PTR.load(Ordering::Acquire);
    if kernel_ptr.is_null() {
        return;
    }

    let kernel = unsafe { &*kernel_ptr };
    let current_thread = if get_current_thread_id_fast().is_some() {
        get_current_emu_thread()
    } else {
        get_current_emu_thread()
    };
    if current_thread.is_none() {
        KScheduler::reschedule_cores(cores_needing_scheduling);
        return;
    }
    let current_scheduler = kernel.current_scheduler();

    let current_tid = get_current_thread_id_fast();
    let disable_dispatch =
        with_current_thread_fast_mut(|t| t.get_disable_dispatch_count()).unwrap_or(-1);
    let state = with_current_thread_fast_mut(|t| t.get_state()).unwrap_or(ThreadState::INITIALIZED);
    log::trace!(
        "real_enable_scheduling: tid={:?} disable_dispatch={} state={:?} cores=0x{:x} has_scheduler={}",
        current_tid,
        disable_dispatch,
        state,
        cores_needing_scheduling,
        current_scheduler.is_some()
    );

    KScheduler::enable_scheduling_with_scheduler(
        cores_needing_scheduling,
        current_scheduler,
        kernel.is_phantom_mode_for_single_core(),
    );
}

fn real_update_highest_priority_threads() -> u64 {
    use super::k_scheduler::KScheduler;

    let kernel_ptr = KERNEL_PTR.load(Ordering::Acquire);
    if kernel_ptr.is_null() {
        return 0;
    }
    let kernel = unsafe { &*kernel_ptr };

    let gsc_arc = match kernel.global_scheduler_context() {
        Some(gsc) => gsc.clone(),
        None => return 0,
    };

    // Collect scheduler arcs before locking GSC (lock order: GSC before schedulers).
    let sched_arcs: Vec<_> = (0..hardware_properties::NUM_CPU_CORES as usize)
        .filter_map(|i| kernel.scheduler(i).cloned())
        .collect();

    let migrations;
    let cores_needing_scheduling;
    {
        let mut gsc = gsc_arc.lock().unwrap();

        if !gsc.m_scheduler_update_needed.load(Ordering::Relaxed) {
            return 0;
        }

        let mut sched_guards: Vec<_> = sched_arcs.iter().map(|s| s.lock().unwrap()).collect();

        // Delegate to full implementation with idle core migration.
        let result = KScheduler::update_highest_priority_threads_impl(&mut sched_guards, &mut gsc);
        cores_needing_scheduling = result.0;
        migrations = result.1;
        // GSC lock released here.
    }

    if !migrations.is_empty() {
        enqueue_pending_active_core_updates(migrations);
    }

    apply_pending_active_core_updates();

    cores_needing_scheduling
}

fn enqueue_pending_active_core_updates(migrations: Vec<(u64, i32)>) {
    let mut pending = PENDING_ACTIVE_CORE_UPDATES.lock().unwrap();
    for (thread_id, new_core) in migrations {
        if let Some(existing) = pending
            .iter_mut()
            .find(|(pending_tid, _)| *pending_tid == thread_id)
        {
            existing.1 = new_core;
        } else {
            pending.push((thread_id, new_core));
        }
    }
}

fn apply_pending_active_core_updates() {
    let kernel_ptr = KERNEL_PTR.load(Ordering::Acquire);
    if kernel_ptr.is_null() {
        return;
    }
    let kernel = unsafe { &*kernel_ptr };
    let Some(gsc_arc) = kernel.global_scheduler_context() else {
        return;
    };

    let pending_work = {
        let mut pending = PENDING_ACTIVE_CORE_UPDATES.lock().unwrap();
        if pending.is_empty() {
            return;
        }
        std::mem::take(&mut *pending)
    };

    let gsc = gsc_arc.lock().unwrap();
    let mut still_pending = Vec::new();

    for (thread_id, new_core) in pending_work {
        let Some(thread) = gsc.get_thread_by_thread_id(thread_id) else {
            continue;
        };

        let try_lock_result = thread.try_lock();
        if let Ok(mut thread_guard) = try_lock_result {
            thread_guard.set_active_core(new_core);
        } else {
            still_pending.push((thread_id, new_core));
        }
    }

    drop(gsc);

    if !still_pending.is_empty() {
        let mut pending = PENDING_ACTIVE_CORE_UPDATES.lock().unwrap();
        for (thread_id, new_core) in still_pending {
            if let Some(existing) = pending
                .iter_mut()
                .find(|(pending_tid, _)| *pending_tid == thread_id)
            {
                existing.1 = new_core;
            } else {
                pending.push((thread_id, new_core));
            }
        }
    }
}

// Thread-local current thread pointer.
// Upstream: `static inline thread_local KThread* current_thread{nullptr}` in KernelCore::Impl.
// Each physical core host thread (and any other host thread) stores its own current KThread.
std::thread_local! {
    static CURRENT_THREAD: RefCell<Option<Weak<KThreadLock>>> = RefCell::new(None);
    static CURRENT_THREAD_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static CURRENT_THREAD_PTR: std::cell::Cell<*mut KThread> = const { std::cell::Cell::new(std::ptr::null_mut()) };
    static HOST_DUMMY_THREAD: RefCell<Option<Arc<KThreadLock>>> = const { RefCell::new(None) };
}

#[inline(never)]
fn get_or_create_host_dummy_thread(kernel: &KernelCore) -> Arc<KThreadLock> {
    HOST_DUMMY_THREAD.with(|cell| {
        if let Some(thread) = cell.borrow().as_ref() {
            return Arc::clone(thread);
        }

        let thread = Arc::new(KThreadLock::new(KThread::new()));
        {
            let thread_id = kernel.create_new_thread_id();
            let object_id = kernel.create_new_object_id() as u64;
            let mut guard = thread.lock().unwrap();
            let rc = guard.initialize_dummy_thread(None, thread_id, object_id);
            assert_eq!(rc, crate::hle::result::RESULT_SUCCESS.get_inner_value());
            guard.bind_self_reference(&thread);
        }

        *cell.borrow_mut() = Some(Arc::clone(&thread));
        thread
    })
}

/// Get the current emulation thread for the calling host thread.
/// Upstream: `KernelCore::Impl::GetCurrentEmuThread()`.
#[inline(never)]
pub fn get_current_emu_thread() -> Option<Arc<KThreadLock>> {
    let current = CURRENT_THREAD.with(|cell| cell.borrow().as_ref().and_then(Weak::upgrade));
    if current.is_some() {
        return current;
    }

    let kernel_ptr = KERNEL_PTR.load(Ordering::Acquire);
    if kernel_ptr.is_null() {
        return None;
    }

    let kernel = unsafe { &*kernel_ptr };
    let dummy = get_or_create_host_dummy_thread(kernel);
    set_current_emu_thread(Some(&dummy));
    Some(dummy)
}

/// Set the current emulation thread for the calling host thread.
/// Upstream: `KernelCore::Impl::SetCurrentEmuThread(KThread*)`.
#[inline(never)]
pub fn set_current_emu_thread(thread: Option<&Arc<KThreadLock>>) {
    CURRENT_THREAD.with(|cell| {
        *cell.borrow_mut() = thread.map(Arc::downgrade);
    });
    CURRENT_THREAD_ID.with(|cell| {
        cell.set(
            thread
                .map(|thread| thread.lock().unwrap().get_thread_id())
                .unwrap_or(0),
        );
    });
    CURRENT_THREAD_PTR.with(|cell| {
        let ptr = thread
            .map(|thread| {
                let mut guard = thread.lock().unwrap();
                (&mut *guard) as *mut KThread
            })
            .unwrap_or(std::ptr::null_mut());
        cell.set(ptr);
    });
}

/// Ensure the current host thread has a `CURRENT_THREAD` populated —
/// either a real emu thread set by `set_current_emu_thread`, or the
/// lazily-created per-host-thread dummy KThread. After this returns,
/// `CURRENT_THREAD_ID` is non-zero and `CURRENT_THREAD_PTR` is non-null,
/// matching upstream's invariant that `GetCurrentThreadPointer(kernel)`
/// is total.
///
/// Returns `false` only when the kernel itself has not been initialized
/// (`KERNEL_PTR` is null, e.g., in unit tests with no kernel).
fn ensure_current_thread_populated() -> bool {
    if CURRENT_THREAD_ID.with(|cell| cell.get()) != 0 {
        return true;
    }
    // get_current_emu_thread lazily creates the dummy and calls
    // set_current_emu_thread, which populates all three thread-local
    // fields (CURRENT_THREAD / _ID / _PTR).
    get_current_emu_thread().is_some()
}

#[inline(never)]
pub fn get_current_thread_id_fast() -> Option<u64> {
    // Upstream totality: GetCurrentThreadPointer is always valid during
    // CPU execution. Populate lazily via the dummy-thread fallback if
    // the thread-local hasn't been set yet on this host thread.
    let thread_id = CURRENT_THREAD_ID.with(|cell| cell.get());
    if thread_id != 0 {
        return Some(thread_id);
    }
    if !ensure_current_thread_populated() {
        return None;
    }
    let id = CURRENT_THREAD_ID.with(|cell| cell.get());
    if id == 0 {
        None
    } else {
        Some(id)
    }
}

#[inline(never)]
pub fn with_current_thread_fast_mut<R>(f: impl FnOnce(&mut KThread) -> R) -> Option<R> {
    // Same totality semantics as get_current_thread_id_fast.
    let ptr = CURRENT_THREAD_PTR.with(|cell| cell.get());
    if !ptr.is_null() {
        return Some(unsafe { f(&mut *ptr) });
    }
    if !ensure_current_thread_populated() {
        return None;
    }
    CURRENT_THREAD_PTR.with(|cell| {
        let ptr = cell.get();
        if ptr.is_null() {
            None
        } else {
            Some(unsafe { f(&mut *ptr) })
        }
    })
}

/// Get the current thread pointer for the calling host thread.
/// Upstream: `GetCurrentThreadPointer(kernel)`.
/// Returns None if no thread is set.
pub fn get_current_thread_pointer() -> Option<Arc<KThreadLock>> {
    get_current_emu_thread()
}

/// Get the current hardware timer tick for the active kernel instance.
/// Returns `None` when no kernel or hardware timer is initialized.
pub fn get_current_hardware_tick() -> Option<i64> {
    let kernel_ptr = KERNEL_PTR.load(Ordering::Acquire);
    if kernel_ptr.is_null() {
        return None;
    }

    let kernel = unsafe { &*kernel_ptr };
    if let Some(core_timing) = kernel.core_timing() {
        return Some(core_timing.get_global_time_ns().as_nanos() as i64);
    }

    kernel.hardware_timer().map(|timer| timer.get_tick())
}

/// Get the global hardware timer Arc for the active kernel instance.
pub fn get_hardware_timer_arc() -> Option<Arc<KHardwareTimer>> {
    let kernel_ptr = KERNEL_PTR.load(Ordering::Acquire);
    if kernel_ptr.is_null() {
        return None;
    }

    let kernel = unsafe { &*kernel_ptr };
    kernel.hardware_timer().cloned()
}

/// Constants from the upstream KernelCore::Impl.
pub const APPLICATION_MEMORY_BLOCK_SLAB_HEAP_SIZE: usize = 20_000;
pub const SYSTEM_MEMORY_BLOCK_SLAB_HEAP_SIZE: usize = 10_000;
pub const BLOCK_INFO_SLAB_HEAP_SIZE: usize = 4000;
const RESERVED_DYNAMIC_PAGE_COUNT: usize = 64;

/// Represents a single instance of the kernel.
///
/// Maps to upstream KernelCore and its inner Impl struct.
pub struct KernelCore {
    // -- Initialization state --
    is_multicore: bool,
    is_shutting_down: AtomicBool,
    exception_exited: bool,

    // -- ID counters --
    next_object_id: AtomicU32,
    next_kernel_process_id: AtomicU64,
    next_user_process_id: AtomicU64,
    next_thread_id: AtomicU64,

    // -- Subsystems --
    hardware_timer: Option<Arc<KHardwareTimer>>,
    global_object_list_container: Option<KAutoObjectWithListContainer>,
    global_scheduler_context: Option<Arc<Mutex<GlobalSchedulerContext>>>,
    object_name_global_data: Option<KObjectNameGlobalData>,

    // -- Physical cores and schedulers --
    /// Per-core KScheduler instances.
    /// Upstream: `std::array<std::unique_ptr<Kernel::KScheduler>, NUM_CPU_CORES> schedulers`.
    schedulers: Vec<Arc<Mutex<KScheduler>>>,
    /// Per-core PhysicalCore instances.
    /// Upstream: `std::array<std::unique_ptr<Kernel::PhysicalCore>, NUM_CPU_CORES> cores`.
    cores: Vec<Arc<PhysicalCore>>,

    /// Per-core shutdown threads.
    /// Upstream: created in `InitializeShutdownThreads()` via `KThread::InitializeHighPriorityThread`.
    shutdown_threads: Vec<Arc<KThreadLock>>,
    /// Per-core main threads.
    /// Upstream: created in `InitializePhysicalCores()` via `KThread::InitializeMainThread`.
    main_threads: Vec<Arc<KThreadLock>>,
    /// Per-core idle threads.
    /// Upstream: created in `InitializePhysicalCores()` via `KThread::InitializeIdleThread`.
    idle_threads: Vec<Arc<KThreadLock>>,

    /// The application's main thread (created by KProcess::run).
    /// Used to set as the current thread when entering guest dispatch.
    application_thread: Option<Arc<KThreadLock>>,

    // -- Slab resource counts --
    slab_resource_counts: KSlabResourceCounts,

    // -- Process tracking --
    /// Kernel process list.
    ///
    /// Upstream: `KernelCore::Impl::process_list`, populated by `KProcess::Register`.
    /// Rust stores the stable `Arc<ProcessLock>` owners instead of raw `KProcess*`.
    process_list: Mutex<Vec<Arc<ProcessLock>>>,
    process_list_lock: Mutex<()>,

    /// Processes removed from the upstream-visible process list by
    /// `terminate_all_processes`, but retained until cooperative CPU fibers
    /// have stopped and Rust can safely release their thread owners.
    terminating_processes: Mutex<Vec<Arc<ProcessLock>>>,

    // -- Registered objects for leak tracking --
    registered_objects: Mutex<Vec<u64>>,
    registered_in_use_objects: Mutex<Vec<u64>>,

    // -- Host thread management --
    next_host_thread_id: AtomicU32,
    /// In single-core mode, the host thread ID of the single core thread.
    /// Upstream: `u32 single_core_thread_id{}` in Impl.
    single_core_thread_id: AtomicU32,

    // -- Memory management --
    /// Physical memory manager. Upstream: `Impl::memory_manager`.
    memory_manager: KMemoryManager,

    /// Kernel-owned shared memory exposed by the platform font services.
    /// Upstream: `KernelCore::Impl::font_shared_mem`.
    font_shared_mem: Option<(u64, Arc<KSharedMemory>)>,

    /// Kernel-owned shared memory exposed by the IRS service.
    /// Upstream: `KernelCore::Impl::irs_shared_mem`.
    irs_shared_mem: Option<(u64, Arc<KSharedMemory>)>,

    /// Kernel-wide resource limit. Upstream:
    /// `KernelCore::Impl::system_resource_limit` set by
    /// `InitializeSystemResourceLimit` at boot. Holds PhysicalMemoryMax,
    /// ThreadCountMax, EventCountMax, TransferMemoryCountMax, SessionCountMax.
    /// Used by services like KSystemControl::GetInsecureMemoryResourceLimit.
    system_resource_limit: Option<Arc<super::k_resource_limit::KResourceLimit>>,

    /// Compatibility alias to the application resource's memory-block
    /// manager, used by isolated legacy/test page-table construction.
    memory_block_slab_manager:
        Option<Arc<super::k_dynamic_resource_manager::KMemoryBlockSlabManager>>,

    /// Compatibility alias to the application resource's block-info manager.
    block_info_manager: Option<Arc<super::k_dynamic_resource_manager::KBlockInfoManager>>,

    /// Default application and system manager sets created by
    /// `InitializeResourceManagers`.
    app_system_resource: Option<Arc<super::k_system_resource::KSystemResource>>,
    system_system_resource: Option<Arc<super::k_system_resource::KSystemResource>>,

    /// Kernel-wide physical memory layout. Upstream:
    /// `KernelCore::Impl::memory_layout` populated at boot by
    /// `KMemoryLayoutInit` from the SoC region tree. ruzu populates with
    /// `populate_default_dram_user_pools` at boot — the same data
    /// `core.rs` previously hardcoded inline for `initialize_pool` calls.
    memory_layout: Option<Arc<Mutex<super::k_memory_layout::KMemoryLayout>>>,

    // -- Core timing --
    /// Reference to the system's CoreTiming.
    /// Upstream: accessed via `system.CoreTiming()` through `System& system` reference.
    /// Stored here so fiber closures (guest_activate, idle thread) can access it
    /// without needing a System reference.
    core_timing: Option<Arc<CoreTiming>>,

    /// Reference to the owning System.
    /// Upstream: `Core::System& system` stored in KernelCore::Impl.
    /// Used by SVC dispatch (`Svc::Call(system, svc_number)`) and other
    /// kernel operations that need access to System-level state.
    system_ref: crate::core::SystemRef,

    /// Preemption timer event (10ms interval).
    /// Upstream: `std::shared_ptr<Core::Timing::EventType> preemption_event`.
    preemption_event: Option<Arc<parking_lot::Mutex<crate::core_timing::EventType>>>,

    /// Active service server managers.
    /// Upstream: `Impl::server_managers`.
    server_managers: Mutex<Vec<TrackedServerManager>>,

    /// Guest-core managers whose Rust owners must survive until the CPU fibers
    /// have stopped. Upstream service threads can complete during
    /// `CloseServices`; ruzu's cooperative guest fibers are only guaranteed
    /// not to touch their captured managers after `CpuManager::shutdown`.
    deferred_server_managers: Mutex<Vec<TrackedServerManager>>,
    /// Rust owners for service objects that upstream keeps on a guest service
    /// thread's native stack. Cooperative fiber shutdown can discard a
    /// suspended Rust stack without running local destructors, so these owners
    /// are released explicitly after the fibers stop.
    deferred_service_owners: Mutex<Vec<Box<dyn std::any::Any + Send>>>,

    /// Main host service threads returned by `RunOnHostCoreProcess`.
    ///
    /// Upstream detaches these `std::jthread`s, while `ServerManager` shutdown
    /// synchronizes their event-loop exit. Rust retains the join handles so a
    /// detached thread cannot outlive the raw `KernelCore` pointer captured by
    /// its closure.
    host_service_threads: Mutex<Vec<std::thread::JoinHandle<()>>>,

    /// Guest service processes created by `RunOnGuestCoreProcess`.
    /// Upstream keeps them alive after `KProcess::Register(*this, process)`.
    service_processes: Mutex<Vec<Arc<ProcessLock>>>,

    /// Host service processes created by `RunOnHostCoreProcess`.
    /// Upstream keeps them alive after `KProcess::Register(*this, process)` too.
    host_service_processes: Mutex<Vec<Arc<ProcessLock>>>,
}

// KProcess initial ID constants (matching upstream).
const INITIAL_PROCESS_ID_MIN: u64 = 1;
const PROCESS_ID_MIN: u64 = 81;

impl KernelCore {
    /// Construct a new kernel instance.
    pub fn new() -> Self {
        Self {
            is_multicore: true,
            is_shutting_down: AtomicBool::new(false),
            exception_exited: false,

            next_object_id: AtomicU32::new(0),
            next_kernel_process_id: AtomicU64::new(INITIAL_PROCESS_ID_MIN),
            next_user_process_id: AtomicU64::new(PROCESS_ID_MIN),
            next_thread_id: AtomicU64::new(1),

            hardware_timer: None,
            global_object_list_container: None,
            global_scheduler_context: None,
            object_name_global_data: None,

            schedulers: Vec::new(),
            cores: Vec::new(),

            shutdown_threads: Vec::new(),
            main_threads: Vec::new(),
            idle_threads: Vec::new(),
            application_thread: None,

            slab_resource_counts: KSlabResourceCounts::create_default(),

            process_list: Mutex::new(Vec::new()),
            process_list_lock: Mutex::new(()),
            terminating_processes: Mutex::new(Vec::new()),
            registered_objects: Mutex::new(Vec::new()),
            registered_in_use_objects: Mutex::new(Vec::new()),

            memory_manager: KMemoryManager::new(),
            font_shared_mem: None,
            irs_shared_mem: None,
            system_resource_limit: None,
            memory_block_slab_manager: None,
            block_info_manager: None,
            app_system_resource: None,
            system_system_resource: None,
            memory_layout: None,
            next_host_thread_id: AtomicU32::new(hardware_properties::NUM_CPU_CORES),
            single_core_thread_id: AtomicU32::new(0),
            core_timing: None,
            system_ref: crate::core::SystemRef::null(),
            preemption_event: None,
            server_managers: Mutex::new(Vec::new()),
            deferred_server_managers: Mutex::new(Vec::new()),
            deferred_service_owners: Mutex::new(Vec::new()),
            host_service_threads: Mutex::new(Vec::new()),
            service_processes: Mutex::new(Vec::new()),
            host_service_processes: Mutex::new(Vec::new()),
        }
    }

    /// Set whether emulation is multicore or single core.
    /// Must be called before Initialize.
    pub fn set_multicore(&mut self, is_multicore: bool) {
        self.is_multicore = is_multicore;
    }

    /// Initialize the kernel.
    pub fn initialize(&mut self) {
        self.hardware_timer = Some(Arc::new(KHardwareTimer::new()));

        self.global_object_list_container = Some(KAutoObjectWithListContainer::new());
        self.global_scheduler_context = Some(Arc::new(Mutex::new(GlobalSchedulerContext::new())));

        // Cache the GSC's `m_scheduler_lock` raw pointer so any site can
        // acquire `KScopedSchedulerLock` via `kernel::scheduler_lock()`.
        // The GSC sits behind an Arc held by the kernel for its entire
        // lifetime — the address of `m_scheduler_lock` is stable.
        if let Some(ref gsc_arc) = self.global_scheduler_context {
            let gsc_guard = gsc_arc.lock().unwrap();
            let sl_ptr = &gsc_guard.m_scheduler_lock
                as *const super::k_scheduler_lock::KAbstractSchedulerLock
                as *mut super::k_scheduler_lock::KAbstractSchedulerLock;
            SCHEDULER_LOCK_PTR.store(sl_ptr, Ordering::Release);
        }

        // Initialize slab resource counts.
        super::init::init_slab_setup::initialize_slab_resource_counts(
            &mut self.slab_resource_counts,
        );

        // Initialize shutdown threads (before physical cores, matching upstream order).
        self.initialize_shutdown_threads();

        // Initialize physical cores.
        self.initialize_physical_cores();

        // Initialize global data.
        self.object_name_global_data = Some(KObjectNameGlobalData::new());

        // Wire up scheduler lock callbacks.
        // The callbacks need kernel access but are plain fn pointers.
        // Store a raw pointer to self in a static for the callbacks to use.
        KERNEL_PTR.store(self as *mut KernelCore, Ordering::Release);
        if let Some(ref gsc) = self.global_scheduler_context {
            gsc.lock()
                .unwrap()
                .m_scheduler_lock
                .set_callbacks(&SCHEDULER_CALLBACKS);
        }

        // Initialize preemption event.
        // Upstream: InitializePreemption creates a looping CoreTiming event.
        // The callback takes KScopedSchedulerLock then calls PreemptThreads.
        self.preemption_event = Some(crate::core_timing::create_event(
            "PreemptionCallback".to_string(),
            Box::new(move |_time, _ns| {
                let kernel_ptr = KERNEL_PTR.load(Ordering::Acquire);
                if kernel_ptr.is_null() {
                    return None;
                }
                let kernel = unsafe { &*kernel_ptr };

                if let Some(scheduler_lock) = scheduler_lock() {
                    let _scheduler_guard =
                        super::k_scheduler_lock::KScopedSchedulerLock::new(scheduler_lock);
                    let current_thread_id = get_current_thread_id_fast();
                    if let Some(gsc_arc) = kernel.global_scheduler_context() {
                        gsc_arc.lock().unwrap().preempt_threads(current_thread_id);
                    }
                }

                if DUMP_REQUESTED.load(Ordering::Relaxed) {
                    // Register snapshots are refreshed when the JIT returns.
                    // This interrupt is diagnostic-only; normal preemption
                    // uses the core mask produced when the scheduler lock is
                    // released, matching upstream.
                    kernel.interrupt_all_cores();

                    // The per-core register snapshots are refreshed when the JIT
                    // returns. Wait for the interrupt above before dumping so
                    // SIGUSR1 reports the live guest loop rather than the
                    // preceding SVC state.
                    let deadline =
                        std::time::Instant::now() + std::time::Duration::from_millis(100);
                    while std::time::Instant::now() < deadline
                        && (0..hardware_properties::NUM_CPU_CORES as usize).any(|core_id| {
                            kernel
                                .physical_core(core_id)
                                .is_some_and(|core| core.is_interrupted())
                        })
                    {
                        std::thread::yield_now();
                    }
                    dump_thread_state(kernel);
                }

                None
            }),
        ));

        // Upstream: RegisterHostThread(nullptr) at the end of Kernel initialization.
        self.register_host_thread();
    }

    /// Run a service function on a guest core as a KThread with fiber context.
    ///
    /// Port of upstream `KernelCore::RunOnGuestCoreProcess` (kernel.cpp:1105-1139).
    /// Creates a new KProcess and KThread, initializes the thread as a service
    /// thread (HighPriority, core 3, priority 16), and makes it schedulable.
    /// The scheduler will pick it up and run the fiber on guest core 3.
    pub fn run_on_guest_core_process(&self, name: &str, func: Box<dyn FnOnce() + Send>) {
        use super::k_resource_limit::LimitableResource;
        use super::k_scoped_resource_reservation::KScopedResourceReservation;

        const SERVICE_THREAD_PRIORITY: i32 = 16;
        const SERVICE_THREAD_CORE: i32 = 3;

        // Make and initialize the service process before registration.
        // Upstream uses a default CreateProcessParameter and is_real=false.
        let process = Arc::new(ProcessLock::from_value(super::k_process::KProcess::new()));
        {
            let mut process_guard = process.lock().unwrap();
            let rc = process_guard.initialize(
                &[],
                0,
                0,
                0,
                0,
                0,
                self.get_system_resource_limit(),
                false,
            );
            assert_eq!(rc, crate::hle::result::RESULT_SUCCESS.get_inner_value());
            process_guard.bind_self_reference(&process);
        }

        self.register_process(Arc::clone(&process));
        self.service_processes
            .lock()
            .unwrap()
            .push(Arc::clone(&process));

        // Reserve the service thread from the process resource limit before
        // creating it, matching KScopedResourceReservation upstream.
        let resource_limit = process.lock().unwrap().resource_limit.clone();
        let mut thread_reservation =
            KScopedResourceReservation::new(resource_limit, LimitableResource::ThreadCountMax, 1);
        assert!(thread_reservation.succeeded());

        // Create the service thread.
        let thread = Arc::new(KThreadLock::new(super::k_thread::KThread::new()));
        let thread_id = self.create_new_thread_id();
        let object_id = self.create_new_object_id() as u64;
        if std::env::var_os("RUZU_TRACE_THREAD_ID").is_some() {
            log::info!("[THREAD_ID_NAME] tid={} name=service:{}", thread_id, name);
        }

        // Give the process a scheduler reference for the target core.
        // Upstream: service threads run on core 3, so use core 3's scheduler.
        if let Some(scheduler) = self.scheduler(SERVICE_THREAD_CORE as usize) {
            process.lock().unwrap().scheduler = Some(Arc::downgrade(scheduler));
        }
        // Wire GSC so the thread's notify_state_transition can update PQ directly.
        if let Some(gsc) = self.global_scheduler_context() {
            process
                .lock()
                .unwrap()
                .set_global_scheduler_context(gsc.clone());
        }

        {
            let mut t = thread.lock().unwrap();
            t.initialize_service_thread(
                self.system_ref,
                &thread,
                func,
                SERVICE_THREAD_PRIORITY,
                SERVICE_THREAD_CORE,
                &process,
                thread_id,
                object_id,
            );
        }

        // Commit the reservation, then register the thread before making it
        // runnable, matching upstream ordering.
        thread_reservation.commit();
        process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&thread));
        self.register_kernel_object(thread.lock().unwrap().get_object_id());

        // Make the thread runnable. KThread::run_thread() → set_state(RUNNABLE) →
        // notify_state_transition pushes to PQ via the GSC reference
        // (wired during initialize_service_thread from the process).
        // Must be called OUTSIDE GSC lock scope to avoid deadlock.
        super::k_thread::KThread::run_thread(&thread);

        // Request reschedule so the scheduler picks up the new thread.
        // Upstream: SetSchedulerUpdateNeeded + KScopedSchedulerLock release triggers reschedule.
        // Request reschedule on ALL cores (upstream does this via SetSchedulerUpdateNeeded
        // which marks a global flag checked by all cores).
        for core_id in 0..crate::hardware_properties::NUM_CPU_CORES as usize {
            if let Some(scheduler) = self.scheduler(core_id) {
                scheduler.lock().unwrap().request_schedule();
            }
        }
        // Verify the thread is in the priority queue.
        if let Some(gsc) = self.global_scheduler_context() {
            let gsc = gsc.lock().unwrap();
            let front = gsc
                .m_priority_queue
                .get_scheduled_front(SERVICE_THREAD_CORE);
            log::info!(
                "KernelCore::run_on_guest_core_process: '{}' thread_id={} core={} priority={} pq_front={:?}",
                name, thread_id, SERVICE_THREAD_CORE, SERVICE_THREAD_PRIORITY, front
            );
        }
    }

    /// Run a service function on a host thread with a dummy KThread for tracking.
    ///
    /// Shared helper for spawning a host OS thread with a dummy KThread.
    ///
    /// Port of upstream `RunHostThreadFunc(kernel, process, thread_name, func)`
    /// (kernel.cpp:1044-1075).
    ///
    /// Creates a dummy KThread owned by `process`, registers it, then spawns
    /// a host thread that sets the dummy as the current emulation thread and
    /// runs `func`.
    fn run_host_thread_func(
        &self,
        process: &Arc<ProcessLock>,
        thread_name: String,
        func: Box<dyn FnOnce() + Send>,
    ) -> std::thread::JoinHandle<()> {
        use super::k_resource_limit::LimitableResource;
        use super::k_scoped_resource_reservation::KScopedResourceReservation;

        let kernel_ptr = self as *const KernelCore as usize;
        let resource_limit = process.lock().unwrap().resource_limit.clone();
        let mut thread_reservation =
            KScopedResourceReservation::new(resource_limit, LimitableResource::ThreadCountMax, 1);
        assert!(thread_reservation.succeeded());

        let thread = Arc::new(KThreadLock::new(KThread::new()));
        {
            let thread_id = self.create_new_thread_id();
            let object_id = self.create_new_object_id() as u64;
            if std::env::var_os("RUZU_TRACE_THREAD_ID").is_some() {
                log::info!(
                    "[THREAD_ID_NAME] tid={} name=host:{}",
                    thread_id,
                    thread_name
                );
            }
            let mut thread_guard = thread.lock().unwrap();
            let rc = thread_guard.initialize_dummy_thread(Some(process), thread_id, object_id);
            assert_eq!(rc, crate::hle::result::RESULT_SUCCESS.get_inner_value());
            thread_guard.bind_self_reference(&thread);
        }
        thread_reservation.commit();

        process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&thread));
        let object_id = {
            let thread = thread.lock().unwrap();
            thread.get_object_id()
        };
        self.register_kernel_object(object_id);

        std::thread::Builder::new()
            .name(thread_name.clone())
            .spawn({
                let thread = Arc::clone(&thread);
                move || {
                    let kernel = unsafe { &*(kernel_ptr as *const KernelCore) };
                    kernel.register_host_thread_with_existing(Some(&thread));
                    // Per-thread alternate signal stack so the rdynarmic SIGSEGV
                    // handler (SA_ONSTACK; sigaltstack is per-thread) runs on a
                    // dedicated 2MB stack rather than this host thread's stack —
                    // otherwise a fault that reaches the handler's
                    // FastmemPatchTable HashMap/SipHash lookup can overflow the
                    // stack into a secondary SIGSEGV (silent exit 139). Mirrors
                    // the CPU-core registration in cpu_manager.rs.
                    rdynarmic::backend::x64::exception_handler::register_thread_signal_stack();
                    log::info!("Host service thread '{}' started", thread_name);
                    func();
                    log::info!("Host service thread '{}' exited", thread_name);

                    let (object_id, parent, resource_limit_release_hint) = {
                        let mut thread = thread.lock().unwrap();
                        let object_id = thread.get_object_id();
                        let parent = thread.parent.as_ref().and_then(Weak::upgrade);
                        let resource_limit_release_hint = thread.resource_limit_release_hint;
                        thread.finalize();
                        (object_id, parent, resource_limit_release_hint)
                    };
                    KThread::post_destroy(parent, resource_limit_release_hint);
                    kernel.unregister_kernel_object(object_id);
                    kernel.set_current_emu_thread(None);
                }
            })
            .expect("Failed to spawn host service thread")
    }

    /// Port of upstream `KernelCore::RunOnHostCoreProcess` (kernel.cpp:1077-1094).
    ///
    /// Creates a new KProcess, then delegates to `run_host_thread_func`.
    /// Used for CPU-intensive services (audio, filesystem, etc.) that benefit
    /// from running on host hardware.
    pub fn run_on_host_core_process(
        &self,
        name: &str,
        func: Box<dyn FnOnce() + Send>,
    ) -> std::thread::JoinHandle<()> {
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        {
            let mut process_guard = process.lock().unwrap();
            let rc = process_guard.initialize(
                &[],
                0,
                0,
                0,
                0,
                0,
                self.get_system_resource_limit(),
                false,
            );
            assert_eq!(rc, crate::hle::result::RESULT_SUCCESS.get_inner_value());
            process_guard.bind_self_reference(&process);
            // KProcess owns a KernelCore reference upstream and can always
            // reach the global scheduler. Preserve that dependency explicitly
            // in Rust so host dummy threads can use KSynchronizationObject::Wait
            // instead of MultiWait's polling fallback.
            let dummy_core = (hardware_properties::NUM_CPU_CORES - 1) as usize;
            if let Some(scheduler) = self.scheduler(dummy_core) {
                process_guard.attach_scheduler(scheduler);
            }
        }

        self.register_process(Arc::clone(&process));
        self.host_service_processes
            .lock()
            .unwrap()
            .push(Arc::clone(&process));

        self.run_host_thread_func(&process, format!("HLE:{}", name), func)
    }

    /// Retain an upstream-detached host service thread until `CloseServices`.
    ///
    /// The closure captures a raw `KernelCore` pointer, so Rust must join the
    /// thread before the kernel owner can be destroyed.
    pub fn track_host_service_thread(&self, thread: std::thread::JoinHandle<()>) {
        self.host_service_threads.lock().unwrap().push(thread);
    }

    /// Port of upstream `KernelCore::RunOnHostCoreThread` (kernel.cpp:1096-1103).
    ///
    /// Reuses the current emulation thread's process, then delegates to
    /// `run_host_thread_func`. Used by `ServerManager::start_additional_host_threads`.
    pub fn run_on_host_core_thread(
        &self,
        name: &str,
        func: Box<dyn FnOnce() + Send>,
    ) -> std::thread::JoinHandle<()> {
        let process = self
            .get_current_emu_thread()
            .and_then(|t| t.lock().unwrap().parent.as_ref().and_then(Weak::upgrade))
            .expect("run_on_host_core_thread: no current process");

        self.run_host_thread_func(&process, name.to_string(), func)
    }

    /// Wire the hardware timer to CoreTiming.
    /// Must be called after initialize() when System has CoreTiming available.
    pub fn wire_hardware_timer(&self, core_timing: Arc<CoreTiming>) {
        if let Some(ref timer) = self.hardware_timer {
            KHardwareTimer::wire_callback(timer, core_timing);
        }
    }

    /// Store a reference to CoreTiming so that fiber closures (guest_activate,
    /// idle thread) can access it without needing a System reference.
    /// Must be called after System creates CoreTiming and before CPU threads start.
    pub fn set_core_timing(&mut self, core_timing: Arc<CoreTiming>) {
        self.core_timing = Some(core_timing);
    }

    /// Get the CoreTiming reference.
    /// Upstream: accessed via `system.CoreTiming()`.
    pub fn core_timing(&self) -> Option<&Arc<CoreTiming>> {
        self.core_timing.as_ref()
    }

    /// Schedule the preemption timer event (10ms interval).
    /// Matches upstream `InitializePreemption(kernel)` in kernel.cpp.
    /// Must be called after set_core_timing().
    pub fn schedule_preemption_event(&self, core_timing: &Arc<CoreTiming>) {
        if let Some(ref event) = self.preemption_event {
            let interval = std::time::Duration::from_millis(10);
            core_timing.schedule_looping_event(interval, interval, event, false);
            log::info!("KernelCore: preemption event scheduled (10ms interval)");
        }

        // Keep the local SIGUSR1 diagnostic, but poll it from the upstream
        // preemption event instead of adding a second scheduling thread.
        install_sigusr1_handler();
    }

    /// Set the System reference.
    /// Upstream: `KernelCore(System& system)` stores it at construction.
    pub fn set_system_ref(&mut self, system_ref: crate::core::SystemRef) {
        self.system_ref = system_ref;
    }

    /// Get the System reference.
    /// Upstream: `KernelCore::System()`.
    pub fn system(&self) -> crate::core::SystemRef {
        self.system_ref
    }

    /// Set the application's main thread.
    pub fn set_application_thread(&mut self, thread: Arc<KThreadLock>) {
        self.application_thread = Some(thread);
    }

    /// Get the application's main thread.
    pub fn get_application_thread(&self) -> Option<Arc<KThreadLock>> {
        self.application_thread.clone()
    }

    /// Shutdown the kernel.
    pub fn shutdown(&mut self) {
        self.is_shutting_down.store(true, Ordering::Relaxed);

        self.close_services();
        self.finalize_services_after_cpu_shutdown();

        self.next_object_id.store(0, Ordering::Relaxed);
        self.next_kernel_process_id
            .store(INITIAL_PROCESS_ID_MIN, Ordering::Relaxed);
        self.next_user_process_id
            .store(PROCESS_ID_MIN, Ordering::Relaxed);
        self.next_thread_id.store(1, Ordering::Relaxed);
        self.preemption_event = None;

        // Clean up registered objects.
        {
            let mut in_use = self.registered_in_use_objects.lock().unwrap();
            in_use.clear();
        }
        {
            let mut registered = self.registered_objects.lock().unwrap();
            if !registered.is_empty() {
                log::debug!(
                    "{} kernel objects were dangling on shutdown!",
                    registered.len()
                );
                registered.clear();
            }
        }

        self.object_name_global_data = None;

        // Stop the timing callback while the scheduler lock and per-core
        // schedulers are still alive. `Finalize()` waits for an in-flight
        // callback, matching upstream's default UnscheduleEvent mode.
        if let Some(ref timer) = self.hardware_timer {
            timer.finalize();
        }
        self.hardware_timer = None;

        // Upstream closes each shutdown thread before resetting that core's
        // scheduler. CPU and service host threads have already been joined by
        // `System::shutdown_main_process`, so callback globals can no longer
        // safely expose this kernel while the scheduler owners are released.
        self.shutdown_threads.clear();
        KERNEL_PTR.store(std::ptr::null_mut(), Ordering::Release);
        SCHEDULER_LOCK_PTR.store(std::ptr::null_mut(), Ordering::Release);
        PENDING_ACTIVE_CORE_UPDATES.lock().unwrap().clear();
        self.schedulers.clear();
        self.cores.clear();
        self.main_threads.clear();
        self.idle_threads.clear();
        self.application_thread = None;
        self.service_processes.lock().unwrap().clear();
        self.host_service_processes.lock().unwrap().clear();
        self.process_list.lock().unwrap().clear();
        self.terminating_processes.lock().unwrap().clear();
        self.font_shared_mem = None;
        self.irs_shared_mem = None;

        // Upstream's thread/process Close() chain leaves no objects in the
        // scheduler context. Drop the Rust owning container now rather than
        // retaining stopped fibers until the next Initialize call.
        self.global_scheduler_context = None;
        self.core_timing = None;

        // Upstream closes its persistent system resource limit in Shutdown.
        self.system_resource_limit = None;

        if let Some(ref container) = self.global_object_list_container {
            container.finalize();
        }
        self.global_object_list_container = None;

        self.is_shutting_down.store(false, Ordering::Relaxed);
    }

    /// Close all active services.
    /// Upstream `KernelCore::CloseServices()` clears the tracked
    /// `ServerManager` owners; `ServerManager::~ServerManager` requests stop,
    /// signals its wakeup event, waits for the loop to stop, and clears extra
    /// `jthread`s. Rust stores service managers behind `Arc<Mutex<_>>`, and
    /// the service loop may hold that mutex while blocked in `WaitSignaled`.
    /// Keep stop/wakeup handles outside the mutex so the destructor ordering
    /// can be reproduced without ABBA-deadlocking the close path.
    pub fn close_services(&self) {
        // Host service processes are created asynchronously. Prevent a late
        // RunServer from registering after the manager list has been taken.
        self.is_shutting_down.store(true, Ordering::Release);

        let server_managers = {
            let mut managers = self.server_managers.lock().unwrap();
            std::mem::take(&mut *managers)
        };

        for tracked in &server_managers {
            tracked.stop_requested.store(true, Ordering::Release);
            tracked.wakeup_event.signal();
        }

        // Upstream destroys each ServerManager here. Its guest threads can
        // complete synchronously during destruction; ruzu's cooperative
        // fibers cannot, because CpuManager is still running and may retain a
        // captured Arc. Keep every owner alive until CPU shutdown instead of
        // waiting for `stopped` here and deadlocking shutdown on a suspended
        // guest fiber.
        self.deferred_server_managers
            .lock()
            .unwrap()
            .extend(server_managers);
    }

    /// Release guest service-manager owners after all CPU fibers have stopped.
    ///
    /// This is the Rust ownership counterpart of the synchronous guest-thread
    /// completion performed by upstream `ServerManager::~ServerManager`.
    /// It must run after `CpuManager::shutdown` and before scheduler/process
    /// owners are cleared.
    pub fn finalize_services_after_cpu_shutdown(&self) {
        let host_threads = {
            let mut threads = self.host_service_threads.lock().unwrap();
            std::mem::take(&mut *threads)
        };
        for thread in host_threads {
            if let Err(err) = thread.join() {
                log::warn!(
                    "KernelCore::finalize_services_after_cpu_shutdown: \
                     host service thread failed: {err:?}"
                );
            }
        }

        let deferred = {
            let mut managers = self.deferred_server_managers.lock().unwrap();
            std::mem::take(&mut *managers)
        };

        for tracked in deferred {
            ServerManager::join_host_threads(&tracked.host_threads, &tracked.name);
        }

        let service_owners = {
            let mut owners = self.deferred_service_owners.lock().unwrap();
            std::mem::take(&mut *owners)
        };
        drop(service_owners);
    }

    /// Preserve an upstream stack-owned service object outside a cooperative
    /// guest fiber so its Rust destructor runs during shutdown.
    pub fn retain_service_lifetime_owner<T>(&self, owner: T)
    where
        T: Send + 'static,
    {
        self.deferred_service_owners
            .lock()
            .unwrap()
            .push(Box::new(owner));
    }

    /// Release process and thread owners after cooperative guest fibers stop.
    ///
    /// Upstream intrusive references run `KThread::Finalize` and
    /// `KProcess::Finalize` from the final `Close()`. Rust must defer that final
    /// owner release until `CpuManager::shutdown`, because a suspended fiber may
    /// still be borrowing its `KThread` before then.
    pub fn finalize_terminated_processes_after_cpu_shutdown(&self) {
        let processes = {
            let mut processes = self.terminating_processes.lock().unwrap();
            std::mem::take(&mut *processes)
        };

        for process in processes {
            let threads = {
                let process = process.lock().unwrap();
                process.thread_objects.values().cloned().collect::<Vec<_>>()
            };
            for thread in threads {
                let (thread_id, object_id, global_scheduler_context) = {
                    let thread = thread.lock().unwrap();
                    (
                        thread.get_thread_id(),
                        thread.get_object_id(),
                        thread
                            .global_scheduler_context
                            .as_ref()
                            .and_then(Weak::upgrade),
                    )
                };
                if let Some(gsc) = global_scheduler_context {
                    gsc.lock().unwrap().remove_thread(thread_id);
                }

                let (parent, resource_limit_release_hint) = {
                    let mut thread = thread.lock().unwrap();
                    let parent = thread.parent.as_ref().and_then(Weak::upgrade);
                    let resource_limit_release_hint = thread.resource_limit_release_hint;
                    thread.finalize();
                    (parent, resource_limit_release_hint)
                };
                KThread::post_destroy(parent, resource_limit_release_hint);
                self.unregister_kernel_object(object_id);
            }

            // Session service destructors unregister process-owned events.
            // Detach and release those Arc owners without holding ProcessLock,
            // otherwise their callbacks recursively acquire the same mutex.
            let (client_sessions, sessions) = {
                let mut process = process.lock().unwrap();
                process.take_session_owners_for_finalize()
            };
            drop(client_sessions);
            drop(sessions);

            process.lock().unwrap().finalize();
        }
    }

    /// Run a service manager that already lives in its final shared Rust owner.
    pub fn run_server_shared(&self, manager: Arc<Mutex<ServerManager>>) {
        {
            let mut managers = self.server_managers.lock().unwrap();
            if self.is_shutting_down.load(Ordering::Relaxed) {
                return;
            }
            let (stop_requested, wakeup_event, host_threads, name) = {
                let guard = manager.lock().unwrap();
                (
                    guard.stop_requested_arc(),
                    guard.wakeup_event_arc(),
                    guard.host_threads_arc(),
                    guard.name().to_owned(),
                )
            };
            managers.push(TrackedServerManager {
                manager: Arc::clone(&manager),
                stop_requested,
                wakeup_event,
                host_threads,
                name,
            });
        }

        crate::hle::service::server_manager::ServerManager::loop_process_shared(&manager);
    }

    pub(crate) fn track_server_manager_for_test(&self, server_manager: Arc<Mutex<ServerManager>>) {
        let (stop_requested, wakeup_event, host_threads, name) = {
            let guard = server_manager.lock().unwrap();
            (
                guard.stop_requested_arc(),
                guard.wakeup_event_arc(),
                guard.host_threads_arc(),
                guard.name().to_owned(),
            )
        };
        self.server_managers
            .lock()
            .unwrap()
            .push(TrackedServerManager {
                manager: server_manager,
                stop_requested,
                wakeup_event,
                host_threads,
                name,
            });
    }

    pub(crate) fn ensure_tracked_server_manager_port_registrations(
        &self,
        process: Arc<ProcessLock>,
    ) {
        let managers = self.server_managers.lock().unwrap();
        for tracked in managers.iter() {
            tracked
                .manager
                .lock()
                .unwrap()
                .ensure_kernel_port_registrations_for_process(Arc::clone(&process));
        }
    }

    /// Register a process in the kernel-global process list.
    ///
    /// Upstream performs this from `KProcess::Register`. Keeping the owner in
    /// `KernelCore` lets services such as PM query the live process list instead
    /// of carrying per-service snapshots.
    pub fn register_process(&self, process: Arc<ProcessLock>) {
        let _guard = self.process_list_lock.lock().unwrap();
        let mut process_list = self.process_list.lock().unwrap();
        if !process_list
            .iter()
            .any(|registered| Arc::ptr_eq(registered, &process))
        {
            process_list.push(process);
        }
    }

    /// Remove a process from the kernel-global process list.
    ///
    /// Upstream: `KernelCore::RemoveProcess`.
    pub fn remove_process(&self, process: &Arc<ProcessLock>) {
        let _guard = self.process_list_lock.lock().unwrap();
        self.process_list
            .lock()
            .unwrap()
            .retain(|registered| !Arc::ptr_eq(registered, process));
    }

    /// Return the live kernel process list.
    ///
    /// Upstream returns a `std::list<KScopedAutoObject<KProcess>>` copy. Cloning
    /// the `Arc` owners gives Rust callers the same snapshot semantics.
    pub fn get_process_list(&self) -> Vec<Arc<ProcessLock>> {
        let _guard = self.process_list_lock.lock().unwrap();
        self.process_list.lock().unwrap().clone()
    }

    /// Terminate every registered process and clear the live process list.
    /// Matches upstream `KernelCore::Impl::TerminateAllProcesses()`.
    fn terminate_all_processes(&self) {
        let _guard = self.process_list_lock.lock().unwrap();
        let processes = {
            let mut process_list = self.process_list.lock().unwrap();
            std::mem::take(&mut *process_list)
        };

        for process in &processes {
            let _ = process.lock().unwrap().terminate();
        }

        // Upstream drops the process-list reference with Close(). Cooperative
        // Rust fibers need the equivalent final release deferred until the CPU
        // threads have joined.
        self.terminating_processes.lock().unwrap().extend(processes);
    }

    /// Suspend or resume emulation threads for the current application process.
    ///
    /// Upstream: `KernelCore::SuspendEmulation(bool)`.
    /// This port currently tracks only the frontend-loaded application process.
    pub fn suspend_emulation(&self, suspended: bool) {
        let should_suspend = self.exception_exited || suspended;
        let Some(process) = self.system_ref.get().current_process_arc.as_ref().cloned() else {
            return;
        };

        let threads: Vec<Arc<KThreadLock>> = {
            let process_guard = process.lock().unwrap();
            process_guard.thread_objects.values().cloned().collect()
        };

        for thread in threads {
            let mut thread_guard = thread.lock().unwrap();
            if should_suspend {
                thread_guard.request_suspend(SuspendType::System);
            } else {
                thread_guard.resume(SuspendType::System);
            }
        }

        if should_suspend {
            self.interrupt_all_cores();
        }
    }

    /// Begin kernel-side shutdown for all registered processes.
    ///
    /// Upstream: `KernelCore::ShutdownCores()`.
    /// Rust uses the same all-process termination point, then interrupts each
    /// core to drive cooperative guest fibers into their shutdown yield path.
    pub fn shutdown_cores(&self) {
        self.terminate_all_processes();
        KWorkerTaskManager::wait_for_global_idle();

        self.interrupt_all_cores();
    }

    /// Rust helper for owner lookups that upstream performs through the kernel
    /// process list. Returns the frontend application process or a guest
    /// service process with the matching process id.
    pub fn get_process_by_id(&self, process_id: u64) -> Option<Arc<ProcessLock>> {
        self.get_process_list()
            .into_iter()
            .find(|process| process.lock().unwrap().get_process_id() == process_id)
    }

    /// Rust counterpart to upstream `KernelCore::GetProcessList()` scans that
    /// identify a process by comparing `GetPageTable().GetBasePageTable()`.
    pub fn get_process_by_page_table_base(
        &self,
        table: *const super::k_page_table_base::KPageTableBase,
    ) -> Option<Arc<ProcessLock>> {
        if table.is_null() {
            return None;
        }

        if let Some(process) = (!self.system_ref.is_null())
            .then(|| self.system_ref.get().current_process_arc.as_ref().cloned())
            .flatten()
        {
            let process_guard = process.lock().unwrap();
            let process_table = process_guard.page_table.get_base()
                as *const super::k_page_table_base::KPageTableBase;
            if std::ptr::eq(process_table, table) {
                drop(process_guard);
                return Some(process);
            }
        }

        self.service_processes
            .lock()
            .unwrap()
            .iter()
            .find(|process| {
                let process_guard = process.lock().unwrap();
                let process_table = process_guard.page_table.get_base()
                    as *const super::k_page_table_base::KPageTableBase;
                std::ptr::eq(process_table, table)
            })
            .cloned()
            .or_else(|| {
                self.host_service_processes
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|process| {
                        let process_guard = process.lock().unwrap();
                        let process_table = process_guard.page_table.get_base()
                            as *const super::k_page_table_base::KPageTableBase;
                        std::ptr::eq(process_table, table)
                    })
                    .cloned()
            })
    }

    /// Rust helper for event owner lookup via the kernel process list.
    pub fn get_event_owner_process_id(&self, event_object_id: u64) -> Option<u64> {
        if let Some(process) = self.system_ref.get().current_process_arc.as_ref().cloned() {
            let process_guard = process.lock().unwrap();
            if process_guard
                .get_event_by_object_id(event_object_id)
                .is_some()
            {
                return Some(process_guard.get_process_id());
            }
        }

        self.service_processes
            .lock()
            .unwrap()
            .iter()
            .find_map(|process| {
                let process_guard = process.lock().unwrap();
                process_guard
                    .get_event_by_object_id(event_object_id)
                    .map(|_| process_guard.get_process_id())
            })
            .or_else(|| {
                self.host_service_processes
                    .lock()
                    .unwrap()
                    .iter()
                    .find_map(|process| {
                        let process_guard = process.lock().unwrap();
                        process_guard
                            .get_event_by_object_id(event_object_id)
                            .map(|_| process_guard.get_process_id())
                    })
            })
    }

    /// Rust helper for server-session owner lookup via the kernel process list.
    pub fn get_session_owner_process_id(&self, session_object_id: u64) -> Option<u64> {
        if let Some(process) = self.system_ref.get().current_process_arc.as_ref().cloned() {
            let process_guard = process.lock().unwrap();
            if process_guard
                .get_server_session_by_object_id(session_object_id)
                .is_some()
            {
                return Some(process_guard.get_process_id());
            }
        }

        self.service_processes
            .lock()
            .unwrap()
            .iter()
            .find_map(|process| {
                let process_guard = process.lock().unwrap();
                process_guard
                    .get_server_session_by_object_id(session_object_id)
                    .map(|_| process_guard.get_process_id())
            })
            .or_else(|| {
                self.host_service_processes
                    .lock()
                    .unwrap()
                    .iter()
                    .find_map(|process| {
                        let process_guard = process.lock().unwrap();
                        process_guard
                            .get_server_session_by_object_id(session_object_id)
                            .map(|_| process_guard.get_process_id())
                    })
            })
    }

    /// Rust helper for named client-port lookup via the kernel process list.
    ///
    /// This is the Rust counterpart to upstream `KObjectName::Find<KClientPort>(kernel, name)`.
    /// `KObjectName` already stores the named client-port object id; this helper
    /// resolves that object id back to the owning `KPort` by scanning the kernel
    /// process registries that currently own client-port objects.
    pub fn get_client_port_by_object_id(
        &self,
        client_port_object_id: u64,
    ) -> Option<Arc<Mutex<KPort>>> {
        if let Some(process) = self.system_ref.get().current_process_arc.as_ref().cloned() {
            let process_guard = process.lock().unwrap();
            if let Some(port) = process_guard.get_client_port_by_object_id(client_port_object_id) {
                return Some(port);
            }
        }

        if let Some(port) = self
            .process_list
            .lock()
            .unwrap()
            .iter()
            .find_map(|process| {
                process
                    .lock()
                    .unwrap()
                    .get_client_port_by_object_id(client_port_object_id)
            })
        {
            return Some(port);
        }

        self.service_processes
            .lock()
            .unwrap()
            .iter()
            .find_map(|process| {
                process
                    .lock()
                    .unwrap()
                    .get_client_port_by_object_id(client_port_object_id)
            })
            .or_else(|| {
                self.host_service_processes
                    .lock()
                    .unwrap()
                    .iter()
                    .find_map(|process| {
                        process
                            .lock()
                            .unwrap()
                            .get_client_port_by_object_id(client_port_object_id)
                    })
            })
    }

    /// Get the global scheduler context (Arc reference).
    pub fn global_scheduler_context(&self) -> Option<&Arc<Mutex<GlobalSchedulerContext>>> {
        self.global_scheduler_context.as_ref()
    }

    /// Get a physical core by index.
    /// Upstream: `KernelCore::PhysicalCore(id)`.
    pub fn physical_core(&self, id: usize) -> Option<&PhysicalCore> {
        self.cores.get(id).map(Arc::as_ref)
    }

    /// Get a physical core mutably by index.
    pub fn physical_core_mut(&mut self, id: usize) -> Option<&mut PhysicalCore> {
        self.cores.get_mut(id).and_then(Arc::get_mut)
    }

    /// Get a per-core scheduler by index.
    /// Upstream: `KernelCore::Scheduler(id)` (kernel.cpp:924).
    pub fn scheduler(&self, id: usize) -> Option<&Arc<Mutex<KScheduler>>> {
        self.schedulers.get(id)
    }

    fn interrupt_all_cores(&self) {
        for core_id in 0..self.cores.len() {
            if let Some(core) = self.physical_core(core_id) {
                core.interrupt();
            }
        }
    }

    /// Get the scheduler for the calling host thread's core.
    /// Upstream: `KernelCore::CurrentScheduler()` (kernel.cpp:956-963).
    /// Returns None if called from a non-core thread.
    pub fn current_scheduler(&self) -> Option<&Arc<Mutex<KScheduler>>> {
        let core_id = self.get_current_host_thread_id();
        if core_id >= hardware_properties::NUM_CPU_CORES {
            return None;
        }
        self.schedulers.get(core_id as usize)
    }

    /// Temporary workaround for the current fiber-return-to-originating-thread
    /// behavior: returns true only for the `NUM_CPU_CORES` dedicated guest
    /// core OS threads and false for HLE host service threads, the main
    /// thread, etc.
    ///
    /// TODO: remove this once host service waits no longer need the
    /// guest-core/host-thread split workaround in `ServerManager`.
    pub fn is_current_thread_guest_core(&self) -> bool {
        self.get_current_host_thread_id() < hardware_properties::NUM_CPU_CORES
    }

    /// Get the physical core index for the calling host thread.
    /// Upstream: `KernelCore::CurrentPhysicalCoreIndex()` (kernel.cpp:940-946).
    pub fn current_physical_core_index(&self) -> usize {
        let core_id = self.get_current_host_thread_id();
        if core_id >= hardware_properties::NUM_CPU_CORES {
            return (hardware_properties::NUM_CPU_CORES - 1) as usize;
        }
        core_id as usize
    }

    /// Get the physical core for the calling host thread.
    /// Upstream: `KernelCore::CurrentPhysicalCore()` (kernel.cpp:948).
    pub fn current_physical_core(&self) -> &PhysicalCore {
        self.cores[self.current_physical_core_index()].as_ref()
    }

    /// Get the physical core for the calling host thread (mutable).
    pub fn current_physical_core_mut(&mut self) -> &mut PhysicalCore {
        let idx = self.current_physical_core_index();
        Arc::get_mut(&mut self.cores[idx])
            .expect("current_physical_core_mut requires exclusive PhysicalCore ownership")
    }

    /// Register a CPU core thread by setting the thread-local host thread ID.
    /// Upstream: `KernelCore::RegisterCoreThread(core_id)` (kernel.cpp:1032).
    /// Must be called from the host thread that will run this core.
    pub fn register_core_thread(&self, core_id: usize) {
        assert!(core_id < hardware_properties::NUM_CPU_CORES as usize);
        let this_id = set_host_thread_id(core_id);
        if !self.is_multicore {
            self.single_core_thread_id.store(this_id, Ordering::Relaxed);
        }
    }

    /// Register a host thread (non-core) by allocating the next host thread ID.
    /// Upstream: `KernelCore::RegisterHostThread(existing_thread)` (kernel.cpp:1036).
    pub fn register_host_thread_with_existing(&self, existing_thread: Option<&Arc<KThreadLock>>) {
        HOST_THREAD_ID.with(|id| {
            if id.get() == u32::MAX {
                let new_id = self.next_host_thread_id.fetch_add(1, Ordering::Relaxed);
                id.set(new_id);
            }
        });

        if let Some(thread) = existing_thread {
            set_current_emu_thread(Some(thread));
        } else {
            let dummy = get_or_create_host_dummy_thread(self);
            set_current_emu_thread(Some(&dummy));
        }
    }

    /// Register a host thread (non-core) by allocating the next host thread ID.
    /// Upstream: `KernelCore::RegisterHostThread(existing_thread)` (kernel.cpp:1036).
    pub fn register_host_thread(&self) {
        self.register_host_thread_with_existing(None);
    }

    /// Get the host thread ID for the calling thread.
    /// Upstream: `Impl::GetCurrentHostThreadID()` (kernel.cpp:403-409).
    /// In single-core mode, if the calling thread is the single core thread,
    /// returns the current core index from CpuManager instead of the raw ID.
    pub fn get_current_host_thread_id(&self) -> u32 {
        let this_id = get_host_thread_id();
        if !self.is_multicore && this_id == self.single_core_thread_id.load(Ordering::Relaxed) {
            // Upstream: `system.GetCpuManager().CurrentCore()`.
            return if self.system_ref.is_null() {
                0
            } else {
                self.system_ref.get().get_cpu_manager().current_core() as u32
            };
        }
        this_id
    }

    /// Get the hardware timer (Arc reference).
    pub fn hardware_timer(&self) -> Option<&Arc<KHardwareTimer>> {
        self.hardware_timer.as_ref()
    }

    /// Get the object list container.
    pub fn object_list_container(&self) -> Option<&KAutoObjectWithListContainer> {
        self.global_object_list_container.as_ref()
    }

    /// Get the object name global data.
    pub fn object_name_global_data(&self) -> Option<&KObjectNameGlobalData> {
        self.object_name_global_data.as_ref()
    }

    pub fn ensure_object_name_global_data_for_test(&mut self) {
        if self.object_name_global_data.is_none() {
            self.object_name_global_data = Some(KObjectNameGlobalData::new());
        }
    }

    /// Get the current emulation thread for the calling host thread.
    /// Matches upstream `KernelCore::GetCurrentEmuThread()`.
    /// Delegates to the thread-local `get_current_emu_thread()` free function.
    pub fn get_current_emu_thread(&self) -> Option<Arc<KThreadLock>> {
        get_current_emu_thread()
    }

    /// Set the current emulation thread for the calling host thread.
    /// Matches upstream `KernelCore::SetCurrentEmuThread(KThread*)`.
    /// Delegates to the thread-local `set_current_emu_thread()` free function.
    pub fn set_current_emu_thread(&self, thread: Option<&Arc<KThreadLock>>) {
        set_current_emu_thread(thread);
    }

    /// Register a kernel object for leak tracking.
    pub fn register_kernel_object(&self, object_id: u64) {
        self.registered_objects.lock().unwrap().push(object_id);
    }

    /// Unregister a kernel object from leak tracking.
    pub fn unregister_kernel_object(&self, object_id: u64) {
        self.registered_objects
            .lock()
            .unwrap()
            .retain(|&id| id != object_id);
    }

    /// Register a kernel object as in-use.
    pub fn register_in_use_object(&self, object_id: u64) {
        self.registered_in_use_objects
            .lock()
            .unwrap()
            .push(object_id);
    }

    /// Unregister an in-use kernel object.
    pub fn unregister_in_use_object(&self, object_id: u64) {
        self.registered_in_use_objects
            .lock()
            .unwrap()
            .retain(|&id| id != object_id);
    }

    /// Whether the kernel is in multicore mode.
    pub fn is_multicore(&self) -> bool {
        self.is_multicore
    }

    /// Whether the kernel is shutting down.
    pub fn is_shutting_down(&self) -> bool {
        self.is_shutting_down.load(Ordering::Relaxed)
    }

    /// Workaround for single-core mode phantom mode.
    pub fn is_phantom_mode_for_single_core(&self) -> bool {
        is_phantom_mode_for_single_core()
    }

    /// Set the phantom mode for single core.
    pub fn set_is_phantom_mode_for_single_core(&self, value: bool) {
        assert!(!self.is_multicore);
        set_is_phantom_mode_for_single_core(value);
    }

    /// Get the slab resource counts.
    pub fn slab_resource_counts(&self) -> &KSlabResourceCounts {
        &self.slab_resource_counts
    }

    /// Get the slab resource counts (mutable).
    pub fn slab_resource_counts_mut(&mut self) -> &mut KSlabResourceCounts {
        &mut self.slab_resource_counts
    }

    // -- Private methods --

    /// Create a new object ID.
    pub fn create_new_object_id(&self) -> u32 {
        self.next_object_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Create a new kernel process ID.
    pub fn create_new_kernel_process_id(&self) -> u64 {
        self.next_kernel_process_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Create a new user process ID.
    pub fn create_new_user_process_id(&self) -> u64 {
        self.next_user_process_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Create a new thread ID.
    #[track_caller]
    pub fn create_new_thread_id(&self) -> u64 {
        let id = self.next_thread_id.fetch_add(1, Ordering::Relaxed);
        // RUZU_TRACE_THREAD_ID=1 — log every thread id allocation with caller
        if std::env::var_os("RUZU_TRACE_THREAD_ID").is_some() {
            let caller = std::panic::Location::caller();
            let bt = std::backtrace::Backtrace::force_capture();
            log::info!(
                "[THREAD_ID] alloc tid={} caller={}:{}\nBacktrace:\n{}",
                id,
                caller.file(),
                caller.line(),
                bt
            );
        }
        id
    }

    /// Get the memory manager.
    /// Upstream: `KernelCore::MemoryManager()`.
    pub fn memory_manager(&self) -> &KMemoryManager {
        &self.memory_manager
    }

    /// Get the memory manager (mutable).
    pub fn memory_manager_mut(&mut self) -> &mut KMemoryManager {
        &mut self.memory_manager
    }

    /// Initialize the kernel-owned shared font memory.
    ///
    /// This is the font portion of upstream
    /// `KernelCore::Impl::InitializeHackSharedMemory`: the object is allocated
    /// once by the kernel, has no owner mapping, and is exposed read-only to
    /// user processes.
    pub fn initialize_font_shared_memory(&mut self, device_memory: &DeviceMemory) -> ResultCode {
        const FONT_SHARED_MEMORY_SIZE: usize = 0x1100000;

        if self.font_shared_mem.is_some() {
            return crate::hle::result::RESULT_SUCCESS;
        }

        let mut shared_memory = KSharedMemory::new();
        let result = shared_memory.initialize(
            device_memory,
            &mut self.memory_manager,
            MemoryPermission::None,
            MemoryPermission::Read,
            FONT_SHARED_MEMORY_SIZE,
        );
        if result.is_error() {
            return result;
        }

        let object_id = self.create_new_object_id() as u64;
        self.font_shared_mem = Some((object_id, Arc::new(shared_memory)));
        crate::hle::result::RESULT_SUCCESS
    }

    /// Get the persistent shared font-memory object and its kernel object id.
    /// Upstream: `KernelCore::GetFontSharedMem()`.
    pub fn get_font_shared_mem(&self) -> Option<(u64, Arc<KSharedMemory>)> {
        self.font_shared_mem
            .as_ref()
            .map(|(object_id, shared_memory)| (*object_id, Arc::clone(shared_memory)))
    }

    /// Initialize the kernel-owned IRS shared memory.
    ///
    /// This is the IRS portion of upstream
    /// `KernelCore::Impl::InitializeHackSharedMemory`: the object is allocated
    /// once by the kernel, has no owner mapping, and is exposed read-only to
    /// user processes.
    pub fn initialize_irs_shared_memory(&mut self, device_memory: &DeviceMemory) -> ResultCode {
        const IRS_SHARED_MEMORY_SIZE: usize = 0x8000;

        if self.irs_shared_mem.is_some() {
            return crate::hle::result::RESULT_SUCCESS;
        }

        let mut shared_memory = KSharedMemory::new();
        let result = shared_memory.initialize(
            device_memory,
            &mut self.memory_manager,
            MemoryPermission::None,
            MemoryPermission::Read,
            IRS_SHARED_MEMORY_SIZE,
        );
        if result.is_error() {
            return result;
        }

        let object_id = self.create_new_object_id() as u64;
        self.irs_shared_mem = Some((object_id, Arc::new(shared_memory)));
        crate::hle::result::RESULT_SUCCESS
    }

    /// Get the persistent IRS shared-memory object and its kernel object id.
    /// Upstream: `KernelCore::GetIrsSharedMem()`.
    pub fn get_irs_shared_mem(&self) -> Option<(u64, Arc<KSharedMemory>)> {
        self.irs_shared_mem
            .as_ref()
            .map(|(object_id, shared_memory)| (*object_id, Arc::clone(shared_memory)))
    }

    /// Get the kernel-wide resource limit. Upstream:
    /// `KernelCore::GetSystemResourceLimit()`.
    pub fn get_system_resource_limit(
        &self,
    ) -> Option<Arc<super::k_resource_limit::KResourceLimit>> {
        self.system_resource_limit.clone()
    }

    /// Compatibility accessor for legacy/test page-table construction.
    /// Runtime processes select `GetAppSystemResource` or
    /// `GetSystemSystemResource`, as Eden does.
    pub fn get_memory_block_slab_manager(
        &self,
    ) -> Option<Arc<super::k_dynamic_resource_manager::KMemoryBlockSlabManager>> {
        self.memory_block_slab_manager.clone()
    }

    /// Test-only style initializer retained for isolated kernel fixtures that
    /// do not build the complete resource-manager graph.
    pub fn initialize_memory_block_slab_manager(&mut self, capacity: usize) {
        let mut slab = super::k_dynamic_resource_manager::KMemoryBlockSlabManager::new();
        slab.initialize(capacity);
        self.memory_block_slab_manager = Some(Arc::new(slab));
    }

    pub fn get_block_info_manager(
        &self,
    ) -> Option<Arc<super::k_dynamic_resource_manager::KBlockInfoManager>> {
        self.block_info_manager.clone()
    }

    pub fn initialize_block_info_manager(&mut self, capacity: usize) {
        let mut manager = super::k_dynamic_resource_manager::KBlockInfoManager::new();
        manager.initialize(capacity);
        self.block_info_manager = Some(Arc::new(manager));
    }

    /// Get the kernel-wide physical memory layout. Upstream:
    /// `KernelCore::MemoryLayout()`. Populated by
    /// `initialize_memory_layout` at boot.
    pub fn get_memory_layout(&self) -> Option<Arc<Mutex<super::k_memory_layout::KMemoryLayout>>> {
        self.memory_layout.clone()
    }

    /// Populate the kernel-wide physical memory layout with the three
    /// Switch DRAM user pools (Application / Applet / SystemNonSecure).
    /// Mirrors the upstream init pass that walks the SoC region tree.
    pub fn initialize_memory_layout(
        &mut self,
        application: (u64, usize),
        applet: (u64, usize),
        system: (u64, usize),
    ) {
        let mut layout = super::k_memory_layout::KMemoryLayout::new();
        layout.populate_default_dram_user_pools(
            application.0,
            application.1,
            applet.0,
            applet.1,
            system.0,
            system.1,
        );
        self.memory_layout = Some(Arc::new(Mutex::new(layout)));
    }

    /// Port of `KernelCore::Impl::InitializeResourceManagers`.
    pub fn initialize_resource_managers(&mut self, address: u64, size: usize) {
        use super::k_dynamic_page_manager::KDynamicPageManager;
        use super::k_dynamic_resource_manager::{
            KBlockInfoManager, KBlockInfoSlabHeap, KMemoryBlockSlabHeap, KMemoryBlockSlabManager,
        };
        use super::k_dynamic_slab_heap::KDynamicSlabHeap;
        use super::k_memory_block::{KMemoryBlock, PAGE_SIZE};
        use super::k_page_buffer::KPageBufferSlabHeap;
        use super::k_page_group::KBlockInfo;
        use super::k_page_table_manager::KPageTableManager;
        use super::k_page_table_slab_heap::KPageTableSlabHeap;
        use super::k_system_resource::KSystemResource;

        assert_eq!(address % PAGE_SIZE as u64, 0);
        assert_eq!(size % PAGE_SIZE, 0);

        let reference_count_size = common::alignment::align_up(
            KPageTableSlabHeap::calculate_reference_count_size(size) as u64,
            PAGE_SIZE as u64,
        ) as usize;
        assert!(reference_count_size < size);
        let manager_region_size = size - reference_count_size;

        let page_allocator = Arc::new(Mutex::new(KDynamicPageManager::new()));
        page_allocator
            .lock()
            .unwrap()
            .initialize(
                address,
                manager_region_size,
                PAGE_SIZE.max(KPageBufferSlabHeap::BUFFER_SIZE),
            )
            .expect("resource-manager dynamic page pool initialization");

        let mut page_buffer_slab_heap = KPageBufferSlabHeap::new();
        page_buffer_slab_heap.initialize();

        let app_memory_block_heap = Arc::new(KMemoryBlockSlabHeap::new(false));
        let system_memory_block_heap = Arc::new(KMemoryBlockSlabHeap::new(false));
        let block_info_heap = Arc::new(KBlockInfoSlabHeap::new(false));
        app_memory_block_heap.initialize_with_pages(
            Arc::clone(&page_allocator),
            APPLICATION_MEMORY_BLOCK_SLAB_HEAP_SIZE
                .div_ceil(KDynamicSlabHeap::<KMemoryBlock>::entries_per_page()),
        );
        system_memory_block_heap.initialize_with_pages(
            Arc::clone(&page_allocator),
            SYSTEM_MEMORY_BLOCK_SLAB_HEAP_SIZE
                .div_ceil(KDynamicSlabHeap::<KMemoryBlock>::entries_per_page()),
        );
        block_info_heap.initialize_with_pages(
            Arc::clone(&page_allocator),
            BLOCK_INFO_SLAB_HEAP_SIZE.div_ceil(KDynamicSlabHeap::<KBlockInfo>::entries_per_page()),
        );

        let num_page_table_pages = {
            let allocator = page_allocator.lock().unwrap();
            allocator
                .get_count()
                .checked_sub(allocator.get_used() + RESERVED_DYNAMIC_PAGE_COUNT)
                .expect("resource-manager region must retain reserved dynamic pages")
        };
        let page_table_heap = Arc::new(KPageTableSlabHeap::new());
        page_table_heap.initialize(Arc::clone(&page_allocator), num_page_table_pages);

        let app_memory_block_manager = Arc::new(KMemoryBlockSlabManager::new_with_resources(
            None,
            Arc::clone(&app_memory_block_heap),
        ));
        let system_memory_block_manager = Arc::new(KMemoryBlockSlabManager::new_with_resources(
            Some(Arc::clone(&page_allocator)),
            Arc::clone(&system_memory_block_heap),
        ));
        let app_block_info_manager = Arc::new(KBlockInfoManager::new_with_resources(
            None,
            Arc::clone(&block_info_heap),
        ));
        let system_block_info_manager = Arc::new(KBlockInfoManager::new_with_resources(
            Some(Arc::clone(&page_allocator)),
            Arc::clone(&block_info_heap),
        ));
        let app_page_table_manager = Arc::new(KPageTableManager::new_with_resources(
            None,
            Arc::clone(&page_table_heap),
        ));
        let system_page_table_manager = Arc::new(KPageTableManager::new_with_resources(
            Some(Arc::clone(&page_allocator)),
            Arc::clone(&page_table_heap),
        ));

        let allocator = page_allocator.lock().unwrap();
        assert_eq!(
            allocator.get_count() - allocator.get_used(),
            RESERVED_DYNAMIC_PAGE_COUNT
        );
        drop(allocator);

        let mut app_system_resource = KSystemResource::new();
        app_system_resource.set_managers(
            Arc::clone(&app_memory_block_manager),
            Arc::clone(&app_block_info_manager),
            Arc::clone(&app_page_table_manager),
        );
        let mut system_system_resource = KSystemResource::new();
        system_system_resource.set_managers(
            system_memory_block_manager,
            system_block_info_manager,
            system_page_table_manager,
        );

        // Compatibility accessors represent Eden's application manager set.
        self.memory_block_slab_manager = Some(app_memory_block_manager);
        self.block_info_manager = Some(app_block_info_manager);
        self.app_system_resource = Some(Arc::new(app_system_resource));
        self.system_system_resource = Some(Arc::new(system_system_resource));
    }

    pub fn get_app_system_resource(
        &self,
    ) -> Option<Arc<super::k_system_resource::KSystemResource>> {
        self.app_system_resource.clone()
    }

    pub fn get_system_system_resource(
        &self,
    ) -> Option<Arc<super::k_system_resource::KSystemResource>> {
        self.system_system_resource.clone()
    }

    /// Initialize the kernel-wide resource limit. Upstream:
    /// `KernelCore::Impl::InitializeSystemResourceLimit` (kernel.cpp:214):
    ///
    /// ```cpp
    /// system_resource_limit = KResourceLimit::Create(...);
    /// system_resource_limit->Initialize();
    /// SetLimitValue(PhysicalMemoryMax, total_size);
    /// SetLimitValue(ThreadCountMax, 800);
    /// SetLimitValue(EventCountMax, 900);
    /// SetLimitValue(TransferMemoryCountMax, 200);
    /// SetLimitValue(SessionCountMax, 1133);
    /// Reserve(PhysicalMemoryMax, kernel_size);
    /// Reserve(PhysicalMemoryMax, secure_applet_memory_size /* 4 MiB */);
    /// ```
    pub fn initialize_system_resource_limit(&mut self, total_size: i64, kernel_size: i64) {
        use super::k_resource_limit::{KResourceLimit, LimitableResource};
        let rl = KResourceLimit::new();
        let _ = rl.set_limit_value(LimitableResource::PhysicalMemoryMax, total_size);
        let _ = rl.set_limit_value(LimitableResource::ThreadCountMax, 800);
        let _ = rl.set_limit_value(LimitableResource::EventCountMax, 900);
        let _ = rl.set_limit_value(LimitableResource::TransferMemoryCountMax, 200);
        let _ = rl.set_limit_value(LimitableResource::SessionCountMax, 1133);
        let _ = rl.reserve(LimitableResource::PhysicalMemoryMax, kernel_size);
        // Reserve secure applet memory introduced in firmware 5.0.0.
        let secure_applet_memory_size: i64 = 4 * 1024 * 1024;
        let _ = rl.reserve(
            LimitableResource::PhysicalMemoryMax,
            secure_applet_memory_size,
        );
        self.system_resource_limit = Some(Arc::new(rl));
    }

    /// Initialize shutdown threads (one per core).
    ///
    /// Upstream: `Impl::InitializeShutdownThreads()` (kernel.cpp:340-348).
    /// Creates 4 high-priority kernel threads used for graceful shutdown.
    /// Must be called before `initialize_physical_cores()` so that thread IDs
    /// match upstream (shutdown threads get IDs 1-4, physical core threads 5-12).
    fn initialize_shutdown_threads(&mut self) {
        self.shutdown_threads.clear();

        let kernel_ptr = self as *const KernelCore as usize;

        for core_id in 0..hardware_properties::NUM_CPU_CORES {
            let thread = Arc::new(KThreadLock::new(KThread::new()));
            {
                let thread_id = self.create_new_thread_id();
                let object_id = self.create_new_object_id() as u64;
                let mut t = thread.lock().unwrap();
                t.set_current_core(core_id as i32);

                // Upstream: InitializeHighPriorityThread passes GetShutdownThreadStartFunc().
                let kp = kernel_ptr;
                let shutdown_func: Box<dyn FnOnce() + Send> = Box::new(move || {
                    let kernel = unsafe { &*(kp as *const KernelCore) };
                    crate::cpu_manager::CpuManager::shutdown_thread_function(kernel);
                });

                t.initialize_high_priority_thread(
                    core_id as i32,
                    thread_id,
                    object_id,
                    Some(shutdown_func),
                );
                t.bind_self_reference(&thread);
            }
            self.register_kernel_object(thread.lock().unwrap().get_object_id());
            self.shutdown_threads.push(thread);
        }
    }

    /// Initialize physical cores and per-core schedulers.
    ///
    /// Upstream: `Impl::InitializePhysicalCores()` (kernel.cpp:192-211).
    /// For each core, creates a KScheduler and PhysicalCore, then creates
    /// main and idle threads (ownerless, `ThreadType::Main`) and initializes
    /// the scheduler with them so that `get_scheduler_current_thread()` returns
    /// the main thread with a valid host fiber context.
    fn initialize_physical_cores(&mut self) {
        self.schedulers.clear();
        self.cores.clear();
        self.main_threads.clear();
        self.idle_threads.clear();

        // Capture a raw pointer to self for use in fiber closures.
        // Safety: KernelCore outlives all fibers — fibers are destroyed during
        // kernel shutdown which happens before KernelCore is dropped.
        let kernel_ptr = self as *const KernelCore as usize;

        for i in 0..hardware_properties::NUM_CPU_CORES as usize {
            let core_id = i as i32;

            // Create scheduler and physical core.
            let scheduler = Arc::new(Mutex::new(KScheduler::new(core_id)));
            // Wire the global scheduler context so the scheduler can find threads.
            if let Some(ref gsc) = self.global_scheduler_context {
                scheduler.lock().unwrap().global_scheduler_context = Some(gsc.clone());
            }
            self.schedulers.push(scheduler.clone());
            self.cores
                .push(Arc::new(PhysicalCore::new(i, self.is_multicore)));

            // Create main thread.
            // Upstream: auto* main_thread = KThread::Create(kernel);
            //           main_thread->SetCurrentCore(core);
            //           KThread::InitializeMainThread(system, main_thread, core);
            //           KThread::Register(kernel, main_thread);
            //
            // Upstream passes system.GetCpuManager().GetGuestActivateFunc() as the
            // fiber entry point. GuestActivate calls scheduler->Activate().
            let main_thread = Arc::new(KThreadLock::new(KThread::new()));
            {
                let thread_id = self.create_new_thread_id();
                let object_id = self.create_new_object_id() as u64;
                let mut t = main_thread.lock().unwrap();
                t.set_current_core(core_id);

                // Upstream: InitializeMainThread passes GetGuestActivateFunc().
                // GuestActivate() calls kernel.CurrentScheduler()->Activate().
                let kp = kernel_ptr;
                let guest_activate_func: Box<dyn FnOnce() + Send> = Box::new(move || {
                    // Safety: kernel_ptr is valid for the lifetime of this fiber.
                    let kernel = unsafe { &*(kp as *const KernelCore) };
                    crate::cpu_manager::CpuManager::guest_activate(kernel);
                });

                t.initialize_kernel_main_thread(
                    core_id,
                    thread_id,
                    object_id,
                    Some(guest_activate_func),
                );
                t.bind_self_reference(&main_thread);
            }
            // Upstream: KThread::Register(kernel, main_thread) → adds to object list container.
            self.register_kernel_object(main_thread.lock().unwrap().get_object_id());

            // Create idle thread.
            // Upstream: auto* idle_thread = KThread::Create(kernel);
            //           idle_thread->SetCurrentCore(core);
            //           KThread::InitializeIdleThread(system, idle_thread, core);
            //           KThread::Register(kernel, idle_thread);
            //
            // Upstream passes system.GetCpuManager().GetIdleThreadStartFunc() as the
            // fiber entry point. IdleThreadFunction dispatches to MultiCoreRunIdleThread
            // or SingleCoreRunIdleThread.
            let idle_thread = Arc::new(KThreadLock::new(KThread::new()));
            {
                let thread_id = self.create_new_thread_id();
                let object_id = self.create_new_object_id() as u64;
                let mut t = idle_thread.lock().unwrap();
                t.set_current_core(core_id);

                // Upstream: InitializeIdleThread passes GetIdleThreadStartFunc().
                // IdleThreadFunction calls MultiCoreRunIdleThread or SingleCoreRunIdleThread.
                let kp = kernel_ptr;
                let is_mc = self.is_multicore;
                let idle_func: Box<dyn FnOnce() + Send> = Box::new(move || {
                    // Safety: kernel_ptr is valid for the lifetime of this fiber.
                    let kernel = unsafe { &*(kp as *const KernelCore) };
                    if is_mc {
                        crate::cpu_manager::CpuManager::multi_core_run_idle_thread_entry(kernel);
                    } else {
                        // Single-core idle requires CoreTiming and additional state
                        // that are not available from the fiber context yet.
                        // For now, run the multicore idle path which is functionally
                        // equivalent (idle + handle interrupt loop).
                        crate::cpu_manager::CpuManager::multi_core_run_idle_thread_entry(kernel);
                    }
                });

                t.initialize_kernel_idle_thread(core_id, thread_id, object_id, Some(idle_func));
                t.bind_self_reference(&idle_thread);
            }
            self.register_kernel_object(idle_thread.lock().unwrap().get_object_id());

            // Initialize the scheduler with the main and idle threads.
            // Upstream: schedulers[i]->Initialize(main_thread, idle_thread, core);
            // This sets m_current_thread = main_thread so get_scheduler_current_thread()
            // returns a valid thread.
            scheduler
                .lock()
                .unwrap()
                .initialize_with_threads(&main_thread, &idle_thread, core_id);

            self.main_threads.push(main_thread);
            self.idle_threads.push(idle_thread);
        }

        // Rust adaptation: schedulers do not hold the upstream `KernelCore&`
        // owner, so wire the created PhysicalCore set into every scheduler for
        // save/load/interrupt paths that would otherwise go through `m_kernel`.
        for scheduler in &self.schedulers {
            scheduler.lock().unwrap().physical_cores = self.cores.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::SystemRef;

    #[test]
    fn font_shared_memory_is_kernel_owned_and_persistent() {
        use crate::device_memory::dram_memory_map;
        use crate::hle::kernel::k_memory_manager::Pool;

        // The buddy allocator rounds this 17 MiB contiguous request up to its
        // 32 MiB block class, so leave a second block for the pool boundary.
        const MEMORY_SIZE: usize = 0x400_0000;
        let device_memory = DeviceMemory::with_size(MEMORY_SIZE);
        let mut kernel = KernelCore::new();
        kernel.memory_manager_mut().initialize_pool(
            Pool::SECURE,
            dram_memory_map::BASE,
            MEMORY_SIZE,
        );

        assert!(kernel
            .initialize_font_shared_memory(&device_memory)
            .is_success());
        let (first_id, first) = kernel.get_font_shared_mem().unwrap();
        assert!(kernel
            .initialize_font_shared_memory(&device_memory)
            .is_success());
        let (second_id, second) = kernel.get_font_shared_mem().unwrap();

        assert_eq!(first.get_size(), 0x1100000);
        assert_eq!(first_id, second_id);
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn irs_shared_memory_is_kernel_owned_and_persistent() {
        use crate::device_memory::dram_memory_map;
        use crate::hle::kernel::k_memory_manager::Pool;

        let device_memory = DeviceMemory::with_size(0x1_0000);
        let mut kernel = KernelCore::new();
        kernel
            .memory_manager_mut()
            .initialize_pool(Pool::SECURE, dram_memory_map::BASE, 0x1_0000);

        assert!(kernel
            .initialize_irs_shared_memory(&device_memory)
            .is_success());
        let (first_id, first) = kernel.get_irs_shared_mem().unwrap();
        assert!(kernel
            .initialize_irs_shared_memory(&device_memory)
            .is_success());
        let (second_id, second) = kernel.get_irs_shared_mem().unwrap();

        assert_eq!(first.get_size(), 0x8000);
        assert_eq!(first_id, second_id);
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn scoped_test_kernels_are_isolated_per_host_thread() {
        use std::sync::mpsc;

        let first = ScopedKernelForTest::new();
        let first_ptr = get_kernel_ref().unwrap() as *const KernelCore as usize;
        let (second_ptr_tx, second_ptr_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _second = ScopedKernelForTest::new();
            second_ptr_tx
                .send(get_kernel_ref().unwrap() as *const KernelCore as usize)
                .unwrap();
            release_rx.recv().unwrap();
        });

        let second_ptr = second_ptr_rx.recv().unwrap();
        assert_ne!(first_ptr, second_ptr);
        assert_eq!(
            get_kernel_ref().unwrap() as *const KernelCore as usize,
            first_ptr
        );
        release_tx.send(()).unwrap();
        worker.join().unwrap();

        let nested = ScopedKernelForTest::new();
        assert_ne!(
            get_kernel_ref().unwrap() as *const KernelCore as usize,
            first_ptr
        );
        drop(nested);
        assert_eq!(
            get_kernel_ref().unwrap() as *const KernelCore as usize,
            first_ptr
        );
        drop(first);
    }

    #[test]
    fn close_services_requests_stop_on_tracked_server_managers() {
        let kernel = KernelCore::new();
        let manager = ServerManager::new_shared(SystemRef::null());

        kernel.track_server_manager_for_test(Arc::clone(&manager));
        kernel.close_services();

        assert!(manager.lock().unwrap().stop_requested_for_test());
    }

    #[test]
    fn close_services_defers_manager_ownership_until_cpu_shutdown() {
        let kernel = KernelCore::new();
        let manager = ServerManager::new_shared(SystemRef::null());
        kernel.track_server_manager_for_test(Arc::clone(&manager));

        kernel.close_services();

        assert!(manager.lock().unwrap().stop_requested_for_test());
        assert!(!manager.lock().unwrap().is_stopped());
        assert!(kernel.server_managers.lock().unwrap().is_empty());
        assert_eq!(kernel.deferred_server_managers.lock().unwrap().len(), 1);
    }

    #[test]
    fn finalize_services_releases_deferred_manager_ownership() {
        let kernel = KernelCore::new();
        let manager = ServerManager::new_shared(SystemRef::null());
        kernel.track_server_manager_for_test(manager);
        kernel.close_services();
        assert_eq!(kernel.deferred_server_managers.lock().unwrap().len(), 1);

        kernel.finalize_services_after_cpu_shutdown();
        assert!(kernel.deferred_server_managers.lock().unwrap().is_empty());
    }

    #[test]
    fn finalize_services_drops_stack_owned_service_adapters() {
        struct DropProbe(Arc<AtomicBool>);

        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let kernel = KernelCore::new();
        let dropped = Arc::new(AtomicBool::new(false));
        kernel.retain_service_lifetime_owner(DropProbe(Arc::clone(&dropped)));

        assert!(!dropped.load(Ordering::Acquire));
        kernel.finalize_services_after_cpu_shutdown();
        assert!(dropped.load(Ordering::Acquire));
    }

    #[test]
    fn finalize_services_does_not_lock_a_stopped_guest_manager() {
        let kernel = KernelCore::new();
        let manager = ServerManager::new_shared(SystemRef::null());
        kernel.track_server_manager_for_test(Arc::clone(&manager));
        kernel.close_services();

        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let _manager = manager.lock().unwrap();
            locked_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        locked_rx.recv().unwrap();

        // A stopped cooperative guest fiber may retain this mutex forever.
        // Post-CPU finalization must only touch the independently synchronized
        // host-thread handles.
        kernel.finalize_services_after_cpu_shutdown();

        release_tx.send(()).unwrap();
        holder.join().unwrap();
    }

    #[test]
    fn process_list_registration_is_idempotent_and_queryable() {
        let kernel = KernelCore::new();
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        process.lock().unwrap().process_id = 0x1234;

        kernel.register_process(Arc::clone(&process));
        kernel.register_process(Arc::clone(&process));

        assert_eq!(kernel.get_process_list().len(), 1);
        assert!(kernel
            .get_process_by_id(0x1234)
            .is_some_and(|found| Arc::ptr_eq(&found, &process)));
    }

    #[test]
    fn run_on_guest_core_process_retains_service_process_owner() {
        let mut kernel = KernelCore::new();
        kernel.initialize();

        kernel.run_on_guest_core_process("svc-test", Box::new(|| {}));

        assert_eq!(kernel.service_processes.lock().unwrap().len(), 1);

        let service_process = kernel.service_processes.lock().unwrap()[0].clone();
        let thread = {
            let process = service_process.lock().unwrap();
            assert!(process.is_initialized);
            process.thread_objects.values().next().cloned()
        }
        .expect("service process should keep its thread object");

        assert!(thread
            .lock()
            .unwrap()
            .parent
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some());
    }

    #[test]
    fn stopped_guest_service_releases_captured_owner() {
        let mut kernel = KernelCore::new();
        kernel.initialize();
        kernel.initialize_system_resource_limit(16 * 1024 * 1024, 0);

        let owner = Arc::new(());
        let weak_owner = Arc::downgrade(&owner);
        kernel.run_on_guest_core_process(
            "svc-lifetime-test",
            Box::new({
                let owner = Arc::clone(&owner);
                move || drop(owner)
            }),
        );
        drop(owner);

        assert!(weak_owner.upgrade().is_some());
        assert_eq!(kernel.get_process_list().len(), 1);

        kernel.shutdown_cores();
        assert!(kernel.get_process_list().is_empty());
        kernel.finalize_services_after_cpu_shutdown();
        kernel.finalize_terminated_processes_after_cpu_shutdown();
        kernel.shutdown();

        assert!(weak_owner.upgrade().is_none());
    }

    #[test]
    fn guest_service_thread_reservation_is_balanced_at_shutdown() {
        use super::super::k_resource_limit::LimitableResource;

        let mut kernel = KernelCore::new();
        kernel.initialize();
        kernel.initialize_system_resource_limit(16 * 1024 * 1024, 0);

        kernel.run_on_guest_core_process("sm", Box::new(|| {}));

        let process = kernel.service_processes.lock().unwrap()[0].clone();
        assert_eq!(process.lock().unwrap().thread_objects.len(), 1);
        assert_eq!(
            kernel
                .get_system_resource_limit()
                .unwrap()
                .get_current_value(LimitableResource::ThreadCountMax),
            1
        );

        kernel.shutdown_cores();
        kernel.finalize_services_after_cpu_shutdown();
        kernel.finalize_terminated_processes_after_cpu_shutdown();

        assert_eq!(
            kernel
                .get_system_resource_limit()
                .unwrap()
                .get_current_value(LimitableResource::ThreadCountMax),
            0
        );
        kernel.shutdown();
    }

    #[test]
    fn run_on_host_core_process_wires_dummy_thread_to_scheduler() {
        use super::super::k_resource_limit::LimitableResource;

        let mut kernel = KernelCore::new();
        kernel.initialize();
        kernel.initialize_system_resource_limit(16 * 1024 * 1024, 0);

        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let host_thread = kernel.run_on_host_core_process(
            "host-svc-test",
            Box::new(move || {
                ready_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            }),
        );
        ready_rx.recv().unwrap();

        let process = kernel.host_service_processes.lock().unwrap()[0].clone();
        let thread = {
            let process = process.lock().unwrap();
            let process_scheduler = process
                .scheduler
                .as_ref()
                .and_then(Weak::upgrade)
                .expect("host process should inherit the dummy core scheduler");
            assert!(Arc::ptr_eq(
                &process_scheduler,
                kernel
                    .scheduler((hardware_properties::NUM_CPU_CORES - 1) as usize)
                    .unwrap()
            ));
            assert!(process.global_scheduler_context.is_some());
            process.thread_objects.values().next().cloned()
        };
        let thread = thread.expect("host service process should retain its live dummy thread");

        let thread = thread.lock().unwrap();
        let object_id = thread.get_object_id();
        let thread_id = thread.get_thread_id();
        assert!(thread.is_dummy_thread());
        assert!(thread.scheduler.as_ref().and_then(Weak::upgrade).is_some());
        assert!(thread
            .global_scheduler_context
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some());
        drop(thread);
        assert_eq!(
            kernel
                .get_system_resource_limit()
                .unwrap()
                .get_current_value(LimitableResource::ThreadCountMax),
            1
        );

        release_tx.send(()).unwrap();
        host_thread.join().unwrap();

        let process = process.lock().unwrap();
        assert!(process.thread_objects.is_empty());
        assert!(process.get_thread_by_thread_id(thread_id).is_none());
        drop(process);
        assert!(!kernel
            .registered_objects
            .lock()
            .unwrap()
            .contains(&object_id));
        assert_eq!(
            kernel
                .get_system_resource_limit()
                .unwrap()
                .get_current_value(LimitableResource::ThreadCountMax),
            0
        );
    }

    #[test]
    fn register_host_thread_sets_dummy_current_thread() {
        let mut kernel = KernelCore::new();
        kernel.initialize();

        set_current_emu_thread(None);
        kernel.register_host_thread();

        let current = kernel
            .get_current_emu_thread()
            .expect("host thread should have a current dummy thread");
        assert!(current.lock().unwrap().is_dummy_thread());
    }

    #[test]
    fn register_host_thread_with_existing_keeps_existing_thread_as_current() {
        let mut kernel = KernelCore::new();
        kernel.initialize();

        let thread = Arc::new(KThreadLock::new(KThread::new()));
        {
            let thread_id = kernel.create_new_thread_id();
            let object_id = kernel.create_new_object_id() as u64;
            let mut guard = thread.lock().unwrap();
            let rc = guard.initialize_dummy_thread(None, thread_id, object_id);
            assert_eq!(rc, crate::hle::result::RESULT_SUCCESS.get_inner_value());
            guard.bind_self_reference(&thread);
        }

        set_current_emu_thread(None);
        kernel.register_host_thread_with_existing(Some(&thread));

        let current = kernel
            .get_current_emu_thread()
            .expect("existing thread should remain current");
        assert!(Arc::ptr_eq(&current, &thread));
    }

    #[test]
    fn host_thread_id_is_reloaded_after_cross_thread_fiber_resume() {
        use common::fiber::Fiber;
        use std::sync::atomic::AtomicUsize;
        use std::sync::{Barrier, Mutex as StdMutex};

        struct FiberMigrationState {
            kernel: Arc<KernelCore>,
            barrier: Barrier,
            phase: AtomicUsize,
            observed_first: AtomicUsize,
            observed_second: AtomicUsize,
            observed_first_thread: AtomicUsize,
            observed_second_thread: AtomicUsize,
            observed_first_fast_thread: AtomicUsize,
            observed_second_fast_thread: AtomicUsize,
            host_threads: [Arc<KThreadLock>; 2],
            thread_fibers: [StdMutex<Option<Arc<Fiber>>>; 2],
            worker: StdMutex<Option<Arc<Fiber>>>,
        }

        let host_threads = [0x1001, 0x1002].map(|thread_id| {
            let thread = Arc::new(KThreadLock::new(KThread::new()));
            thread.lock().unwrap().thread_id = thread_id;
            thread
        });
        let state = Arc::new(FiberMigrationState {
            kernel: Arc::new(KernelCore::new()),
            barrier: Barrier::new(2),
            phase: AtomicUsize::new(0),
            observed_first: AtomicUsize::new(usize::MAX),
            observed_second: AtomicUsize::new(usize::MAX),
            observed_first_thread: AtomicUsize::new(usize::MAX),
            observed_second_thread: AtomicUsize::new(usize::MAX),
            observed_first_fast_thread: AtomicUsize::new(usize::MAX),
            observed_second_fast_thread: AtomicUsize::new(usize::MAX),
            host_threads,
            thread_fibers: [StdMutex::new(None), StdMutex::new(None)],
            worker: StdMutex::new(None),
        });

        let weak_state = Arc::downgrade(&state);
        let worker = Fiber::new(Box::new(move || {
            let state = weak_state.upgrade().unwrap();
            state
                .observed_first
                .store(state.kernel.current_physical_core_index(), Ordering::SeqCst);
            state.observed_first_thread.store(
                get_current_emu_thread()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .get_thread_id() as usize,
                Ordering::SeqCst,
            );
            state.observed_first_fast_thread.store(
                get_current_thread_id_fast().unwrap() as usize,
                Ordering::SeqCst,
            );
            let worker = state.worker.lock().unwrap().as_ref().unwrap().clone();
            let thread_fiber = state.thread_fibers[0]
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .clone();
            Fiber::yield_to(Arc::downgrade(&worker), &thread_fiber);

            state
                .observed_second
                .store(state.kernel.current_physical_core_index(), Ordering::SeqCst);
            state.observed_second_thread.store(
                get_current_emu_thread()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .get_thread_id() as usize,
                Ordering::SeqCst,
            );
            state.observed_second_fast_thread.store(
                get_current_thread_id_fast().unwrap() as usize,
                Ordering::SeqCst,
            );
            let worker = state.worker.lock().unwrap().as_ref().unwrap().clone();
            let thread_fiber = state.thread_fibers[1]
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .clone();
            Fiber::yield_to(Arc::downgrade(&worker), &thread_fiber);
        }));
        *state.worker.lock().unwrap() = Some(worker);

        let first_state = Arc::clone(&state);
        let first = std::thread::spawn(move || {
            first_state.kernel.register_core_thread(0);
            set_current_emu_thread(Some(&first_state.host_threads[0]));
            let thread_fiber = Fiber::thread_to_fiber();
            *first_state.thread_fibers[0].lock().unwrap() = Some(Arc::clone(&thread_fiber));
            first_state.barrier.wait();

            let worker = first_state.worker.lock().unwrap().as_ref().unwrap().clone();
            Fiber::yield_to(Arc::downgrade(&thread_fiber), &worker);
            first_state.phase.store(1, Ordering::Release);
            thread_fiber.exit();
        });

        let second_state = Arc::clone(&state);
        let second = std::thread::spawn(move || {
            second_state.kernel.register_core_thread(1);
            set_current_emu_thread(Some(&second_state.host_threads[1]));
            let thread_fiber = Fiber::thread_to_fiber();
            *second_state.thread_fibers[1].lock().unwrap() = Some(Arc::clone(&thread_fiber));
            second_state.barrier.wait();
            while second_state.phase.load(Ordering::Acquire) == 0 {
                std::hint::spin_loop();
            }

            let worker = second_state
                .worker
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .clone();
            Fiber::yield_to(Arc::downgrade(&thread_fiber), &worker);
            thread_fiber.exit();
        });

        first.join().unwrap();
        second.join().unwrap();
        assert_eq!(state.observed_first.load(Ordering::SeqCst), 0);
        assert_eq!(state.observed_second.load(Ordering::SeqCst), 1);
        assert_eq!(state.observed_first_thread.load(Ordering::SeqCst), 0x1001);
        assert_eq!(state.observed_second_thread.load(Ordering::SeqCst), 0x1002);
        assert_eq!(
            state.observed_first_fast_thread.load(Ordering::SeqCst),
            0x1001
        );
        assert_eq!(
            state.observed_second_fast_thread.load(Ordering::SeqCst),
            0x1002
        );
    }

    #[test]
    fn phantom_mode_is_local_to_the_calling_host_thread() {
        let mut kernel = KernelCore::new();
        kernel.is_multicore = false;
        let kernel = Arc::new(kernel);
        let barrier = Arc::new(std::sync::Barrier::new(2));

        let first_kernel = Arc::clone(&kernel);
        let first_barrier = Arc::clone(&barrier);
        let first = std::thread::spawn(move || {
            first_kernel.set_is_phantom_mode_for_single_core(true);
            first_barrier.wait();
            assert!(first_kernel.is_phantom_mode_for_single_core());
        });

        let second_kernel = Arc::clone(&kernel);
        let second = std::thread::spawn(move || {
            barrier.wait();
            assert!(!second_kernel.is_phantom_mode_for_single_core());
        });

        first.join().unwrap();
        second.join().unwrap();
    }

    #[test]
    fn initialize_physical_cores_wires_physical_cores_into_each_scheduler() {
        let mut kernel = KernelCore::new();
        kernel.initialize();

        for scheduler in &kernel.schedulers {
            let scheduler = scheduler.lock().unwrap();
            assert_eq!(scheduler.physical_cores.len(), kernel.cores.len());
        }
    }

    #[test]
    fn memory_block_slab_capacity_matches_upstream_heaps() {
        assert_eq!(APPLICATION_MEMORY_BLOCK_SLAB_HEAP_SIZE, 20_000);
        assert_eq!(SYSTEM_MEMORY_BLOCK_SLAB_HEAP_SIZE, 10_000);
    }

    #[test]
    fn resource_managers_preserve_app_fixed_and_system_dynamic_growth() {
        let mut kernel = KernelCore::new();
        kernel.initialize_resource_managers(
            0xFFFF_E000_0000_0000,
            crate::hle::kernel::k_memory_layout::KERNEL_PAGE_TABLE_HEAP_SIZE,
        );
        let app = kernel.get_app_system_resource().unwrap();
        let system = kernel.get_system_system_resource().unwrap();
        let app_blocks = app.block_info_manager_arc();
        let system_blocks = system.block_info_manager_arc();

        assert!(!Arc::ptr_eq(&app_blocks, &system_blocks));
        assert_eq!(app_blocks.get_count(), system_blocks.get_count());
        let mut held = Vec::with_capacity(app_blocks.get_count() + 1);
        for _ in 0..app_blocks.get_count() {
            held.push(app_blocks.allocate().unwrap());
        }
        assert!(app_blocks.allocate().is_none());
        let old_count = system_blocks.get_count();
        held.push(
            system_blocks
                .allocate()
                .expect("system block-info manager must grow shared heap"),
        );
        assert!(system_blocks.get_count() > old_count);

        for block in held {
            system_blocks.free(block);
        }
    }
}

impl Default for KernelCore {
    fn default() -> Self {
        Self::new()
    }
}
