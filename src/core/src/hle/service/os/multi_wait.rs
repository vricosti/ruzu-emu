// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/os/multi_wait.h
//! Port of zuyu/src/core/hle/service/os/multi_wait.cpp
//!
//! MultiWait — waits on multiple synchronization objects.
//! Upstream wraps svcWaitSynchronization/KSynchronizationObject::Wait.

use std::ffi::OsStr;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::hle::kernel::k_synchronization_object;
use crate::hle::kernel::kernel::KernelCore;
use crate::hle::result::RESULT_SUCCESS;

use super::multi_wait_holder::MultiWaitHolder;

/// Host-side notification token for the local wait-many fallback. Unlike an
/// emulated thread, an unregistered host worker cannot use the guest scheduler's
/// wait queue. Remember notifications between the signaled scan and host sleep.
#[derive(Default)]
pub(super) struct HostMultiWaitSignal {
    notified: Mutex<bool>,
    cv: Condvar,
}

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
    fn boot_trace_enabled() -> bool {
        std::env::var_os("RUZU_APPLET_BOOT_TRACE").is_some_and(|value| value != OsStr::new("0"))
    }

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

    /// Rust-only helper for host-side service threads that must avoid the
    /// kernel-backed wait path entirely.
    ///
    /// This performs the same non-blocking signaled scan as the local fallback
    /// path without consulting `KernelCore` or `WaitSynchronization`.
    pub fn try_wait_any_local(&self) -> Option<*mut MultiWaitHolder> {
        let holders = self.holders_snapshot();
        self.local_try_wait_any(&holders)
    }

    /// Wait without a guest-thread context, using host notifications for Events.
    pub fn wait_any_local(&self) -> Option<*mut MultiWaitHolder> {
        self.local_timed_wait(&self.holders, -1)
    }

    fn timed_wait_impl(
        &self,
        kernel: &KernelCore,
        timeout_ns: i64,
    ) -> Option<*mut MultiWaitHolder> {
        let trace_boot = Self::boot_trace_enabled();
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

        let current_thread = match kernel.get_current_emu_thread() {
            Some(thread) => thread,
            None => {
                if trace_boot {
                    log::info!(
                        "MultiWait::timed_wait_impl: falling back local (no current_emu_thread)"
                    );
                }
                return self.local_timed_wait(&holders, timeout_ns);
            }
        };
        let process = match current_thread
            .lock()
            .unwrap()
            .parent
            .as_ref()
            .and_then(|parent| parent.upgrade())
        {
            Some(process) => process,
            None => {
                if trace_boot {
                    log::info!("MultiWait::timed_wait_impl: falling back local (no process)");
                }
                return self.local_timed_wait(&holders, timeout_ns);
            }
        };
        let scheduler = kernel
            .current_scheduler()
            .cloned()
            .or_else(|| {
                current_thread
                    .lock()
                    .unwrap()
                    .scheduler
                    .as_ref()
                    .and_then(|scheduler| scheduler.upgrade())
            })
            .or_else(|| {
                process
                    .lock()
                    .unwrap()
                    .scheduler
                    .as_ref()
                    .and_then(|scheduler| scheduler.upgrade())
            });
        let Some(scheduler) = scheduler else {
            if trace_boot {
                log::info!("MultiWait::timed_wait_impl: falling back local (no scheduler)");
            }
            return self.local_timed_wait(&holders, timeout_ns);
        };

        let mut object_ids = Vec::with_capacity(holders.len());
        let mut waitable_objects = Vec::with_capacity(holders.len());
        let mut kinds: Vec<&'static str> = if trace_wait {
            Vec::with_capacity(holders.len())
        } else {
            Vec::new()
        };
        for holder in &holders {
            let Some((object_id, waitable_object)) =
                (unsafe { &**holder }).native_waitable_object()
            else {
                if trace_boot || trace_wait {
                    eprintln!(
                        "[MULTI_WAIT] holders={} → falling back local (holder missing native object)",
                        holders.len()
                    );
                }
                return self.local_timed_wait(&holders, timeout_ns);
            };
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
            &scheduler,
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

        if result == RESULT_SUCCESS && out_index >= 0 {
            holders.get(out_index as usize).copied()
        } else {
            None
        }
    }

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
        let notification = if holders.iter().all(|holder| unsafe {
            (**holder).host_event().is_some()
        }) {
            let notification = Arc::new(HostMultiWaitSignal::default());
            for &holder in holders {
                unsafe { (*holder).host_event().unwrap() }.register_host_waiter(&notification);
            }
            Some(notification)
        } else {
            None
        };
        let timeout = (timeout_ns > 0).then(|| Duration::from_nanos(timeout_ns as u64));
        loop {
            if let Some(holder) = self.local_try_wait_any(holders) {
                return Some(holder);
            }
            let remaining = timeout.map(|timeout| timeout.saturating_sub(start.elapsed()));
            if remaining.is_some_and(|remaining| remaining.is_zero()) {
                return None;
            }
            if let Some(notification) = &notification {
                notification.wait(remaining);
            } else {
                // Other native holder types still use their kernel wait path
                // when a guest context exists; preserve the legacy host fallback.
                let interval = Duration::from_micros(100);
                std::thread::sleep(remaining.map_or(interval, |left| left.min(interval)));
            }
        }
    }

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
