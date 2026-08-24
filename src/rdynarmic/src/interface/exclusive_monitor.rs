// Port of dynarmic/interface/exclusive_monitor.h and the host exclusive_monitor.cpp files.
//
// Multi-core exclusive monitor for ARM load-exclusive/store-exclusive
// synchronization. Each processor marks an address via ReadAndMark, then
// attempts a store via DoExclusiveOperation which succeeds only if the
// address is still exclusively held.

use std::mem::MaybeUninit;

use crate::common::spin_lock::SpinLock;

/// 128-bit value storage (two u64s), matching Dynarmic::Vector.
pub type Vector = [u64; 2];

const RESERVATION_GRANULE_MASK: u64 = 0xFFFF_FFFF_FFFF_FFFF;
const INVALID_EXCLUSIVE_ADDRESS: u64 = 0xDEAD_DEAD_DEAD_DEAD;
const MAX_NUM_CPU_CORES: usize = 4;

/// Multi-core exclusive monitor.
///
/// Matches `Dynarmic::ExclusiveMonitor` — provides ReadAndMark / DoExclusiveOperation
/// for implementing ARM LDXR/STXR (load-exclusive/store-exclusive) across cores.
pub struct ExclusiveMonitor {
    pub(crate) lock: SpinLock,
    pub(crate) exclusive_addresses: Vec<u64>,
    pub(crate) exclusive_values: Vec<Vector>,
}

impl ExclusiveMonitor {
    /// Create a new ExclusiveMonitor for `processor_count` cores.
    ///
    /// Matches `Dynarmic::ExclusiveMonitor::ExclusiveMonitor(size_t processor_count)`.
    pub fn new(processor_count: usize) -> Self {
        assert!(processor_count <= MAX_NUM_CPU_CORES);
        Self {
            lock: SpinLock::new(),
            exclusive_addresses: vec![INVALID_EXCLUSIVE_ADDRESS; processor_count],
            exclusive_values: vec![[0u64; 2]; processor_count],
        }
    }

    /// Number of processors this monitor supports.
    pub fn get_processor_count(&self) -> usize {
        self.exclusive_addresses.len()
    }

    /// Mark a region as exclusive for `processor_id` and read the value via `op`.
    ///
    /// Matches `Dynarmic::ExclusiveMonitor::ReadAndMark<T>`.
    /// The closure `op` performs the actual memory read.
    pub fn read_and_mark<T: Copy>(
        &mut self,
        processor_id: usize,
        address: u64,
        op: impl FnOnce() -> T,
    ) -> T {
        assert!(std::mem::size_of::<T>() <= std::mem::size_of::<Vector>());
        let masked_address = address & RESERVATION_GRANULE_MASK;

        self.lock.lock();
        self.exclusive_addresses[processor_id] = masked_address;
        let value = op();
        // Store exactly sizeof(T) bytes, matching upstream's memcpy.
        unsafe {
            std::ptr::copy_nonoverlapping(
                &value as *const T as *const u8,
                self.exclusive_values[processor_id].as_mut_ptr() as *mut u8,
                std::mem::size_of::<T>(),
            );
        }
        self.lock.unlock();
        value
    }

    /// Attempt an exclusive store for `processor_id` at `address`.
    ///
    /// Matches `Dynarmic::ExclusiveMonitor::DoExclusiveOperation<T>`.
    /// If this processor still has exclusive access, calls `op(saved_value)`
    /// which should perform a compare-and-swap write. Returns true if the
    /// operation succeeded.
    pub fn do_exclusive_operation<T: Copy>(
        &mut self,
        processor_id: usize,
        address: u64,
        op: impl FnOnce(T) -> bool,
    ) -> bool {
        assert!(std::mem::size_of::<T>() <= std::mem::size_of::<Vector>());
        if !self.check_and_clear(processor_id, address) {
            return false;
        }

        // Extract exactly sizeof(T) bytes, matching upstream's memcpy.
        let mut saved_value = MaybeUninit::<T>::uninit();
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.exclusive_values[processor_id].as_ptr() as *const u8,
                saved_value.as_mut_ptr() as *mut u8,
                std::mem::size_of::<T>(),
            );
        }
        let result = op(unsafe { saved_value.assume_init() });

        self.lock.unlock();
        result
    }

    /// Clear all exclusive state for all processors.
    ///
    /// Matches `Dynarmic::ExclusiveMonitor::Clear()`.
    pub fn clear(&mut self) {
        self.lock.lock();
        self.exclusive_addresses.fill(INVALID_EXCLUSIVE_ADDRESS);
        self.lock.unlock();
    }

    /// Clear exclusive state for a single processor.
    ///
    /// Matches `Dynarmic::ExclusiveMonitor::ClearProcessor(processor_id)`.
    pub fn clear_processor(&mut self, processor_id: usize) {
        self.lock.lock();
        self.exclusive_addresses[processor_id] = INVALID_EXCLUSIVE_ADDRESS;
        self.lock.unlock();
    }

    /// Check if `processor_id` holds exclusive access to `address`.
    /// If so, clears all processors that share the same masked address
    /// and returns true (lock remains held for DoExclusiveOperation).
    /// If not, returns false (lock is released).
    ///
    /// Matches `Dynarmic::ExclusiveMonitor::CheckAndClear`.
    fn check_and_clear(&mut self, processor_id: usize, address: u64) -> bool {
        let masked_address = address & RESERVATION_GRANULE_MASK;

        self.lock.lock();
        if self.exclusive_addresses[processor_id] != masked_address {
            self.lock.unlock();
            return false;
        }

        // Clear all processors that share this address.
        for addr in &mut self.exclusive_addresses {
            if *addr == masked_address {
                *addr = INVALID_EXCLUSIVE_ADDRESS;
            }
        }
        // Lock remains held — caller (DoExclusiveOperation) will unlock.
        true
    }
}

