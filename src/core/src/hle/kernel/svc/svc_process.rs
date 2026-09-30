//! Port of zuyu/src/core/hle/kernel/svc/svc_process.cpp
//! Status: Ported
//! Derniere synchro: 2026-03-20
//!
//! SVC handlers for process operations (ExitProcess, GetProcessId, GetProcessList, etc.).

use crate::core::System;
use crate::hle::kernel::svc::svc_results::*;
use crate::hle::kernel::svc::svc_types::*;
use crate::hle::kernel::svc_common::{Handle, PseudoHandle};
use crate::hle::result::{ResultCode, RESULT_SUCCESS};

/// Exits the current process.
pub fn exit_process(system: &System) {
    let current_process = system.current_process_arc();
    let process = current_process.lock().unwrap();
    let process_id = process.get_process_id();
    log::info!("svc::ExitProcess called for process {}", process_id);
    assert_eq!(
        process.get_state(),
        crate::hle::kernel::k_process::ProcessState::Running,
        "process has already exited"
    );
    drop(process);

    system.exit();
}

/// Gets the ID of the specified process or a specified thread's owning process.
pub fn get_process_id(system: &System, out_process_id: &mut u64, handle: Handle) -> ResultCode {
    log::debug!("svc::GetProcessId called handle=0x{:08X}", handle);

    // Handle pseudo-handle for current process.
    if handle == PseudoHandle::CurrentProcess as Handle {
        let process_arc = system.current_process_arc();
        let process = process_arc.lock().unwrap();
        *out_process_id = process.get_process_id();
        return RESULT_SUCCESS;
    }

    // Get the object from the handle table.
    let process_arc = system.current_process_arc();
    let process = process_arc.lock().unwrap();
    let object_id = match process.handle_table.get_object(handle) {
        Some(id) => id,
        None => {
            return RESULT_INVALID_HANDLE;
        }
    };

    // Check if it's a thread (look up in thread_objects).
    if let Some(thread) = process.get_thread_by_object_id(object_id) {
        let thread_guard = thread.lock().unwrap();
        if let Some(parent) = thread_guard.parent.as_ref().and_then(|w| w.upgrade()) {
            let parent_process = parent.lock().unwrap();
            *out_process_id = parent_process.get_process_id();
            return RESULT_SUCCESS;
        }
    }

    // Otherwise treat it as a process handle. In single-process mode, the
    // only process is the current one.
    *out_process_id = process.get_process_id();
    RESULT_SUCCESS
}

/// Gets the list of process IDs.
pub fn get_process_list(
    system: &System,
    out_num_processes: &mut i32,
    out_process_ids: u64,
    out_process_ids_size: i32,
) -> ResultCode {
    log::debug!(
        "svc::GetProcessList called. out_process_ids_size={}",
        out_process_ids_size
    );

    // If the supplied size is negative or greater than INT32_MAX / sizeof(u64), bail.
    if (out_process_ids_size as u32 & 0xF000_0000) != 0 {
        log::error!(
            "Supplied size outside [0, 0x0FFFFFFF] range. out_process_ids_size={}",
            out_process_ids_size
        );
        return RESULT_OUT_OF_RANGE;
    }

    // In our single-process model, we have one process.
    let process_arc = system.current_process_arc();
    let process = process_arc.lock().unwrap();
    let process_id = process.get_process_id();
    let num_processes: i32 = 1;

    if out_process_ids_size > 0 {
        // Validate that the output buffer is in range.
        let total_size = (out_process_ids_size as u64) * 8;
        let addr = crate::hle::kernel::k_typed_address::KProcessAddress::new(out_process_ids);
        if !process.page_table.contains(addr, total_size as usize) {
            log::error!(
                "Address range outside address space. begin=0x{:016X}, end=0x{:016X}",
                out_process_ids,
                out_process_ids + total_size
            );
            return RESULT_INVALID_CURRENT_MEMORY;
        }

        // Write the process ID to guest memory.
        drop(process);
        if let Some(memory) = system.get_svc_memory() {
            let m = memory.access().unwrap();
            m.write_64(out_process_ids, process_id);
        } else {
            let mut mem = system.shared_process_memory().write().unwrap();
            mem.write_64(out_process_ids, process_id);
        }
    }

    *out_num_processes = num_processes;
    RESULT_SUCCESS
}

