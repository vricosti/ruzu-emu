//! Port of zuyu/src/core/gpu_dirty_memory_manager.h
//! Status: COMPLET
//! Derniere synchro: 2026-03-11
//!
//! GPU dirty memory tracking. Collects memory write notifications and allows
//! gathering them in batches for GPU page table invalidation.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::device_memory_manager::DEVICE_PAGEBITS;

/// Tracks dirty (modified) memory regions for GPU page table synchronization.
///
/// Corresponds to the C++ `GPUDirtyMemoryManager` class.
///
/// The manager uses a lock-free approach for the common case (single page being
/// dirtied repeatedly), with a fallback to a mutex-protected buffer when pages
/// change. Uses double-buffering to allow concurrent collection and gathering.
pub struct GpuDirtyMemoryManager {
    /// Current transform address, stored as a packed u64 (address:u32 | mask:u32).
    /// Accessed atomically for lock-free collection.
    current: AtomicU64,

    /// Back buffer for accumulated transforms (producer side). This mutex is
    /// the Rust counterpart of upstream's `guard`.
    back_buffer: Mutex<Vec<TransformAddress>>,

    /// Front buffer for gathered transforms (consumer side). Its separate
    /// mutex permits `gather` to take `&self` without blocking producers while
    /// callbacks consume the gathered ranges.
    front_buffer: Mutex<Vec<TransformAddress>>,
}

/// Packed address + dirty mask pair.
/// The address identifies a half-page (page_bits = DEVICE_PAGEBITS - 1),
/// and the mask tracks 64-byte aligned dirty sub-regions within that half-page.
#[derive(Debug, Clone, Copy)]
#[repr(C, align(8))]
struct TransformAddress {
    address: u32,
    mask: u32,
}

impl TransformAddress {
    /// Pack into a u64 for atomic operations.
    fn to_u64(self) -> u64 {
        (self.address as u64) | ((self.mask as u64) << 32)
    }

    /// Unpack from a u64.
    fn from_u64(val: u64) -> Self {
        Self {
            address: val as u32,
            mask: (val >> 32) as u32,
        }
    }
}

// Constants matching upstream
const PAGE_BITS: usize = DEVICE_PAGEBITS - 1;
const PAGE_SIZE: usize = 1 << PAGE_BITS;
const PAGE_MASK: usize = PAGE_SIZE - 1;

const ALIGN_BITS: usize = 6;
const ALIGN_SIZE: usize = 1 << ALIGN_BITS;
const ALIGN_MASK: usize = ALIGN_SIZE - 1;

const DEFAULT_TRANSFORM: TransformAddress = TransformAddress {
    address: !0u32,
    mask: 0u32,
};

impl GpuDirtyMemoryManager {
    /// Create a new dirty memory manager.
    pub fn new() -> Self {
        Self {
            current: AtomicU64::new(DEFAULT_TRANSFORM.to_u64()),
            back_buffer: Mutex::new(Vec::with_capacity(256)),
            front_buffer: Mutex::new(Vec::with_capacity(256)),
        }
    }

    /// Collect a dirty memory notification for the given physical address and size.
    ///
    /// This is the hot path, called from GPU memory write handlers. Uses lock-free
    /// atomic operations for the common case where the same page is being dirtied
    /// repeatedly.
    pub fn collect(&self, mut address: u64, mut size: usize) {
        while size != 0 {
            let page_offset = (address as usize) & PAGE_MASK;
            let page_size = (PAGE_SIZE - page_offset).min(size);
            self.collect_transform(self.build_transform(address, page_size));
            address = address.wrapping_add(page_size as u64);
            size -= page_size;
        }
    }

    fn collect_transform(&self, t: TransformAddress) {
        let mut tmp;
        let mut original;

        loop {
            tmp = TransformAddress::from_u64(self.current.load(Ordering::Acquire));
            original = tmp;

            if tmp.address != t.address {
                if Self::is_valid(tmp.address as u64) {
                    let mut back_buffer = self.back_buffer.lock().unwrap();
                    back_buffer.push(tmp);
                    self.current.swap(t.to_u64(), Ordering::Relaxed);
                    return;
                }
                tmp.address = t.address;
                tmp.mask = 0;
            }

            if (tmp.mask | t.mask) == tmp.mask {
                return;
            }
            tmp.mask |= t.mask;

            match self.current.compare_exchange_weak(
                original.to_u64(),
                tmp.to_u64(),
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(_) => continue,
            }
        }
    }

