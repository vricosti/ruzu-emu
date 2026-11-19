//! Port of zuyu/src/core/hle/kernel/k_spin_lock.h and k_spin_lock.cpp
//! Status: COMPLET
//! Derniere synchro: 2026-03-11
//!
//! KSpinLock — Eden wraps std::mutex; ruzu ports Lichii's spin-then-sleep lock (see below).
//! KAlignedSpinLock and KNotAlignedSpinLock are aliases (upstream: "Alias for now").

use std::sync::atomic::{AtomicU32, Ordering};

/// Lichii `SpinCount` (k_spin_lock.cpp): spins of about 30-140 cycles each before a contended
/// `lock` sleeps (a few microseconds).
const SPIN_COUNT: u32 = 128;

/// KSpinLock — Lock/Unlock/TryLock interface.
/// Mirrors upstream `Kernel::KSpinLock`.
///
/// Eden wraps `std::mutex`. This is Lichii's three-state futex lock (63244330cd, "kernel: spin
/// briefly before sleeping on a contended KSpinLock"; a Lichii divergence from Eden): the
/// scheduler lock is taken by every core thousands of times a frame and held for microseconds,
/// so a contended `lock` spins ~128 pauses before sleeping instead of paying a host wake-up.
/// `std::atomic::wait`/`notify_one` become `parking_lot_core::park`/`unpark_one` on the state's
/// address.
pub struct KSpinLock {
    /// 0 free, 1 held, 2 held with (possible) sleepers.
    m_state: AtomicU32,
}

impl KSpinLock {
    pub const fn new() -> Self {
        Self {
            m_state: AtomicU32::new(0),
        }
    }

    /// Lock the spin lock. Mirrors upstream `KSpinLock::Lock()`.
    pub fn lock(&self) {
        if self
            .m_state
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return;
        }
        for _ in 0..SPIN_COUNT {
            std::hint::spin_loop();
            if self.m_state.load(Ordering::Relaxed) == 0
                && self
                    .m_state
                    .compare_exchange_weak(0, 1, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
            {
                return;
            }
        }
        // Mark the lock contended and sleep until it is released; taken this way it stays
        // marked, so its unlock wakes the next sleeper.
        while self.m_state.swap(2, Ordering::Acquire) != 0 {
            let key = self.m_state.as_ptr() as usize;
            unsafe {
                parking_lot_core::park(
                    key,
                    || self.m_state.load(Ordering::Relaxed) == 2,
                    || {},
                    |_, _| {},
                    parking_lot_core::DEFAULT_PARK_TOKEN,
                    None,
                );
            }
        }
    }

    /// Unlock the spin lock. Mirrors upstream `KSpinLock::Unlock()`.
    pub fn unlock(&self) {
        if self.m_state.swap(0, Ordering::Release) == 2 {
            let key = self.m_state.as_ptr() as usize;
            unsafe {
                parking_lot_core::unpark_one(key, |_| parking_lot_core::DEFAULT_UNPARK_TOKEN);
            }
        }
    }

    /// Try to lock. Returns true if the lock was acquired.
    /// Mirrors upstream `KSpinLock::TryLock()`.
    pub fn try_lock(&self) -> bool {
        self.m_state
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }
}

impl Default for KSpinLock {
    fn default() -> Self {
        Self::new()
    }
}

// Aliases matching upstream.
// Upstream has cache-line aligned and non-aligned variants; aliased here.
pub type KAlignedSpinLock = KSpinLock;
pub type KNotAlignedSpinLock = KSpinLock;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spin_lock_lock_unlock() {
        let lock = KSpinLock::new();
        lock.lock();
        lock.unlock();
    }

    #[test]
    fn test_spin_lock_try_lock() {
        let lock = KSpinLock::new();
        assert!(lock.try_lock());
        // Lock is held — try_lock should fail.
        assert!(!lock.try_lock());
        lock.unlock();
        // Now it should succeed again.
        assert!(lock.try_lock());
        lock.unlock();
    }

    #[test]
    fn test_spin_lock_contended_threads_sleep_and_wake() {
        use std::sync::atomic::AtomicU64;
        use std::sync::Arc;
        let lock = Arc::new(KSpinLock::new());
        let counter = Arc::new(AtomicU64::new(0));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let (lock, counter) = (lock.clone(), counter.clone());
                std::thread::spawn(move || {
                    for _ in 0..20_000 {
                        lock.lock();
                        // Non-atomic read-modify-write under the lock.
                        let value = counter.load(Ordering::Relaxed);
                        counter.store(value + 1, Ordering::Relaxed);
                        lock.unlock();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(counter.load(Ordering::Relaxed), 80_000);
        assert!(lock.try_lock());
        lock.unlock();
    }
}
