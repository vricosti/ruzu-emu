// SPDX-FileCopyrightText: Copyright 2024 ruzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `video_core/host1x/syncpoint_manager.h` and `syncpoint_manager.cpp`.
//!
//! Manages syncpoint values and actions for guest and host synchronization.

use std::collections::LinkedList;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Condvar, Mutex};

/// Maximum number of syncpoints supported by the hardware.
const NUM_MAX_SYNCPOINTS: usize = 192;

/// A registered action that fires when a syncpoint reaches the expected value.
pub struct RegisteredAction {
    pub expected_value: u32,
    pub action: Box<dyn FnOnce() + Send>,
}

/// Stable handle into the action list. Wraps an index for deregistration.
/// In upstream this is a `std::list::iterator`; we use a unique ID approach
/// since Rust LinkedList does not expose stable iterators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionHandle(u64);

impl ActionHandle {
    pub fn raw(self) -> u64 {
        self.0
    }

    pub fn from_raw(raw: u64) -> Self {
        Self(raw)
    }
}

/// Internal storage for actions keyed by a monotonic ID.
struct ActionEntry {
    id: u64,
    expected_value: u32,
    action: Option<Box<dyn FnOnce() + Send>>,
}

struct ActionStorage {
    entries: LinkedList<ActionEntry>,
    next_id: u64,
}

impl ActionStorage {
    fn new() -> Self {
        Self {
            entries: LinkedList::new(),
            next_id: 0,
        }
    }

    fn insert(&mut self, expected_value: u32, action: Box<dyn FnOnce() + Send>) -> ActionHandle {
        let id = self.next_id;
        self.next_id += 1;
        let entry = ActionEntry {
            id,
            expected_value,
            action: Some(action),
        };

        // Insert in sorted order by expected_value (ascending), matching upstream.
        let mut cursor = self.entries.iter().enumerate();
        let mut insert_pos = None;
        while let Some((i, e)) = cursor.next() {
            if e.expected_value >= expected_value {
                insert_pos = Some(i);
                break;
            }
        }

        match insert_pos {
            Some(pos) => {
                // Split, push, then rejoin.
                let mut tail = self.entries.split_off(pos);
                self.entries.push_back(entry);
                self.entries.append(&mut tail);
            }
            None => {
                self.entries.push_back(entry);
            }
        }

        ActionHandle(id)
    }

    fn remove(&mut self, handle: &ActionHandle) {
        // Walk the list and remove the matching entry, matching upstream's
        // iterator-validation behavior.
        let mut new_list = LinkedList::new();
        let mut found = false;
        while let Some(entry) = self.entries.pop_front() {
            if !found && entry.id == handle.0 {
                found = true;
                // Drop the entry (don't run the action).
                continue;
            }
            new_list.push_back(entry);
        }
        self.entries = new_list;
    }

    /// Fire all actions whose expected_value <= new_value, in order.
    fn fire_up_to(&mut self, new_value: u32) {
        while let Some(front) = self.entries.front() {
            if front.expected_value > new_value {
                break;
            }
            let mut entry = self.entries.pop_front().unwrap();
            if let Some(action) = entry.action.take() {
                action();
            }
        }
    }
}

/// Manages syncpoint values and registered wait actions.
///
/// Port of `Tegra::Host1x::SyncpointManager`.
pub struct SyncpointManager {
    syncpoints_guest: [AtomicU32; NUM_MAX_SYNCPOINTS],
    syncpoints_host: [AtomicU32; NUM_MAX_SYNCPOINTS],

    /// Guards action_storage and condvar waits.
    guard: Mutex<SyncpointManagerInner>,
    wait_guest_cv: Condvar,
    wait_host_cv: Condvar,
}

struct SyncpointManagerInner {
    guest_action_storage: Vec<ActionStorage>,
    host_action_storage: Vec<ActionStorage>,
}

