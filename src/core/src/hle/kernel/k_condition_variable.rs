//! Port of zuyu/src/core/hle/kernel/k_condition_variable.h/.cpp
//! Status: Ported (matches upstream structure)
//! Derniere synchro: 2026-03-21
//!
//! KConditionVariable: implements condition-variable-style synchronization
//! for userspace mutexes and condition variables.
//!
//! Upstream holds `System& m_system`, `KernelCore& m_kernel`, and
//! `ThreadTree m_tree`. In Rust, the System/KernelCore references are
//! passed by callers (process + thread Arcs) because the kernel singleton
//! pattern does not map directly to Rust ownership.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::k_process::ProcessLock;
use crate::arm::exclusive_monitor::ExclusiveMonitor;
use crate::hle::kernel::k_process::KProcess;
use crate::hle::kernel::k_thread::{
    ConditionVariableThreadKey, KThread, KThreadLock, LockWithPriorityInheritanceInfo,
    ThreadWaitReasonForDebugging,
};
use crate::hle::kernel::k_thread_queue::KThreadQueue;
use crate::hle::kernel::k_typed_address::KProcessAddress;
use crate::hle::kernel::svc::svc_results::{
    RESULT_INVALID_CURRENT_MEMORY, RESULT_INVALID_HANDLE, RESULT_INVALID_STATE,
    RESULT_TERMINATION_REQUESTED,
};
use crate::hle::kernel::svc_common::Handle;
use crate::hle::kernel::svc_common::{HANDLE_WAIT_MASK, INVALID_HANDLE};
use crate::hle::result::{ResultCode, RESULT_SUCCESS};

static TRACE_KCV_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Diagnostic protocol-health counters for the userspace mutex/condvar
/// protocol, dumped on SIGUSR1. Relaxed atomic increments — always on.
/// "Empty" operations are wasted syscalls: userspace observed a
/// waiters/has-waiters flag that the kernel view does not corroborate.
pub mod cv_stats {
    use std::sync::atomic::{AtomicU64, Ordering};

    pub static SIGNALS: AtomicU64 = AtomicU64::new(0);
    /// SignalProcessWideKey calls that found no waiter in the tree.
    pub static SIGNALS_EMPTY: AtomicU64 = AtomicU64::new(0);
    /// Empty signals where the guest-visible cv word was 0 (guest ignored
    /// the has-waiters hint — game/SDK behavior, upstream pays it too).
    pub static SIGNALS_EMPTY_HINT0: AtomicU64 = AtomicU64::new(0);
    /// Empty signals where the cv word was non-zero (our clear is not
    /// visible to the guest — a ruzu bug).
    pub static SIGNALS_EMPTY_HINT_SET: AtomicU64 = AtomicU64::new(0);
    /// Total waiters woken/requeued across all signals.
    pub static SIGNALED_WAITERS: AtomicU64 = AtomicU64::new(0);
    /// signal_impl outcomes: mutex free → thread woken directly.
    pub static SIGNAL_WAKE_DIRECT: AtomicU64 = AtomicU64::new(0);
    /// signal_impl outcomes: mutex held → thread requeued on owner.
    pub static SIGNAL_REQUEUE_ON_OWNER: AtomicU64 = AtomicU64::new(0);
    pub static UNLOCKS: AtomicU64 = AtomicU64::new(0);
    /// ArbitrateUnlock calls with no kernel waiter on the key.
    pub static UNLOCKS_EMPTY: AtomicU64 = AtomicU64::new(0);
    pub static WAITS: AtomicU64 = AtomicU64::new(0);
    /// Waits that returned immediately with timeout==0 (TimedOut).
    pub static WAITS_TIMEOUT0: AtomicU64 = AtomicU64::new(0);
    pub static WFA: AtomicU64 = AtomicU64::new(0);
    /// ArbitrateLock calls resolved without waiting (tag mismatch).
    pub static WFA_IMMEDIATE: AtomicU64 = AtomicU64::new(0);

    pub fn inc(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn summary_string() -> String {
        format!(
            "[CV_STATS] signals={} signals_empty={} empty_hint0={} empty_hint_set={} signaled_waiters={} wake_direct={} requeue_on_owner={} \
             unlocks={} unlocks_empty={} waits={} waits_timeout0={} wfa={} wfa_immediate={}",
            SIGNALS.load(Ordering::Relaxed),
            SIGNALS_EMPTY.load(Ordering::Relaxed),
            SIGNALS_EMPTY_HINT0.load(Ordering::Relaxed),
            SIGNALS_EMPTY_HINT_SET.load(Ordering::Relaxed),
            SIGNALED_WAITERS.load(Ordering::Relaxed),
            SIGNAL_WAKE_DIRECT.load(Ordering::Relaxed),
            SIGNAL_REQUEUE_ON_OWNER.load(Ordering::Relaxed),
            UNLOCKS.load(Ordering::Relaxed),
            UNLOCKS_EMPTY.load(Ordering::Relaxed),
            WAITS.load(Ordering::Relaxed),
            WAITS_TIMEOUT0.load(Ordering::Relaxed),
            WFA.load(Ordering::Relaxed),
            WFA_IMMEDIATE.load(Ordering::Relaxed),
        )
    }
}

fn trace_kcv(args: std::fmt::Arguments<'_>) {
    static LIMIT: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    let Some(limit) = *LIMIT.get_or_init(|| {
        std::env::var_os("RUZU_TRACE_KCV")?;
        Some(
            std::env::var("RUZU_TRACE_KCV_LIMIT")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(256),
        )
    }) else {
        return;
    };
    let idx = TRACE_KCV_COUNT.fetch_add(1, Ordering::Relaxed);
    if idx < limit {
        log::info!("{}", args);
    }
}

fn should_trace_kcv_wake() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_KCV_WAKE").is_some())
}

/// Returns Some(key) if the env var `RUZU_TRACE_CV_KEY=0xHEX` matches the
/// given `cv_key`. Used to focus tracing on one specific cv address (e.g.
/// the lost-wakeup target 0x808EA038) without paying the 256-entry limit
/// of `RUZU_TRACE_KCV`. When set, every signal/wait/end_wait on that key
/// is logged unconditionally.
fn cv_key_trace_target() -> Option<u64> {
    static TARGET: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    *TARGET.get_or_init(|| {
        let raw = std::env::var("RUZU_TRACE_CV_KEY").ok()?;
        let trimmed = raw.trim();
        let trimmed = trimmed
            .strip_prefix("0x")
            .or_else(|| trimmed.strip_prefix("0X"))
            .unwrap_or(trimmed);
        u64::from_str_radix(trimmed, 16).ok()
    })
}

fn should_trace_cv_key(cv_key: u64) -> bool {
    matches!(cv_key_trace_target(), Some(target) if target == cv_key)
}

fn lock_pi_addr_trace_target() -> Option<u64> {
    static TARGET: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    *TARGET.get_or_init(|| {
        let raw = std::env::var("RUZU_TRACE_LOCK_ADDR").ok()?;
        let trimmed = raw.trim();
        let trimmed = trimmed
            .strip_prefix("0x")
            .or_else(|| trimmed.strip_prefix("0X"))
            .unwrap_or(trimmed);
        u64::from_str_radix(trimmed, 16).ok()
    })
}

fn trace_ct_fire_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_CT_FIRE").is_some())
}

fn trace_unlock_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_UNLOCK").is_some())
}

fn should_trace_lock_pi(addr: u64) -> bool {
    common::trace::is_enabled(common::trace::cat::LOCK_PI)
        && lock_pi_addr_trace_target().map_or(true, |target| target == addr)
}

fn trace_lock_pi(
    stage: u64,
    tid: u64,
    addr: u64,
    handle: u32,
    tag: u32,
    test_tag: u32,
    owner_tid: u64,
    next_tid: u64,
    has_waiters: bool,
    next_value: u32,
    result: ResultCode,
    owner_obj_id: u64,
    waiter_count: u32,
    extra: u64,
) {
    common::trace::emit_raw(
        common::trace::cat::LOCK_PI,
        &[
            stage,
            tid,
            addr,
            handle as u64,
            tag as u64,
            test_tag as u64,
            owner_tid,
            next_tid,
            u64::from(has_waiters),
            next_value as u64,
            result.get_inner_value() as u64,
            owner_obj_id,
            waiter_count as u64,
            extra,
        ],
    );
}

fn current_trace_owner() -> (Option<u64>, i32) {
    let tid = super::kernel::get_current_thread_id_fast();
    let core = super::kernel::get_kernel_ref()
        .map(|kernel| kernel.current_physical_core_index() as i32)
        .unwrap_or(-1);
    (tid, core)
}

/// Condition variable for thread synchronization.
///
/// Upstream class: `Kernel::KConditionVariable`
/// Holds a ThreadTree (BTreeSet-based condition variable thread tree),
/// matching upstream's `ThreadTree m_tree`.
pub struct KConditionVariable {
    waiting_threads: ConditionVariableThreadTree,
}

#[derive(Default)]
struct ConditionVariableThreadTree {
    ordered: BTreeMap<(u64, i32), Vec<u64>>,
    by_thread_id: HashMap<u64, ConditionVariableThreadKey>,
}

// ---------------------------------------------------------------------------
// Thread queue implementations matching upstream anonymous namespace classes
// ---------------------------------------------------------------------------

/// Matches upstream `ThreadQueueImplForKConditionVariableWaitForAddress`.
/// CancelWait removes the waiter from its lock owner, then delegates to base.
struct ThreadQueueImplForKConditionVariableWaitForAddress;

impl ThreadQueueImplForKConditionVariableWaitForAddress {
    fn queue() -> KThreadQueue {
        KThreadQueue::with_callbacks(None, Some(Self::cancel_wait))
    }

