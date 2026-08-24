//! Port of zuyu/src/core/hle/kernel/svc/svc_thread.cpp
//! Status: Partial (kernel object-backed thread create/start/query path)
//! Derniere synchro: 2026-03-14
//!
//! SVC handlers for thread operations.

use std::sync::Arc;

use crate::core::System;
use crate::hle::kernel::k_light_lock::KScopedLightLock;
use crate::hle::kernel::k_resource_limit::LimitableResource;
use crate::hle::kernel::k_scoped_resource_reservation::KScopedResourceReservation;
use crate::hle::kernel::k_thread::{KThread, KThreadLock};
use crate::hle::kernel::svc::svc_results::*;
use crate::hle::kernel::svc::svc_types::*;
use crate::hle::kernel::svc_common::{Handle, PseudoHandle, INVALID_HANDLE};
use crate::hle::result::{ResultCode, RESULT_SUCCESS};

fn is_valid_virtual_core_id(core_id: i32) -> bool {
    (0..4).contains(&core_id)
}

fn sleep_timeout_tick_from_ns(current_tick: i64, ns: i64) -> i64 {
    debug_assert!(ns > 0);

    let offset_tick = ns;
    if offset_tick > 0 {
        let timeout = current_tick.saturating_add(offset_tick).saturating_add(2);
        if timeout <= 0 {
            i64::MAX
        } else {
            timeout
        }
    } else {
        i64::MAX
    }
}

fn resolve_thread_handle(system: &System, handle: Handle) -> Option<Arc<KThreadLock>> {
    if handle == PseudoHandle::CurrentThread as Handle {
        return system.current_thread();
    }

    let process_arc = system.current_process_arc();
    let process = process_arc.lock().unwrap();
    let object_id = process.handle_table.get_object(handle)?;
    process.get_thread_by_object_id(object_id)
}

fn resolve_current_thread(system: &System) -> Option<Arc<KThreadLock>> {
    system.current_thread()
}

fn trace_thread_core_mask(
    stage: u64,
    caller_tid: u64,
    target: Option<&Arc<KThreadLock>>,
    handle: Handle,
    core_id: i32,
    affinity_mask: u64,
    result: ResultCode,
) {
    if !common::trace::is_enabled(common::trace::cat::THREAD_CORE_MASK) {
        return;
    }
    let (target_tid, state, active_core, current_core, pinned, waiter_count) =
        if let Some(thread) = target {
            let thread = thread.lock().unwrap();
            (
                thread.get_thread_id(),
                thread.get_raw_state().bits() as u64,
                thread.get_active_core() as u64,
                thread.get_current_core() as u64,
                thread.stack_parameters.is_pinned as u64,
                thread.pinned_waiter_list.len() as u64,
            )
        } else {
            (0, 0, u64::MAX, u64::MAX, 0, 0)
        };
    common::trace::emit_raw(
        common::trace::cat::THREAD_CORE_MASK,
        &[
            stage,
            caller_tid,
            target_tid,
            handle as u64,
            core_id as u32 as u64,
            affinity_mask,
            state,
            active_core,
            current_core,
            result.get_inner_value() as u64,
            pinned,
            waiter_count,
        ],
    );
}

#[allow(clippy::too_many_arguments)]
fn trace_create_thread(
    stage: u64,
    caller_tid: u64,
    entry_point: u64,
    arg: u64,
    stack_bottom: u64,
    priority: i32,
    requested_core_id: i32,
    effective_core_id: i32,
    process_core_mask: u64,
    result: ResultCode,
    out_handle: Handle,
    extra: u64,
) {
    if !common::trace::is_enabled(common::trace::cat::CREATE_THREAD) {
        return;
    }
    common::trace::emit_raw(
        common::trace::cat::CREATE_THREAD,
        &[
            stage,
            caller_tid,
            entry_point,
            arg,
            stack_bottom,
            priority as u32 as u64,
            requested_core_id as u32 as u64,
            effective_core_id as u32 as u64,
            process_core_mask,
            result.get_inner_value() as u64,
            out_handle as u64,
            extra,
        ],
    );
}

