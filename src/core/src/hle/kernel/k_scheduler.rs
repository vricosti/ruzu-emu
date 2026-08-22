//! Port of zuyu/src/core/hle/kernel/k_scheduler.h / k_scheduler.cpp
//! Status: Partial (structural port, complex methods stubbed)
//! Derniere synchro: 2026-03-11
//!
//! KScheduler: per-core scheduler managing thread dispatch and context switching.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread;
use std::time::Duration;

use common::fiber::Fiber;

use super::k_process::ProcessLock;
use super::k_thread::KThreadLock;
use super::k_thread::ThreadState;

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

fn should_trace_sched_pick() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_SCHED_PICK").is_some())
}

fn encode_trace_tid(thread_id: Option<u64>) -> u64 {
    thread_id.unwrap_or(u64::MAX)
}

pub(crate) fn trace_tid_filter_matches(thread_id: Option<u64>) -> bool {
    let Some(thread_id) = thread_id else {
        return false;
    };
    let Some(filter) = trace_sched_state_filter() else {
        return true;
    };
    filter.is_empty() || filter.contains(&thread_id)
}

fn should_log_sched_pick_for(ids: &[Option<u64>]) -> bool {
    should_trace_sched_pick() && ids.iter().copied().any(trace_tid_filter_matches)
}

fn trace_sched_fiber_event(
    stage: u64,
    core_id: i32,
    cur_thread_id: Option<u64>,
    target_thread: Option<&Arc<KThreadLock>>,
    has_host_context: bool,
    needs_scheduling: bool,
) {
    let target_info = target_thread.map(|thread| {
        let guard = thread.lock().unwrap();
        (
            guard.get_thread_id(),
            guard.get_active_core(),
            guard.get_current_core(),
            guard.get_priority(),
            guard
                .get_host_context()
                .map(|ctx| Arc::as_ptr(ctx) as usize as u64)
                .unwrap_or(0),
        )
    });
    let target_id = target_info.map(|(id, _, _, _, _)| id);
    if !common::trace::is_enabled(common::trace::cat::SCHED_STATE)
        || !(trace_tid_filter_matches(cur_thread_id) || trace_tid_filter_matches(target_id))
    {
        return;
    }
    let (target_id, active_core, current_core, priority, target_host_context) =
        target_info.unwrap_or((u64::MAX, -1, -1, 0, 0));
    common::trace::emit_raw(
        common::trace::cat::SCHED_STATE,
        &[
            stage,
            encode_trace_tid(cur_thread_id),
            encode_trace_tid(Some(target_id)),
            core_id as u32 as u64,
            active_core as u32 as u64,
            current_core as u32 as u64,
            priority as u32 as u64,
            has_host_context as u64,
            needs_scheduling as u64,
            target_host_context,
        ],
    );
}

fn trace_reschedule_path(source: u64, phase: u64, scheduler_core: i32, needs_scheduling: bool) {
    static TRACE_COUNT: AtomicU64 = AtomicU64::new(0);
    let current_thread_id = super::kernel::get_current_thread_id_fast();
    if !common::trace::is_enabled(common::trace::cat::SCHED_STATE)
        || !trace_tid_filter_matches(current_thread_id)
    {
        return;
    }

    let host_core = super::kernel::get_kernel_ref()
        .map(|kernel| kernel.current_physical_core_index() as i32)
        .unwrap_or(-1);
    if scheduler_core == host_core && TRACE_COUNT.fetch_add(1, Ordering::Relaxed) >= 4096 {
        return;
    }
    common::trace::emit_raw(
        common::trace::cat::SCHED_STATE,
        &[
            22,
            source,
            phase,
            scheduler_core as u32 as u64,
            host_core as u32 as u64,
            encode_trace_tid(current_thread_id),
            needs_scheduling as u64,
        ],
    );
}

fn context_guard_site_id(site: &'static str) -> u64 {
    match site {
        "switch_fiber" => 1,
        "unload" => 2,
        "runtime_init" => 3,
        "runtime_unlock" => 4,
        "switch_retry" => 5,
        _ => 0,
    }
}

fn trace_context_guard_event(
    action: u64,
    site: &'static str,
    thread_id: u64,
    requester_core: i32,
    owner: i32,
    current_core: i32,
    active_core: i32,
) {
    if !common::trace::is_enabled(common::trace::cat::SCHED_STATE)
        || !trace_tid_filter_matches(Some(thread_id))
    {
        return;
    }

    common::trace::emit_raw(
        common::trace::cat::SCHED_STATE,
        &[
            19,
            action,
            context_guard_site_id(site),
            thread_id,
            requester_core as u32 as u64,
            owner as u32 as u64,
            current_core as u32 as u64,
            active_core as u32 as u64,
        ],
    );
}

fn scheduled_front_with_pinned_thread(
    gsc: &mut super::global_scheduler_context::GlobalSchedulerContext,
    core_id: i32,
) -> Option<u64> {
    let mut top_thread_id = gsc.get_scheduled_front(core_id);

    // Upstream UpdateHighestPriorityThreadsImpl: if the top thread's
    // process has a pinned thread for this core, prefer that pinned thread
    // unless the normal top thread has kernel waiters.
    if let Some(top_id) = top_thread_id {
        let pinned_id = gsc.get_thread_by_thread_id(top_id).and_then(|top_thread| {
            let top_guard = top_thread.lock().unwrap();
            if top_guard.get_num_kernel_waiters() != 0 {
                return None;
            }
            let parent = top_guard.parent.as_ref().and_then(|w| w.upgrade())?;
            drop(top_guard);

            parent.lock().unwrap().get_pinned_thread(core_id)
        });

        if let Some(pinned_id) = pinned_id {
            if pinned_id != top_id {
                let pinned_is_runnable = gsc
                    .get_thread_by_thread_id(pinned_id)
                    .map(|pinned_thread| {
                        pinned_thread.lock().unwrap().get_raw_state() == ThreadState::RUNNABLE
                    })
                    .unwrap_or(false);
                top_thread_id = if pinned_is_runnable {
                    Some(pinned_id)
                } else {
                    None
                };
            }
        }
    }

    top_thread_id
}

/// Scheduling state held per-core.
/// Matches upstream `KScheduler::SchedulingState` (k_scheduler.h).
pub struct SchedulingState {
    pub needs_scheduling: AtomicBool,
    pub interrupt_task_runnable: bool,
    pub should_count_idle: bool,
    pub idle_count: u64,
    pub highest_priority_thread_id: Option<u64>,
    pub idle_thread_stack: usize, // void* — opaque
    pub prev_thread_id: Option<u64>,
    // interrupt_task_manager — opaque
}

impl Default for SchedulingState {
    fn default() -> Self {
        Self {
            needs_scheduling: AtomicBool::new(false),
            interrupt_task_runnable: false,
            should_count_idle: false,
            idle_count: 0,
            highest_priority_thread_id: None,
            idle_thread_stack: 0,
            prev_thread_id: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::kernel::global_scheduler_context::GlobalSchedulerContext;
    use crate::hle::kernel::k_process::KProcess;
    use crate::hle::kernel::k_thread::KThread;

    #[test]
    fn thread_context_guard_stays_locked_until_explicit_unlock() {
        let thread = Arc::new(KThreadLock::new(KThread::new()));

        assert!(!KScheduler::unlock_thread_context(&thread, 0));
        assert_eq!(
            thread
                .lock()
                .unwrap()
                .context_guard_owner
                .load(Ordering::SeqCst),
            super::super::k_thread::CONTEXT_GUARD_UNOWNED
        );

        assert!(KScheduler::try_lock_thread_context(&thread, 0));
        assert!(!KScheduler::try_lock_thread_context(&thread, 1));
        assert_eq!(
            thread
                .lock()
                .unwrap()
                .context_guard_owner
                .load(Ordering::SeqCst),
            0
        );

        assert!(!KScheduler::unlock_thread_context(&thread, 1));

        assert!(KScheduler::unlock_thread_context(&thread, 0));
        assert_eq!(
            thread
                .lock()
                .unwrap()
                .context_guard_owner
                .load(Ordering::SeqCst),
            super::super::k_thread::CONTEXT_GUARD_UNOWNED
        );
    }

    #[test]
    fn schedule_impl_fiber_keeps_idle_handoff_when_highest_is_none() {
        let current_thread = Arc::new(KThreadLock::new(KThread::new()));
        current_thread.lock().unwrap().thread_id = 42;

        let mut scheduler = KScheduler::new(0);
        scheduler.current_thread = Some(Arc::downgrade(&current_thread));
        scheduler.current_thread_id = Some(42);
        scheduler.state.highest_priority_thread_id = None;
        scheduler.state.interrupt_task_runnable = false;

        scheduler.schedule_impl_fiber();

        assert!(scheduler.switch_cur_thread.is_some());
        assert!(scheduler.switch_highest_priority_thread.is_none());
        assert!(scheduler.switch_from_schedule);
    }

    #[test]
    fn late_scheduler_update_ignores_stale_captured_highest_thread() {
        let stale_thread = Arc::new(KThreadLock::new(KThread::new()));
        stale_thread.lock().unwrap().thread_id = 75;

        let gsc = Arc::new(Mutex::new(GlobalSchedulerContext::new()));
        gsc.lock().unwrap().add_thread(Arc::clone(&stale_thread));

        let mut scheduler = KScheduler::new(0);
        scheduler.global_scheduler_context = Some(gsc);
        scheduler.state.highest_priority_thread_id = None;
        scheduler.switch_highest_priority_thread = Some(Arc::downgrade(&stale_thread));

        assert!(scheduler
            .switch_highest_priority_thread
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some());
        assert!(scheduler
            .resolve_highest_priority_thread_from_state()
            .is_none());
    }

    #[test]
    fn switch_thread_impl_uses_idle_thread_without_gsc_membership() {
        let current_thread = Arc::new(KThreadLock::new(KThread::new()));
        current_thread.lock().unwrap().thread_id = 42;

        let idle_thread = Arc::new(KThreadLock::new(KThread::new()));
        idle_thread.lock().unwrap().thread_id = 7;

        let mut scheduler = KScheduler::new(0);
        scheduler.current_thread = Some(Arc::downgrade(&current_thread));
        scheduler.current_thread_id = Some(42);
        scheduler.idle_thread = Some(Arc::downgrade(&idle_thread));
        scheduler.idle_thread_id = Some(7);

        scheduler.switch_thread_impl(None, 7);

        assert_eq!(scheduler.current_thread_id, Some(7));
        let resolved = scheduler
            .current_thread
            .as_ref()
            .and_then(Weak::upgrade)
            .unwrap();
        assert!(Arc::ptr_eq(&resolved, &idle_thread));
    }

    #[test]
    fn switch_thread_impl_preserves_previous_thread_current_core() {
        let current_thread = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut guard = current_thread.lock().unwrap();
            guard.thread_id = 42;
            guard.set_current_core(1);
        }

        let next_thread = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut guard = next_thread.lock().unwrap();
            guard.thread_id = 43;
            guard.set_current_core(-1);
        }

        let gsc = Arc::new(Mutex::new(GlobalSchedulerContext::new()));
        gsc.lock().unwrap().add_thread(Arc::clone(&next_thread));

        let mut scheduler = KScheduler::new(1);
        scheduler.global_scheduler_context = Some(gsc);
        scheduler.current_thread = Some(Arc::downgrade(&current_thread));
        scheduler.current_thread_id = Some(42);

        scheduler.switch_thread_impl(None, 43);

        assert_eq!(current_thread.lock().unwrap().get_current_core(), 1);
        assert_eq!(next_thread.lock().unwrap().get_current_core(), 1);
    }

    #[test]
    fn schedule_impl_fiber_fast_path_repairs_stale_current_thread_id() {
        let current_thread = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut guard = current_thread.lock().unwrap();
            guard.thread_id = 75;
            guard.set_current_core(0);
            guard.set_active_core(0);
        }

