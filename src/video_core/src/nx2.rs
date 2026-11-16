// SPDX-License-Identifier: GPL-3.0-or-later
//! Port of eden-nx2/src/video_core/nx2.h (header-only).

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

pub static G_NX2_AMPERE: AtomicBool = AtomicBool::new(false);

/// Lichii `Nx2WrittenPageBits` (nx2.h:50).
pub const NX2_WRITTEN_PAGE_BITS: u64 = 12;
const NX2_GPU_WRITTEN_WORDS: usize = (1usize << (36 - NX2_WRITTEN_PAGE_BITS)) / 64;

/// Device pages the GPU has written through the buffer cache (shader
/// stores, copies), ever: one bit per 4 KiB of the 36-bit device address
/// space (2 MiB). The bits never clear.
///
/// Lichii `g_nx2_gpu_written_pages` (nx2.h:45-51). ruzu marks it where Lichii
/// does, at the two places the buffer cache adds a range to its GPU-modified
/// ranges (`mark_written_buffer` and `dma_copy`), and reads it where this port
/// reaches (`BufferCache::constant_data_reaches_buffers`,
/// `RasterizerVulkan::accelerate_inline_to_memory`). Lichii's command list
/// page bitmap beside it has no ruzu counterpart.
pub static G_NX2_GPU_WRITTEN_PAGES: [AtomicU64; NX2_GPU_WRITTEN_WORDS] =
    [const { AtomicU64::new(0) }; NX2_GPU_WRITTEN_WORDS];

/// Lichii `Nx2MarkGpuWritten` (nx2.h:53-71): a word at a time.
pub fn nx2_mark_gpu_written(device_address: u64, size: u64) {
    if size == 0 {
        return;
    }
    let last = ((device_address + size - 1) >> NX2_WRITTEN_PAGE_BITS)
        .min(NX2_GPU_WRITTEN_WORDS as u64 * 64 - 1);
    let mut page = device_address >> NX2_WRITTEN_PAGE_BITS;
    while page <= last {
        let first_bit = page % 64;
        let bits = (64 - first_bit).min(last - page + 1);
        let mask = if bits == 64 { !0u64 } else { (1u64 << bits) - 1 } << first_bit;
        let word = &G_NX2_GPU_WRITTEN_PAGES[(page / 64) as usize];
        if word.load(Ordering::Relaxed) & mask != mask {
            word.fetch_or(mask, Ordering::Relaxed);
        }
        page += bits;
    }
}

/// False only when no page of `[device_address, device_address + size)` has
/// been GPU-written. Ranges past the bitmap answer true.
///
/// Lichii `Nx2MayBeGpuWritten` (nx2.h:120-137).
pub fn nx2_may_be_gpu_written(device_address: u64, size: u64) -> bool {
    if size == 0 {
        return false;
    }
    let first = device_address >> NX2_WRITTEN_PAGE_BITS;
    let last = device_address.wrapping_add(size).wrapping_sub(1) >> NX2_WRITTEN_PAGE_BITS;
    if last < first || last >= NX2_GPU_WRITTEN_WORDS as u64 * 64 {
        return true;
    }
    (first..=last).any(|page| {
        (G_NX2_GPU_WRITTEN_PAGES[(page / 64) as usize].load(Ordering::Relaxed) >> (page % 64)) & 1
            != 0
    })
}
pub const NX2_MAX_SEMAPHORE_REGIONS: usize = 32;
pub static G_NX2_SEMAPHORE_REGIONS: [AtomicU64; NX2_MAX_SEMAPHORE_REGIONS] =
    [const { AtomicU64::new(0) }; NX2_MAX_SEMAPHORE_REGIONS];

pub fn nx2_add_semaphore_region(start: u64) {
    for region in &G_NX2_SEMAPHORE_REGIONS {
        if region.load(Ordering::SeqCst) == start {
            return;
        }
    }
    for region in &G_NX2_SEMAPHORE_REGIONS {
        if region
            .compare_exchange(0, start, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return;
        }
    }
}

pub fn nx2_remove_semaphore_region(start: u64) {
    for region in &G_NX2_SEMAPHORE_REGIONS {
        let _ = region.compare_exchange(start, 0, Ordering::SeqCst, Ordering::SeqCst);
    }
}

pub fn nx2_semaphore_mirror(address: u64) -> u64 {
    for region in &G_NX2_SEMAPHORE_REGIONS {
        let start = region.load(Ordering::Relaxed);
        if start == 0 {
            continue;
        }
        if address >= start.wrapping_add(0x1000) && address < start.wrapping_add(0x2000) {
            return address.wrapping_sub(0x1000);
        }
    }
    0
}

struct Waiter {
    // Upstream stores const void* purely as an identity, never dereferences it.
    owner: usize,
    address: u64,
    value: u32,
    operation: u32,
    latched: bool,
}

pub struct Nx2SemaphoreWaiters {
    // The vector and its protecting mutex remain owned by the upstream class.
    waiters: Mutex<Vec<Waiter>>,
    count: AtomicUsize,
}

impl Nx2SemaphoreWaiters {
    pub const ACQUIRE_EQUAL: u32 = 0x1;
    pub const ACQUIRE_GEQUAL: u32 = 0x4;
    pub const ACQUIRE_MASK: u32 = 0x8;