/// Creates a new thread.
pub fn create_thread(
    system: &System,
    out_handle: &mut Handle,
    entry_point: u64,
    arg: u64,
    stack_bottom: u64,
    priority: i32,
    mut core_id: i32,
) -> ResultCode {
    let requested_core_id = core_id;
    let caller_tid = system.current_thread_id().unwrap_or(0);
    trace_create_thread(
        1,
        caller_tid,
        entry_point,
        arg,
        stack_bottom,
        priority,
        requested_core_id,
        core_id,
        0,
        RESULT_SUCCESS,
        0,
        0,
    );
    log::debug!(
        "svc::CreateThread called entry=0x{:08X}, arg=0x{:08X}, stack=0x{:08X}, prio={}, core={}",
        entry_point,
        arg,
        stack_bottom,
        priority,
        core_id
    );
    let current_process = system.current_process_arc().clone();

    {
        let process = current_process.lock().unwrap();
        if core_id == IDEAL_CORE_USE_PROCESS_VALUE {
            core_id = process.get_ideal_core_id();
        }
        let process_core_mask = process.get_core_mask();

        if !is_valid_virtual_core_id(core_id) {
            trace_create_thread(
                2,
                caller_tid,
                entry_point,
                arg,
                stack_bottom,
                priority,
                requested_core_id,
                core_id,
                process_core_mask,
                RESULT_INVALID_CORE_ID,
                0,
                0,
            );
            return RESULT_INVALID_CORE_ID;
        }
        if ((1u64 << core_id) & process_core_mask) == 0 {
            trace_create_thread(
                2,
                caller_tid,
                entry_point,
                arg,
                stack_bottom,
                priority,
                requested_core_id,
                core_id,
                process_core_mask,
                RESULT_INVALID_CORE_ID,
                0,
                1,
            );
            return RESULT_INVALID_CORE_ID;
        }
        if !(HIGHEST_THREAD_PRIORITY..=LOWEST_THREAD_PRIORITY).contains(&priority) {
            trace_create_thread(
                3,
                caller_tid,
                entry_point,
                arg,
                stack_bottom,
                priority,
                requested_core_id,
                core_id,
                process_core_mask,
                RESULT_INVALID_PRIORITY,
                0,
                0,
            );
            return RESULT_INVALID_PRIORITY;
        }
        if !process.check_thread_priority(priority) {
            trace_create_thread(
                3,
                caller_tid,
                entry_point,
                arg,
                stack_bottom,
                priority,
                requested_core_id,
                core_id,
                process_core_mask,
                RESULT_INVALID_PRIORITY,
                0,
                1,
            );
            return RESULT_INVALID_PRIORITY;
        }

        log::trace!(
            "svc::CreateThread resolved entry=0x{:08X} prio={} requested_core={} effective_core={} process_ideal_core={}",
            entry_point,
            priority,
            requested_core_id,
            core_id,
            process.get_ideal_core_id(),
        );
    }

    let thread_reservation_timeout = crate::hle::kernel::kernel::get_current_hardware_tick()
        .unwrap_or(i64::MAX)
        .saturating_add(100_000_000);
    let mut thread_reservation = {
        let process = current_process.lock().unwrap();
        KScopedResourceReservation::new_with_timeout(
            process.resource_limit.clone(),
            LimitableResource::ThreadCountMax,
            1,
            thread_reservation_timeout,
        )
    };
    if !thread_reservation.succeeded() {
        trace_create_thread(
            4,
            caller_tid,
            entry_point,
            arg,
            stack_bottom,
            priority,
            requested_core_id,
            core_id,
            0,
            RESULT_LIMIT_REACHED,
            0,
            0,
        );
        return RESULT_LIMIT_REACHED;
    }

    let object_id = system.kernel().unwrap().create_new_object_id() as u64;
    let thread_id = system.kernel().unwrap().create_new_thread_id();
    let process_is_64bit = current_process.lock().unwrap().is_64bit();

    // Create the guest thread fiber entry — matches upstream
    // `system.GetCpuManager().GetGuestThreadFunc()`.
    let guest_thread_func: Option<Box<dyn FnOnce() + Send>> = {
        let kernel = system.kernel().expect("kernel must exist");
        let kp = kernel as *const crate::hle::kernel::kernel::KernelCore as usize;
        let is_multicore = kernel.is_multicore();
        Some(Box::new(move || {
            let kernel = unsafe { &*(kp as *const crate::hle::kernel::kernel::KernelCore) };
            if is_multicore {
                crate::cpu_manager::CpuManager::multi_core_run_guest_thread(kernel);
            } else {
                crate::cpu_manager::CpuManager::single_core_run_guest_thread_entry(kernel);
            }
        }))
    };

    let thread = Arc::new(KThreadLock::new(KThread::new()));
    {
        let process_state_lock = {
            let process = current_process.lock().unwrap();
            process.get_state_lock()
        };
        let _process_state_guard = KScopedLightLock::new(process_state_lock.as_ref());
        let mut new_thread = thread.lock().unwrap();
        let result = new_thread.initialize_user_thread_with_init_func(
            entry_point,
            arg,
            stack_bottom,
            priority,
            core_id,
            &current_process,
            thread_id,
            object_id,
            process_is_64bit,
            guest_thread_func,
        );
        if result != RESULT_SUCCESS.get_inner_value() {
            trace_create_thread(
                5,
                caller_tid,
                entry_point,
                arg,
                stack_bottom,
                priority,
                requested_core_id,
                core_id,
                0,
                ResultCode::new(result),
                0,
                thread_id,
            );
            return ResultCode::new(result);
        }
    }

    {
        let mut new_thread = thread.lock().unwrap();
        // Cache the owning process raw pointer for scheduler-lock-protected
        // paths (matches upstream's `KProcess*` access from KThread). The
        // `KProcess` is pinned by the Arc so the pointer stays valid for the
        // thread's lifetime.
        let parent_ptr = {
            let mut process_guard = current_process.lock().unwrap();
            (&mut *process_guard) as *mut crate::hle::kernel::k_process::KProcess as usize
        };
        new_thread.set_parent_raw_ptr(parent_ptr);

        let current_thread = system.current_thread().expect("current thread must exist");
        let current_thread = current_thread.lock().unwrap();
        new_thread.clone_fpu_status_from(&current_thread);
    }

    thread_reservation.commit();

    let thread_tls_address = {
        let thread = thread.lock().unwrap();
        thread.get_tls_address()
    };

    let mut process = current_process.lock().unwrap();
    let handle_table_result = process.ensure_handle_table_initialized();
    if handle_table_result != RESULT_SUCCESS.get_inner_value() {
        let _ = process.delete_thread_local_region(thread_tls_address);
        trace_create_thread(
            6,
            caller_tid,
            entry_point,
            arg,
            stack_bottom,
            priority,
            requested_core_id,
            core_id,
            process.get_core_mask(),
            ResultCode::new(handle_table_result),
            0,
            thread_id,
        );
        return ResultCode::new(handle_table_result);
    }

    // Register the thread with the GlobalSchedulerContext so the scheduler can
    // find it by thread_id during fiber dispatch.
    // Upstream: KThread::Register(kernel, thread) adds to kernel object list.
    if let Some(ref gsc) = process.global_scheduler_context {
        gsc.lock().unwrap().add_thread(Arc::clone(&thread));
    }

    process.register_thread_object(thread);
    match process.handle_table.add(object_id) {
        Ok(handle) => {
            *out_handle = handle;
            trace_create_thread(
                7,
                caller_tid,
                entry_point,
                arg,
                stack_bottom,
                priority,
                requested_core_id,
                core_id,
                process.get_core_mask(),
                RESULT_SUCCESS,
                handle,
                thread_id,
            );
            RESULT_SUCCESS
        }
        Err(_) => {
            process.unregister_thread_object_by_object_id(object_id);
            let _ = process.delete_thread_local_region(thread_tls_address);
            trace_create_thread(
                8,
                caller_tid,
                entry_point,
                arg,
                stack_bottom,
                priority,
                requested_core_id,
                core_id,
                process.get_core_mask(),
                RESULT_OUT_OF_HANDLES,
                0,
                thread_id,
            );
            RESULT_OUT_OF_HANDLES
        }
    }
}

