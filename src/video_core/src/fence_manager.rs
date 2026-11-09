// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `video_core/fence_manager.h`.

use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Condvar, Mutex,
};
use std::thread::{self, JoinHandle};

use common::settings;
use common::thread::{set_current_thread_name, set_current_thread_priority, ThreadPriority};

use crate::delayed_destruction_ring::DelayedDestructionRing;

pub const TEXTURE_FLUSHES: u8 = 1;
pub const BUFFER_FLUSHES: u8 = 2;

/// Upstream FenceManager::CacheFlushes: submitted cache queues for one fence.
#[derive(Clone, Copy, Debug, Default)]
pub struct CacheFlushes {
    pub should_flush: bool,
    pub committed: u8,
}

type Operation = Box<dyn FnOnce() + Send + 'static>;

/// Base trait for fence objects.
pub trait FenceBase {
    /// Returns true if this fence is stubbed (no actual GPU wait needed).
    fn is_stubbed(&self) -> bool;

    /// Wait until the backend fence completes.
    fn wait_for_fence(&self);
}

struct PendingFence<F: FenceBase + Send + 'static> {
    fence: F,
    committed_flushes: u8,
    pre_operations: VecDeque<Operation>,
    operations: VecDeque<Operation>,
}

struct FenceManagerState<F: FenceBase + Send + 'static> {
    fences: VecDeque<PendingFence<F>>,
    uncommitted_operations: VecDeque<Operation>,
}

struct FenceManagerShared<F: FenceBase + Send + 'static> {
    state: Mutex<FenceManagerState<F>>,
    ring_guard: Mutex<DelayedDestructionRing<F, 8>>,
    cv: Condvar,
    stop_requested: AtomicBool,
}

/// Generic fence manager that coordinates fence lifecycle, async flushes,
/// and deferred operations.
///
/// The upstream C++ uses CRTP templates; here we use a generic owner with
/// callback parameters supplied by the owning rasterizer.
pub struct FenceManager<F: FenceBase + Send + 'static> {
    shared: Arc<FenceManagerShared<F>>,
    has_async_check: bool,
    fence_thread: Option<JoinHandle<()>>,
    skipped_fences: u32,
}