    pub fn get() -> &'static Self {
        static WAITERS: OnceLock<Nx2SemaphoreWaiters> = OnceLock::new();
        WAITERS.get_or_init(|| Self {
            waiters: Mutex::new(Vec::new()),
            count: AtomicUsize::new(0),
        })
    }

    pub fn add(&self, owner: usize, device_address: u64, value: u32, operation: u32) {
        let mut waiters = self.waiters.lock().unwrap();
        waiters.retain(|w| w.owner != owner);
        waiters.push(Waiter {
            owner,
            address: device_address,
            value,
            operation,
            latched: false,
        });
        self.count.store(waiters.len(), Ordering::Release);
    }

    pub fn remove(&self, owner: usize) {
        if self.count.load(Ordering::Acquire) == 0 {
            return;
        }
        let mut waiters = self.waiters.lock().unwrap();
        waiters.retain(|w| w.owner != owner);
        self.count.store(waiters.len(), Ordering::Release);
    }

    pub fn latched(&self, owner: usize) -> bool {
        if self.count.load(Ordering::Acquire) == 0 {
            return false;
        }
        self.waiters
            .lock()
            .unwrap()
            .iter()
            .any(|w| w.owner == owner && w.latched)
    }

    pub fn on_write(&self, device_address: u64, value: u32) {
        if self.count.load(Ordering::Acquire) == 0 {
            return;
        }
        let mut waiters = self.waiters.lock().unwrap();
        for w in waiters.iter_mut() {
            if w.address == device_address && !w.latched && Self::matches(w, value) {
                w.latched = true;
            }
        }
    }

    fn matches(w: &Waiter, current: u32) -> bool {
        match w.operation {
            Self::ACQUIRE_EQUAL => current == w.value,
            Self::ACQUIRE_GEQUAL => current >= w.value,
            Self::ACQUIRE_MASK => (current & w.value) != 0,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_written_pages_are_seen_across_words_and_never_cleared() {
        // Process-wide bitmap: addresses no other test uses.
        let base = 0xC_7000_0000u64;
        assert!(!nx2_may_be_gpu_written(base, 0x10_0000));
        assert!(!nx2_may_be_gpu_written(base, 0));
        // 70 pages starting mid-word: two words touched.
        nx2_mark_gpu_written(base + 0x3_0ff0, 70 * 0x1000);
        assert!(nx2_may_be_gpu_written(base + 0x3_0000, 1));
        assert!(nx2_may_be_gpu_written(base + 0x3_0000 + 70 * 0x1000, 1));
        assert!(!nx2_may_be_gpu_written(base + 0x3_0000 + 71 * 0x1000, 0x1000));
        assert!(!nx2_may_be_gpu_written(base, 0x3_0000));
        // A range reaching it from below.
        assert!(nx2_may_be_gpu_written(base, 0x3_0001));
        // Past the 36-bit bitmap: may be written.
        assert!(nx2_may_be_gpu_written(1u64 << 36, 4));
        nx2_mark_gpu_written(base + 0x3_0000, 0);
        assert!(nx2_may_be_gpu_written(base + 0x3_0ff0, 4));
    }

    #[test]
    fn region_mirroring_boundaries_capacity_and_removal() {
        nx2_add_semaphore_region(0);
        for i in 1..=32 {
            nx2_add_semaphore_region(i * 0x10000);
            nx2_add_semaphore_region(i * 0x10000);
        }
        nx2_add_semaphore_region(0x400000);
        assert_eq!(nx2_semaphore_mirror(0x401000), 0);
        for i in 1..=32 {
            let start = i * 0x10000;
            assert_eq!(nx2_semaphore_mirror(start + 0xfff), 0);
            assert_eq!(nx2_semaphore_mirror(start + 0x1000), start);
            assert_eq!(nx2_semaphore_mirror(start + 0x1fff), start + 0xfff);
            assert_eq!(nx2_semaphore_mirror(start + 0x2000), 0);
            nx2_remove_semaphore_region(start);
            assert_eq!(nx2_semaphore_mirror(start + 0x1000), 0);
        }
        // C++ unsigned addition wraps at the top of the address space.
        nx2_add_semaphore_region(u64::MAX - 0x7ff);
        assert_eq!(nx2_semaphore_mirror(0x800), 0xffff_ffff_ffff_f800);
        nx2_remove_semaphore_region(u64::MAX - 0x7ff);
    }

    #[test]
    fn writes_latch_matching_waiters_until_replaced_or_removed() {
        let waiters = Nx2SemaphoreWaiters {
            waiters: Mutex::new(Vec::new()),
            count: AtomicUsize::new(0),
        };
        waiters.add(1, 0x1000, 5, Nx2SemaphoreWaiters::ACQUIRE_EQUAL);
        waiters.on_write(0x2000, 5);
        assert!(!waiters.latched(1));
        waiters.on_write(0x1000, 5);
        waiters.on_write(0x1000, 6);
        assert!(waiters.latched(1));
        waiters.add(1, 0x2000, u32::MAX, Nx2SemaphoreWaiters::ACQUIRE_GEQUAL);
        assert!(!waiters.latched(1));
        waiters.on_write(0x2000, 0);
        assert!(!waiters.latched(1));
        waiters.on_write(0x2000, u32::MAX);
        assert!(waiters.latched(1));
        waiters.add(2, 0x2000, 0x80, Nx2SemaphoreWaiters::ACQUIRE_MASK);
        waiters.add(3, 0x2000, 0xff, 0xff);
        waiters.on_write(0x2000, 0x7f);
        assert!(!waiters.latched(2));
        waiters.on_write(0x2000, 0xff);
        assert!(waiters.latched(2));
        assert!(!waiters.latched(3));
        waiters.remove(1);
        waiters.remove(2);
        waiters.remove(3);
        assert!(!waiters.latched(1));
        assert_eq!(waiters.count.load(Ordering::Acquire), 0);
    }
}
