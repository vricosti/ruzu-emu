//! Port of zuyu/src/core/hle/kernel/k_address_arbiter.{h,cpp}
//! Status: Upstream-faithful (scheduler-lock + intrusive tree + BeginWait)
//! Derniere synchro: 2026-04-20
//!
//! KAddressArbiter: implements userspace address-based thread arbitration
//! (futex-like SignalToAddress / WaitForAddress SVCs).
//!
//! Upstream structure:
//! - `ThreadTree m_tree` — red-black tree of waiters keyed by (addr, priority).
//! - 5 public methods: `Signal`, `SignalAndIncrementIfEqual`,
//!   `SignalAndModifyByWaitingCountIfEqual`, `WaitIfLessThan`, `WaitIfEqual`.
//! - Every signal method opens `KScopedSchedulerLock` for its entire body.
//! - Every wait method opens `KScopedSchedulerLockAndSleep` for the setup
//!   phase; the scope exit triggers the fiber switch.
//! - Atomic userspace operations use the exclusive monitor (LDREX/STREX).
//!
//! Ruzu adaptation:
//! - The tree is a BTreeMap<(addr, priority), Vec<thread_id>> + reverse
//!   HashMap<thread_id, (addr, priority)>, mirroring the condvar tree.
//! - `AddressArbiterThreadTree` is the type-level equivalent of upstream's
//!   `KThread::AddressArbiterThreadTreeType`.
//! - Atomic userspace ops take a `&KProcess` guard and use the existing
//!   `process_memory`/page-table read/write path instead of the exclusive
//!   monitor; the scheduler lock already serializes against other guest
//!   threads at the same address.

use std::collections::{BTreeMap, HashMap};

use std::sync::Arc;

use crate::hle::kernel::k_process::{KProcess, ProcessLock};
use crate::hle::kernel::k_scheduler_lock::KScopedSchedulerLock;
use crate::hle::kernel::k_scoped_scheduler_lock_and_sleep::KScopedSchedulerLockAndSleep;
use crate::hle::kernel::k_thread::{KThread, KThreadLock, ThreadWaitReasonForDebugging};
use crate::hle::kernel::k_thread_queue::KThreadQueue;
use crate::hle::kernel::svc::svc_results::{
    RESULT_INVALID_CURRENT_MEMORY, RESULT_INVALID_STATE, RESULT_TERMINATION_REQUESTED,
    RESULT_TIMED_OUT,
};
use crate::hle::result::{ResultCode, RESULT_SUCCESS};

/// Signal type for address arbiter operations. Maps to `Svc::SignalType`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalType {
    Signal = 0,
    SignalAndIncrementIfEqual = 1,
    SignalAndModifyByWaitingCountIfEqual = 2,
}

/// Arbitration type for address arbiter wait operations. Maps to `Svc::ArbitrationType`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArbitrationType {
    WaitIfLessThan = 0,
    DecrementAndWaitIfLessThan = 1,
    WaitIfEqual = 2,
}

// ---------------------------------------------------------------------------
// Guest-memory helpers matching upstream anonymous namespace.
// ---------------------------------------------------------------------------

/// Read an s32 from process memory. Port of upstream `ReadFromUser`.
fn read_from_user(process_guard: &KProcess, address: u64) -> Option<i32> {
    if let Some(memory) = process_guard.get_memory().as_ref() {
        Some(memory.access().unwrap().read_32(address) as i32)
    } else {
        let mem = process_guard.process_memory.read().unwrap();
        if !mem.is_valid_range(address, 4) {
            return None;
        }
        Some(mem.read_32(address) as i32)
    }
}

