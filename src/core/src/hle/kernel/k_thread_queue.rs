//! Port of zuyu/src/core/hle/kernel/k_thread_queue.h / k_thread_queue.cpp
//! Status: Partial (structural port)
//! Derniere synchro: 2026-03-11
//!
//! KThreadQueue and KThreadQueueWithoutEndWait: thread wait queue abstractions.

use std::sync::{Arc, Weak};

use super::k_hardware_timer::KHardwareTimer;
use super::k_thread::{KThread, KThreadLock};

/// Rust representation of a derived `KThreadQueue::CancelWait` override.
///
/// The boolean selects whether the base implementation must run. Eden's
/// light-condition-variable queue returns without invoking the base method for
/// an allowed termination request, so a plain function pointer without the
/// result and cancellation arguments cannot represent the upstream contract.
pub type CancelWaitCallback = Arc<dyn Fn(&mut KThread, u32, bool) -> bool + Send + Sync + 'static>;

/// Base KThreadQueue holding a reference to the kernel and an optional hardware timer.
/// Matches upstream `KThreadQueue` (k_thread_queue.h).
#[derive(Clone)]
pub struct KThreadQueue {
    // In upstream: KernelCore& m_kernel; KHardwareTimer* m_hardware_timer;
    pub hardware_timer: Option<Arc<KHardwareTimer>>,
    pub end_wait_allowed: bool,
    pub notify_available_impl: Option<fn(&KThreadQueue, &mut KThread, *const super::k_synchronization_object::SynchronizationObjectState, u32) -> bool>,
    pub cancel_wait_impl: Option<CancelWaitCallback>,
    pub pinned_wait_owner: Option<Weak<KThreadLock>>,
}

impl KThreadQueue {
    pub fn new() -> Self {
        Self {
            hardware_timer: None,
            end_wait_allowed: true,
            notify_available_impl: None,
            cancel_wait_impl: None,
            pinned_wait_owner: None,
        }
    }

    pub fn with_callbacks(
        notify_available_impl: Option<fn(&KThreadQueue, &mut KThread, *const super::k_synchronization_object::SynchronizationObjectState, u32) -> bool>,
        cancel_wait_impl: Option<fn(&mut KThread)>,
    ) -> Self {
        let cancel_wait_impl = cancel_wait_impl.map(|callback| {
            Arc::new(
                move |thread: &mut KThread, _wait_result: u32, _cancel_timer_task: bool| {
                    callback(thread);
                    true
                },
            ) as CancelWaitCallback
        });
        Self::with_cancel_wait_callback(notify_available_impl, cancel_wait_impl)
    }

    /// Construct a queue with a stateful derived cancellation override.
    /// This is the Rust equivalent of a C++ derived queue retaining pointers
    /// to its owning wait structure.
    pub fn with_cancel_wait_callback(
        notify_available_impl: Option<fn(&KThreadQueue, &mut KThread, *const super::k_synchronization_object::SynchronizationObjectState, u32) -> bool>,
        cancel_wait_impl: Option<CancelWaitCallback>,
    ) -> Self {
        Self {
            hardware_timer: None,
            end_wait_allowed: true,
            notify_available_impl,
            cancel_wait_impl,
            pinned_wait_owner: None,
        }
    }

    pub fn without_end_wait(
        notify_available_impl: Option<fn(&KThreadQueue, &mut KThread, *const super::k_synchronization_object::SynchronizationObjectState, u32) -> bool>,
        cancel_wait_impl: Option<fn(&mut KThread)>,
    ) -> Self {
        let mut queue = Self::with_callbacks(notify_available_impl, cancel_wait_impl);
        queue.end_wait_allowed = false;
        queue
    }

    pub fn without_end_wait_callback(
        notify_available_impl: Option<fn(&KThreadQueue, &mut KThread, *const super::k_synchronization_object::SynchronizationObjectState, u32) -> bool>,
        cancel_wait_impl: Option<CancelWaitCallback>,
    ) -> Self {
        Self {
            hardware_timer: None,
            end_wait_allowed: false,
            notify_available_impl,
            cancel_wait_impl,
            pinned_wait_owner: None,
        }
    }

    pub fn set_hardware_timer(&mut self, hardware_timer: Arc<KHardwareTimer>) {
        self.hardware_timer = Some(hardware_timer);
    }

    /// Upstream: virtual NotifyAvailable is UNREACHABLE in base KThreadQueue.
    /// Derived queues override it. We use function pointers instead.
    pub fn notify_available(
        &self,
        thread: &mut KThread,
        signaled_object: *const super::k_synchronization_object::SynchronizationObjectState,
        wait_result: u32,
    ) -> bool {
        if thread.get_state() != super::k_thread::ThreadState::WAITING {
            return false;
        }

        if let Some(notify_impl) = self.notify_available_impl {
            notify_impl(self, thread, signaled_object, wait_result)
        } else {
            // Base KThreadQueue::NotifyAvailable is UNREACHABLE in upstream.
            // If we reach here, a queue was used without a notify_available impl.
            unreachable!("KThreadQueue::NotifyAvailable called on base queue without override");
        }
    }

