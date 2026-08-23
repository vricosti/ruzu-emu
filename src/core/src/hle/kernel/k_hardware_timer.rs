//! Port of zuyu/src/core/hle/kernel/k_hardware_timer.h/.cpp
//! Status: EN COURS
//! Derniere synchro: 2026-03-16
//!
//! KHardwareTimer: the kernel hardware timer, responsible for scheduling
//! timer callbacks via CoreTiming. Inherits from KHardwareTimerBase.

use std::collections::HashMap;
use std::mem;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use super::k_hardware_timer_base::KHardwareTimerBase;
use super::k_scheduler_lock::KScopedSchedulerLock;
use super::k_thread::{KScopedDisableDispatch, KThread, KThreadLock};
use crate::core_timing::{self, CoreTiming, EventType, UnscheduleEventType};

/// The kernel hardware timer.
///
/// Upstream inherits from KInterruptTask and KHardwareTimerBase.
/// It registers a CoreTiming callback to fire DoTask() at the appropriate
/// absolute time.
struct KHardwareTimerState {
    base: KHardwareTimerBase,
    /// Absolute time in nanoseconds for the next wakeup.
    m_wakeup_time: i64,
    /// CoreTiming event for scheduling callbacks.
    m_event_type: Option<Arc<parking_lot::Mutex<EventType>>>,
    /// CoreTiming reference for scheduling/unscheduling events.
    core_timing: Option<Arc<CoreTiming>>,
    /// Map of thread_id -> raw KThread pointer for resolving timer tasks.
    /// Upstream stores the `KTimerTask*` directly because `KThread` inherits it.
    /// Rust uses a raw pointer here so timer delivery can run under the
    /// scheduler lock without relocking the sleeping thread's mutex.
    thread_ptrs: HashMap<u64, usize>,
    /// GSC reference for PQ updates when timer wakes threads.
    gsc: Option<Weak<Mutex<super::global_scheduler_context::GlobalSchedulerContext>>>,
}

pub struct KHardwareTimer {
    state: Mutex<KHardwareTimerState>,
}

enum TimerTaskTarget {
    ThreadArc(Arc<KThreadLock>),
    RawPtr(usize),
}

fn with_current_emu_thread_dispatch_disabled<R>(f: impl FnOnce() -> R) -> R {
    let current_thread = super::kernel::get_current_emu_thread();
    let _dispatch_guard = current_thread.as_ref().map(KScopedDisableDispatch::new);
    f()
}

