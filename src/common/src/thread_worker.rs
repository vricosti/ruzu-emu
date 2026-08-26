// SPDX-FileCopyrightText: Copyright 2020 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/common/thread_worker.h
//!
//! Provides `StatefulThreadWorker<S>` -- a thread pool where each worker owns
//! a piece of per-thread state `S`, and `ThreadWorker` (the stateless variant).
//!
//! Tasks are queued and distributed to worker threads. `wait_for_requests`
//! blocks until all queued work has been processed.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::thread::{
    set_current_thread_priority, set_current_thread_to_all_cores,
    set_current_thread_to_background_work, set_current_thread_to_efficiency_cores, ThreadPlacement,
    ThreadPriority,
};

/// Internal shared state between the pool and its workers.
struct SharedState<S: Send + 'static> {
    queue: Mutex<VecDeque<Box<dyn FnOnce(&mut S) + Send>>>,
    /// Signalled when a new task is available or stop is requested.
    condition: Condvar,
    /// Signalled when the queue becomes empty or a worker stops.
    wait_condition: Condvar,
    work_scheduled: AtomicUsize,
    work_done: AtomicUsize,
    workers_stopped: AtomicUsize,
    workers_queued: usize,
    stop: AtomicBool,
}

/// A thread pool where each worker thread owns per-thread state of type `S`.
///
/// Tasks are closures `FnOnce(&mut S)` that receive a mutable reference to
/// the worker's state.
///
/// The stateless variant `ThreadWorker` uses `S = ()`.
pub struct StatefulThreadWorker<S: Send + 'static> {
    shared: Arc<SharedState<S>>,
    threads: Vec<JoinHandle<()>>,
}