/// Decrement the value at `address` if it is < `value`. Returns the pre-decrement value.
///
/// Port of upstream `DecrementIfLessThan` (k_address_arbiter.cpp:30-68). Upstream
/// uses the exclusive monitor (LDREX/STREX CAS loop). We mirror that with
/// `Memory::write_exclusive_32` which performs `compare_exchange(SeqCst, SeqCst)`
/// on the guest atomic. Retry on contention.
///
/// The non-CAS variant was racy: two cores reading the same `current` and both
/// writing `current-1` lost a decrement, corrupting the arbiter waiter counter
/// state corruption).
fn decrement_if_less_than(process_guard: &KProcess, address: u64, value: i32) -> Option<i32> {
    loop {
        let current = read_from_user(process_guard, address)?;
        if current >= value {
            return Some(current);
        }
        let new_value = current.wrapping_sub(1) as u32;
        let expected = current as u32;
        let cas_ok = if let Some(memory) = process_guard.get_memory().as_ref() {
            memory
                .access()
                .unwrap()
                .write_exclusive_32(address, new_value, expected)
        } else {
            let mut mem = process_guard.process_memory.write().unwrap();
            if !mem.is_valid_range(address, 4) {
                return None;
            }
            // process_memory fallback path: best-effort plain write (no CAS
            // primitive). Used only when the main Memory backing is absent
            // (early init / unit tests), so contention should not occur here.
            mem.write_32(address, new_value);
            true
        };
        if cas_ok {
            return Some(current);
        }
        // CAS failed — another core won the race. Retry.
    }
}

/// Replace the value at `address` with `new_value` if it currently equals `value`.
/// Returns the pre-replacement value.
///
/// Port of upstream `UpdateIfEqual` (k_address_arbiter.cpp:70-108). Same CAS-loop
/// rationale as `decrement_if_less_than` above.
fn update_if_equal(
    process_guard: &KProcess,
    address: u64,
    value: i32,
    new_value: i32,
) -> Option<i32> {
    loop {
        let current = read_from_user(process_guard, address)?;
        if current != value {
            return Some(current);
        }
        let expected = current as u32;
        let cas_ok = if let Some(memory) = process_guard.get_memory().as_ref() {
            memory
                .access()
                .unwrap()
                .write_exclusive_32(address, new_value as u32, expected)
        } else {
            let mut mem = process_guard.process_memory.write().unwrap();
            if !mem.is_valid_range(address, 4) {
                return None;
            }
            mem.write_32(address, new_value as u32);
            true
        };
        if cas_ok {
            return Some(current);
        }
    }
}

// ---------------------------------------------------------------------------
// ThreadQueueImplForKAddressArbiter — mirror upstream
// ---------------------------------------------------------------------------

/// Matches upstream `ThreadQueueImplForKAddressArbiter`. On cancel, removes
/// the waiter from its owning arbiter tree. The tree pointer is stored in
/// the process (via `KThread::get_parent_raw_ptr`), mirroring how condvar
/// recovers its tree from the parent process under the scheduler lock.
struct ThreadQueueImplForKAddressArbiter;

impl ThreadQueueImplForKAddressArbiter {
    fn queue() -> KThreadQueue {
        KThreadQueue::with_callbacks(None, Some(Self::cancel_wait))
    }

    fn cancel_wait(waiting_thread: &mut KThread) {
        if waiting_thread.is_waiting_for_address_arbiter() {
            if let Some(parent_ptr) = waiting_thread.get_parent_raw_ptr() {
                // SAFETY: parent raw pointer is populated at thread
                // initialization and remains valid for the thread's lifetime;
                // we only dereference under the scheduler lock.
                let parent = unsafe { &mut *parent_ptr };
                parent
                    .address_arbiter
                    .waiting_threads
                    .erase_by_thread_id(waiting_thread.thread_id);
            }
            waiting_thread.clear_address_arbiter();
        }
    }
}

// ---------------------------------------------------------------------------
// AddressArbiterThreadTree — type-level equivalent of upstream's
// `KThread::AddressArbiterThreadTreeType`.
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct AddressArbiterThreadTree {
    ordered: BTreeMap<(u64, i32), Vec<u64>>,
    by_thread_id: HashMap<u64, (u64, i32)>,
}