// AtomicU32 arrays cannot use Default, so implement manually.
impl SyncpointManager {
    pub fn new() -> Self {
        let syncpoints_guest: [AtomicU32; NUM_MAX_SYNCPOINTS] =
            std::array::from_fn(|_| AtomicU32::new(0));
        let syncpoints_host: [AtomicU32; NUM_MAX_SYNCPOINTS] =
            std::array::from_fn(|_| AtomicU32::new(0));

        let mut guest_storage = Vec::with_capacity(NUM_MAX_SYNCPOINTS);
        let mut host_storage = Vec::with_capacity(NUM_MAX_SYNCPOINTS);
        for _ in 0..NUM_MAX_SYNCPOINTS {
            guest_storage.push(ActionStorage::new());
            host_storage.push(ActionStorage::new());
        }

        Self {
            syncpoints_guest,
            syncpoints_host,
            guard: Mutex::new(SyncpointManagerInner {
                guest_action_storage: guest_storage,
                host_action_storage: host_storage,
            }),
            wait_guest_cv: Condvar::new(),
            wait_host_cv: Condvar::new(),
        }
    }

    pub fn get_guest_syncpoint_value(&self, id: u32) -> u32 {
        self.syncpoints_guest[id as usize].load(Ordering::Acquire)
    }

    pub fn get_host_syncpoint_value(&self, id: u32) -> u32 {
        self.syncpoints_host[id as usize].load(Ordering::Acquire)
    }

    pub fn register_guest_action(
        &self,
        syncpoint_id: u32,
        expected_value: u32,
        action: Box<dyn FnOnce() + Send>,
    ) -> Option<ActionHandle> {
        self.register_action(
            &self.syncpoints_guest[syncpoint_id as usize],
            syncpoint_id as usize,
            true,
            expected_value,
            action,
        )
    }

    pub fn register_host_action(
        &self,
        syncpoint_id: u32,
        expected_value: u32,
        action: Box<dyn FnOnce() + Send>,
    ) -> Option<ActionHandle> {
        self.register_action(
            &self.syncpoints_host[syncpoint_id as usize],
            syncpoint_id as usize,
            false,
            expected_value,
            action,
        )
    }

    pub fn deregister_guest_action(&self, syncpoint_id: u32, handle: &ActionHandle) {
        let mut inner = self.guard.lock().unwrap();
        inner.guest_action_storage[syncpoint_id as usize].remove(handle);
    }

    pub fn deregister_host_action(&self, syncpoint_id: u32, handle: &ActionHandle) {
        let mut inner = self.guard.lock().unwrap();
        inner.host_action_storage[syncpoint_id as usize].remove(handle);
    }

    pub fn increment_guest(&self, syncpoint_id: u32) {
        self.increment(
            &self.syncpoints_guest[syncpoint_id as usize],
            syncpoint_id as usize,
            true,
        );
    }

    pub fn increment_host(&self, syncpoint_id: u32) {
        self.increment(
            &self.syncpoints_host[syncpoint_id as usize],
            syncpoint_id as usize,
            false,
        );
    }

    pub fn wait_guest(&self, syncpoint_id: u32, expected_value: u32) {
        self.wait(
            &self.syncpoints_guest[syncpoint_id as usize],
            &self.wait_guest_cv,
            expected_value,
        );
    }

    pub fn wait_host(&self, syncpoint_id: u32, expected_value: u32) {
        self.wait(
            &self.syncpoints_host[syncpoint_id as usize],
            &self.wait_host_cv,
            expected_value,
        );
    }

    pub fn is_ready_guest(&self, syncpoint_id: u32, expected_value: u32) -> bool {
        self.syncpoints_guest[syncpoint_id as usize].load(Ordering::Acquire) >= expected_value
    }

    pub fn is_ready_host(&self, syncpoint_id: u32, expected_value: u32) -> bool {
        self.syncpoints_host[syncpoint_id as usize].load(Ordering::Acquire) >= expected_value
    }

    // --- Private helpers ---