impl<S: Send + 'static> StatefulThreadWorker<S> {
    /// Create a new pool with `num_workers` threads. Each worker's state is
    /// created by calling `state_maker()`.
    pub fn new<F>(num_workers: usize, name: String, state_maker: F) -> Self
    where
        F: Fn() -> S + Send + Clone + 'static,
    {
        Self::new_with_placement(num_workers, name, state_maker, ThreadPlacement::Default)
    }

    /// Create a worker pool with upstream `ThreadPlacement` semantics.
    pub fn new_with_placement<F>(
        num_workers: usize,
        name: String,
        state_maker: F,
        placement: ThreadPlacement,
    ) -> Self
    where
        F: Fn() -> S + Send + Clone + 'static,
    {
        let shared = Arc::new(SharedState {
            queue: Mutex::new(VecDeque::new()),
            condition: Condvar::new(),
            wait_condition: Condvar::new(),
            work_scheduled: AtomicUsize::new(0),
            work_done: AtomicUsize::new(0),
            workers_stopped: AtomicUsize::new(0),
            workers_queued: num_workers,
            stop: AtomicBool::new(false),
        });

        let mut threads = Vec::with_capacity(num_workers);
        for _ in 0..num_workers {
            let shared_clone = shared.clone();
            let name_clone = name.clone();
            let maker = state_maker.clone();
            threads.push(
                thread::Builder::new()
                    .name(name_clone)
                    .spawn(move || {
                        if placement != ThreadPlacement::Default {
                            set_current_thread_priority(ThreadPriority::Low);
                        }
                        match placement {
                            ThreadPlacement::Efficiency => set_current_thread_to_efficiency_cores(),
                            ThreadPlacement::Background => set_current_thread_to_background_work(),
                            ThreadPlacement::Default => set_current_thread_to_all_cores(),
                        }
                        let mut state = maker();
                        loop {
                            let task;
                            {
                                let mut queue = shared_clone.queue.lock().unwrap();
                                if queue.is_empty() {
                                    shared_clone.wait_condition.notify_all();
                                }
                                queue = shared_clone
                                    .condition
                                    .wait_while(queue, |q| {
                                        q.is_empty() && !shared_clone.stop.load(Ordering::Acquire)
                                    })
                                    .unwrap();
                                if shared_clone.stop.load(Ordering::Acquire) {
                                    break;
                                }
                                task = queue.pop_front().unwrap();
                            }
                            task(&mut state);
                            shared_clone.work_done.fetch_add(1, Ordering::Release);
                        }
                        shared_clone.workers_stopped.fetch_add(1, Ordering::Release);
                        shared_clone.wait_condition.notify_all();
                    })
                    .expect("failed to spawn worker thread"),
            );
        }

        Self { shared, threads }
    }

    /// Queue a task for execution by one of the worker threads.
    pub fn queue_work<F>(&self, work: F)
    where
        F: FnOnce(&mut S) + Send + 'static,
    {
        {
            let mut queue = self.shared.queue.lock().unwrap();
            queue.push_back(Box::new(work));
            self.shared.work_scheduled.fetch_add(1, Ordering::Release);
        }
        self.shared.condition.notify_one();
    }

    /// Block until all queued work has been completed or all workers have
    /// stopped.
    pub fn wait_for_requests(&self) {
        let queue = self.shared.queue.lock().unwrap();
        let _guard = self
            .shared
            .wait_condition
            .wait_while(queue, |_| {
                let stopped = self.shared.workers_stopped.load(Ordering::Acquire);
                let done = self.shared.work_done.load(Ordering::Acquire);
                let scheduled = self.shared.work_scheduled.load(Ordering::Acquire);
                stopped < self.shared.workers_queued && done < scheduled
            })
            .unwrap();
    }

    /// Block until all queued work has completed, or permanently stop this
    /// worker when the caller requests cancellation.
    ///
    /// This mirrors upstream `WaitForRequests(std::stop_token)`: cancellation
    /// requests stop on every worker thread, so queued requests that have not
    /// started are abandoned and the worker must not be reused afterwards.
    pub fn wait_for_requests_or_stop(&self, stop_requested: &AtomicBool) {
        let mut queue = self.shared.queue.lock().unwrap();
        loop {
            if stop_requested.load(Ordering::Acquire) {
                self.shared.stop.store(true, Ordering::Release);
                self.shared.condition.notify_all();
            }
            let stopped = self.shared.workers_stopped.load(Ordering::Acquire);
            let done = self.shared.work_done.load(Ordering::Acquire);
            let scheduled = self.shared.work_scheduled.load(Ordering::Acquire);
            if stopped >= self.shared.workers_queued || done >= scheduled {
                break;
            }
            let (next_queue, _) = self
                .shared
                .wait_condition
                .wait_timeout(queue, Duration::from_millis(10))
                .unwrap();
            queue = next_queue;
        }
    }
}

impl<S: Send + 'static> Drop for StatefulThreadWorker<S> {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.condition.notify_all();
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
    }
}

/// Stateless thread worker -- equivalent to upstream `ThreadWorker`.
pub type ThreadWorker = StatefulThreadWorker<()>;

impl ThreadWorker {
    /// Create a stateless thread worker pool.
    pub fn new_stateless(num_workers: usize, name: String) -> Self {
        Self::new(num_workers, name, || ())
    }

    /// Create a stateless worker with upstream placement semantics.
    pub fn new_stateless_with_placement(
        num_workers: usize,
        name: String,
        placement: ThreadPlacement,
    ) -> Self {
        Self::new_with_placement(num_workers, name, || (), placement)
    }