    /// False when `gather` would find nothing. Every `collect` that adds a
    /// write leaves its transform in `current` (by compare-exchange, or by
    /// swap after pushing the one before to `back_buffer` under its lock),
    /// and only `gather`, which takes `back_buffer` too, empties `current`
    /// again. An empty transform has no mask bits. Without the locks: a
    /// collect it misses is one the caller may order after itself.
    ///
    /// Lichii `GPUDirtyMemoryManager::MayHavePending`
    /// (gpu_dirty_memory_manager.h:83-90); Eden has no counterpart.
    pub fn may_have_pending(&self) -> bool {
        TransformAddress::from_u64(self.current.load(Ordering::Acquire)).mask != 0
    }

    /// Gather all accumulated dirty regions and invoke the callback for each
    /// contiguous dirty range.
    ///
    /// The callback receives (physical_address, size) pairs.
    pub fn gather(&self, callback: &mut dyn FnMut(u64, usize)) {
        let mut front_buffer = self.front_buffer.lock().unwrap();
        {
            let mut back_buffer = self.back_buffer.lock().unwrap();
            let t = TransformAddress::from_u64(
                self.current
                    .swap(DEFAULT_TRANSFORM.to_u64(), Ordering::Relaxed),
            );
            core::mem::swap(&mut *front_buffer, &mut *back_buffer);
            if Self::is_valid(t.address as u64) {
                front_buffer.push(t);
            }
        }

        for transform in front_buffer.iter() {
            Self::for_each_run(transform, callback);
        }

        front_buffer.clear();
    }

    /// Calls `callback` for each write collected and not yet applied, without
    /// consuming them: those still collecting and those a `gather` is handing
    /// out.
    ///
    /// Lichii `GPUDirtyMemoryManager::ForEachPending`
    /// (`src/core/gpu_dirty_memory_manager.h:46-62`). The `front_buffer`
    /// mutex is the Rust counterpart of Lichii's `gather_guard` (see
    /// `gather`), taken before `back_buffer` (`guard`) as there.
    pub fn for_each_pending(&self, callback: &mut dyn FnMut(u64, usize)) {
        let pending = {
            let front_buffer = self.front_buffer.lock().unwrap();
            let back_buffer = self.back_buffer.lock().unwrap();
            let mut pending = back_buffer.clone();
            pending.extend(front_buffer.iter().copied());
            let t = TransformAddress::from_u64(self.current.load(Ordering::Acquire));
            if Self::is_valid(t.address as u64) {
                pending.push(t);
            }
            pending
        };
        for transform in &pending {
            Self::for_each_run(transform, callback);
        }
    }

    /// The runs of 64-byte blocks a transform's mask holds.
    ///
    /// Lichii `GPUDirtyMemoryManager::ForEachRun`
    /// (`gpu_dirty_memory_manager.h:159-172`); the loop body is the one
    /// `gather` had inline (Eden `Gather`).
    fn for_each_run(transform: &TransformAddress, callback: &mut dyn FnMut(u64, usize)) {
        let mut offset: usize = 0;
        let mut mask = transform.mask as u64;

        while mask != 0 {
            let empty_bits = mask.trailing_zeros() as usize;
            offset += empty_bits << ALIGN_BITS;
            mask >>= empty_bits;

            let continuous_bits = (!mask).trailing_zeros() as usize;
            let phys_addr = ((transform.address as u64) << PAGE_BITS) + offset as u64;
            callback(phys_addr, continuous_bits << ALIGN_BITS);

            if continuous_bits < ALIGN_SIZE {
                mask >>= continuous_bits;
            } else {
                mask = 0;
            }
            offset += continuous_bits << ALIGN_BITS;
        }
    }

    // --- Private helpers ---

    fn is_valid(address: u64) -> bool {
        address < (1u64 << 39)
    }