    /// Upstream CancelWait: removes waiter from owner, then delegates to
    /// base KThreadQueue::CancelWait (which sets RUNNABLE + clears queue).
    /// Upstream: `waiting_thread->GetLockOwner()->RemoveWaiter(waiting_thread)`.
    /// The caller (`do_task` / SVC paths) holds the scheduler lock, which
    /// serializes this raw-pointer owner access against guest fibers.
    fn cancel_wait(waiting_thread: &mut KThread) {
        if let Some(owner_ptr) = waiting_thread.get_lock_owner_raw() {
            let owner = unsafe { &mut *owner_ptr };
            owner.remove_waiter_by_thread_id(waiting_thread.thread_id);
        }
        waiting_thread.set_waiting_lock_info(None);
    }
}

/// Matches upstream `ThreadQueueImplForKConditionVariableWaitConditionVariable`.
/// CancelWait removes the waiter from its lock owner (if any), removes from
/// the condvar tree if waiting on a condition variable, then delegates to base.
struct ThreadQueueImplForKConditionVariableWaitConditionVariable;

impl ThreadQueueImplForKConditionVariableWaitConditionVariable {
    fn queue() -> KThreadQueue {
        KThreadQueue::with_callbacks(None, Some(Self::cancel_wait))
    }

    fn cancel_wait(waiting_thread: &mut KThread) {
        // Remove the thread as a waiter from its owner. Upstream:
        // `waiting_thread->GetLockOwner()->RemoveWaiter(...)` via raw KThread*;
        // safe because the caller holds the scheduler lock.
        if let Some(owner_ptr) = waiting_thread.get_lock_owner_raw() {
            let owner = unsafe { &mut *owner_ptr };
            owner.remove_waiter_by_thread_id(waiting_thread.thread_id);
        }
        waiting_thread.set_waiting_lock_info(None);

        // If the thread is waiting on a condvar, remove it from the tree.
        // Upstream: `m_tree->erase(m_tree->iterator_to(*waiting_thread))`, where
        // `m_tree` is a pointer captured when the wait began. The Rust port stores
        // the cond_var tree on `KProcess`; accessing it through `parent.lock()`
        // here reintroduces the lock-ordering bug that would deadlock CoreTiming's
        // timer thread against guest fibers. Use a raw pointer stashed by
        // `set_parent_raw_ptr()` at thread construction, so this runs without
        // re-acquiring the process mutex, matching upstream's raw-pointer access
        // under the scheduler lock.
        if waiting_thread.is_waiting_for_condition_variable() {
            if trace_ct_fire_enabled() {
                log::info!(
                    "ThreadQueueImpl CV cancel_wait tid={} before_parent",
                    waiting_thread.thread_id
                );
            }
            if let Some(parent_ptr) = waiting_thread.get_parent_raw_ptr() {
                let parent = unsafe { &mut *parent_ptr };
                parent.remove_condition_variable_waiter(waiting_thread.thread_id);
            } else if let Some(parent) = waiting_thread
                .parent
                .as_ref()
                .and_then(|parent| parent.upgrade())
            {
                parent
                    .lock()
                    .unwrap()
                    .remove_condition_variable_waiter(waiting_thread.thread_id);
            }
            waiting_thread.clear_condition_variable();
        }
    }
}

// ---------------------------------------------------------------------------
// Helper functions matching upstream anonymous namespace
// ---------------------------------------------------------------------------

/// Read a u32 from process memory.
/// Matches upstream `ReadFromUser(KernelCore&, u32*, KProcessAddress)`.
fn read_from_user(process_guard: &KProcess, address: u64) -> Option<u32> {
    if let Some(memory) = process_guard.get_memory().as_ref() {
        Some(memory.lock().unwrap().read_32(address))
    } else {
        let mem = process_guard.process_memory.read().unwrap();
        if !mem.is_valid_range(address, 4) {
            return None;
        }
        Some(mem.read_32(address))
    }
}

/// Write a u32 to process memory.
/// Matches upstream `WriteToUser(KernelCore&, KProcessAddress, const u32*)`.
fn write_to_user(process_guard: &KProcess, address: u64, value: u32) -> bool {
    if let Some(memory) = process_guard.get_memory().as_ref() {
        memory
            .lock()
            .unwrap()
            .write_32_no_rasterizer(address, value);
        true
    } else if process_guard
        .process_memory
        .read()
        .unwrap()
        .is_valid_range(address, 4)
    {
        process_guard
            .process_memory
            .write()
            .unwrap()
            .write_32(address, value);
        true
    } else {
        false
    }
}

/// Atomic update of the lock tag at `address`.
/// Matches upstream `UpdateLockAtomic(KernelCore&, u32*, KProcessAddress, u32, u32)`.
///
/// Upstream uses an ExclusiveMonitor CAS loop (ExclusiveRead32/ExclusiveWrite32).
/// Rust follows that path when the process owns an initialized exclusive
/// monitor. Bare-process unit tests can still reach this before
/// `initialize_interfaces()`, so a serialized fallback remains for those
/// non-runtime paths only.
fn update_lock_atomic(
    process_guard: &mut KProcess,
    address: u64,
    if_zero: u32,
    new_orr_mask: u32,
) -> Option<u32> {
    if let Some(monitor) = process_guard.exclusive_monitor.as_mut() {
        let current_core = super::kernel::get_kernel_ref()
            .map(|kernel| kernel.current_physical_core_index())
            .unwrap_or(0);

        loop {
            // Load the value from the address.
            let expected = monitor.exclusive_read32(current_core, address);

            // Orr in the new mask.
            let mut value = expected | new_orr_mask;

            // If the value is zero, use the if_zero value, otherwise use the newly orr'd value.
            if expected == 0 {
                value = if_zero;
            }

            // Try to store; if we fail, retry like upstream.
            if monitor.exclusive_write32(current_core, address, value) {
                return Some(expected);
            }
        }
    }

    // Fallback for tests / early bare-process setup paths that do not own an
    // initialized process-backed exclusive monitor yet.
    // Load the value from the address.
    let expected = read_from_user(process_guard, address)?;

    // Orr in the new mask.
    let mut value = expected | new_orr_mask;

    // If the value is zero, use the if_zero value, otherwise use the newly orr'd value.
    if expected == 0 {
        value = if_zero;
    }

    // Store the value.
    if !write_to_user(process_guard, address, value) {
        return None;
    }

    // Return the original value.
    Some(expected)
}

fn transfer_lock_info_to_next_owner(
    process_guard: &mut KProcess,
    next_owner_id: u64,
    lock_info: LockWithPriorityInheritanceInfo,
) -> bool {
    let Some(next_owner_thread) = process_guard.get_thread_by_thread_id(next_owner_id) else {
        return false;
    };

    let remaining_waiters = lock_info.waiter_keys();
    let is_kernel_address_key = lock_info.get_is_kernel_address_key();
    let waiter_count = lock_info.get_waiter_count();
    let next_owner_ptr = {
        let mut next_owner = next_owner_thread.lock().unwrap();
        let ptr = (&mut *next_owner as *mut KThread) as usize;
        next_owner.add_held_lock(lock_info);
        if is_kernel_address_key {
            next_owner.add_transferred_kernel_waiters(waiter_count);
        }
        ptr
    };

    for waiter in remaining_waiters {
        if let Some(waiter_thread) = process_guard.get_thread_by_thread_id(waiter.thread_id) {
            waiter_thread
                .lock()
                .unwrap()
                .set_waiting_lock_owner_thread_id(Some(next_owner_id), next_owner_ptr);
        }
    }

    true
}

// ---------------------------------------------------------------------------
// KConditionVariable implementation
// ---------------------------------------------------------------------------

impl KConditionVariable {
    pub fn new() -> Self {
        Self {
            waiting_threads: ConditionVariableThreadTree::default(),
        }
    }

    // -- Arbitration (static in upstream) --