/// Gets information about a process.
pub fn get_process_info(
    system: &System,
    out: &mut i64,
    process_handle: Handle,
    info_type: ProcessInfoType,
) -> ResultCode {
    log::debug!(
        "svc::GetProcessInfo called, handle=0x{:08X}, type={:?}",
        process_handle,
        info_type
    );

    // Get the process from the handle.
    // For pseudo-handle or handle 0 -> current process.
    let process_arc =
        if process_handle == PseudoHandle::CurrentProcess as Handle || process_handle == 0 {
            system.current_process_arc()
        } else {
            let current = system.current_process_arc();
            let guard = current.lock().unwrap();
            match guard.handle_table.get_object(process_handle) {
                Some(_) => {
                    drop(guard);
                    system.current_process_arc()
                }
                None => {
                    log::error!(
                        "Process handle does not exist, process_handle=0x{:08X}",
                        process_handle
                    );
                    return RESULT_INVALID_HANDLE;
                }
            }
        };

    if !matches!(info_type, ProcessInfoType::ProcessState) {
        log::error!(
            "Expected info_type to be ProcessState but got {:?}",
            info_type
        );
        return RESULT_INVALID_ENUM_VALUE;
    }

    let process = process_arc.lock().unwrap();
    *out = process.get_state() as i64;
    RESULT_SUCCESS
}

/// Creates a process. (Upstream: UNIMPLEMENTED)
pub fn create_process(
    _out_handle: &mut Handle,
    _parameters: u64,
    _caps: u64,
    _num_caps: i32,
) -> ResultCode {
    log::warn!("svc::CreateProcess: Upstream UNIMPLEMENTED");
    RESULT_NOT_IMPLEMENTED
}

/// Starts a process. (Upstream: UNIMPLEMENTED)
pub fn start_process(
    _process_handle: Handle,
    _priority: i32,
    _core_id: i32,
    _main_thread_stack_size: u64,
) -> ResultCode {
    log::warn!("svc::StartProcess: Upstream UNIMPLEMENTED");
    RESULT_NOT_IMPLEMENTED
}

/// Terminates a process. (Upstream: UNIMPLEMENTED)
pub fn terminate_process(_process_handle: Handle) -> ResultCode {
    log::warn!("svc::TerminateProcess: Upstream UNIMPLEMENTED");
    RESULT_NOT_IMPLEMENTED
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::System;
    use crate::hle::kernel::k_process::{KProcess, ProcessLock, ProcessState};
    use crate::hle::kernel::k_scheduler::KScheduler;
    use crate::hle::kernel::k_thread::{KThread, KThreadLock, ThreadState};
    use std::sync::{Arc, Mutex};

    fn test_system() -> System {
        let mut system = System::new_for_test();

        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let scheduler = Arc::new(Mutex::new(KScheduler::new(0)));
        scheduler.lock().unwrap().set_scheduler_current_thread_id(1);

        let current_thread = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut thread = current_thread.lock().unwrap();
            thread.thread_id = 1;
            thread.object_id = 1;
            thread.parent = Some(Arc::downgrade(&process));
            thread.scheduler = Some(Arc::downgrade(&scheduler));
            thread.set_state(ThreadState::RUNNABLE);
        }

        {
            let mut process_guard = process.lock().unwrap();
            process_guard.bind_self_reference(&process);
            process_guard.attach_scheduler(&scheduler);
            process_guard.state = ProcessState::Running;
            process_guard.increment_running_thread_count();
            process_guard.register_thread_object(current_thread);
        }

        let shared_memory = process.lock().unwrap().get_shared_memory();
        system.set_current_process_arc(process);
        system.set_scheduler_arc(scheduler);
        system.set_shared_process_memory(shared_memory);
        system.set_runtime_program_id(1);
        system.set_runtime_64bit(false);
        system
    }

    #[test]
    fn exit_process_notifies_the_frontend() {
        let mut system = test_system();
        let exit_called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let callback_exit_called = Arc::clone(&exit_called);
        system.register_exit_callback(Box::new(move || {
            callback_exit_called.store(true, std::sync::atomic::Ordering::Release);
        }));

        exit_process(&system);

        let process_arc = system.current_process_arc();
        let process = process_arc.lock().unwrap();
        let current_thread = process.get_thread_by_thread_id(1).unwrap();
        assert_eq!(process.state, ProcessState::Running);
        drop(process);

        let thread = current_thread.lock().unwrap();
        assert!(!thread.is_termination_requested());
        assert!(exit_called.load(std::sync::atomic::Ordering::Acquire));
    }
}
