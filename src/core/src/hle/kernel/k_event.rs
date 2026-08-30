//! Port of zuyu/src/core/hle/kernel/k_event.h / k_event.cpp
//! Status: Partial (owner/process wiring ported)
//! Derniere synchro: 2026-03-14
//!
//! KEvent: a kernel event object containing a readable event.

use std::sync::{Arc, Mutex};

use super::k_process::KProcess;
use super::k_process::ProcessLock;
use super::k_scheduler_lock::KScopedSchedulerLock;
use crate::hle::result::RESULT_SUCCESS;

/// The kernel event object.
/// Matches upstream `KEvent` class (k_event.h).
pub struct KEvent {
    /// Embedded readable event object id registered in the owner process.
    pub readable_event_id: u64,
    /// Owner process id.
    pub owner_process_id: Option<u64>,
    pub initialized: bool,
    pub readable_event_destroyed: bool,
}

impl KEvent {
    /// Acquire the kernel scheduler lock used by upstream `KEvent::Signal`
    /// and `Clear`. Resolves through the kernel singleton; no GSC mutex
    /// round-trip is needed.
    fn lock_scheduler() -> Option<KScopedSchedulerLock<'static>> {
        let scheduler_lock = super::kernel::scheduler_lock()?;
        Some(KScopedSchedulerLock::new(scheduler_lock))
    }

    pub fn new() -> Self {
        Self {
            readable_event_id: 0,
            owner_process_id: None,
            initialized: false,
            readable_event_destroyed: false,
        }
    }

    /// Initialize the event with an owner process.
    pub fn initialize(&mut self, owner_process_id: u64, readable_event_id: u64) {
        self.owner_process_id = Some(owner_process_id);
        self.readable_event_id = readable_event_id;
        self.initialized = true;
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    pub fn get_owner_process_id(&self) -> Option<u64> {
        self.owner_process_id
    }

    /// Signal the event.
    /// Matches upstream `KEvent::Signal`.
    pub fn signal(&mut self, process: &KProcess) -> u32 {
        if self.readable_event_destroyed {
            return RESULT_SUCCESS.get_inner_value();
        }

        let Some(readable_event) = process.get_readable_event_by_object_id(self.readable_event_id)
        else {
            return RESULT_SUCCESS.get_inner_value();
        };

        // Eden embeds KReadableEvent directly in KEvent, so taking the
        // scheduler lock at function entry cannot block on an intermediate
        // object lock. Ruzu stores it behind a Mutex. Lock that Rust-owned
        // wrapper first, otherwise this path (scheduler -> readable event)
        // deadlocks with direct KReadableEvent::signal callers (readable event
        // -> scheduler). The scheduler guard still covers the same upstream
        // state checks and delegation to KReadableEvent::Signal.
        let mut readable_event = readable_event.lock().unwrap();
        let _scheduler_guard = Self::lock_scheduler();
        let result = readable_event.signal();
        // Do not let scheduler-unlock rescheduling suspend this fiber while it
        // still owns the Rust wrapper mutex.
        drop(readable_event);
        result
    }

    /// Clear the event.
    /// Matches upstream `KEvent::Clear`.
    pub fn clear(&mut self, process: &KProcess) -> u32 {
        if self.readable_event_destroyed {
            return RESULT_SUCCESS.get_inner_value();
        }

        let Some(readable_event) = process.get_readable_event_by_object_id(self.readable_event_id)
        else {
            return RESULT_SUCCESS.get_inner_value();
        };

        let mut readable_event = readable_event.lock().unwrap();
        let _scheduler_guard = Self::lock_scheduler();
        let result = readable_event.clear();
        drop(readable_event);
        result
    }

    /// Rust helper for signaling a shared `KEvent` through the current owner process.
    ///
    /// Upstream stores the readable event inline and signals it directly. Rust stores
    /// the owner `KEvent` and readable event in per-process object maps, so this
    /// helper resolves the readable end from the owner process and signals it.
    /// The process mutex is used only to resolve the readable end and is
    /// released before the scheduler-protected signal operation.
    pub fn signal_arc(event: &Arc<Mutex<KEvent>>, process: &Arc<ProcessLock>) -> u32 {
        let readable_event_id = {
            let event = event.lock().unwrap();
            if event.readable_event_destroyed {
                return RESULT_SUCCESS.get_inner_value();
            }
            event.readable_event_id
        };

        let readable_event = {
            let process = process.lock().unwrap();
            let Some(readable_event) = process.get_readable_event_by_object_id(readable_event_id)
            else {
                return RESULT_SUCCESS.get_inner_value();
            };
            readable_event
        };

        // See `signal`: resolve and lock the Rust-owned wrappers before the
        // scheduler guard. Eden's embedded objects do not introduce these
        // blocking mutex acquisitions.
        let mut readable_event = readable_event.lock().unwrap();
        let _scheduler_guard = Self::lock_scheduler();
        let result = readable_event.signal_from_host();
        drop(readable_event);
        result
    }

    /// Rust helper for clearing a shared `KEvent` through the current owner process.
    pub fn clear_arc(event: &Arc<Mutex<KEvent>>, process: &Arc<ProcessLock>) -> u32 {
        let readable_event_id = {
            let event = event.lock().unwrap();
            if event.readable_event_destroyed {
                return RESULT_SUCCESS.get_inner_value();
            }
            event.readable_event_id
        };

        let process = process.lock().unwrap();
        let Some(readable_event) = process.get_readable_event_by_object_id(readable_event_id)
        else {
            return RESULT_SUCCESS.get_inner_value();
        };

        let mut readable_event = readable_event.lock().unwrap();
        let _scheduler_guard = Self::lock_scheduler();
        let result = readable_event.clear();
        drop(readable_event);
        result
    }

    /// Called when the readable event is destroyed.
    pub fn on_readable_event_destroyed(&mut self) {
        self.readable_event_destroyed = true;
    }

    /// Finalize the event.
    pub fn finalize(&mut self) {
        self.initialized = false;
    }
}