    pub fn base_end_wait(&self, thread: &mut KThread, wait_result: u32) {
        thread.wait_result = wait_result;
        thread.set_state(super::k_thread::ThreadState::RUNNABLE);
        thread.clear_wait_queue();

        if let (Some(hardware_timer), Some(thread_arc)) = (
            self.hardware_timer.as_ref(),
            thread
                .self_reference
                .as_ref()
                .and_then(std::sync::Weak::upgrade),
        ) {
            let thread_id = thread.get_thread_id();
            let task_time = thread.get_timer_task_time();
            thread.set_timer_task_time(0);
            drop(thread_arc);
            hardware_timer.cancel_task_by_id(thread_id, task_time);
        }

        // Unpark the host thread that is blocked in begin_wait.
        thread.unpark_wait();
    }

    pub fn end_wait(&self, thread: &mut KThread, wait_result: u32) {
        if !self.end_wait_allowed {
            // Upstream's KThreadQueueWithoutEndWait::EndWait is [[noreturn]]
            // and calls UNREACHABLE(). Hitting this in ruzu indicates a real
            // bug — a thread waiting on a sync-object (without_end_wait) queue
            // got woken via the wrong path. Log the context, but do NOT clear
            // the wait: doing so resumes a thread from the wrong wait object
            // and can make it re-enter userspace with a stale TLS reply buffer
            // fresh request). Upstream would abort here; ruzu keeps running by
            // preserving the original wait.
            log::error!(
                "[KTHREAD_QUEUE_WITHOUT_END_WAIT] thread_id={} wait_result=0x{:X} state={:?} wait_reason={:?} addr_key=0x{:X}",
                thread.get_thread_id(),
                wait_result,
                thread.get_state(),
                thread.wait_reason_for_debugging,
                thread.address_key.get(),
            );
            if std::env::var_os("RUZU_PANIC_ON_WITHOUT_END_WAIT").is_some() {
                panic!("KThreadQueueWithoutEndWait::end_wait should never be called");
            }
            return;
        }
        self.base_end_wait(thread, wait_result);
    }

    pub fn cancel_wait(&self, thread: &mut KThread, wait_result: u32, cancel_timer_task: bool) {
        if let Some(owner) = self.pinned_wait_owner.as_ref().and_then(Weak::upgrade) {
            owner
                .lock()
                .unwrap()
                .pinned_waiter_list
                .retain(|thread_id| *thread_id != thread.get_thread_id());
        }

        if let Some(cancel_impl) = self.cancel_wait_impl.as_ref() {
            if !cancel_impl(thread, wait_result, cancel_timer_task) {
                return;
            }
        }

        thread.wait_result = wait_result;
        thread.set_state(super::k_thread::ThreadState::RUNNABLE);
        thread.clear_wait_queue();

        if cancel_timer_task {
            if let (Some(hardware_timer), Some(thread_arc)) = (
                self.hardware_timer.as_ref(),
                thread
                    .self_reference
                    .as_ref()
                    .and_then(std::sync::Weak::upgrade),
            ) {
                let thread_id = thread.get_thread_id();
                let task_time = thread.get_timer_task_time();
                thread.set_timer_task_time(0);
                drop(thread_arc);
                hardware_timer.cancel_task_by_id(thread_id, task_time);
            }
        }

        // Unpark the host thread that is blocked in begin_wait.
        thread.unpark_wait();
    }
}

/// KThreadQueueWithoutEndWait: a queue that panics if EndWait is called.
/// Matches upstream `KThreadQueueWithoutEndWait` (k_thread_queue.h).
pub struct KThreadQueueWithoutEndWait {
    pub base: KThreadQueue,
}

impl KThreadQueueWithoutEndWait {
    pub fn new() -> Self {
        Self {
            base: KThreadQueue {
                hardware_timer: None,
                end_wait_allowed: false,
                notify_available_impl: None,
                cancel_wait_impl: None,
                pinned_wait_owner: None,
            },
        }
    }

    pub fn with_callbacks(
        notify_available_impl: Option<fn(&KThreadQueue, &mut KThread, *const super::k_synchronization_object::SynchronizationObjectState, u32) -> bool>,
        cancel_wait_impl: Option<fn(&mut KThread)>,
    ) -> Self {
        Self {
            base: KThreadQueue::without_end_wait(notify_available_impl, cancel_wait_impl),
        }
    }
}

impl Default for KThreadQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for KThreadQueueWithoutEndWait {
    fn default() -> Self {
        Self::new()
    }
}

impl KThreadQueueWithoutEndWait {
    pub fn end_wait(&self, _waiting_thread: &mut KThread, _wait_result: u32) {
        // Upstream: ASSERT(false) — should never be called.
        panic!("KThreadQueueWithoutEndWait::end_wait should never be called");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::kernel::k_thread::ThreadState;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn derived_cancel_wait_can_skip_the_base_transition() {
        let callback_ran = Arc::new(AtomicBool::new(false));
        let callback_state = Arc::clone(&callback_ran);
        let callback: CancelWaitCallback = Arc::new(move |_, _, _| {
            callback_state.store(true, Ordering::Release);
            false
        });
        let queue = KThreadQueue::with_cancel_wait_callback(None, Some(callback));
        let mut thread = KThread::new();
        thread.set_state(ThreadState::WAITING);

        queue.cancel_wait(&mut thread, 0xDEAD, true);

        assert!(callback_ran.load(Ordering::Acquire));
        assert_eq!(thread.get_state(), ThreadState::WAITING);
        assert_ne!(thread.get_wait_result(), 0xDEAD);
    }
}
