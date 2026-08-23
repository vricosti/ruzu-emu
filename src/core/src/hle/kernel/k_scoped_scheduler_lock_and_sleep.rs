//! Port of zuyu/src/core/hle/kernel/k_scoped_scheduler_lock_and_sleep.h
//! Status: COMPLET
//! Derniere synchro: 2026-03-21
//!
//! KScopedSchedulerLockAndSleep — RAII guard that locks the scheduler and
//! optionally registers a timer task on drop.

use std::sync::Arc;

use super::k_hardware_timer::KHardwareTimer;
use super::k_scheduler_lock::KAbstractSchedulerLock;
/// KScopedSchedulerLockAndSleep — acquires the scheduler lock on construction,
/// and on drop registers an absolute timer task if the timeout is positive,
/// then releases the scheduler lock.
///
/// Matches upstream `Kernel::KScopedSchedulerLockAndSleep`.
pub struct KScopedSchedulerLockAndSleep<'a> {
    m_scheduler_lock: &'a KAbstractSchedulerLock,
    m_timeout_tick: i64,
    m_thread_id: u64,
    m_thread_ptr: usize,
    m_timer: Option<Arc<KHardwareTimer>>,
}

impl<'a> KScopedSchedulerLockAndSleep<'a> {
    /// Create a new scoped scheduler lock and sleep.
    ///
    /// Matches upstream constructor:
    /// - Locks the scheduler lock
    /// - Sets the hardware timer reference if timeout > 0
    /// - Returns the timer reference via `out_timer` for caller use
    #[track_caller]
    pub fn new(
        scheduler_lock: &'a KAbstractSchedulerLock,
        hardware_timer: Option<&Arc<KHardwareTimer>>,
        thread_id: u64,
        thread_ptr: usize,
        timeout_tick: i64,
    ) -> (Self, Option<Arc<KHardwareTimer>>) {
        let caller = std::panic::Location::caller();
        log::trace!(
            "KScopedSchedulerLockAndSleep::new tid={} timeout_tick={} caller={}:{}",
            thread_id,
            timeout_tick,
            caller.file(),
            caller.line()
        );
        // Lock the scheduler.
        scheduler_lock.lock();

        // Set our timer only if the time is positive.
        let timer = if timeout_tick > 0 {
            hardware_timer.cloned()
        } else {
            None
        };

        let out_timer = timer.clone();
        (
            Self {
                m_scheduler_lock: scheduler_lock,
                m_timeout_tick: timeout_tick,
                m_thread_id: thread_id,
                m_thread_ptr: thread_ptr,
                m_timer: timer,
            },
            out_timer,
        )
    }

    /// Cancel the sleep — prevents the timer registration on drop.
    /// Matches upstream `CancelSleep()`.
    pub fn cancel_sleep(&mut self) {
        self.m_timeout_tick = 0;
    }

    /// Check if a timer will be registered on drop.
    pub fn has_timer(&self) -> bool {
        self.m_timeout_tick > 0
    }
}

impl Drop for KScopedSchedulerLockAndSleep<'_> {
    fn drop(&mut self) {
        let timeout_tick = self.m_timeout_tick;
        let thread_id = self.m_thread_id;
        let thread_ptr = self.m_thread_ptr;
        let timer = self.m_timer.clone();
        log::trace!(
            "KScopedSchedulerLockAndSleep::drop tid={} timeout_tick={} timer={}",
            self.m_thread_id,
            self.m_timeout_tick,
            self.m_timer.is_some()
        );
        if timeout_tick > 0 {
            if let Some(timer) = timer {
                timer.register_absolute_task_by_id(thread_id, thread_ptr, timeout_tick);
            }
        }

        log::trace!(
            "KScopedSchedulerLockAndSleep::drop tid={} before unlock",
            self.m_thread_id
        );
        self.m_scheduler_lock.unlock();
        log::trace!(
            "KScopedSchedulerLockAndSleep::drop tid={} after unlock",
            self.m_thread_id
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::hle::kernel::k_thread::{KThread, KThreadLock};

    #[test]
    fn test_cancel_sleep() {
        let lock = KAbstractSchedulerLock::new();
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        let thread_ptr = {
            let mut thread = thread.lock().unwrap();
            (&mut *thread) as *mut KThread as usize
        };
        let (mut slp, _timer) = KScopedSchedulerLockAndSleep::new(&lock, None, 1, thread_ptr, 100);
        assert!(slp.has_timer());
        slp.cancel_sleep();
        assert!(!slp.has_timer());
    }
}
