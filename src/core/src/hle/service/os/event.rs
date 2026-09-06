// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/os/event.h
//! Port of zuyu/src/core/hle/service/os/event.cpp
//!
//! Event wrapper for kernel KEvent.

use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};

use super::multi_wait::HostMultiWaitSignal;

use crate::hle::kernel::k_event::KEvent;
use crate::hle::kernel::k_process::ProcessLock;
use crate::hle::kernel::k_readable_event::KReadableEvent;
use crate::hle::service::hle_ipc::{HLERequestContext, Handle};

/// Bridge from a service-layer Event to a kernel KReadableEvent.
///
/// Upstream `Event` holds a `KEvent*` and `Signal()` calls `m_event->Signal()`
/// which in turn calls `KReadableEvent::Signal()`. This bridge replicates that
/// chain: when the service Event is signaled, we also signal the kernel readable
/// event to wake any threads blocked in WaitSynchronization.
#[derive(Clone)]
struct KernelEventBridge {
    event: Arc<Mutex<KEvent>>,
    readable_event: Arc<Mutex<KReadableEvent>>,
    process: Weak<ProcessLock>,
}

/// Event — wraps a kernel event for service use.
///
/// Upstream stores a `KEvent*` and calls `m_event->Signal()` / `m_event->Clear()`.
/// The optional `kernel_bridge` allows signaling a kernel KReadableEvent,
/// which wakes threads blocked in WaitSynchronization on the event handle.
pub struct Event {
    signaled: Arc<Mutex<bool>>,
    cv: Arc<Condvar>,
    kernel_bridge: Mutex<Option<KernelEventBridge>>,
    // Host-only counterpart of the kernel's synchronization wait list. Most
    // events never need it, so signaling them requires no additional mutex.
    host_waiters: OnceLock<Mutex<Vec<Weak<HostMultiWaitSignal>>>>,
}

impl Event {
    pub fn new() -> Self {
        Self {
            signaled: Arc::new(Mutex::new(false)),
            cv: Arc::new(Condvar::new()),
            kernel_bridge: Mutex::new(None),
            host_waiters: OnceLock::new(),
        }
    }

    /// Create an Event bridged to a kernel KReadableEvent.
    ///
    /// When `signal()` is called, the kernel readable event is also signaled,
    /// waking any threads blocked in WaitSynchronization on this event's handle.
    pub fn new_with_kernel_event(
        event: Arc<Mutex<KEvent>>,
        readable_event: Arc<Mutex<KReadableEvent>>,
        process: Arc<ProcessLock>,
    ) -> Self {
        Self {
            signaled: Arc::new(Mutex::new(false)),
            cv: Arc::new(Condvar::new()),
            kernel_bridge: Mutex::new(Some(KernelEventBridge {
                event,
                readable_event,
                process: Arc::downgrade(&process),
            })),
            host_waiters: OnceLock::new(),
        }
    }

    /// Install a kernel bridge lazily.
    ///
    /// This keeps service owners faithful to upstream `Event` ownership while allowing
    /// the kernel readable-end to be materialized only when an IPC handle is requested.
    pub fn attach_kernel_event_owner(
        &self,
        event: Arc<Mutex<KEvent>>,
        readable_event: Arc<Mutex<KReadableEvent>>,
        process: Arc<ProcessLock>,
    ) {
        let mut bridge = self.kernel_bridge.lock().unwrap();
        if bridge.is_none() {
            *bridge = Some(KernelEventBridge {
                event,
                readable_event,
                process: Arc::downgrade(&process),
            });
        }
    }

    /// Compatibility bridge for existing service owners that already materialize a
    /// readable event themselves. The bridge still stores an owner `KEvent`, but
    /// that owner remains local to this wrapper instead of being process-registered.
    pub fn attach_kernel_event(
        &self,
        readable_event: Arc<Mutex<KReadableEvent>>,
        process: Arc<ProcessLock>,
    ) {
        let owner_process_id = process.lock().unwrap().get_process_id();
        let readable_event_id = readable_event.lock().unwrap().object_id;
        let mut event = KEvent::new();
        event.initialize(owner_process_id, readable_event_id);
        self.attach_kernel_event_owner(Arc::new(Mutex::new(event)), readable_event, process);
    }