    /// Signal to the given address, releasing the lock to the next waiter.
    /// Matches upstream `KConditionVariable::SignalToAddress(KernelCore&, KProcessAddress)`.
    ///
    /// Upstream wraps the body in `KScopedSchedulerLock sl(kernel)`.
    pub fn signal_to_address(
        process: &Arc<ProcessLock>,
        current_thread: &Arc<KThreadLock>,
        addr: u64,
    ) -> ResultCode {
        let current_thread_id = current_thread.lock().unwrap().get_thread_id();
        let trace_lock = should_trace_lock_pi(addr);
        if trace_lock {
            trace_lock_pi(
                5,
                current_thread_id,
                addr,
                0,
                0,
                0,
                current_thread_id,
                0,
                false,
                0,
                RESULT_SUCCESS,
                0,
                0,
                0,
            );
        }

        // Upstream opens `KScopedSchedulerLock sl(kernel)` at entry. The
        // kernel's scheduler_lock is a singleton — always valid once the
        // kernel is initialized. No per-thread fallback.
        let scheduler_lock = super::kernel::scheduler_lock()
            .expect("scheduler_lock must exist — kernel not initialized?");
        let _scheduler_guard = super::k_scheduler_lock::KScopedSchedulerLock::new(scheduler_lock);

        let (result, next_owner_thread) = {
            let mut process_guard = process.lock().unwrap();
            let Some(owner_thread) = process_guard.get_thread_by_thread_id(current_thread_id)
            else {
                return RESULT_INVALID_HANDLE;
            };

            // Remove waiter thread.
            // Matches upstream: owner_thread->RemoveUserWaiterByKey(&has_waiters, addr)
            let mut has_waiters = false;
            let next_owner_result = owner_thread.lock().unwrap().remove_waiter_by_key(
                KProcessAddress::new(addr),
                false, // user address key (RemoveUserWaiterByKey)
                &mut has_waiters,
            );
            cv_stats::inc(&cv_stats::UNLOCKS);
            if next_owner_result.is_none() {
                cv_stats::inc(&cv_stats::UNLOCKS_EMPTY);
            }

            // If there are remaining waiters, transfer the lock info to the next owner.
            let next_owner_thread = if let Some((next_owner_id, _priority, _, transfer_lock_info)) =
                next_owner_result
            {
                let remaining_waiter_count = transfer_lock_info
                    .as_ref()
                    .map(|lock_info| lock_info.get_waiter_count())
                    .unwrap_or(0);
                if trace_lock {
                    trace_lock_pi(
                        6,
                        current_thread_id,
                        addr,
                        0,
                        0,
                        0,
                        current_thread_id,
                        next_owner_id,
                        has_waiters,
                        0,
                        RESULT_SUCCESS,
                        0,
                        remaining_waiter_count,
                        0,
                    );
                }
                if let Some(lock_info) = transfer_lock_info {
                    if !transfer_lock_info_to_next_owner(
                        &mut process_guard,
                        next_owner_id,
                        lock_info,
                    ) {
                        return RESULT_INVALID_STATE;
                    }
                }
                if let Some(next_thread) = process_guard.get_thread_by_thread_id(next_owner_id) {
                    next_thread.lock().unwrap().set_waiting_lock_info(None);
                    Some(next_thread)
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(current_thread) = process_guard.get_thread_by_thread_id(current_thread_id) {
                current_thread.lock().unwrap().restore_priority();
            }

            // Determine the next tag.
            // Matches upstream: next_value = next_owner_thread->GetAddressKeyValue();
            //                   if (has_waiters) next_value |= HandleWaitMask;
            let next_value = if let Some(thread) = next_owner_thread.as_ref() {
                let mut next_value = thread.lock().unwrap().get_address_key_value();
                if has_waiters {
                    next_value |= HANDLE_WAIT_MASK;
                }
                next_value
            } else {
                // No next owner: upstream writes 0 (next_value is default-initialized to 0).
                0
            };

            // Synchronize memory before proceeding.
            // Matches upstream: std::atomic_thread_fence(std::memory_order_seq_cst)
            std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);

            // Write the value to userspace.
            let result = if write_to_user(&process_guard, addr, next_value) {
                RESULT_SUCCESS
            } else {
                RESULT_INVALID_CURRENT_MEMORY
            };
            if trace_lock {
                trace_lock_pi(
                    7,
                    current_thread_id,
                    addr,
                    0,
                    0,
                    0,
                    current_thread_id,
                    next_owner_thread
                        .as_ref()
                        .map(|t| t.lock().unwrap().get_thread_id())
                        .unwrap_or(0),
                    has_waiters,
                    next_value,
                    result,
                    0,
                    0,
                    0,
                );
            }

            log::trace!(
                "KConditionVariable::signal_to_address owner_tid={} addr=0x{:X} next_owner={:?} has_result={:#x} next_value=0x{:08X}",
                current_thread_id,
                addr,
                next_owner_thread
                    .as_ref()
                    .map(|t| t.lock().unwrap().get_thread_id()),
                result.get_inner_value(),
                next_value
            );

            (result, next_owner_thread)
        };

        // RUZU_TRACE_UNLOCK=1 — log every ArbitrateUnlock with the
        // recovered next_owner, so we can confirm whether the unlock
        if trace_unlock_enabled() {
            let next_id = next_owner_thread
                .as_ref()
                .map(|t| t.lock().unwrap().get_thread_id());
            log::info!(
                "[UNLOCK] tid={} addr=0x{:X} next_owner_tid={:?} result=0x{:08X}",
                current_thread_id,
                addr,
                next_id,
                result.get_inner_value(),
            );
        }

        // If necessary, signal the next owner thread.
        // Matches upstream ordering: `next_owner_thread->EndWait(result)` still runs
        // while the scheduler lock is held.
        if let Some(ref next_owner_thread) = next_owner_thread {
            if trace_lock {
                trace_lock_pi(
                    8,
                    current_thread_id,
                    addr,
                    0,
                    0,
                    0,
                    current_thread_id,
                    next_owner_thread.lock().unwrap().get_thread_id(),
                    false,
                    0,
                    result,
                    0,
                    0,
                    0,
                );
            }
            next_owner_thread
                .lock()
                .unwrap()
                .end_wait(result.get_inner_value());
        }

        result
    }

    /// Wait for the lock at the given address.
    /// Matches upstream `KConditionVariable::WaitForAddress(KernelCore&, Handle, KProcessAddress, u32)`.
    ///
    /// If the tag at `addr` equals `(handle | HANDLE_WAIT_MASK)`, the calling
    /// thread is added as a waiter on the owner thread and enters WAITING state.
    ///
    /// Upstream wraps the body in `KScopedSchedulerLock sl(kernel)`.
    pub fn wait_for_address(
        process: &Arc<ProcessLock>,
        current_thread: &Arc<KThreadLock>,
        handle: Handle,
        addr: u64,
        value: u32,
    ) -> ResultCode {
        let current_thread_id = current_thread.lock().unwrap().get_thread_id();
        let trace_lock = should_trace_lock_pi(addr);
        if trace_lock {
            trace_lock_pi(
                1,
                current_thread_id,
                addr,
                handle,
                value,
                0,
                0,
                0,
                false,
                0,
                RESULT_SUCCESS,
                0,
                0,
                0,
            );
        }

        // Upstream's `KScopedSchedulerLock sl(kernel)` scope covers ONLY
        // the "set up the wait" phase — it drops before the wait result is
        // observed again on this guest fiber.
        let setup_result: Result<(), ResultCode> = (|| {
            let scheduler_lock = super::kernel::scheduler_lock()
                .expect("scheduler_lock must exist — kernel not initialized?");
            let _scheduler_guard =
                super::k_scheduler_lock::KScopedSchedulerLock::new(scheduler_lock);

            let mut process_guard = process.lock().unwrap();

            // Check if the thread should terminate.
            if current_thread.lock().unwrap().is_termination_requested() {
                return Err(RESULT_TERMINATION_REQUESTED);
            }

            // Read the tag from userspace.
            // Matches upstream: ReadFromUser(kernel, &test_tag, addr)
            let Some(test_tag) = read_from_user(&process_guard, addr) else {
                return Err(RESULT_INVALID_CURRENT_MEMORY);
            };

            cv_stats::inc(&cv_stats::WFA);
            // If the tag isn't the handle (with wait mask), we're done.
            // Matches upstream: R_SUCCEED_IF(test_tag != (handle | HandleWaitMask))
            if test_tag != (handle | HANDLE_WAIT_MASK) {
                cv_stats::inc(&cv_stats::WFA_IMMEDIATE);
                if trace_lock {
                    trace_lock_pi(
                        2,
                        current_thread_id,
                        addr,
                        handle,
                        value,
                        test_tag,
                        0,
                        0,
                        false,
                        0,
                        RESULT_SUCCESS,
                        0,
                        0,
                        0,
                    );
                }
                log::trace!(
                    "KConditionVariable::wait_for_address no_wait tid={} handle=0x{:08X} addr=0x{:X} value=0x{:08X} test_tag=0x{:08X}",
                    current_thread_id,
                    handle,
                    addr,
                    value,
                    test_tag
                );
                return Err(RESULT_SUCCESS);
            }

            // Get the lock owner thread.
            // Matches upstream: GetCurrentProcess(kernel).GetHandleTable()
            //                      .GetObjectWithoutPseudoHandle<KThread>(handle)
            let Some(owner_object_id) = process_guard.handle_table.get_object(handle) else {
                if trace_lock {
                    trace_lock_pi(
                        3,
                        current_thread_id,
                        addr,
                        handle,
                        value,
                        test_tag,
                        0,
                        0,
                        false,
                        0,
                        RESULT_INVALID_HANDLE,
                        0,
                        0,
                        0,
                    );
                }
                return Err(RESULT_INVALID_HANDLE);
            };
            let Some(owner_thread) = process_guard.get_thread_by_object_id(owner_object_id) else {
                if trace_lock {
                    trace_lock_pi(
                        3,
                        current_thread_id,
                        addr,
                        handle,
                        value,
                        test_tag,
                        0,
                        0,
                        false,
                        0,
                        RESULT_INVALID_HANDLE,
                        owner_object_id,
                        0,
                        1,
                    );
                }
                return Err(RESULT_INVALID_HANDLE);
            };
            let owner_thread_id = owner_thread.lock().unwrap().get_thread_id();
            if trace_lock {
                trace_lock_pi(
                    3,
                    current_thread_id,
                    addr,
                    handle,
                    value,
                    test_tag,
                    owner_thread_id,
                    0,
                    false,
                    0,
                    RESULT_SUCCESS,
                    owner_object_id,
                    0,
                    0,
                );
                if owner_thread_id == current_thread_id {
                    trace_lock_pi(
                        17,
                        current_thread_id,
                        addr,
                        handle,
                        value,
                        test_tag,
                        owner_thread_id,
                        0,
                        (test_tag & HANDLE_WAIT_MASK) != 0,
                        0,
                        RESULT_SUCCESS,
                        owner_object_id,
                        0,
                        0,
                    );
                }
            }

            // Match upstream sequence (k_condition_variable.cpp:179-184):
            //   cur_thread->SetUserAddressKey(addr, value);
            //   owner_thread->AddWaiter(cur_thread);
            //   cur_thread->BeginWait(&wait_queue);
            //   cur_thread->SetWaitReasonForDebugging(ConditionVar);
            // Tree insert (AddWaiter) MUST happen before BeginWait flips state
            // to WAITING — same upstream-order invariant as wait_locked
            // (CLAUDE.md Rule G).
            let (priority, address_key, is_kernel) = {
                let mut ct = current_thread.lock().unwrap();
                ct.set_user_address_key(KProcessAddress::new(addr), value);
                (
                    ct.get_priority(),
                    ct.get_address_key(),
                    ct.get_is_kernel_address_key(),
                )
            };
            debug_assert_eq!(priority, current_thread.lock().unwrap().get_priority());
            debug_assert_eq!(
                address_key,
                current_thread.lock().unwrap().get_address_key()
            );
            debug_assert_eq!(
                is_kernel,
                current_thread.lock().unwrap().get_is_kernel_address_key()
            );
            KThread::add_waiter_with_process(&mut process_guard, &owner_thread, current_thread);
            if trace_lock {
                trace_lock_pi(
                    4,
                    current_thread_id,
                    addr,
                    handle,
                    value,
                    test_tag,
                    owner_thread_id,
                    0,
                    false,
                    0,
                    RESULT_SUCCESS,
                    owner_object_id,
                    0,
                    0,
                );
            }
            Self::begin_wait_for_address(current_thread);

            log::trace!(
                "KConditionVariable::wait_for_address sleep tid={} handle=0x{:08X} addr=0x{:X} value=0x{:08X}",
                current_thread_id,
                handle,
                addr,
                value
            );

            // Upstream calls owner_thread->Close() here to release the handle
            // reference. In Rust, Arc reference counting handles this automatically.
            drop(process_guard);

            Ok(())
        })();

        if let Err(early_return) = setup_result {
            return early_return;
        }

        let wait_result = current_thread.lock().unwrap().get_wait_result();
        log::trace!(
            "KConditionVariable::wait_for_address woke tid={} handle=0x{:08X} addr=0x{:X} wait_result={:#x}",
            current_thread_id,
            handle,
            addr,
            wait_result
        );
        ResultCode::new(wait_result)
    }

    // -- Condition variable --

    /// Signal up to `count` threads waiting on the given condition variable key.
    /// Matches upstream `KConditionVariable::Signal(u64 cv_key, s32 count)`.
    ///
    /// The caller must already hold the process lock (passes `&mut KProcess`).
    /// Upstream wraps the body in `KScopedSchedulerLock sl(m_kernel)` — we do
    /// the same via `kernel::scheduler_lock()`.
    pub fn signal(&mut self, process_guard: &mut KProcess, cv_key: u64, count: i32) -> ResultCode {
        let scheduler_lock = super::kernel::scheduler_lock()
            .expect("scheduler_lock must exist — kernel not initialized?");
        let _scheduler_guard = super::k_scheduler_lock::KScopedSchedulerLock::new(scheduler_lock);

        let trace_this_key = should_trace_cv_key(cv_key);
        if trace_this_key {
            let (issuer_tid, issuer_core) = current_trace_owner();
            let waiters: Vec<u64> = self
                .waiting_threads
                .ordered
                .iter()
                .filter(|((k, _), _)| *k == cv_key)
                .flat_map(|(_, ids)| ids.iter().copied())
                .collect();
            log::info!(
                "[CV_KEY_TRACE] signal_enter cv_key=0x{:X} count={} issuer_tid={:?} issuer_core={} waiters_in_tree={:?}",
                cv_key,
                count,
                issuer_tid,
                issuer_core,
                waiters
            );
        }

        let mut num_waiters = 0i32;
        log::trace!(
            "KConditionVariable::signal begin cv_key=0x{:X} count={} first_waiter={:?}",
            cv_key,
            count,
            self.waiting_threads
                .nfind_key(cv_key)
                .map(|key| key.thread_id)
        );

        // Iterate the tree, finding threads with matching cv_key.
        // Matches upstream: auto it = m_tree.nfind_key({cv_key, -1});
        while count <= 0 || num_waiters < count {
            let Some(waiting_thread_key) = self.waiting_threads.nfind_key(cv_key) else {
                break;
            };
            if waiting_thread_key.cv_key != cv_key {
                break;
            }

            // Kernel invariant: a cv-tree entry references a live thread
            // whose own condvar_key equals the tree-key's cv_key. The tree is
            // only inserted under scheduler lock by wait_locked (after
            // SetConditionVariable) and only erased here or in cancel paths
            // that also clear the thread's state. Upstream dereferences
            // `*it` unconditionally (k_condition_variable.cpp:252); we match
            // that and panic on invariant break instead of silently dropping
            // the signal.
            let waiting_thread_id = waiting_thread_key.thread_id;
            let waiting_thread = process_guard
                .get_thread_by_thread_id(waiting_thread_id)
                .expect(
                    "KConditionVariable::signal: cv tree references unknown thread \
                     (kernel invariant violated — thread cleanup missed tree entry)",
                );
            debug_assert_eq!(
                waiting_thread.lock().unwrap().get_condition_variable_key(),
                cv_key,
                "cv tree key and thread condvar_key out of sync",
            );

            // Remove from tree and clear condvar state.
            // Matches upstream: it = m_tree.erase(it);
            //                   target_thread->ClearConditionVariable();
            self.waiting_threads.erase(waiting_thread_key);
            waiting_thread.lock().unwrap().clear_condition_variable();

            // Signal the thread.
            // Matches upstream: this->SignalImpl(target_thread);
            self.signal_impl(process_guard, &waiting_thread);
            if trace_this_key {
                let woke_state = waiting_thread.lock().unwrap().get_state();
                log::info!(
                    "[CV_KEY_TRACE] signal_impl_done cv_key=0x{:X} waiter_tid={} new_state={:?}",
                    cv_key,
                    waiting_thread_id,
                    woke_state
                );
            }
            let (issuer_tid, issuer_core) = current_trace_owner();
            trace_kcv(format_args!(
                "KCV::signal issuer_tid={:?} issuer_core={} cv_key=0x{:X} woke_tid={} remaining_first={:?}",
                issuer_tid,
                issuer_core,
                cv_key,
                waiting_thread_id,
                self.waiting_threads.nfind_key(cv_key).map(|k| k.thread_id)
            ));
            log::trace!(
                "KConditionVariable::signal woke waiter tid={} cv_key=0x{:X}",
                waiting_thread_id,
                cv_key
            );

            num_waiters += 1;
        }

        cv_stats::inc(&cv_stats::SIGNALS);
        if num_waiters == 0 {
            cv_stats::inc(&cv_stats::SIGNALS_EMPTY);
            // Diagnose why the guest syscalled with no kernel waiters: read
            // the guest-visible cv word. 0 → the guest ignored the
            // has-waiters hint (SDK behavior); non-zero → our clear is not
            // landing (ruzu bug).
            match read_from_user(process_guard, cv_key) {
                Some(0) => cv_stats::inc(&cv_stats::SIGNALS_EMPTY_HINT0),
                Some(_) => cv_stats::inc(&cv_stats::SIGNALS_EMPTY_HINT_SET),
                None => {}
            }
        }
        cv_stats::SIGNALED_WAITERS
            .fetch_add(num_waiters as u64, std::sync::atomic::Ordering::Relaxed);

        // If we have no waiters left for this key, clear the has waiter flag.
        // Matches upstream: if (it == m_tree.end() || it->GetConditionVariableKey() != cv_key)
        //                       WriteToUser(m_kernel, cv_key, &has_waiter_flag);
        if !matches!(
            self.waiting_threads.nfind_key(cv_key),
            Some(key) if key.cv_key == cv_key
        ) {
            write_to_user(process_guard, cv_key, 0);
            let (issuer_tid, issuer_core) = current_trace_owner();
            trace_kcv(format_args!(
                "KCV::signal issuer_tid={:?} issuer_core={} cv_key=0x{:X} clear_has_waiter_flag no_remaining_waiters",
                issuer_tid, issuer_core, cv_key,
            ));
        }

        RESULT_SUCCESS
    }

    /// Wait on the condition variable.
    /// Matches upstream `KConditionVariable::Wait(KProcessAddress addr, u64 key, u32 value, s64 timeout)`.
    pub fn wait(
        &mut self,
        process: &Arc<ProcessLock>,
        current_thread: &Arc<KThreadLock>,
        addr: u64,
        key: u64,
        value: u32,
        timeout: i64,
    ) -> ResultCode {
        let current_thread_id = current_thread.lock().unwrap().get_thread_id();
        // Upstream opens `KScopedSchedulerLockAndSleep slp(kernel, ...)` at
        // entry, which internally acquires the kernel's scheduler lock.
        let scheduler_lock = super::kernel::scheduler_lock()
            .expect("scheduler_lock must exist — kernel not initialized?");
        let hardware_timer = super::kernel::get_hardware_timer_arc();
        let thread_ptr = {
            let guard = current_thread.lock().unwrap();
            &*guard as *const super::k_thread::KThread as usize
        };

        let result = {
            let (mut sleep_guard, timer) =
                super::k_scoped_scheduler_lock_and_sleep::KScopedSchedulerLockAndSleep::new(
                    scheduler_lock,
                    hardware_timer.as_ref(),
                    current_thread_id,
                    thread_ptr,
                    timeout,
                );
            let mut process_guard = process.lock().unwrap();
            self.wait_locked_after_sleep_guard(
                &mut process_guard,
                current_thread,
                addr,
                key,
                value,
                timeout,
                &mut sleep_guard,
                timer,
            )
        };

        if result == RESULT_SUCCESS {
            // KScopedSchedulerLockAndSleep drops here. Upstream returns to the
            // physical-core scheduler after BeginWait. The CPU-manager
            // post-SVC path observes ThreadState::WAITING and performs the
            // core handoff; PhysicalCore defers SVC return registers until the
            // wait is completed.
            ResultCode::new(current_thread.lock().unwrap().get_wait_result())
        } else {
            result
        }
    }

    /// Internal wait implementation, called when the process lock is already held.
    /// Matches upstream `KConditionVariable::Wait()` body, which wraps in
    /// `KScopedSchedulerLockAndSleep`.
    pub fn wait_locked(
        &mut self,
        process_guard: &mut KProcess,
        current_thread: &Arc<KThreadLock>,
        addr: u64,
        key: u64,
        value: u32,
        timeout: i64,
    ) -> ResultCode {
        let current_thread_id = current_thread.lock().unwrap().get_thread_id();
        // Upstream: `KScopedSchedulerLockAndSleep slp(kernel, ...)` — kernel
        // singleton's scheduler_lock, not a per-thread cache.
        let scheduler_lock = super::kernel::scheduler_lock()
            .expect("scheduler_lock must exist — kernel not initialized?");
        let hardware_timer = super::kernel::get_hardware_timer_arc();
        let thread_ptr = {
            let guard = current_thread.lock().unwrap();
            &*guard as *const super::k_thread::KThread as usize
        };

        log::trace!(
            "KConditionVariable::wait_locked tid={} before scoped_sleep_lock addr=0x{:X} key=0x{:X}",
            current_thread_id,
            addr,
            key
        );

        let (mut sleep_guard, timer) =
            super::k_scoped_scheduler_lock_and_sleep::KScopedSchedulerLockAndSleep::new(
                scheduler_lock,
                hardware_timer.as_ref(),
                current_thread_id,
                thread_ptr,
                timeout,
            );
        log::trace!(
            "KConditionVariable::wait_locked tid={} after scoped_sleep_lock",
            current_thread_id
        );

        self.wait_locked_after_sleep_guard(
            process_guard,
            current_thread,
            addr,
            key,
            value,
            timeout,
            &mut sleep_guard,
            timer,
        )
    }

    pub(crate) fn wait_locked_after_sleep_guard(
        &mut self,
        process_guard: &mut KProcess,
        current_thread: &Arc<KThreadLock>,
        addr: u64,
        key: u64,
        value: u32,
        timeout: i64,
        sleep_guard: &mut super::k_scoped_scheduler_lock_and_sleep::KScopedSchedulerLockAndSleep<
            '_,
        >,
        timer: Option<Arc<super::k_hardware_timer::KHardwareTimer>>,
    ) -> ResultCode {
        let current_thread_id = current_thread.lock().unwrap().get_thread_id();
        let mut wait_queue = ThreadQueueImplForKConditionVariableWaitConditionVariable::queue();
        let trace_lock = should_trace_lock_pi(addr);

        // Check that the thread isn't terminating.
        // Matches upstream: if (cur_thread->IsTerminationRequested()) { slp.CancelSleep(); ... }
        if current_thread.lock().unwrap().is_termination_requested() {
            sleep_guard.cancel_sleep();
            return RESULT_TERMINATION_REQUESTED;
        }

        // Update the value and process for the next owner.
        // Matches upstream block inside the KScopedSchedulerLockAndSleep scope.
        {
            // Remove waiter thread.
            // Matches upstream: cur_thread->RemoveUserWaiterByKey(&has_waiters, addr)
            let mut has_waiters = false;
            let next_owner_result = current_thread.lock().unwrap().remove_waiter_by_key(
                KProcessAddress::new(addr),
                false, // user address key
                &mut has_waiters,
            );
            log::trace!(
                "KConditionVariable::wait_locked tid={} after remove_waiter_by_key next_owner={:?} has_waiters={}",
                current_thread_id,
                next_owner_result.as_ref().map(|(tid, _, _, _)| *tid),
                has_waiters
            );

            // Update for the next owner thread.
            // Matches upstream: next_value = next_owner_thread->GetAddressKeyValue();
            let mut next_value = 0u32;
            let mut trace_next_owner_id = 0u64;
            if let Some((next_owner_thread_id, _priority, _, transfer_lock_info)) =
                next_owner_result
            {
                trace_next_owner_id = next_owner_thread_id;
                let Some(next_owner_thread) =
                    process_guard.get_thread_by_thread_id(next_owner_thread_id)
                else {
                    sleep_guard.cancel_sleep();
                    return RESULT_INVALID_STATE;
                };

                if let Some(lock_info) = transfer_lock_info {
                    if !transfer_lock_info_to_next_owner(
                        process_guard,
                        next_owner_thread_id,
                        lock_info,
                    ) {
                        sleep_guard.cancel_sleep();
                        return RESULT_INVALID_STATE;
                    }
                }

                next_owner_thread
                    .lock()
                    .unwrap()
                    .set_waiting_lock_info(None);

                if let Some(current_thread) =
                    process_guard.get_thread_by_thread_id(current_thread_id)
                {
                    current_thread.lock().unwrap().restore_priority();
                }

                next_value = next_owner_thread.lock().unwrap().get_address_key_value();
                if has_waiters {
                    next_value |= HANDLE_WAIT_MASK;
                }

                if trace_lock {
                    trace_lock_pi(
                        9,
                        current_thread_id,
                        addr,
                        0,
                        value,
                        0,
                        current_thread_id,
                        next_owner_thread_id,
                        has_waiters,
                        next_value,
                        RESULT_SUCCESS,
                        0,
                        0,
                        key,
                    );
                }

                // Wake up the next owner.
                // Matches upstream: next_owner_thread->EndWait(ResultSuccess);
                next_owner_thread
                    .lock()
                    .unwrap()
                    .end_wait(RESULT_SUCCESS.get_inner_value());
            }

            // Write to the cv key.
            // Matches upstream: WriteToUser(m_kernel, key, &has_waiter_flag);
            //                   std::atomic_thread_fence(std::memory_order_seq_cst);
            if !write_to_user(process_guard, key, 1) {
                sleep_guard.cancel_sleep();
                return RESULT_INVALID_CURRENT_MEMORY;
            }
            if trace_lock {
                trace_lock_pi(
                    10,
                    current_thread_id,
                    addr,
                    0,
                    value,
                    0,
                    current_thread_id,
                    trace_next_owner_id,
                    has_waiters,
                    1,
                    RESULT_SUCCESS,
                    0,
                    0,
                    key,
                );
            }
            std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
            log::trace!(
                "KConditionVariable::wait_locked tid={} after write_to_user key",
                current_thread_id
            );

            // Write the value to userspace.
            // Matches upstream: WriteToUser(m_kernel, addr, &next_value)
            if !write_to_user(process_guard, addr, next_value) {
                sleep_guard.cancel_sleep();
                return RESULT_INVALID_CURRENT_MEMORY;
            }
            if trace_lock {
                trace_lock_pi(
                    11,
                    current_thread_id,
                    addr,
                    0,
                    value,
                    0,
                    current_thread_id,
                    trace_next_owner_id,
                    has_waiters,
                    next_value,
                    RESULT_SUCCESS,
                    0,
                    0,
                    key,
                );
            }
            log::trace!(
                "KConditionVariable::wait_locked tid={} after write_to_user addr next_value=0x{:08X}",
                current_thread_id,
                next_value
            );
        }

        // If timeout is zero, time out.
        // Matches upstream: R_UNLESS(timeout != 0, ResultTimedOut);
        cv_stats::inc(&cv_stats::WAITS);
        if timeout == 0 {
            cv_stats::inc(&cv_stats::WAITS_TIMEOUT0);
            sleep_guard.cancel_sleep();
            return crate::hle::kernel::svc::svc_results::RESULT_TIMED_OUT;
        }

        // Match upstream `KConditionVariable::Wait` tail (k_condition_variable.cpp:324-330):
        //   cur_thread->SetConditionVariable(&m_tree, addr, key, value);
        //   m_tree.insert(*cur_thread);
        //   wait_queue.SetHardwareTimer(timer);
        //   cur_thread->BeginWait(&wait_queue);
        //   cur_thread->SetWaitReasonForDebugging(ConditionVar);
        // Tree insert MUST happen before BeginWait flips state to WAITING —
        // every step is under the same KScopedSchedulerLockAndSleep, so a
        // concurrent signal cannot observe the intermediate states, but the
        // upstream order is preserved literally per CLAUDE.md Rule G.
        let thread_key = {
            let mut t = current_thread.lock().unwrap();
            t.set_condition_variable(KProcessAddress::new(addr), key, value);
            t.condition_variable_tree_key()
        };
        self.waiting_threads.insert(thread_key);
        if trace_lock {
            trace_lock_pi(
                12,
                current_thread_id,
                addr,
                0,
                value,
                0,
                0,
                0,
                false,
                0,
                RESULT_SUCCESS,
                0,
                0,
                key,
            );
        }
        if should_trace_cv_key(key) {
            log::info!(
                "[CV_KEY_TRACE] wait_insert cv_key=0x{:X} tid={} addr=0x{:X} tag=0x{:08X} timeout={}",
                key,
                current_thread_id,
                addr,
                value,
                timeout
            );
        }

        if let Some(timer) = timer {
            wait_queue.set_hardware_timer(timer);
        }

        {
            let mut t = current_thread.lock().unwrap();
            t.begin_wait_with_queue(wait_queue);
            t.set_wait_reason_for_debugging(ThreadWaitReasonForDebugging::ConditionVar);
        }

        trace_kcv(format_args!(
            "KCV::wait enqueue tid={} addr=0x{:X} key=0x{:X} tag=0x{:08X} timeout={} bucket_count={}",
            current_thread_id,
            addr,
            key,
            value,
            timeout,
            self.waiting_threads.bucket_count()
        ));
        log::trace!(
            "KConditionVariable::wait_locked enqueued tid={} addr=0x{:X} key=0x{:X} tag=0x{:08X} timeout={}",
            current_thread_id,
            addr,
            key,
            value,
            timeout
        );

        RESULT_SUCCESS
    }

    /// Core signal implementation.
    /// Matches upstream `KConditionVariable::SignalImpl(KThread* thread)`.
    ///
    /// Uses UpdateLockAtomic to atomically update the lock tag, then either
    /// wakes the thread (if nobody held the lock) or adds it as a waiter on
    /// the previous lock owner.
    fn signal_impl(&mut self, process_guard: &mut KProcess, waiting_thread: &Arc<KThreadLock>) {
        // Update the tag.
        // Matches upstream: KProcessAddress address = thread->GetAddressKey();
        //                   u32 own_tag = thread->GetAddressKeyValue();
        let (address, own_tag) = {
            let wt = waiting_thread.lock().unwrap();
            (wt.get_address_key(), wt.get_address_key_value())
        };
        let trace_lock = should_trace_lock_pi(address.get());

        // UpdateLockAtomic: atomically read-modify-write the lock tag.
        // Matches upstream: UpdateLockAtomic(m_kernel, &prev_tag, address, own_tag, HandleWaitMask)
        // TODO(upstream): CanAccessAtomic(..) check is also a TODO upstream.
        let can_access = true;
        let prev_tag = if can_access {
            update_lock_atomic(process_guard, address.get(), own_tag, HANDLE_WAIT_MASK)
        } else {
            None
        };

        let waiting_thread_id = waiting_thread.lock().unwrap().get_thread_id();
        if trace_lock {
            trace_lock_pi(
                13,
                waiting_thread_id,
                address.get(),
                own_tag,
                own_tag,
                prev_tag.unwrap_or(0),
                0,
                0,
                false,
                0,
                RESULT_SUCCESS,
                0,
                0,
                0,
            );
        }
        log::trace!(
            "KConditionVariable::signal_impl tid={} addr=0x{:X} own_tag=0x{:08X} prev_tag={:?}",
            waiting_thread_id,
            address.get(),
            own_tag,
            prev_tag
        );

        if let Some(prev_tag) = prev_tag {
            if should_trace_kcv_wake() {
                log::info!(
                    "KCV_WAKE signal_impl waiter_tid={} addr=0x{:X} own_tag=0x{:08X} prev_tag=0x{:08X}",
                    waiting_thread_id,
                    address.get(),
                    own_tag,
                    prev_tag,
                );
            }
            if prev_tag == INVALID_HANDLE {
                cv_stats::inc(&cv_stats::SIGNAL_WAKE_DIRECT);
                // If nobody held the lock previously, we're all good.
                // Matches upstream: thread->EndWait(ResultSuccess);
                if trace_lock {
                    trace_lock_pi(
                        14,
                        waiting_thread_id,
                        address.get(),
                        own_tag,
                        own_tag,
                        prev_tag,
                        0,
                        0,
                        false,
                        0,
                        RESULT_SUCCESS,
                        0,
                        0,
                        0,
                    );
                }
                if should_trace_kcv_wake() {
                    log::info!(
                        "KCV_WAKE immediate_end_wait waiter_tid={} addr=0x{:X}",
                        waiting_thread_id,
                        address.get(),
                    );
                }
                waiting_thread
                    .lock()
                    .unwrap()
                    .end_wait(RESULT_SUCCESS.get_inner_value());
            } else {
                // Get the previous owner.
                // Matches upstream: GetObjectWithoutPseudoHandle<KThread>(prev_tag & ~HandleWaitMask)
                let owner_handle = (prev_tag & !HANDLE_WAIT_MASK) as Handle;
                let owner_thread = process_guard
                    .handle_table
                    .get_object(owner_handle)
                    .and_then(|obj_id| process_guard.get_thread_by_object_id(obj_id));

                if let Some(owner_thread) = owner_thread {
                    // Add the thread as a waiter on the owner.
                    // Matches upstream: owner_thread->AddWaiter(thread);
                    //                   owner_thread->Close();
                    let wt = waiting_thread.lock().unwrap();
                    let wt_id = wt.get_thread_id();
                    let wt_priority = wt.get_priority();
                    let wt_address_key = wt.get_address_key();
                    let wt_is_kernel = wt.get_is_kernel_address_key();
                    drop(wt);

                    let owner_id = owner_thread.lock().unwrap().get_thread_id();
                    if trace_lock {
                        trace_lock_pi(
                            15,
                            wt_id,
                            address.get(),
                            own_tag,
                            own_tag,
                            prev_tag,
                            owner_id,
                            wt_id,
                            (prev_tag & HANDLE_WAIT_MASK) != 0,
                            0,
                            RESULT_SUCCESS,
                            0,
                            0,
                            0,
                        );
                    }
                    if should_trace_kcv_wake() {
                        log::info!(
                            "KCV_WAKE requeue_waiter waiter_tid={} owner_tid={} addr=0x{:X} prev_tag=0x{:08X}",
                            wt_id,
                            owner_id,
                            wt_address_key.get(),
                            prev_tag,
                        );
                    }

                    cv_stats::inc(&cv_stats::SIGNAL_REQUEUE_ON_OWNER);
                    // Matches upstream `owner_thread->AddWaiter(thread)`
                    // (k_condition_variable.cpp:230): KThread::add_waiter
                    // now sets the waiter's lock-owner back-pointer too.
                    debug_assert_eq!(wt_id, waiting_thread.lock().unwrap().get_thread_id());
                    debug_assert_eq!(wt_priority, waiting_thread.lock().unwrap().get_priority());
                    debug_assert_eq!(
                        wt_address_key,
                        waiting_thread.lock().unwrap().get_address_key()
                    );
                    debug_assert_eq!(
                        wt_is_kernel,
                        waiting_thread.lock().unwrap().get_is_kernel_address_key()
                    );
                    KThread::add_waiter_with_process(process_guard, &owner_thread, waiting_thread);
                } else {
                    // The lock was tagged with a thread that doesn't exist.
                    // Matches upstream: thread->EndWait(ResultInvalidState);
                    if trace_lock {
                        trace_lock_pi(
                            16,
                            waiting_thread_id,
                            address.get(),
                            own_tag,
                            own_tag,
                            prev_tag,
                            0,
                            waiting_thread_id,
                            (prev_tag & HANDLE_WAIT_MASK) != 0,
                            0,
                            RESULT_INVALID_STATE,
                            0,
                            0,
                            0,
                        );
                    }
                    waiting_thread
                        .lock()
                        .unwrap()
                        .end_wait(RESULT_INVALID_STATE.get_inner_value());
                }
            }
        } else {
            // If the address wasn't accessible, note so.
            // Matches upstream: thread->EndWait(ResultInvalidCurrentMemory);
            if trace_lock {
                trace_lock_pi(
                    16,
                    waiting_thread_id,
                    address.get(),
                    own_tag,
                    own_tag,
                    0,
                    0,
                    waiting_thread_id,
                    false,
                    0,
                    RESULT_INVALID_CURRENT_MEMORY,
                    0,
                    0,
                    1,
                );
            }
            waiting_thread
                .lock()
                .unwrap()
                .end_wait(RESULT_INVALID_CURRENT_MEMORY.get_inner_value());
        }
    }

    /// Remove a thread from the condition variable tree before updating its priority.
    /// Matches upstream free function `BeforeUpdatePriority(kernel, tree, thread)`.
    pub fn before_update_priority(&mut self, thread_id: u64) {
        self.waiting_threads.erase_by_thread_id(thread_id);
    }

    /// Re-insert a thread into the condition variable tree after updating its priority.
    /// Matches upstream free function `AfterUpdatePriority(kernel, tree, thread)`.
    pub fn after_update_priority(&mut self, thread_key: ConditionVariableThreadKey) {
        if thread_key.cv_key != 0 {
            self.waiting_threads.insert(thread_key);
        }
    }

    /// Remove a waiter from the condition variable tree.
    pub fn remove_waiter(&mut self, thread_id: u64) {
        self.waiting_threads.erase_by_thread_id(thread_id);
    }

    /// Get all waiting thread IDs in tree order.
    pub fn waiting_thread_ids(&self) -> Vec<u64> {
        self.waiting_threads.ordered_thread_ids()
    }

    /// Set up a thread to wait for an address lock.
    /// Matches upstream logic inside WaitForAddress:
    ///   cur_thread->SetUserAddressKey(addr, value);
    ///   owner_thread->AddWaiter(cur_thread);
    ///   cur_thread->BeginWait(&wait_queue);
    /// Begin-wait tail for `wait_for_address`, matching upstream
    /// `KConditionVariable::WaitForAddress` (k_condition_variable.cpp:182-184):
    ///   cur_thread->BeginWait(&wait_queue);
    ///   cur_thread->SetWaitReasonForDebugging(ConditionVar);
    ///
    /// The lock-owner back-pointer is no longer set here — `KThread::add_waiter`
    /// (called by the caller right before this) sets it as part of upstream's
    /// `AddWaiter` → `lock_info->AddWaiter` → `waiter->SetWaitingLockInfo(this)`
    /// chain. The `set_user_address_key` step is also pulled up to the caller
    /// to match upstream's exact sequencing.
    fn begin_wait_for_address(current_thread: &Arc<KThreadLock>) {
        let mut current_thread = current_thread.lock().unwrap();
        current_thread
            .begin_wait_with_queue(ThreadQueueImplForKConditionVariableWaitForAddress::queue());
        current_thread.set_wait_reason_for_debugging(ThreadWaitReasonForDebugging::ConditionVar);
    }
}

// ---------------------------------------------------------------------------
// ConditionVariableThreadTree — BTreeSet-based tree matching upstream ThreadTree
// ---------------------------------------------------------------------------

impl ConditionVariableThreadTree {
    fn insert(&mut self, key: ConditionVariableThreadKey) {
        if let Some(existing) = self.by_thread_id.insert(key.thread_id, key) {
            self.remove_from_bucket(existing.cv_key, existing.priority, existing.thread_id);
        }

        self.ordered
            .entry((key.cv_key, key.priority))
            .or_default()
            .push(key.thread_id);
    }