impl KHardwareTimer {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(KHardwareTimerState {
                base: KHardwareTimerBase::new(),
                m_wakeup_time: i64::MAX,
                m_event_type: None,
                core_timing: None,
                thread_ptrs: HashMap::new(),
                gsc: None,
            }),
        }
    }

    pub fn set_gsc(
        &mut self,
        gsc: Weak<Mutex<super::global_scheduler_context::GlobalSchedulerContext>>,
    ) {
        self.state.lock().unwrap().gsc = Some(gsc);
    }

    /// Create the CoreTiming callback.
    /// Upstream: `KHardwareTimer::Initialize()` in k_hardware_timer.cpp
    ///
    /// Must be called after the timer is placed behind Arc<Mutex<>> so that
    /// `wire_callback` can capture a weak reference to self.
    pub fn initialize(&mut self) {
        // Event creation deferred to wire_callback() because we need
        // a Weak<Mutex<KHardwareTimer>> reference for the callback closure.
    }

    /// Wire the CoreTiming callback. Must be called after the timer is
    /// wrapped in Arc<Mutex<>> in KernelCore.
    ///
    /// Matches upstream constructor behavior where `this` pointer is captured
    /// in the CoreTiming callback lambda.
    pub fn wire_callback(timer: &Arc<KHardwareTimer>, core_timing: Arc<CoreTiming>) {
        let timer_weak = Arc::downgrade(timer);
        let event_type = core_timing::create_event(
            "KHardwareTimer::Callback".to_string(),
            Box::new(move |_late, _ns| {
                if let Some(timer) = timer_weak.upgrade() {
                    timer.do_task();
                }
                None
            }),
        );
        let mut state = timer.state.lock().unwrap();
        state.m_event_type = Some(event_type);
        state.core_timing = Some(core_timing);
    }

    /// Unschedule the event and clean up.
    /// Matches upstream: `KHardwareTimer::Finalize()`
    ///
    /// Takes `&self` like every other method on this type. Upstream owns the
    /// timer through a `std::unique_ptr` and calls `Finalize()` through it
    /// without caring how many `KHardwareTimer*` raw pointers are still around
    /// — `KThreadQueue` holds exactly such a pointer. Rust models those raw
    /// pointers as `Arc` clones, so requiring `&mut self` here would demand
    /// sole ownership that upstream never has: a thread queue outliving the
    /// kernel by one drop is normal, not an error.
    pub fn finalize(&self) {
        let event = {
            let mut state = self.state.lock().unwrap();
            state.m_wakeup_time = i64::MAX;
            state
                .core_timing
                .as_ref()
                .zip(state.m_event_type.as_ref())
                .map(|(core_timing, event_type)| (Arc::clone(core_timing), Arc::clone(event_type)))
        };

        if let Some((core_timing, event_type)) = event {
            // Upstream uses the default `Wait` mode in `Finalize()`. Do not
            // hold `state` while waiting: an in-flight callback may already be
            // waiting for that mutex.
            core_timing.unschedule_event(&event_type, UnscheduleEventType::Wait);
        }

        let mut state = self.state.lock().unwrap();
        state.m_wakeup_time = i64::MAX;
        state.m_event_type = None;
        state.core_timing = None;
        state.thread_ptrs.clear();
    }

    /// Get the current tick (global time in nanoseconds).
    /// Matches upstream: `KHardwareTimer::GetTick()`
    pub fn get_tick(&self) -> i64 {
        let state = self.state.lock().unwrap();
        if let Some(ref core_timing) = state.core_timing {
            core_timing.get_global_time_ns().as_nanos() as i64
        } else {
            0
        }
    }

    /// Register an absolute timer task. If the new task is earlier than
    /// the current wakeup time, re-arm the interrupt.
    ///
    /// Matches upstream: `KHardwareTimer::RegisterAbsoluteTask()`
    /// Upstream takes KTimerTask* (which is KThread via inheritance).
    /// We take thread_id + Weak<KThreadLock> for resolution.
    pub fn register_absolute_task(&self, thread: &Arc<KThreadLock>, task_time: i64) {
        let thread_id = {
            let mut thread_guard = thread.lock().unwrap();
            thread_guard.set_timer_task_time(task_time);
            thread_guard.get_thread_id()
        };
        let thread_ptr = {
            let mut thread_guard = thread.lock().unwrap();
            (&mut *thread_guard) as *mut KThread as usize
        };
        with_current_emu_thread_dispatch_disabled(|| {
            let mut state = self.state.lock().unwrap();
            state.thread_ptrs.insert(thread_id, thread_ptr);

            if state.base.register_absolute_task_impl(thread_id, task_time) {
                if task_time <= state.m_wakeup_time {
                    self.enable_interrupt_locked(&mut state, task_time);
                }
            }
        });
    }

    pub fn register_absolute_task_by_id(&self, thread_id: u64, thread_ptr: usize, task_time: i64) {
        static TRACE_WAIT_SYNC: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *TRACE_WAIT_SYNC.get_or_init(|| std::env::var_os("RUZU_TRACE_WAIT_SYNC").is_some()) {
            log::info!(
                "KHardwareTimer::register_absolute_task_by_id tid={} task_time={} now_tick={}",
                thread_id,
                task_time,
                self.get_tick()
            );
        }
        log::trace!(
            "KHardwareTimer::register_absolute_task_by_id tid={} task_time={} ptr=0x{:x}",
            thread_id,
            task_time,
            thread_ptr
        );
        with_current_emu_thread_dispatch_disabled(|| {
            log::trace!(
                "KHardwareTimer::register_absolute_task_by_id tid={} before state.lock",
                thread_id
            );
            let mut state = self.state.lock().unwrap();
            log::trace!(
                "KHardwareTimer::register_absolute_task_by_id tid={} after state.lock wakeup_time={}",
                thread_id,
                state.m_wakeup_time
            );
            state.thread_ptrs.insert(thread_id, thread_ptr);

            if state.base.register_absolute_task_impl(thread_id, task_time) {
                log::trace!(
                    "KHardwareTimer::register_absolute_task_by_id tid={} inserted task_time={} wakeup_time={}",
                    thread_id,
                    task_time,
                    state.m_wakeup_time
                );
                if task_time <= state.m_wakeup_time {
                    log::trace!(
                        "KHardwareTimer::register_absolute_task_by_id tid={} enabling interrupt for task_time={}",
                        thread_id,
                        task_time
                    );
                    self.enable_interrupt_locked(&mut state, task_time);
                }
            }
        });
    }

    /// Cancel a task.
    /// Matches upstream: `KHardwareTimerBase::CancelTask()`
    pub fn cancel_task(&self, thread: &Arc<KThreadLock>) {
        let (thread_id, task_time) = {
            let thread_guard = thread.lock().unwrap();
            (
                thread_guard.get_thread_id(),
                thread_guard.get_timer_task_time(),
            )
        };
        with_current_emu_thread_dispatch_disabled(|| {
            let mut state = self.state.lock().unwrap();
            if task_time > 0 {
                state.base.cancel_task(thread_id, task_time);
            }
            thread.lock().unwrap().set_timer_task_time(0);
            state.thread_ptrs.remove(&thread_id);
        });
    }

    pub fn cancel_task_by_id(&self, thread_id: u64, task_time: i64) {
        with_current_emu_thread_dispatch_disabled(|| {
            let mut state = self.state.lock().unwrap();
            if task_time > 0 {
                state.base.cancel_task(thread_id, task_time);
            }
            state.thread_ptrs.remove(&thread_id);
        });
    }

    /// Matches upstream: `KHardwareTimer::EnableInterrupt()`
    fn enable_interrupt_locked(&self, state: &mut KHardwareTimerState, wakeup_time: i64) {
        log::trace!(
            "KHardwareTimer::enable_interrupt_locked wakeup_time={} old_wakeup_time={}",
            wakeup_time,
            state.m_wakeup_time
        );
        self.disable_interrupt_locked(state);

        state.m_wakeup_time = wakeup_time;
        if let (Some(ref core_timing), Some(ref event_type)) =
            (&state.core_timing, &state.m_event_type)
        {
            log::trace!(
                "KHardwareTimer::enable_interrupt_locked before schedule_event wakeup_time={}",
                wakeup_time
            );
            core_timing.schedule_event(
                Duration::from_nanos(wakeup_time as u64),
                event_type,
                true, // absolute time
            );
            log::trace!(
                "KHardwareTimer::enable_interrupt_locked after schedule_event wakeup_time={}",
                wakeup_time
            );
        }
    }

    /// Rearm the timer from inside the timer callback.
    ///
    /// Upstream `DoTask()` notes that disabling the interrupt is not necessary
    /// because CoreTiming has already popped the current event before invoking
    /// the callback. Re-entering `unschedule_event()` from this callback can
    /// deadlock our Rust CoreTiming implementation, so the callback path must
    /// only update `m_wakeup_time` and schedule the next absolute event.
    fn rearm_interrupt_after_callback_locked(
        &self,
        state: &mut KHardwareTimerState,
        wakeup_time: i64,
    ) {
        log::trace!(
            "KHardwareTimer::rearm_interrupt_after_callback_locked wakeup_time={} old_wakeup_time={}",
            wakeup_time,
            state.m_wakeup_time
        );
        state.m_wakeup_time = wakeup_time;
        if let (Some(ref core_timing), Some(ref event_type)) =
            (&state.core_timing, &state.m_event_type)
        {
            log::trace!(
                "KHardwareTimer::rearm_interrupt_after_callback_locked before schedule_event wakeup_time={}",
                wakeup_time
            );
            core_timing.schedule_event(Duration::from_nanos(wakeup_time as u64), event_type, true);
            log::trace!(
                "KHardwareTimer::rearm_interrupt_after_callback_locked after schedule_event wakeup_time={}",
                wakeup_time
            );
        }
    }

    /// Matches upstream: `KHardwareTimer::DisableInterrupt()`
    fn disable_interrupt_locked(&self, state: &mut KHardwareTimerState) {
        if !Self::get_interrupt_enabled_locked(state) {
            return;
        }
        if let (Some(ref core_timing), Some(ref event_type)) =
            (&state.core_timing, &state.m_event_type)
        {
            core_timing.unschedule_event(event_type, UnscheduleEventType::NoWait);
        }
        state.m_wakeup_time = i64::MAX;
    }

    fn get_interrupt_enabled_locked(state: &KHardwareTimerState) -> bool {
        state.m_wakeup_time != i64::MAX
    }

    /// Called by the CoreTiming callback.
    /// Matches upstream: `KHardwareTimer::DoTask()`
    fn do_task(&self) {
        static TRACE_CT_FIRE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let trace = *TRACE_CT_FIRE.get_or_init(|| std::env::var_os("RUZU_TRACE_CT_FIRE").is_some());
        if trace {
            log::info!("KHardwareTimer::do_task entry");
        }
        let gsc = self
            .state
            .lock()
            .unwrap()
            .gsc
            .as_ref()
            .and_then(Weak::upgrade);
        if trace {
            log::info!("KHardwareTimer::do_task before_scheduler_lock");
        }
        // do_task MUST run under the scheduler lock — it mutates kernel
        // wait/waiter state (on_timer -> cancel_wait -> remove_waiter*) that
        // guest fibers also mutate under that same lock. Acquire the kernel's
        // singleton scheduler lock (the very instance every guest SVC path and
        // the audio/ServerManager host threads use); the timer's own `gsc` Weak
        // frequently upgrades to None, which previously left do_task running
        // unserialized and racing guest fibers on `held_lock_info_list` /
        // waiter trees (OOB and BTreeSet-corruption panics — 0 panics after).
        //
        // Deadlock-free only because `CoreTiming::advance` now releases the
        // EventType Mutex before invoking this callback (see core_timing.rs):
        // otherwise a guest holding the scheduler lock while (un)scheduling a
        // timer event would invert against the timing thread holding that Mutex.
        //
        // A/B-tested: this does NOT regress the pre-existing ~50% `pl:u`
        // host-thread-IPC boot stall (committed baseline stalls at the same
        // rate). The bounds-checked accessors in k_thread.rs remain as a safety
        // net for the rare `scheduler_lock unavailable` startup window.
        let local_scheduler_lock = if super::kernel::scheduler_lock().is_none() {
            gsc.as_ref().map(|gsc| {
                let gsc = gsc.lock().unwrap();
                &gsc.m_scheduler_lock as *const super::k_scheduler_lock::KAbstractSchedulerLock
            })
        } else {
            None
        };
        let scheduler_lock = super::kernel::scheduler_lock().or_else(|| {
            local_scheduler_lock.map(|lock| {
                // SAFETY: `gsc` above owns the scheduler lock and remains
                // alive for the duration of this callback.
                unsafe { &*lock }
            })
        });
        let Some(scheduler_lock) = scheduler_lock else {
            // There is no kernel waiter state to update once both the kernel
            // singleton and the timer's GSC owner have gone away.
            log::debug!("KHardwareTimer::do_task: scheduler lock unavailable during shutdown");
            return;
        };
        let _scheduler_guard = KScopedSchedulerLock::new(scheduler_lock);
        if trace {
            log::info!("KHardwareTimer::do_task after_scheduler_lock");
        }
        if trace {
            log::info!("KHardwareTimer::do_task before_state_lock");
        }
        let mut state = self.state.lock().unwrap();
        if trace {
            log::info!("KHardwareTimer::do_task after_state_lock");
        }

        if !Self::get_interrupt_enabled_locked(&state) {
            if trace {
                log::info!("KHardwareTimer::do_task interrupt_not_enabled early_return");
            }
            return;
        }

        // Disable the timer interrupt while we handle this.
        state.m_wakeup_time = i64::MAX;

        let cur_tick = if let Some(ref core_timing) = state.core_timing {
            core_timing.get_global_time_ns().as_nanos() as i64
        } else {
            0
        };

        let trace_enabled = trace;
        let gsc_ref = gsc.as_ref();
        let mut thread_ptrs = mem::take(&mut state.thread_ptrs);
        let next_time = state.base.do_interrupt_task_impl(cur_tick, |task_id| {
            let target = if let Some(gsc) = gsc_ref {
                gsc.lock()
                    .unwrap()
                    .get_thread_by_thread_id(task_id)
                    .map(TimerTaskTarget::ThreadArc)
            } else {
                None
            }
            .or_else(|| {
                thread_ptrs
                    .get(&task_id)
                    .copied()
                    .filter(|ptr| *ptr != 0)
                    .map(TimerTaskTarget::RawPtr)
            });
            thread_ptrs.remove(&task_id);

            let Some(target) = target else {
                return;
            };

            match target {
                TimerTaskTarget::ThreadArc(thread) => {
                    if trace_enabled {
                        log::info!("KHardwareTimer::do_task before_thread_lock (ThreadArc)");
                    }
                    let mut thread = thread.lock().unwrap();
                    if trace_enabled {
                        log::info!(
                            "KHardwareTimer::do_task after_thread_lock tid={}",
                            thread.get_thread_id()
                        );
                    }
                    log::trace!(
                        "KHardwareTimer::do_task delivering tid={} state={:?} active_core={} current_core={}",
                        thread.get_thread_id(),
                        thread.get_state(),
                        thread.get_active_core(),
                        thread.get_current_core()
                    );
                    thread.set_timer_task_time(0);
                    thread.on_timer();
                }
                TimerTaskTarget::RawPtr(thread_ptr) => {
                    if trace_enabled {
                        log::info!("KHardwareTimer::do_task RawPtr delivery");
                    }
                    let thread = unsafe { &mut *(thread_ptr as *mut KThread) };
                    if trace_enabled {
                        log::info!(
                            "KHardwareTimer::do_task RawPtr tid={} pre_on_timer",
                            thread.get_thread_id()
                        );
                    }
                    log::trace!(
                        "KHardwareTimer::do_task delivering raw tid={} state={:?} active_core={} current_core={}",
                        thread.get_thread_id(),
                        thread.get_state(),
                        thread.get_active_core(),
                        thread.get_current_core()
                    );
                    thread.set_timer_task_time(0);
                    thread.on_timer();
                }
            }
        });
        state.thread_ptrs = thread_ptrs;
        log::trace!(
            "KHardwareTimer::do_task cur_tick={} next_time={}",
            cur_tick,
            next_time
        );

        if next_time > 0 && next_time <= state.m_wakeup_time {
            self.rearm_interrupt_after_callback_locked(&mut state, next_time);
        }
    }
}