    fn create_kernel_bridge_from_context(
        &self,
        ctx: &HLERequestContext,
    ) -> Option<Arc<Mutex<KReadableEvent>>> {
        if let Some(bridge) = self.kernel_bridge.lock().unwrap().as_ref() {
            return Some(Arc::clone(&bridge.readable_event));
        }

        let thread = ctx.get_thread()?;
        let thread_guard = thread.lock().unwrap();
        let process = thread_guard.parent.as_ref()?.upgrade()?;
        drop(thread_guard);

        let kernel = crate::hle::kernel::kernel::get_kernel_ref()?;

        let event_object_id = kernel.create_new_object_id() as u64;
        let readable_event_object_id = kernel.create_new_object_id() as u64;
        let owner_process_id = process.lock().unwrap().get_process_id();
        let signaled = self.is_signaled();

        let mut event = KEvent::new();
        let mut readable_event = KReadableEvent::new();
        event.initialize(owner_process_id, readable_event_object_id);
        readable_event.initialize(event_object_id, readable_event_object_id);
        if signaled {
            readable_event
                .is_signaled
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }

        let event = Arc::new(Mutex::new(event));
        let readable_event = Arc::new(Mutex::new(readable_event));

        {
            let mut process_guard = process.lock().unwrap();
            process_guard.register_event_object(event_object_id, Arc::clone(&event));
            process_guard.register_readable_event_object(
                readable_event_object_id,
                Arc::clone(&readable_event),
            );
        }

        self.attach_kernel_event_owner(
            Arc::clone(&event),
            Arc::clone(&readable_event),
            Arc::clone(&process),
        );

        Some(readable_event)
    }

    /// Return a copy handle to the readable end of this service event.
    ///
    /// This is the Rust counterpart of upstream `Event::GetHandle()`: the owner is
    /// the service-layer `Event`, while the IPC-facing readable end is materialized
    /// lazily as a real kernel event/readable-event pair when first requested.
    pub fn copy_handle(&self, ctx: &HLERequestContext) -> Option<Handle> {
        let readable_event = self.create_kernel_bridge_from_context(ctx)?;
        // logging the full backtrace at copy_handle correlates the
        // [EVENT_REG] log with the calling service (e.g. `vi::
        // application_display_service::get_display_vsync_event`). Off by
        // default; gated to keep release builds quiet.
        if std::env::var_os("RUZU_TRACE_EVENT_HANDOUT").is_some() {
            let object_id = readable_event.lock().unwrap().object_id;
            log::info!(
                "[EVENT_HANDOUT] readable_event object_id={} bt:\n{}",
                object_id,
                std::backtrace::Backtrace::force_capture()
            );
        }
        ctx.copy_handle_for_readable_event(readable_event)
    }

    /// Return the readable event object id without pre-allocating a process
    /// handle. Upstream `Event::GetHandle()` returns `KReadableEvent*`; IPC
    /// write-back translates that object into a copy handle later.
    pub fn copy_object_id(&self, ctx: &HLERequestContext) -> Option<u64> {
        let readable_event = self.create_kernel_bridge_from_context(ctx)?;
        if std::env::var_os("RUZU_TRACE_EVENT_HANDOUT").is_some() {
            let object_id = readable_event.lock().unwrap().object_id;
            log::info!(
                "[EVENT_HANDOUT] readable_event object_id={} deferred_handle bt:\n{}",
                object_id,
                std::backtrace::Backtrace::force_capture()
            );
        }
        ctx.register_readable_event_object(readable_event)
    }

    pub fn kernel_object_id(&self) -> Option<u64> {
        self.kernel_bridge
            .lock()
            .unwrap()
            .as_ref()
            .map(|bridge| bridge.readable_event.lock().unwrap().object_id)
    }

    pub fn readable_event(&self) -> Option<Arc<Mutex<KReadableEvent>>> {
        self.kernel_bridge
            .lock()
            .unwrap()
            .as_ref()
            .map(|bridge| Arc::clone(&bridge.readable_event))
    }

    /// Signal the event. Wakes all waiters.
    /// Port of upstream `Event::Signal()` → `m_event->Signal()`.
    pub fn signal(&self) {
        self.signal_host_only();

        // Bridge to kernel: signal the KEvent owner, which wakes the readable end.
        let bridge = self.kernel_bridge.lock().unwrap().clone();
        if let Some(bridge) = bridge {
            let readable_object_id = bridge.readable_event.lock().unwrap().object_id;
            let trace_boot = std::env::var_os("RUZU_APPLET_BOOT_TRACE")
                .is_some_and(|value| value != std::ffi::OsStr::new("0"));
            if trace_boot {
                log::info!(
                    "Service::Event::signal: bridging readable_object_id={} begin",
                    readable_object_id
                );
            }
            log::trace!(
                "Service::Event::signal bridging readable_object_id={}",
                readable_object_id
            );
            if let Some(process) = bridge.process.upgrade() {
                KEvent::signal_arc(&bridge.event, &process);
            }
            if trace_boot {
                log::info!(
                    "Service::Event::signal: bridge signal complete readable_object_id={}",
                    readable_object_id
                );
            }
        }
    }

