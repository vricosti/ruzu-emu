//! Port of zuyu/src/core/hle/kernel/k_interrupt_manager.h/.cpp
//! Status: COMPLET
//! Derniere synchro: 2026-03-20
//!
//! KInterruptManager: namespace-level functions for handling inter-processor
//! interrupts and scheduling interrupts. Upstream is a namespace, not a class.

use crate::hardware_properties;

use super::kernel::KernelCore;

/// Handle an interrupt on the given core.
///
/// Upstream: `KInterruptManager::HandleInterrupt(KernelCore&, s32)` (k_interrupt_manager.cpp:13-34).
///
/// 1. Acknowledge the interrupt by clearing it on the physical core.
/// 2. If the current thread has a non-zero user disable count and is not already
///    pinned, pin it and set the interrupt flag in the thread's TLS.
/// 3. Request rescheduling on the current scheduler.
pub fn handle_interrupt(kernel: &KernelCore, core_id: i32) {
    // Acknowledge the interrupt.
    // Upstream: kernel.PhysicalCore(core_id).ClearInterrupt();
    if let Some(physical_core) = kernel.physical_core(core_id as usize) {
        physical_core.clear_interrupt();
    }

    // Get the current thread.
    // Upstream: auto& current_thread = GetCurrentThread(kernel);
    let current_thread = super::kernel::get_current_emu_thread();

    if let Some(ref thread_arc) = current_thread {
        // Snapshot KThread fields and drop the guard before taking
        // scheduler lock + KProcess lock. Enforces lock order
        // scheduler → process → thread, matching upstream.
        let snapshot = {
            let thread = thread_arc.lock().unwrap();
            let parent_arc = thread.parent.as_ref().and_then(|w| w.upgrade());
            parent_arc.map(|p| {
                (
                    p,
                    thread.get_user_disable_count(),
                    thread.get_thread_id(),
                    thread.is_termination_requested(),
                    thread.scheduler_lock_ptr,
                )
            })
        };

        if let Some((
            parent_arc,
            user_disable_count,
            thread_id,
            is_termination_requested,
            scheduler_lock_ptr,
        )) = snapshot
        {
            if user_disable_count != 0 {
                let already_pinned = {
                    let process = parent_arc.lock().unwrap();
                    process.get_pinned_thread(core_id).is_some()
                };

                if !already_pinned {
                    // Upstream: `KScopedSchedulerLock sl{kernel};` wraps
                    // the pin operation in k_interrupt_manager.cpp. We
                    // take the matching guard via the current thread's
                    // `scheduler_lock_ptr` (populated at thread init to
                    // `&GlobalSchedulerContext::m_scheduler_lock`).
                    //
                    // The scheduler lock is reentrant for the current
                    // host thread (see `KAbstractSchedulerLock::lock`).
                    //
                    // SAFETY: `scheduler_lock_ptr` is the address of a
                    // `KAbstractSchedulerLock` stored in the GSC, which
                    // outlives every KThread belonging to that kernel.
                    let _sl = if scheduler_lock_ptr != 0 {
                        Some(super::k_scheduler_lock::KScopedSchedulerLock::new(unsafe {
                            &*(scheduler_lock_ptr
                                as *const super::k_scheduler_lock::KAbstractSchedulerLock)
                        }))
                    } else {
                        None
                    };

                    // Upstream: process->PinCurrentThread();
                    {
                        let mut process = parent_arc.lock().unwrap();
                        process.pin_current_thread(core_id, thread_id, is_termination_requested);
                    }

                    // Upstream: cur_thread->Pin(core_id)
                    {
                        let mut thread = thread_arc.lock().unwrap();
                        thread.pin(core_id);
                    }

                    // Upstream: GetCurrentThread(kernel).SetInterruptFlag();
                    {
                        let thread = thread_arc.lock().unwrap();
                        thread.set_interrupt_flag();
                    }
                    // `_sl` drops here, releasing the scheduler lock.
                }
            }
        }
    }

    // Request interrupt scheduling.
    // Upstream: kernel.CurrentScheduler()->RequestScheduleOnInterrupt();
    if let Some(scheduler) = kernel.current_scheduler() {
        scheduler.lock().unwrap().request_schedule_on_interrupt();
    }
}

/// Send an inter-processor interrupt to the cores indicated by the bitmask.
///
/// Upstream: `KInterruptManager::SendInterProcessorInterrupt(KernelCore&, u64)`
/// (k_interrupt_manager.cpp:36-42).
pub fn send_inter_processor_interrupt(kernel: &KernelCore, core_mask: u64) {
    for core_id in 0..hardware_properties::NUM_CPU_CORES as usize {
        if core_mask & (1u64 << core_id) != 0 {
            if let Some(physical_core) = kernel.physical_core(core_id) {
                physical_core.interrupt();
            }
        }
    }
}