impl<F: FenceBase + Send + 'static> FenceManager<F> {
    pub fn new(has_async_check: bool) -> Self {
        let shared = Arc::new(FenceManagerShared {
            state: Mutex::new(FenceManagerState {
                fences: VecDeque::new(),
                uncommitted_operations: VecDeque::new(),
            }),
            ring_guard: Mutex::new(DelayedDestructionRing::new()),
            cv: Condvar::new(),
            stop_requested: AtomicBool::new(false),
        });
        let fence_thread = if has_async_check {
            let shared = Arc::clone(&shared);
            Some(
                thread::Builder::new()
                    .name("GPUFencingThread".to_string())
                    .spawn(move || release_thread_func(shared))
                    .expect("failed to spawn GPUFencingThread"),
            )
        } else {
            None
        };
        Self {
            shared,
            has_async_check,
            fence_thread,
            skipped_fences: 0,
        }
    }

    /// Notify the fence manager about a new frame.
    pub fn tick_frame(&mut self) {
        let mut ring = self.shared.ring_guard.lock().unwrap();
        ring.tick();
    }

    /// Port of `FenceManager::SignalOrdering()`.
    pub fn signal_ordering<FS, FSW, FPF, FAF>(
        &mut self,
        mut should_wait_async_flushes: FSW,
        mut is_fence_signaled: FS,
        mut pop_async_flushes: FPF,
        mut accumulate_flushes: FAF,
    ) where
        FSW: FnMut() -> bool,
        FS: FnMut(&F) -> bool,
        FPF: FnMut(u8) + Send + 'static,
        FAF: FnMut(),
    {
        if !self.has_async_check {
            self.try_release_pending_fences(false, |pending_fence, force_wait| {
                try_release_fence(
                    pending_fence,
                    force_wait,
                    &mut should_wait_async_flushes,
                    &mut is_fence_signaled,
                    None::<&mut fn(&F)>,
                    &mut pop_async_flushes,
                )
            });
        }
        accumulate_flushes();
    }

    /// Port of `FenceManager::SignalReference()`.
    pub fn signal_reference<FC, FQ, FSW, FIS, FPF, FSHF, FCAF, FFL, FINV, FPW>(
        &mut self,
        create_fence: FC,
        queue_fence: FQ,
        should_wait_async_flushes: FSW,
        is_fence_signaled: FIS,
        pop_async_flushes: FPF,
        commit_cache_flushes: FSHF,
        commit_async_flushes: FCAF,
        flush_commands: FFL,
        invalidate_gpu_cache: FINV,
        has_pending_fence_work: FPW,
    ) where
        FC: FnMut(bool) -> F,
        FQ: FnMut(&mut F),
        FSW: FnMut() -> bool,
        FIS: FnMut(&F) -> bool,
        FPF: FnMut(u8) + Send + 'static,
        FSHF: FnMut() -> CacheFlushes,
        FCAF: FnMut() -> bool,
        FFL: FnMut(),
        FINV: FnMut(),
        FPW: FnMut() -> bool,
    {
        self.signal_fence(
            Box::new(|| {}),
            create_fence,
            queue_fence,
            should_wait_async_flushes,
            is_fence_signaled,
            pop_async_flushes,
            commit_cache_flushes,
            commit_async_flushes,
            flush_commands,
            invalidate_gpu_cache,
            has_pending_fence_work,
        )
    }

    /// Port of upstream `SkipEmptyFence`. Backends without the query capability
    /// supply a predicate that always reports pending work.
    fn skip_empty_fence(&mut self, has_pending_fence_work: &mut impl FnMut() -> bool) -> bool {
        if self.skipped_fences >= 64 || has_pending_fence_work() {
            return false;
        }
        if !self.shared.state.lock().unwrap().uncommitted_operations.is_empty() {
            return false;
        }
        self.skipped_fences += 1;
        true
    }

    /// Port of `FenceManager::SyncOperation()`.
    pub fn sync_operation(&mut self, func: Operation) {
        let mut state = self.shared.state.lock().unwrap();
        state.uncommitted_operations.push_back(func);
    }

    /// Port of `FenceManager::SignalFence()`.
    pub fn signal_fence<FC, FQ, FSW, FIS, FPF, FSHF, FCAF, FFL, FINV, FPW>(
        &mut self,
        func: Operation,
        mut create_fence: FC,
        mut queue_fence: FQ,
        mut should_wait_async_flushes: FSW,
        mut is_fence_signaled: FIS,
        mut pop_async_flushes: FPF,
        mut commit_cache_flushes: FSHF,
        mut commit_async_flushes: FCAF,
        mut flush_commands: FFL,
        mut invalidate_gpu_cache: FINV,
        mut has_pending_fence_work: FPW,
    ) where
        FC: FnMut(bool) -> F,
        FQ: FnMut(&mut F),
        FSW: FnMut() -> bool,
        FIS: FnMut(&F) -> bool,
        FPF: FnMut(u8) + Send + 'static,
        FSHF: FnMut() -> CacheFlushes,
        FCAF: FnMut() -> bool,
        FFL: FnMut(),
        FINV: FnMut(),
        FPW: FnMut() -> bool,
    {
        let delay_fence = {
            let values = settings::values();
            if settings::is_gpu_fence_behavior_default(&values) {
                settings::is_gpu_level_high(&values)
            } else {
                settings::is_gpu_fence_behavior_balanced(&values)
                    || settings::is_gpu_fence_behavior_accurate(&values)
            }
        };

        if !self.has_async_check {
            self.try_release_pending_fences(false, |pending_fence, force_wait| {
                try_release_fence(
                    pending_fence,
                    force_wait,
                    &mut should_wait_async_flushes,
                    &mut is_fence_signaled,
                    None::<&mut fn(&F)>,
                    &mut pop_async_flushes,
                )
            });
        }

        let cache_flushes = commit_cache_flushes();
        let committed_flushes = cache_flushes.committed;
        if !delay_fence && committed_flushes == 0 && self.skip_empty_fence(&mut has_pending_fence_work) {
            let _scope = crate::nx2_profile::Scope::new(crate::nx2_profile::Zone::FenceSkipped);
            func();
            invalidate_gpu_cache();
            return;
        }
        self.skipped_fences = 0;
        // Query flushes are committed once per real fence, after the caches.
        let query_flush = commit_async_flushes();
        let should_flush_now = cache_flushes.should_flush || query_flush;

        let mut new_fence = create_fence(!should_flush_now);
        let mut maybe_func = Some(func);
        let mut pre_operations = VecDeque::new();
        if self.has_async_check {
            pre_operations.push_back(Box::new(move || pop_async_flushes(committed_flushes)) as Operation);
        }

        if self.has_async_check {
            let mut state = self.shared.state.lock().unwrap();
            if delay_fence {
                state.uncommitted_operations.push_back(
                    maybe_func
                        .take()
                        .expect("fence callback must be consumed once"),
                );
            }
            let operations = std::mem::take(&mut state.uncommitted_operations);
            queue_fence(&mut new_fence);
            if !delay_fence {
                maybe_func
                    .take()
                    .expect("fence callback must be consumed once")();
            }
            state.fences.push_back(PendingFence {
                fence: new_fence,
                committed_flushes,
                pre_operations,
                operations,
            });
            if should_flush_now {
                flush_commands();
            }
        } else {
            let operations = {
                let mut state = self.shared.state.lock().unwrap();
                if delay_fence {
                    state.uncommitted_operations.push_back(
                        maybe_func
                            .take()
                            .expect("fence callback must be consumed once"),
                    );
                }
                std::mem::take(&mut state.uncommitted_operations)
            };
            queue_fence(&mut new_fence);
            if !delay_fence {
                maybe_func
                    .take()
                    .expect("fence callback must be consumed once")();
            }
            self.shared
                .state
                .lock()
                .unwrap()
                .fences
                .push_back(PendingFence {
                    fence: new_fence,
                    committed_flushes,
                    pre_operations,
                    operations,
                });
            if should_flush_now {
                flush_commands();
            }
        }
        if self.has_async_check {
            self.shared.cv.notify_all();
        }
        invalidate_gpu_cache();
    }

    /// Port of `FenceManager::SignalSyncPoint()`.
    pub fn signal_sync_point<FG, FH, FC, FQ, FSW, FIS, FPF, FSHF, FCAF, FFL, FINV, FPW>(
        &mut self,
        value: u32,
        mut increment_guest: FG,
        mut increment_host: FH,
        create_fence: FC,
        queue_fence: FQ,
        should_wait_async_flushes: FSW,
        is_fence_signaled: FIS,
        pop_async_flushes: FPF,
        commit_cache_flushes: FSHF,
        commit_async_flushes: FCAF,
        flush_commands: FFL,
        invalidate_gpu_cache: FINV,
        has_pending_fence_work: FPW,
    ) where
        FG: FnMut(u32),
        FH: FnMut(u32) + Send + 'static,
        FC: FnMut(bool) -> F,
        FQ: FnMut(&mut F),
        FSW: FnMut() -> bool,
        FIS: FnMut(&F) -> bool,
        FPF: FnMut(u8) + Send + 'static,
        FSHF: FnMut() -> CacheFlushes,
        FCAF: FnMut() -> bool,
        FFL: FnMut(),
        FINV: FnMut(),
        FPW: FnMut() -> bool,
    {
        increment_guest(value);
        self.signal_fence(
            Box::new(move || increment_host(value)),
            create_fence,
            queue_fence,
            should_wait_async_flushes,
            is_fence_signaled,
            pop_async_flushes,
            commit_cache_flushes,
            commit_async_flushes,
            flush_commands,
            invalidate_gpu_cache,
            has_pending_fence_work,
        );
    }

    /// Port of `FenceManager::WaitPendingFences()`.
    pub fn wait_pending_fences<FC, FQ, FS, FW, FSW, FPF, FSHF, FCAF, FFL, FINV, FPW>(
        &mut self,
        force: bool,
        create_fence: FC,
        queue_fence: FQ,
        mut should_wait_async_flushes: FSW,
        mut is_fence_signaled: FS,
        mut wait_fence: FW,
        mut pop_async_flushes: FPF,
        commit_cache_flushes: FSHF,
        commit_async_flushes: FCAF,
        flush_commands: FFL,
        invalidate_gpu_cache: FINV,
        has_pending_fence_work: FPW,
    ) where
        FC: FnMut(bool) -> F,
        FQ: FnMut(&mut F),
        FS: FnMut(&F) -> bool,
        FW: FnMut(&F),
        FSW: FnMut() -> bool,
        FPF: FnMut(u8) + Send + 'static,
        FSHF: FnMut() -> CacheFlushes,
        FCAF: FnMut() -> bool,
        FFL: FnMut(),
        FINV: FnMut(),
        FPW: FnMut() -> bool,
    {
        if !self.has_async_check {
            // Eden instantiates `TryReleasePendingFences<true>()` for every
            // non-async backend; `force` only gates the async drain-fence path.
            self.try_release_pending_fences(true, |pending_fence, force_wait| {
                try_release_fence(
                    pending_fence,
                    force_wait,
                    &mut should_wait_async_flushes,
                    &mut is_fence_signaled,
                    Some(&mut wait_fence),
                    &mut pop_async_flushes,
                )
            });
            return;
        }

        if !force {
            return;
        }

        let wait_pair = Arc::new((Mutex::new(false), Condvar::new()));
        let wait_pair_for_callback = Arc::clone(&wait_pair);
        self.signal_fence(
            Box::new(move || {
                let (lock, cv) = &*wait_pair_for_callback;
                let mut finished = lock.lock().unwrap();
                *finished = true;
                cv.notify_all();
            }),
            create_fence,
            queue_fence,
            should_wait_async_flushes,
            is_fence_signaled,
            pop_async_flushes,
            commit_cache_flushes,
            commit_async_flushes,
            flush_commands,
            invalidate_gpu_cache,
            has_pending_fence_work,
        );

        // Diagnostic scope excludes SignalFence setup above, but includes the
        // host mutex acquisition and predicate wait for the drain callback.
        let _scope = crate::nx2_profile::Scope::new(crate::nx2_profile::Zone::FenceHostWait);
        let (lock, cv) = &*wait_pair;
        let mut finished = lock.lock().unwrap();
        while !*finished {
            finished = cv.wait(finished).unwrap();
        }
    }

    #[cfg(test)]
    pub(crate) fn queued_fence_count(&self) -> usize {
        self.shared.state.lock().unwrap().fences.len()
    }
}