// SAFETY: ExclusiveMonitor uses a SpinLock for thread safety, and its vectors are never resized.
unsafe impl Send for ExclusiveMonitor {}
unsafe impl Sync for ExclusiveMonitor {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_exclusive_rw() {
        let mut monitor = ExclusiveMonitor::new(4);
        assert_eq!(monitor.get_processor_count(), 4);

        // Processor 0 reads address 0x1000.
        let val: u32 = monitor.read_and_mark(0, 0x1000, || 42u32);
        assert_eq!(val, 42);

        // Processor 0 writes exclusively — should succeed.
        let success = monitor.do_exclusive_operation::<u32>(0, 0x1000, |saved| {
            assert_eq!(saved, 42);
            true // Simulate successful CAS
        });
        assert!(success);
    }

    #[test]
    fn test_exclusive_fails_after_clear() {
        let mut monitor = ExclusiveMonitor::new(2);

        monitor.read_and_mark::<u32>(0, 0x2000, || 99);
        monitor.clear_processor(0);

        let success = monitor.do_exclusive_operation::<u32>(0, 0x2000, |_| true);
        assert!(!success);
    }

    #[test]
    fn test_exclusive_cross_processor_invalidation() {
        let mut monitor = ExclusiveMonitor::new(2);

        // Both processors mark the same address.
        monitor.read_and_mark::<u32>(0, 0x3000, || 10);
        monitor.read_and_mark::<u32>(1, 0x3000, || 20);

        // Processor 0 does exclusive op — clears both.
        let success = monitor.do_exclusive_operation::<u32>(0, 0x3000, |saved| {
            assert_eq!(saved, 10);
            true
        });
        assert!(success);

        // Processor 1's exclusive should now fail.
        let success = monitor.do_exclusive_operation::<u32>(1, 0x3000, |_| true);
        assert!(!success);
    }

    #[test]
    fn test_exclusive_different_addresses() {
        let mut monitor = ExclusiveMonitor::new(2);

        monitor.read_and_mark::<u64>(0, 0x4000, || 100);
        monitor.read_and_mark::<u64>(1, 0x5000, || 200);

        // Both should succeed since different addresses.
        let s0 = monitor.do_exclusive_operation::<u64>(0, 0x4000, |saved| {
            assert_eq!(saved, 100);
            true
        });
        let s1 = monitor.do_exclusive_operation::<u64>(1, 0x5000, |saved| {
            assert_eq!(saved, 200);
            true
        });
        assert!(s0);
        assert!(s1);
    }

    #[test]
    fn test_exclusive_128bit() {
        let mut monitor = ExclusiveMonitor::new(1);

        let val: u128 = monitor.read_and_mark(0, 0x6000, || {
            0xDEAD_BEEF_CAFE_BABEu128 << 64 | 0x1234_5678_9ABC_DEF0u128
        });
        assert_eq!(
            val,
            0xDEAD_BEEF_CAFE_BABEu128 << 64 | 0x1234_5678_9ABC_DEF0u128
        );

        let success = monitor.do_exclusive_operation::<u128>(0, 0x6000, |saved| {
            assert_eq!(saved, val);
            true
        });
        assert!(success);
    }

    #[test]
    fn test_clear_all() {
        let mut monitor = ExclusiveMonitor::new(3);

        monitor.read_and_mark::<u32>(0, 0x7000, || 1);
        monitor.read_and_mark::<u32>(1, 0x8000, || 2);
        monitor.read_and_mark::<u32>(2, 0x9000, || 3);

        monitor.clear();

        assert!(!monitor.do_exclusive_operation::<u32>(0, 0x7000, |_| true));
        assert!(!monitor.do_exclusive_operation::<u32>(1, 0x8000, |_| true));
        assert!(!monitor.do_exclusive_operation::<u32>(2, 0x9000, |_| true));
    }

    #[test]
    #[should_panic]
    fn processor_count_cannot_exceed_upstream_capacity() {
        let _monitor = ExclusiveMonitor::new(MAX_NUM_CPU_CORES + 1);
    }
}
