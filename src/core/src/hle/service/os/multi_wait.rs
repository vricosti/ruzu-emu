// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/os/multi_wait.h
//! Port of zuyu/src/core/hle/service/os/multi_wait.cpp
//!
//! MultiWait — waits on multiple synchronization objects.
//! Upstream wraps svcWaitSynchronization/KSynchronizationObject::Wait.

#[cfg(test)]
use std::sync::{Arc, Condvar, Mutex};
#[cfg(test)]
use std::time::{Duration, Instant};

use crate::hle::kernel::k_synchronization_object;
use crate::hle::kernel::kernel::KernelCore;

use super::multi_wait_holder::MultiWaitHolder;

/// Notification token used only by null-kernel unit-test fixtures. Production
/// host workers use their own dummy KThread and the native synchronization list.
/// Remember notifications between the fixture's signaled scan and host sleep.
#[derive(Default)]
#[cfg(test)]
pub(super) struct HostMultiWaitSignal {
    notified: Mutex<bool>,
    cv: Condvar,
}

#[cfg(test)]
impl HostMultiWaitSignal {
    pub(super) fn signal(&self) {
        *self.notified.lock().unwrap() = true;
        self.cv.notify_all();
    }

    fn wait(&self, timeout: Option<Duration>) {
        let notified = self.notified.lock().unwrap();
        let mut notified = if let Some(timeout) = timeout {
            self.cv.wait_timeout_while(notified, timeout, |v| !*v).unwrap().0
        } else {
            self.cv.wait_while(notified, |v| !*v).unwrap()
        };
        *notified = false;
    }
}

/// MultiWait — manages a list of MultiWaitHolders to wait on.
///
/// Upstream stores an intrusive list of MultiWaitHolder and calls
/// svcWaitSynchronization on all their native handles.
pub struct MultiWait {
    pub(crate) holders: Vec<*mut MultiWaitHolder>,
}

// Safety: MultiWait holders are managed by the service layer on a single thread.
unsafe impl Send for MultiWait {}
unsafe impl Sync for MultiWait {}

impl MultiWait {
    pub fn new() -> Self {
        Self {
            holders: Vec::new(),
        }
    }

    /// Link a holder to this MultiWait.
    pub fn link_holder(&mut self, holder: *mut MultiWaitHolder) {
        unsafe {
            (*holder).link_to_multi_wait(self as *mut MultiWait);
        }
    }

    /// Unlink a holder from this MultiWait.
    pub fn unlink_holder(&mut self, holder: *mut MultiWaitHolder) {
        unsafe {
            (*holder).unlink_from_multi_wait();
        }
    }

    /// Port of upstream `MultiWait::MoveAll`.
    ///
    /// Rust move-aware adaptation of upstream intrusive-list splicing.
    ///
    /// Upstream can splice holders without repairing owner pointers because
    /// the intrusive node lives in a stable pointee. Rust service owners can
    /// move `MultiWait` values, so this method drains `other.holders`
    /// directly and rewrites each holder backlink instead of relying on the
    /// previous `holder.multi_wait` owner pointer still being valid.
    pub fn move_all(&mut self, other: &mut MultiWait) {
        let moved: Vec<*mut MultiWaitHolder> = other.holders.drain(..).collect();
        for holder in moved {
            unsafe {
                (*holder).reset_multi_wait_linkage_for_owner_move();
                (*holder).link_to_multi_wait(self as *mut MultiWait);
            }
        }
    }

    pub fn holders_snapshot(&self) -> Vec<*mut MultiWaitHolder> {
        self.holders.clone()
    }

    /// WaitAny — block until any holder is signaled, return the signaled holder.
    /// Port of upstream `MultiWait::WaitAny()`.
    /// Upstream calls svcWaitSynchronization on all holder handles.
    pub fn wait_any(&self, kernel: &KernelCore) -> Option<*mut MultiWaitHolder> {
        self.timed_wait_impl(kernel, -1)
    }

    /// TryWaitAny — non-blocking check, return the first signaled holder if any.
    /// Port of upstream `MultiWait::TryWaitAny()`.
    pub fn try_wait_any(&self, kernel: &KernelCore) -> Option<*mut MultiWaitHolder> {
        self.timed_wait_impl(kernel, 0)
    }

