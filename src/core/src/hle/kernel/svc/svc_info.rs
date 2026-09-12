//! Port of zuyu/src/core/hle/kernel/svc/svc_info.cpp
//! Status: Ported
//! Derniere synchro: 2026-03-20
//!
//! SVC handlers for GetInfo and GetSystemInfo.

use crate::core::System;
use crate::hle::kernel::svc::svc_results::*;
use crate::hle::kernel::svc::svc_types::*;
use crate::hle::kernel::svc_common::{Handle, PseudoHandle, INVALID_HANDLE};
use crate::hle::result::{ResultCode, RESULT_SUCCESS};

/// Gets system/memory information for the current process.
///
/// This is a large switch over InfoType, querying process page tables,
/// resource limits, random entropy, thread tick counts, idle tick counts, etc.
pub fn get_info(
    system: &System,
    result: &mut u64,
    info_id_type: InfoType,
    handle: Handle,
    info_sub_id: u64,
) -> ResultCode {
    log::info!(
        "svc::GetInfo called info_id={:?}, info_sub_id=0x{:X}, handle=0x{:08X}",
        info_id_type,
        info_sub_id,
        handle
    );

    let rc = get_info_impl(system, result, info_id_type, handle, info_sub_id);

    // RUZU_TRACE_QUERY_RET=N (or comma list, or "*"/"all") — log GetInfo return values
    // for the matching tid(s). Same env var as QueryMemory so we get a unified return-value
    // log against the zuyu side.
    if let Some(target_str) = std::env::var_os("RUZU_TRACE_QUERY_RET") {
        let tid = system
            .current_thread()
            .and_then(|t| t.lock().ok().map(|g| g.get_thread_id()))
            .unwrap_or(0);
        let s = target_str.to_string_lossy();
        let want = s == "*"
            || s == "all"
            || s.split(',')
                .filter_map(|p| p.trim().parse::<u64>().ok())
                .any(|t| t == tid);
        if want {
            eprintln!(
                "[QUERY_RET] tid={} GetInfo(id={:?}, handle=0x{:08X}, sub=0x{:X}) -> rc=0x{:X} result=0x{:X}",
                tid,
                info_id_type,
                handle,
                info_sub_id,
                rc.0,
                *result,
            );
        }
    }

    rc
}