        let stale_thread = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut guard = stale_thread.lock().unwrap();
            guard.thread_id = 104;
            guard.set_current_core(0);
            guard.set_active_core(0);
        }

        let gsc = Arc::new(Mutex::new(GlobalSchedulerContext::new()));
        gsc.lock().unwrap().add_thread(Arc::clone(&current_thread));

        let mut scheduler = KScheduler::new(0);
        scheduler.global_scheduler_context = Some(gsc);
        scheduler.current_thread = Some(Arc::downgrade(&stale_thread));
        scheduler.current_thread_id = Some(104);
        scheduler.state.highest_priority_thread_id = Some(75);
        scheduler
            .state
            .needs_scheduling
            .store(true, Ordering::Relaxed);

        crate::hle::kernel::kernel::set_current_emu_thread(Some(&current_thread));
        scheduler.schedule_impl_fiber();
        crate::hle::kernel::kernel::set_current_emu_thread(None);

        assert_eq!(scheduler.current_thread_id, Some(75));
        let resolved = scheduler
            .current_thread
            .as_ref()
            .and_then(Weak::upgrade)
            .unwrap();
        assert!(Arc::ptr_eq(&resolved, &current_thread));
    }

    #[test]
    fn schedule_impl_fiber_creates_switch_fiber_lazily() {
        let mut scheduler = KScheduler::new(0);

        assert!(scheduler.switch_fiber.is_none());

        scheduler.schedule_impl_fiber();

        assert!(scheduler.switch_fiber.is_some());
    }

    #[test]
    fn update_highest_priority_threads_impl_migrates_suggested_thread_to_idle_core() {
        let mut gsc = GlobalSchedulerContext::new();
        let migrated = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut guard = migrated.lock().unwrap();
            guard.thread_id = 1;
            guard.object_id = 1;
            guard.priority = 10;
            guard.core_id = 0;
            guard.physical_affinity_mask.set_affinity_mask(0b0011);
            guard.set_state(ThreadState::RUNNABLE);
        }
        gsc.add_thread(Arc::clone(&migrated));

        gsc.m_priority_queue
            .push_back(1, 10, 0, 0b0011, false, None);
        gsc.m_priority_queue
            .push_back(2, 20, 0, 0b0001, false, None);

        let scheduler_arcs: Vec<_> = (0..crate::hardware_properties::NUM_CPU_CORES)
            .map(|core_id| Mutex::new(KScheduler::new(core_id as i32)))
            .collect();
        let mut schedulers: Vec<_> = scheduler_arcs.iter().map(|s| s.lock().unwrap()).collect();

        gsc.m_scheduler_lock.lock();
        let (cores_needing_scheduling, migrations) =
            KScheduler::update_highest_priority_threads_impl(&mut schedulers, &mut gsc);
        gsc.m_scheduler_lock.unlock();

        assert!(migrations.is_empty());
        assert_eq!(migrated.lock().unwrap().get_active_core(), 1);
        assert_eq!(
            gsc.m_priority_queue
                .get_thread_props(1)
                .map(|props| props.active_core),
            Some(1)
        );
        assert_eq!(schedulers[0].state.highest_priority_thread_id, Some(2));
        assert_eq!(schedulers[1].state.highest_priority_thread_id, Some(1));
        assert_ne!(cores_needing_scheduling & (1 << 1), 0);
    }

    #[test]
    #[should_panic(expected = "assertion failed: gsc.is_locked()")]
    fn update_highest_priority_threads_impl_requires_scheduler_lock() {
        let mut gsc = GlobalSchedulerContext::new();
        let scheduler_arcs: Vec<_> = (0..crate::hardware_properties::NUM_CPU_CORES)
            .map(|core_id| Mutex::new(KScheduler::new(core_id as i32)))
            .collect();
        let mut schedulers: Vec<_> = scheduler_arcs.iter().map(|s| s.lock().unwrap()).collect();

        let _ = KScheduler::update_highest_priority_threads_impl(&mut schedulers, &mut gsc);
    }

    #[test]
    fn update_highest_priority_thread_records_idle_and_running_thread() {
        let process = Arc::new(crate::hle::kernel::k_process::ProcessLock::from_value(
            crate::hle::kernel::k_process::KProcess::new(),
        ));
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut thread = thread.lock().unwrap();
            thread.thread_id = 42;
            thread.parent = Some(Arc::downgrade(&process));
        }

        let mut gsc = GlobalSchedulerContext::new();
        gsc.add_thread(thread);
        let mut scheduler = KScheduler::new(1);
        scheduler.state.should_count_idle = true;

        assert_eq!(scheduler.update_highest_priority_thread(None, &mut gsc), 0);
        scheduler.state.highest_priority_thread_id = Some(7);
        assert_ne!(scheduler.update_highest_priority_thread(None, &mut gsc), 0);
        assert_eq!(scheduler.state.idle_count, 1);

        assert_ne!(
            scheduler.update_highest_priority_thread(Some(42), &mut gsc),
            0
        );
        let process = process.lock().unwrap();
        assert_eq!(process.running_threads[1], Some(42));
        assert_eq!(process.running_thread_idle_counts[1], 1);
        assert_eq!(process.running_thread_switch_counts[1], 0);
    }

    #[test]
    fn rotate_scheduled_queue_migrates_same_priority_suggestion_to_front() {
        let mut gsc = GlobalSchedulerContext::new();
        let priority = 59;

        gsc.m_priority_queue
            .push_back(100, priority, 0, 0b0001, false, None);
        gsc.m_priority_queue
            .push_back(101, priority, 0, 0b0001, false, None);
        gsc.m_priority_queue
            .push_back(200, 10, 1, 0b0010, false, None);
        gsc.m_priority_queue
            .push_back(201, priority, 1, 0b0011, false, None);

        KScheduler::rotate_scheduled_queue(&mut gsc, 0, priority, None);

        assert_eq!(
            gsc.m_priority_queue
                .get_scheduled_front_at_priority(0, priority),
            Some(201)
        );
        assert_eq!(
            gsc.m_priority_queue
                .get_thread_props(201)
                .map(|props| props.active_core),
            Some(0)
        );
    }

    #[test]
    fn rotate_scheduled_queue_prefers_next_thread_that_waited_longer() {
        let mut gsc = GlobalSchedulerContext::new();
        let priority = 59;

        gsc.m_priority_queue
            .push_back(100, priority, 0, 0b0001, false, None);
        gsc.m_priority_queue
            .push_back(101, priority, 0, 0b0001, false, None);
        gsc.m_priority_queue
            .push_back(200, 10, 1, 0b0010, false, None);
        gsc.m_priority_queue
            .push_back(201, priority, 1, 0b0011, false, None);
        gsc.m_priority_queue.set_last_scheduled_tick(101, 1);
        gsc.m_priority_queue.set_last_scheduled_tick(201, 10);

        KScheduler::rotate_scheduled_queue(&mut gsc, 0, priority, None);

        assert_eq!(
            gsc.m_priority_queue
                .get_scheduled_front_at_priority(0, priority),
            Some(101)
        );
        assert_eq!(
            gsc.m_priority_queue
                .get_thread_props(201)
                .map(|props| props.active_core),
            Some(1)
        );
    }

    #[test]
    fn rotate_scheduled_queue_excludes_current_before_higher_priority_migration() {
        let mut gsc = GlobalSchedulerContext::new();
        let priority = 59;

        gsc.m_priority_queue
            .push_back(100, 10, 0, 0b0001, false, None);
        gsc.m_priority_queue
            .push_back(101, priority, 0, 0b0001, false, None);
        gsc.m_priority_queue
            .push_back(200, 10, 1, 0b0010, false, None);
        gsc.m_priority_queue
            .push_back(201, 20, 1, 0b0011, false, None);

        KScheduler::rotate_scheduled_queue(&mut gsc, 0, priority, Some(100));

        assert_eq!(
            gsc.m_priority_queue.get_scheduled_front_at_priority(0, 20),
            Some(201)
        );
        assert_eq!(
            gsc.m_priority_queue
                .get_thread_props(201)
                .map(|props| props.active_core),
            Some(0)
        );
    }

    #[test]
    fn yield_with_core_migration_moves_suggestion_to_front_like_upstream() {
        let mut gsc = GlobalSchedulerContext::new();
        let suggested = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut guard = suggested.lock().unwrap();
            guard.thread_id = 10;
            guard.object_id = 10;
            guard.priority = 20;
            guard.core_id = 2;
            guard.physical_affinity_mask.set_affinity_mask(0b0110);
            guard.set_state(ThreadState::RUNNABLE);
        }
        gsc.add_thread(Arc::clone(&suggested));

        gsc.m_priority_queue
            .push_back(10, 20, 2, 0b0110, false, None);
        suggested.lock().unwrap().set_active_core(1);
        gsc.m_priority_queue.change_core(2, 10, 1, 20, false, true);

        assert_eq!(suggested.lock().unwrap().get_active_core(), 1);
        assert_eq!(gsc.m_priority_queue.get_scheduled_front(1), Some(10));
        assert_eq!(
            gsc.m_priority_queue
                .get_thread_props(10)
                .map(|props| props.active_core),
            Some(1)
        );
    }

    #[test]
    fn yield_to_any_thread_moves_current_to_no_active_core_like_upstream() {
        let mut gsc = GlobalSchedulerContext::new();
        let current = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut guard = current.lock().unwrap();
            guard.thread_id = 11;
            guard.object_id = 11;
            guard.priority = 30;
            guard.core_id = 1;
            guard.physical_affinity_mask.set_affinity_mask(0b1110);
            guard.set_state(ThreadState::RUNNABLE);
        }
        gsc.add_thread(Arc::clone(&current));

        gsc.m_priority_queue
            .push_back(11, 30, 1, 0b1110, false, None);
        current.lock().unwrap().set_active_core(-1);
        gsc.m_priority_queue
            .change_core(1, 11, -1, 30, false, false);

        assert_eq!(current.lock().unwrap().get_active_core(), -1);
        assert_ne!(gsc.m_priority_queue.get_scheduled_front(1), Some(11));
        assert_eq!(gsc.m_priority_queue.get_suggested_front(1), Some(11));
        assert_eq!(
            gsc.m_priority_queue
                .get_thread_props(11)
                .map(|props| props.active_core),
            Some(-1)
        );
    }

    #[test]
    fn scheduled_front_prefers_runnable_pinned_thread_like_upstream() {
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let top = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut guard = top.lock().unwrap();
            guard.thread_id = 1;
            guard.object_id = 1;
            guard.priority = 30;
            guard.core_id = 2;
            guard.parent = Some(Arc::downgrade(&process));
            guard.set_state(ThreadState::RUNNABLE);
        }

        let pinned = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut guard = pinned.lock().unwrap();
            guard.thread_id = 2;
            guard.object_id = 2;
            guard.priority = 44;
            guard.core_id = 2;
            guard.parent = Some(Arc::downgrade(&process));
            guard.set_state(ThreadState::RUNNABLE);
        }

        {
            let mut process_guard = process.lock().unwrap();
            process_guard.register_thread_object(Arc::clone(&top));
            process_guard.register_thread_object(Arc::clone(&pinned));
            process_guard.pinned_threads[2] = Some(2);
        }

        let mut gsc = GlobalSchedulerContext::new();
        gsc.add_thread(Arc::clone(&top));
        gsc.add_thread(Arc::clone(&pinned));
        gsc.m_priority_queue
            .push_back(1, 30, 2, 0b0100, false, None);

        assert_eq!(scheduled_front_with_pinned_thread(&mut gsc, 2), Some(2));
    }

    #[test]
    fn non_runnable_pinned_thread_does_not_make_scheduled_core_idle() {
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let top = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut guard = top.lock().unwrap();
            guard.thread_id = 1;
            guard.object_id = 1;
            guard.priority = 30;
            guard.core_id = 2;
            guard.parent = Some(Arc::downgrade(&process));
            guard.physical_affinity_mask.set_affinity_mask(0b0100);
            guard.set_state(ThreadState::RUNNABLE);
        }

        let pinned = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut guard = pinned.lock().unwrap();
            guard.thread_id = 2;
            guard.object_id = 2;
            guard.priority = 44;
            guard.core_id = 2;
            guard.parent = Some(Arc::downgrade(&process));
            guard.physical_affinity_mask.set_affinity_mask(0b0100);
            guard.set_state(ThreadState::WAITING);
        }

        let core_zero_top = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut guard = core_zero_top.lock().unwrap();
            guard.thread_id = 3;
            guard.object_id = 3;
            guard.priority = 10;
            guard.core_id = 0;
            guard.physical_affinity_mask.set_affinity_mask(0b0001);
            guard.set_state(ThreadState::RUNNABLE);
        }

        let movable = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut guard = movable.lock().unwrap();
            guard.thread_id = 4;
            guard.object_id = 4;
            guard.priority = 20;
            guard.core_id = 0;
            guard.physical_affinity_mask.set_affinity_mask(0b0101);
            guard.set_state(ThreadState::RUNNABLE);
        }

        {
            let mut process_guard = process.lock().unwrap();
            process_guard.register_thread_object(Arc::clone(&top));
            process_guard.register_thread_object(Arc::clone(&pinned));
            process_guard.pinned_threads[2] = Some(2);
        }

        let mut gsc = GlobalSchedulerContext::new();
        for thread in [&top, &pinned, &core_zero_top, &movable] {
            gsc.add_thread(Arc::clone(thread));
        }
        gsc.m_priority_queue
            .push_back(1, 30, 2, 0b0100, false, None);
        gsc.m_priority_queue
            .push_back(3, 10, 0, 0b0001, false, None);
        gsc.m_priority_queue
            .push_back(4, 20, 0, 0b0101, false, None);

        let scheduler_arcs: Vec<_> = (0..crate::hardware_properties::NUM_CPU_CORES)
            .map(|core_id| Mutex::new(KScheduler::new(core_id as i32)))
            .collect();
        let mut schedulers: Vec<_> = scheduler_arcs.iter().map(|s| s.lock().unwrap()).collect();

        gsc.m_scheduler_lock.lock();
        KScheduler::update_highest_priority_threads_impl(&mut schedulers, &mut gsc);
        gsc.m_scheduler_lock.unlock();

        assert_eq!(schedulers[2].state.highest_priority_thread_id, None);
        assert_eq!(movable.lock().unwrap().get_active_core(), 0);
        assert_eq!(
            gsc.m_priority_queue
                .get_thread_props(4)
                .map(|props| props.active_core),
            Some(0)
        );
    }

    #[test]
    fn update_highest_priority_threads_impl_requests_wait_for_non_runnable_dummy_current_thread() {
        let current_thread = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut guard = current_thread.lock().unwrap();
            guard.initialize_dummy_thread(None, 99, 99);
            guard.set_state(ThreadState::WAITING);
            guard.dummy_thread_runnable.store(true, Ordering::Relaxed);
        }
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&current_thread));

        let mut gsc = GlobalSchedulerContext::new();
        let scheduler_arcs: Vec<_> = (0..crate::hardware_properties::NUM_CPU_CORES)
            .map(|core_id| Mutex::new(KScheduler::new(core_id as i32)))
            .collect();
        let mut schedulers: Vec<_> = scheduler_arcs.iter().map(|s| s.lock().unwrap()).collect();

        gsc.m_scheduler_lock.lock();
        let _ = KScheduler::update_highest_priority_threads_impl(&mut schedulers, &mut gsc);
        gsc.m_scheduler_lock.unlock();

        assert!(!current_thread
            .lock()
            .unwrap()
            .dummy_thread_runnable
            .load(Ordering::Relaxed));

        crate::hle::kernel::kernel::set_current_emu_thread(None);
    }

    #[test]
    fn scan_runnable_threads_respects_core_affinity() {
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let scheduler = KScheduler::new(0);

        let local = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut guard = local.lock().unwrap();
            guard.thread_id = 17;
            guard.object_id = 17;
            guard.priority = 44;
            guard.core_id = 0;
            guard.physical_affinity_mask.set_affinity_mask(0b0001);
            guard.set_state(ThreadState::RUNNABLE);
        }

        let foreign = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut guard = foreign.lock().unwrap();
            guard.thread_id = 2;
            guard.object_id = 2;
            guard.priority = 16;
            guard.core_id = 3;
            guard.physical_affinity_mask.set_affinity_mask(0b1000);
            guard.set_state(ThreadState::RUNNABLE);
        }

        {
            let mut process_guard = process.lock().unwrap();
            process_guard.register_thread_object(local);
            process_guard.register_thread_object(foreign);
        }

        assert_eq!(scheduler.scan_runnable_threads(&process), Some(17));
    }

    #[test]
    fn enable_scheduling_defers_switch_for_nested_non_runnable_current_thread() {
        let scheduler = Arc::new(Mutex::new(KScheduler::new(0)));
        scheduler
            .lock()
            .unwrap()
            .state
            .needs_scheduling
            .store(true, Ordering::Relaxed);
        let current_thread = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut thread = current_thread.lock().unwrap();
            thread.thread_id = 99;
            thread.disable_dispatch();
            thread.disable_dispatch();
            thread.set_state(ThreadState::WAITING);
        }

        crate::hle::kernel::kernel::set_current_emu_thread(Some(&current_thread));
        KScheduler::enable_scheduling_with_scheduler(0, Some(&scheduler), false);

        {
            let thread = current_thread.lock().unwrap();
            assert_eq!(thread.get_disable_dispatch_count(), 1);
            assert_eq!(thread.get_state(), ThreadState::WAITING);
        }
        assert!(scheduler.lock().unwrap().needs_scheduling());

        crate::hle::kernel::kernel::set_current_emu_thread(None);
    }
}

