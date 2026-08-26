// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `video_core/textures/workers.h` and `workers.cpp`.
//!
//! Provides a shared thread worker pool for texture transcoding operations
//! (ASTC decompression, BCN compression, etc.).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

// ── Thread Worker Pool ───────────────────────────────────────────────────────

/// Work item for the thread pool.
type WorkItem = Box<dyn FnOnce() + Send>;

/// A simple thread worker pool for texture transcoding.
///
/// Port of `Common::ThreadWorker` usage in `workers.cpp`.
///
/// The upstream implementation returns a static `Common::ThreadWorker` with
/// `max(hardware_concurrency, 2) / 2` threads named "ImageTranscode".
pub struct ThreadWorker {
    num_threads: usize,
    /// Work queue shared between producer and worker threads.
    queue: Arc<Mutex<VecDeque<WorkItem>>>,
    /// Condition variable to wake worker threads.
    condvar: Arc<Condvar>,
    /// Number of work items accepted by the queue.
    work_scheduled: Arc<AtomicUsize>,
    /// Number of work items fully executed by workers.
    work_done: Arc<AtomicUsize>,
    /// Condition variable for waiting until all work is done.
    done_condvar: Arc<Condvar>,
    /// Stop flag for worker threads.
    stop_flag: Arc<AtomicBool>,
    /// Worker thread handles.
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl ThreadWorker {
    /// Create a new worker pool with the specified number of threads.
    pub fn new(num_threads: usize) -> Self {
        Self::new_named(num_threads, "ImageTranscode")
    }

    /// Create a new worker pool with the specified number of threads and name.
    pub fn new_named(num_threads: usize, name: &str) -> Self {
        let queue = Arc::new(Mutex::new(VecDeque::<WorkItem>::new()));
        let condvar = Arc::new(Condvar::new());
        let work_scheduled = Arc::new(AtomicUsize::new(0));
        let work_done = Arc::new(AtomicUsize::new(0));
        let done_condvar = Arc::new(Condvar::new());
        let stop_flag = Arc::new(AtomicBool::new(false));

        let mut threads = Vec::with_capacity(num_threads);
        for i in 0..num_threads {
            let queue = Arc::clone(&queue);
            let condvar = Arc::clone(&condvar);
            let work_done = Arc::clone(&work_done);
            let done_condvar = Arc::clone(&done_condvar);
            let stop_flag = Arc::clone(&stop_flag);
            let thread_name = format!("{}:{}", name, i);

            let handle = std::thread::Builder::new()
                .name(thread_name)
                .spawn(move || loop {
                    let work = {
                        let mut locked = queue.lock().unwrap();
                        loop {
                            if stop_flag.load(Ordering::Relaxed) {
                                return;
                            }
                            if let Some(item) = locked.pop_front() {
                                break Some(item);
                            }
                            locked = condvar.wait(locked).unwrap();
                        }
                    };

                    if let Some(work) = work {
                        work();
                        work_done.fetch_add(1, Ordering::Release);
                        done_condvar.notify_all();
                    }
                })
                .expect("Failed to spawn ImageTranscode thread");
            threads.push(handle);
        }

        Self {
            num_threads,
            queue,
            condvar,
            work_scheduled,
            work_done,
            done_condvar,
            stop_flag,
            threads,
        }
    }

    /// Number of worker threads in the pool.
    pub fn num_threads(&self) -> usize {
        self.num_threads
    }

    /// Queue work to be executed by the thread pool.
    ///
    /// Port of `Common::ThreadWorker::QueueWork`.
    pub fn queue_work<F: FnOnce() + Send + 'static>(&self, work: F) {
        let mut locked = self.queue.lock().unwrap();
        locked.push_back(Box::new(work));
        self.work_scheduled.fetch_add(1, Ordering::Release);
        self.condvar.notify_one();
    }

    /// Wait for all queued work to complete.
    ///
    /// Port of `Common::ThreadWorker::WaitForRequests`.
    pub fn wait_for_requests(&self) {
        let locked = self.queue.lock().unwrap();
        let _guard = self
            .done_condvar
            .wait_while(locked, |_| {
                self.work_done.load(Ordering::Acquire) < self.work_scheduled.load(Ordering::Acquire)
            })
            .unwrap();
    }
}

impl Drop for ThreadWorker {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Release);
        self.condvar.notify_all();
        for handle in self.threads.drain(..) {
            handle.join().ok();
        }
    }
}

/// Global singleton thread worker pool for texture transcoding.
///
/// Port of the static `Common::ThreadWorker` in `GetThreadWorkers()`.
static THREAD_WORKERS: OnceLock<ThreadWorker> = OnceLock::new();

/// Returns a reference to the shared texture transcoding thread worker pool.
///
/// Port of `Tegra::Texture::GetThreadWorkers()`.
///
/// The pool is lazily initialized with `max(hardware_concurrency, 2) / 2` threads.
pub fn get_thread_workers() -> &'static ThreadWorker {
    THREAD_WORKERS.get_or_init(|| {
        let num_threads = std::cmp::max(
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(2),
            2,
        ) / 2;
        ThreadWorker::new(num_threads)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;
    use std::time::Duration;

    #[test]
    fn worker_pool_initializes() {
        let workers = get_thread_workers();
        assert!(workers.num_threads() >= 1);
    }

    #[test]
    fn worker_executes_work() {
        let worker = ThreadWorker::new(2);
        let counter = Arc::new(AtomicU32::new(0));

        for _ in 0..10 {
            let counter = Arc::clone(&counter);
            worker.queue_work(move || {
                counter.fetch_add(1, Ordering::Relaxed);
            });
        }

        worker.wait_for_requests();
        assert_eq!(counter.load(Ordering::Relaxed), 10);
    }

    #[test]
    fn single_worker_executes_requests_in_fifo_order() {
        let worker = ThreadWorker::new(1);
        let order = Arc::new(Mutex::new(Vec::new()));
        let (release_sender, release_receiver) = std::sync::mpsc::channel();

        worker.queue_work(move || release_receiver.recv().unwrap());
        for value in 0..4 {
            let order = Arc::clone(&order);
            worker.queue_work(move || order.lock().unwrap().push(value));
        }
        release_sender.send(()).unwrap();

        worker.wait_for_requests();
        assert_eq!(*order.lock().unwrap(), vec![0, 1, 2, 3]);
    }

    #[test]
    fn wait_for_requests_waits_for_the_running_request() {
        let worker = Arc::new(ThreadWorker::new(1));
        let (started_sender, started_receiver) = std::sync::mpsc::channel();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        let completed = Arc::new(AtomicBool::new(false));
        let completed_for_work = Arc::clone(&completed);

        worker.queue_work(move || {
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
}