fn get_info_impl(
    system: &System,
    result: &mut u64,
    info_id_type: InfoType,
    handle: Handle,
    info_sub_id: u64,
) -> ResultCode {
    match info_id_type {
        InfoType::CoreMask
        | InfoType::PriorityMask
        | InfoType::AliasRegionAddress
        | InfoType::AliasRegionSize
        | InfoType::HeapRegionAddress
        | InfoType::HeapRegionSize
        | InfoType::AslrRegionAddress
        | InfoType::AslrRegionSize
        | InfoType::StackRegionAddress
        | InfoType::StackRegionSize
        | InfoType::TotalMemorySize
        | InfoType::UsedMemorySize
        | InfoType::SystemResourceSizeTotal
        | InfoType::SystemResourceSizeUsed
        | InfoType::ProgramId
        | InfoType::UserExceptionContextAddress
        | InfoType::TotalNonSystemMemorySize
        | InfoType::UsedNonSystemMemorySize
        | InfoType::IsApplication
        | InfoType::FreeThreadCount
        | InfoType::AliasRegionExtraSize => {
            if info_sub_id != 0 {
                return RESULT_INVALID_ENUM_VALUE;
            }

            // Get the process from the handle.
            // Upstream: GetCurrentProcess(kernel).GetHandleTable().GetObject<KProcess>(handle)
            // For pseudo-handle CurrentProcess (0xFFFF8001) or handle 0 (self), use current process.
            let process_arc = if handle == PseudoHandle::CurrentProcess as Handle || handle == 0 {
                system.current_process_arc()
            } else {
                let current = system.current_process_arc();
                let guard = current.lock().unwrap();
                match guard.handle_table.get_object(handle) {
                    Some(_) => {
                        drop(guard);
                        system.current_process_arc()
                    }
                    None => {
                        return RESULT_INVALID_HANDLE;
                    }
                }
            };

            let process = process_arc.lock().unwrap();

            match info_id_type {
                InfoType::CoreMask => {
                    *result = process.get_core_mask();
                }
                InfoType::PriorityMask => {
                    *result = process.get_priority_mask();
                }
                InfoType::AliasRegionAddress => {
                    *result = process.page_table.get_alias_region_start().get();
                }
                InfoType::AliasRegionSize => {
                    *result = process.page_table.get_alias_region_size() as u64;
                }
                InfoType::HeapRegionAddress => {
                    *result = process.page_table.get_heap_region_start().get();
                }
                InfoType::HeapRegionSize => {
                    *result = process.page_table.get_heap_region_size() as u64;
                }
                InfoType::AslrRegionAddress => {
                    *result = process.page_table.get_alias_code_region_start().get();
                }
                InfoType::AslrRegionSize => {
                    *result = process.page_table.get_alias_code_region_size() as u64;
                }
                InfoType::StackRegionAddress => {
                    *result = process.page_table.get_stack_region_start().get();
                }
                InfoType::StackRegionSize => {
                    *result = process.page_table.get_stack_region_size() as u64;
                }
                InfoType::TotalMemorySize => {
                    *result = process.get_total_user_physical_memory_size() as u64;
                }
                InfoType::UsedMemorySize => {
                    *result = process.get_used_user_physical_memory_size() as u64;
                }
                InfoType::SystemResourceSizeTotal => {
                    *result = process.get_total_system_resource_size() as u64;
                }
                InfoType::SystemResourceSizeUsed => {
                    *result = process.get_used_system_resource_size() as u64;
                }
                InfoType::ProgramId => {
                    *result = process.get_program_id();
                }
                InfoType::UserExceptionContextAddress => {
                    *result = process.get_process_local_region_address().get();
                }
                InfoType::TotalNonSystemMemorySize => {
                    *result = process.get_total_non_system_user_physical_memory_size() as u64;
                }
                InfoType::UsedNonSystemMemorySize => {
                    *result = process.get_used_non_system_user_physical_memory_size() as u64;
                }
                InfoType::IsApplication => {
                    *result = if process.is_application() { 1 } else { 0 };
                }
                InfoType::FreeThreadCount => {
                    // Upstream: resource_limit->GetLimitValue(ThreadCountMax) - resource_limit->GetCurrentValue(ThreadCountMax)
                    // Approximate with a reasonable default.
                    if let Some(ref rl) = process.resource_limit {
                        let limit = rl.get_limit_value(
                            crate::hle::kernel::k_resource_limit::LimitableResource::ThreadCountMax,
                        );
                        let current = rl.get_current_value(
                            crate::hle::kernel::k_resource_limit::LimitableResource::ThreadCountMax,
                        );
                        *result = (limit - current) as u64;
                    } else {
                        *result = 0;
                    }
                }
                InfoType::AliasRegionExtraSize => {
                    // Local fw-18+ extension used by newer homebrew/libnx.
                    // When no extra 39-bit alias region is configured, the kernel returns 0.
                    *result = 0;
                }
                _ => {
                    log::error!("Unimplemented svcGetInfo id=0x{:X}", info_id_type as u32);
                    return RESULT_INVALID_ENUM_VALUE;
                }
            }

            RESULT_SUCCESS
        }

        InfoType::DebuggerAttached => {
            *result = 0;
            RESULT_SUCCESS
        }

        InfoType::ResourceLimit => {
            if handle != 0 {
                return RESULT_INVALID_HANDLE;
            }
            if info_sub_id != 0 {
                return RESULT_INVALID_COMBINATION;
            }

            // Upstream: Get current process resource limit, add to handle table, return the handle.
            // If no resource limit, return InvalidHandle (upstream considers this success).
            let process_arc = system.current_process_arc();
            let mut process = process_arc.lock().unwrap();
            if process.resource_limit.is_none() {
                *result = INVALID_HANDLE as u64;
                return RESULT_SUCCESS;
            }

            // Add the resource limit to the handle table.
            // Use object_id 0xFFFF_FFFF as a sentinel for resource limit objects.
            match process.handle_table.add(0xFFFF_FFFF) {
                Ok(resource_handle) => {
                    *result = resource_handle as u64;
                }
                Err(_) => {
                    *result = INVALID_HANDLE as u64;
                }
            }
            RESULT_SUCCESS
        }

        InfoType::RandomEntropy => {
            if handle != 0 {
                return RESULT_INVALID_HANDLE;
            }
            if info_sub_id >= 4 {
                return RESULT_INVALID_COMBINATION;
            }

            let process_arc = system.current_process_arc();
            let process = process_arc.lock().unwrap();
            *result = process.get_random_entropy(info_sub_id as usize);
            RESULT_SUCCESS
        }

        InfoType::InitialProcessIdRange => {
            // Upstream: STUBBED — returns 0 for privileged process id bounds.
            log::warn!("svc::GetInfo(InitialProcessIdRange): Upstream STUBBED, returned 0");
            *result = 0;
            RESULT_SUCCESS
        }

        InfoType::ThreadTickCount => {
            const NUM_CPUS: u64 = 4;
            if info_sub_id != 0xFFFF_FFFF_FFFF_FFFF && info_sub_id >= NUM_CPUS {
                log::error!(
                    "Core count is out of range, expected {} but got {}",
                    NUM_CPUS,
                    info_sub_id
                );
                return RESULT_INVALID_COMBINATION;
            }

            let process_arc = system.current_process_arc();
            let target_thread = {
                let process = process_arc.lock().unwrap();
                let Some(thread_obj_id) = process.handle_table.get_object(handle) else {
                    log::error!("Thread handle does not exist, handle=0x{:08X}", handle);
                    return RESULT_INVALID_HANDLE;
                };
                match process.get_thread_by_object_id(thread_obj_id) {
                    Some(thread) => thread,
                    None => {
                        log::error!("Thread handle does not exist, handle=0x{:08X}", handle);
                        return RESULT_INVALID_HANDLE;
                    }
                }
            };

            let Some(current_thread) = system.current_thread() else {
                *result = 0;
                return RESULT_SUCCESS;
            };
            let Some(kernel) = system.kernel() else {
                *result = 0;
                return RESULT_SUCCESS;
            };
            let scheduler_arc = kernel
                .current_scheduler()
                .cloned()
                .unwrap_or_else(|| system.scheduler_arc());

            let same_thread = std::sync::Arc::ptr_eq(&current_thread, &target_thread);
            let scheduler = scheduler_arc.lock().unwrap();
            let prev_ctx_ticks = scheduler.get_last_context_switch_time();
            let current_ticks = scheduler
                .core_timing
                .as_ref()
                .map(|core_timing| core_timing.get_global_time_ns().as_nanos() as i64)
                .unwrap_or(prev_ctx_ticks);
            let elapsed = current_ticks - prev_ctx_ticks;
            let current_core = kernel.current_physical_core_index() as u64;

            *result = if same_thread && info_sub_id == 0xFFFF_FFFF_FFFF_FFFF {
                let thread_ticks = current_thread.lock().unwrap().get_cpu_time();
                thread_ticks.wrapping_add(elapsed) as u64
            } else if same_thread && info_sub_id == current_core {
                elapsed as u64
            } else {
                0
            };
            RESULT_SUCCESS
        }

        InfoType::IdleTickCount => {
            if handle != INVALID_HANDLE {
                return RESULT_INVALID_HANDLE;
            }

            let Some(kernel) = system.kernel() else {
                return RESULT_INVALID_COMBINATION;
            };
            let current_core = kernel.current_physical_core_index() as u64;
            let core_valid = info_sub_id == 0xFFFF_FFFF_FFFF_FFFF || info_sub_id == current_core;
            if !core_valid {
                return RESULT_INVALID_COMBINATION;
            }

            let scheduler_arc = kernel
                .current_scheduler()
                .cloned()
                .unwrap_or_else(|| system.scheduler_arc());
            let scheduler = scheduler_arc.lock().unwrap();
            *result = scheduler
                .idle_thread
                .as_ref()
                .and_then(std::sync::Weak::upgrade)
                .map(|idle| idle.lock().unwrap().get_cpu_time() as u64)
                .unwrap_or(0);
            RESULT_SUCCESS
        }

        InfoType::Unknown37 | InfoType::Unknown38 => {
            log::warn!(
                "(STUBBED) svc::GetInfo called, info_id={:#x}, info_sub_id={:#x}, handle={:#08x}",
                info_id_type as u32,
                info_sub_id,
                handle
            );
            *result = 0;
            RESULT_SUCCESS
        }

        InfoType::MesosphereCurrentProcess => {
            if handle != INVALID_HANDLE {
                return RESULT_INVALID_HANDLE;
            }
            if info_sub_id != 0 {
                return RESULT_INVALID_COMBINATION;
            }

            // Upstream: Add current process to its own handle table, return the new handle.
            let process_arc = system.current_process_arc();
            let mut process = process_arc.lock().unwrap();
            let process_id = process.get_process_id();
            match process.handle_table.add(process_id) {
                Ok(h) => {
                    *result = h as u64;
                }
                Err(_) => {
                    *result = 0;
                    return RESULT_OUT_OF_HANDLES;
                }
            }
            RESULT_SUCCESS
        }

        _ => {
            log::error!("Unimplemented svcGetInfo id=0x{:X}", info_id_type as u32);
            RESULT_INVALID_ENUM_VALUE
        }
    }
}