impl<F: FenceBase + Send + 'static> Drop for FenceManager<F> {
    fn drop(&mut self) {
        if let Some(fence_thread) = self.fence_thread.take() {
            self.shared.stop_requested.store(true, Ordering::Relaxed);
            self.shared.cv.notify_all();
            let _ = fence_thread.join();
        }
    }
}

impl<F: FenceBase + Send + 'static> FenceManager<F> {
    fn try_release_pending_fences<FR>(&mut self, force_wait: bool, mut should_release: FR)
    where
        FR: FnMut(&PendingFence<F>, bool) -> bool,
    {
        loop {
            let should_pop = {
                let state = self.shared.state.lock().unwrap();
                let Some(pending_fence) = state.fences.front() else {
                    return;
                };
                should_release(pending_fence, force_wait)
            };
            if !should_pop {
                return;
            }

            let (pre_operations, operations) = {
                let mut state = self.shared.state.lock().unwrap();
                let pending_fence = state
                    .fences
                    .front_mut()
                    .expect("pending fence must exist while releasing");
                (
                    std::mem::take(&mut pending_fence.pre_operations),
                    std::mem::take(&mut pending_fence.operations),
                )
            };
            for operation in pre_operations {
                operation();
            }
            for operation in operations {
                operation();
            }
            let pending_fence = self
                .shared
                .state
                .lock()
                .unwrap()
                .fences
                .pop_front()
                .expect("pending fence must exist after releasing its operations");
            let mut ring = self.shared.ring_guard.lock().unwrap();
            ring.push(pending_fence.fence);
        }
    }
}