    /// Relative nanoseconds converted to the kernel hardware timer's absolute
    /// global-time deadline, as in upstream TimedWaitAny. Both host and guest
    /// suspension are expired by that timer, not by a second host wall clock.
    pub fn timed_wait_any(&self, kernel: &KernelCore, timeout_ns: i64) -> Option<*mut MultiWaitHolder> {
        let now = kernel.hardware_timer().expect("MultiWait requires a hardware timer").get_tick();
        self.timed_wait_impl(kernel, now.saturating_add(timeout_ns))
    }

    /// Non-blocking scan for null-kernel unit-test fixtures only.
    #[cfg(test)]
    pub fn try_wait_any_local(&self) -> Option<*mut MultiWaitHolder> {
        let holders = self.holders_snapshot();
        self.local_try_wait_any(&holders)
    }

    /// Block a null-kernel unit-test fixture on its local Event notifications.
    #[cfg(test)]
    pub fn wait_any_local(&self) -> Option<*mut MultiWaitHolder> {
        self.local_timed_wait(&self.holders, -1)
    }

    fn timed_wait_impl(
        &self,
        kernel: &KernelCore,
        timeout_ns: i64,
    ) -> Option<*mut MultiWaitHolder> {
        let trace_wait = std::env::var_os("RUZU_TRACE_MULTI_WAIT").is_some();
        let holders = self.holders_snapshot();
        assert!(
            holders.len() <= k_synchronization_object::ARGUMENT_HANDLE_COUNT_MAX,
            "MultiWait exceeds ArgumentHandleCountMax"
        );
        if holders.is_empty() {
            if trace_wait {
                eprintln!("[MULTI_WAIT] timeout={} holders=empty → None", timeout_ns);
            }
            return None;
        }

        let current_thread = kernel.get_current_emu_thread()
            .expect("MultiWait requires an initialized kernel thread identity");

        let mut object_ids = Vec::with_capacity(holders.len());
        let mut waitable_objects = Vec::with_capacity(holders.len());
        let mut kinds: Vec<&'static str> = if trace_wait {
            Vec::with_capacity(holders.len())
        } else {
            Vec::new()
        };
        for holder in &holders {
            let (object_id, waitable_object) = (unsafe { &**holder }).native_waitable_object()
                .expect("MultiWait holder must own a native kernel synchronization object");
            if trace_wait {
                kinds.push((unsafe { &**holder }).kind_name());
            }
            object_ids.push(object_id);
            waitable_objects.push(waitable_object);
        }

        let mut out_index = -1;
        let object_ids_copy = if trace_wait {
            object_ids.clone()
        } else {
            Vec::new()
        };
        let result = k_synchronization_object::wait_on_objects(
            &current_thread,
            &mut out_index,
            object_ids,
            waitable_objects,
            timeout_ns,
        );

        if trace_wait {
            let pairs: Vec<String> = object_ids_copy
                .iter()
                .zip(kinds.iter())
                .map(|(id, k)| format!("{}:{}", k, id))
                .collect();
            eprintln!(
                "[MULTI_WAIT] timeout={} holders={} ids=[{}] result=0x{:X} out_index={}",
                timeout_ns,
                holders.len(),
                pairs.join(","),
                result.get_inner_value(),
                out_index
            );
        }