impl Default for KEvent {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::kernel::global_scheduler_context::GlobalSchedulerContext;

    #[test]
    fn signal_clear_signal_roundtrip_uses_same_readable_event() {
        let mut process = KProcess::new();
        process.global_scheduler_context =
            Some(Arc::new(Mutex::new(GlobalSchedulerContext::new())));

        let readable_id = 123;
        let event_id = 456;

        let mut event = KEvent::new();
        event.initialize(1, readable_id);

        let readable = Arc::new(Mutex::new(
            super::super::k_readable_event::KReadableEvent::new(),
        ));
        readable.lock().unwrap().initialize(event_id, readable_id);
        process.register_readable_event_object(readable_id, Arc::clone(&readable));

        assert_eq!(event.signal(&process), RESULT_SUCCESS.get_inner_value());
        assert!(readable.lock().unwrap().is_signaled());

        assert_eq!(event.clear(&process), RESULT_SUCCESS.get_inner_value());
        assert!(!readable.lock().unwrap().is_signaled());

        assert_eq!(event.signal(&process), RESULT_SUCCESS.get_inner_value());
        assert!(readable.lock().unwrap().is_signaled());
    }

    #[test]
    fn shared_event_signal_clear_roundtrip_uses_same_readable_event() {
        let process = Arc::new(ProcessLock::new(KProcess::new()));
        let readable_id = 123;
        let event_id = 456;

        let event = Arc::new(Mutex::new(KEvent::new()));
        event.lock().unwrap().initialize(1, readable_id);

        let readable = Arc::new(Mutex::new(
            super::super::k_readable_event::KReadableEvent::new(),
        ));
        readable.lock().unwrap().initialize(event_id, readable_id);
        process
            .lock()
            .unwrap()
            .register_readable_event_object(readable_id, Arc::clone(&readable));

        assert_eq!(
            KEvent::signal_arc(&event, &process),
            RESULT_SUCCESS.get_inner_value()
        );
        assert!(readable.lock().unwrap().is_signaled());

        assert_eq!(
            KEvent::clear_arc(&event, &process),
            RESULT_SUCCESS.get_inner_value()
        );
        assert!(!readable.lock().unwrap().is_signaled());
    }
}