/// Starts the thread for the provided handle.
pub fn start_thread(system: &System, thread_handle: Handle) -> ResultCode {
    log::debug!("svc::StartThread called thread=0x{:08X}", thread_handle);

    let Some(thread) = resolve_thread_handle(system, thread_handle) else {
        return RESULT_INVALID_HANDLE;
    };

    let result = KThread::run_thread(&thread);
    ResultCode::new(result)
}

/// Called when a thread exits.
pub fn exit_thread(system: &System) {
    let Some(thread) = resolve_current_thread(system) else {
        log::warn!("svc::ExitThread: current thread missing");
        return;
    };

    // Upstream `ExitThread` removes the current thread from
    // `GlobalSchedulerContext` before entering `KThread::Exit()`, so the global
    // scheduler cannot pick it again while termination is being finalized.
    let (thread_id, global_scheduler_context) = {
        let thread_guard = thread.lock().unwrap();
        (
            thread_guard.get_thread_id(),
            thread_guard
                .global_scheduler_context
                .as_ref()
                .and_then(std::sync::Weak::upgrade),
        )
    };
    if let Some(gsc) = global_scheduler_context {
        gsc.lock().unwrap().remove_thread(thread_id);
    }

    thread.lock().unwrap().exit();
    system.scheduler_arc().lock().unwrap().request_schedule();
}

