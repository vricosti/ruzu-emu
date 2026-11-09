//! Port of zuyu/src/common/bounded_threadsafe_queue.h

use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

/// Default capacity matching the C++ `detail::DefaultCapacity`.
pub const DEFAULT_CAPACITY: usize = 0x1000;

/// Cache-line-aligned wrapper mirroring upstream's `alignas(128)` on the
/// SPSC read/write indices. Prevents false sharing between producer and
/// consumer cores bouncing the same cache line.
#[repr(align(128))]
struct CacheAligned<T>(T);

impl<T> std::ops::Deref for CacheAligned<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> std::ops::DerefMut for CacheAligned<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

/// Bounded SPSC queue with blocking operations.
///
/// Mirrors the upstream fixed-capacity ring-buffer ownership:
/// atomic read/write indices plus separate producer/consumer wait objects.
pub struct BoundedSPSCQueue<T> {
    capacity: usize,
    read_index: CacheAligned<AtomicUsize>,
    write_index: CacheAligned<AtomicUsize>,
    data: Box<[UnsafeCell<MaybeUninit<T>>]>,
    producer_cv: Condvar,
    producer_cv_mutex: Mutex<()>,
    consumer_cv: Condvar,
    consumer_cv_mutex: Mutex<()>,
}

unsafe impl<T: Send> Send for BoundedSPSCQueue<T> {}
unsafe impl<T: Send> Sync for BoundedSPSCQueue<T> {}

impl<T> BoundedSPSCQueue<T> {
    /// Create a new bounded SPSC queue with the given capacity.
    pub fn new(capacity: usize) -> Self {
        assert!(
            capacity > 0 && (capacity & (capacity - 1)) == 0,
            "Capacity must be a power of two."
        );
        let mut data = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            data.push(UnsafeCell::new(MaybeUninit::uninit()));
        }
        Self {
            capacity,
            read_index: CacheAligned(AtomicUsize::new(0)),
            write_index: CacheAligned(AtomicUsize::new(0)),
            data: data.into_boxed_slice(),
            producer_cv: Condvar::new(),
            producer_cv_mutex: Mutex::new(()),
            consumer_cv: Condvar::new(),
            consumer_cv_mutex: Mutex::new(()),
        }
    }

    /// Create with the default capacity (0x1000).
    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }

    /// Try to emplace (push) a value. Returns `false` if the queue is full.
    pub fn try_emplace(&self, value: T) -> bool {
        let write_index = self.write_index.load(Ordering::Relaxed);
        if (write_index - self.read_index.load(Ordering::Acquire)) == self.capacity {
            return false;
        }
        self.write_at(write_index, value);
        true
    }

    /// Push a value, blocking until space is available.
    pub fn emplace_wait(&self, value: T) {
        let write_index = self.write_index.load(Ordering::Relaxed);
        if (write_index - self.read_index.load(Ordering::Acquire)) == self.capacity {
            let lock = self.producer_cv_mutex.lock().unwrap();
            let _guard = self
                .producer_cv
                .wait_while(lock, |_| {
                    (write_index - self.read_index.load(Ordering::Acquire)) == self.capacity
                })
                .unwrap();
        }
        self.write_at(write_index, value);
    }

    /// Rust cancellation counterpart of EmplaceWait for a worker that can
    /// unwind. Returns ownership of an unqueued value when stopping, so a full
    /// queue cannot strand its producer after the consumer has failed.
    pub fn emplace_wait_with_stop<F>(&self, value: T, should_stop: F) -> Result<(), T>
    where F: Fn() -> bool {
        let write_index = self.write_index.load(Ordering::Relaxed);
        if (write_index - self.read_index.load(Ordering::Acquire)) == self.capacity {
            let lock = self.producer_cv_mutex.lock().unwrap();
            let _guard = self.producer_cv.wait_while(lock, |_| {
                !should_stop()
                    && (write_index - self.read_index.load(Ordering::Acquire)) == self.capacity
            }).unwrap();
        }
        if should_stop() { return Err(value); }
        self.write_at(write_index, value);
        Ok(())
    }

    /// Try to pop a value. Returns `None` if the queue is empty.
    pub fn try_pop(&self) -> Option<T> {
        let read_index = self.read_index.load(Ordering::Relaxed);
        if read_index == self.write_index.load(Ordering::Acquire) {
            return None;
        }
        Some(self.read_at(read_index))
    }

    /// Pop a value, blocking until one is available.
    pub fn pop_wait(&self) -> T {
        let read_index = self.read_index.load(Ordering::Relaxed);
        if read_index == self.write_index.load(Ordering::Acquire) {
            let lock = self.consumer_cv_mutex.lock().unwrap();
            let _guard = self
                .consumer_cv
                .wait_while(lock, |_| {
                    read_index == self.write_index.load(Ordering::Acquire)
                })
                .unwrap();
        }
        self.read_at(read_index)
    }

    /// Pop a value, blocking until one is available or `should_stop` returns true.
    pub fn pop_wait_with_stop<F>(&self, should_stop: F) -> Option<T>
    where
        F: Fn() -> bool,
    {
        let read_index = self.read_index.load(Ordering::Relaxed);
        if read_index == self.write_index.load(Ordering::Acquire) {
            let lock = self.consumer_cv_mutex.lock().unwrap();
            let _guard = self
                .consumer_cv
                .wait_while(lock, |_| {
                    !should_stop() && read_index == self.write_index.load(Ordering::Acquire)
                })
                .unwrap();
        }
        // Upstream checks the stop token even if data is already queued.
        if should_stop() { return None; }
        Some(self.read_at(read_index))
    }

    /// Wake any producer/consumer waiters.
    pub fn notify_all(&self) {
        // Pair external stop notification with each waiter's predicate check.
        // Otherwise stop can be signalled between that check and wait(), and
        // both threads can sleep forever despite the stop flag being set.
        {
            let _lock = self.producer_cv_mutex.lock().unwrap();
            self.producer_cv.notify_all();
        }
        {
            let _lock = self.consumer_cv_mutex.lock().unwrap();
            self.consumer_cv.notify_all();
        }
    }

    fn write_at(&self, write_index: usize, value: T) {
        let pos = write_index % self.capacity;
        unsafe {
            (*self.data[pos].get()).write(value);
        }
        self.write_index.store(write_index + 1, Ordering::SeqCst);
        let _lock = self.consumer_cv_mutex.lock().unwrap();
        self.consumer_cv.notify_one();
    }

    fn read_at(&self, read_index: usize) -> T {
        let pos = read_index % self.capacity;
        let value = unsafe { (*self.data[pos].get()).assume_init_read() };
        self.read_index.store(read_index + 1, Ordering::SeqCst);
        let _lock = self.producer_cv_mutex.lock().unwrap();
        self.producer_cv.notify_one();
        value
    }
}