    fn create_mask(top_bit: usize, minor_bit: usize) -> u32 {
        // Upstream `CreateMask<u32>`:
        //
        //     mask <<= (sizeof(T) * 8 - top_bit);
        //     mask >>= (sizeof(T) * 8 - top_bit);
        //     mask >>= minor_bit;
        //     mask <<= minor_bit;
        //
        // `top_bit` is `(minor_address + size + align_mask) >> align_bits`.
        // Production calls are split at each 2 KiB transform-page boundary,
        // keeping top_bit within the 32-bit mask. The clamp remains defensive
        // for direct helper use because reproducing upstream's oversized shift
        // would be undefined behavior in C++ and a panic in Rust.
        //
        // DELIBERATE DIVERGENCE. Upstream cannot be reproduced bit-for-bit here
        // without reproducing that UB, and its x86 behaviour is not what the
        // code intends:
        //
        //   * top_bit = 64 -> shift count (32-64) & 31 == 0, so the mask stays
        //     all-ones and only `minor_bit` is honoured. Same as the clamp below.
        //   * top_bit = 33 -> shift count (32-33) & 31 == 31, so `!0 << 31 >> 31`
        //     leaves a *single* bit set while 33 units are being written. That
        //     under-reports dirty memory, which is a missed invalidation.
        //
        // Rust panics on the underflow instead ("attempt to subtract with
        // overflow"). Clamping keeps this helper total, while `collect` ensures
        // no bytes in following transform pages are discarded.
        const WIDTH: usize = u32::BITS as usize;
        if top_bit == 0 || minor_bit >= WIDTH {
            return 0;
        }
        let top_bit = top_bit.min(WIDTH);
        let shift = WIDTH - top_bit;

        let mut mask: u32 = !0u32;
        mask <<= shift;
        mask >>= shift;
        mask >>= minor_bit;
        mask <<= minor_bit;
        mask
    }

    fn build_transform(&self, address: u64, size: usize) -> TransformAddress {
        let minor_address = (address as usize) & PAGE_MASK;
        let minor_bit = minor_address >> ALIGN_BITS;
        let top_bit = (minor_address + size + ALIGN_MASK) >> ALIGN_BITS;
        TransformAddress {
            address: (address >> PAGE_BITS) as u32,
            mask: Self::create_mask(top_bit, minor_bit),
        }
    }
}

