// SPDX-FileCopyrightText: 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/core/hle/service/nvdrv/core/heap_mapper.h
//! Port of zuyu/src/core/hle/service/nvdrv/core/heap_mapper.cpp

use std::sync::Mutex;

use crate::core::SystemRef;

/// HeapMapper manages mapping regions of a process heap into the device address space.
pub struct HeapMapper {
    m_vaddress: u64,
    m_daddress: u64,
    m_size: usize,
    m_asid: u32,
    system: SystemRef,
    m_internal: Mutex<HeapMapperInternal>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MappedRange {
    start: u64,
    end: u64,
    count: u32,
}

#[derive(Default)]
struct HeapMapperInternal {
    mapped_ranges: Vec<MappedRange>,
}

impl HeapMapper {
    pub fn new(
        start_vaddress: u64,
        start_daddress: u64,
        size: usize,
        asid: u32,
        system: SystemRef,
    ) -> Self {
        Self {
            m_vaddress: start_vaddress,
            m_daddress: start_daddress,
            m_size: size,
            m_asid: asid,
            system,
            m_internal: Mutex::new(HeapMapperInternal::default()),
        }
    }

    pub fn is_in_bounds(&self, start: u64, size: usize) -> bool {
        let end = start.wrapping_add(size as u64);
        start >= self.m_vaddress && end <= self.m_vaddress.wrapping_add(self.m_size as u64)
    }

    pub fn map(&self, start: u64, size: usize) -> u64 {
        let mut internal = self.m_internal.lock().unwrap();
        let d_address = self
            .m_daddress
            .wrapping_add(start.wrapping_sub(self.m_vaddress));

        if size != 0 {
            let end = start.wrapping_add(size as u64);
            let unmapped = internal.unmapped_subranges(start, end);
            for (sub_start, sub_end) in unmapped {
                self.map_subrange(sub_start, (sub_end - sub_start) as usize);
            }
            internal.add_range(start, end);
        }

        d_address
    }

    pub fn unmap(&self, start: u64, size: usize) {
        let mut internal = self.m_internal.lock().unwrap();
        for (sub_start, sub_end) in internal.subtract_range(start, start.wrapping_add(size as u64))
        {
            self.unmap_subrange(sub_start, (sub_end - sub_start) as usize);
        }
    }

    pub fn get_region_start(&self) -> u64 {
        self.m_daddress
    }

    pub fn get_region_size(&self) -> usize {
        self.m_size
    }

    /// Mechanical destructor body, also called at upstream's mapper.reset()
    /// point when a deferred NX2 callback still retains the Rust allocation.
    /// Draining ownership prevents that callback/Drop from unmapping reused
    /// SMMU backing after the session's address range has been freed.
    pub(crate) fn finalize(&self) {
        let mut internal = self.m_internal.lock().unwrap();
        for range in internal.mapped_ranges.drain(..) {
            self.unmap_subrange(range.start, (range.end - range.start) as usize);
        }
    }

    fn map_subrange(&self, start: u64, size: usize) {
        let sys = self.system.get();
        let Some(host1x) = sys.host1x_core() else {
            return;
        };
        host1x.smmu_map(
            self.m_daddress
                .wrapping_add(start.wrapping_sub(self.m_vaddress)),
            start,
            size,
            self.m_asid,
            false,
        );
    }

    fn unmap_subrange(&self, start: u64, size: usize) {
        let d_address = self
            .m_daddress
            .wrapping_add(start.wrapping_sub(self.m_vaddress));
        if std::env::var_os("RUZU_TRACE_NVMAP_PIN").is_some() {
            eprintln!(
                "[HEAP_UNMAP] vaddr=0x{:X} size=0x{:X} -> smmu_unmap d_address=0x{:X}",
                start, size, d_address
            );
        }
        if let Some(host1x) = self.system.get().host1x_core() {
            host1x.smmu_unmap(d_address, size);
        }
    }
}

impl Drop for HeapMapper {
    fn drop(&mut self) {
        self.finalize();
    }
}

impl HeapMapperInternal {
    fn unmapped_subranges(&self, start: u64, end: u64) -> Vec<(u64, u64)> {
        if start >= end {
            return Vec::new();
        }
        let mut result = Vec::new();
        let mut cursor = start;
        for range in &self.mapped_ranges {
            if range.end <= cursor {
                continue;
            }
            if range.start >= end {
                break;
            }
            if cursor < range.start {
                result.push((cursor, range.start.min(end)));
            }
            cursor = cursor.max(range.end);
            if cursor >= end {
                break;
            }
        }
        if cursor < end {
            result.push((cursor, end));
        }
        result
    }

    fn add_range(&mut self, start: u64, end: u64) {
        self.with_boundaries(start, end);
        let mut gaps = Vec::new();
        let mut cursor = start;
        for range in &mut self.mapped_ranges {
            if range.end <= start || range.start >= end {
                continue;
            }
            if cursor < range.start {
                gaps.push(MappedRange {
                    start: cursor,
                    end: range.start,
                    count: 1,
                });
            }
            range.count += 1;
            cursor = range.end;
        }
        if cursor < end {
            gaps.push(MappedRange {
                start: cursor,
                end,
                count: 1,
            });
        }
        self.mapped_ranges.extend(gaps);
        self.merge_adjacent();
    }

