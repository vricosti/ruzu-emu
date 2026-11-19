//! Port of zuyu/src/core/hle/kernel/global_scheduler_context.h/.cpp
//! Status: EN COURS
//! Derniere synchro: 2026-03-27
//!
//! GlobalSchedulerContext: the global scheduler context that manages the
//! priority queue of threads and the scheduler lock.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::hardware_properties;

use super::k_priority_queue::KPriorityQueue;
use super::k_scheduler_lock::KAbstractSchedulerLock;
use super::k_thread::{KThread, KThreadLock, ThreadState, ThreadType};

static TRACE_GSC_STATE_COUNT: AtomicUsize = AtomicUsize::new(0);

fn trace_sched_state_filter() -> &'static Option<Vec<u64>> {
    static FILTER: OnceLock<Option<Vec<u64>>> = OnceLock::new();
    FILTER.get_or_init(|| {
        let raw = std::env::var_os("RUZU_TRACE_SCHED_STATE")?;
        let raw = raw.to_string_lossy();
        let raw = raw.trim();
        if raw.is_empty() || raw == "1" || raw.eq_ignore_ascii_case("all") {
            return Some(Vec::new());
        }
        Some(
            raw.split(',')
                .filter_map(|value| {
                    let value = value.trim();
                    if value.is_empty() {
                        return None;
                    }
                    value
                        .strip_prefix("0x")
                        .or_else(|| value.strip_prefix("0X"))
                        .and_then(|hex| u64::from_str_radix(hex, 16).ok())
                        .or_else(|| value.parse::<u64>().ok())
                })
                .collect(),
        )
    })
}