impl Default for KHardwareTimer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_timing::CoreTiming;
    use crate::hle::kernel::global_scheduler_context::GlobalSchedulerContext;
    use crate::hle::kernel::k_thread::ThreadState;

    /// Upstream owns the timer through a `std::unique_ptr` and calls
    /// `Finalize()` through it no matter how many `KHardwareTimer*` raw
    /// pointers `KThreadQueue`s are still holding. Those raw pointers are
    /// `Arc` clones here, so finalizing must not require sole ownership —
    /// asserting it aborted `KernelCore::shutdown` when a thread queue
    /// outlived the kernel by one drop.
    #[test]
    fn the_timer_finalizes_while_a_thread_queue_still_holds_it() {
        let timer = Arc::new(KHardwareTimer::new());
        let core_timing = Arc::new(CoreTiming::new());
        KHardwareTimer::wire_callback(&timer, Arc::clone(&core_timing));
        {
            let mut state = timer.state.lock().unwrap();
            state.m_wakeup_time = 42;
            state.thread_ptrs.insert(17, 0xdead_beef);
        }

        // The clone a sleeping `KThreadQueue` would be holding.
        let queue_side = Arc::clone(&timer);

        timer.finalize();

        let state = queue_side.state.lock().unwrap();
        assert_eq!(state.m_wakeup_time, i64::MAX);
        assert!(state.m_event_type.is_none());
        assert!(state.core_timing.is_none());
        assert!(state.thread_ptrs.is_empty());
    }

    #[test]
    fn do_task_wakes_waiting_thread_via_gsc_owner() {
        let mut timer = KHardwareTimer::new();
        let gsc = Arc::new(Mutex::new(GlobalSchedulerContext::new()));
        let core_timing = CoreTiming::new();
        core_timing.set_multicore(true);
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut guard = thread.lock().unwrap();
            guard.thread_id = 17;
            guard.begin_wait();
            guard.set_timer_task_time(10);
            guard.bind_self_reference(&thread);
        }
        gsc.lock().unwrap().add_thread(Arc::clone(&thread));
        timer.set_gsc(Arc::downgrade(&gsc));
        {
            let mut state = timer.state.lock().unwrap();
            state.core_timing = Some(Arc::new(core_timing));
            state.m_wakeup_time = 10;
            state.base.register_absolute_task_impl(17, 10);
        }

        timer.do_task();

        let guard = thread.lock().unwrap();
        assert_eq!(guard.get_state(), ThreadState::RUNNABLE);
        assert_eq!(
            guard.wait_result,
            crate::hle::kernel::svc::svc_results::RESULT_TIMED_OUT.get_inner_value()
        );
        assert_eq!(guard.get_timer_task_time(), 0);
    }

    #[test]
    fn do_task_rearms_next_deadline_without_unscheduling_callback_event() {
        let timer = Arc::new(KHardwareTimer::new());
        let gsc = Arc::new(Mutex::new(GlobalSchedulerContext::new()));
        let core_timing = Arc::new(CoreTiming::new());
        core_timing.set_multicore(true);
        KHardwareTimer::wire_callback(&timer, Arc::clone(&core_timing));
        let first_deadline = (core_timing.get_global_time_ns().as_nanos() as i64).max(1);
        let second_deadline = first_deadline + 1_000_000_000;
        {
            let mut state = timer.state.lock().unwrap();
            state.gsc = Some(Arc::downgrade(&gsc));
            state.m_wakeup_time = first_deadline;
            state.base.register_absolute_task_impl(1, first_deadline);
            state.base.register_absolute_task_impl(2, second_deadline);
        }

        let waiter1 = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut guard = waiter1.lock().unwrap();
            guard.thread_id = 1;
            guard.begin_wait();
            guard.set_timer_task_time(first_deadline);
        }
        let waiter2 = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut guard = waiter2.lock().unwrap();
            guard.thread_id = 2;
            guard.begin_wait();
            guard.set_timer_task_time(second_deadline);
        }
        {
            let gsc_guard = gsc.lock().unwrap();
            gsc_guard.add_thread(Arc::clone(&waiter1));
            gsc_guard.add_thread(Arc::clone(&waiter2));
        }

        timer.do_task();

        let state = timer.state.lock().unwrap();
        assert_eq!(state.m_wakeup_time, second_deadline);
        drop(state);
        assert_eq!(waiter1.lock().unwrap().get_state(), ThreadState::RUNNABLE);
        assert_eq!(waiter2.lock().unwrap().get_state(), ThreadState::WAITING);
    }
}