/// The per-core kernel scheduler.
/// Matches upstream `KScheduler` class (k_scheduler.h).
pub struct KScheduler {
    pub state: SchedulingState,
    pub is_active: bool,
    pub core_id: i32,
    pub last_context_switch_time: i64,
    pub idle_thread_id: Option<u64>,
    pub idle_thread: Option<Weak<KThreadLock>>,
    pub current_thread_id: Option<u64>,
    pub current_thread: Option<Weak<KThreadLock>>,
    pub yielded_thread_id: Option<u64>,

    // Kernel references — upstream stores KernelCore& m_kernel
    pub global_scheduler_context:
        Option<Arc<Mutex<super::global_scheduler_context::GlobalSchedulerContext>>>,
    pub physical_cores: Vec<Arc<super::physical_core::PhysicalCore>>,
    pub core_timing: Option<Arc<crate::core_timing::CoreTiming>>,

    // Fiber fields for host-thread switching
    /// Upstream: `std::shared_ptr<Common::Fiber> m_switch_fiber`
    pub switch_fiber: Option<Arc<Fiber>>,
    pub switch_cur_thread: Option<Weak<KThreadLock>>,
    pub switch_highest_priority_thread: Option<Weak<KThreadLock>>,
    pub switch_from_schedule: bool,
}

impl KScheduler {
    fn resolve_thread_for_switch(&self, next_thread_id: u64) -> Option<Arc<KThreadLock>> {
        if self.idle_thread_id == Some(next_thread_id) {
            if let Some(idle_thread) = self.idle_thread.as_ref().and_then(Weak::upgrade) {
                return Some(idle_thread);
            }
        }

        self.global_scheduler_context
            .as_ref()
            .and_then(|gsc| gsc.lock().unwrap().get_thread_by_thread_id(next_thread_id))
    }

    fn trace_needs_scheduling_clear(
        &self,
        reason: u64,
        fiber_thread: Option<&Arc<KThreadLock>>,
        selected_thread: Option<&Arc<KThreadLock>>,
    ) {
        if !common::trace::is_enabled(common::trace::cat::SCHED_STATE) {
            return;
        }

        let scheduler_current = self.current_thread.as_ref().and_then(Weak::upgrade);
        let scheduler_current_info = scheduler_current.as_ref().map(|thread| {
            let thread = thread.lock().unwrap();
            (
                thread.get_thread_id(),
                thread.get_raw_state().bits(),
                thread.get_current_core(),
                thread.get_active_core(),
            )
        });
        let fiber_thread_id = fiber_thread.map(|thread| thread.lock().unwrap().get_thread_id());
        let selected_thread_id =
            selected_thread.map(|thread| thread.lock().unwrap().get_thread_id());

        if ![
            self.current_thread_id,
            scheduler_current_info.map(|info| info.0),
            fiber_thread_id,
            selected_thread_id,
            self.state.highest_priority_thread_id,
        ]
        .into_iter()
        .flatten()
        .any(|thread_id| trace_tid_filter_matches(Some(thread_id)))
        {
            return;
        }

        let (_, current_state, current_core, active_core) =
            scheduler_current_info.unwrap_or((u64::MAX, 0, -1, -1));
        common::trace::emit_raw(
            common::trace::cat::SCHED_STATE,
            &[
                18,
                reason,
                self.core_id as u32 as u64,
                encode_trace_tid(self.current_thread_id),
                encode_trace_tid(fiber_thread_id),
                encode_trace_tid(self.state.highest_priority_thread_id),
                encode_trace_tid(selected_thread_id),
                current_state as u64,
                current_core as u32 as u64,
                active_core as u32 as u64,
                self.switch_from_schedule as u64,
            ],
        );
    }

    fn current_thread_for_scheduler_core(&self) -> Option<Arc<KThreadLock>> {
        let tls_thread = super::kernel::get_current_thread_pointer();
        if let Some(thread) = tls_thread {
            let belongs_to_core = thread.lock().unwrap().get_current_core() == self.core_id;
            if belongs_to_core {
                return Some(thread);
            }
        }

        self.current_thread.as_ref().and_then(Weak::upgrade)
    }

    fn try_lock_thread_context(thread: &Arc<KThreadLock>, core_id: i32) -> bool {
        Self::try_lock_thread_context_at(thread, core_id, "switch_fiber")
    }