    fn erase(&mut self, key: ConditionVariableThreadKey) {
        self.erase_by_thread_id(key.thread_id);
    }

    fn erase_by_thread_id(&mut self, thread_id: u64) {
        if let Some(existing) = self.by_thread_id.remove(&thread_id) {
            self.remove_from_bucket(existing.cv_key, existing.priority, thread_id);
        }
    }

    /// Find the first entry with cv_key >= the given key.
    /// Matches upstream `m_tree.nfind_key({cv_key, -1})`.
    fn nfind_key(&self, cv_key: u64) -> Option<ConditionVariableThreadKey> {
        self.ordered
            .range((cv_key, i32::MIN)..)
            .find_map(|(_, thread_ids)| {
                thread_ids
                    .first()
                    .and_then(|thread_id| self.by_thread_id.get(thread_id))
                    .copied()
            })
    }

    fn ordered_thread_ids(&self) -> Vec<u64> {
        self.ordered
            .values()
            .flat_map(|thread_ids| thread_ids.iter().copied())
            .collect()
    }

    fn bucket_count(&self) -> usize {
        self.ordered.len()
    }

    fn remove_from_bucket(&mut self, cv_key: u64, priority: i32, thread_id: u64) {
        let bucket_key = (cv_key, priority);
        let should_remove_bucket = if let Some(thread_ids) = self.ordered.get_mut(&bucket_key) {
            if let Some(position) = thread_ids
                .iter()
                .position(|existing| *existing == thread_id)
            {
                thread_ids.remove(position);
            }
            thread_ids.is_empty()
        } else {
            false
        };

        if should_remove_bucket {
            self.ordered.remove(&bucket_key);
        }
    }
}

impl Default for KConditionVariable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::System;
    use crate::hle::kernel::k_scheduler::KScheduler;
    use crate::hle::kernel::k_thread::{ConditionVariableTreeState, ThreadState};
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    fn ensure_scheduler_lock_for_test() {
        static INIT: OnceLock<()> = OnceLock::new();
        INIT.get_or_init(|| {
            let mut system = System::new_for_test();
            system.initialize();
            system
                .kernel_mut()
                .expect("test system must own a kernel")
                .initialize();
            Box::leak(Box::new(system));
        });
    }