impl<T> Drop for BoundedSPSCQueue<T> {
    fn drop(&mut self) {
        let read_index = *self.read_index.get_mut();
        let write_index = *self.write_index.get_mut();
        for index in read_index..write_index {
            let pos = index % self.capacity;
            unsafe {
                (*self.data[pos].get()).assume_init_drop();
            }
        }
    }
}

/// Bounded MPSC queue (multiple producers, single consumer).
///
/// Mirrors the C++ `Common::MPSCQueue<T, Capacity>`.
pub struct BoundedMPSCQueue<T> {
    inner: BoundedSPSCQueue<T>,
    write_mutex: Mutex<()>,
}

impl<T> BoundedMPSCQueue<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: BoundedSPSCQueue::new(capacity),
            write_mutex: Mutex::new(()),
        }
    }

    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }

    pub fn try_emplace(&self, value: T) -> bool {
        let _lock = self.write_mutex.lock().unwrap();
        self.inner.try_emplace(value)
    }

    pub fn emplace_wait(&self, value: T) {
        let _lock = self.write_mutex.lock().unwrap();
        self.inner.emplace_wait(value);
    }

    pub fn try_pop(&self) -> Option<T> {
        self.inner.try_pop()
    }

    pub fn pop_wait(&self) -> T {
        self.inner.pop_wait()
    }

    pub fn pop_wait_with_stop<F>(&self, should_stop: F) -> Option<T>
    where
        F: Fn() -> bool,
    {
        self.inner.pop_wait_with_stop(should_stop)
    }

    pub fn notify_all(&self) {
        self.inner.notify_all();
    }
}

/// Bounded MPMC queue (multiple producers, multiple consumers).
///
/// Mirrors the C++ `Common::MPMCQueue<T, Capacity>`.
pub struct BoundedMPMCQueue<T> {
    inner: BoundedSPSCQueue<T>,
    write_mutex: Mutex<()>,
    read_mutex: Mutex<()>,
}

