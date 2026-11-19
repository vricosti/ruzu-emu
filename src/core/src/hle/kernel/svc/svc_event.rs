//! Port of zuyu/src/core/hle/kernel/svc/svc_event.cpp
//! Status: Partial (kernel object access implemented)
//! Derniere synchro: 2026-03-14
//!
//! SVC handlers for event operations (SignalEvent, ClearEvent, CreateEvent).

use std::sync::{Arc, Mutex};

use crate::core::System;
use crate::hle::kernel::k_event::KEvent;
use crate::hle::kernel::k_readable_event::KReadableEvent;
use crate::hle::kernel::svc::svc_results::*;
use crate::hle::kernel::svc_common::Handle;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};

/// Signals an event.
pub fn signal_event(system: &System, event_handle: Handle) -> ResultCode {
    log::debug!(
        "svc::SignalEvent called, event_handle=0x{:08X}",
        event_handle
    );

    let process_arc = system.current_process_arc();
    let process = process_arc.lock().unwrap();
    let event = process.handle_table.get_object(event_handle)
        .and_then(|object_id| process.get_event_by_object_id(object_id));
    // Upstream SignalEvent treats unknown system-applet handles as best effort.
    if process.get_program_id() & 0xFFFF_FFFF_FFFF_FF00 == 0x0100_0000_0000_1000 {
        drop(process);
        if let Some(event) = event {
            KEvent::signal_arc(&event, &process_arc);
        } else {
            log::warn!("SignalEvent best-effort unknown handle={event_handle:#010x} (ignored)");
        }
        return RESULT_SUCCESS;
    }
    let Some(event) = event else {
        return RESULT_INVALID_HANDLE;
    };
    drop(process);
    ResultCode::new(KEvent::signal_arc(&event, &process_arc))
}

/// Clears an event. Tries writable event first, then readable event.
pub fn clear_event(system: &System, event_handle: Handle) -> ResultCode {
    log::trace!(
        "svc::ClearEvent called, event_handle=0x{:08X}",
        event_handle
    );

    let process_arc = system.current_process_arc();
    let process = process_arc.lock().unwrap();
    let Some(object_id) = process.handle_table.get_object(event_handle) else {
        return RESULT_INVALID_HANDLE;
    };

    if let Some(event) = process.get_event_by_object_id(object_id) {
        drop(process);
        return ResultCode::new(KEvent::clear_arc(&event, &process_arc));
    }
    if let Some(readable_event) = process.get_readable_event_by_object_id(object_id) {
        drop(process);
        let mut readable_event = readable_event.lock().unwrap();
        // Release the wrapper before the scheduler can switch guest fibers.
        let _scheduler_guard = crate::hle::kernel::kernel::scheduler_lock()
            .map(crate::hle::kernel::k_scheduler_lock::KScopedSchedulerLock::new);
        let result = readable_event.clear();
        drop(readable_event);
        return ResultCode::new(result);
    }

    RESULT_INVALID_HANDLE
}

/// Creates an event, returning write and read handles.
pub fn create_event(system: &System, out_write: &mut Handle, out_read: &mut Handle) -> ResultCode {
    log::debug!("svc::CreateEvent called");

    let event_object_id = system.kernel().unwrap().create_new_object_id() as u64;
    let readable_event_object_id = system.kernel().unwrap().create_new_object_id() as u64;

    let event = Arc::new(Mutex::new(KEvent::new()));
    let readable_event = Arc::new(Mutex::new(KReadableEvent::new()));

    {
        let mut event_guard = event.lock().unwrap();
        let mut readable_event_guard = readable_event.lock().unwrap();
        let process_arc = system.current_process_arc();
        let process = process_arc.lock().unwrap();

        readable_event_guard.initialize(event_object_id, readable_event_object_id);
        event_guard.initialize(process.process_id, readable_event_object_id);
    }

    let process_arc = system.current_process_arc();
    let mut process = process_arc.lock().unwrap();
    if process.ensure_handle_table_initialized() != RESULT_SUCCESS.get_inner_value() {
        return RESULT_OUT_OF_RESOURCE;
    }

    process.register_event_object(event_object_id, event);
    process.register_readable_event_object(readable_event_object_id, readable_event);

    let write_handle = match process.handle_table.add(event_object_id) {
        Ok(handle) => handle,
        Err(_) => {
            process.unregister_event_object_by_object_id(event_object_id);
            process.unregister_readable_event_object_by_object_id(readable_event_object_id);
            return RESULT_OUT_OF_HANDLES;
        }
    };
    let read_handle = match process.handle_table.add(readable_event_object_id) {
        Ok(handle) => handle,
        Err(_) => {
            process.handle_table.remove(write_handle);
            process.unregister_event_object_by_object_id(event_object_id);
            process.unregister_readable_event_object_by_object_id(readable_event_object_id);
            return RESULT_OUT_OF_HANDLES;
        }
    };

    *out_write = write_handle;
    *out_read = read_handle;
    RESULT_SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::System;
    use crate::hle::kernel::k_process::{KProcess, ProcessLock};
    use crate::hle::kernel::k_thread::{KThread, KThreadLock};

    fn test_system() -> System {
        let mut system = System::new_for_test();

        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        {
            let mut process_guard = process.lock().unwrap();
            process_guard.process_id = 1;
            process_guard.initialize_handle_table();
        }

        let current_thread = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut thread = current_thread.lock().unwrap();
            thread.thread_id = 1;
            thread.object_id = 1;
        }
        process
            .lock()
            .unwrap()
            .register_thread_object(current_thread);

        let scheduler = Arc::new(Mutex::new(
            crate::hle::kernel::k_scheduler::KScheduler::new(0),
        ));
        scheduler.lock().unwrap().initialize(1, 0, 0);
        let shared_memory = process.lock().unwrap().get_shared_memory();

        system.set_current_process_arc(process);
        system.set_scheduler_arc(scheduler);
        system.set_shared_process_memory(shared_memory);
        system.set_runtime_program_id(1);
        system.set_runtime_64bit(false);
        system
    }

    #[test]
    fn create_signal_and_clear_event_round_trip() {
        let system = test_system();
        let mut write_handle = 0;
        let mut read_handle = 0;

        assert_eq!(
            create_event(&system, &mut write_handle, &mut read_handle),
            RESULT_SUCCESS
        );
        assert_ne!(write_handle, 0);
        assert_ne!(read_handle, 0);

        assert_eq!(signal_event(&system, write_handle), RESULT_SUCCESS);

        let process_arc = system.current_process_arc();
        let process = process_arc.lock().unwrap();
        let readable_object_id = process.handle_table.get_object(read_handle).unwrap();
        let readable = process
            .get_readable_event_by_object_id(readable_object_id)
            .unwrap();
        assert!(readable.lock().unwrap().is_signaled());
        drop(process);

        assert_eq!(clear_event(&system, read_handle), RESULT_SUCCESS);
        let process_arc = system.current_process_arc();
        let process = process_arc.lock().unwrap();
        let readable_object_id = process.handle_table.get_object(read_handle).unwrap();
        let readable = process
            .get_readable_event_by_object_id(readable_object_id)
            .unwrap();
        assert!(!readable.lock().unwrap().is_signaled());
    }
}