    fn setup_threads() -> (
        Arc<ProcessLock>,
        Arc<KThreadLock>,
        Arc<KThreadLock>,
        Handle,
        u64,
    ) {
        ensure_scheduler_lock_for_test();

        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let scheduler = Arc::new(Mutex::new(KScheduler::new(0)));
        scheduler.lock().unwrap().initialize(1, 0, 0);
        {
            let mut process_guard = process.lock().unwrap();
            process_guard.attach_scheduler(&scheduler);
            process_guard.bind_self_reference(&process);
            process_guard.initialize_handle_table();
            process_guard.allocate_code_memory(0x1000, 0x4000);
        }

        let owner = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut owner_guard = owner.lock().unwrap();
            owner_guard.object_id = 10;
            owner_guard.thread_id = 1;
            owner_guard.parent = Some(Arc::downgrade(&process));
            owner_guard.scheduler = Some(Arc::downgrade(&scheduler));
            owner_guard.set_state(ThreadState::RUNNABLE);
        }

        let waiter = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut waiter_guard = waiter.lock().unwrap();
            waiter_guard.object_id = 11;
            waiter_guard.thread_id = 2;
            waiter_guard.parent = Some(Arc::downgrade(&process));
            waiter_guard.scheduler = Some(Arc::downgrade(&scheduler));
            waiter_guard.set_state(ThreadState::RUNNABLE);
        }

        let owner_handle = {
            let mut process_guard = process.lock().unwrap();
            process_guard.register_thread_object(owner.clone());
            process_guard.register_thread_object(waiter.clone());
            process_guard.handle_table.add(10).unwrap()
        };

        (process, owner, waiter, owner_handle, 0x1800)
    }

    #[test]
    fn wait_for_address_enqueues_waiter_on_owner_thread() {
        let (process, owner, waiter, owner_handle, address) = setup_threads();
        process
            .lock()
            .unwrap()
            .process_memory
            .write()
            .unwrap()
            .write_32(address, owner_handle | HANDLE_WAIT_MASK);

        let result =
            KConditionVariable::wait_for_address(&process, &waiter, owner_handle, address, 0x1234);

        assert_eq!(
            result.get_inner_value(),
            waiter.lock().unwrap().get_wait_result()
        );
        assert_eq!(waiter.lock().unwrap().get_state(), ThreadState::WAITING);
        assert_eq!(
            waiter.lock().unwrap().get_wait_reason_for_debugging(),
            ThreadWaitReasonForDebugging::ConditionVar
        );
        assert!(
            !waiter.lock().unwrap().is_waiting_for_address_arbiter(),
            "WaitForAddress should not set address-arbiter tree state on this path"
        );
        assert_eq!(owner.lock().unwrap().waiter_thread_ids(), vec![2]);

        assert_eq!(
            KConditionVariable::signal_to_address(&process, &owner, address),
            RESULT_SUCCESS
        );
        assert_eq!(waiter.lock().unwrap().get_state(), ThreadState::RUNNABLE);
        assert_eq!(
            waiter.lock().unwrap().get_wait_result(),
            RESULT_SUCCESS.get_inner_value()
        );
    }

    #[test]
    fn wait_for_address_defers_success_result_until_signal() {
        let (process, owner, waiter, owner_handle, address) = setup_threads();
        process
            .lock()
            .unwrap()
            .process_memory
            .write()
            .unwrap()
            .write_32(address, owner_handle | HANDLE_WAIT_MASK);

        let result =
            KConditionVariable::wait_for_address(&process, &waiter, owner_handle, address, 0x1234);

        assert_eq!(
            result.get_inner_value(),
            waiter.lock().unwrap().get_wait_result()
        );
        assert_eq!(waiter.lock().unwrap().get_state(), ThreadState::WAITING);
        assert_eq!(
            KConditionVariable::signal_to_address(&process, &owner, address),
            RESULT_SUCCESS
        );
        assert_eq!(waiter.lock().unwrap().get_state(), ThreadState::RUNNABLE);
        assert_eq!(
            waiter.lock().unwrap().get_wait_result(),
            RESULT_SUCCESS.get_inner_value()
        );
    }

    #[test]
    fn wait_for_address_cancel_clears_waiting_lock_info() {
        let (_process, owner, waiter, _owner_handle, address) = setup_threads();
        {
            let mut waiter_guard = waiter.lock().unwrap();
            waiter_guard.set_user_address_key(KProcessAddress::new(address), 0x1234);
        }
        {
            let wt = waiter.lock().unwrap();
            let priority = wt.get_priority();
            let addr_key = wt.get_address_key();
            let is_kernel = wt.get_is_kernel_address_key();
            drop(wt);
            owner
                .lock()
                .unwrap()
                .add_waiter(&waiter, 2, priority, addr_key, is_kernel);
        }

        assert_eq!(owner.lock().unwrap().waiter_thread_ids(), vec![2]);
        assert_eq!(waiter.lock().unwrap().get_lock_owner_thread_id(), Some(1));

        ThreadQueueImplForKConditionVariableWaitForAddress::cancel_wait(
            &mut waiter.lock().unwrap(),
        );

        assert!(owner.lock().unwrap().waiter_thread_ids().is_empty());
        assert_eq!(waiter.lock().unwrap().get_lock_owner_thread_id(), None);
    }

    #[test]
    fn thread_tree_reinsert_replaces_stale_key_for_same_thread() {
        let mut tree = ConditionVariableThreadTree::default();

        tree.insert(ConditionVariableThreadKey {
            cv_key: 0x1000,
            priority: 10,
            thread_id: 42,
        });
        tree.insert(ConditionVariableThreadKey {
            cv_key: 0x2000,
            priority: 10,
            thread_id: 42,
        });

        assert_eq!(
            tree.nfind_key(0x1000)
                .map(|key| (key.cv_key, key.thread_id)),
            Some((0x2000, 42))
        );
        assert_eq!(
            tree.nfind_key(0x2000)
                .map(|key| (key.cv_key, key.thread_id)),
            Some((0x2000, 42))
        );
        assert_eq!(tree.bucket_count(), 1);
        assert_eq!(tree.by_thread_id.len(), 1);
    }

    #[test]
    fn thread_tree_allows_multiple_waiters_with_same_key_and_priority() {
        let mut tree = ConditionVariableThreadTree::default();

        tree.insert(ConditionVariableThreadKey {
            cv_key: 0x3000,
            priority: 10,
            thread_id: 41,
        });
        tree.insert(ConditionVariableThreadKey {
            cv_key: 0x3000,
            priority: 10,
            thread_id: 42,
        });

        assert_eq!(tree.bucket_count(), 1);
        assert_eq!(tree.by_thread_id.len(), 2);
        assert_eq!(tree.ordered_thread_ids(), vec![41, 42]);
        assert_eq!(
            tree.nfind_key(0x3000)
                .map(|key| (key.cv_key, key.priority, key.thread_id)),
            Some((0x3000, 10, 41))
        );

        tree.erase_by_thread_id(41);
        assert_eq!(tree.ordered_thread_ids(), vec![42]);
        assert_eq!(
            tree.nfind_key(0x3000)
                .map(|key| (key.cv_key, key.priority, key.thread_id)),
            Some((0x3000, 10, 42))
        );
    }

    #[test]
    fn signal_to_address_wakes_next_waiter_and_updates_tag() {
        let (process, owner, waiter, owner_handle, address) = setup_threads();
        {
            let mut waiter_guard = waiter.lock().unwrap();
            waiter_guard.set_user_address_key(KProcessAddress::new(address), 0xCAFE);
            waiter_guard.begin_wait();
            waiter_guard.set_wait_reason_for_debugging(ThreadWaitReasonForDebugging::ConditionVar);
        }
        {
            let wt = waiter.lock().unwrap();
            let addr_key = wt.get_address_key();
            let is_kernel = wt.get_is_kernel_address_key();
            let priority = wt.get_priority();
            drop(wt);
            // KThread::add_waiter sets the waiter's lock-owner back-pointer
            // internally (matches upstream AddWaiter semantics).
            owner
                .lock()
                .unwrap()
                .add_waiter(&waiter, 2, priority, addr_key, is_kernel);
        }

        let result = KConditionVariable::signal_to_address(&process, &owner, address);

        assert_eq!(result, RESULT_SUCCESS);
        assert_eq!(waiter.lock().unwrap().get_state(), ThreadState::RUNNABLE);
        assert_eq!(
            waiter.lock().unwrap().wait_result,
            RESULT_SUCCESS.get_inner_value()
        );
        assert_eq!(
            process
                .lock()
                .unwrap()
                .process_memory
                .read()
                .unwrap()
                .read_32(address),
            0xCAFE
        );
        assert!(
            owner.lock().unwrap().waiter_thread_ids().is_empty(),
            "owner should have no waiters after signal"
        );
        assert_ne!(owner_handle, 0);
    }

    #[test]
    fn signal_to_address_transfers_remaining_waiters_to_new_owner() {
        let (process, owner, next_owner, _owner_handle, address) = setup_threads();
        let remaining_waiter = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut waiter_guard = remaining_waiter.lock().unwrap();
            waiter_guard.object_id = 12;
            waiter_guard.thread_id = 3;
            waiter_guard.parent = Some(Arc::downgrade(&process));
            waiter_guard.set_state(ThreadState::RUNNABLE);
        }
        process
            .lock()
            .unwrap()
            .register_thread_object(remaining_waiter.clone());

        for (thread, tid, tag) in [(&next_owner, 2, 0xCAFE), (&remaining_waiter, 3, 0xBEEF)] {
            {
                let mut thread_guard = thread.lock().unwrap();
                thread_guard.set_user_address_key(KProcessAddress::new(address), tag);
                thread_guard.begin_wait();
                thread_guard
                    .set_wait_reason_for_debugging(ThreadWaitReasonForDebugging::ConditionVar);
            }
            let wt = thread.lock().unwrap();
            let priority = wt.get_priority();
            let addr_key = wt.get_address_key();
            let is_kernel = wt.get_is_kernel_address_key();
            drop(wt);
            owner
                .lock()
                .unwrap()
                .add_waiter(thread, tid, priority, addr_key, is_kernel);
        }

        assert_eq!(
            remaining_waiter.lock().unwrap().get_lock_owner_thread_id(),
            Some(1)
        );

        let mut has_waiters = false;
        let next_owner_result = owner.lock().unwrap().remove_waiter_by_key(
            KProcessAddress::new(address),
            false,
            &mut has_waiters,
        );
        let (next_owner_id, _priority, _, transfer_lock_info) = next_owner_result.unwrap();
        assert_eq!(next_owner_id, 2);
        assert!(has_waiters);

        let mut process_guard = process.lock().unwrap();
        assert!(transfer_lock_info_to_next_owner(
            &mut process_guard,
            next_owner_id,
            transfer_lock_info.unwrap(),
        ));
        drop(process_guard);
        next_owner.lock().unwrap().set_waiting_lock_info(None);

        assert!(owner.lock().unwrap().waiter_thread_ids().is_empty());
        assert_eq!(
            next_owner
                .lock()
                .unwrap()
                .waiter_thread_ids_for_address(KProcessAddress::new(address)),
            vec![3]
        );
        assert_eq!(next_owner.lock().unwrap().get_lock_owner_thread_id(), None);
        assert_eq!(
            remaining_waiter.lock().unwrap().get_lock_owner_thread_id(),
            Some(2)
        );
    }

    #[test]
    fn wait_with_timeout_keeps_condvar_queue_until_timer_cancels_it() {
        let (process, _owner, waiter, _owner_handle, address) = setup_threads();
        let key = 0x1c00;
        let timeout_tick = super::super::kernel::get_current_hardware_tick()
            .expect("test kernel must expose its hardware tick")
            .saturating_add(1_000_000_000);
        let process_for_wait = Arc::clone(&process);
        let waiter_for_wait = Arc::clone(&waiter);
        let wait_thread = std::thread::spawn(move || {
            KProcess::wait_condition_variable(
                &process_for_wait,
                &waiter_for_wait,
                address,
                key,
                0x1234,
                timeout_tick,
            )
        });

        std::thread::sleep(Duration::from_millis(10));

        assert!(waiter.lock().unwrap().has_wait_queue());
        assert_eq!(
            process.lock().unwrap().cond_var.waiting_thread_ids(),
            vec![2]
        );

        waiter.lock().unwrap().on_timer();
        wait_thread.join().unwrap();

        let waiter_guard = waiter.lock().unwrap();
        assert_eq!(waiter_guard.get_state(), ThreadState::RUNNABLE);
        assert!(!waiter_guard.has_wait_queue());
        assert_eq!(
            waiter_guard.wait_result,
            crate::hle::kernel::svc::svc_results::RESULT_TIMED_OUT.get_inner_value()
        );
        assert_eq!(
            waiter_guard.condvar_tree_state,
            ConditionVariableTreeState::None
        );
        drop(waiter_guard);
        assert!(process
            .lock()
            .unwrap()
            .cond_var
            .waiting_thread_ids()
            .is_empty());
    }

    #[test]
    fn process_owned_condition_variable_is_visible_to_signalers_while_waiting() {
        let (process, _owner, waiter, _owner_handle, address) = setup_threads();
        let key = 0x1c40;

        let waiter_process = Arc::clone(&process);
        let waiter_thread = Arc::clone(&waiter);
        let wait_handle = std::thread::spawn(move || {
            KProcess::wait_condition_variable(
                &waiter_process,
                &waiter_thread,
                address,
                key,
                0x1234,
                5_000_000,
            )
        });

        std::thread::sleep(Duration::from_millis(10));
        {
            let process_guard = process.lock().unwrap();
            assert_eq!(process_guard.cond_var.waiting_thread_ids(), vec![2]);
        }

        process.lock().unwrap().signal_condition_variable(key, 1);

        let result = wait_handle.join().unwrap();
        assert_eq!(result, RESULT_SUCCESS.get_inner_value());
        assert_eq!(waiter.lock().unwrap().get_state(), ThreadState::RUNNABLE);
        assert!(process
            .lock()
            .unwrap()
            .cond_var
            .waiting_thread_ids()
            .is_empty());
    }

    #[test]
    fn condition_variable_waiters_reorder_on_priority_change() {
        ensure_scheduler_lock_for_test();

        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let scheduler = Arc::new(Mutex::new(KScheduler::new(0)));
        scheduler.lock().unwrap().initialize(1, 0, 0);
        {
            let mut process_guard = process.lock().unwrap();
            process_guard.attach_scheduler(&scheduler);
            process_guard.bind_self_reference(&process);
            process_guard.initialize_handle_table();
            process_guard.allocate_code_memory(0x1000, 0x4000);
        }

        let waiter_a = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut waiter_a_guard = waiter_a.lock().unwrap();
            waiter_a_guard.object_id = 11;
            waiter_a_guard.thread_id = 2;
            waiter_a_guard.parent = Some(Arc::downgrade(&process));
            waiter_a_guard.scheduler = Some(Arc::downgrade(&scheduler));
            waiter_a_guard.set_state(ThreadState::RUNNABLE);
            waiter_a_guard.set_base_priority(20);
        }

        let waiter_b = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut waiter_b_guard = waiter_b.lock().unwrap();
            waiter_b_guard.object_id = 12;
            waiter_b_guard.thread_id = 3;
            waiter_b_guard.parent = Some(Arc::downgrade(&process));
            waiter_b_guard.scheduler = Some(Arc::downgrade(&scheduler));
            waiter_b_guard.set_state(ThreadState::RUNNABLE);
            waiter_b_guard.set_base_priority(30);
        }

        {
            let mut process_guard = process.lock().unwrap();
            process_guard.register_thread_object(waiter_a.clone());
            process_guard.register_thread_object(waiter_b.clone());
        }

        let key = 0x1c00;
        {
            let mut process_guard = process.lock().unwrap();
            let mut cond_var = std::mem::take(&mut process_guard.cond_var);
            cond_var.wait_locked(&mut process_guard, &waiter_a, 0x1800, key, 0x1111, -1);
            cond_var.wait_locked(&mut process_guard, &waiter_b, 0x1810, key, 0x2222, -1);
            process_guard.cond_var = cond_var;
            assert_eq!(process_guard.cond_var.waiting_thread_ids(), vec![2, 3]);
        }

        waiter_b.lock().unwrap().set_base_priority(10);

        assert_eq!(
            process.lock().unwrap().cond_var.waiting_thread_ids(),
            vec![3, 2]
        );
    }

    #[test]
    fn wait_locked_returns_only_after_signal() {
        let (process, owner, _waiter, _owner_handle, address) = setup_threads();
        let key = 0x1c00;

        let result = KProcess::wait_condition_variable(&process, &owner, address, key, 0x1111, -1);

        assert_eq!(result, owner.lock().unwrap().get_wait_result());
        assert_eq!(owner.lock().unwrap().get_state(), ThreadState::WAITING);
        process.lock().unwrap().signal_condition_variable(key, 1);
        assert_eq!(owner.lock().unwrap().get_state(), ThreadState::RUNNABLE);
        assert_eq!(
            owner.lock().unwrap().get_wait_result(),
            RESULT_SUCCESS.get_inner_value()
        );
    }

    #[test]
    fn wait_condition_variable_resumes_after_signal() {
        let (process, owner, _waiter, _owner_handle, address) = setup_threads();
        let key = 0x1c40;

        let result = KProcess::wait_condition_variable(&process, &owner, address, key, 0x2222, -1);

        assert_eq!(result, owner.lock().unwrap().get_wait_result());
        assert_eq!(owner.lock().unwrap().get_state(), ThreadState::WAITING);
        process.lock().unwrap().signal_condition_variable(key, 1);
        assert_eq!(owner.lock().unwrap().get_state(), ThreadState::RUNNABLE);
        assert_eq!(
            owner.lock().unwrap().get_wait_result(),
            RESULT_SUCCESS.get_inner_value()
        );
    }
}