    /// Queue a stateless task.
    pub fn queue_stateless_work<F>(&self, work: F)
    where
        F: FnOnce() + Send + 'static,
    {
        self.queue_work(move |_: &mut ()| work());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;
    use std::time::Duration;

    #[test]
    fn test_stateless_worker() {
        let counter = Arc::new(AtomicU32::new(0));
        let worker = ThreadWorker::new_stateless(2, "test-worker".to_string());

        for _ in 0..100 {
            let c = counter.clone();
            worker.queue_stateless_work(move || {
                c.fetch_add(1, Ordering::SeqCst);
            });
        }

        worker.wait_for_requests();
        assert_eq!(counter.load(Ordering::SeqCst), 100);
    }

    #[test]
    fn single_worker_executes_requests_in_fifo_order() {
        let worker = ThreadWorker::new_stateless(1, "fifo-worker".to_string());
        let order = Arc::new(Mutex::new(Vec::new()));
        let (release_sender, release_receiver) = std::sync::mpsc::channel();

        worker.queue_stateless_work(move || release_receiver.recv().unwrap());
        for value in 0..4 {
            let order = Arc::clone(&order);
            worker.queue_stateless_work(move || order.lock().unwrap().push(value));
        }
        release_sender.send(()).unwrap();

        worker.wait_for_requests();
        assert_eq!(*order.lock().unwrap(), vec![0, 1, 2, 3]);
    }

    #[test]
    fn wait_for_requests_waits_for_the_running_request() {
        let worker = Arc::new(ThreadWorker::new_stateless(1, "wait-worker".to_string()));
        let (started_sender, started_receiver) = std::sync::mpsc::channel();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        let completed = Arc::new(AtomicBool::new(false));
        let completed_for_work = Arc::clone(&completed);

        worker.queue_stateless_work(move || {
            started_sender.send(()).unwrap();
            release_receiver.recv().unwrap();
            completed_for_work.store(true, Ordering::Release);
        });
        started_receiver.recv().unwrap();

        let worker_for_wait = Arc::clone(&worker);
        let (waited_sender, waited_receiver) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            worker_for_wait.wait_for_requests();
            waited_sender.send(()).unwrap();
        });
        assert!(waited_receiver
            .recv_timeout(Duration::from_millis(20))
            .is_err());

        release_sender.send(()).unwrap();
        waited_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        waiter.join().unwrap();
        assert!(completed.load(Ordering::Acquire));
    }

    #[test]
    fn placed_stateless_worker_executes_requests() {
        let counter = Arc::new(AtomicU32::new(0));
        let worker = ThreadWorker::new_stateless_with_placement(
            1,
            "placed-worker".to_string(),
            ThreadPlacement::Efficiency,
        );
        let queued_counter = Arc::clone(&counter);
        worker.queue_stateless_work(move || {
            queued_counter.fetch_add(1, Ordering::SeqCst);
        });
        worker.wait_for_requests();
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cancellation_stops_workers_without_draining_queued_requests() {
        let stop = AtomicBool::new(false);
        let task_started = Arc::new(AtomicBool::new(false));
        let release_task = Arc::new(AtomicBool::new(false));
        let counter = Arc::new(AtomicU32::new(0));
        let worker = ThreadWorker::new_stateless(1, "cancelled-worker".to_string());
        let started = Arc::clone(&task_started);
        let release = Arc::clone(&release_task);
        worker.queue_stateless_work(move || {
            started.store(true, Ordering::Release);
            while !release.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
        });
        while !task_started.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        for _ in 0..8 {
            let counter = Arc::clone(&counter);
            worker.queue_stateless_work(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            });
        }

        stop.store(true, Ordering::Release);
        let release = Arc::clone(&release_task);
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            release.store(true, Ordering::Release);
        });
        worker.wait_for_requests_or_stop(&stop);
        releaser.join().unwrap();

        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_stateful_worker() {
        // Each worker accumulates a local sum.
        let worker = StatefulThreadWorker::new(1, "stateful-test".to_string(), || 0u64);

        let result = Arc::new(AtomicU64::new(0));
        use std::sync::atomic::AtomicU64;

        for i in 0..10u64 {
            let r = result.clone();
            worker.queue_work(move |state: &mut u64| {
                *state += i;
                r.store(*state, Ordering::SeqCst);
            });
        }

        worker.wait_for_requests();
        // Sum of 0..10 = 45
        assert_eq!(result.load(Ordering::SeqCst), 45);
    }
}