    fn subtract_range(&mut self, start: u64, end: u64) -> Vec<(u64, u64)> {
        self.with_boundaries(start, end);
        let mut removed = Vec::new();
        for range in &mut self.mapped_ranges {
            if range.end <= start || range.start >= end {
                continue;
            }
            if range.count > 0 {
                range.count -= 1;
                if range.count == 0 {
                    removed.push((range.start, range.end));
                }
            }
        }
        self.mapped_ranges.retain(|range| range.count != 0);
        self.merge_adjacent();
        removed
    }

    fn with_boundaries(&mut self, start: u64, end: u64) {
        self.split_at(start);
        self.split_at(end);
    }

    fn split_at(&mut self, point: u64) {
        let Some(index) = self
            .mapped_ranges
            .iter()
            .position(|range| range.start < point && point < range.end)
        else {
            return;
        };
        let range = self.mapped_ranges[index];
        self.mapped_ranges[index].end = point;
        self.mapped_ranges.insert(
            index + 1,
            MappedRange {
                start: point,
                end: range.end,
                count: range.count,
            },
        );
    }

    fn merge_adjacent(&mut self) {
        self.mapped_ranges.sort_by_key(|range| range.start);
        let mut merged: Vec<MappedRange> = Vec::with_capacity(self.mapped_ranges.len());
        for range in self.mapped_ranges.drain(..) {
            if range.start == range.end {
                continue;
            }
            if let Some(last) = merged.last_mut() {
                if last.end == range.start && last.count == range.count {
                    last.end = range.end;
                    continue;
                }
            }
            merged.push(range);
        }
        self.mapped_ranges = merged;
    }
}

#[cfg(test)]
mod tests {
    use super::{HeapMapper, HeapMapperInternal};
    use crate::core::SystemRef;

    #[test]
    fn explicit_retirement_drains_ranges_before_last_capture_is_dropped() {
        let system = crate::core::System::new_boxed_for_test();
        let mapper = std::sync::Arc::new(HeapMapper::new(
            0x1000, 0x8000, 0x4000, 1, SystemRef::from_ref(&system),
        ));
        mapper.map(0x1000, 0x2000);
        mapper.map(0x2000, 0x2000);
        let pending_release = std::sync::Arc::clone(&mapper);
        assert_eq!(mapper.m_internal.lock().unwrap().mapped_ranges.len(), 3);
        mapper.finalize();
        assert!(pending_release.m_internal.lock().unwrap().mapped_ranges.is_empty());
        drop(mapper);
        pending_release.unmap(0x1000, 0x2000);
        assert!(pending_release.m_internal.lock().unwrap().mapped_ranges.is_empty());
    }

    #[test]
    fn is_in_bounds_uses_upstream_wrapping_address_arithmetic() {
        let mapper = HeapMapper::new(0, 0, 0x1000, 0, SystemRef::null());

        // Upstream uses unsigned VAddr arithmetic:
        // `VAddr end = start + size; end <= (m_vaddress + m_size)`.
        // Keep debug Rust builds from introducing an overflow panic here.
        assert!(mapper.is_in_bounds(u64::MAX - 0x10, 0x20));
    }

    #[test]
    fn overlap_ranges_only_remove_when_last_mapping_is_unmapped() {
        let mut internal = HeapMapperInternal::default();

        assert_eq!(
            internal.unmapped_subranges(0x1000, 0x3000),
            vec![(0x1000, 0x3000)]
        );
        internal.add_range(0x1000, 0x3000);

        assert_eq!(
            internal.unmapped_subranges(0x2000, 0x4000),
            vec![(0x3000, 0x4000)]
        );
        internal.add_range(0x2000, 0x4000);

        assert_eq!(
            internal.subtract_range(0x1000, 0x3000),
            vec![(0x1000, 0x2000)]
        );
        assert_eq!(
            internal.subtract_range(0x2000, 0x4000),
            vec![(0x2000, 0x4000)]
        );
    }

    #[test]
    fn partial_overlap_unmap_removes_only_zero_count_subranges() {
        let mut internal = HeapMapperInternal::default();

        internal.add_range(0x1000, 0x3000);
        internal.add_range(0x2000, 0x4000);

        assert_eq!(
            internal.subtract_range(0x2800, 0x3800),
            vec![(0x3000, 0x3800)]
        );
        assert_eq!(
            internal.mapped_ranges,
            vec![
                super::MappedRange {
                    start: 0x1000,
                    end: 0x2000,
                    count: 1,
                },
                super::MappedRange {
                    start: 0x2000,
                    end: 0x2800,
                    count: 2,
                },
                super::MappedRange {
                    start: 0x2800,
                    end: 0x3000,
                    count: 1,
                },
                super::MappedRange {
                    start: 0x3800,
                    end: 0x4000,
                    count: 1,
                },
            ]
        );
    }

    #[test]
    fn mapped_ranges_merge_only_when_counts_match() {
        let mut internal = HeapMapperInternal::default();

        internal.add_range(0x1000, 0x2000);
        internal.add_range(0x2000, 0x3000);
        assert_eq!(
            internal.mapped_ranges,
            vec![super::MappedRange {
                start: 0x1000,
                end: 0x3000,
                count: 1,
            }]
        );

        internal.add_range(0x1800, 0x2800);
        assert_eq!(
            internal.mapped_ranges,
            vec![
                super::MappedRange {
                    start: 0x1000,
                    end: 0x1800,
                    count: 1,
                },
                super::MappedRange {
                    start: 0x1800,
                    end: 0x2800,
                    count: 2,
                },
                super::MappedRange {
                    start: 0x2800,
                    end: 0x3000,
                    count: 1,
                },
            ]
        );
    }
}