impl Default for GpuDirtyMemoryManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_construction() {
        let mgr = GpuDirtyMemoryManager::new();
        let current = TransformAddress::from_u64(mgr.current.load(Ordering::Relaxed));
        assert_eq!(current.address, !0u32);
        assert_eq!(current.mask, 0);
    }

    #[test]
    fn may_have_pending_follows_collect_and_gather() {
        let mgr = GpuDirtyMemoryManager::new();
        assert!(!mgr.may_have_pending());
        mgr.collect(0x1_0040, 0x40);
        assert!(mgr.may_have_pending());
        // A write on another page pushes the first to back_buffer; `current`
        // still shows a write.
        mgr.collect(0x3_0000, 0x10);
        assert!(mgr.may_have_pending());
        let mut gathered = 0;
        mgr.gather(&mut |_, _| gathered += 1);
        assert_eq!(gathered, 2);
        assert!(!mgr.may_have_pending());
        // A write spanning two transform pages is still seen.
        mgr.collect(0x4_07f0, 0x20);
        assert!(mgr.may_have_pending());
    }

    #[test]
    fn for_each_pending_reports_collected_writes_without_consuming_them() {
        let mgr = GpuDirtyMemoryManager::new();
        mgr.collect(0x1_0040, 0x40); // pushed to back_buffer by the next page
        mgr.collect(0x2_0080, 0x80); // still in `current`
        let mut pending = Vec::new();
        mgr.for_each_pending(&mut |address, size| pending.push((address, size)));
        pending.sort_unstable();
        assert_eq!(pending, vec![(0x1_0040, 0x40), (0x2_0080, 0x80)]);

        let mut gathered = Vec::new();
        mgr.gather(&mut |address, size| gathered.push((address, size)));
        gathered.sort_unstable();
        assert_eq!(gathered, pending);

        let mut after = Vec::new();
        mgr.for_each_pending(&mut |address, size| after.push((address, size)));
        assert!(after.is_empty());
    }

    #[test]
    fn create_mask_clamps_a_top_bit_past_the_mask_width() {
        // Direct helper use can produce top_bit above 32. Production `collect`
        // splits first, but this must remain panic-free as a defensive boundary.
        assert_eq!(GpuDirtyMemoryManager::create_mask(64, 0), !0u32);
        assert_eq!(GpuDirtyMemoryManager::create_mask(33, 0), !0u32);
        // Clamping keeps the low bits masked off, marking only minor_bit..end.
        assert_eq!(GpuDirtyMemoryManager::create_mask(64, 4), !0u32 << 4);
    }

    #[test]
    fn create_mask_handles_a_minor_bit_past_the_mask_width() {
        // Would shift a u32 by >= 32, which also panics in Rust.
        assert_eq!(GpuDirtyMemoryManager::create_mask(64, 32), 0);
        assert_eq!(GpuDirtyMemoryManager::create_mask(64, 99), 0);
    }

    #[test]
    fn collect_splits_a_write_at_the_transform_page_boundary() {
        let mgr = GpuDirtyMemoryManager::new();
        let start = (PAGE_SIZE - ALIGN_SIZE) as u64;
        mgr.collect(start, ALIGN_SIZE * 2);

        let mut results = Vec::new();
        mgr.gather(&mut |address, size| results.push((address, size)));

        assert_eq!(
            results,
            vec![(start, ALIGN_SIZE), (PAGE_SIZE as u64, ALIGN_SIZE),]
        );
    }

    #[test]
    fn collect_preserves_every_transform_page_in_a_large_write() {
        let mgr = GpuDirtyMemoryManager::new();
        let start = (PAGE_SIZE - ALIGN_SIZE) as u64;
        mgr.collect(start, PAGE_SIZE + ALIGN_SIZE * 2);

        let mut results = Vec::new();
        mgr.gather(&mut |address, size| results.push((address, size)));

        assert_eq!(
            results,
            vec![
                (start, ALIGN_SIZE),
                (PAGE_SIZE as u64, PAGE_SIZE),
                ((PAGE_SIZE * 2) as u64, ALIGN_SIZE),
            ]
        );
    }

    #[test]
    fn collect_does_not_wait_for_gather_callbacks() {
        use std::sync::{mpsc, Arc};
        use std::time::Duration;

        let manager = Arc::new(GpuDirtyMemoryManager::new());
        manager.collect(0, ALIGN_SIZE);

        let gather_manager = Arc::clone(&manager);
        let (callback_entered_tx, callback_entered_rx) = mpsc::channel();
        let (release_callback_tx, release_callback_rx) = mpsc::channel();
        let gather_thread = std::thread::spawn(move || {
            gather_manager.gather(&mut |_, _| {
                callback_entered_tx.send(()).unwrap();
                release_callback_rx.recv().unwrap();
            });
        });

        callback_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("gather callback did not start");

        let collect_manager = Arc::clone(&manager);
        let (collect_done_tx, collect_done_rx) = mpsc::channel();
        let collect_thread = std::thread::spawn(move || {
            collect_manager.collect(PAGE_SIZE as u64, ALIGN_SIZE);
            collect_done_tx.send(()).unwrap();
        });

        collect_done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("collect blocked while gather callback was running");
        release_callback_tx.send(()).unwrap();
        collect_thread.join().unwrap();
        gather_thread.join().unwrap();
    }

    #[test]
    fn test_create_mask() {
        // Full mask
        let m = GpuDirtyMemoryManager::create_mask(32, 0);
        assert_eq!(m, !0u32);

        // Single bit
        let m = GpuDirtyMemoryManager::create_mask(1, 0);
        assert_eq!(m, 1);

        // Empty
        let m = GpuDirtyMemoryManager::create_mask(0, 0);
        assert_eq!(m, 0);
    }

    #[test]
    fn test_transform_address_round_trip() {
        let t = TransformAddress {
            address: 0x12345678,
            mask: 0xABCDEF01,
        };
        let packed = t.to_u64();
        let unpacked = TransformAddress::from_u64(packed);
        assert_eq!(unpacked.address, t.address);
        assert_eq!(unpacked.mask, t.mask);
    }

    #[test]
    fn transform_address_matches_upstream_layout() {
        assert_eq!(core::mem::size_of::<TransformAddress>(), 8);
        assert_eq!(core::mem::align_of::<TransformAddress>(), 8);
    }

    #[test]
    fn test_collect_and_gather() {
        let mgr = GpuDirtyMemoryManager::new();

        // Collect a single dirty region
        mgr.collect(0x1000, 64);

        let mut results = Vec::new();
        mgr.gather(&mut |addr, size| {
            results.push((addr, size));
        });

        assert_eq!(results, vec![(0x1000, 64)]);
    }
}