impl AddressArbiterThreadTree {
    fn insert(&mut self, addr: u64, priority: i32, thread_id: u64) {
        if let Some((old_addr, old_priority)) =
            self.by_thread_id.insert(thread_id, (addr, priority))
        {
            self.remove_from_bucket(old_addr, old_priority, thread_id);
        }
        self.ordered
            .entry((addr, priority))
            .or_default()
            .push(thread_id);
    }

    fn erase_by_thread_id(&mut self, thread_id: u64) {
        if let Some((addr, priority)) = self.by_thread_id.remove(&thread_id) {
            self.remove_from_bucket(addr, priority, thread_id);
        }
    }

    /// Return the first waiter thread_id for exactly `addr` (lowest priority
    /// wins, then lowest thread_id within a bucket). Matches upstream's
    /// `m_tree.nfind_key({addr, -1})` follow-up "key == addr" check.
    fn first_waiter_for_addr(&self, addr: u64) -> Option<u64> {
        self.ordered
            .range((addr, i32::MIN)..)
            .next()
            .and_then(|((bucket_addr, _), thread_ids)| {
                if *bucket_addr == addr {
                    thread_ids.first().copied()
                } else {
                    None
                }
            })
    }

    /// Count waiters queued on exactly `addr`.
    fn count_waiters_for_addr(&self, addr: u64) -> i32 {
        self.ordered
            .range((addr, i32::MIN)..=(addr, i32::MAX))
            .map(|(_, ids)| ids.len() as i32)
            .sum()
    }

    fn remove_from_bucket(&mut self, addr: u64, priority: i32, thread_id: u64) {
        let key = (addr, priority);
        let empty = if let Some(ids) = self.ordered.get_mut(&key) {
            if let Some(idx) = ids.iter().position(|existing| *existing == thread_id) {
                ids.remove(idx);
            }
            ids.is_empty()
        } else {
            false
        };
        if empty {
            self.ordered.remove(&key);
        }
    }
}

// ---------------------------------------------------------------------------
// KAddressArbiter
// ---------------------------------------------------------------------------

/// Address arbiter — port of upstream `KAddressArbiter`.
///
/// Owned by `KProcess::m_address_arbiter`. All mutation happens under the
/// kernel scheduler lock, held by every public method's entry.
pub struct KAddressArbiter {
    waiting_threads: AddressArbiterThreadTree,
}

impl KAddressArbiter {
    pub fn new() -> Self {
        Self {
            waiting_threads: AddressArbiterThreadTree::default(),
        }
    }

    /// Dispatch a signal operation. Called from `KProcess::signal_address_arbiter`.
    /// Upstream corresponds to the inline dispatch in `GetCurrentProcess(kernel)`
    /// methods that forward to the per-signal-type call.
    pub fn signal_to_address(
        &mut self,
        process_guard: &mut KProcess,
        addr: u64,
        signal_type: SignalType,
        value: i32,
        count: i32,
    ) -> ResultCode {
        match signal_type {
            SignalType::Signal => self.signal(process_guard, addr, count),
            SignalType::SignalAndIncrementIfEqual => {
                self.signal_and_increment_if_equal(process_guard, addr, value, count)
            }
            SignalType::SignalAndModifyByWaitingCountIfEqual => {
                self.signal_and_modify_by_waiting_count_if_equal(process_guard, addr, value, count)
            }
        }
    }

    /// Dispatch a wait operation. Called from `KProcess::wait_address_arbiter`.
    pub fn wait_for_address(
        &mut self,
        process: &Arc<ProcessLock>,
        current_thread: &Arc<KThreadLock>,
        addr: u64,
        arb_type: ArbitrationType,
        value: i32,
        timeout: i64,
    ) -> ResultCode {
        match arb_type {
            ArbitrationType::WaitIfLessThan => {
                self.wait_if_less_than(process, current_thread, addr, value, false, timeout)
            }
            ArbitrationType::DecrementAndWaitIfLessThan => {
                self.wait_if_less_than(process, current_thread, addr, value, true, timeout)
            }
            ArbitrationType::WaitIfEqual => {
                self.wait_if_equal(process, current_thread, addr, value, timeout)
            }
        }
    }