    /// Wake only host-side waiters on this service event.
    ///
    /// This is used by internal `ServerManager` wakeup paths that only need to
    /// poke the host service thread's condvar. Calling full `signal()` there can
    /// re-enter the owning `KProcess` while the caller already holds its mutex.
    pub fn signal_host_only(&self) {
        let trace_boot = std::env::var_os("RUZU_APPLET_BOOT_TRACE")
            .is_some_and(|value| value != std::ffi::OsStr::new("0"));
        {
            let mut signaled = self.signaled.lock().unwrap();
            *signaled = true;
            self.cv.notify_all();
        }
        if let Some(waiters) = self.host_waiters.get() {
            waiters.lock().unwrap().retain(|waiter| {
                if let Some(waiter) = waiter.upgrade() {
                    waiter.signal();
                    true
                } else {
                    false
                }
            });
        }
        if trace_boot {
            log::info!("Service::Event::signal_host_only: host condvar signaled");
        }
    }

    /// Clear the event (reset signaled state).
    /// Port of upstream `Event::Clear()` → `m_event->Clear()`.
    pub fn clear(&self) {
        {
            let mut signaled = self.signaled.lock().unwrap();
            *signaled = false;
        }

        // Bridge to kernel: clear the readable end too, matching upstream
        // Event::Clear() -> KEvent::Clear().
        let bridge = self.kernel_bridge.lock().unwrap().clone();
        if let Some(bridge) = bridge {
            if let Some(process) = bridge.process.upgrade() {
                KEvent::clear_arc(&bridge.event, &process);
            }
        }
    }

    /// Check if the event is currently signaled.
    pub fn is_signaled(&self) -> bool {
        *self.signaled.lock().unwrap()
    }

    pub(super) fn register_host_waiter(&self, waiter: &Arc<HostMultiWaitSignal>) {
        let mut waiters = self.host_waiters.get_or_init(Mutex::default).lock().unwrap();
        waiters.retain(|waiter| waiter.strong_count() != 0);
        waiters.push(Arc::downgrade(waiter));
    }

    /// Wait for the event to be signaled.
    pub fn wait(&self) {
        let guard = self.signaled.lock().unwrap();
        let _guard = self.cv.wait_while(guard, |s| !*s).unwrap();
    }

    /// Wait for the event with a timeout. Returns true if signaled, false if timed out.
    pub fn wait_timeout(&self, timeout: std::time::Duration) -> bool {
        let guard = self.signaled.lock().unwrap();
        let (guard, _result) = self.cv.wait_timeout_while(guard, timeout, |s| !*s).unwrap();
        *guard
    }
}

impl Default for Event {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::kernel::k_process::KProcess;

    #[test]
    fn host_wait_registration_does_not_retain_finished_waiters() {
        let event = Event::new();
        for _ in 0..8 {
            let notification = Arc::new(HostMultiWaitSignal::default());
            event.register_host_waiter(&notification);
            assert_eq!(event.host_waiters.get().unwrap().lock().unwrap().len(), 1);
            let weak = Arc::downgrade(&notification);
            drop(notification);
            assert!(weak.upgrade().is_none());
        }
        event.signal_host_only();
        assert!(event.host_waiters.get().unwrap().lock().unwrap().is_empty());
    }

    #[test]
    fn clear_bridges_to_kernel_readable_event() {
        let mut process = KProcess::new();
        let readable = Arc::new(Mutex::new(KReadableEvent::new()));
        readable.lock().unwrap().initialize(1, 2);
        process.register_readable_event_object(2, Arc::clone(&readable));

        let process = Arc::new(ProcessLock::from_value(process));
        let event_owner = Arc::new(Mutex::new(crate::hle::kernel::k_event::KEvent::new()));
        event_owner.lock().unwrap().initialize(1, 2);
        let event = Event::new_with_kernel_event(
            Arc::clone(&event_owner),
            Arc::clone(&readable),
            Arc::clone(&process),
        );

        event.signal();
        assert!(event.is_signaled());
        assert!(readable.lock().unwrap().is_signaled());

        event.clear();
        assert!(!event.is_signaled());
        assert!(!readable.lock().unwrap().is_signaled());
    }

    #[test]
    fn kernel_bridge_does_not_own_process() {
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let weak_process = Arc::downgrade(&process);
        let readable = Arc::new(Mutex::new(KReadableEvent::new()));
        readable.lock().unwrap().initialize(1, 2);
        let event_owner = Arc::new(Mutex::new(KEvent::new()));
        event_owner.lock().unwrap().initialize(1, 2);
        let event = Event::new_with_kernel_event(event_owner, readable, Arc::clone(&process));

        drop(process);

        assert!(
            weak_process.upgrade().is_none(),
            "service events must not extend the owning process lifetime"
        );
        event.signal();
        assert!(event.is_signaled());
        event.clear();
        assert!(!event.is_signaled());
    }
}
