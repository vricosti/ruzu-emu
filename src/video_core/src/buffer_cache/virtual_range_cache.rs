// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//! Port of Eden's `video_core/buffer_cache/virtual_range_cache.h`
//! (Eden a538cd9aff).
//! Status: COMPLET
//! Derniere synchro: 2026-09-11
//!
//! Resolves the host-contiguous segments backing a GPU virtual range so a
//! storage buffer spanning non-contiguous device pages can be bound as one
//! buffer (see `Vulkan::MultiRangeBufferCache`). Unmaps are recorded from the
//! GPU thread through `unmap` (mutex + atomic flag, no buffer-cache lock) and
//! applied lazily on the next `query`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use smallvec::SmallVec;

use super::buffer_cache_base::GpuMemoryAccess;

/// Upstream `VideoCommon::VirtualSegment`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualSegment {
    pub gpu_addr: u64,
    pub device_addr: u64,
    pub size: u32,
}

/// Upstream `boost::container::small_vector<VirtualSegment, 8>`.
pub type VirtualSegments = SmallVec<[VirtualSegment; 8]>;

/// Upstream `VideoCommon::VirtualRangeCache`.
pub struct VirtualRangeCache {
    entries: HashMap<u64, Entry>,
    /// Upstream `deferred` + `deferred_overflow`, both guarded by
    /// `deferred_mutex`.
    deferred: Mutex<DeferredState>,
    has_deferred: AtomicBool,
}

struct Entry {
    segments: VirtualSegments,
    as_id: usize,
    gpu_addr: u64,
    size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DeferredUnmap {
    as_id: usize,
    gpu_addr: u64,
    size: u64,
}

#[derive(Default)]
struct DeferredState {
    pending: Vec<DeferredUnmap>,
    overflow: bool,
}

impl Default for VirtualRangeCache {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtualRangeCache {
    pub const MAX_ENTRIES: usize = 8192;
    pub const MAX_DEFERRED: usize = 4096;

    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            deferred: Mutex::new(DeferredState::default()),
            has_deferred: AtomicBool::new(false),
        }
    }

    /// Upstream `Query`: returns the segments covering `[gpu_addr, gpu_addr + size)`.
    /// The list is empty when the range is not fully mapped or not contiguous in
    /// GPU space; a single segment means the range is host-contiguous.
    pub fn query(
        &mut self,
        memory: &dyn GpuMemoryAccess,
        gpu_addr: u64,
        size: u32,
    ) -> &VirtualSegments {
        if self.has_deferred.load(Ordering::Acquire) {
            self.apply_deferred();
        }
        if self.entries.len() > Self::MAX_ENTRIES {
            self.entries.clear();
        }
        let as_id = memory.get_id();
        let key = Self::make_key(as_id, gpu_addr);
        let hit = self.entries.get(&key).is_some_and(|entry| {
            entry.as_id == as_id && entry.gpu_addr == gpu_addr && entry.size == size
        });
        if hit {
            return &self.entries[&key].segments;
        }
        let mut entry = Entry {
            segments: VirtualSegments::new(),
            as_id,
            gpu_addr,
            size,
        };
        let ranges = memory.get_submapped_range(gpu_addr, u64::from(size));
        let mut expected = gpu_addr;
        let mut contiguous = true;
        for (range_addr, range_size) in ranges {
            if range_addr != expected || range_size == 0 {
                contiguous = false;
                break;
            }
            let device_addr = memory.gpu_to_cpu_address(range_addr);
            let Some(device_addr) = device_addr.filter(|addr| *addr != 0) else {
                contiguous = false;
                break;
            };
            if range_size > u64::from(u32::MAX) {
                contiguous = false;
                break;
            }
            entry.segments.push(VirtualSegment {
                gpu_addr: range_addr,
                device_addr,
                size: range_size as u32,
            });
            expected = expected.wrapping_add(range_size);
        }
        if !contiguous || expected != gpu_addr.wrapping_add(u64::from(size)) {
            entry.segments.clear();
        }
        &self.entries.entry(key).insert_entry(entry).into_mut().segments
    }