/// Gets kernel-wide memory-pool sizes or the privileged process-ID range.
pub fn get_system_info(
    system: &System,
    out: &mut u64,
    info_type: SystemInfoType,
    handle: Handle,
    info_subtype: u64,
) -> ResultCode {
    if handle != 0 {
        return RESULT_INVALID_HANDLE;
    }

    match info_type {
        SystemInfoType::TotalPhysicalMemorySize | SystemInfoType::UsedPhysicalMemorySize => {
            if info_subtype > 3 {
                return RESULT_INVALID_COMBINATION;
            }
            let Some(kernel) = system.kernel() else {
                return RESULT_INVALID_STATE;
            };
            let pool = match info_subtype {
                0 => crate::hle::kernel::k_memory_manager::Pool::Application,
                1 => crate::hle::kernel::k_memory_manager::Pool::Applet,
                2 => crate::hle::kernel::k_memory_manager::Pool::System,
                3 => crate::hle::kernel::k_memory_manager::Pool::SystemNonSecure,
                _ => unreachable!(),
            };
            let memory_manager = kernel.memory_manager();
            let total = memory_manager.get_size(pool);
            *out = match info_type {
                SystemInfoType::TotalPhysicalMemorySize => total as u64,
                SystemInfoType::UsedPhysicalMemorySize => {
                    (total - memory_manager.get_free_size(pool)) as u64
                }
                SystemInfoType::InitialProcessIdRange => unreachable!(),
            };
        }
        SystemInfoType::InitialProcessIdRange => {
            if info_subtype > 1 {
                return RESULT_INVALID_COMBINATION;
            }
            *out = match info_subtype {
                0 => 1,
                1 => 8,
                _ => unreachable!(),
            };
        }
    }

    RESULT_SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::System;
    use crate::hle::kernel::k_process::KProcess;
    use crate::hle::kernel::k_thread::KThread;
    use std::sync::{Arc, Mutex};

    fn test_system() -> System {
        let mut system = System::new_for_test();

        let process = Arc::new(crate::hle::kernel::k_process::ProcessLock::from_value(
            KProcess::new(),
        ));
        {
            let mut process_guard = process.lock().unwrap();
            process_guard.process_id = 1;
            process_guard.initialize_handle_table();
        }

        let current_thread = Arc::new(crate::hle::kernel::k_thread::KThreadLock::new(
            KThread::new(),
        ));
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

    fn test_system_with_thread_and_idle() -> (
        System,
        Arc<crate::hle::kernel::k_thread::KThreadLock>,
        Arc<crate::hle::kernel::k_thread::KThreadLock>,
        Handle,
    ) {
        let mut system = System::new_for_test();

        let process = Arc::new(crate::hle::kernel::k_process::ProcessLock::from_value(
            KProcess::new(),
        ));
        {
            let mut process_guard = process.lock().unwrap();
            process_guard.process_id = 1;
            process_guard.initialize_handle_table();
        }

        let current_thread = Arc::new(crate::hle::kernel::k_thread::KThreadLock::new(
            KThread::new(),
        ));
        {
            let mut thread = current_thread.lock().unwrap();
            thread.thread_id = 1;
            thread.object_id = 1;
            thread.add_cpu_time(0, 100);
        }
        process
            .lock()
            .unwrap()
            .register_thread_object(current_thread.clone());
        let thread_handle = process.lock().unwrap().handle_table.add(1).unwrap();

        let idle_thread = Arc::new(crate::hle::kernel::k_thread::KThreadLock::new(
            KThread::new(),
        ));
        {
            let mut thread = idle_thread.lock().unwrap();
            thread.thread_id = 2;
            thread.object_id = 2;
            thread.add_cpu_time(0, 55);
        }

        let scheduler = Arc::new(Mutex::new(
            crate::hle::kernel::k_scheduler::KScheduler::new(0),
        ));
        {
            let mut scheduler_guard = scheduler.lock().unwrap();
            scheduler_guard.initialize_with_threads(&current_thread, &idle_thread, 0);
            scheduler_guard.core_timing = Some(Arc::new(crate::core_timing::CoreTiming::new()));
            scheduler_guard.core_timing.as_ref().unwrap().add_ticks(10);
            scheduler_guard.last_context_switch_time = 0;
        }

        let shared_memory = process.lock().unwrap().get_shared_memory();
        system.set_current_process_arc(process);
        system.set_scheduler_arc(scheduler);
        system.set_shared_process_memory(shared_memory);
        system.set_runtime_program_id(1);
        system.set_runtime_64bit(false);
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&current_thread));

        (system, current_thread, idle_thread, thread_handle)
    }

    #[test]
    fn alias_region_extra_size_returns_zero() {
        let system = test_system();
        let mut result = u64::MAX;

        assert_eq!(
            get_info(&system, &mut result, InfoType::AliasRegionExtraSize, 0, 0),
            RESULT_SUCCESS
        );
        assert_eq!(result, 0);
    }

    #[test]
    fn unknown_37_and_38_return_zero() {
        let system = test_system();
        for info in [InfoType::Unknown37, InfoType::Unknown38] {
            let mut result = u64::MAX;
            assert_eq!(get_info(&system, &mut result, info, 0, 0), RESULT_SUCCESS);
            assert_eq!(result, 0);
        }
    }

    #[test]
    fn system_info_reports_total_and_used_pool_memory() {
        use crate::hle::kernel::k_memory_block::PAGE_SIZE;
        use crate::hle::kernel::k_memory_manager::{Direction, KMemoryManager, Pool};

        let mut system = test_system();
        let manager = system.kernel_mut().unwrap().memory_manager_mut();
        manager.initialize_pool(Pool::Application, 0x1_0000_0000, 16 * PAGE_SIZE);
        let option = KMemoryManager::encode_option(Pool::Application, Direction::FromFront);
        assert_ne!(manager.allocate_and_open_continuous(3, 1, option), 0);

        let mut result = u64::MAX;
        assert_eq!(
            get_system_info(
                &system,
                &mut result,
                SystemInfoType::TotalPhysicalMemorySize,
                0,
                Pool::Application as u64,
            ),
            RESULT_SUCCESS
        );
        assert_eq!(result, (16 * PAGE_SIZE) as u64);

        assert_eq!(
            get_system_info(
                &system,
                &mut result,
                SystemInfoType::UsedPhysicalMemorySize,
                0,
                Pool::Application as u64,
            ),
            RESULT_SUCCESS
        );
        assert_eq!(result, (3 * PAGE_SIZE) as u64);
    }

    #[test]
    fn system_info_validates_arguments_and_reports_privileged_id_range() {
        let system = test_system();
        let mut result = u64::MAX;

        assert_eq!(
            get_system_info(
                &system,
                &mut result,
                SystemInfoType::InitialProcessIdRange,
                1,
                0,
            ),
            RESULT_INVALID_HANDLE
        );
        assert_eq!(
            get_system_info(
                &system,
                &mut result,
                SystemInfoType::TotalPhysicalMemorySize,
                0,
                4,
            ),
            RESULT_INVALID_COMBINATION
        );
        assert_eq!(
            get_system_info(
                &system,
                &mut result,
                SystemInfoType::InitialProcessIdRange,
                0,
                0,
            ),
            RESULT_SUCCESS
        );
        assert_eq!(result, 1);
        assert_eq!(
            get_system_info(
                &system,
                &mut result,
                SystemInfoType::InitialProcessIdRange,
                0,
                1,
            ),
            RESULT_SUCCESS
        );
        assert_eq!(result, 8);
        assert!(SystemInfoType::from_u32(3).is_none());
    }

    #[test]
    fn tick_count_info_matches_current_thread_and_idle_cpu_time() {
        let (system, _current_thread, _idle_thread, thread_handle) =
            test_system_with_thread_and_idle();
        let mut result = u64::MAX;

        assert_eq!(
            get_info(
                &system,
                &mut result,
                InfoType::ThreadTickCount,
                thread_handle,
                0xFFFF_FFFF_FFFF_FFFF,
            ),
            RESULT_SUCCESS
        );
        assert!(result >= 100);

        result = u64::MAX;
        assert_eq!(
            get_info(
                &system,
                &mut result,
                InfoType::IdleTickCount,
                INVALID_HANDLE,
                0,
            ),
            RESULT_SUCCESS
        );
        assert_eq!(result, 55);

        crate::hle::kernel::kernel::set_current_emu_thread(None);
    }
}