        if out_index >= 0 {
            holders.get(out_index as usize).copied()
        } else {
            None
        }
    }

    #[cfg(test)]
    fn local_timed_wait(
        &self,
        holders: &[*mut MultiWaitHolder],
        timeout_ns: i64,
    ) -> Option<*mut MultiWaitHolder> {
        let start = Instant::now();
        if let Some(holder) = self.local_try_wait_any(holders) {
            return Some(holder);
        }

        if timeout_ns == 0 || holders.is_empty() {
            return None;
        }

        // Register on the whole set before rescanning it. An event signaled
        // before registration is found by the scan; one signaled after it
        // remembers a notification even if it precedes the condvar wait.
        // Only null-kernel Event fixtures use this harness. Native object
        // tests must exercise the real kernel queues, never polling.
        let notification = Arc::new(HostMultiWaitSignal::default());
        for &holder in holders {
            unsafe { (*holder).host_event().expect("local test wait requires service Events") }
                .register_host_waiter(&notification);
        }
        let timeout = (timeout_ns > 0).then(|| Duration::from_nanos(timeout_ns as u64));
        loop {
            if let Some(holder) = self.local_try_wait_any(holders) {
                return Some(holder);
            }
            let remaining = timeout.map(|timeout| timeout.saturating_sub(start.elapsed()));
            if remaining.is_some_and(|remaining| remaining.is_zero()) {
                return None;
            }
            notification.wait(remaining);
        }
    }

    #[cfg(test)]
    fn local_try_wait_any(&self, holders: &[*mut MultiWaitHolder]) -> Option<*mut MultiWaitHolder> {
        for &holder in holders {
            unsafe {
                if (*holder).is_signaled() {
                    return Some(holder);
                }
            }
        }
        None
    }
}

impl Default for MultiWait {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::event::Event;
    use std::sync::mpsc;