impl<T> BoundedMPMCQueue<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: BoundedSPSCQueue::new(capacity),
            write_mutex: Mutex::new(()),
            read_mutex: Mutex::new(()),
        }
    }

    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }

    pub fn try_emplace(&self, value: T) -> bool {
        let _lock = self.write_mutex.lock().unwrap();
        self.inner.try_emplace(value)
    }

    pub fn emplace_wait(&self, value: T) {
        let _lock = self.write_mutex.lock().unwrap();
        self.inner.emplace_wait(value);
    }

    pub fn try_pop(&self) -> Option<T> {
        let _lock = self.read_mutex.lock().unwrap();
        self.inner.try_pop()
    }

    pub fn pop_wait(&self) -> T {
        let _lock = self.read_mutex.lock().unwrap();
        self.inner.pop_wait()
    }

    pub fn pop_wait_with_stop<F>(&self, should_stop: F) -> Option<T>
    where
        F: Fn() -> bool,
    {
        let _lock = self.read_mutex.lock().unwrap();
        self.inner.pop_wait_with_stop(should_stop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[test]
    fn test_try_emplace_pop() {
        let q = BoundedSPSCQueue::new(4);
        assert!(q.try_emplace(1));
        assert!(q.try_emplace(2));
        assert!(q.try_emplace(3));
        assert!(q.try_emplace(4));
        assert!(!q.try_emplace(5));

        assert_eq!(q.try_pop(), Some(1));
        assert_eq!(q.try_pop(), Some(2));
        assert!(q.try_emplace(5));
    }

    #[test]
    fn test_blocking_pop() {
        let q = Arc::new(BoundedSPSCQueue::new(4));
        let q2 = q.clone();

        let handle = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(10));
            q2.try_emplace(42);
        });

        let val = q.pop_wait();
        assert_eq!(val, 42);
        handle.join().unwrap();
    }

    #[test]
    fn test_mpsc() {
        let q = Arc::new(BoundedMPSCQueue::new(16));
        let q1 = q.clone();
        let q2 = q.clone();

        let h1 = std::thread::spawn(move || {
            for i in 0..4 {
                q1.emplace_wait(i);
            }
        });
        let h2 = std::thread::spawn(move || {
            for i in 10..14 {
                q2.emplace_wait(i);
            }
        });

        h1.join().unwrap();
        h2.join().unwrap();

        let mut values = Vec::new();
        while let Some(v) = q.try_pop() {
            values.push(v);
        }
        assert_eq!(values.len(), 8);
    }

    #[test]
    fn stopped_pop_preserves_already_queued_value_like_upstream() {
        let q = BoundedSPSCQueue::new(1);
        q.emplace_wait(7);
        assert_eq!(q.pop_wait_with_stop(|| true), None);
        assert_eq!(q.try_pop(), Some(7));
    }

    #[test]
    fn cancelled_full_enqueue_returns_the_unqueued_value() {
        let q = Arc::new(BoundedSPSCQueue::new(1));
        q.emplace_wait(7);
        let stop = Arc::new(AtomicBool::new(false));
        let (checked, ready) = std::sync::mpsc::channel();
        let (done, result) = std::sync::mpsc::channel();
        let worker_q = q.clone();
        let worker_stop = stop.clone();
        let worker = std::thread::spawn(move || {
            let outcome = worker_q.emplace_wait_with_stop(9, || {
                let _ = checked.send(());
                worker_stop.load(Ordering::Acquire)
            });
            done.send(outcome).unwrap();
        });
        ready.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
        stop.store(true, Ordering::Release);
        q.notify_all();
        assert_eq!(result.recv_timeout(std::time::Duration::from_secs(3)).unwrap(), Err(9));
        worker.join().unwrap();
        assert_eq!(q.try_pop(), Some(7));
        assert_eq!(q.try_pop(), None);
        assert_eq!(q.emplace_wait_with_stop(11, || true), Err(11));
        assert_eq!(q.emplace_wait_with_stop(13, || false), Ok(()));
        assert_eq!(q.try_pop(), Some(13));
    }

    #[test]
    fn test_pop_wait_with_stop_wakes_after_notify_all() {
        let q = Arc::new(BoundedMPSCQueue::<u32>::new(4));
        let stop = Arc::new(AtomicBool::new(false));

        let q2 = q.clone();
        let stop2 = stop.clone();
        let handle =
            std::thread::spawn(move || q2.pop_wait_with_stop(|| stop2.load(Ordering::Relaxed)));

        std::thread::sleep(std::time::Duration::from_millis(10));
        stop.store(true, Ordering::Relaxed);
        q.notify_all();

        assert_eq!(handle.join().unwrap(), None);
    }
}