    // -- Signal variants (upstream-faithful) -------------------------------

    /// Port of upstream `KAddressArbiter::Signal`.
    fn signal(&mut self, process_guard: &mut KProcess, addr: u64, count: i32) -> ResultCode {
        let scheduler_lock = super::kernel::scheduler_lock()
            .expect("scheduler_lock must exist — kernel not initialized?");
        let _sl = KScopedSchedulerLock::new(scheduler_lock);

        self.signal_locked(process_guard, addr, count);
        RESULT_SUCCESS
    }

    /// Port of upstream `KAddressArbiter::SignalAndIncrementIfEqual`.
    fn signal_and_increment_if_equal(
        &mut self,
        process_guard: &mut KProcess,
        addr: u64,
        value: i32,
        count: i32,
    ) -> ResultCode {
        let scheduler_lock = super::kernel::scheduler_lock()
            .expect("scheduler_lock must exist — kernel not initialized?");
        let _sl = KScopedSchedulerLock::new(scheduler_lock);

        // Upstream: UpdateIfEqual(kernel, &user_value, addr, value, value + 1);
        let Some(user_value) = update_if_equal(process_guard, addr, value, value.wrapping_add(1))
        else {
            return RESULT_INVALID_CURRENT_MEMORY;
        };
        if user_value != value {
            return RESULT_INVALID_STATE;
        }

        self.signal_locked(process_guard, addr, count);
        RESULT_SUCCESS
    }

    /// Port of upstream `KAddressArbiter::SignalAndModifyByWaitingCountIfEqual`.
    fn signal_and_modify_by_waiting_count_if_equal(
        &mut self,
        process_guard: &mut KProcess,
        addr: u64,
        value: i32,
        count: i32,
    ) -> ResultCode {
        let scheduler_lock = super::kernel::scheduler_lock()
            .expect("scheduler_lock must exist — kernel not initialized?");
        let _sl = KScopedSchedulerLock::new(scheduler_lock);

        // Determine new_value — mirrors upstream lines 192-217.
        let has_waiters = self.waiting_threads.first_waiter_for_addr(addr).is_some();
        let new_value = if count <= 0 {
            if has_waiters {
                value.wrapping_sub(2)
            } else {
                value.wrapping_add(1)
            }
        } else if has_waiters {
            let waiter_count_after_head = self.waiting_threads.count_waiters_for_addr(addr) - 1;
            if waiter_count_after_head < count {
                value.wrapping_sub(1)
            } else {
                value
            }
        } else {
            value.wrapping_add(1)
        };

        let user_value_opt = if value != new_value {
            update_if_equal(process_guard, addr, value, new_value)
        } else {
            read_from_user(process_guard, addr)
        };
        let Some(user_value) = user_value_opt else {
            return RESULT_INVALID_CURRENT_MEMORY;
        };
        if user_value != value {
            return RESULT_INVALID_STATE;
        }

        self.signal_locked(process_guard, addr, count);
        RESULT_SUCCESS
    }

