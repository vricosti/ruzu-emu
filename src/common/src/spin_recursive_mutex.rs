//! Port of Lichii src/common/spin_recursive_mutex.h (Lichii e08b402527, "video_core: less
//! waiting on the GPU caches' locks"), a Lichii extension absent from Eden.
//!
//! A recursive mutex that spins briefly before sleeping. The GPU caches' locks are taken tens of
//! thousands of times a second by the GPU thread, the fencing thread and CPU write traps, and
//! held for microseconds: a mutex that puts a waiting thread to sleep at once makes it wait for
//! a wake-up that takes longer than the critical section.
//!
//! Divergences:
//! - `std::atomic::wait`/`notify_one` become `parking_lot_core::park`/`unpark_one` keyed on the
//!   state's address (same protocol: sleep while the state is 2).
//! - The owner is a per-thread token (the address of a thread-local) instead of
//!   `std::thread::id`.
//! - Lichii's EDEN_NX2_GPU_PROFILE contention counters are not ported.
//! - `lock` returns a guard that unlocks on drop, as the `parking_lot::ReentrantMutex` it
//!   replaces did.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

/// About 10 microseconds of spinning before sleeping (Lichii `SpinCount`).
const SPIN_COUNT: u32 = 1024;

const FREE: u32 = 0;
const HELD: u32 = 1;
/// Held with (possible) sleepers.
const HELD_WITH_SLEEPERS: u32 = 2;

fn thread_token() -> usize {
    thread_local! {
        static TOKEN: u8 = const { 0 };
    }
    TOKEN.with(|token| token as *const u8 as usize)
}

pub struct SpinRecursiveMutex {
    /// 0 free, 1 held, 2 held with sleepers.
    state: AtomicU32,
    owner: AtomicUsize,
    /// Recursion depth, touched only by the owner.
    depth: UnsafeCell<u32>,
}

// Safety: `depth` is only read and written by the thread that owns the lock.
unsafe impl Send for SpinRecursiveMutex {}
unsafe impl Sync for SpinRecursiveMutex {}

impl Default for SpinRecursiveMutex {
    fn default() -> Self {
        Self::new()
    }
}

#[must_use = "the lock is released when the guard is dropped"]
pub struct SpinRecursiveMutexGuard<'a> {
    mutex: &'a SpinRecursiveMutex,
}

impl Drop for SpinRecursiveMutexGuard<'_> {
    fn drop(&mut self) {
        self.mutex.unlock();
    }
}

impl SpinRecursiveMutex {
    pub const fn new() -> Self {
        Self {
            state: AtomicU32::new(FREE),
            owner: AtomicUsize::new(0),
            depth: UnsafeCell::new(0),
        }
    }

    pub fn lock(&self) -> SpinRecursiveMutexGuard<'_> {
        let this_thread = thread_token();
        if self.owner.load(Ordering::Relaxed) == this_thread {
            unsafe { *self.depth.get() += 1 };
            return SpinRecursiveMutexGuard { mutex: self };
        }
        if self
            .state
            .compare_exchange(FREE, HELD, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            self.lock_contended();
        }
        self.owner.store(this_thread, Ordering::Relaxed);
        unsafe { *self.depth.get() = 1 };
        SpinRecursiveMutexGuard { mutex: self }
    }

    pub fn try_lock(&self) -> Option<SpinRecursiveMutexGuard<'_>> {
        let this_thread = thread_token();
        if self.owner.load(Ordering::Relaxed) == this_thread {
            unsafe { *self.depth.get() += 1 };
            return Some(SpinRecursiveMutexGuard { mutex: self });
        }
        if self
            .state
            .compare_exchange(FREE, HELD, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return None;
        }
        self.owner.store(this_thread, Ordering::Relaxed);
        unsafe { *self.depth.get() = 1 };
        Some(SpinRecursiveMutexGuard { mutex: self })
    }

    fn unlock(&self) {
        unsafe {
            *self.depth.get() -= 1;
            if *self.depth.get() != 0 {
                return;
            }
        }
        self.owner.store(0, Ordering::Relaxed);
        if self.state.swap(FREE, Ordering::Release) == HELD_WITH_SLEEPERS {
            let key = self.state.as_ptr() as usize;
            unsafe {
                parking_lot_core::unpark_one(key, |_| parking_lot_core::DEFAULT_UNPARK_TOKEN);
            }
        }
    }

    #[cold]
    fn lock_contended(&self) {
        self.acquire();
    }

    /// Returns whether it slept.
    fn acquire(&self) -> bool {
        for _ in 0..SPIN_COUNT {
            std::hint::spin_loop();
            if self.state.load(Ordering::Relaxed) == FREE
                && self
                    .state
                    .compare_exchange_weak(FREE, HELD, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
            {
                return false;
            }
        }
        // Taken this way it stays marked 2, so its unlock wakes the next sleeper.
        while self.state.swap(HELD_WITH_SLEEPERS, Ordering::Acquire) != FREE {
            let key = self.state.as_ptr() as usize;
            unsafe {
                parking_lot_core::park(
                    key,
                    || self.state.load(Ordering::Relaxed) == HELD_WITH_SLEEPERS,
                    || {},
                    |_, _| {},
                    parking_lot_core::DEFAULT_PARK_TOKEN,
                    None,
                );
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn recursive_lock_and_try_lock() {
        let mutex = SpinRecursiveMutex::new();
        let outer = mutex.lock();
        let inner = mutex.lock();
        assert!(mutex.try_lock().is_some());
        drop(inner);
        drop(outer);
        assert_eq!(mutex.state.load(Ordering::Relaxed), FREE);
    }

    #[test]
    fn excludes_other_threads() {
        let mutex = Arc::new(SpinRecursiveMutex::new());
        let counter = Arc::new(AtomicUsize::new(0));
        let shared = Arc::new(UnsafeCellBox(UnsafeCell::new(0u64)));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let (mutex, counter, shared) = (mutex.clone(), counter.clone(), shared.clone());
                std::thread::spawn(move || {
                    for _ in 0..20_000 {
                        let _guard = mutex.lock();
                        let _nested = mutex.lock();
                        unsafe { *shared.0.get() += 1 };
                        counter.fetch_add(1, Ordering::Relaxed);
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(unsafe { *shared.0.get() }, 80_000);
        assert_eq!(counter.load(Ordering::Relaxed), 80_000);
        assert!(mutex.try_lock().is_some());
    }

    struct UnsafeCellBox(UnsafeCell<u64>);
    unsafe impl Sync for UnsafeCellBox {}
    unsafe impl Send for UnsafeCellBox {}
}