    /// Upstream `Unmap`: records a GPU unmap to invalidate overlapping
    /// entries on the next query. Callable without the buffer-cache lock.
    pub fn unmap(&self, as_id: usize, gpu_addr: u64, size: u64) {
        if size == 0 {
            return;
        }
        {
            let mut deferred = self.deferred.lock().unwrap();
            if let Some(last) = deferred.pending.last_mut() {
                if last.as_id == as_id && last.gpu_addr.wrapping_add(last.size) == gpu_addr {
                    last.size = last.size.wrapping_add(size);
                    self.has_deferred.store(true, Ordering::Release);
                    return;
                }
            }
            if deferred.pending.len() >= Self::MAX_DEFERRED {
                deferred.pending.clear();
                deferred.overflow = true;
            } else {
                deferred.pending.push(DeferredUnmap {
                    as_id,
                    gpu_addr,
                    size,
                });
            }
        }
        self.has_deferred.store(true, Ordering::Release);
    }

    /// Upstream `MakeKey`.
    fn make_key(as_id: usize, gpu_addr: u64) -> u64 {
        ((as_id as u64) << 48) ^ gpu_addr
    }

    /// Upstream `ApplyDeferred`.
    fn apply_deferred(&mut self) {
        let (pending, overflow) = {
            let mut deferred = self.deferred.lock().unwrap();
            self.has_deferred.store(false, Ordering::Release);
            let pending = std::mem::take(&mut deferred.pending);
            let overflow = std::mem::replace(&mut deferred.overflow, false);
            (pending, overflow)
        };
        if overflow {
            self.entries.clear();
            return;
        }
        if pending.is_empty() || self.entries.is_empty() {
            return;
        }
        self.entries.retain(|_, entry| {
            let entry_end = entry.gpu_addr.wrapping_add(u64::from(entry.size));
            let overlaps = pending.iter().any(|unmap| {
                unmap.as_id == entry.as_id
                    && entry.gpu_addr < unmap.gpu_addr.wrapping_add(unmap.size)
                    && unmap.gpu_addr < entry_end
            });
            !overlaps
        });
    }