/// Sleeps the current thread.
pub fn sleep_thread(system: &System, ns: i64) {
    log::trace!("svc::SleepThread called nanoseconds={}", ns);

    if ns > 0 {
        let current_tick = system
            .kernel()
            .and_then(|_| crate::hle::kernel::kernel::get_current_hardware_tick())
            .expect("svc::SleepThread requires a live kernel hardware timer");
        let timeout = sleep_timeout_tick_from_ns(current_tick, ns);
        let Some(result) = crate::hle::kernel::k_thread::sleep_current_thread(timeout) else {
            log::warn!("svc::SleepThread(sleep): current thread cache missing");
            return;
        };
        if result != RESULT_SUCCESS.get_inner_value() {
            log::warn!("svc::SleepThread(sleep) failed: {:#x}", result);
        }
        return;
    }

    let Some(current_thread_id) = system.current_thread_id() else {
        log::warn!("svc::SleepThread(yield): current thread missing");
        return;
    };
    let current_process = system.current_process_arc();
    let sched_arc = system.scheduler_arc();
    let scheduler_ptr = {
        let mut scheduler = sched_arc.lock().unwrap();
        &mut *scheduler as *mut crate::hle::kernel::k_scheduler::KScheduler
    };
    // Upstream calls the static KScheduler yield helpers directly; no host-side
    // scheduler mutex is held while KScopedSchedulerLock unlock callbacks run.
    unsafe {
        if ns == YieldType::WithoutCoreMigration as i64 {
            (*scheduler_ptr).yield_without_core_migration(&current_process, current_thread_id);
        } else if ns == YieldType::WithCoreMigration as i64 {
            (*scheduler_ptr).yield_with_core_migration(&current_process, current_thread_id);
        } else if ns == YieldType::ToAnyThread as i64 {
            (*scheduler_ptr).yield_to_any_thread(&current_process, current_thread_id);
        }
    }
}

/// Gets the thread context.
pub fn get_thread_context3(system: &System, out_context: u64, thread_handle: Handle) -> ResultCode {
    log::debug!(
        "svc::GetThreadContext3 called, out_context=0x{:08X}, thread_handle=0x{:X}",
        out_context,
        thread_handle
    );

    let Some(current_thread) = system.current_thread() else {
        return RESULT_INVALID_HANDLE;
    };
    let Some(thread) = resolve_thread_handle(system, thread_handle) else {
        return RESULT_INVALID_HANDLE;
    };

    if std::sync::Arc::ptr_eq(&thread, &current_thread) {
        return RESULT_BUSY;
    }

    let mut context = crate::hle::kernel::k_thread::ThreadContext::default();
    let result = thread.lock().unwrap().get_thread_context3(&mut context);
    let result = ResultCode::new(result);
    if result.is_error() {
        return result;
    }

    let context_bytes = unsafe {
        std::slice::from_raw_parts(
            (&context as *const crate::hle::kernel::k_thread::ThreadContext).cast::<u8>(),
            std::mem::size_of::<crate::hle::kernel::svc_types::ThreadContext>(),
        )
    };

    let Some(memory) = system.get_svc_memory() else {
        return RESULT_INVALID_POINTER;
    };
    if !memory
        .lock()
        .unwrap()
        .write_block(out_context, context_bytes)
    {
        return RESULT_INVALID_POINTER;
    }

    RESULT_SUCCESS
}

/// Gets the priority for the specified thread.
pub fn get_thread_priority(system: &System, out_priority: &mut i32, handle: Handle) -> ResultCode {
    let Some(thread) = resolve_thread_handle(system, handle) else {
        return RESULT_INVALID_HANDLE;
    };

    *out_priority = thread.lock().unwrap().get_priority();
    RESULT_SUCCESS
}