    /// Core signal loop shared by the three signal variants. Caller holds the
    /// scheduler lock. Pops up to `count` (or all, if count <= 0) waiters from
    /// the tree at `addr` and fires `EndWait(ResultSuccess)` on each.
    ///
    /// Matches upstream `KAddressArbiter::Signal` body (k_address_arbiter.cpp:139-149):
    ///   target_thread->EndWait(ResultSuccess);
    ///   ASSERT(target_thread->IsWaitingForAddressArbiter());
    ///   target_thread->ClearAddressArbiter();
    ///   it = m_tree.erase(it);
    fn signal_locked(&mut self, process_guard: &mut KProcess, addr: u64, count: i32) {
        let mut num_waiters = 0i32;
        while count <= 0 || num_waiters < count {
            let Some(target_thread_id) = self.waiting_threads.first_waiter_for_addr(addr) else {
                break;
            };
            // Kernel invariant: the arbiter tree only contains live threads
            // that are still waiting on an address arbiter. Upstream's
            // `&*it` dereference is unconditional (k_address_arbiter.cpp:142);
            // we match that and panic on invariant break.
            let target_thread = process_guard
                .get_thread_by_thread_id(target_thread_id)
                .expect(
                    "KAddressArbiter::signal: arbiter tree references unknown thread \
                     (kernel invariant violated — thread cleanup missed tree entry)",
                );
            {
                let mut guard = target_thread.lock().unwrap();
                // EndWait first, matching upstream order.
                guard.end_wait(RESULT_SUCCESS.get_inner_value());
                debug_assert!(guard.is_waiting_for_address_arbiter());
                guard.clear_address_arbiter();
            }
            // Erase from tree last (matches `it = m_tree.erase(it)`).
            self.waiting_threads.erase_by_thread_id(target_thread_id);
            num_waiters += 1;
        }
    }

    // -- Wait variants (upstream-faithful) ---------------------------------

    /// Port of upstream `KAddressArbiter::WaitIfLessThan`.
    fn wait_if_less_than(
        &mut self,
        process: &Arc<ProcessLock>,
        current_thread: &Arc<KThreadLock>,
        addr: u64,
        value: i32,
        decrement: bool,
        timeout: i64,
    ) -> ResultCode {
        let current_thread_id = current_thread.lock().unwrap().get_thread_id();
        let priority = current_thread.lock().unwrap().get_priority();
        let scheduler_lock = super::kernel::scheduler_lock()
            .expect("scheduler_lock must exist — kernel not initialized?");
        let hardware_timer = super::kernel::get_hardware_timer_arc();
        let thread_ptr = {
            let guard = current_thread.lock().unwrap();
            &*guard as *const KThread as usize
        };

        let setup_result: Result<(), ResultCode> = (|| {
            let (mut sleep_guard, timer) = KScopedSchedulerLockAndSleep::new(
                scheduler_lock,
                hardware_timer.as_ref(),
                current_thread_id,
                thread_ptr,
                timeout,
            );

            if current_thread.lock().unwrap().is_termination_requested() {
                sleep_guard.cancel_sleep();
                return Err(RESULT_TERMINATION_REQUESTED);
            }

            let process_guard = process.lock().unwrap();

            // Read / optionally decrement userspace value.
            let user_value = if decrement {
                decrement_if_less_than(&process_guard, addr, value)
            } else {
                read_from_user(&process_guard, addr)
            };
            let Some(user_value) = user_value else {
                sleep_guard.cancel_sleep();
                return Err(RESULT_INVALID_CURRENT_MEMORY);
            };
            if user_value >= value {
                sleep_guard.cancel_sleep();
                return Err(RESULT_INVALID_STATE);
            }
            if timeout == 0 {
                sleep_guard.cancel_sleep();
                return Err(RESULT_TIMED_OUT);
            }

            // Match upstream tail (k_address_arbiter.cpp:289-295):
            //   cur_thread->SetAddressArbiter(&m_tree, addr);
            //   m_tree.insert(*cur_thread);
            //   wait_queue.SetHardwareTimer(timer);
            //   cur_thread->BeginWait(&wait_queue);
            //   cur_thread->SetWaitReasonForDebugging(Arbitration);
            // Tree insert MUST happen before BeginWait flips state to WAITING.
            current_thread.lock().unwrap().set_address_arbiter(addr);
            self.waiting_threads
                .insert(addr, priority, current_thread_id);
            let mut wait_queue = ThreadQueueImplForKAddressArbiter::queue();
            if let Some(timer) = timer {
                wait_queue.set_hardware_timer(timer);
            }
            Self::begin_wait_arbiter(current_thread, wait_queue);
            drop(process_guard);

            Ok(())
        })();

        // Scheduler lock + sleep guard have dropped. Either we short-circuited
        // with an error, or the fiber-wait is armed.
        if let Err(early_return) = setup_result {
            return early_return;
        }

        ResultCode::new(current_thread.lock().unwrap().get_wait_result())
    }

