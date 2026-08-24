//! Host spin lock from upstream `common/spin_lock.h` and host implementations.

use std::sync::atomic::{AtomicU32, Ordering};

/// Four-byte spin lock storage shared with JIT-emitted lock sequences.
///
/// Rust atomics provide the acquire/release contract implemented by Eden's host-specific
/// generated lock routines while preserving the exact storage width they access.
#[repr(C)]
pub struct SpinLock {
    pub(crate) locked: AtomicU32,
}

impl SpinLock {
    pub fn new() -> Self {
        Self {
            locked: AtomicU32::new(0),
        }
    }

    pub fn lock(&self) {
        while self
            .locked
            .compare_exchange_weak(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::hint::spin_loop();
        }
    }

    pub fn unlock(&self) {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            self.locked.swap(0, Ordering::SeqCst);
            std::sync::atomic::fence(Ordering::SeqCst);
        }
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        self.locked.store(0, Ordering::Release);
    }
}

impl Default for SpinLock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_matches_upstream_volatile_int() {
        assert_eq!(std::mem::size_of::<SpinLock>(), 4);
        assert_eq!(std::mem::align_of::<SpinLock>(), 4);
    }
}