fn try_release_fence<F, FSW, FIS, FW, FPF>(
    pending_fence: &PendingFence<F>,
    force_wait: bool,
    should_wait_async_flushes: &mut FSW,
    is_fence_signaled: &mut FIS,
    mut wait_fence: Option<&mut FW>,
    pop_async_flushes: &mut FPF,
) -> bool
where
    F: FenceBase + Send + 'static,
    FSW: FnMut() -> bool,
    FIS: FnMut(&F) -> bool,
    FW: FnMut(&F),
    FPF: FnMut(u8),
{
    if should_wait_async_flushes() && !is_fence_signaled(&pending_fence.fence) {
        if force_wait {
            if let Some(wait_fence) = wait_fence.as_mut() {
                wait_fence(&pending_fence.fence);
            }
        } else {
            return false;
        }
    }
    pop_async_flushes(pending_fence.committed_flushes);
    true
}

fn release_thread_func<F: FenceBase + Send + 'static>(shared: Arc<FenceManagerShared<F>>) {
    set_current_thread_name("GPUFencingThread");
    set_current_thread_priority(ThreadPriority::High);

    loop {
        let mut state = shared.state.lock().unwrap();
        while !shared.stop_requested.load(Ordering::Relaxed) && state.fences.is_empty() {
            state = shared.cv.wait(state).unwrap();
        }
        if shared.stop_requested.load(Ordering::Relaxed) {
            return;
        }
        let pending_fence = state
            .fences
            .pop_front()
            .expect("fence queue must contain the signaled fence");
        drop(state);
        if !pending_fence.fence.is_stubbed() {
            pending_fence.fence.wait_for_fence();
        }
        for operation in pending_fence.pre_operations {
            operation();
        }
        for operation in pending_fence.operations {
            operation();
        }
        let mut ring = shared.ring_guard.lock().unwrap();
        ring.push(pending_fence.fence);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::settings_enums::GpuAccuracy;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct TestFence {
        stubbed: bool,
    }

    impl FenceBase for TestFence {
        fn is_stubbed(&self) -> bool {
            self.stubbed
        }

        fn wait_for_fence(&self) {}
    }

    fn wait_until(flag: &AtomicBool) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while !flag.load(Ordering::Relaxed) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for async callback"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn signal_ordering_accumulates_flushes_after_releasing_signaled_fences() {
        let mut manager = FenceManager::<TestFence>::new(false);
        let released = Arc::new(AtomicBool::new(false));
        let accumulated = Arc::new(AtomicBool::new(false));
        let popped = Arc::new(AtomicBool::new(false));

        {
            let mut state = manager.shared.state.lock().unwrap();
            state.fences.push_back(PendingFence {
                fence: TestFence { stubbed: false },
                committed_flushes: TEXTURE_FLUSHES | BUFFER_FLUSHES,
                pre_operations: VecDeque::new(),
                operations: VecDeque::from([Box::new({
                    let released = Arc::clone(&released);
                    move || {
                        released.store(true, Ordering::Relaxed);
                    }
                }) as Operation]),
            });
        }

        manager.signal_ordering(
            || false,
            |_| true,
            {
                let popped = Arc::clone(&popped);
                move |_| popped.store(true, Ordering::Relaxed)
            },
            {
                let accumulated = Arc::clone(&accumulated);
                move || accumulated.store(true, Ordering::Relaxed)
            },
        );

        assert!(released.load(Ordering::Relaxed));
        assert!(popped.load(Ordering::Relaxed));
        assert!(accumulated.load(Ordering::Relaxed));
        assert_eq!(manager.queued_fence_count(), 0);
    }

    #[test]
    fn signal_fence_commits_flush_and_invalidate_in_upstream_order() {
        let _gpu_accuracy = crate::test_support::GpuAccuracyGuard::set(GpuAccuracy::Low);

        let mut manager = FenceManager::<TestFence>::new(false);
        let committed = Arc::new(AtomicBool::new(false));
        let callback_hit = Arc::new(AtomicBool::new(false));
        let flushed = Arc::new(AtomicBool::new(false));
        let invalidated = Arc::new(AtomicBool::new(false));
        let previous_fence_released = Arc::new(AtomicBool::new(false));

        {
            let mut state = manager.shared.state.lock().unwrap();
            state.fences.push_back(PendingFence {
                fence: TestFence { stubbed: false },
                committed_flushes: TEXTURE_FLUSHES | BUFFER_FLUSHES,
                pre_operations: VecDeque::new(),
                operations: VecDeque::from([Box::new({
                    let previous_fence_released = Arc::clone(&previous_fence_released);
                    move || previous_fence_released.store(true, Ordering::Relaxed)
                }) as Operation]),
            });
        }

        manager.signal_fence(
            Box::new({
                let callback_hit = Arc::clone(&callback_hit);
                move || callback_hit.store(true, Ordering::Relaxed)
            }),
            |is_stubbed| TestFence {
                stubbed: is_stubbed,
            },
            |_| {},
            || false,
            |_| true,
            |_| {},
            {
                let previous_fence_released = Arc::clone(&previous_fence_released);
                move || {
                    assert!(
                        previous_fence_released.load(Ordering::Relaxed),
                        "upstream commits caches after releasing pending fences"
                    );
                    CacheFlushes { should_flush: true, committed: TEXTURE_FLUSHES }
                }
            },
            {
                let committed = Arc::clone(&committed);
                let previous_fence_released = Arc::clone(&previous_fence_released);
                move || {
                    assert!(previous_fence_released.load(Ordering::Relaxed));
                    committed.store(true, Ordering::Relaxed);
                    false
                }
            },
            {
                let flushed = Arc::clone(&flushed);
                move || flushed.store(true, Ordering::Relaxed)
            },
            {
                let invalidated = Arc::clone(&invalidated);
                move || invalidated.store(true, Ordering::Relaxed)
            },
            || true,
        );

        assert!(committed.load(Ordering::Relaxed));
        assert!(callback_hit.load(Ordering::Relaxed));
        assert!(flushed.load(Ordering::Relaxed));
        assert!(invalidated.load(Ordering::Relaxed));
        assert_eq!(manager.queued_fence_count(), 1);
    }

    #[test]
    fn async_signal_holds_guard_from_queue_through_publication() {
        let _gpu_accuracy = crate::test_support::GpuAccuracyGuard::set(GpuAccuracy::Low);

        let mut manager = FenceManager::<TestFence>::new(true);
        let shared = Arc::clone(&manager.shared);

        manager.signal_fence(
            Box::new(|| {}),
            |is_stubbed| TestFence {
                stubbed: is_stubbed,
            },
            move |_| {
                assert!(
                    shared.state.try_lock().is_err(),
                    "Eden holds guard while QueueFence publishes the fence"
                );
            },
            || false,
            |_| true,
            |_| {},
            CacheFlushes::default,
            || false,
            || {},
            || {},
            || true,
        );
    }

    #[test]
    fn non_async_release_keeps_fence_queued_until_operations_complete() {
        let mut manager = FenceManager::<TestFence>::new(false);
        manager
            .shared
            .state
            .lock()
            .unwrap()
            .fences
            .push_back(PendingFence {
                fence: TestFence { stubbed: false },
                committed_flushes: TEXTURE_FLUSHES | BUFFER_FLUSHES,
                pre_operations: VecDeque::new(),
                operations: VecDeque::from([Box::new(|| panic!("operation failed")) as Operation]),
            });

        let result = catch_unwind(AssertUnwindSafe(|| {
            manager.signal_ordering(|| false, |_| true, |_| {}, || {});
        }));

        assert!(result.is_err());
        let state = manager.shared.state.lock().unwrap();
        assert_eq!(state.fences.len(), 1);
        assert!(state.fences.front().unwrap().operations.is_empty());
    }

    #[test]
    fn signal_sync_point_increments_guest_then_host() {
        let _gpu_accuracy = crate::test_support::GpuAccuracyGuard::set(GpuAccuracy::Low);

        let mut manager = FenceManager::<TestFence>::new(false);
        let guest = Arc::new(AtomicU32::new(0));
        let host = Arc::new(AtomicU32::new(0));

        manager.signal_sync_point(
            7,
            {
                let guest = Arc::clone(&guest);
                move |value| guest.store(value, Ordering::Relaxed)
            },
            {
                let host = Arc::clone(&host);
                move |value| host.store(value, Ordering::Relaxed)
            },
            |is_stubbed| TestFence {
                stubbed: is_stubbed,
            },
            |_| {},
            || false,
            |_| true,
            |_| {},
            CacheFlushes::default,
            || false,
            || {},
            || {},
            || true,
        );

        assert_eq!(guest.load(Ordering::Relaxed), 7);
        assert_eq!(host.load(Ordering::Relaxed), 7);
    }

    #[test]
    fn stubbed_fence_still_runs_async_flush_waiter_path() {
        let mut manager = FenceManager::<TestFence>::new(false);
        let popped = Arc::new(AtomicBool::new(false));

        {
            let mut state = manager.shared.state.lock().unwrap();
            state.fences.push_back(PendingFence {
                fence: TestFence { stubbed: true },
                committed_flushes: TEXTURE_FLUSHES | BUFFER_FLUSHES,
                pre_operations: VecDeque::new(),
                operations: VecDeque::new(),
            });
        }

        manager.wait_pending_fences(
            false,
            |is_stubbed| TestFence {
                stubbed: is_stubbed,
            },
            |_| {},
            || false,
            |_| true,
            |_| {},
            {
                let popped = Arc::clone(&popped);
                move |_| popped.store(true, Ordering::Relaxed)
            },
            CacheFlushes::default,
            || false,
            || {},
            || {},
            || true,
        );

        assert!(popped.load(Ordering::Relaxed));
        assert_eq!(manager.queued_fence_count(), 0);
    }

    #[test]
    fn non_async_wait_pending_fences_always_forces_host_wait() {
        let mut manager = FenceManager::<TestFence>::new(false);
        let waited = Arc::new(AtomicBool::new(false));
        let popped = Arc::new(AtomicBool::new(false));

        manager
            .shared
            .state
            .lock()
            .unwrap()
            .fences
            .push_back(PendingFence {
                fence: TestFence { stubbed: false },
                committed_flushes: TEXTURE_FLUSHES | BUFFER_FLUSHES,
                pre_operations: VecDeque::new(),
                operations: VecDeque::new(),
            });

        manager.wait_pending_fences(
            false,
            |is_stubbed| TestFence {
                stubbed: is_stubbed,
            },
            |_| {},
            || true,
            |_| false,
            {
                let waited = Arc::clone(&waited);
                move |_| waited.store(true, Ordering::Relaxed)
            },
            {
                let popped = Arc::clone(&popped);
                move |_| popped.store(true, Ordering::Relaxed)
            },
            CacheFlushes::default,
            || false,
            || {},
            || {},
            || true,
        );

        assert!(waited.load(Ordering::Relaxed));
        assert!(popped.load(Ordering::Relaxed));
        assert_eq!(manager.queued_fence_count(), 0);
    }

    #[test]
    fn async_signal_fence_executes_delayed_callback_without_manual_release() {
        let _gpu_accuracy = crate::test_support::GpuAccuracyGuard::set(GpuAccuracy::High);

        let mut manager = FenceManager::<TestFence>::new(true);
        let callback_hit = Arc::new(AtomicBool::new(false));
        let popped = Arc::new(AtomicBool::new(false));

        manager.signal_fence(
            Box::new({
                let callback_hit = Arc::clone(&callback_hit);
                move || callback_hit.store(true, Ordering::Relaxed)
            }),
            |is_stubbed| TestFence {
                stubbed: is_stubbed,
            },
            |_| {},
            || false,
            |_| true,
            {
                let popped = Arc::clone(&popped);
                move |_| popped.store(true, Ordering::Relaxed)
            },
            CacheFlushes::default,
            || false,
            || {},
            || {},
            || true,
        );

        wait_until(&popped);
        wait_until(&callback_hit);
    }

    #[test]
    fn mixed_cache_masks_preserve_per_fence_pop_and_callback_order() {
        let _accuracy = crate::test_support::GpuAccuracyGuard::set(GpuAccuracy::High);
        for asynchronous in [false, true] {
            let mut manager = FenceManager::<TestFence>::new(asynchronous);
            let events = Arc::new(Mutex::new(Vec::new()));
            let finished = Arc::new(AtomicBool::new(false));
            let masks = [TEXTURE_FLUSHES, 0, BUFFER_FLUSHES, TEXTURE_FLUSHES | BUFFER_FLUSHES];
            for (index, mask) in masks.into_iter().enumerate() {
                let callback_events = Arc::clone(&events);
                let pop_events = Arc::clone(&events);
                let finished = Arc::clone(&finished);
                manager.signal_fence(
                    Box::new(move || {
                        callback_events.lock().unwrap().push((1, index as u8));
                        if index == 3 {
                            finished.store(true, Ordering::Relaxed);
                        }
                    }),
                    |stubbed| TestFence { stubbed },
                    |_| {},
                    || false,
                    |_| true,
                    move |committed| pop_events.lock().unwrap().push((0, committed)),
                    // Async texture buffers can retire without a download wait.
                    move || CacheFlushes { should_flush: false, committed: mask },
                    || false,
                    || panic!("empty downloads must not force a submission"),
                    || {},
                    || true,
        );
            }
            if asynchronous {
                wait_until(&finished);
            } else {
                let events = Arc::clone(&events);
                manager.signal_ordering(
                    || false,
                    |_| true,
                    move |mask| events.lock().unwrap().push((0, mask)),
                    || {},
                );
            }
            let expected: Vec<_> = masks.into_iter().enumerate()
                .flat_map(|(index, mask)| [(0, mask), (1, index as u8)])
                .collect();
            assert_eq!(*events.lock().unwrap(), expected);
        }
    }

    fn queue_empty_fence_candidate(
        manager: &mut FenceManager<TestFence>,
        events: &Arc<Mutex<Vec<&'static str>>>,
        committed: u8,
        queries_pending: bool,
    ) {
        let callback = Arc::clone(events);
        let create = Arc::clone(events);
        let queries = Arc::clone(events);
        let invalidate = Arc::clone(events);
        manager.signal_fence(
            Box::new(move || callback.lock().unwrap().push("callback")),
            move |stubbed| {
                create.lock().unwrap().push("create");
                TestFence { stubbed }
            },
            |_| {},
            || false,
            |_| true,
            |_| {},
            move || CacheFlushes { should_flush: false, committed },
            move || { queries.lock().unwrap().push("query_commit"); false },
            || panic!("no downloads to submit"),
            move || invalidate.lock().unwrap().push("invalidate"),
            move || queries_pending,
        );
    }

    #[test]
    fn empty_fence_skip_limit_and_pending_work_match_upstream() {
        let _accuracy = crate::test_support::GpuAccuracyGuard::set(GpuAccuracy::Low);
        let mut manager = FenceManager::<TestFence>::new(false);
        let events = Arc::new(Mutex::new(Vec::new()));
        for index in 1..=64 {
            queue_empty_fence_candidate(&mut manager, &events, 0, false);
            assert_eq!(manager.skipped_fences, index);
            assert_eq!(manager.queued_fence_count(), 0);
        }
        // No query commit, fence allocation or queue publication on skipped calls.
        assert_eq!(*events.lock().unwrap(), ["callback", "invalidate"].repeat(64));
        queue_empty_fence_candidate(&mut manager, &events, 0, false);
        assert_eq!(manager.skipped_fences, 0);
        assert_eq!(manager.queued_fence_count(), 1);
        assert_eq!(&events.lock().unwrap()[128..], &["query_commit", "create", "callback", "invalidate"]);
        queue_empty_fence_candidate(&mut manager, &events, 0, false);
        assert_eq!(manager.skipped_fences, 1);
        assert_eq!(manager.queued_fence_count(), 0);

        for (mask, pending) in [(0, true), (TEXTURE_FLUSHES, false), (BUFFER_FLUSHES, false)] {
            queue_empty_fence_candidate(&mut manager, &events, mask, pending);
            assert_eq!(manager.skipped_fences, 0);
            assert_eq!(manager.queued_fence_count(), 1);
        }
        let operation_events = Arc::clone(&events);
        manager.sync_operation(Box::new(move || operation_events.lock().unwrap().push("operation")));
        queue_empty_fence_candidate(&mut manager, &events, 0, false);
        assert_eq!(manager.queued_fence_count(), 1);
        assert_eq!(manager.skipped_fences, 0);
        // The queued operation is carried by that real fence and retires once.
        queue_empty_fence_candidate(&mut manager, &events, 0, false);
        assert_eq!(events.lock().unwrap().iter().filter(|&&e| e == "operation").count(), 1);
        assert_eq!(manager.skipped_fences, 1);
    }

    #[test]
    fn high_accuracy_keeps_empty_fence_and_delays_callback() {
        let _accuracy = crate::test_support::GpuAccuracyGuard::set(GpuAccuracy::High);
        let mut manager = FenceManager::<TestFence>::new(false);
        let events = Arc::new(Mutex::new(Vec::new()));
        queue_empty_fence_candidate(&mut manager, &events, 0, false);
        assert_eq!(manager.skipped_fences, 0);
        assert_eq!(manager.queued_fence_count(), 1);
        assert_eq!(*events.lock().unwrap(), ["query_commit", "create", "invalidate"]);
        manager.signal_ordering(|| false, |_| true, |_| {}, || {});
        assert_eq!(*events.lock().unwrap(), ["query_commit", "create", "invalidate", "callback"]);
    }

    #[test]
    fn async_wait_pending_fences_force_signals_drain_fence_and_waits_for_callback() {
        let _gpu_accuracy = crate::test_support::GpuAccuracyGuard::set(GpuAccuracy::High);

        let mut manager = FenceManager::<TestFence>::new(true);
        let created = Arc::new(AtomicU32::new(0));
        let queued = Arc::new(AtomicU32::new(0));

        manager.wait_pending_fences(
            true,
            {
                let created = Arc::clone(&created);
                move |is_stubbed| {
                    created.fetch_add(1, Ordering::Relaxed);
                    TestFence {
                        stubbed: is_stubbed,
                    }
                }
            },
            {
                let queued = Arc::clone(&queued);
                move |_| {
                    queued.fetch_add(1, Ordering::Relaxed);
                }
            },
            || false,
            |_| true,
            |_| {},
            |_| {},
            CacheFlushes::default,
            || false,
            || {},
            || {},
            || true,
        );

        assert_eq!(created.load(Ordering::Relaxed), 1);
        assert_eq!(queued.load(Ordering::Relaxed), 1);
        assert_eq!(manager.queued_fence_count(), 0);
    }
}