    #[test]
    fn native_process_and_closed_session_wake_host_selection() {
        use crate::hle::kernel::{kernel, k_process::{KProcess, ProcessLock, ProcessState},
            k_server_session::KServerSession, k_scheduler_lock::KScopedSchedulerLock,
            k_synchronization_object::tests::await_kernel_registration};

        let mut kernel = Box::new(KernelCore::new());
        kernel.initialize();
        for session_case in [false, true] {
            let process = Arc::new(ProcessLock::new(KProcess::new()));
            process.lock().unwrap().process_id = 42;
            process.lock().unwrap().state = ProcessState::RunningAttached;
            let session = Arc::new(Mutex::new(KServerSession::new()));
            session.lock().unwrap().initialize(42);
            let worker_process = Arc::clone(&process);
            let worker_session = Arc::clone(&session);
            let (ready_tx, ready_rx) = mpsc::channel();
            let (done_tx, done_rx) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                let kernel = kernel::get_kernel_ref().unwrap();
                let thread = kernel.get_current_emu_thread().unwrap();
                let mut holder = if session_case {
                    MultiWaitHolder::from_server_session(worker_session)
                } else {
                    MultiWaitHolder::from_process(worker_process)
                };
                let mut wait = MultiWait::new();
                wait.link_holder(&mut holder);
                ready_tx.send(thread).unwrap();
                done_tx.send(wait.wait_any(kernel) == Some(&mut holder as *mut _)).unwrap();
            });
            let thread = ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            await_kernel_registration(&thread);
            {
                let _lock = KScopedSchedulerLock::new(kernel::scheduler_lock().unwrap());
                if session_case {
                    // The kernel result is SessionClosed, but the selected
                    // native object still must be delivered to ServerManager.
                    session.lock().unwrap().on_client_closed();
                } else {
                    process.lock().unwrap().set_debug_break();
                }
            }
            assert!(done_rx.recv_timeout(Duration::from_secs(2)).unwrap());
            worker.join().unwrap();
            assert!(session.lock().unwrap().sync_object.is_empty());
            assert!(process.lock().unwrap().sync_object.is_empty());
        }
        kernel.shutdown();
    }

    #[test]
    fn native_time_events_support_parentless_host_waits() {
        use crate::hle::kernel::kernel;
        use crate::hle::service::psc::time::common::OperationEvent;
        use crate::hle::service::psc::time::alarms::Alarms;
        use crate::hle::service::psc::time::clocks::standard_user_system_clock_core::StandardUserSystemClockCore;

        let mut kernel = Box::new(KernelCore::new());
        kernel.initialize();
        {
            let operation = OperationEvent::new();
            let alarms = Alarms::new(Box::new(|| 0));
            let correction = StandardUserSystemClockCore::new();
            let events = [operation.get_event(), alarms.get_event(), correction.get_event()];
            for event in &events {
                assert!(event.readable_event().is_some(), "time event missing native owner");
            }
            let (ready_tx, ready_rx) = mpsc::channel();
            let workers: Vec<_> = (0..2).map(|_| {
                let events = events.clone();
                let ready_tx = ready_tx.clone();
                std::thread::spawn(move || {
                    let kernel = kernel::get_kernel_ref().unwrap();
                    let thread = kernel.get_current_emu_thread().unwrap();
                    assert!(thread.lock().unwrap().parent.is_none());
                    let mut holders: Vec<_> = events.into_iter().map(MultiWaitHolder::from_event).collect();
                    let mut wait = MultiWait::new();
                    for holder in &mut holders { wait.link_holder(holder); }
                    let expected = &mut holders[1] as *mut _;
                    ready_tx.send(thread).unwrap();
                    assert_eq!(wait.wait_any(kernel), Some(expected));
                    // Manual-reset: a second wait must still observe the event.
                    assert_eq!(wait.try_wait_any(kernel), Some(expected));
                })
            }).collect();
            let first = ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            let second = ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(!Arc::ptr_eq(&first, &second), "host threads must have their own identity");
            crate::hle::kernel::k_synchronization_object::tests::await_kernel_registration(&first);
            crate::hle::kernel::k_synchronization_object::tests::await_kernel_registration(&second);
            events[1].signal();
            for worker in workers { worker.join().unwrap(); }
            assert!(events[1].readable_event().unwrap().lock().unwrap().sync_object.is_empty());
        }
        kernel.shutdown();
    }

    #[test]
    fn local_event_wait_keeps_order_and_manual_reset_state() {
        let first = Arc::new(Event::new());
        let second = Arc::new(Event::new());
        let mut first_holder = MultiWaitHolder::from_event(Arc::clone(&first));
        let mut second_holder = MultiWaitHolder::from_event(Arc::clone(&second));
        let mut wait = MultiWait::new();
        wait.link_holder(&mut first_holder);
        wait.link_holder(&mut second_holder);
        assert!(wait.local_timed_wait(&wait.holders, 0).is_none());
        second.signal_host_only();
        first.signal_host_only();
        assert_eq!(wait.wait_any_local(), Some(&mut first_holder as *mut _));
        assert!(first.is_signaled());
        first.clear();
        assert_eq!(wait.wait_any_local(), Some(&mut second_holder as *mut _));
        assert!(second.is_signaled());
    }

    #[test]
    fn local_event_wait_wakes_all_waiters_without_consuming_event() {
        let event = Arc::new(Event::new());
        let (ready_tx, ready_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let threads: Vec<_> = (0..2).map(|_| {
            let event = Arc::clone(&event);
            let ready = ready_tx.clone();
            let done = done_tx.clone();
            std::thread::spawn(move || {
                let never_signaled = Arc::new(Event::new());
                let mut first = MultiWaitHolder::from_event(never_signaled);
                let mut second = MultiWaitHolder::from_event(event);
                let mut wait = MultiWait::new();
                wait.link_holder(&mut first);
                wait.link_holder(&mut second);
                ready.send(()).unwrap();
                assert_eq!(wait.wait_any_local(), Some(&mut second as *mut _));
                done.send(()).unwrap();
            })
        }).collect();
        for _ in 0..2 {
            ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        event.signal_host_only();
        for _ in 0..2 {
            done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        for thread in threads {
            thread.join().unwrap();
        }
        assert!(event.is_signaled());
    }

    #[test]
    fn host_notification_between_scan_and_sleep_is_retained() {
        let event = Event::new();
        let notification = Arc::new(HostMultiWaitSignal::default());
        event.register_host_waiter(&notification);
        assert!(!event.is_signaled());
        // Simulate the exact lost-wakeup window, not a sleep-based race.
        event.signal_host_only();
        assert!(*notification.notified.lock().unwrap());
        notification.wait(None);
        assert!(!*notification.notified.lock().unwrap());
        assert!(event.is_signaled());
    }

    #[test]
    fn local_event_wait_honors_finite_timeout() {
        let mut holder = MultiWaitHolder::from_event(Arc::new(Event::new()));
        let mut wait = MultiWait::new();
        wait.link_holder(&mut holder);
        let start = Instant::now();
        assert!(wait.local_timed_wait(&wait.holders, 2_000_000).is_none());
        assert!(start.elapsed() >= Duration::from_millis(2));
        assert!(MultiWait::new().wait_any_local().is_none());
    }
}
