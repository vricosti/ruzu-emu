//! Port of zuyu/src/core/hle/kernel/svc/svc_synchronization.cpp
//! Status: COMPLET
//! Derniere synchro: 2026-03-20
//!
//! SVC handlers for synchronization operations (CloseHandle, ResetSignal,
//! WaitSynchronization, CancelSynchronization, SynchronizePreemptionState).

use crate::core::System;
use crate::hle::kernel::k_synchronization_object;
use crate::hle::kernel::svc::svc_results::*;
use crate::hle::kernel::svc_common::{Handle, ARGUMENT_HANDLE_COUNT_MAX};
use crate::hle::result::{ResultCode, RESULT_SUCCESS};

fn should_trace_wait_sync() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_WAIT_SYNC").is_some())
}

fn trace_wait_sync_record(
    stage: u64,
    current_thread_id: u64,
    num_handles: i32,
    timeout_ns: i64,
    result: u32,
    out_index: i32,
    handles: &[Handle],
    object_ids: &[u64],
    object_kind_ids: &[u64],
    object_signaled: &[bool],
) {
    if !common::trace::is_enabled(common::trace::cat::WAIT_SYNC) {
        return;
    }

    let mut args = Vec::with_capacity(14);
    args.extend_from_slice(&[
        stage,
        current_thread_id,
        num_handles as u32 as u64,
        timeout_ns as u64,
        result as u64,
        out_index as u32 as u64,
    ]);
    for i in 0..handles.len().min(2) {
        args.push(handles[i] as u64);
        args.push(object_ids.get(i).copied().unwrap_or_default());
        args.push(object_kind_ids.get(i).copied().unwrap_or_default());
        args.push(u64::from(object_signaled.get(i).copied().unwrap_or(false)));
    }
    common::trace::emit_raw(common::trace::cat::WAIT_SYNC, &args);
}

fn synchronization_timeout_tick_from_ns(current_tick: i64, timeout_ns: i64) -> i64 {
    debug_assert!(timeout_ns > 0);

    let timeout = current_tick.saturating_add(timeout_ns).saturating_add(2);
    if timeout <= 0 {
        i64::MAX
    } else {
        timeout
    }
}

/// Closes a handle.
pub fn close_handle(system: &System, handle: Handle) -> ResultCode {
    log::trace!("svc::CloseHandle closing handle 0x{:08X}", handle);

    if system
        .current_process_arc()
        .lock()
        .unwrap()
        .remove_handle(handle)
    {
        RESULT_SUCCESS
    } else {
        RESULT_INVALID_HANDLE
    }
}

/// Clears the signaled state of an event or process.
pub fn reset_signal(system: &System, handle: Handle) -> ResultCode {
    log::trace!("svc::ResetSignal called handle 0x{:08X}", handle);

    let process_arc = system.current_process_arc();
    let mut process = process_arc.lock().unwrap();
    let Some(object_id) = process.handle_table.get_object(handle) else {
        return RESULT_INVALID_HANDLE;
    };

    if let Some(readable_event) = process.get_readable_event_by_object_id(object_id) {
        return ResultCode::new(readable_event.lock().unwrap().reset());
    }
    if process.process_id == object_id {
        return ResultCode::new(process.reset());
    }

    RESULT_INVALID_HANDLE
}

