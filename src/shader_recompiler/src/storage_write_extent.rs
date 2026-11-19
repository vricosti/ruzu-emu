// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native storage write bounds, from Lichii `shader_recompiler/storage_write_extent.h`.
//! A bound is relative to the raw guest pointer, not the aligned host binding.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StorageWriteExtent {
    /// Unknown bounds require tracking the whole binding as written.
    pub bounded: bool,
    pub guarded: bool,
    pub const_end: u32,
    pub local_stride: [u32; 3],
    pub group_stride: [u32; 3],
    pub guard_const: u32,
    pub guard_local_stride: [u32; 3],
    pub guard_group_stride: [u32; 3],
}

/// Maximum exclusive byte offset reached by a dispatch. `None` grid denotes an
/// indirect dispatch, whose group dimensions are unavailable to the CPU.
pub fn resolve_storage_write_extent(
    extent: &StorageWriteExtent,
    block: [u32; 3],
    grid: Option<[u32; 3]>,
) -> Option<u64> {
    if !extent.bounded {
        return None;
    }
    let evaluate = |constant: u32, local: [u32; 3], group: [u32; 3]| -> Option<u64> {
        const LIMIT: u64 = 1 << 62;
        let mut total = u64::from(constant);
        for i in 0..3 {
            if block[i] == 0 {
                return None;
            }
            let local_max = u64::from(block[i]) - 1;
            if local[i] != 0 && local_max != 0 {
                if local_max > LIMIT / u64::from(local[i]) {
                    return None;
                }
                total += u64::from(local[i]) * local_max;
            }
            if group[i] == 0 {
                continue;
            }
            let groups = grid?;
            if groups[i] == 0 {
                return None;
            }
            let group_max = u64::from(groups[i]) - 1;
            if group_max != 0 {
                if group_max > LIMIT / u64::from(group[i]) {
                    return None;
                }
                total += u64::from(group[i]) * group_max;
            }
            if total > LIMIT {
                return None;
            }
        }
        Some(total)
    };
    if extent.guarded {
        let guard = evaluate(extent.guard_const, extent.guard_local_stride, extent.guard_group_stride)?;
        if guard >= 0x10000 {
            return None;
        }
    }
    evaluate(extent.const_end, extent.local_stride, extent.group_stride)
}

/// Widen both the store bound and the optional 16-bit multiplication guard.
pub fn merge_storage_write_extent(base: &mut StorageWriteExtent, other: &StorageWriteExtent) {
    if !base.bounded || !other.bounded {
        *base = StorageWriteExtent::default();
        return;
    }
    let max3 = |lhs: &mut [u32; 3], rhs: [u32; 3]| {
        for i in 0..3 {
            lhs[i] = lhs[i].max(rhs[i]);
        }
    };
    base.const_end = base.const_end.max(other.const_end);
    max3(&mut base.local_stride, other.local_stride);
    max3(&mut base.group_stride, other.group_stride);
    if other.guarded {
        if base.guarded {
            base.guard_const = base.guard_const.max(other.guard_const);
            max3(&mut base.guard_local_stride, other.guard_local_stride);
            max3(&mut base.guard_group_stride, other.guard_group_stride);
        } else {
            base.guarded = true;
            base.guard_const = other.guard_const;
            base.guard_local_stride = other.guard_local_stride;
            base.guard_group_stride = other.guard_group_stride;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_extent_includes_all_local_and_group_axes() {
        let extent = StorageWriteExtent {
            bounded: true, const_end: 4,
            local_stride: [4, 32, 96], group_stride: [192, 384, 1152],
            ..Default::default()
        };
        assert_eq!(resolve_storage_write_extent(&extent, [8, 3, 2], Some([2, 3, 4])), Some(4608));
        assert_eq!(resolve_storage_write_extent(&extent, [8, 3, 2], None), None);
        assert_eq!(resolve_storage_write_extent(&extent, [0, 3, 2], Some([2, 3, 4])), None);
        assert_eq!(resolve_storage_write_extent(&extent, [8, 3, 2], Some([2, 0, 4])), None);
    }

    #[test]
    fn indirect_dispatch_accepts_group_independent_bounds_only() {
        let mut extent = StorageWriteExtent { bounded: true, const_end: 4,
            local_stride: [4, 0, 0], ..Default::default() };
        assert_eq!(resolve_storage_write_extent(&extent, [64, 1, 1], None), Some(256));
        extent.group_stride[0] = 1;
        assert_eq!(resolve_storage_write_extent(&extent, [64, 1, 1], None), None);
        assert_eq!(resolve_storage_write_extent(&StorageWriteExtent::default(), [1; 3], Some([1; 3])), None);
    }

    #[test]
    fn split_multiply_guard_rejects_sixteen_bit_wrap() {
        let mut extent = StorageWriteExtent { bounded: true, guarded: true,
            const_end: 16, guard_const: 65534, guard_local_stride: [1, 0, 0],
            ..Default::default() };
        assert_eq!(resolve_storage_write_extent(&extent, [2, 1, 1], None), Some(16));
        assert_eq!(resolve_storage_write_extent(&extent, [3, 1, 1], None), None);
        extent.guard_group_stride[1] = 1;
        assert_eq!(resolve_storage_write_extent(&extent, [1; 3], None), None);
    }

    #[test]
    fn oversized_products_remain_unknown() {
        let mut extent = StorageWriteExtent { bounded: true,
            local_stride: [u32::MAX, 0, 0], ..Default::default() };
        assert_eq!(resolve_storage_write_extent(&extent, [u32::MAX, 1, 1], None), None);
        extent.local_stride = [0; 3];
        extent.group_stride = [u32::MAX, 0, 0];
        assert_eq!(resolve_storage_write_extent(&extent, [1; 3], Some([u32::MAX, 1, 1])), None);
    }

    #[test]
    fn merge_widens_bounds_and_guards_and_propagates_unknown() {
        let mut a = StorageWriteExtent { bounded: true, const_end: 4,
            local_stride: [8, 0, 3], ..Default::default() };
        let b = StorageWriteExtent { bounded: true, guarded: true, const_end: 8,
            local_stride: [4, 2, 0], guard_const: 9, guard_group_stride: [3, 1, 0],
            ..Default::default() };
        merge_storage_write_extent(&mut a, &b);
        assert_eq!(a.const_end, 8);
        assert_eq!(a.local_stride, [8, 2, 3]);
        assert!(a.guarded);
        assert_eq!(a.guard_group_stride, [3, 1, 0]);
        let c = StorageWriteExtent { guard_const: 7, guard_group_stride: [1, 5, 2], ..b };
        merge_storage_write_extent(&mut a, &c);
        assert_eq!(a.guard_const, 9);
        assert_eq!(a.guard_group_stride, [3, 5, 2]);
        merge_storage_write_extent(&mut a, &StorageWriteExtent::default());
        assert_eq!(a, StorageWriteExtent::default());
        merge_storage_write_extent(&mut a, &b);
        assert_eq!(a, StorageWriteExtent::default());
    }
}