    fn register_action(
        &self,
        syncpoint: &AtomicU32,
        storage_idx: usize,
        is_guest: bool,
        expected_value: u32,
        action: Box<dyn FnOnce() + Send>,
    ) -> Option<ActionHandle> {
        // Fast path: already reached.
        let current = syncpoint.load(Ordering::Acquire);
        if current >= expected_value {
            action();
            return None;
        }

        let mut inner = self.guard.lock().unwrap();

        // Double-check under lock (relaxed is fine here, matching upstream).
        let current = syncpoint.load(Ordering::Relaxed);
        if current >= expected_value {
            action();
            return None;
        }

        let storage = if is_guest {
            &mut inner.guest_action_storage[storage_idx]
        } else {
            &mut inner.host_action_storage[storage_idx]
        };

        Some(storage.insert(expected_value, action))
    }

    fn increment(&self, syncpoint: &AtomicU32, storage_idx: usize, is_guest: bool) {
        let new_value = syncpoint.fetch_add(1, Ordering::AcqRel) + 1;

        let mut inner = self.guard.lock().unwrap();
        let storage = if is_guest {
            &mut inner.guest_action_storage[storage_idx]
        } else {
            &mut inner.host_action_storage[storage_idx]
        };
        storage.fire_up_to(new_value);

        // Notify waiters.
        if is_guest {
            self.wait_guest_cv.notify_all();
        } else {
            self.wait_host_cv.notify_all();
        }
    }

    fn wait(&self, syncpoint: &AtomicU32, cv: &Condvar, expected_value: u32) {
        if syncpoint.load(Ordering::Acquire) >= expected_value {
            return;
        }

        let mut inner = self.guard.lock().unwrap();
        while syncpoint.load(Ordering::Acquire) < expected_value {
            inner = cv.wait(inner).unwrap();
        }
    }
}

impl Default for SyncpointManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_basic_increment_and_wait() {
        let mgr = Arc::new(SyncpointManager::new());
        assert_eq!(mgr.get_host_syncpoint_value(0), 0);

        mgr.increment_host(0);
        assert_eq!(mgr.get_host_syncpoint_value(0), 1);
        assert!(mgr.is_ready_host(0, 1));
        assert!(!mgr.is_ready_host(0, 2));
    }

    #[test]
    fn test_action_fires_immediately_if_ready() {
        let mgr = SyncpointManager::new();
        mgr.increment_guest(5);

        let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fired_clone = fired.clone();
        let handle = mgr.register_guest_action(
            5,
            1,
            Box::new(move || {
                fired_clone.store(true, Ordering::SeqCst);
            }),
        );

        // Should have fired immediately, returning None.
        assert!(handle.is_none());
        assert!(fired.load(Ordering::SeqCst));
    }

    #[test]
    fn test_action_fires_on_increment() {
        let mgr = SyncpointManager::new();
        let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fired_clone = fired.clone();

        let _handle = mgr.register_host_action(
            3,
            1,
            Box::new(move || {
                fired_clone.store(true, Ordering::SeqCst);
            }),
        );

        assert!(!fired.load(Ordering::SeqCst));
        mgr.increment_host(3);
        assert!(fired.load(Ordering::SeqCst));
    }

    #[test]
    fn actions_fire_in_expected_value_order() {
        let mgr = SyncpointManager::new();
        let fired = Arc::new(std::sync::Mutex::new(Vec::new()));

        for (expected, value) in [(3, 3), (1, 1), (2, 2)] {
            let fired = Arc::clone(&fired);
            mgr.register_host_action(
                7,
                expected,
                Box::new(move || {
                    fired.lock().unwrap().push(value);
                }),
            );
        }

        mgr.increment_host(7);
        assert_eq!(&*fired.lock().unwrap(), &[1]);
        mgr.increment_host(7);
        assert_eq!(&*fired.lock().unwrap(), &[1, 2]);
        mgr.increment_host(7);
        assert_eq!(&*fired.lock().unwrap(), &[1, 2, 3]);
    }

    #[test]
    fn deregistered_action_does_not_fire() {
        let mgr = SyncpointManager::new();
        let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fired_clone = Arc::clone(&fired);

        let handle = mgr
            .register_guest_action(
                8,
                1,
                Box::new(move || {
                    fired_clone.store(true, Ordering::SeqCst);
                }),
            )
            .expect("action should be pending");

        mgr.deregister_guest_action(8, &handle);
        mgr.increment_guest(8);
        assert!(!fired.load(Ordering::SeqCst));
    }
}