    #[cfg(test)]
    fn cached_entries(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Address space with explicit page mappings: `(gpu_page, device_page)`.
    struct MappedMemory {
        id: usize,
        pages: Vec<(u64, u64)>,
    }

    const PAGE: u64 = 0x1000;

    impl GpuMemoryAccess for MappedMemory {
        fn gpu_to_cpu_address(&self, gpu_addr: u64) -> Option<u64> {
            let page = gpu_addr & !(PAGE - 1);
            self.pages
                .iter()
                .find(|(gpu, _)| *gpu == page)
                .map(|(_, dev)| dev + (gpu_addr & (PAGE - 1)))
        }
        fn read_u64(&self, _gpu_addr: u64) -> Option<u64> {
            None
        }
        fn read_u32(&self, _gpu_addr: u64) -> Option<u32> {
            None
        }
        fn is_within_gpu_address_range(&self, _gpu_addr: u64) -> bool {
            true
        }
        fn max_continuous_range(&self, _gpu_addr: u64, size: u64) -> u64 {
            size
        }
        fn get_memory_layout_size(&self, _gpu_addr: u64) -> u64 {
            0
        }
        fn get_id(&self) -> usize {
            self.id
        }
        /// Splits `[gpu_addr, gpu_addr + size)` into host-contiguous runs of
        /// mapped pages, like `MemoryManager::GetSubmappedRange`.
        fn get_submapped_range(&self, gpu_addr: u64, size: u64) -> Vec<(u64, u64)> {
            let mut ranges: Vec<(u64, u64)> = Vec::new();
            let mut last_dev: Option<u64> = None;
            let mut addr = gpu_addr;
            let end = gpu_addr + size;
            while addr < end {
                let page_end = ((addr | (PAGE - 1)) + 1).min(end);
                let len = page_end - addr;
                match self.gpu_to_cpu_address(addr) {
                    Some(dev) => {
                        let contiguous = last_dev == Some(dev);
                        if contiguous {
                            ranges.last_mut().unwrap().1 += len;
                        } else {
                            ranges.push((addr, len));
                        }
                        last_dev = Some(dev + len);
                    }
                    None => last_dev = None,
                }
                addr = page_end;
            }
            ranges
        }
    }

    #[test]
    fn contiguous_host_range_yields_single_segment() {
        let memory = MappedMemory {
            id: 1,
            pages: vec![(0x1000, 0x8000), (0x2000, 0x9000)],
        };
        let mut cache = VirtualRangeCache::new();
        let segments = cache.query(&memory, 0x1000, 0x2000);
        assert_eq!(segments.len(), 1);
        assert_eq!(
            segments[0],
            VirtualSegment {
                gpu_addr: 0x1000,
                device_addr: 0x8000,
                size: 0x2000
            }
        );
    }

    #[test]
    fn split_host_range_yields_one_segment_per_run() {
        let memory = MappedMemory {
            id: 1,
            pages: vec![(0x1000, 0x8000), (0x2000, 0x20000), (0x3000, 0x21000)],
        };
        let mut cache = VirtualRangeCache::new();
        let segments = cache.query(&memory, 0x1000, 0x3000).clone();
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].device_addr, 0x8000);
        assert_eq!(segments[0].size, 0x1000);
        assert_eq!(segments[1].gpu_addr, 0x2000);
        assert_eq!(segments[1].device_addr, 0x20000);
        assert_eq!(segments[1].size, 0x2000);
        // Cached: a second query for the same key returns the same entry.
        assert_eq!(cache.cached_entries(), 1);
        assert_eq!(cache.query(&memory, 0x1000, 0x3000).len(), 2);
        assert_eq!(cache.cached_entries(), 1);
    }

    #[test]
    fn unmapped_hole_clears_segments() {
        let memory = MappedMemory {
            id: 1,
            pages: vec![(0x1000, 0x8000), (0x3000, 0xA000)],
        };
        let mut cache = VirtualRangeCache::new();
        assert!(cache.query(&memory, 0x1000, 0x3000).is_empty());
    }

    #[test]
    fn deferred_unmap_evicts_overlapping_entries_only() {
        let memory = MappedMemory {
            id: 7,
            pages: vec![(0x1000, 0x8000), (0x2000, 0x20000), (0x5000, 0x30000), (0x6000, 0x40000)],
        };
        let mut cache = VirtualRangeCache::new();
        cache.query(&memory, 0x1000, 0x2000);
        cache.query(&memory, 0x5000, 0x2000);
        assert_eq!(cache.cached_entries(), 2);
        // Other address space: ignored.
        cache.unmap(8, 0x1000, 0x2000);
        // Adjacent unmaps merge into one deferred record.
        cache.unmap(7, 0x2000, 0x1000);
        cache.unmap(7, 0x3000, 0x1000);
        assert_eq!(cache.deferred.lock().unwrap().pending.len(), 2);
        cache.query(&memory, 0x5000, 0x2000);
        assert_eq!(cache.cached_entries(), 1);
        assert!(!cache.has_deferred.load(Ordering::Acquire));
    }

    #[test]
    fn deferred_overflow_flushes_every_entry() {
        let memory = MappedMemory {
            id: 1,
            pages: vec![(0x1000, 0x8000), (0x2000, 0x20000)],
        };
        let mut cache = VirtualRangeCache::new();
        cache.query(&memory, 0x1000, 0x2000);
        for index in 0..=VirtualRangeCache::MAX_DEFERRED as u64 {
            // Non-adjacent so nothing merges.
            cache.unmap(99, 0x1_0000_0000 + index * 0x2000, 0x1000);
        }
        cache.query(&memory, 0x1000, 0x1000);
        // The overflow cleared the map before the new query inserted its entry.
        assert_eq!(cache.cached_entries(), 1);
    }
}