/// Wait for the given handles to synchronize, timeout after the specified nanoseconds.
pub fn wait_synchronization(
    system: &System,
    out_index: &mut i32,
    user_handles: u64,
    num_handles: i32,
    timeout_ns: i64,
) -> ResultCode {
    log::trace!(
        "svc::WaitSynchronization called user_handles=0x{:x}, num_handles={}, timeout_ns={}",
        user_handles,
        num_handles,
        timeout_ns
    );
    // Env-gated: log every WaitSync's actual handle values + caller tid.
    // Useful for identifying wedge-targeted handles (e.g. tid=73 polling
    // handle 0x000D01E8 with timeout=0).
    if std::env::var_os("RUZU_TRACE_WAITSYNC").is_some() && num_handles > 0 {
        let tid = system.current_thread_id().unwrap_or(0);
        // Quick peek: read first handle without holding any lock.
        let process_arc = system.current_process_arc();
        let process = process_arc.lock().unwrap();
        let h0 = if let Some(memory) = process.get_memory().as_ref() {
            memory.lock().unwrap().read_32(user_handles)
        } else {
            0
        };
        drop(process);
        log::info!(
            "WaitSync tid={} num={} timeout={} handle[0]=0x{:X}",
            tid,
            num_handles,
            timeout_ns,
            h0
        );
    }

    // Ensure number of handles is valid.
    if !(0..=ARGUMENT_HANDLE_COUNT_MAX).contains(&num_handles) {
        return RESULT_OUT_OF_RANGE;
    }

    let process_arc = system.current_process_arc();
    let process = process_arc.lock().unwrap();

    // Read handle array from guest memory.
    // Upstream: R_UNLESS(GetCurrentMemory(kernel).ReadBlock(user_handles, handles.data(),
    //                    sizeof(Handle) * num_handles), ResultInvalidPointer)
    let mut handles = Vec::with_capacity(num_handles as usize);
    if num_handles > 0 {
        let handle_bytes = num_handles as usize * std::mem::size_of::<Handle>();
        if let Some(memory) = process.get_memory().as_ref() {
            let m = memory.lock().unwrap();
            if !m.is_valid_virtual_address_range(user_handles, handle_bytes as u64) {
                return RESULT_INVALID_POINTER;
            }
            let mut handle_data = vec![0u8; handle_bytes];
            let read_ok = m.read_block_checked_quiet(user_handles, &mut handle_data);
            drop(m);
            if !read_ok {
                return RESULT_INVALID_POINTER;
            }
            for chunk in handle_data.chunks_exact(std::mem::size_of::<Handle>()) {
                handles.push(Handle::from_le_bytes([
                    chunk[0], chunk[1], chunk[2], chunk[3],
                ]));
            }
        } else {
            return RESULT_INVALID_POINTER;
        }
    }

    let Some(current_thread_id) = system.current_thread_id() else {
        return RESULT_INVALID_HANDLE;
    };
    let Some(current_thread) = process.get_thread_by_thread_id(current_thread_id) else {
        return RESULT_INVALID_HANDLE;
    };

    let trace_wait_sync_ring = common::trace::is_enabled(common::trace::cat::WAIT_SYNC);
    let mut object_ids = Vec::with_capacity(num_handles as usize);
    let mut object_kinds = Vec::with_capacity(num_handles as usize);
    let mut object_kind_ids = Vec::with_capacity(num_handles as usize);
    let mut object_signaled = Vec::with_capacity(num_handles as usize);
    for handle in &handles {
        let Some(object_id) = process.handle_table.get_object(*handle) else {
            return RESULT_INVALID_HANDLE;
        };
        let (kind, kind_id) = if process.get_readable_event_by_object_id(object_id).is_some() {
            ("readable_event", 1)
        } else if process.get_thread_by_object_id(object_id).is_some() {
            ("thread", 2)
        } else if process.process_id == object_id {
            ("process", 3)
        } else if process.get_event_by_object_id(object_id).is_some() {
            ("event", 4)
        } else if process.get_server_port_by_object_id(object_id).is_some() {
            ("server_port", 5)
        } else if process.get_server_session_by_object_id(object_id).is_some() {
            ("server_session", 6)
        } else {
            ("unknown", 0)
        };
        let signaled = trace_wait_sync_ring
            && k_synchronization_object::is_object_signaled(&process, object_id);
        object_ids.push(object_id);
        object_kinds.push(kind);
        object_kind_ids.push(kind_id);
        object_signaled.push(signaled);
    }
    if trace_wait_sync_ring {
        trace_wait_sync_record(
            1,
            current_thread_id,
            num_handles,
            timeout_ns,
            0,
            -1,
            &handles,
            &object_ids,
            &object_kind_ids,
            &object_signaled,
        );
    }
    if should_trace_wait_sync() {
        let process_id = process.get_process_id();
        log::info!(
            "WaitSynchronization pid={} tid={} handles={:?} object_ids={:?} kinds={:?} timeout_ns={}",
            process_id,
            current_thread_id,
            handles,
            object_ids,
            object_kinds,
            timeout_ns
        );
    }
    // Narrow diagnostic for wait wedges. Set RUZU_TRACE_WAITSYNC_TID to a
    // comma-separated tid list after identifying the target with SIGUSR1.
    let trace_this_tid = std::env::var("RUZU_TRACE_WAITSYNC_TID")
        .ok()
        .map(|spec| {
            spec.split(',')
                .filter_map(|part| part.trim().parse::<u64>().ok())
                .any(|tid| tid == current_thread_id)
        })
        .unwrap_or(false);
    if trace_this_tid {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static TRACE_TID_COUNT: AtomicUsize = AtomicUsize::new(0);
        let cnt = TRACE_TID_COUNT.fetch_add(1, Ordering::Relaxed);
        // First 16 calls verbose, then every 256th call.
        if cnt < 16 || cnt.is_power_of_two() {
            log::info!(
                "[WAIT_SYNC_DBG] tid={} call#{} handles={:?} object_ids={:?} kinds={:?} timeout_ns={}",
                current_thread_id,
                cnt,
                handles,
                object_ids,
                object_kinds,
                timeout_ns,
            );
        }
    }
    log::trace!(
        "  WaitSynchronization handles={:?} object_ids={:?} kinds={:?} timeout_ns={}",
        handles,
        object_ids,
        object_kinds,
        timeout_ns
    );
    let timeout = if timeout_ns > 0 {
        let current_tick = system
            .kernel()
            .and_then(|_| crate::hle::kernel::kernel::get_current_hardware_tick())
            .unwrap_or(i64::MAX);
        synchronization_timeout_tick_from_ns(current_tick, timeout_ns)
    } else {
        timeout_ns
    };
    let scheduler = system.scheduler_arc().clone();
    drop(process);
    let result = k_synchronization_object::wait(
        &process_arc,
        &current_thread,
        &scheduler,
        out_index,
        object_ids,
        timeout,
    );
    if trace_wait_sync_ring {
        trace_wait_sync_record(
            2,
            current_thread_id,
            num_handles,
            timeout_ns,
            result.get_inner_value(),
            *out_index,
            &handles,
            &[],
            &[],
            &[],
        );
    }
    if should_trace_wait_sync() {
        let process = process_arc.lock().unwrap();
        log::info!(
            "WaitSynchronization return pid={} tid={} result=0x{:08X} out_index={}",
            process.get_process_id(),
            current_thread_id,
            result.get_inner_value(),
            *out_index
        );
    }
    log::trace!(
        "  WaitSynchronization result={:?} out_index={}",
        result,
        *out_index
    );
    result
}