    fn try_lock_thread_context_at(
        thread: &Arc<KThreadLock>,
        core_id: i32,
        site: &'static str,
    ) -> bool {
        let thread_guard = thread.lock().unwrap();
        let thread_id = thread_guard.get_thread_id();
        let owner = thread_guard.context_guard_owner.load(Ordering::SeqCst);
        trace_context_guard_event(
            1,
            site,
            thread_id,
            core_id,
            owner,
            thread_guard.get_current_core(),
            thread_guard.get_active_core(),
        );
        if thread_guard
            .context_guard_owner
            .compare_exchange(
                super::k_thread::CONTEXT_GUARD_UNOWNED,
                core_id,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_err()
        {
            return false;
        }
        thread_guard.record_context_guard_event(true, site);
        trace_context_guard_event(
            2,
            site,
            thread_id,
            core_id,
            core_id,
            thread_guard.get_current_core(),
            thread_guard.get_active_core(),
        );
        true
    }

    fn unlock_thread_context(thread: &Arc<KThreadLock>, core_id: i32) -> bool {
        Self::unlock_thread_context_at(thread, core_id, "unload")
    }

    fn unlock_thread_context_at(
        thread: &Arc<KThreadLock>,
        core_id: i32,
        site: &'static str,
    ) -> bool {
        let thread_guard = thread.lock().unwrap();
        let thread_id = thread_guard.get_thread_id();
        let owner = thread_guard.context_guard_owner.load(Ordering::SeqCst);
        trace_context_guard_event(
            3,
            site,
            thread_id,
            core_id,
            owner,
            thread_guard.get_current_core(),
            thread_guard.get_active_core(),
        );
        if owner == super::k_thread::CONTEXT_GUARD_UNOWNED {
            return false;
        }
        if owner != core_id {
            trace_context_guard_event(
                4,
                site,
                thread_id,
                core_id,
                owner,
                thread_guard.get_current_core(),
                thread_guard.get_active_core(),
            );
            static LOGGED: AtomicBool = AtomicBool::new(false);
            if !LOGGED.swap(true, Ordering::Relaxed) {
                log::error!(
                    "unlock_thread_context({site}): refusing cross-core unlock tid={} requester={} owner={}",
                    thread_guard.get_thread_id(),
                    core_id,
                    owner,
                );
            }
            return false;
        }

        if thread_guard
            .context_guard_owner
            .compare_exchange(
                core_id,
                super::k_thread::CONTEXT_GUARD_UNOWNED,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_err()
        {
            return false;
        }
        thread_guard.record_context_guard_event(false, site);
        trace_context_guard_event(
            5,
            site,
            thread_id,
            core_id,
            super::k_thread::CONTEXT_GUARD_UNOWNED,
            thread_guard.get_current_core(),
            thread_guard.get_active_core(),
        );
        true
    }

    pub(crate) fn lock_thread_context_for_runtime(thread: &Arc<KThreadLock>, core_id: i32) -> bool {
        Self::try_lock_thread_context_at(thread, core_id, "runtime_init")
    }

    pub(crate) fn unlock_thread_context_for_runtime(
        thread: &Arc<KThreadLock>,
        core_id: i32,
    ) -> bool {
        Self::unlock_thread_context_at(thread, core_id, "runtime_unlock")
    }

    fn exit_thread_if_termination_requested(
        &self,
        process: &Arc<ProcessLock>,
        thread_id: u64,
    ) -> bool {
        let thread = {
            let process = process.lock().unwrap();
            process.get_thread_by_thread_id(thread_id)
        };
        let Some(thread) = thread else {
            return false;
        };

        if {
            let thread = thread.lock().unwrap();
            thread.is_termination_requested() && !thread.is_signaled()
        } {
            thread.lock().unwrap().exit();
            return true;
        }

        false
    }

    /// Create a new scheduler for the given core.
    pub fn new(core_id: i32) -> Self {
        Self {
            state: SchedulingState::default(),
            is_active: false,
            core_id,
            last_context_switch_time: 0,
            idle_thread_id: None,
            idle_thread: None,
            current_thread_id: None,
            current_thread: None,
            yielded_thread_id: None,
            global_scheduler_context: None,
            physical_cores: Vec::new(),
            core_timing: None,
            switch_fiber: None,
            switch_cur_thread: None,
            switch_highest_priority_thread: None,
            switch_from_schedule: false,
        }
    }

    /// Initialize the scheduler with main and idle threads (ID-only variant).
    /// Used by tests that don't need full thread references.
    pub fn initialize(&mut self, main_thread_id: u64, idle_thread_id: u64, core_id: i32) {
        self.core_id = core_id;
        self.idle_thread_id = Some(idle_thread_id);
        self.current_thread_id = Some(main_thread_id);
    }

    /// Initialize the scheduler with main and idle thread references.
    /// Matches upstream `KScheduler::Initialize(main_thread, idle_thread, core_id)`
    /// (k_scheduler.cpp:147-168).
    ///
    /// Sets `m_current_thread = main_thread` so that `get_scheduler_current_thread()`
    /// returns a valid thread with a host context for fiber switching.
    pub fn initialize_with_threads(
        &mut self,
        main_thread: &Arc<KThreadLock>,
        idle_thread: &Arc<KThreadLock>,
        core_id: i32,
    ) {
        self.core_id = core_id;

        let main_thread_id = main_thread.lock().unwrap().get_thread_id();
        let idle_thread_id = idle_thread.lock().unwrap().get_thread_id();

        self.idle_thread_id = Some(idle_thread_id);
        self.idle_thread = Some(Arc::downgrade(idle_thread));

        self.current_thread_id = Some(main_thread_id);
        self.current_thread = Some(Arc::downgrade(main_thread));
    }

    /// Activate the scheduler.
    /// Matches upstream `KScheduler::Activate()`.
    pub fn activate(&mut self) {
        self.is_active = true;
        self.reschedule_current_core();
    }

    /// Get the idle thread count.
    pub fn get_idle_count(&self) -> u64 {
        self.state.idle_count
    }

    /// Is the scheduler currently idle?
    pub fn is_idle(&self) -> bool {
        self.current_thread_id == self.idle_thread_id
    }

    /// Get the previous thread id.
    pub fn get_previous_thread_id(&self) -> Option<u64> {
        self.state.prev_thread_id
    }

    /// Get the current thread id.
    pub fn get_scheduler_current_thread_id(&self) -> Option<u64> {
        self.current_thread_id
    }

    /// Get the scheduler's current thread (Arc).
    /// Upstream: `KScheduler::GetSchedulerCurrentThread()`.
    pub fn get_scheduler_current_thread(&self) -> Option<Arc<KThreadLock>> {
        self.current_thread.as_ref().and_then(Weak::upgrade)
    }

    /// Get the last context switch time.
    pub fn get_last_context_switch_time(&self) -> i64 {
        self.last_context_switch_time
    }

    /// Set the interrupt task as runnable.
    /// Matches upstream: sets flag and needs_scheduling.
    pub fn set_interrupt_task_runnable(&mut self) {
        self.state.interrupt_task_runnable = true;
        self.state.needs_scheduling.store(true, Ordering::Relaxed);
    }

    /// Request schedule on interrupt.
    /// Matches upstream: sets needs_scheduling, calls ScheduleOnInterrupt
    /// if dispatch is allowed.
    pub fn request_schedule_on_interrupt(&mut self) {
        self.state.needs_scheduling.store(true, Ordering::Relaxed);
        // Full upstream path: if (CanSchedule()) { ScheduleOnInterrupt(); }
        // We defer the actual fiber switch to the caller (cpu_manager) via
        // schedule_raw_if_needed(), so that the switch never happens while
        // the scheduler Mutex is held (holding a Mutex across a fiber yield
        // causes deadlock when the next fiber tries to lock the same Mutex).
    }

    /// Activate the scheduler and schedule without holding the Mutex.
    ///
    /// Matches upstream `KScheduler::Activate()` (k_scheduler.cpp:138-141):
    ///   m_is_active = true;
    ///   RescheduleCurrentCore();
    ///
    /// Called via raw pointer from `CpuManager::guest_activate` so that the
    /// fiber switch inside `schedule_impl_fiber` never occurs while the
    /// per-core scheduler Mutex is held.
    ///
    /// # Safety
    /// Must be called on the core OS thread.  The caller must have already
    /// dropped the Mutex guard before invoking this function.  The scheduler
    /// object must remain alive for the duration of the call (guaranteed by
    /// the Arc kept in KernelCore).
    pub unsafe fn activate_and_schedule_raw(sched: *mut KScheduler) {
        trace_reschedule_path(
            1,
            1,
            (*sched).core_id,
            (*sched).state.needs_scheduling.load(Ordering::SeqCst),
        );
        (*sched).is_active = true;
        // Upstream: RescheduleCurrentCore() → EnableDispatch + Schedule if needed.
        if let Some(cur_thread) = super::kernel::get_current_thread_pointer() {
            cur_thread.lock().unwrap().enable_dispatch();
        }
        if (*sched).state.needs_scheduling.load(Ordering::SeqCst) {
            (*sched).reschedule_current_core_impl();
        }
        trace_reschedule_path(
            1,
            2,
            (*sched).core_id,
            (*sched).state.needs_scheduling.load(Ordering::SeqCst),
        );
    }

    /// Perform a scheduling fiber switch without holding the Mutex.
    ///
    /// Matches the scheduling half of upstream `KScheduler::ScheduleOnInterrupt()`:
    ///   DisableDispatch(); Schedule(); EnableDispatch();
    ///
    /// Called via raw pointer from `CpuManager` after `handle_interrupt()` so that
    /// the fiber switch inside `schedule_impl_fiber` never occurs while the
    /// per-core scheduler Mutex is held.
    ///
    /// # Safety
    /// Same requirements as `activate_and_schedule_raw`.
    pub unsafe fn schedule_raw_if_needed(sched: *mut KScheduler, trace_source: u64) {
        trace_reschedule_path(
            trace_source,
            1,
            (*sched).core_id,
            (*sched).state.needs_scheduling.load(Ordering::SeqCst),
        );
        if (*sched).state.needs_scheduling.load(Ordering::SeqCst) {
            let cur_thread = (*sched).current_thread_for_scheduler_core();
            if let Some(thread) = &cur_thread {
                thread.lock().unwrap().disable_dispatch();
            }
            (*sched).schedule_impl_fiber();
            if let Some(thread) = cur_thread {
                let mut thread = thread.lock().unwrap();
                if thread.get_disable_dispatch_count() > 0 {
                    thread.enable_dispatch();
                }
            }
        }
        trace_reschedule_path(
            trace_source,
            2,
            (*sched).core_id,
            (*sched).state.needs_scheduling.load(Ordering::SeqCst),
        );
    }

    /// Raw helper for wait owners that already know the current thread became
    /// non-runnable and need the same immediate handoff as upstream
    /// `RescheduleCurrentCore()`, but cannot hold the scheduler mutex across a
    /// fiber yield.
    ///
    /// # Safety
    /// Same requirements as `schedule_raw_if_needed`.
    pub unsafe fn reschedule_current_core_raw(sched: *mut KScheduler) {
        trace_reschedule_path(
            3,
            1,
            (*sched).core_id,
            (*sched).state.needs_scheduling.load(Ordering::SeqCst),
        );
        if let Some(cur_thread) = (*sched).current_thread_for_scheduler_core() {
            let mut cur_thread = cur_thread.lock().unwrap();
            if cur_thread.get_disable_dispatch_count() > 0 {
                cur_thread.enable_dispatch();
            }
        }

        if (*sched).global_scheduler_context.is_some() {
            (*sched)
                .state
                .needs_scheduling
                .store(true, Ordering::SeqCst);
        } else {
            (*sched)
                .state
                .needs_scheduling
                .store(true, Ordering::SeqCst);
        }

        (*sched).reschedule_current_core_impl();
        trace_reschedule_path(
            3,
            2,
            (*sched).core_id,
            (*sched).state.needs_scheduling.load(Ordering::SeqCst),
        );
    }

    /// Ensure the switch fiber exists on the current host thread.
    ///
    /// Upstream owns `m_switch_fiber` inside `KScheduler`; it is then used only
    /// by the core thread that owns this scheduler. In Rust, creating it lazily
    /// from the host core thread avoids constructing the coroutine on the wrong
    /// OS thread during kernel bootstrap.
    fn ensure_switch_fiber(&mut self) {
        if self.switch_fiber.is_some() {
            return;
        }

        log::trace!(
            "KScheduler::ensure_switch_fiber core={} host={}",
            self.core_id,
            std::thread::current().name().unwrap_or("?"),
        );
        let sched_ptr = self as *mut KScheduler as usize;
        self.switch_fiber = Some(Fiber::new(Box::new(move || loop {
            let sched = unsafe { &mut *(sched_ptr as *mut KScheduler) };
            log::trace!(
                "KScheduler::switch_fiber entry core={} host={}",
                sched.core_id,
                std::thread::current().name().unwrap_or("?"),
            );
            sched.schedule_impl_fiber_loop();
        })));
    }

    /// Preempt single core.
    /// Matches upstream `KScheduler::PreemptSingleCore()`:
    /// disables dispatch, unloads thread, yields to switch fiber, enables dispatch.
    pub fn preempt_single_core(&mut self) {
        // Upstream:
        //   GetCurrentThread(m_kernel).DisableDispatch();
        //   auto* thread = GetCurrentThreadPointer(m_kernel);
        //   auto& previous_scheduler = m_kernel.Scheduler(thread->GetCurrentCore());
        //   previous_scheduler.Unload(thread);
        //   Common::Fiber::YieldTo(thread->GetHostContext(), *m_switch_fiber);
        //   GetCurrentThread(m_kernel).EnableDispatch();

        let cur_thread = super::kernel::get_current_thread_pointer();
        if let Some(ref cur_thread) = cur_thread {
            self.ensure_switch_fiber();
            cur_thread.lock().unwrap().disable_dispatch();

            // Unload the current thread.
            //
            // Upstream unloads through `m_kernel.Scheduler(thread->GetCurrentCore())`,
            // NOT through `this`. `CpuManager::PreemptSingleCore` advances
            // `current_core` before calling into the scheduler, so `self` is the
            // scheduler of the *next* core while the thread still lives on its
            // previous core. `Unload` saves `PhysicalCore(core_id)`'s JIT into
            // the thread, so using `self.core_id` here copies the wrong core's
            // guest context into this thread.
            let previous_core = cur_thread.lock().unwrap().get_current_core();
            let Ok(previous_core_index) = usize::try_from(previous_core) else {
                log::error!(
                    "KScheduler::preempt_single_core: current thread has invalid core {}",
                    previous_core
                );
                cur_thread.lock().unwrap().enable_dispatch();
                return;
            };
            if previous_core_index >= self.physical_cores.len() {
                log::error!(
                    "KScheduler::preempt_single_core: current thread core {} is out of range",
                    previous_core
                );
                cur_thread.lock().unwrap().enable_dispatch();
                return;
            }
            self.unload_on_core(cur_thread, previous_core);

            // Upstream (k_scheduler.cpp KScheduler::PreemptSingleCore):
            //   Common::Fiber::YieldTo(thread->GetHostContext(), *m_switch_fiber);
            // raw pointers, no mutex held across yield.
            //
            // Ruzu equivalent: clone the Arc<Fiber> out of the KThread
            // guard so the guard drops at the end of the assignment
            // statement, BEFORE `Fiber::yield_to` runs. Holding the KThread
            // Mutex across the yield would hand control to another fiber
            // while cur_thread's guard is alive, deadlocking any other
            // host-thread path that tries to lock the same KThread (e.g.,
            // handle_interrupt's `thread.get_thread_id()` or scheduler
            // fiber operations).
            if let Some(ref switch_fiber) = &self.switch_fiber {
                let host_ctx = cur_thread.lock().unwrap().host_context.clone();
                if let Some(host_ctx) = host_ctx {
                    Fiber::yield_to(Arc::downgrade(&host_ctx), switch_fiber);
                }
            }

            if let Some(cur_thread) = super::kernel::get_current_thread_pointer() {
                cur_thread.lock().unwrap().enable_dispatch();
            }
        }
    }

    /// Called when a thread first starts executing on this core.
    /// Matches upstream `KScheduler::OnThreadStart()`.
    pub fn on_thread_start(&self, current_thread: &Arc<KThreadLock>) {
        current_thread.lock().unwrap().enable_dispatch();
    }

    /// Unload a thread's context (save guest state).
    /// Matches upstream `KScheduler::Unload(KThread*)`.
    pub fn unload(&self, thread: &Arc<KThreadLock>) {
        self.unload_on_core(thread, self.core_id);
    }

    /// `KScheduler::Unload(KThread*)` as executed by the scheduler of `core_id`.
    ///
    /// Upstream expresses this by picking the scheduler to unload through
    /// (`m_kernel.Scheduler(core).Unload(thread)`); since only `m_core_id` is
    /// read from it, ruzu passes the core explicitly instead.
    fn unload_on_core(&self, thread: &Arc<KThreadLock>, core_id: i32) {
        // Upstream: m_kernel.PhysicalCore(m_core_id).SaveContext(thread)
        if let Some(core) = usize::try_from(core_id)
            .ok()
            .and_then(|idx| self.physical_cores.get(idx))
        {
            core.save_context(&mut thread.lock().unwrap());
        }

        // Check if the thread is terminated by checking the DPC flags.
        let thread_guard = thread.lock().unwrap();
        let dpc_flags = thread_guard.get_dpc();
        drop(thread_guard);
        if (dpc_flags & super::k_thread::DpcFlag::TERMINATED.bits() as u8) == 0 {
            // Upstream: thread->m_context_guard.unlock()
            Self::unlock_thread_context(thread, core_id);
        }
    }

    /// Reload a thread's context (restore guest state).
    /// Matches upstream `KScheduler::Reload(KThread*)`.
    /// Upstream: `m_kernel.PhysicalCore(m_core_id).LoadContext(thread)`.
    pub fn reload(&self, thread: &Arc<KThreadLock>) {
        // Inline PhysicalCore::LoadContext since the scheduler doesn't hold
        // a reference to the kernel's physical cores.
        let thread_guard = thread.lock().unwrap();
        let parent = match thread_guard.parent.as_ref().and_then(|w| w.upgrade()) {
            Some(p) => p,
            None => return,
        };
        let mut process = parent.lock().unwrap();
        if let Some(jit) = process.get_arm_interface_mut(self.core_id as usize) {
            let k_ctx = &thread_guard.thread_context;
            let arm_ctx: &crate::arm::arm_interface::ThreadContext = unsafe {
                &*(k_ctx as *const super::k_thread::ThreadContext
                    as *const crate::arm::arm_interface::ThreadContext)
            };
            jit.set_context(arm_ctx);
            jit.set_tpidrro_el0(thread_guard.get_tls_address().get());
            if common::trace::is_enabled(common::trace::cat::SCHED_STATE)
                && trace_tid_filter_matches(Some(thread_guard.get_thread_id()))
            {
                common::trace::emit_raw(
                    common::trace::cat::SCHED_STATE,
                    &[
                        16,
                        thread_guard.get_thread_id(),
                        self.core_id as u32 as u64,
                        k_ctx.pc,
                        k_ctx.sp,
                        k_ctx.r[0],
                        k_ctx.r[1],
                        k_ctx.lr,
                        thread_guard.get_tls_address().get(),
                        jit.get_tpidrro_el0(),
                    ],
                );
            }
            log::trace!(
                "KScheduler::Reload: core={} r15/PC=0x{:X} r13/SP=0x{:X}",
                self.core_id,
                k_ctx.r[15],
                k_ctx.r[13],
            );
        }
    }

    /// Reschedule other cores by sending IPI.
    /// Matches upstream `KScheduler::RescheduleOtherCores(u64)`.
    pub fn reschedule_other_cores(&self, cores_needing_scheduling: u64) {
        let core_mask = cores_needing_scheduling & !(1u64 << self.core_id);
        if core_mask != 0 {
            self.reschedule_cores_impl(core_mask);
        }
    }

    /// Send IPI to cores that need rescheduling.
    /// Matches upstream `KScheduler::RescheduleCores(kernel, core_mask)`.
    fn reschedule_cores_impl(&self, core_mask: u64) {
        for i in 0..crate::hardware_properties::NUM_CPU_CORES as usize {
            if core_mask & (1u64 << i) != 0 {
                if let Some(core) = self.physical_cores.get(i) {
                    core.interrupt();
                }
            }
        }
    }

    /// Static version of RescheduleCores.
    /// Matches upstream `KScheduler::RescheduleCores(kernel, core_mask)`.
    pub fn reschedule_cores(core_mask: u64) {
        if core_mask == 0 {
            return;
        }
        log::trace!("KScheduler::reschedule_cores mask=0x{:x}", core_mask);

        if let Some(kernel) = super::kernel::get_kernel_ref() {
            for i in 0..crate::hardware_properties::NUM_CPU_CORES as usize {
                if core_mask & (1u64 << i) != 0 {
                    if let Some(core) = kernel.physical_core(i) {
                        core.interrupt();
                    }
                }
            }
        }
    }

    /// Matches upstream `KScheduler::RescheduleCurrentHLEThread(kernel)`.
    /// Called when no scheduler is available (non-core threads, phantom mode).
    pub fn reschedule_current_hle_thread() {
        if let Some(cur_thread) = super::kernel::get_current_thread_pointer() {
            let mut t = cur_thread.lock().unwrap();
            debug_assert!(t.get_disable_dispatch_count() == 1);

            // Upstream: GetCurrentThread(kernel).DummyThreadBeginWait();
            // Ensure dummy threads that are waiting block.
            if t.is_dummy_thread() {
                t.dummy_thread_begin_wait();
            }

            debug_assert!(t.get_state() != ThreadState::WAITING);
            t.enable_dispatch();
        }
    }

    /// Reschedule the current core.
    /// Matches upstream `KScheduler::RescheduleCurrentCore()`.
    pub fn reschedule_current_core(&mut self) {
        trace_reschedule_path(
            4,
            1,
            self.core_id,
            self.state.needs_scheduling.load(Ordering::SeqCst),
        );
        // Upstream: ASSERT(!m_kernel.IsPhantomModeForSingleCore());
        // Upstream: ASSERT(GetCurrentThread(m_kernel).GetDisableDispatchCount() == 1);

        // Upstream: GetCurrentThread(m_kernel).EnableDispatch();
        //
        // Upstream asserts the count is exactly 1 here (every caller
        // increments it once via DisableScheduling or DisableDispatch). The
        // Rust host-fiber path can re-enter scheduler-lock drop after a service
        // reply has already balanced the dummy/current-thread dispatch count.
        // Guard the decrement so those host-fiber re-entries do not underflow;
        // the explicit `needs_scheduling` reschedule call below still runs.
        if let Some(cur_thread) = super::kernel::get_current_thread_pointer() {
            let mut t = cur_thread.lock().unwrap();
            if t.get_disable_dispatch_count() > 0 {
                t.enable_dispatch();
            }
        }

        if self.state.needs_scheduling.load(Ordering::SeqCst) {
            self.reschedule_current_core_impl();
        }
        trace_reschedule_path(
            4,
            2,
            self.core_id,
            self.state.needs_scheduling.load(Ordering::SeqCst),
        );
    }

    pub(crate) fn reschedule_current_core_impl(&mut self) {
        trace_reschedule_path(
            5,
            1,
            self.core_id,
            self.state.needs_scheduling.load(Ordering::SeqCst),
        );
        // Upstream: if (m_state.needs_scheduling.load()) [[likely]] {
        //     GetCurrentThread(m_kernel).DisableDispatch();
        //     Schedule();
        //     GetCurrentThread(m_kernel).EnableDispatch();
        // }
        if self.state.needs_scheduling.load(Ordering::SeqCst) {
            let cur_thread = self.current_thread_for_scheduler_core();
            if let Some(thread) = &cur_thread {
                thread.lock().unwrap().disable_dispatch();
            }
            self.schedule();
            if let Some(thread) = cur_thread {
                let mut thread = thread.lock().unwrap();
                if thread.get_disable_dispatch_count() > 0 {
                    thread.enable_dispatch();
                }
            }
        }
        trace_reschedule_path(
            5,
            2,
            self.core_id,
            self.state.needs_scheduling.load(Ordering::SeqCst),
        );
    }

    /// Matches upstream `KScheduler::Schedule()`.
    fn schedule(&mut self) {
        // Upstream: ScheduleImpl() which yields to the switch fiber.
        self.schedule_impl_fiber();
    }

    /// Clear previous thread across all schedulers.
    /// Matches upstream `KScheduler::ClearPreviousThread(kernel, thread)`.
    pub fn clear_previous_thread(schedulers: &mut [KScheduler], thread_id: u64) {
        for scheduler in schedulers.iter_mut() {
            if scheduler.state.prev_thread_id == Some(thread_id) {
                scheduler.state.prev_thread_id = None;
            }
        }
    }

    // -- Static methods --
    // In upstream these take `KernelCore&` and access global state via
    // GetCurrentThread(kernel). Here they take the current thread directly.

    /// Matches upstream `KScheduler::DisableScheduling(kernel)`.
    /// Increments the current thread's disable_dispatch_count.
    pub fn disable_scheduling(current_thread: &Arc<KThreadLock>) {
        let mut t = current_thread.lock().unwrap();
        debug_assert!(t.get_disable_dispatch_count() >= 0);
        t.disable_dispatch();
    }

    /// Matches upstream `KScheduler::EnableScheduling(kernel, cores_needing_scheduling)`.
    /// Decrements dispatch count. If it reaches 0, triggers rescheduling.
    pub fn enable_scheduling_with_scheduler(
        cores_needing_scheduling: u64,
        scheduler: Option<&Arc<Mutex<KScheduler>>>,
        is_phantom_mode: bool,
    ) {
        let initial_disable_dispatch =
            super::kernel::with_current_thread_fast_mut(|t| t.get_disable_dispatch_count())
                .unwrap_or(0);
        debug_assert!(initial_disable_dispatch >= 1);

        if scheduler.is_none() || is_phantom_mode {
            // Upstream: KScheduler::RescheduleCores(kernel, cores_needing_scheduling);
            //           KScheduler::RescheduleCurrentHLEThread(kernel);
            Self::reschedule_cores(cores_needing_scheduling);
            Self::reschedule_current_hle_thread();
            return;
        }

        let scheduler = scheduler.unwrap();
        {
            let sched = scheduler.lock().unwrap();
            sched.reschedule_other_cores(cores_needing_scheduling);
        }

        if initial_disable_dispatch > 1 {
            // Upstream checks the nested disable count before attempting to
            // reschedule. A thread may already be WAITING while still inside
            // an outer dispatch-disable scope; in that case EnableScheduling
            // only balances the nested disable and leaves the actual fiber
            // switch to the outermost unlock.
            super::kernel::with_current_thread_fast_mut(|t| {
                t.enable_dispatch();
            });
        } else {
            let sched_ptr = {
                let guard = scheduler.lock().unwrap();
                &*guard as *const KScheduler as *mut KScheduler
            };
            unsafe {
                (*sched_ptr).reschedule_current_core();
            }
        }
    }

    /// Matches upstream `KScheduler::UpdateHighestPriorityThreads(kernel)`.
    /// Called by KAbstractSchedulerLock::Unlock.
    ///
    /// Upstream checks IsSchedulerUpdateNeeded and if set, calls
    /// UpdateHighestPriorityThreadsImpl. Returns bitmask of cores needing rescheduling.
    ///
    /// Note: The static version without kernel context returns 0.
    /// The full implementation requires GlobalSchedulerContext access and is
    /// invoked through the scheduler callbacks wired by the kernel.
    pub fn update_highest_priority_threads() -> u64 {
        // Without kernel context, return 0. When wired through SchedulerCallbacks,
        // the kernel provides the actual implementation via
        // update_highest_priority_threads_with_context().
        0
    }

    /// Full implementation of UpdateHighestPriorityThreads with GSC access.
    /// Called from wired scheduler callbacks that have kernel context.
    pub fn update_highest_priority_threads_with_context(
        gsc: &mut super::global_scheduler_context::GlobalSchedulerContext,
        schedulers: &mut [std::sync::MutexGuard<'_, KScheduler>],
    ) -> (u64, Vec<(u64, i32)>) {
        if gsc
            .m_scheduler_update_needed
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            Self::update_highest_priority_threads_impl(schedulers, gsc)
        } else {
            (0, Vec::new())
        }
    }

    /// Matches upstream `KScheduler::UpdateHighestPriorityThreadsImpl(kernel)`.
    ///
    /// Returns (cores_needing_scheduling, migrations). Migrations are applied
    /// immediately when the target KThread lock can be acquired, matching
    /// upstream's SetActiveCore-before-ChangeCore ordering. The returned list
    /// remains for older callers and should normally be empty.
    pub fn update_highest_priority_threads_impl(
        schedulers: &mut [std::sync::MutexGuard<'_, KScheduler>],
        gsc: &mut super::global_scheduler_context::GlobalSchedulerContext,
    ) -> (u64, Vec<(u64, i32)>) {
        use crate::hardware_properties::NUM_CPU_CORES;

        assert!(gsc.is_locked());

        // Clear scheduler update needed.
        gsc.m_scheduler_update_needed
            .store(false, std::sync::atomic::Ordering::Relaxed);

        let mut cores_needing_scheduling = 0u64;
        let mut idle_cores = 0u64;
        let mut top_threads: [Option<u64>; crate::hardware_properties::NUM_CPU_CORES as usize] =
            [None; crate::hardware_properties::NUM_CPU_CORES as usize];
        let migrations: Vec<(u64, i32)> = Vec::new();

        // Select top thread per core from PQ.
        for core_id in 0..NUM_CPU_CORES as usize {
            let raw_top_thread_id = gsc.get_scheduled_front(core_id as i32);
            let top_thread_id = scheduled_front_with_pinned_thread(gsc, core_id as i32);

            // Upstream marks a core idle only when GetScheduledFront() itself
            // returns null. A non-runnable pinned thread can replace a real
            // scheduled front with null, but that does not make the core a
            // migration target during this update.
            if raw_top_thread_id.is_none() {
                idle_cores |= 1u64 << core_id;
            }
            top_threads[core_id] = top_thread_id;
            if core_id < schedulers.len() {
                cores_needing_scheduling |=
                    schedulers[core_id].update_highest_priority_thread(top_thread_id, gsc);
            }
        }

        // Hot path: this runs on every scheduling decision. Keep at trace level
        // so it is gated out at info/warn — emitting it at info throttled guest
        log::trace!(
            "update_highest_prio: cores_needing=0x{:x} idle=0x{:x} tops={:?}",
            cores_needing_scheduling,
            idle_cores,
            top_threads
        );
        while idle_cores != 0 {
            let core_id = idle_cores.trailing_zeros() as usize;
            if let Some(mut suggested_id) = gsc.m_priority_queue.get_suggested_front(core_id as i32)
            {
                let mut migration_candidates =
                    [0i32; crate::hardware_properties::NUM_CPU_CORES as usize];
                let mut num_candidates = 0usize;

                loop {
                    let Some(suggested_props) =
                        gsc.m_priority_queue.get_thread_props(suggested_id).cloned()
                    else {
                        break;
                    };
                    let suggested_core = suggested_props.active_core;
                    let top_on_suggested_core = if suggested_core >= 0 {
                        top_threads[suggested_core as usize]
                    } else {
                        None
                    };

                    if top_on_suggested_core != Some(suggested_id) {
                        if let Some(top_thread_id) = top_on_suggested_core {
                            if let Some(top_props) =
                                gsc.m_priority_queue.get_thread_props(top_thread_id)
                            {
                                if top_props.priority
                                    < super::global_scheduler_context::HIGHEST_CORE_MIGRATION_ALLOWED_PRIORITY
                                {
                                    break;
                                }
                            }
                        }

                        let Some(suggested_thread) = gsc.get_thread_by_thread_id(suggested_id)
                        else {
                            break;
                        };
                        let Ok(mut suggested_thread) = suggested_thread.try_lock() else {
                            break;
                        };
                        suggested_thread.set_active_core(core_id as i32);
                        drop(suggested_thread);
                        gsc.m_priority_queue.change_core(
                            suggested_core,
                            suggested_id,
                            core_id as i32,
                            suggested_props.priority,
                            suggested_props.is_dummy,
                            false,
                        );
                        top_threads[core_id] = Some(suggested_id);
                        if core_id < schedulers.len() {
                            cores_needing_scheduling |= schedulers[core_id]
                                .update_highest_priority_thread(top_threads[core_id], gsc);
                        }
                        break;
                    }

                    if num_candidates < migration_candidates.len() {
                        migration_candidates[num_candidates] = suggested_core;
                        num_candidates += 1;
                    }

                    let Some(next_suggested_id) = gsc.m_priority_queue.get_suggested_next(
                        core_id as i32,
                        suggested_id,
                        suggested_props.priority,
                    ) else {
                        suggested_id = 0;
                        break;
                    };
                    suggested_id = next_suggested_id;
                }

                if suggested_id == 0 {
                    for &candidate_core in &migration_candidates[..num_candidates] {
                        if candidate_core < 0 {
                            continue;
                        }

                        let Some(candidate_thread_id) = top_threads[candidate_core as usize] else {
                            continue;
                        };
                        let Some(candidate_props) = gsc
                            .m_priority_queue
                            .get_thread_props(candidate_thread_id)
                            .cloned()
                        else {
                            continue;
                        };

                        let Some(next_on_candidate_core) = gsc.m_priority_queue.get_scheduled_next(
                            candidate_core,
                            candidate_thread_id,
                            candidate_props.priority,
                        ) else {
                            continue;
                        };

                        top_threads[candidate_core as usize] = Some(next_on_candidate_core);
                        if (candidate_core as usize) < schedulers.len() {
                            cores_needing_scheduling |= schedulers[candidate_core as usize]
                                .update_highest_priority_thread(
                                    top_threads[candidate_core as usize],
                                    gsc,
                                );
                        }

                        let Some(candidate_thread) =
                            gsc.get_thread_by_thread_id(candidate_thread_id)
                        else {
                            continue;
                        };
                        let Ok(mut candidate_thread) = candidate_thread.try_lock() else {
                            continue;
                        };
                        candidate_thread.set_active_core(core_id as i32);
                        drop(candidate_thread);
                        gsc.m_priority_queue.change_core(
                            candidate_core,
                            candidate_thread_id,
                            core_id as i32,
                            candidate_props.priority,
                            candidate_props.is_dummy,
                            false,
                        );
                        top_threads[core_id] = Some(candidate_thread_id);
                        if core_id < schedulers.len() {
                            cores_needing_scheduling |= schedulers[core_id]
                                .update_highest_priority_thread(top_threads[core_id], gsc);
                        }
                        break;
                    }
                }
            }

            idle_cores &= !(1u64 << core_id);
        }

        // Wake up waiting dummy threads.
        gsc.wakeup_waiting_dummy_threads();

        // Upstream hack: if the current host dummy thread became non-runnable
        // during this scheduling update, make it block when dispatch resumes.
        if let Some(cur_thread) = super::kernel::get_current_thread_pointer() {
            let cur_thread = cur_thread.lock().unwrap();
            if cur_thread.is_dummy_thread() && cur_thread.get_state() != ThreadState::RUNNABLE {
                cur_thread.request_dummy_thread_wait();
            }
        }

        (cores_needing_scheduling, migrations)
    }

    /// Update the highest priority thread for this core.
    /// Matches upstream `KScheduler::UpdateHighestPriorityThread(KThread*)`.
    pub fn update_highest_priority_thread(
        &mut self,
        highest_thread_id: Option<u64>,
        gsc: &mut super::global_scheduler_context::GlobalSchedulerContext,
    ) -> u64 {
        let prev = self.state.highest_priority_thread_id;
        if prev != highest_thread_id {
            if let Some(prev_id) = prev {
                gsc.m_priority_queue.increment_scheduled_count(prev_id);
                let tick = self
                    .core_timing
                    .as_ref()
                    .map(|core_timing| core_timing.get_clock_ticks() as i64)
                    .unwrap_or(0);
                gsc.m_priority_queue.set_last_scheduled_tick(prev_id, tick);
                if let Some(prev_thread) = gsc.get_thread_by_thread_id(prev_id) {
                    prev_thread.lock().unwrap().set_last_scheduled_tick(tick);
                }
            }
            if self.state.should_count_idle {
                if let Some(highest_thread_id) = highest_thread_id {
                    if let Some(highest_thread) = gsc.get_thread_by_thread_id(highest_thread_id) {
                        let parent = highest_thread
                            .lock()
                            .unwrap()
                            .parent
                            .as_ref()
                            .and_then(Weak::upgrade);
                        if let Some(parent) = parent {
                            parent.lock().unwrap().set_running_thread(
                                self.core_id,
                                highest_thread_id,
                                self.state.idle_count,
                                0,
                            );
                        }
                    }
                } else {
                    self.state.idle_count = self.state.idle_count.wrapping_add(1);
                }
            }
            if self.core_id == 0 || self.core_id == 1 {
                log::trace!(
                    "update_highest_priority_thread: core={} prev={:?} next={:?}",
                    self.core_id,
                    prev,
                    highest_thread_id
                );
            }
            self.state.highest_priority_thread_id = highest_thread_id;
            self.state.needs_scheduling.store(true, Ordering::Relaxed);
            1u64 << self.core_id
        } else {
            0
        }
    }

    /// On thread state changed.
    /// Matches upstream `KScheduler::OnThreadStateChanged(kernel, thread, old_state)`.
    ///
    /// Updates the priority queue when a thread transitions to/from RUNNABLE.
    pub fn on_thread_state_changed(
        &mut self,
        thread_id: u64,
        old_state: ThreadState,
        new_state: ThreadState,
    ) {
        if old_state == new_state {
            return;
        }

        // Always request scheduling on state change for cooperative dispatch.
        self.state.prev_thread_id = Some(thread_id);
        self.request_schedule();

        // PQ updates happen at the call sites that transition thread state
        // while holding the process lock (condvar wait/signal, sync wait,
        // thread exit, etc.). The dispatch loop's scan_runnable_threads
        // fallback catches any threads that bypass PQ updates.
    }

    /// On thread priority changed.
    /// Matches upstream `KScheduler::OnThreadPriorityChanged(kernel, thread, old_priority)`.
    pub fn on_thread_priority_changed(&mut self, thread_id: u64, _old_priority: i32) {
        self.state.prev_thread_id = Some(thread_id);
        self.request_schedule();

        // Upstream: if thread is RUNNABLE, call
        // priority_queue.ChangePriority(old_priority, is_running, thread).
        // Same as above: PQ update deferred until PQ-based dispatch.
    }

    /// Yield without core migration.
    /// Matches upstream `KScheduler::YieldWithoutCoreMigration(kernel)`.
    pub fn yield_without_core_migration(
        &mut self,
        process: &Arc<ProcessLock>,
        current_thread_id: u64,
    ) {
        // Cooperative runtime approximation of a thread observing its pending
        // termination and exiting at the next yield boundary.
        if self.exit_thread_if_termination_requested(process, current_thread_id) {
            self.request_schedule();
            return;
        }

        let current_thread = {
            let process = process.lock().unwrap();
            process.get_thread_by_thread_id(current_thread_id)
        };
        let Some(current_thread) = current_thread else {
            return;
        };

        let process = process.lock().unwrap();
        {
            let current_thread = current_thread.lock().unwrap();
            if current_thread.get_yield_schedule_count() == process.get_scheduled_count() {
                return;
            }
        }

        // Upstream wraps the runnable-state check, priority-queue rotation,
        // scheduled-count update, and yield-count update in KScopedSchedulerLock.
        //
        // Rust must keep the existing host-lock order (`ProcessLock` before
        // scheduler lock) because several wait/sleep paths already take that
        // order. Upstream has no equivalent host process mutex, so this is a
        // Rust-only deadlock-avoidance adaptation while still protecting the
        // priority-queue mutation with KScopedSchedulerLock.
        let scheduler_lock = super::kernel::scheduler_lock()
            .expect("global scheduler lock must be initialized before yielding");
        let _scheduler_guard = super::k_scheduler_lock::KScopedSchedulerLock::new(scheduler_lock);

        {
            let current_thread = current_thread.lock().unwrap();
            if current_thread.get_raw_state() != ThreadState::RUNNABLE {
                return;
            }
            if current_thread.get_yield_schedule_count() == process.get_scheduled_count() {
                return;
            }
        }

        // Move current thread to the back of its priority level in the PQ.
        // Upstream: next_thread = priority_queue.MoveToScheduledBack(cur_thread)
        let next_thread_id = if let Some(ref gsc) = process.global_scheduler_context {
            let ct = current_thread.lock().unwrap();
            let pri = ct.priority;
            let core = ct.core_id;
            let is_dummy = ct.thread_type == super::k_thread::ThreadType::Dummy;
            drop(ct);
            gsc.lock()
                .unwrap()
                .move_to_scheduled_back(current_thread_id, pri, core, is_dummy)
        } else {
            None
        };
        process.increment_scheduled_count();

        if let Some(next_id) = next_thread_id {
            if next_id != current_thread_id {
                // A different thread is now at the front — schedule update needed.
                self.yielded_thread_id = Some(current_thread_id);
                self.request_schedule();
            } else {
                // No other thread at this priority — set yield count.
                current_thread
                    .lock()
                    .unwrap()
                    .set_yield_schedule_count(process.get_scheduled_count());
            }
        } else {
            // PQ was empty (thread not in PQ) — fall back to old behavior.
            self.yielded_thread_id = Some(current_thread_id);
            self.request_schedule();
        }
    }

    fn highest_priority_thread_id_on_core(&self, core_id: i32) -> Option<u64> {
        if core_id < 0 {
            return None;
        }
        if core_id == self.core_id {
            return self.state.highest_priority_thread_id;
        }
        super::kernel::get_kernel_ref()
            .and_then(|kernel| kernel.scheduler(core_id as usize))
            .and_then(|scheduler| {
                scheduler
                    .lock()
                    .ok()
                    .and_then(|guard| guard.state.highest_priority_thread_id)
            })
    }

    /// Yield with core migration.
    /// Matches upstream `KScheduler::YieldWithCoreMigration(kernel)`.
    pub fn yield_with_core_migration(
        &mut self,
        process: &Arc<ProcessLock>,
        current_thread_id: u64,
    ) {
        if self.exit_thread_if_termination_requested(process, current_thread_id) {
            self.request_schedule();
            return;
        }

        let current_thread = {
            let process = process.lock().unwrap();
            process.get_thread_by_thread_id(current_thread_id)
        };
        let Some(current_thread) = current_thread else {
            return;
        };

        let process = process.lock().unwrap();
        {
            let current_thread = current_thread.lock().unwrap();
            if current_thread.get_yield_schedule_count() == process.get_scheduled_count() {
                return;
            }
        }

        let scheduler_lock = super::kernel::scheduler_lock()
            .expect("global scheduler lock must be initialized before yielding");
        let _scheduler_guard = super::k_scheduler_lock::KScopedSchedulerLock::new(scheduler_lock);

        let (core_id, cur_priority, cur_is_dummy) = {
            let current_thread = current_thread.lock().unwrap();
            if current_thread.get_raw_state() != ThreadState::RUNNABLE {
                return;
            }
            if current_thread.get_yield_schedule_count() == process.get_scheduled_count() {
                return;
            }
            (
                current_thread.get_active_core(),
                current_thread.priority,
                current_thread.thread_type == super::k_thread::ThreadType::Dummy,
            )
        };

        let Some(ref gsc_arc) = process.global_scheduler_context else {
            self.yielded_thread_id = Some(current_thread_id);
            self.request_schedule();
            return;
        };

        let mut gsc = gsc_arc.lock().unwrap();
        let next_thread_id = gsc.m_priority_queue.move_to_scheduled_back(
            current_thread_id,
            cur_priority,
            core_id,
            cur_is_dummy,
        );
        process.increment_scheduled_count();

        let mut recheck = false;
        let mut suggested_id = if core_id >= 0 {
            gsc.m_priority_queue.get_suggested_front(core_id)
        } else {
            None
        };

        while let Some(candidate_id) = suggested_id {
            let Some(candidate_props) =
                gsc.m_priority_queue.get_thread_props(candidate_id).cloned()
            else {
                break;
            };
            let suggested_core = candidate_props.active_core;
            let running_on_suggested_core = self.highest_priority_thread_id_on_core(suggested_core);

            if running_on_suggested_core != Some(candidate_id) {
                let next_waited_longer = next_thread_id
                    .filter(|next_id| *next_id != current_thread_id)
                    .and_then(|next_id| gsc.get_thread_by_thread_id(next_id))
                    .zip(gsc.get_thread_by_thread_id(candidate_id))
                    .map(|(next, suggested)| {
                        next.lock().unwrap().get_last_scheduled_tick()
                            < suggested.lock().unwrap().get_last_scheduled_tick()
                    })
                    .unwrap_or(false);

                if candidate_props.priority > cur_priority
                    || (candidate_props.priority == cur_priority && next_waited_longer)
                {
                    suggested_id = None;
                    break;
                }

                let running_priority = running_on_suggested_core
                    .and_then(|tid| gsc.m_priority_queue.get_thread_props(tid))
                    .map(|props| props.priority);
                if running_priority.is_none_or(|priority| {
                    priority
                        >= super::global_scheduler_context::HIGHEST_CORE_MIGRATION_ALLOWED_PRIORITY
                }) {
                    if let Some(suggested_thread) = gsc.get_thread_by_thread_id(candidate_id) {
                        suggested_thread.lock().unwrap().set_active_core(core_id);
                    }
                    gsc.m_priority_queue.change_core(
                        suggested_core,
                        candidate_id,
                        core_id,
                        candidate_props.priority,
                        candidate_props.is_dummy,
                        true,
                    );
                    gsc.m_priority_queue.increment_scheduled_count(candidate_id);
                    break;
                } else {
                    recheck = true;
                }
            }

            suggested_id = gsc.m_priority_queue.get_suggested_next(
                core_id,
                candidate_id,
                candidate_props.priority,
            );
        }

        if suggested_id.is_some() || next_thread_id != Some(current_thread_id) {
            gsc.m_scheduler_update_needed
                .store(true, std::sync::atomic::Ordering::Release);
        } else if !recheck {
            current_thread
                .lock()
                .unwrap()
                .set_yield_schedule_count(process.get_scheduled_count());
        }
    }

    /// Yield to any thread.
    pub fn yield_to_any_thread(&mut self, process: &Arc<ProcessLock>, current_thread_id: u64) {
        if self.exit_thread_if_termination_requested(process, current_thread_id) {
            self.request_schedule();
            return;
        }

        let current_thread = {
            let process = process.lock().unwrap();
            process.get_thread_by_thread_id(current_thread_id)
        };
        let Some(current_thread) = current_thread else {
            return;
        };

        let process = process.lock().unwrap();
        {
            let current_thread = current_thread.lock().unwrap();
            if current_thread.get_yield_schedule_count() == process.get_scheduled_count() {
                return;
            }
        }

        let scheduler_lock = super::kernel::scheduler_lock()
            .expect("global scheduler lock must be initialized before yielding");
        let _scheduler_guard = super::k_scheduler_lock::KScopedSchedulerLock::new(scheduler_lock);

        let (core_id, cur_priority, cur_affinity, cur_is_dummy) = {
            let current_thread = current_thread.lock().unwrap();
            if current_thread.get_raw_state() != ThreadState::RUNNABLE {
                return;
            }
            if current_thread.get_yield_schedule_count() == process.get_scheduled_count() {
                return;
            }
            (
                current_thread.get_active_core(),
                current_thread.priority,
                current_thread.physical_affinity_mask.get_affinity_mask(),
                current_thread.thread_type == super::k_thread::ThreadType::Dummy,
            )
        };

        let Some(ref gsc_arc) = process.global_scheduler_context else {
            self.yielded_thread_id = Some(current_thread_id);
            self.request_schedule();
            return;
        };

        let mut gsc = gsc_arc.lock().unwrap();
        current_thread.lock().unwrap().set_active_core(-1);
        gsc.m_priority_queue.change_core(
            core_id,
            current_thread_id,
            -1,
            cur_priority,
            cur_is_dummy,
            false,
        );
        gsc.m_priority_queue
            .increment_scheduled_count(current_thread_id);
        process.increment_scheduled_count();

        let scheduled_front = if core_id >= 0 {
            gsc.m_priority_queue.get_scheduled_front(core_id)
        } else {
            None
        };

        if scheduled_front.is_none() {
            let mut suggested_id = if core_id >= 0 && (cur_affinity & (1u64 << core_id)) != 0 {
                gsc.m_priority_queue.get_suggested_front(core_id)
            } else {
                None
            };

            while let Some(candidate_id) = suggested_id {
                let Some(candidate_props) =
                    gsc.m_priority_queue.get_thread_props(candidate_id).cloned()
                else {
                    break;
                };
                let suggested_core = candidate_props.active_core;
                let top_on_suggested_core = if suggested_core >= 0 {
                    gsc.m_priority_queue.get_scheduled_front(suggested_core)
                } else {
                    None
                };

                if top_on_suggested_core != Some(candidate_id) {
                    let top_priority = top_on_suggested_core
                        .and_then(|tid| gsc.m_priority_queue.get_thread_props(tid))
                        .map(|props| props.priority);
                    if top_priority.is_none_or(|priority| {
                        priority
                            >= super::global_scheduler_context::HIGHEST_CORE_MIGRATION_ALLOWED_PRIORITY
                    }) {
                        if let Some(suggested_thread) = gsc.get_thread_by_thread_id(candidate_id) {
                            suggested_thread.lock().unwrap().set_active_core(core_id);
                        }
                        gsc.m_priority_queue.change_core(
                            suggested_core,
                            candidate_id,
                            core_id,
                            candidate_props.priority,
                            candidate_props.is_dummy,
                            false,
                        );
                        gsc.m_priority_queue.increment_scheduled_count(candidate_id);
                    }
                    break;
                }

                suggested_id = gsc.m_priority_queue.get_suggested_next(
                    core_id,
                    candidate_id,
                    candidate_props.priority,
                );
            }

            if suggested_id != Some(current_thread_id) {
                gsc.m_scheduler_update_needed
                    .store(true, std::sync::atomic::Ordering::Release);
            } else {
                current_thread
                    .lock()
                    .unwrap()
                    .set_yield_schedule_count(process.get_scheduled_count());
            }
        } else {
            gsc.m_scheduler_update_needed
                .store(true, std::sync::atomic::Ordering::Release);
        }
    }

    pub fn set_scheduler_current_thread_id(&mut self, thread_id: u64) {
        self.current_thread_id = Some(thread_id);
        if self.yielded_thread_id == Some(thread_id) {
            self.yielded_thread_id = None;
        }
        self.state.needs_scheduling.store(false, Ordering::Relaxed);
    }

    pub fn set_scheduler_current_thread(&mut self, thread: &Arc<KThreadLock>) {
        let thread_id = thread.lock().unwrap().get_thread_id();
        self.current_thread_id = Some(thread_id);
        self.current_thread = Some(Arc::downgrade(thread));
        if self.yielded_thread_id == Some(thread_id) {
            self.yielded_thread_id = None;
        }
        self.state.needs_scheduling.store(false, Ordering::Relaxed);
        super::kernel::set_current_emu_thread(Some(thread));
    }

    pub fn request_schedule(&self) {
        self.state.needs_scheduling.store(true, Ordering::Relaxed);
    }

    /// Rotate the scheduled queue at a given priority for a core.
    /// Matches upstream `KScheduler::RotateScheduledQueue(kernel, core_id, priority)`.
    ///
    /// Moves the front thread at `priority` to the back, then tries to
    /// migrate a suggested thread to fill the gap.
    pub fn rotate_scheduled_queue(
        gsc: &mut super::global_scheduler_context::GlobalSchedulerContext,
        core_id: i32,
        priority: i32,
        current_thread_id: Option<u64>,
    ) {
        let top_thread_id = gsc
            .m_priority_queue
            .get_scheduled_front_at_priority(core_id, priority);
        let mut next_thread_id = None;
        if let Some(top_id) = top_thread_id {
            let (t_priority, t_core, t_dummy) = gsc
                .m_priority_queue
                .get_thread_props(top_id)
                .map(|p| (p.priority, p.active_core, p.is_dummy))
                .unwrap_or((priority, core_id, false));
            next_thread_id = gsc
                .m_priority_queue
                .move_to_scheduled_back(top_id, t_priority, t_core, t_dummy);
            if next_thread_id != Some(top_id) {
                gsc.m_priority_queue.increment_scheduled_count(top_id);
                if let Some(next_id) = next_thread_id {
                    gsc.m_priority_queue.increment_scheduled_count(next_id);
                }
            }
        }

        let mut suggested_id = gsc
            .m_priority_queue
            .get_suggested_front_at_priority(core_id, priority);
        while let Some(candidate_id) = suggested_id {
            let Some(candidate_props) =
                gsc.m_priority_queue.get_thread_props(candidate_id).cloned()
            else {
                break;
            };
            let suggested_core = candidate_props.active_core;
            let top_on_suggested_core = if suggested_core >= 0 {
                gsc.m_priority_queue.get_scheduled_front(suggested_core)
            } else {
                None
            };

            if top_on_suggested_core != Some(candidate_id) {
                let next_waited_longer = top_thread_id != next_thread_id
                    && next_thread_id
                        .and_then(|next_id| gsc.m_priority_queue.get_thread_props(next_id))
                        .is_some_and(|next_props| {
                            next_props.last_scheduled_tick < candidate_props.last_scheduled_tick
                        });
                if next_waited_longer {
                    break;
                }

                let top_priority = top_on_suggested_core
                    .and_then(|thread_id| gsc.m_priority_queue.get_thread_props(thread_id))
                    .map(|props| props.priority);
                if top_priority.is_none_or(|top_priority| {
                    top_priority
                        >= super::global_scheduler_context::HIGHEST_CORE_MIGRATION_ALLOWED_PRIORITY
                }) {
                    if let Some(candidate) = gsc.get_thread_by_thread_id(candidate_id) {
                        candidate.lock().unwrap().set_active_core(core_id);
                    }
                    gsc.m_priority_queue.change_core(
                        suggested_core,
                        candidate_id,
                        core_id,
                        candidate_props.priority,
                        candidate_props.is_dummy,
                        true,
                    );
                    gsc.m_priority_queue.increment_scheduled_count(candidate_id);
                    break;
                }
            }

            suggested_id = gsc
                .m_priority_queue
                .get_same_priority_next(core_id, candidate_id);
        }

        let mut best_thread_id = gsc.m_priority_queue.get_scheduled_front(core_id);
        if best_thread_id == current_thread_id {
            best_thread_id = best_thread_id.and_then(|thread_id| {
                let priority = gsc
                    .m_priority_queue
                    .get_thread_props(thread_id)
                    .map(|props| props.priority)?;
                gsc.m_priority_queue
                    .get_scheduled_next(core_id, thread_id, priority)
            });
        }

        if let Some(best_id) = best_thread_id {
            if let Some(best_priority) = gsc
                .m_priority_queue
                .get_thread_props(best_id)
                .map(|props| props.priority)
                .filter(|best_priority| *best_priority >= priority)
            {
                let mut suggested_id = gsc.m_priority_queue.get_suggested_front(core_id);
                while let Some(candidate_id) = suggested_id {
                    let Some(candidate_props) =
                        gsc.m_priority_queue.get_thread_props(candidate_id).cloned()
                    else {
                        break;
                    };
                    if candidate_props.priority >= best_priority {
                        break;
                    }

                    let suggested_core = candidate_props.active_core;
                    let top_on_suggested_core = if suggested_core >= 0 {
                        gsc.m_priority_queue.get_scheduled_front(suggested_core)
                    } else {
                        None
                    };
                    if top_on_suggested_core != Some(candidate_id) {
                        let top_priority = top_on_suggested_core
                            .and_then(|thread_id| gsc.m_priority_queue.get_thread_props(thread_id))
                            .map(|props| props.priority);
                        if top_priority.is_none_or(|top_priority| {
                            top_priority
                                >= super::global_scheduler_context::HIGHEST_CORE_MIGRATION_ALLOWED_PRIORITY
                        }) {
                            if let Some(candidate) = gsc.get_thread_by_thread_id(candidate_id) {
                                candidate.lock().unwrap().set_active_core(core_id);
                            }
                            gsc.m_priority_queue.change_core(
                                suggested_core,
                                candidate_id,
                                core_id,
                                candidate_props.priority,
                                candidate_props.is_dummy,
                                true,
                            );
                            gsc.m_priority_queue.increment_scheduled_count(candidate_id);
                            break;
                        }
                    }

                    suggested_id = gsc.m_priority_queue.get_suggested_next(
                        core_id,
                        candidate_id,
                        candidate_props.priority,
                    );
                }
            }
        }

        gsc.m_scheduler_update_needed
            .store(true, std::sync::atomic::Ordering::Release);
    }

    pub fn needs_scheduling(&self) -> bool {
        self.state.needs_scheduling.load(Ordering::Relaxed)
    }

    pub fn wake_signaled_synchronization_threads(&mut self, process: &Arc<ProcessLock>) -> bool {
        let process = process.lock().unwrap();
        let mut woke_any = false;
        for thread_id in &process.thread_list {
            let Some(thread) = process.get_thread_by_thread_id(*thread_id) else {
                continue;
            };

            let mut thread = thread.lock().unwrap();
            if !thread.is_waiting_on_synchronization() || thread.get_state() != ThreadState::WAITING
            {
                continue;
            }

            // Re-check whether any object in the thread's wait context became
            // signaled (the polling hook used by the scheduler when it wakes
            // up between timer ticks). Matches upstream sync-ready check.
            let Some(synced_index) = thread
                .sync_wait_context
                .object_ids()
                .iter()
                .position(|&oid| {
                    crate::hle::kernel::k_synchronization_object::is_object_signaled(&process, oid)
                })
                .map(|i| i as i32)
            else {
                continue;
            };

            thread.complete_synchronization_wait(
                synced_index,
                crate::hle::result::RESULT_SUCCESS.get_inner_value(),
            );
            woke_any = true;
        }

        if woke_any {
            self.request_schedule();
        }

        woke_any
    }

    pub fn wait_for_next_runnable_thread(&mut self, process: &Arc<ProcessLock>) -> u64 {
        loop {
            // Upstream does not have the scheduler proactively deliver sleep
            // timer expirations by polling thread-local deadlines here.
            // Sleep wakeups are owned by KHardwareTimer::DoTask()/thread.OnTimer().
            // Keep the synchronization-object polling fallback for now, but do
            // not synthesize sleep wakeups from the scheduler loop.
            self.wake_signaled_synchronization_threads(process);

            // PQ-based selection (O(1) for highest priority thread).
            if let Some(next) = self.select_next_thread_from_pq(process) {
                return next;
            }

            // PQ empty — fallback to linear scan for RUNNABLE threads that
            // aren't in the PQ (timer/cancel_wait wakeups, early init).
            // This is a safety net; once all wakeup paths push to PQ, this
            // becomes unreachable.
            if let Some(next) = self.scan_runnable_threads(process) {
                return next;
            }

            thread::sleep(Duration::from_millis(1));
        }
    }

    /// Fallback: scan for the highest-priority RUNNABLE thread.
    /// Used when PQ is empty (threads woken via timer/cancel_wait that
    /// bypass PQ). Returns the thread_id and pushes it to PQ for future use.
    fn scan_runnable_threads(&self, process: &Arc<ProcessLock>) -> Option<u64> {
        let process = process.lock().unwrap();
        let mut best_id = None;
        let mut best_priority = i32::MAX;

        for thread_id in &process.thread_list {
            let Some(thread) = process.get_thread_by_thread_id(*thread_id) else {
                continue;
            };
            let thread = thread.lock().unwrap();
            if thread.get_raw_state() != ThreadState::RUNNABLE {
                continue;
            }
            if thread.get_active_core() != self.core_id {
                continue;
            }
            if (thread.physical_affinity_mask.get_affinity_mask() & (1u64 << self.core_id)) == 0 {
                continue;
            }
            if thread.get_priority() < best_priority {
                best_priority = thread.get_priority();
                best_id = Some(thread.get_thread_id());
            }
        }

        // Push the found thread to PQ so future lookups are O(1).
        if let Some(tid) = best_id {
            process.push_back_to_priority_queue(tid);
        }
        best_id
    }

    /// Select the next thread using the priority queue.
    /// This is the upstream-matching dispatch path: O(1) lookup of the
    /// highest priority RUNNABLE thread for our core.
    fn select_next_thread_from_pq(&mut self, process: &Arc<ProcessLock>) -> Option<u64> {
        let process_guard = process.lock().unwrap();
        let gsc = process_guard.global_scheduler_context.as_ref()?;
        let gsc_guard = gsc.lock().unwrap();
        let next_thread_id = gsc_guard.get_scheduled_front(self.core_id)?;

        // Handle yield: if the current thread yielded, try the next one.
        if let Some(yielded) = self.yielded_thread_id {
            if next_thread_id == yielded {
                let priority = process_guard
                    .get_thread_by_thread_id(next_thread_id)
                    .map(|t| t.lock().unwrap().get_priority())
                    .unwrap_or(63);
                let next = gsc_guard.get_scheduled_next(self.core_id, next_thread_id, priority);
                if let Some(alternative) = next {
                    self.yielded_thread_id = None;
                    return Some(alternative);
                }
                if let Some(thread) = process_guard.get_thread_by_thread_id(yielded) {
                    thread
                        .lock()
                        .unwrap()
                        .set_yield_schedule_count(process_guard.get_scheduled_count());
                }
                self.yielded_thread_id = None;
            }
        }

        Some(next_thread_id)
    }

    /// Matches upstream `KScheduler::ScheduleImpl()` — fiber-based path.
    /// In upstream, this yields to m_switch_fiber which runs ScheduleImplFiber.
    fn schedule_impl_fiber(&mut self) {
        self.ensure_switch_fiber();
        self.trace_needs_scheduling_clear(1, None, None);
        self.state.needs_scheduling.store(false, Ordering::Relaxed);
        std::sync::atomic::fence(Ordering::SeqCst);

        // Upstream `ScheduleImpl()` reads `GetCurrentThreadPointer(kernel)`,
        // i.e. the thread attached to the currently executing host fiber. Do
        // not use `self.current_thread` here: if scheduler bookkeeping drifts,
        // the `highest == current` fast path would return without yielding to
        // the runnable fiber that should actually run.
        let cur_thread = self.current_thread_for_scheduler_core();
        let highest = self.state.highest_priority_thread_id;
        let cur_thread_id = cur_thread
            .as_ref()
            .map(|thread| thread.lock().unwrap().get_thread_id());

        let target = if self.state.interrupt_task_runnable {
            self.idle_thread.as_ref().and_then(Weak::upgrade)
        } else {
            highest.and_then(|id| {
                let gsc = self.global_scheduler_context.as_ref()?;
                gsc.lock().unwrap().get_thread_by_thread_id(id)
            })
        };
        let target_id = target
            .as_ref()
            .map(|thread| thread.lock().unwrap().get_thread_id());
        if should_log_sched_pick_for(&[cur_thread_id, highest, target_id]) {
            log::warn!(
                "[SCHED_PICK] impl core={} cur={:?} highest={:?} target={:?} interrupt={} needs={}",
                self.core_id,
                cur_thread_id,
                highest,
                target_id,
                self.state.interrupt_task_runnable,
                self.state.needs_scheduling.load(Ordering::Relaxed),
            );
        }
        if common::trace::is_enabled(common::trace::cat::SCHED_STATE)
            && (trace_tid_filter_matches(cur_thread_id)
                || trace_tid_filter_matches(highest)
                || trace_tid_filter_matches(target_id))
        {
            let tops = if let Some(gsc) = self.global_scheduler_context.as_ref() {
                let gsc = gsc.lock().unwrap();
                [
                    gsc.get_scheduled_front(0),
                    gsc.get_scheduled_front(1),
                    gsc.get_scheduled_front(2),
                    gsc.get_scheduled_front(3),
                ]
            } else {
                [None, None, None, None]
            };
            common::trace::emit_raw(
                common::trace::cat::SCHED_STATE,
                &[
                    4,
                    encode_trace_tid(cur_thread_id),
                    encode_trace_tid(highest),
                    encode_trace_tid(target_id),
                    self.core_id as u32 as u64,
                    self.state.needs_scheduling.load(Ordering::Relaxed) as u64,
                    self.state.interrupt_task_runnable as u64,
                    self.switch_from_schedule as u64,
                    encode_trace_tid(tops[0]),
                    encode_trace_tid(tops[1]),
                    encode_trace_tid(tops[2]),
                    encode_trace_tid(tops[3]),
                ],
            );
        }
        if self.core_id == 0 || self.core_id == 1 {
            log::trace!(
                "schedule_impl_fiber: core={} cur={:?} highest={:?} target={:?} interrupted={}",
                self.core_id,
                self.current_thread_id,
                highest,
                target_id,
                self.state.interrupt_task_runnable
            );
        }

        // If same as current, nothing to do.
        if let (Some(ref cur), Some(ref tgt)) = (&cur_thread, &target) {
            if Arc::ptr_eq(cur, tgt) {
                // Upstream's scheduler current cannot drift away from the
                // fiber-local current thread. In Rust, TLS/fiber handoff can
                // recover a correct current thread while `self.current_thread`
                // still names an older scheduled slot. Repair that bookkeeping
                // before taking the fast path, otherwise CpuManager will see a
                // permanent scheduler_current != running_fiber mismatch.
                let cur_id = cur.lock().unwrap().get_thread_id();
                if self.current_thread_id != Some(cur_id) {
                    self.current_thread_id = Some(cur_id);
                    self.current_thread = Some(Arc::downgrade(cur));
                    super::kernel::set_current_emu_thread(Some(cur));
                }
                std::sync::atomic::fence(Ordering::SeqCst);
                return;
            }
        }

        // Store switch state for the fiber.
        self.switch_cur_thread = cur_thread.as_ref().map(Arc::downgrade);
        self.switch_highest_priority_thread = target.as_ref().map(Arc::downgrade);
        self.switch_from_schedule = true;

        // Upstream: Common::Fiber::YieldTo(cur_thread->m_host_context, *m_switch_fiber)
        // Get the fiber to yield FROM: prefer cur_thread.host_context, fall back to
        // the core's host fiber stored in CpuManager (for idle/dummy threads
        // that don't have their own fiber context).
        let yield_from: Option<Arc<Fiber>> = cur_thread
            .as_ref()
            .and_then(|t| t.lock().unwrap().host_context.clone())
            .or_else(|| {
                // Fallback: get the host fiber from CpuManager for this core.
                let kernel = super::kernel::get_kernel_ref()?;
                let sys_ref = kernel.system();
                if sys_ref.is_null() {
                    return None;
                }
                sys_ref
                    .get()
                    .get_cpu_manager()
                    .core_host_context(self.core_id as usize)
            });

        // Upstream always yields to the switch fiber when highest != current,
        // even when highest is null (the switch fiber handles it by falling
        // through to the idle thread).
        if let (Some(ref from_ctx), Some(ref switch_fiber)) = (&yield_from, &self.switch_fiber) {
            trace_sched_fiber_event(
                8,
                self.core_id,
                cur_thread_id,
                target.as_ref(),
                true,
                self.state.needs_scheduling.load(Ordering::Relaxed),
            );
            Fiber::yield_to(Arc::downgrade(from_ctx), switch_fiber);
            trace_sched_fiber_event(
                9,
                self.core_id,
                cur_thread_id,
                target.as_ref(),
                true,
                self.state.needs_scheduling.load(Ordering::Relaxed),
            );
        }
    }

    fn resolve_highest_priority_thread_from_state(&self) -> Option<Arc<KThreadLock>> {
        let id = self.state.highest_priority_thread_id?;
        let gsc = self.global_scheduler_context.as_ref()?;
        gsc.lock().unwrap().get_thread_by_thread_id(id)
    }

    /// Matches upstream `KScheduler::ScheduleImplFiber()` (k_scheduler.cpp:420-494).
    /// This runs inside the switch fiber and handles the actual context switch:
    /// unloads old thread, spins to acquire new thread's context_guard,
    /// calls SwitchThread, reloads new thread, then yields back.
    fn schedule_impl_fiber_loop(&mut self) {
        let cur_thread = self.switch_cur_thread.as_ref().and_then(Weak::upgrade);
        // Upstream starts with m_switch_highest_priority_thread captured by
        // ScheduleImpl(), and only refreshes m_state.highest_priority_thread
        // after a retry. Keeping that handoff exact matters for fibers: the
        // switch fiber must resume the thread selected for the yield that just
        // transferred control here.
        let mut highest_priority_thread = self
            .switch_highest_priority_thread
            .as_ref()
            .and_then(Weak::upgrade);

        // If we're not coming from scheduling (i.e., we came from SC preemption),
        // skip the unload and jump straight to retry.
        let mut need_retry = !self.switch_from_schedule;

        if self.switch_from_schedule {
            self.switch_from_schedule = false;

            // Save the original thread context.
            if let Some(ref cur) = cur_thread {
                self.unload(cur);
            }
        }

        // Loop until we successfully switch the thread context.
        loop {
            if need_retry {
                // Clear needs_scheduling and refresh highest priority thread.
                self.trace_needs_scheduling_clear(
                    2,
                    cur_thread.as_ref(),
                    highest_priority_thread.as_ref(),
                );
                self.state.needs_scheduling.store(false, Ordering::Relaxed);
                std::sync::atomic::fence(Ordering::SeqCst);

                highest_priority_thread = self.resolve_highest_priority_thread_from_state();
            }
            need_retry = false;

            // If highest_priority_thread is null, switch to idle thread.
            if highest_priority_thread.is_none() {
                highest_priority_thread = self.idle_thread.as_ref().and_then(Weak::upgrade);
            }

            let Some(ref hpt) = highest_priority_thread else {
                // No thread available at all — retry.
                need_retry = true;
                continue;
            };

            let loop_cur_id = cur_thread
                .as_ref()
                .map(|thread| thread.lock().unwrap().get_thread_id());
            let hpt_id_for_trace = Some(hpt.lock().unwrap().get_thread_id());
            if should_log_sched_pick_for(&[loop_cur_id, hpt_id_for_trace]) {
                log::warn!(
                    "[SCHED_PICK] loop core={} cur={:?} hpt={:?} retry={} hpt_has_ctx={} needs={}",
                    self.core_id,
                    loop_cur_id,
                    hpt_id_for_trace,
                    need_retry,
                    hpt.lock().unwrap().get_host_context().is_some(),
                    self.state.needs_scheduling.load(Ordering::Relaxed),
                );
            }
            trace_sched_fiber_event(
                10,
                self.core_id,
                loop_cur_id,
                Some(hpt),
                hpt.lock().unwrap().get_host_context().is_some(),
                self.state.needs_scheduling.load(Ordering::Relaxed),
            );
            // Try to lock the highest priority thread's context_guard.
            loop {
                if Self::try_lock_thread_context(hpt, self.core_id) {
                    break;
                }
                // Context is locked by another core. Check if we need rescheduling.
                if self.state.needs_scheduling.load(Ordering::SeqCst) {
                    need_retry = true;
                    break;
                }
            }
            if need_retry {
                continue;
            }
            trace_sched_fiber_event(
                11,
                self.core_id,
                cur_thread
                    .as_ref()
                    .map(|thread| thread.lock().unwrap().get_thread_id()),
                Some(hpt),
                hpt.lock().unwrap().get_host_context().is_some(),
                self.state.needs_scheduling.load(Ordering::Relaxed),
            );

            // Switch to the highest priority thread.
            let hpt_id = hpt.lock().unwrap().thread_id;
            self.switch_thread_impl(cur_thread.as_ref(), hpt_id);

            // Check if we need scheduling again. If so, unlock and retry.
            if self.state.needs_scheduling.load(Ordering::SeqCst) {
                Self::unlock_thread_context_at(hpt, self.core_id, "switch_retry");
                need_retry = true;
                continue;
            }

            // Success — break out of the loop.
            break;
        }

        // Reload the guest thread context.
        if let Some(ref hpt) = highest_priority_thread {
            let hpt_id = hpt.lock().unwrap().get_thread_id();
            log::trace!(
                "schedule_impl_fiber_loop: Reload + YieldTo thread {}",
                hpt_id
            );
            self.reload(hpt);

            // Yield back from switch_fiber to the newly scheduled thread's host_context.
            // Upstream: Common::Fiber::YieldTo(m_switch_fiber, *highest_priority_thread->m_host_context)
            if let Some(ref switch_fiber) = self.switch_fiber {
                let host_ctx = hpt.lock().unwrap().get_host_context().cloned();
                if let Some(ref ctx) = host_ctx {
                    log::trace!(
                        "schedule_impl_fiber_loop: host={} core={} yielding from switch_fiber to thread {} fiber",
                        std::thread::current().name().unwrap_or("?"),
                        self.core_id,
                        hpt_id
                    );
                    trace_sched_fiber_event(
                        12,
                        self.core_id,
                        cur_thread
                            .as_ref()
                            .map(|thread| thread.lock().unwrap().get_thread_id()),
                        Some(hpt),
                        true,
                        self.state.needs_scheduling.load(Ordering::Relaxed),
                    );
                    Fiber::yield_to(Arc::downgrade(switch_fiber), ctx);
                    trace_sched_fiber_event(
                        13,
                        self.core_id,
                        cur_thread
                            .as_ref()
                            .map(|thread| thread.lock().unwrap().get_thread_id()),
                        Some(hpt),
                        true,
                        self.state.needs_scheduling.load(Ordering::Relaxed),
                    );
                } else {
                    log::warn!(
                        "schedule_impl_fiber_loop: thread {} has no host_context",
                        hpt_id
                    );
                }
            }
        }
    }

    /// Matches upstream `KScheduler::SwitchThread(KThread* next_thread)`.
    /// Updates CPU time tracking, previous thread, current thread.
    fn switch_thread_impl(
        &mut self,
        switch_cur_thread: Option<&Arc<KThreadLock>>,
        next_thread_id: u64,
    ) {
        // Upstream `SwitchThread()` reads `GetCurrentThreadPointer(kernel)` —
        // the per-core current-thread pointer that `SwitchThread` itself
        // updates at the end of every call. The scheduler-owned
        // `self.current_thread` is that pointer's Rust analogue. It MUST take
        // precedence over the captured `switch_cur_thread`: in the switch
        // fiber's retry loop (needs_scheduling fired mid-switch), the capture
        // is one switch out of date, and comparing `next` against it can hit
        // the same-thread fast return for a thread that is NOT current. That
        // skips the bookkeeping update, leaves `current_thread` pointing at
        // the previous (idle) thread while the reloaded thread runs, and
        // eventually leaks the reloaded thread's `context_guard` and wedges
        // the switch fiber's upstream-faithful try_lock spin forever.
        //
        // TLS (`current_thread_for_scheduler_core`) stays last: fibers share
        // OS-thread TLS, so it can name another resumed fiber's thread.
        let cur_thread = self
            .current_thread
            .as_ref()
            .and_then(Weak::upgrade)
            .or_else(|| switch_cur_thread.cloned())
            .or_else(|| self.current_thread_for_scheduler_core());
        let cur_thread_id = cur_thread
            .as_ref()
            .map(|thread| thread.lock().unwrap().get_thread_id());

        if should_log_sched_pick_for(&[cur_thread_id, Some(next_thread_id)]) {
            log::warn!(
                "[SCHED_PICK] switch core={} cur={:?} next={} prev={:?}",
                self.core_id,
                cur_thread_id,
                next_thread_id,
                self.state.prev_thread_id,
            );
        }
        log::trace!(
            "switch_thread_impl: cur={:?} next={} has_gsc={}",
            cur_thread_id,
            next_thread_id,
            self.global_scheduler_context.is_some()
        );

        // Upstream `KScheduler::SwitchThread` first marks the target thread as
        // resident on this scheduler's core, then returns early if the target
        // is already current. Keep that order: a previous switch away may have
        // cleared ruzu's Rust-only `current_core` marker.
        if let Some(next) = self.resolve_thread_for_switch(next_thread_id) {
            let mut next_guard = next.lock().unwrap();
            if next_guard.get_current_core() != self.core_id {
                next_guard.set_current_core(self.core_id);
            }
        }

        // If same thread, nothing to do.
        if Some(next_thread_id) == cur_thread_id {
            log::trace!("switch_thread_impl: same thread, skipping");
            return;
        }

        // Emit SCHED trace line (zuyu-compatible format).
        super::trace_format::trace_sched(self.core_id, cur_thread_id, next_thread_id);
        if common::trace::is_enabled(common::trace::cat::SCHED_STATE)
            && (trace_tid_filter_matches(cur_thread_id)
                || trace_tid_filter_matches(Some(next_thread_id)))
        {
            common::trace::emit_raw(
                common::trace::cat::SCHED_STATE,
                &[
                    5,
                    encode_trace_tid(cur_thread_id),
                    next_thread_id,
                    0,
                    self.core_id as u32 as u64,
                    self.global_scheduler_context.is_some() as u64,
                    encode_trace_tid(self.state.prev_thread_id),
                ],
            );
        }

        // Update CPU time tracking.
        let prev_tick = self.last_context_switch_time;
        let cur_tick = if let Some(ref ct) = self.core_timing {
            ct.get_global_time_ns().as_nanos() as i64
        } else {
            prev_tick + 1
        };
        let tick_diff = cur_tick - prev_tick;
        self.last_context_switch_time = cur_tick;

        // Add CPU time to current thread.
        if let Some(ref cur) = cur_thread {
            cur.lock().unwrap().add_cpu_time(self.core_id, tick_diff);
        }

        // Update previous thread.
        if let Some(ref cur) = cur_thread {
            let cur_guard = cur.lock().unwrap();
            if !cur_guard.is_termination_requested() && cur_guard.get_active_core() == self.core_id
            {
                self.state.prev_thread_id = cur_thread_id;
            } else {
                self.state.prev_thread_id = None;
            }
        }

        // Set the new current thread.
        self.current_thread_id = Some(next_thread_id);
        if let Some(next) = self.resolve_thread_for_switch(next_thread_id) {
            // Upstream: SetCurrentThread(m_kernel, next_thread);
            super::kernel::set_current_emu_thread(Some(&next));
            self.current_thread = Some(Arc::downgrade(&next));
        }
    }

    /// Matches upstream `KScheduler::Unload(KThread*)`.
    /// Saves guest context and unlocks the thread's context guard.
    pub fn unload_thread(&self, thread: &Arc<KThreadLock>) {
        // Upstream: m_kernel.PhysicalCore(m_core_id).SaveContext(thread)
        if let Some(core) = self.physical_cores.get(self.core_id as usize) {
            core.save_context(&mut thread.lock().unwrap());
        }

        // Unlock context guard if thread is not terminated.
        let thread_guard = thread.lock().unwrap();
        let dpc_flags = thread_guard.get_dpc();
        drop(thread_guard);
        if (dpc_flags & super::k_thread::DpcFlag::TERMINATED.bits() as u8) == 0 {
            // Upstream: thread->m_context_guard.unlock()
            Self::unlock_thread_context(thread, self.core_id);
        }
    }

    /// Matches upstream `KScheduler::Reload(KThread*)`.
    /// Restores guest context.
    pub fn reload_thread(&self, thread: &Arc<KThreadLock>) {
        // Upstream: m_kernel.PhysicalCore(m_core_id).LoadContext(thread)
        if let Some(core) = self.physical_cores.get(self.core_id as usize) {
            core.load_context(&mut thread.lock().unwrap());
        }
    }

    pub fn wait_for_next_thread(
        &mut self,
        process: &Arc<ProcessLock>,
        current_thread_id: u64,
    ) -> Option<Arc<KThreadLock>> {
        if self.exit_thread_if_termination_requested(process, current_thread_id) {
            self.request_schedule();
        }
        let next_thread_id = self.wait_for_next_runnable_thread(process);
        process
            .lock()
            .unwrap()
            .get_thread_by_thread_id(next_thread_id)
    }

    /// Deprecated: linear scan for next thread. Replaced by PQ-based dispatch.
    /// Kept only for test compatibility (svc_thread tests use it directly).
    #[cfg(test)]
    pub fn select_next_thread_id(
        &mut self,
        process: &Arc<ProcessLock>,
        current_thread_id: u64,
    ) -> Option<u64> {
        if self.exit_thread_if_termination_requested(process, current_thread_id) {
            self.request_schedule();
        }
        let mut process = process.lock().unwrap();

        let mut best_thread_id = None;
        let mut best_priority = i32::MAX;
        let mut yielded_alternative_thread_id = None;
        let mut yielded_priority = i32::MAX;
        let yielded_thread_id = self.yielded_thread_id;

        for thread_id in &process.thread_list {
            let Some(thread) = process.get_thread_by_thread_id(*thread_id) else {
                continue;
            };

            let thread = thread.lock().unwrap();
            if thread.get_raw_state() != ThreadState::RUNNABLE {
                continue;
            }

            if thread.get_priority() < best_priority {
                best_priority = thread.get_priority();
                best_thread_id = Some(thread.get_thread_id());
            } else if thread.get_priority() == best_priority {
                let replace = match best_thread_id {
                    None => true,
                    Some(best) => {
                        thread.get_thread_id() == current_thread_id && best != current_thread_id
                    }
                };
                if replace {
                    best_thread_id = Some(thread.get_thread_id());
                }
            }

            if yielded_thread_id == Some(current_thread_id)
                && thread.get_thread_id() != current_thread_id
                && thread.get_priority() <= yielded_priority
            {
                yielded_priority = thread.get_priority();
                yielded_alternative_thread_id = Some(thread.get_thread_id());
            }
        }

        let next_thread_id = if yielded_thread_id == Some(current_thread_id) {
            match yielded_alternative_thread_id {
                Some(candidate) if yielded_priority == best_priority => Some(candidate),
                _ => best_thread_id,
            }
        } else {
            best_thread_id
        };

        if let Some(yielded_thread_id) = yielded_thread_id {
            if next_thread_id == Some(yielded_thread_id) {
                if let Some(current_thread) = process.get_thread_by_thread_id(yielded_thread_id) {
                    current_thread
                        .lock()
                        .unwrap()
                        .set_yield_schedule_count(process.get_scheduled_count());
                }
            }
        }

        self.yielded_thread_id = None;
        next_thread_id
    }
}