/// Sets the priority for the specified thread.
pub fn set_thread_priority(system: &System, thread_handle: Handle, priority: i32) -> ResultCode {
    if !(HIGHEST_THREAD_PRIORITY..=LOWEST_THREAD_PRIORITY).contains(&priority) {
        return RESULT_INVALID_PRIORITY;
    }

    let scheduler_lock = super::super::kernel::scheduler_lock()
        .expect("scheduler_lock must exist - kernel not initialized?");
    let _scheduler_guard =
        super::super::k_scheduler_lock::KScopedSchedulerLock::new(scheduler_lock);

    let process_arc = system.current_process_arc();
    let mut process = process_arc.lock().unwrap();
    if !process.check_thread_priority(priority) {
        return RESULT_INVALID_PRIORITY;
    }

    let thread_id = if thread_handle == PseudoHandle::CurrentThread as Handle {
        let Some(thread) = system.current_thread() else {
            return RESULT_INVALID_HANDLE;
        };
        thread.lock().unwrap().get_thread_id()
    } else {
        let Some(object_id) = process.handle_table.get_object(thread_handle) else {
            return RESULT_INVALID_HANDLE;
        };
        let Some(thread) = process.get_thread_by_object_id(object_id) else {
            return RESULT_INVALID_HANDLE;
        };
        thread.lock().unwrap().get_thread_id()
    };

    KThread::set_base_priority_with_process(&mut process, thread_id, priority);
    RESULT_SUCCESS
}

/// Gets the thread list.
pub fn get_thread_list(
    system: &System,
    out_num_threads: &mut i32,
    _out_thread_ids: u64,
    out_thread_ids_size: i32,
    debug_handle: Handle,
) -> ResultCode {
    if debug_handle != INVALID_HANDLE {
        log::warn!("svc::GetThreadList: debug handle not yet supported");
    }
    if (out_thread_ids_size as u32 & 0xF000_0000) != 0 {
        return RESULT_OUT_OF_RANGE;
    }

    *out_num_threads = system
        .current_process_arc()
        .lock()
        .unwrap()
        .thread_list
        .len() as i32;
    RESULT_NOT_IMPLEMENTED
}

/// Gets the thread core mask.
pub fn get_thread_core_mask(
    system: &System,
    out_core_id: &mut i32,
    out_affinity_mask: &mut u64,
    thread_handle: Handle,
) -> ResultCode {
    let Some(thread) = resolve_thread_handle(system, thread_handle) else {
        return RESULT_INVALID_HANDLE;
    };
    let (core_id, affinity_mask) = thread.lock().unwrap().get_core_mask();
    *out_core_id = core_id;
    *out_affinity_mask = affinity_mask;
    RESULT_SUCCESS
}

/// Sets the thread core mask.
pub fn set_thread_core_mask(
    system: &System,
    thread_handle: Handle,
    mut core_id: i32,
    mut affinity_mask: u64,
) -> ResultCode {
    let caller_tid = system.current_thread_id().unwrap_or(0);
    trace_thread_core_mask(
        1,
        caller_tid,
        None,
        thread_handle,
        core_id,
        affinity_mask,
        RESULT_SUCCESS,
    );
    let process_arc = system.current_process_arc();
    let process = process_arc.lock().unwrap();
    if core_id == IDEAL_CORE_USE_PROCESS_VALUE {
        core_id = process.get_ideal_core_id();
        affinity_mask = 1u64 << core_id;
    } else {
        let process_core_mask = process.get_core_mask();
        if (affinity_mask | process_core_mask) != process_core_mask {
            return RESULT_INVALID_CORE_ID;
        }
        if affinity_mask == 0 {
            return RESULT_INVALID_COMBINATION;
        }
        if is_valid_virtual_core_id(core_id) {
            if ((1u64 << core_id) & affinity_mask) == 0 {
                return RESULT_INVALID_COMBINATION;
            }
        } else if core_id != IDEAL_CORE_NO_UPDATE && core_id != IDEAL_CORE_DONT_CARE {
            return RESULT_INVALID_CORE_ID;
        }
    }
    drop(process);

    let Some(thread) = resolve_thread_handle(system, thread_handle) else {
        trace_thread_core_mask(
            3,
            caller_tid,
            None,
            thread_handle,
            core_id,
            affinity_mask,
            RESULT_INVALID_HANDLE,
        );
        return RESULT_INVALID_HANDLE;
    };
    trace_thread_core_mask(
        2,
        caller_tid,
        Some(&thread),
        thread_handle,
        core_id,
        affinity_mask,
        RESULT_SUCCESS,
    );
    let result = ResultCode::new(thread.lock().unwrap().set_core_mask(core_id, affinity_mask));
    trace_thread_core_mask(
        3,
        caller_tid,
        Some(&thread),
        thread_handle,
        core_id,
        affinity_mask,
        result,
    );
    result
}