/// Resumes a thread waiting on WaitSynchronization.
pub fn cancel_synchronization(system: &System, handle: Handle) -> ResultCode {
    log::trace!("svc::CancelSynchronization called handle=0x{:X}", handle);

    let process_arc = system.current_process_arc();
    let process = process_arc.lock().unwrap();
    let Some(object_id) = process.handle_table.get_object(handle) else {
        return RESULT_INVALID_HANDLE;
    };
    let Some(thread) = process.get_thread_by_object_id(object_id) else {
        return RESULT_INVALID_HANDLE;
    };
    drop(process);

    thread.lock().unwrap().wait_cancel();
    system.scheduler_arc().lock().unwrap().request_schedule();
    RESULT_SUCCESS
}

/// Synchronizes preemption state.
///
/// Upstream: Lock scheduler, check if current thread is pinned on the current core,
/// clear interrupt flag, unpin.
pub fn synchronize_preemption_state(system: &System) {
    // Upstream: `KScopedSchedulerLock sl{kernel};` — unconditional.
    let current_thread = match crate::hle::kernel::kernel::get_current_emu_thread() {
        Some(t) => t,
        None => return,
    };
    let current_thread_id = current_thread.lock().unwrap().get_thread_id();
    let scheduler_lock = crate::hle::kernel::kernel::scheduler_lock()
        .expect("scheduler_lock must exist — kernel not initialized?");
    let _sl = crate::hle::kernel::k_scheduler_lock::KScopedSchedulerLock::new(scheduler_lock);

    let core_id = match system.kernel() {
        Some(k) => k.current_physical_core_index() as i32,
        None => return,
    };

    let process_arc = system.current_process_arc();
    let mut process = process_arc.lock().unwrap();

    // If the current thread is pinned, unpin it.
    if let Some(pinned_id) = process.get_pinned_thread(core_id) {
        if pinned_id == current_thread_id {
            // Clear the current thread's interrupt flag.
            if let Some(thread) = process.get_thread_by_thread_id(current_thread_id) {
                let mut thread = thread.lock().unwrap();
                thread.clear_interrupt_flag();
                thread.unpin();
            }

            // Unpin the current thread.
            process.unpin_current_thread(core_id, current_thread_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::System;
    use crate::hle::kernel::k_memory_block::PAGE_SIZE;
    use crate::hle::kernel::k_memory_manager::Pool;
    use crate::hle::kernel::k_process::{KProcess, ProcessLock};
    use crate::hle::kernel::k_resource_limit::create_resource_limit_for_process;
    use crate::hle::kernel::k_thread::{KThread, KThreadLock, ThreadState};
    use crate::hle::kernel::k_worker_task_manager::KWorkerTaskManager;
    use crate::hle::kernel::kernel::ScopedKernelForTest;
    use crate::hle::kernel::svc::svc_event;
    use crate::hle::kernel::svc::svc_thread;
    use crate::hle::kernel::svc::svc_transfer_memory;
    use crate::hle::kernel::svc::svc_types::MemoryPermission;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex, MutexGuard};

    static SVC_SYNCHRONIZATION_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn svc_synchronization_test_guard() -> MutexGuard<'static, ()> {
        SVC_SYNCHRONIZATION_TEST_LOCK
            .lock()
            .expect("svc_synchronization test lock must not be poisoned")
    }

    fn kernel_with_application_pool_for_test(num_pages: usize) -> ScopedKernelForTest {
        let mut kernel = ScopedKernelForTest::new();
        kernel
            .kernel_mut()
            .initialize_memory_block_slab_manager(4096);
        kernel.memory_manager_mut().initialize_pool(
            Pool::Application,
            0x1_0000_0000,
            num_pages * PAGE_SIZE,
        );
        kernel
    }

    fn test_system() -> System {
        let mut system = System::new_for_test();
        system.initialize();
        {
            let kernel = system
                .kernel_mut()
                .expect("test system must own an initialized kernel");
            kernel.initialize();
            kernel.initialize_memory_block_slab_manager(4096);
            kernel.memory_manager_mut().initialize_pool(
                Pool::Application,
                0x1_0000_0000,
                0x80000 * PAGE_SIZE,
            );
        }
        let mut process = KProcess::new();
        process.process_id = 100;
        process.capabilities.core_mask = 0xF;
        process.capabilities.priority_mask = u64::MAX;
        process.flags = 0;
        process.initialize_handle_table();
        process.resource_limit = Some(Arc::new(create_resource_limit_for_process(0x4000_0000)));
        process.create_memory(&system);
        process.allocate_code_memory(0x200000, 0x1000);
        set_current_page_table_for_test(&mut process);
        process.initialize_thread_local_region_base(0x240000);
        process.initialize_main_thread_stack_region(0x240000, 0x100000);
        process.page_table.set_heap_region(
            crate::hle::kernel::k_typed_address::KProcessAddress::new(0x400000),
            0x200000,
        );

        let process = Arc::new(ProcessLock::from_value(process));
        let current_thread = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut thread = current_thread.lock().unwrap();
            thread.thread_id = 1;
            thread.object_id = 1;
            thread
                .thread_state
                .store(ThreadState::RUNNABLE.bits(), Ordering::Relaxed);
        }
        process
            .lock()
            .unwrap()
            .register_thread_object(current_thread.clone());

        let scheduler = Arc::new(Mutex::new(
            crate::hle::kernel::k_scheduler::KScheduler::new(0),
        ));
        scheduler.lock().unwrap().initialize(1, 0, 0);
        let shared_memory = process.lock().unwrap().get_shared_memory();

        system.set_current_process_arc(process);
        system.set_scheduler_arc(scheduler);
        system.set_shared_process_memory(shared_memory);
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&current_thread));
        system.set_runtime_program_id(1);
        system.set_runtime_64bit(false);
        system
    }

    fn set_current_page_table_for_test(process: &mut KProcess) {
        let page_table = process.page_table.get_base_mut();
        let memory = page_table
            .m_memory
            .as_ref()
            .expect("test page table memory must be attached")
            .clone();
        let impl_pt = page_table
            .m_impl
            .as_mut()
            .expect("test page table backend must be initialized");
        memory
            .lock()
            .unwrap()
            .set_current_page_table(impl_pt.as_mut() as *mut _, true);
    }

    fn write_wait_handles(system: &System, handles: &[Handle]) -> u64 {
        let process_arc = system.current_process_arc();
        let mut process = process_arc.lock().unwrap();
        let (result, heap_base) = process.set_heap_size(0x200000);
        assert_eq!(result, RESULT_SUCCESS.get_inner_value());
        let addr = heap_base.get() + 0x100;
        let memory = process
            .page_table
            .get_base()
            .m_memory
            .as_ref()
            .expect("test process must own upstream-shaped Memory")
            .clone();
        drop(process);

        let mut bytes = Vec::with_capacity(handles.len() * std::mem::size_of::<Handle>());
        for handle in handles {
            bytes.extend_from_slice(&handle.to_le_bytes());
        }
        assert!(memory.lock().unwrap().write_block(addr, &bytes));
        addr
    }

    #[test]
    fn synchronize_preemption_state_is_dispatched_for_both_abis() {
        let _guard = svc_synchronization_test_guard();
        let system = test_system();
        let current_thread = crate::hle::kernel::kernel::get_current_thread_pointer()
            .expect("test current thread must be installed");
        let core_id = system
            .kernel()
            .expect("test kernel must be initialized")
            .current_physical_core_index() as i32;

        for is_64bit in [false, true] {
            system
                .current_process_arc()
                .lock()
                .unwrap()
                .pin_current_thread(core_id, 1, false);
            current_thread.lock().unwrap().pin(core_id);

            let mut args = [0; 8];
            crate::hle::kernel::svc_dispatch::call(
                &system,
                crate::hle::kernel::svc_dispatch::SvcId::SynchronizePreemptionState as u32,
                is_64bit,
                &mut args,
            );

            assert_eq!(
                system
                    .current_process_arc()
                    .lock()
                    .unwrap()
                    .get_pinned_thread(core_id),
                None,
                "SynchronizePreemptionState must unpin through the {is_64bit}-bit dispatcher"
            );
        }
    }

    #[test]
    fn reset_signal_resets_readable_event() {
        let _guard = svc_synchronization_test_guard();
        let system = test_system();
        let mut write_handle = 0;
        let mut read_handle = 0;
        assert_eq!(
            svc_event::create_event(&system, &mut write_handle, &mut read_handle),
            RESULT_SUCCESS
        );
        assert_eq!(
            svc_event::signal_event(&system, write_handle),
            RESULT_SUCCESS
        );
        assert_eq!(reset_signal(&system, read_handle), RESULT_SUCCESS);
    }

    #[test]
    fn wait_synchronization_returns_signaled_index() {
        let _guard = svc_synchronization_test_guard();
        let system = test_system();
        let mut write_handle = 0;
        let mut read_handle = 0;
        assert_eq!(
            svc_event::create_event(&system, &mut write_handle, &mut read_handle),
            RESULT_SUCCESS
        );
        assert_eq!(
            svc_event::signal_event(&system, write_handle),
            RESULT_SUCCESS
        );

        let handles_addr = write_wait_handles(&system, &[read_handle]);

        let mut out_index = -1;
        assert_eq!(
            wait_synchronization(&system, &mut out_index, handles_addr, 1, 0),
            RESULT_SUCCESS
        );
        assert_eq!(out_index, 0);
    }

    #[test]
    fn close_handle_recoalesces_transfer_memory_split_heap_region() {
        let _guard = svc_synchronization_test_guard();
        let _kernel = kernel_with_application_pool_for_test(0x80000);
        let system = test_system();
        let (heap_base, heap_size) = {
            let process_arc = system.current_process_arc();
            let mut process = process_arc.lock().unwrap();
            let (result, heap_base) = process.set_heap_size(0x200000);
            assert_eq!(result, RESULT_SUCCESS.get_inner_value());

            let info = process
                .page_table
                .query_info(heap_base.get() as usize)
                .expect("heap base must query after SetHeapSize");
            (heap_base.get(), info.get_size())
        };

        let transfer_addr = heap_base + 0x10000;
        let transfer_size = 0x20000u64;
        let mut handle = 0;
        assert_eq!(
            svc_transfer_memory::create_transfer_memory(
                &system,
                &mut handle,
                transfer_addr,
                transfer_size,
                MemoryPermission::ReadWrite,
            ),
            RESULT_SUCCESS
        );

        let transfer_object_id = {
            let process_arc = system.current_process_arc();
            let process = process_arc.lock().unwrap();
            let object_id = process
                .handle_table
                .get_object(handle)
                .expect("CreateTransferMemory must return a live handle");

            let split_info = process
                .page_table
                .query_info(heap_base as usize)
                .expect("heap query should succeed while transfer memory is locked");
            assert_eq!(split_info.get_size(), 0x10000);
            assert!(process
                .get_transfer_memory_by_object_id(object_id)
                .is_some());
            object_id
        };

        assert_eq!(close_handle(&system, handle), RESULT_SUCCESS);

        let process_arc = system.current_process_arc();
        let process = process_arc.lock().unwrap();
        let merged_info = process
            .page_table
            .query_info(heap_base as usize)
            .expect("heap query should succeed after transfer memory handle close");
        assert_eq!(merged_info.get_size(), heap_size);
        assert!(process
            .get_transfer_memory_by_object_id(transfer_object_id)
            .is_none());
    }

    #[test]
    fn close_handle_runs_transfer_memory_post_destroy_resource_release() {
        let _guard = svc_synchronization_test_guard();
        let _kernel = kernel_with_application_pool_for_test(0x80000);
        let system = test_system();
        let heap_base = {
            let process_arc = system.current_process_arc();
            let mut process = process_arc.lock().unwrap();
            let (result, heap_base) = process.set_heap_size(0x200000);
            assert_eq!(result, RESULT_SUCCESS.get_inner_value());
            heap_base.get()
        };

        let mut handle = 0;
        assert_eq!(
            svc_transfer_memory::create_transfer_memory(
                &system,
                &mut handle,
                heap_base + 0x10000,
                0x20000,
                MemoryPermission::ReadWrite,
            ),
            RESULT_SUCCESS
        );

        {
            let process_arc = system.current_process_arc();
            let process = process_arc.lock().unwrap();
            let current = process.resource_limit.as_ref().unwrap().get_current_value(
                crate::hle::kernel::k_resource_limit::LimitableResource::TransferMemoryCountMax,
            );
            assert_eq!(current, 1);
        }

        assert_eq!(close_handle(&system, handle), RESULT_SUCCESS);

        let process_arc = system.current_process_arc();
        let process = process_arc.lock().unwrap();
        let current = process.resource_limit.as_ref().unwrap().get_current_value(
            crate::hle::kernel::k_resource_limit::LimitableResource::TransferMemoryCountMax,
        );
        assert_eq!(current, 0);
    }

    #[test]
    fn wait_synchronization_timeout_zero_returns_timed_out() {
        let _guard = svc_synchronization_test_guard();
        let system = test_system();
        let mut write_handle = 0;
        let mut read_handle = 0;
        assert_eq!(
            svc_event::create_event(&system, &mut write_handle, &mut read_handle),
            RESULT_SUCCESS
        );

        let handles_addr = write_wait_handles(&system, &[read_handle]);

        let mut out_index = -1;
        assert_eq!(
            wait_synchronization(&system, &mut out_index, handles_addr, 1, 0),
            RESULT_TIMED_OUT
        );
        assert_eq!(out_index, -1);
    }

    #[test]
    fn synchronization_timeout_tick_from_ns_uses_absolute_tick() {
        let _guard = svc_synchronization_test_guard();
        assert_eq!(synchronization_timeout_tick_from_ns(100, 25), 127);
    }

    #[test]
    #[ignore = "requires a full scheduler handoff harness after WaitSynchronization moved to Memory-backed handle reads"]
    fn wait_synchronization_blocks_then_wakes_on_signal() {
        let _guard = svc_synchronization_test_guard();
        let system = test_system();
        let mut write_handle = 0;
        let mut read_handle = 0;
        assert_eq!(
            svc_event::create_event(&system, &mut write_handle, &mut read_handle),
            RESULT_SUCCESS
        );

        let handles_addr = write_wait_handles(&system, &[read_handle]);

        let system_ref = crate::core::SystemRef::from_ref(&system);
        let waiter = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(5));
            assert_eq!(
                svc_event::signal_event(system_ref.get(), write_handle),
                RESULT_SUCCESS
            );
        });

        let mut out_index = -1;
        assert_eq!(
            wait_synchronization(&system, &mut out_index, handles_addr, 1, -1),
            RESULT_SUCCESS
        );
        assert_eq!(out_index, 0);

        waiter.join().unwrap();

        let current_thread = {
            system
                .current_process_arc()
                .lock()
                .unwrap()
                .get_thread_by_thread_id(1)
                .unwrap()
        };

        let thread = current_thread.lock().unwrap();
        assert_eq!(
            thread.get_state(),
            crate::hle::kernel::k_thread::ThreadState::RUNNABLE
        );
        assert_eq!(thread.get_synced_index(), 0);
        assert_eq!(thread.get_wait_result(), RESULT_SUCCESS.get_inner_value());
        assert_eq!(
            thread.thread_context.r[0],
            RESULT_SUCCESS.get_inner_value() as u64
        );
        assert_eq!(thread.thread_context.r[1], 0);
    }

    #[test]
    fn wait_synchronization_returns_cancelled_when_wait_cancelled_is_set() {
        let _guard = svc_synchronization_test_guard();
        let system = test_system();
        let mut write_handle = 0;
        let mut read_handle = 0;
        assert_eq!(
            svc_event::create_event(&system, &mut write_handle, &mut read_handle),
            RESULT_SUCCESS
        );

        let handles_addr = write_wait_handles(&system, &[read_handle]);

        let current_thread = {
            system
                .current_process_arc()
                .lock()
                .unwrap()
                .get_thread_by_thread_id(1)
                .unwrap()
        };
        current_thread.lock().unwrap().wait_cancel();

        let mut out_index = 123;
        assert_eq!(
            wait_synchronization(&system, &mut out_index, handles_addr, 1, -1),
            RESULT_CANCELLED
        );
        assert_eq!(out_index, -1);
        assert!(!current_thread.lock().unwrap().is_wait_cancelled());
        assert_eq!(
            current_thread.lock().unwrap().get_state(),
            crate::hle::kernel::k_thread::ThreadState::RUNNABLE
        );
    }

    #[test]
    #[ignore = "requires a full scheduler handoff harness after WaitSynchronization moved to Memory-backed handle reads"]
    fn wait_synchronization_blocks_then_wakes_on_thread_exit() {
        let _guard = svc_synchronization_test_guard();
        let system = test_system();

        let mut thread_handle = 0;
        assert_eq!(
            svc_thread::create_thread(
                &system,
                &mut thread_handle,
                0x201000,
                0x1234,
                0x260000,
                44,
                0
            ),
            RESULT_SUCCESS
        );

        let handles_addr = write_wait_handles(&system, &[thread_handle]);

        let object_id = system
            .current_process_arc()
            .lock()
            .unwrap()
            .handle_table
            .get_object(thread_handle)
            .unwrap();
        let target_thread = system
            .current_process_arc()
            .lock()
            .unwrap()
            .get_thread_by_object_id(object_id)
            .unwrap();

        let target_thread_for_exit = target_thread.clone();
        let exiter = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(5));
            target_thread_for_exit.lock().unwrap().exit();
            KWorkerTaskManager::wait_for_global_idle();
        });

        let mut out_index = -1;
        assert_eq!(
            wait_synchronization(&system, &mut out_index, handles_addr, 1, -1),
            RESULT_SUCCESS
        );
        assert_eq!(out_index, 0);

        exiter.join().unwrap();

        let current_thread = {
            system
                .current_process_arc()
                .lock()
                .unwrap()
                .get_thread_by_thread_id(1)
                .unwrap()
        };

        let thread = current_thread.lock().unwrap();
        assert_eq!(
            thread.get_state(),
            crate::hle::kernel::k_thread::ThreadState::RUNNABLE
        );
        assert_eq!(thread.get_synced_index(), 0);
        assert_eq!(thread.get_wait_result(), RESULT_SUCCESS.get_inner_value());
        assert_eq!(
            thread.thread_context.r[0],
            RESULT_SUCCESS.get_inner_value() as u64
        );
        assert_eq!(thread.thread_context.r[1], 0);
    }

    #[test]
    #[ignore = "requires a full scheduler handoff harness after WaitSynchronization moved to Memory-backed handle reads"]
    fn wait_synchronization_blocks_then_wakes_on_process_signal() {
        let _guard = svc_synchronization_test_guard();
        let system = test_system();

        let process_handle = {
            let process_arc = system.current_process_arc();
            let mut process = process_arc.lock().unwrap();
            let process_id = process.process_id;
            process.state = crate::hle::kernel::k_process::ProcessState::RunningAttached;
            process.is_signaled = false;
            process.handle_table.add(process_id).unwrap()
        };

        let handles_addr = write_wait_handles(&system, &[process_handle]);

        let mut out_index = -1;
        assert_eq!(
            wait_synchronization(&system, &mut out_index, handles_addr, 1, -1),
            RESULT_SUCCESS
        );
        assert_eq!(out_index, -1);

        let current_thread = {
            system
                .current_process_arc()
                .lock()
                .unwrap()
                .get_thread_by_thread_id(1)
                .unwrap()
        };
        assert_eq!(
            current_thread.lock().unwrap().get_state(),
            crate::hle::kernel::k_thread::ThreadState::WAITING
        );

        system
            .current_process_arc()
            .lock()
            .unwrap()
            .set_debug_break();

        let thread = current_thread.lock().unwrap();
        assert_eq!(
            thread.get_state(),
            crate::hle::kernel::k_thread::ThreadState::RUNNABLE
        );
        assert_eq!(thread.get_synced_index(), 0);
        assert_eq!(thread.get_wait_result(), RESULT_SUCCESS.get_inner_value());
    }
}