    /// Port of upstream `KAddressArbiter::WaitIfEqual`.
    fn wait_if_equal(
        &mut self,
        process: &Arc<ProcessLock>,
        current_thread: &Arc<KThreadLock>,
        addr: u64,
        value: i32,
        timeout: i64,
    ) -> ResultCode {
        let current_thread_id = current_thread.lock().unwrap().get_thread_id();
        let priority = current_thread.lock().unwrap().get_priority();
        let scheduler_lock = super::kernel::scheduler_lock()
            .expect("scheduler_lock must exist — kernel not initialized?");
        let hardware_timer = super::kernel::get_hardware_timer_arc();
        let thread_ptr = {
            let guard = current_thread.lock().unwrap();
            &*guard as *const KThread as usize
        };

        let setup_result: Result<(), ResultCode> = (|| {
            let (mut sleep_guard, timer) = KScopedSchedulerLockAndSleep::new(
                scheduler_lock,
                hardware_timer.as_ref(),
                current_thread_id,
                thread_ptr,
                timeout,
            );

            if current_thread.lock().unwrap().is_termination_requested() {
                sleep_guard.cancel_sleep();
                return Err(RESULT_TERMINATION_REQUESTED);
            }

            let process_guard = process.lock().unwrap();
            let Some(user_value) = read_from_user(&process_guard, addr) else {
                sleep_guard.cancel_sleep();
                return Err(RESULT_INVALID_CURRENT_MEMORY);
            };
            if user_value != value {
                sleep_guard.cancel_sleep();
                return Err(RESULT_INVALID_STATE);
            }
            if timeout == 0 {
                sleep_guard.cancel_sleep();
                return Err(RESULT_TIMED_OUT);
            }

            // Match upstream tail (k_address_arbiter.cpp:337-343), same
            // sequencing as WaitIfLessThan above.
            current_thread.lock().unwrap().set_address_arbiter(addr);
            self.waiting_threads
                .insert(addr, priority, current_thread_id);
            let mut wait_queue = ThreadQueueImplForKAddressArbiter::queue();
            if let Some(timer) = timer {
                wait_queue.set_hardware_timer(timer);
            }
            Self::begin_wait_arbiter(current_thread, wait_queue);
            drop(process_guard);

            Ok(())
        })();

        if let Err(early_return) = setup_result {
            return early_return;
        }

        ResultCode::new(current_thread.lock().unwrap().get_wait_result())
    }

    /// Begin-wait tail for the arbiter wait variants. Matches upstream
    /// `KAddressArbiter::WaitIfLessThan` / `WaitIfEqual` tail
    /// (k_address_arbiter.cpp:294-295):
    ///   cur_thread->BeginWait(&wait_queue);
    ///   cur_thread->SetWaitReasonForDebugging(Arbitration);
    ///
    /// `SetAddressArbiter` and the tree insert are pulled up to the caller
    /// so they execute BEFORE `BeginWait` flips state to WAITING — same
    /// upstream-order invariant as `KConditionVariable::Wait`.
    fn begin_wait_arbiter(current_thread: &Arc<KThreadLock>, wait_queue: KThreadQueue) {
        let mut thread = current_thread.lock().unwrap();
        thread.begin_wait_with_queue(wait_queue);
        thread.set_wait_reason_for_debugging(ThreadWaitReasonForDebugging::Arbitration);
    }
}

impl Default for KAddressArbiter {
    fn default() -> Self {
        Self::new()
    }
}
