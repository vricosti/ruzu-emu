// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Lichii `video_core/buffer_cache/multi_range_refresh.h` (Lichii extension, absent from
//! Eden): keeps the segments of a storage binding split across several host buffers pointing to
//! live buffers, and rebinds the gathered copy of such a binding for every use.

use super::buffer_cache_base::{BufferId, MultiRangeSegment};

/// Upstream `InvalidateMultiRangeSegments`: forgets a deleted (or merged) buffer.
pub fn invalidate_multi_range_segments(segments: &mut [MultiRangeSegment], deleted: BufferId) {
    for segment in segments {
        if segment.buffer_id == deleted {
            segment.buffer_id = BufferId::default();
        }
    }
}

/// Upstream `MultiRangeSegmentsLive`.
pub fn multi_range_segments_live(
    segments: &[MultiRangeSegment],
    mut is_live: impl FnMut(BufferId) -> bool,
) -> bool {
    segments
        .iter()
        .all(|segment| segment.buffer_id.is_valid() && is_live(segment.buffer_id))
}

/// Upstream `RefreshMultiRangeSegments`: finds the buffers of segments whose buffer went away.
/// Finding a later segment can merge away an earlier one, so it retries a bounded number of times.
/// `owner` is the cache both callbacks work on (upstream's lambdas capture `this`).
pub fn refresh_multi_range_segments<C>(
    segments: &mut [MultiRangeSegment],
    owner: &mut C,
    is_live: impl Fn(&C, BufferId) -> bool,
    mut resolve: impl FnMut(&mut C, u64, u32) -> BufferId,
) -> bool {
    for _pass in 0..16 {
        for segment in segments.iter_mut() {
            if !segment.buffer_id.is_valid() || !is_live(owner, segment.buffer_id) {
                segment.buffer_id = resolve(owner, segment.device_addr, segment.size);
            }
        }
        if multi_range_segments_live(segments, |id| is_live(owner, id)) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(id: u32, device_addr: u64) -> MultiRangeSegment {
        MultiRangeSegment { buffer_id: BufferId { index: id }, device_addr, size: 0x1000 }
    }

    #[test]
    fn deleted_buffers_are_forgotten_and_resolved_again() {
        let mut segments = [segment(1, 0x1000), segment(2, 0x5000)];
        invalidate_multi_range_segments(&mut segments, BufferId { index: 2 });
        assert!(!segments[1].buffer_id.is_valid());
        assert!(!multi_range_segments_live(&segments, |_| true));
        let refreshed = refresh_multi_range_segments(&mut segments, &mut (), |_, id| id.index != 2,
            |_, address, _| BufferId { index: (address >> 12) as u32 + 10 });
        assert!(refreshed);
        assert_eq!(segments[0].buffer_id.index, 1);
        assert_eq!(segments[1].buffer_id.index, 15);
    }

    #[test]
    fn refresh_gives_up_when_buffers_never_become_live() {
        let mut segments = [segment(1, 0x1000)];
        assert!(!refresh_multi_range_segments(&mut segments, &mut (), |_, _| false,
            |_, _, _| BufferId { index: 3 }));
    }
}
