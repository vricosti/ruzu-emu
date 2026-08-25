// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/video_core/query_cache/query_stream.h
//!
//! Defines `StreamerInterface`, the shared non-virtual streamer state and
//! interface, plus `SimpleStreamer<Q>`, the slot-backed generic streamer owner.

use std::collections::VecDeque;
use std::sync::Mutex;

use super::query_base::{QueryBase, VAddr};

/// Shared non-virtual state for C++ `StreamerInterface`.
#[derive(Debug)]
pub struct StreamerInterfaceBase {
    pub id: usize,
    pub dependence_mask: u64,
    pub dependent_mask: u64,
    pub amend_value: u64,
    pub accumulation_value: u64,
}

impl StreamerInterfaceBase {
    pub fn new(id: usize) -> Self {
        Self {
            id,
            dependence_mask: 0,
            dependent_mask: 0,
            amend_value: 0,
            accumulation_value: 0,
        }
    }
}

/// Base interface for all query streamers.
///
/// Maps to C++ `StreamerInterface`. Rust uses a trait for the virtual surface
/// and `StreamerInterfaceBase` for the shared state that lives in the base
/// class in C++.
pub trait StreamerInterface {
    fn get_query(&self, id: usize) -> Option<&QueryBase>;
    fn get_query_mut(&mut self, id: usize) -> Option<&mut QueryBase>;
    fn write_counter(
        &mut self,
        address: VAddr,
        has_timestamp: bool,
        value: u32,
        subreport: Option<u32>,
    ) -> usize;
    fn free(&mut self, query_id: usize);
    fn base(&self) -> &StreamerInterfaceBase;
    fn base_mut(&mut self) -> &mut StreamerInterfaceBase;

    fn start_counter(&mut self) {}
    fn pause_counter(&mut self) {}
    fn reset_counter(&mut self) {}
    fn close_counter(&mut self) {}
    fn has_pending_sync(&self) -> bool {
        false
    }
    fn presync_writes(&mut self) {}
    fn sync_writes(&mut self) {}
    fn has_unsynced_queries(&self) -> bool {
        false
    }
    fn push_unsynced_queries(&mut self) {}
    fn pop_unsynced_queries(&mut self) {}

    fn get_id(&self) -> usize {
        self.base().id
    }

    fn get_dependence_mask(&self) -> u64 {
        self.base().dependence_mask
    }

    fn get_dependent_mask(&self) -> u64 {
        self.base().dependent_mask
    }

    fn get_amend_value(&self) -> u64 {
        self.base().amend_value
    }

    fn set_accumulation_value(&mut self, new_value: u64) {
        self.base_mut().accumulation_value = new_value;
    }

    fn make_dependent(&mut self, depend_on: &mut dyn StreamerInterface) {
        let depend_on_id = depend_on.get_id();
        self.base_mut().dependence_mask |= 1u64 << depend_on_id;
        depend_on.base_mut().dependent_mask |= 1u64 << self.get_id();
    }
}

/// Generic slot-backed streamer owner.
///
/// Maps to C++ `SimpleStreamer<QueryType>`.
pub struct SimpleStreamer<Q> {
    pub base: StreamerInterfaceBase,
    pub guard: Mutex<()>,
    pub slot_queries: VecDeque<Q>,
    pub old_queries: VecDeque<usize>,
}

impl<Q> SimpleStreamer<Q> {
    pub fn new(id: usize) -> Self {
        Self {
            base: StreamerInterfaceBase::new(id),
            guard: Mutex::new(()),
            slot_queries: VecDeque::new(),
            old_queries: VecDeque::new(),
        }
    }

    pub fn build_query(&mut self, query: Q) -> usize {
        let _lock = self.guard.lock().unwrap();
        if let Some(new_id) = self.old_queries.pop_front() {
            self.slot_queries[new_id] = query;
            return new_id;
        }
        let new_id = self.slot_queries.len();
        self.slot_queries.push_back(query);
        new_id
    }

    pub fn release_query(&mut self, query_id: usize) {
        if query_id < self.slot_queries.len() {
            self.old_queries.push_back(query_id);
            return;
        }
        unreachable!(
            "SimpleStreamer::release_query: query_id {} out of bounds (len {})",
            query_id,
            self.slot_queries.len()
        );
    }

    pub fn free(&mut self, query_id: usize) {
        // Upstream locks here because callers may only hold shared ownership.
        // Rust already requires `&mut self`, so preserve the owner method but
        // avoid a second borrow through the held mutex guard.
        self.release_query(query_id);
    }

    pub fn get_query(&self, query_id: usize) -> Option<&Q> {
        self.slot_queries.get(query_id)
    }

    pub fn get_query_mut(&mut self, query_id: usize) -> Option<&mut Q> {
        self.slot_queries.get_mut(query_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyStreamer {
        simple: SimpleStreamer<DummyQuery>,
    }

    #[derive(Clone)]
    struct DummyQuery {
        base: QueryBase,
    }

    impl DummyStreamer {
        fn new(id: usize) -> Self {
            Self {
                simple: SimpleStreamer::new(id),
            }
        }
    }

    impl StreamerInterface for DummyStreamer {
        fn get_query(&self, id: usize) -> Option<&QueryBase> {
            self.simple.get_query(id).map(|query| &query.base)
        }

        fn get_query_mut(&mut self, id: usize) -> Option<&mut QueryBase> {
            self.simple.get_query_mut(id).map(|query| &mut query.base)
        }

        fn write_counter(
            &mut self,
            address: VAddr,
            has_timestamp: bool,
            value: u32,
            _subreport: Option<u32>,
        ) -> usize {
            let mut flags = super::super::query_base::QueryFlagBits::empty();
            if has_timestamp {
                flags |= super::super::query_base::QueryFlagBits::HAS_TIMESTAMP;
            }
            self.simple.build_query(DummyQuery {
                base: QueryBase::with_params(address, flags, value as u64),
            })
        }

        fn free(&mut self, query_id: usize) {
            self.simple.free(query_id);
        }

        fn base(&self) -> &StreamerInterfaceBase {
            &self.simple.base
        }

        fn base_mut(&mut self) -> &mut StreamerInterfaceBase {
            &mut self.simple.base
        }
    }

    #[test]
    fn make_dependent_reports_dependence_and_dependent_masks_separately() {
        let mut a = DummyStreamer::new(3);
        let mut b = DummyStreamer::new(5);
        a.make_dependent(&mut b);

        assert_eq!(a.get_dependence_mask(), 1u64 << 5);
        assert_eq!(a.get_dependent_mask(), 0);
        assert_eq!(b.get_dependence_mask(), 0);
        assert_eq!(b.get_dependent_mask(), 1u64 << 3);
    }

    #[test]
    fn simple_streamer_reuses_released_slot_ids() {
        let mut streamer = DummyStreamer::new(0);
        let first = streamer.write_counter(0x1000, false, 1, None);
        let second = streamer.write_counter(0x2000, false, 2, None);
        streamer.free(first);
        let reused = streamer.write_counter(0x3000, true, 3, None);

        assert_eq!(first, 0);
        assert_eq!(second, 1);
        assert_eq!(reused, first);
        assert_eq!(streamer.get_query(reused).unwrap().guest_address, 0x3000);
    }
}