fn increment_process_scheduled_count(
    counter: Option<&std::sync::Arc<std::sync::atomic::AtomicI64>>,
) {
    if let Some(counter) = counter {
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

fn should_trace_sched_state(thread_id: u64) -> bool {
    let Some(filter) = trace_sched_state_filter() else {
        return false;
    };
    filter.is_empty() || filter.contains(&thread_id)
}

fn trace_sched_state(args: std::fmt::Arguments<'_>) {
    if trace_sched_state_filter().is_none() {
        return;
    }
    let idx = TRACE_GSC_STATE_COUNT.fetch_add(1, Ordering::Relaxed);
    if idx < 1024 {
        log::info!("{}", args);
    }
}

fn encode_top(top: Option<u64>) -> u64 {
    top.unwrap_or(u64::MAX)
}

fn trace_sched_state_fast(
    stage: u64,
    thread_id: u64,
    old_state: ThreadState,
    new_state: ThreadState,
    priority: i32,
    active_core: i32,
    queue: &KPriorityQueue,
) {
    if !common::trace::is_enabled(common::trace::cat::SCHED_STATE) {
        return;
    }
    if !should_trace_sched_state(thread_id) {
        return;
    }
    common::trace::emit_raw(
        common::trace::cat::SCHED_STATE,
        &[
            stage,
            thread_id,
            old_state.bits() as u64,
            new_state.bits() as u64,
            priority as u32 as u64,
            active_core as u32 as u64,
            encode_top(queue.get_scheduled_front(0)),
            encode_top(queue.get_scheduled_front(1)),
            encode_top(queue.get_scheduled_front(2)),
            encode_top(queue.get_scheduled_front(3)),
        ],
    );
}

/// Priority at or above which core migration is allowed.
pub const HIGHEST_CORE_MIGRATION_ALLOWED_PRIORITY: i32 = 2;

/// Preemption priorities for each core (indices 0-5).
pub const PREEMPTION_PRIORITIES: [u32; 4] = [59, 59, 59, 63];
/// Existing experimental NX2 values; firmware priorities remain unverified.
const NX2_PREEMPTION_PRIORITIES: [u32; 6] = [59, 59, 59, 63, 59, 59];

fn preemption_priorities(profile: hardware_properties::HardwareProfile) -> &'static [u32] {
    match profile {
        hardware_properties::HardwareProfile::Switch1 => &PREEMPTION_PRIORITIES,
        hardware_properties::HardwareProfile::Nx2 => &NX2_PREEMPTION_PRIORITIES,
    }
}

/// The global scheduler context.
///
/// Matches upstream `GlobalSchedulerContext` (global_scheduler_context.h).
/// Owns the KSchedulerPriorityQueue, the scheduler lock, the thread list,
/// and dummy thread wakeup tracking.
pub struct GlobalSchedulerContext {
    pub hardware_profile: hardware_properties::HardwareProfile,
    pub m_scheduler_update_needed: std::sync::atomic::AtomicBool,
    pub m_priority_queue: KPriorityQueue,
    pub m_scheduler_lock: KAbstractSchedulerLock,

    /// Dummy threads pending wakeup on lock release.
    m_woken_dummy_threads: Mutex<HashMap<u64, Arc<KThreadLock>>>,
    /// All thread pointers that are alive.
    ///
    /// Upstream stores intrusive `KThread*` entries and can identify a thread
    /// without locking it. Keep the Rust owner keyed by `thread_id` so lookup
    /// does not relock arbitrary thread mutexes while the scheduler lock is
    /// unwinding.
    m_thread_list: Mutex<Vec<(u64, Arc<KThreadLock>)>>,
}

impl GlobalSchedulerContext {
    pub fn new() -> Self { Self::with_hardware_profile(hardware_properties::SWITCH1_PROFILE) }

    pub fn with_hardware_profile(hardware_profile: hardware_properties::HardwareProfile) -> Self {
        Self {
            hardware_profile,
            m_scheduler_update_needed: std::sync::atomic::AtomicBool::new(false),
            m_priority_queue: KPriorityQueue::new(),
            m_scheduler_lock: KAbstractSchedulerLock::new(),
            m_woken_dummy_threads: Mutex::new(HashMap::new()),
            m_thread_list: Mutex::new(Vec::new()),
        }
    }

    // -- Thread list management --

    pub fn add_thread(&self, thread: Arc<KThreadLock>) {
        // Matches upstream intrusive ownership: the thread list must never
        // silently accept an invalid identifier. Callers that already hold the
        // thread mutex must use add_thread_with_id() to avoid re-locking.
        let thread_id = thread.lock().unwrap().get_thread_id();
        self.m_thread_list.lock().unwrap().push((thread_id, thread));
    }

    /// Add a thread when the thread_id is already known (avoids re-locking).
    pub fn add_thread_with_id(&self, thread_id: u64, thread: Arc<KThreadLock>) {
        self.m_thread_list.lock().unwrap().push((thread_id, thread));
    }

    pub fn remove_thread(&self, thread_id: u64) {
        self.m_thread_list
            .lock()
            .unwrap()
            .retain(|(id, _)| *id != thread_id);
    }

    pub fn get_thread_list(&self) -> Vec<Arc<KThreadLock>> {
        self.m_thread_list
            .lock()
            .unwrap()
            .iter()
            .map(|(_, thread)| Arc::clone(thread))
            .collect()
    }

    pub fn get_thread_by_thread_id(&self, thread_id: u64) -> Option<Arc<KThreadLock>> {
        self.m_thread_list
            .lock()
            .unwrap()
            .iter()
            .find(|(id, _)| *id == thread_id)
            .map(|(_, thread)| Arc::clone(thread))
    }

    // -- Centralized PQ state change handler --
    // Matches upstream KScheduler::OnThreadStateChanged (k_scheduler.cpp:522).
    //
    // This is the single authority for keeping the PQ in sync with thread state.
    // All thread state transitions that cross Runnable must call this.
    //
    // Thread properties are passed directly (extracted by the caller while
    // holding the thread lock), so this function never locks any KThread.

    /// Called when a thread's state changes. Updates the PQ accordingly.
    /// Matches upstream `KScheduler::OnThreadStateChanged`.
    ///
    /// `old_state` and `new_state` are the base states (masked with ThreadState::MASK).
    pub fn on_thread_state_changed(
        &mut self,
        thread_id: u64,
        old_state: ThreadState,
        new_state: ThreadState,
        priority: i32,
        active_core: i32,
        affinity: u64,
        is_dummy: bool,
        thread: Option<Arc<KThreadLock>>,
        process_schedule_count: Option<std::sync::Arc<std::sync::atomic::AtomicI64>>,
        last_scheduled_tick: i64,
    ) {
        if should_trace_sched_state(thread_id) {
            trace_sched_state(format_args!(
                "GSC::on_thread_state_changed tid={} {:?}->{:?} prio={} active_core={} affinity=0x{:x} dummy={} sched_count={}",
                thread_id,
                old_state,
                new_state,
                priority,
                active_core,
                affinity,
                is_dummy,
                process_schedule_count
                    .as_ref()
                    .map(|c| c.load(std::sync::atomic::Ordering::Relaxed))
                    .unwrap_or(-1),
            ));
        }
        log::trace!(
            "GSC::on_thread_state_changed tid={} {:?}->{:?} prio={} active_core={} affinity=0x{:x} dummy={}",
            thread_id,
            old_state,
            new_state,
            priority,
            active_core,
            affinity,
            is_dummy
        );
        if old_state == new_state {
            return;
        }
        trace_sched_state_fast(
            1,
            thread_id,
            old_state,
            new_state,
            priority,
            active_core,
            &self.m_priority_queue,
        );

        if old_state == ThreadState::RUNNABLE {
            let process_schedule_count = self
                .m_priority_queue
                .get_thread_props(thread_id)
                .and_then(|props| props.process_schedule_count.clone())
                .or(process_schedule_count);
            // Was runnable, now not — remove from PQ.
            self.m_priority_queue
                .remove(thread_id, priority, active_core, affinity, is_dummy);
            increment_process_scheduled_count(process_schedule_count.as_ref());
            self.m_scheduler_update_needed
                .store(true, Ordering::Release);

            if is_dummy {
                self.unregister_dummy_thread_for_wakeup(thread_id);
            }
            if should_trace_sched_state(thread_id) {
                trace_sched_state(format_args!(
                    "GSC::pq remove tid={} prio={} core={} dummy={} top0={:?} top1={:?} top2={:?} top3={:?}",
                    thread_id,
                    priority,
                    active_core,
                    is_dummy,
                    self.m_priority_queue.get_scheduled_front(0),
                    self.m_priority_queue.get_scheduled_front(1),
                    self.m_priority_queue.get_scheduled_front(2),
                    self.m_priority_queue.get_scheduled_front(3),
                ));
            }
            trace_sched_state_fast(
                2,
                thread_id,
                old_state,
                new_state,
                priority,
                active_core,
                &self.m_priority_queue,
            );
        } else if new_state == ThreadState::RUNNABLE {
            // Was not runnable, now is — add to PQ.
            self.m_priority_queue.push_back(
                thread_id,
                priority,
                active_core,
                affinity,
                is_dummy,
                process_schedule_count,
            );
            self.m_priority_queue
                .set_last_scheduled_tick(thread_id, last_scheduled_tick);
            self.m_priority_queue.increment_scheduled_count(thread_id);
            self.m_scheduler_update_needed
                .store(true, Ordering::Release);

            if is_dummy {
                let thread = thread.expect("dummy thread must own a self reference");
                self.register_dummy_thread_for_wakeup(thread_id, thread);
            }

            if should_trace_sched_state(thread_id) {
                trace_sched_state(format_args!(
                    "GSC::pq push tid={} prio={} core={} dummy={} top0={:?} top1={:?} top2={:?} top3={:?}",
                    thread_id,
                    priority,
                    active_core,
                    is_dummy,
                    self.m_priority_queue.get_scheduled_front(0),
                    self.m_priority_queue.get_scheduled_front(1),
                    self.m_priority_queue.get_scheduled_front(2),
                    self.m_priority_queue.get_scheduled_front(3),
                ));
            }
            trace_sched_state_fast(
                3,
                thread_id,
                old_state,
                new_state,
                priority,
                active_core,
                &self.m_priority_queue,
            );
        }
    }

    /// Called when a thread's priority changes while it is Runnable.
    /// Matches upstream `KScheduler::OnThreadPriorityChanged`.
    pub fn on_thread_priority_changed(
        &mut self,
        _thread_id: u64,
        old_priority: i32,
        new_priority: i32,
        active_core: i32,
        affinity: u64,
        is_running: bool,
        is_dummy: bool,
        _member_id: u64,
    ) {
        let process_schedule_count = self
            .m_priority_queue
            .get_thread_props(_member_id)
            .and_then(|props| props.process_schedule_count.clone());
        self.m_priority_queue.change_priority(
            old_priority,
            is_running,
            _member_id,
            new_priority,
            active_core,
            affinity,
            is_dummy,
        );
        increment_process_scheduled_count(process_schedule_count.as_ref());
        self.m_scheduler_update_needed
            .store(true, Ordering::Release);
    }

    /// Called when a thread's affinity mask changes while it is Runnable.
    /// Matches upstream `KScheduler::OnThreadAffinityMaskChanged`.
    pub fn on_thread_affinity_changed(
        &mut self,
        thread_id: u64,
        prev_core: i32,
        prev_affinity: u64,
        new_core: i32,
        new_affinity: u64,
        priority: i32,
        is_dummy: bool,
    ) {
        self.m_priority_queue.change_affinity_mask(
            prev_core,
            prev_affinity,
            thread_id,
            new_core,
            new_affinity,
            priority,
            is_dummy,
        );
        self.m_priority_queue.increment_scheduled_count(thread_id);
        self.m_scheduler_update_needed
            .store(true, std::sync::atomic::Ordering::Release);
    }

    // -- Legacy PQ operations (for callers that pass properties directly) --

    pub fn push_back_to_priority_queue(
        &mut self,
        thread_id: u64,
        priority: i32,
        active_core: i32,
        affinity: u64,
        is_dummy: bool,
        process_schedule_count: Option<std::sync::Arc<std::sync::atomic::AtomicI64>>,
        last_scheduled_tick: i64,
    ) {
        self.m_priority_queue.push_back(
            thread_id,
            priority,
            active_core,
            affinity,
            is_dummy,
            process_schedule_count,
        );
        self.m_priority_queue
            .set_last_scheduled_tick(thread_id, last_scheduled_tick);
        self.m_scheduler_update_needed
            .store(true, std::sync::atomic::Ordering::Release);
    }

    pub fn remove_from_priority_queue(
        &mut self,
        thread_id: u64,
        priority: i32,
        active_core: i32,
        affinity: u64,
        is_dummy: bool,
    ) {
        self.m_priority_queue
            .remove(thread_id, priority, active_core, affinity, is_dummy);
        self.m_scheduler_update_needed
            .store(true, std::sync::atomic::Ordering::Release);
    }

    pub fn get_scheduled_front(&self, core: i32) -> Option<u64> {
        self.m_priority_queue.get_scheduled_front(core)
    }

    pub fn move_to_scheduled_back(
        &mut self,
        thread_id: u64,
        priority: i32,
        active_core: i32,
        is_dummy: bool,
    ) -> Option<u64> {
        let result = self.m_priority_queue.move_to_scheduled_back(
            thread_id,
            priority,
            active_core,
            is_dummy,
        );
        self.m_scheduler_update_needed
            .store(true, std::sync::atomic::Ordering::Release);
        result
    }

    pub fn get_scheduled_next(&self, core: i32, thread_id: u64, priority: i32) -> Option<u64> {
        self.m_priority_queue
            .get_scheduled_next(core, thread_id, priority)
    }

    // -- PreemptThreads --

    pub fn preempt_threads(&mut self, current_thread_id: Option<u64>) {
        for core_id in 0..self.hardware_profile.num_cpu_cores() {
            let priority = preemption_priorities(self.hardware_profile)[core_id as usize] as i32;
            super::k_scheduler::KScheduler::rotate_scheduled_queue(
                self,
                core_id as i32,
                priority,
                current_thread_id,
            );
        }
    }

    // -- Scheduler lock and state --

    pub fn is_locked(&self) -> bool {
        self.m_scheduler_lock.is_locked_by_current_thread()
    }

    pub fn scheduler_lock(&self) -> &KAbstractSchedulerLock {
        &self.m_scheduler_lock
    }

    pub fn register_dummy_thread_for_wakeup(&self, thread_id: u64, thread: Arc<KThreadLock>) {
        debug_assert!(self.is_locked());
        self.m_woken_dummy_threads
            .lock()
            .unwrap()
            .insert(thread_id, thread);
    }

    pub fn unregister_dummy_thread_for_wakeup(&self, thread_id: u64) {
        debug_assert!(self.is_locked());
        self.m_woken_dummy_threads
            .lock()
            .unwrap()
            .remove(&thread_id);
    }

    pub fn wakeup_waiting_dummy_threads(&self) {
        debug_assert!(self.is_locked());
        let threads = {
            let mut threads = self.m_woken_dummy_threads.lock().unwrap();
            std::mem::take(&mut *threads)
        };

        for thread in threads.into_values() {
            thread.lock().unwrap().dummy_thread_end_wait();
        }
    }

    /// Helper: extract PQ-relevant properties from a locked KThread.
    /// Used by callers that need to pass properties to on_thread_state_changed.
    pub fn extract_thread_props(thread: &KThread) -> (u64, i32, i32, u64, bool) {
        (
            thread.get_thread_id(),
            thread.priority,
            thread.core_id,
            thread.physical_affinity_mask.get_affinity_mask(),
            thread.thread_type == ThreadType::Dummy,
        )
    }
}

impl Default for GlobalSchedulerContext {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Arc;

    use super::{GlobalSchedulerContext, PREEMPTION_PRIORITIES};
    use crate::hle::kernel::k_thread::{KThread, KThreadLock, ThreadState};

    #[test]
    fn hardware_profiles_select_upstream_preemption_priorities() {
        assert_eq!(super::preemption_priorities(crate::hardware_properties::SWITCH1_PROFILE), &[59,59,59,63]);
        assert_eq!(super::preemption_priorities(crate::hardware_properties::NX2_PROFILE), &[59,59,59,63,59,59]);
    }

    #[test]
    fn get_thread_by_thread_id_does_not_need_thread_mutex() {
        let gsc = GlobalSchedulerContext::new();
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().thread_id = 0x1234;
        gsc.add_thread(Arc::clone(&thread));

        let _held = thread.lock().unwrap();
        let looked_up = gsc.get_thread_by_thread_id(0x1234);
        assert!(looked_up.is_some());
    }

    #[test]
    fn remove_thread_only_removes_matching_thread_id() {
        let gsc = GlobalSchedulerContext::new();
        let exiting = Arc::new(KThreadLock::new(KThread::new()));
        let survivor = Arc::new(KThreadLock::new(KThread::new()));
        exiting.lock().unwrap().thread_id = 0x10;
        survivor.lock().unwrap().thread_id = 0x20;

        gsc.add_thread(Arc::clone(&exiting));
        gsc.add_thread(Arc::clone(&survivor));
        gsc.remove_thread(0x10);

        assert!(gsc.get_thread_by_thread_id(0x10).is_none());
        assert!(Arc::ptr_eq(
            &gsc.get_thread_by_thread_id(0x20).unwrap(),
            &survivor
        ));
    }

    #[test]
    fn dummy_state_transitions_hold_thread_directly_until_scheduler_unlock() {
        let mut gsc = GlobalSchedulerContext::new();
        let thread = Arc::new(KThreadLock::new(KThread::new()));

        gsc.m_scheduler_lock.lock();
        gsc.on_thread_state_changed(
            0x33,
            ThreadState::WAITING,
            ThreadState::RUNNABLE,
            65,
            3,
            0b1000,
            true,
            Some(Arc::clone(&thread)),
            None,
            0,
        );

        assert!(gsc.get_thread_list().is_empty());
        assert_eq!(Arc::strong_count(&thread), 2);

        gsc.on_thread_state_changed(
            0x33,
            ThreadState::RUNNABLE,
            ThreadState::WAITING,
            65,
            3,
            0b1000,
            true,
            Some(Arc::clone(&thread)),
            None,
            0,
        );
        assert_eq!(Arc::strong_count(&thread), 1);

        gsc.on_thread_state_changed(
            0x33,
            ThreadState::WAITING,
            ThreadState::RUNNABLE,
            65,
            3,
            0b1000,
            true,
            Some(Arc::clone(&thread)),
            None,
            0,
        );
        gsc.wakeup_waiting_dummy_threads();
        gsc.m_scheduler_lock.unlock();

        assert!(gsc.m_woken_dummy_threads.lock().unwrap().is_empty());
        assert_eq!(Arc::strong_count(&thread), 1);
    }

    #[test]
    fn state_change_remove_increments_process_scheduled_count_after_props_are_removed() {
        let mut gsc = GlobalSchedulerContext::new();
        let scheduled_count = Arc::new(AtomicI64::new(0));

        gsc.on_thread_state_changed(
            100,
            ThreadState::WAITING,
            ThreadState::RUNNABLE,
            44,
            2,
            0b0100,
            false,
            None,
            Some(Arc::clone(&scheduled_count)),
            0,
        );
        assert_eq!(scheduled_count.load(Ordering::Relaxed), 1);

        gsc.on_thread_state_changed(
            100,
            ThreadState::RUNNABLE,
            ThreadState::WAITING,
            44,
            2,
            0b0100,
            false,
            None,
            None,
            0,
        );

        assert_eq!(scheduled_count.load(Ordering::Relaxed), 2);
        assert!(gsc.m_priority_queue.get_thread_props(100).is_none());
    }

    #[test]
    fn runnable_priority_change_increments_process_scheduled_count() {
        let mut gsc = GlobalSchedulerContext::new();
        let scheduled_count = Arc::new(AtomicI64::new(0));

        gsc.on_thread_state_changed(
            100,
            ThreadState::WAITING,
            ThreadState::RUNNABLE,
            44,
            2,
            0b0100,
            false,
            None,
            Some(Arc::clone(&scheduled_count)),
            0,
        );
        gsc.on_thread_priority_changed(100, 44, 29, 2, 0b0100, false, false, 100);

        assert_eq!(scheduled_count.load(Ordering::Relaxed), 2);
        assert_eq!(
            gsc.m_priority_queue.get_thread_props(100).unwrap().priority,
            29
        );
    }

    #[test]
    fn preempt_threads_rotates_each_core_queue_and_requests_scheduler_update() {
        let mut gsc = GlobalSchedulerContext::new();
        let scheduled_count = Arc::new(AtomicI64::new(0));
        let priority = PREEMPTION_PRIORITIES[0] as i32;

        gsc.m_priority_queue.push_back(
            100,
            priority,
            0,
            0b0001,
            false,
            Some(Arc::clone(&scheduled_count)),
        );
        gsc.m_priority_queue.push_back(
            101,
            priority,
            0,
            0b0001,
            false,
            Some(Arc::clone(&scheduled_count)),
        );

        assert_eq!(
            gsc.m_priority_queue
                .get_scheduled_front_at_priority(0, priority),
            Some(100)
        );
        assert!(!gsc.m_scheduler_update_needed.load(Ordering::Acquire));

        gsc.preempt_threads(None);

        assert_eq!(
            gsc.m_priority_queue
                .get_scheduled_front_at_priority(0, priority),
            Some(101)
        );
        assert_eq!(scheduled_count.load(Ordering::Relaxed), 2);
        assert!(gsc.m_scheduler_update_needed.load(Ordering::Acquire));
    }
}
