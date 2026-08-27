// SPDX-FileCopyrightText: Ryujinx Team and Contributors
// SPDX-License-Identifier: MIT

//! Port of `video_core/host1x/sync_manager.h` and `sync_manager.cpp`.

use crate::host1x::host1x::Host1x;
use std::sync::Mutex;

/// One queued host1x syncpoint increment.
///
/// Port of `Tegra::Host1x::SyncptIncr`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncptIncr {
    pub id: u32,
    pub class_id: u32,
    pub syncpt_id: u32,
    pub complete: bool,
}

impl SyncptIncr {
    pub fn new(id: u32, class_id: u32, syncpt_id: u32, complete: bool) -> Self {
        Self {
            id,
            class_id,
            syncpt_id,
            complete,
        }
    }
}

/// Preserves submission order while delaying host1x syncpoint increments.
///
/// Port of `Tegra::Host1x::SyncptIncrManager`.
pub struct SyncptIncrManager<'a> {
    increments: Vec<SyncptIncr>,
    // Present in Eden's owner even though its current methods do not acquire it.
    #[allow(dead_code)]
    increment_lock: Mutex<()>,
    current_id: u32,
    host1x: &'a Host1x,
}

impl<'a> SyncptIncrManager<'a> {
    pub fn new(host1x: &'a Host1x) -> Self {
        Self {
            increments: Vec::new(),
            increment_lock: Mutex::new(()),
            current_id: 0,
            host1x,
        }
    }

    /// Add a completed syncpoint increment and drain the completed prefix.
    pub fn increment(&mut self, id: u32) {
        self.increments.push(SyncptIncr::new(0, 0, id, true));
        self.increment_all_done();
    }

    /// Queue an increment and return the handle used to complete it later.
    pub fn increment_when_done(&mut self, class_id: u32, id: u32) -> u32 {
        let handle = self.current_id;
        self.current_id = self.current_id.wrapping_add(1);
        self.increments
            .push(SyncptIncr::new(handle, class_id, id, false));
        handle
    }

    /// Mark the first queued increment with `handle` complete, then drain.
    pub fn signal_done(&mut self, handle: u32) {
        if let Some(increment) = self
            .increments
            .iter_mut()
            .find(|increment| increment.id == handle)
        {
            increment.complete = true;
        }
        self.increment_all_done();
    }

    /// Increment and erase the sequential completed prefix.
    pub fn increment_all_done(&mut self) {
        let done_count = self
            .increments
            .iter()
            .take_while(|increment| increment.complete)
            .count();
        for increment in &self.increments[..done_count] {
            self.host1x
                .syncpoint_manager()
                .increment_guest(increment.syncpt_id);
            self.host1x
                .syncpoint_manager()
                .increment_host(increment.syncpt_id);
        }
        self.increments.drain(..done_count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syncpoint_increment_layout_matches_upstream() {
        assert_eq!(std::mem::size_of::<SyncptIncr>(), 16);
        assert_eq!(std::mem::align_of::<SyncptIncr>(), 4);
        assert_eq!(std::mem::offset_of!(SyncptIncr, id), 0);
        assert_eq!(std::mem::offset_of!(SyncptIncr, class_id), 4);
        assert_eq!(std::mem::offset_of!(SyncptIncr, syncpt_id), 8);
        assert_eq!(std::mem::offset_of!(SyncptIncr, complete), 12);
    }

    #[test]
    fn completed_prefix_is_incremented_in_submission_order() {
        let host1x = Host1x::new();
        let syncpoints = host1x.syncpoint_manager();
        let mut manager = SyncptIncrManager::new(&host1x);

        let handle = manager.increment_when_done(0x5d, 3);
        manager.increment(4);
        assert_eq!(syncpoints.get_guest_syncpoint_value(3), 0);
        assert_eq!(syncpoints.get_host_syncpoint_value(3), 0);
        assert_eq!(syncpoints.get_guest_syncpoint_value(4), 0);
        assert_eq!(syncpoints.get_host_syncpoint_value(4), 0);

        manager.signal_done(handle);
        assert_eq!(syncpoints.get_guest_syncpoint_value(3), 1);
        assert_eq!(syncpoints.get_host_syncpoint_value(3), 1);
        assert_eq!(syncpoints.get_guest_syncpoint_value(4), 1);
        assert_eq!(syncpoints.get_host_syncpoint_value(4), 1);
        assert!(manager.increments.is_empty());
    }

    #[test]
    fn handles_preserve_upstream_u32_wrapping() {
        let host1x = Host1x::new();
        let mut manager = SyncptIncrManager::new(&host1x);
        manager.current_id = u32::MAX;

        assert_eq!(manager.increment_when_done(0, 1), u32::MAX);
        assert_eq!(manager.increment_when_done(0, 2), 0);
    }
}
