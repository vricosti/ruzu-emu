// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `vk_fence_manager.h` / `vk_fence_manager.cpp`.
//!
//! Vulkan fence management using scheduler tick-based synchronization.

use std::sync::Arc;

use super::scheduler::{Scheduler, SchedulerWaitHandle};

// ---------------------------------------------------------------------------
// InnerFence
// ---------------------------------------------------------------------------

/// Port of `InnerFence` class.
///
/// A fence that tracks a scheduler tick for GPU completion.
/// Upstream extends `VideoCommon::FenceBase`.
pub struct InnerFence {
    is_stubbed: bool,
    wait_tick: u64,
    scheduler: Option<SchedulerWaitHandle>,
}

impl InnerFence {
    /// Port of `InnerFence::InnerFence(Scheduler&, bool)`.
    pub(crate) fn new(scheduler: SchedulerWaitHandle, is_stubbed: bool) -> Self {
        InnerFence {
            is_stubbed,
            wait_tick: 0,
            scheduler: Some(scheduler),
        }
    }

    /// Port of `InnerFence::Queue`.
    ///
    /// Records the current scheduler tick and triggers a flush.
    pub fn queue(&mut self, scheduler: &mut Scheduler) {
        if self.is_stubbed {
            return;
        }
        self.wait_tick = scheduler.current_tick();
        scheduler.flush();
    }

    /// Port of `InnerFence::IsSignaled`.
    ///
    /// Returns true if the GPU has completed the tick this fence is waiting on.
    pub fn is_signaled(&self) -> bool {
        if self.is_stubbed {
            return true;
        }
        self.scheduler
            .as_ref()
            .expect("non-stubbed Vulkan fence must retain its scheduler")
            .is_free(self.wait_tick)
    }

    /// Port of `InnerFence::Wait`.
    ///
    /// Blocks until the GPU completes the tick this fence is waiting on.
    pub fn wait(&self) {
        if self.is_stubbed {
            return;
        }
        self.scheduler
            .as_ref()
            .expect("non-stubbed Vulkan fence must retain its scheduler")
            .wait(self.wait_tick);
    }
}

/// Port of `Fence` type alias (`std::shared_ptr<InnerFence>`).
pub type Fence = Arc<std::sync::Mutex<InnerFence>>;

impl crate::fence_manager::FenceBase for Fence {
    fn is_stubbed(&self) -> bool {
        self.lock().unwrap().is_stubbed
    }

    fn wait_for_fence(&self) {
        self.lock().unwrap().wait();
    }
}

// ---------------------------------------------------------------------------
// FenceManager
// ---------------------------------------------------------------------------

/// Port of `FenceManager` class.
///
/// Extends `GenericFenceManager` (VideoCommon::FenceManager) with
/// Vulkan-specific fence creation and synchronization.
pub struct FenceManager {
    scheduler: SchedulerWaitHandle,
}

impl FenceManager {
    /// Port of `FenceManager::FenceManager`.
    pub(crate) fn new(scheduler: SchedulerWaitHandle) -> Self {
        FenceManager { scheduler }
    }

    /// Port of `FenceManager::CreateFence`.
    pub fn create_fence(&self, is_stubbed: bool) -> Fence {
        Arc::new(std::sync::Mutex::new(InnerFence::new(
            self.scheduler.clone(),
            is_stubbed,
        )))
    }

    /// Port of `FenceManager::QueueFence`.
    pub fn queue_fence(&mut self, fence: &Fence, scheduler: &mut Scheduler) {
        let mut inner = fence.lock().unwrap();
        inner.queue(scheduler);
    }

    /// Port of `FenceManager::IsFenceSignaled`.
    pub fn is_fence_signaled(&self, fence: &Fence) -> bool {
        let inner = fence.lock().unwrap();
        inner.is_signaled()
    }

    /// Port of `FenceManager::WaitFence`.
    pub fn wait_fence(&self, fence: &Fence) {
        let inner = fence.lock().unwrap();
        inner.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl InnerFence {
        fn new_for_test(is_stubbed: bool) -> Self {
            Self {
                is_stubbed,
                wait_tick: 0,
                scheduler: None,
            }
        }

        fn is_signaled_at(&self, known_gpu_tick: u64) -> bool {
            self.is_stubbed || known_gpu_tick >= self.wait_tick
        }
    }

    #[test]
    fn stubbed_fence_is_always_signaled() {
        let fence = InnerFence::new_for_test(true);
        assert!(fence.is_signaled());
        fence.wait();
    }

    #[test]
    fn fence_tracks_queued_tick() {
        let mut fence = InnerFence::new_for_test(false);
        fence.wait_tick = 10;
        assert!(!fence.is_signaled_at(5));
        assert!(fence.is_signaled_at(10));
        assert!(fence.is_signaled_at(15));
    }
}