/// Gets the ID for the specified thread.
pub fn get_thread_id(
    system: &System,
    out_thread_id: &mut u64,
    thread_handle: Handle,
) -> ResultCode {
    let Some(thread) = resolve_thread_handle(system, thread_handle) else {
        return RESULT_INVALID_HANDLE;
    };
    *out_thread_id = thread.lock().unwrap().get_thread_id();
    RESULT_SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::System;
    use crate::hle::kernel::k_process::{KProcess, ProcessLock};
    use crate::hle::kernel::k_resource_limit::{
        create_resource_limit_for_process, LimitableResource,
    };
    use crate::hle::kernel::k_thread::ThreadState;
    use crate::hle::kernel::k_thread_local_page::KThreadLocalPage;
    use crate::hle::kernel::k_typed_address::KProcessAddress;
    use crate::hle::kernel::k_worker_task_manager::KWorkerTaskManager;
    use std::sync::Mutex;

    fn test_system() -> System {
        let mut system = System::new_for_test();

        let mut process = KProcess::new();
        process.capabilities.core_mask = 0xF;
        process.capabilities.priority_mask = u64::MAX;
        process.flags = 0;
        process.allocate_code_memory(0x200000, 0x20000);
        process.initialize_handle_table();
        process
            .thread_local_pages
            .push(KThreadLocalPage::new(KProcessAddress::new(0x240000)));
        process.resource_limit = Some(Arc::new(create_resource_limit_for_process(0x1_0000_0000)));

        let process = Arc::new(ProcessLock::from_value(process));
        let current_thread = Arc::new(KThreadLock::new(KThread::new()));
        let scheduler = Arc::new(Mutex::new(
            crate::hle::kernel::k_scheduler::KScheduler::new(0),
        ));
        {
            let mut thread = current_thread.lock().unwrap();
            thread.initialize_main_thread(0x200000, 0x250000, 0, 0x23f000, &process, 1, 1, false);
            thread.set_priority(44);
            thread.set_base_priority(44);
        }
        process
            .lock()
            .unwrap()
            .register_thread_object(current_thread.clone());
        // Push main thread into priority queue (it's already RUNNABLE).
        process.lock().unwrap().push_back_to_priority_queue(1);
        scheduler.lock().unwrap().initialize(1, 0, 0);

        let shared_memory = process.lock().unwrap().get_shared_memory();

        system.set_current_process_arc(process);
        system.set_scheduler_arc(scheduler);
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&current_thread));
        system.set_shared_process_memory(shared_memory);
        system.set_runtime_program_id(1);
        system.set_runtime_64bit(false);
        system
    }

    #[test]
    fn create_and_start_thread_registers_kernel_objects() {
        let system = test_system();
        let mut handle = 0;
        let result = create_thread(&system, &mut handle, 0x201000, 0x1234, 0x260000, 44, 0);
        assert_eq!(result, RESULT_SUCCESS);
        assert_ne!(handle, INVALID_HANDLE);

        let process_arc = system.current_process_arc();
        let process = process_arc.lock().unwrap();
        let object_id = process.handle_table.get_object(handle).unwrap();
        let thread = process.get_thread_by_object_id(object_id).unwrap();
        drop(process);

        assert_eq!(
            thread.lock().unwrap().get_state(),
            crate::hle::kernel::k_thread::ThreadState::INITIALIZED
        );

        let result = start_thread(&system, handle);
        assert_eq!(result, RESULT_SUCCESS);
        assert_eq!(
            thread.lock().unwrap().get_state(),
            crate::hle::kernel::k_thread::ThreadState::RUNNABLE
        );
    }

    #[test]
    fn sleep_thread_uses_upstream_absolute_timeout_tick() {
        assert_eq!(sleep_timeout_tick_from_ns(1234, 10), 1246);
    }

    #[test]
    #[ignore = "requires a fully kernel-owned scheduler/current-core fixture"]
    fn sleep_thread_positive_requests_reschedule_and_blocks_current_thread() {
        let system = test_system();

        assert!(!system.scheduler_arc().lock().unwrap().needs_scheduling());

        {
            let scheduler_lock = crate::hle::kernel::kernel::scheduler_lock().unwrap();
            let _guard =
                crate::hle::kernel::k_scheduler_lock::KScopedSchedulerLock::new(scheduler_lock);
            sleep_thread(&system, 10);
        }

        let current_thread = system.current_thread().unwrap();
        let thread = current_thread.lock().unwrap();
        assert_eq!(thread.get_state(), ThreadState::WAITING);
        assert_eq!(
            thread.get_wait_reason_for_debugging(),
            crate::hle::kernel::k_thread::ThreadWaitReasonForDebugging::Sleep
        );
        drop(thread);

        assert!(system.scheduler_arc().lock().unwrap().needs_scheduling());
    }

    #[test]
    #[ignore = "requires a fully kernel-owned scheduler/current-core fixture"]
    fn yield_switches_to_other_equal_priority_thread() {
        let system = test_system();
        let mut handle = 0;
        let result = create_thread(&system, &mut handle, 0x201000, 0x1234, 0x260000, 44, 0);
        assert_eq!(result, RESULT_SUCCESS);
        assert_eq!(start_thread(&system, handle), RESULT_SUCCESS);

        // Get the new thread's ID by looking up its handle.
        let new_thread_id = {
            let process_arc = system.current_process_arc();
            let process = process_arc.lock().unwrap();
            let object_id = process.handle_table.get_object(handle).unwrap();
            let thread = process.get_thread_by_object_id(object_id).unwrap();
            let tid = thread.lock().unwrap().get_thread_id();
            tid
        };

        let current_thread_id = system.current_thread_id().unwrap();
        let current_process = system.current_process_arc();
        let next_before_yield = system
            .scheduler_arc()
            .lock()
            .unwrap()
            .select_next_thread_id(&current_process, current_thread_id);
        assert_eq!(next_before_yield, Some(current_thread_id));

        {
            let scheduler_lock = crate::hle::kernel::kernel::scheduler_lock().unwrap();
            let _guard =
                crate::hle::kernel::k_scheduler_lock::KScopedSchedulerLock::new(scheduler_lock);
            sleep_thread(&system, YieldType::WithoutCoreMigration as i64);
        }

        let next_after_yield = system
            .scheduler_arc()
            .lock()
            .unwrap()
            .select_next_thread_id(&current_process, current_thread_id);
        assert_eq!(next_after_yield, Some(new_thread_id));
    }

    #[test]
    #[ignore = "requires a fully kernel-owned scheduler/current-core fixture"]
    fn yield_exits_current_thread_when_termination_was_requested() {
        let system = test_system();
        let mut handle = 0;
        let result = create_thread(&system, &mut handle, 0x201000, 0x1234, 0x260000, 44, 0);
        assert_eq!(result, RESULT_SUCCESS);
        assert_eq!(start_thread(&system, handle), RESULT_SUCCESS);

        let current_thread = system.current_thread().unwrap();
        current_thread.lock().unwrap().request_terminate();

        {
            let scheduler_lock = crate::hle::kernel::kernel::scheduler_lock().unwrap();
            let _guard =
                crate::hle::kernel::k_scheduler_lock::KScopedSchedulerLock::new(scheduler_lock);
            sleep_thread(&system, YieldType::WithoutCoreMigration as i64);
        }
        KWorkerTaskManager::wait_for_global_idle();

        let thread = current_thread.lock().unwrap();
        assert_eq!(thread.get_state(), ThreadState::TERMINATED);
        assert!(thread.is_signaled());
    }

    #[test]
    fn create_thread_returns_limit_reached_when_thread_count_reservation_fails() {
        let system = test_system();
        {
            let process = system.current_process_arc();
            let process = process.lock().unwrap();
            let resource_limit = process.resource_limit.as_ref().unwrap().clone();
            resource_limit
                .set_limit_value(LimitableResource::ThreadCountMax, 0)
                .unwrap();
        }

        let mut handle = INVALID_HANDLE;
        let result = create_thread(&system, &mut handle, 0x201000, 0x1234, 0x260000, 44, 0);
        assert_eq!(result, RESULT_LIMIT_REACHED);
        assert_eq!(handle, INVALID_HANDLE);
    }
}
