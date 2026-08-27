// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/video_core/control/channel_state_cache.h,
//!            zuyu/src/video_core/control/channel_state_cache.cpp, and
//!            zuyu/src/video_core/control/channel_state_cache.inc
//!
//! Provides per-channel cache state and a generic `ChannelSetupCaches<P>`
//! container that tracks channel ↔ address-space mappings.
//!
//! The C++ code uses a class template (`ChannelSetupCaches<P>`) with a
//! separate `.inc` file for the template method bodies.  In Rust this is
//! expressed as a generic struct with an `impl<P>` block.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use parking_lot::Mutex;

use super::channel_state::ChannelState;
use crate::memory_manager::MemoryManager;

// ---------------------------------------------------------------------------
// ChannelInfo — non-generic, corresponds to VideoCommon::ChannelInfo
// ---------------------------------------------------------------------------

/// Channel-owned engine and memory references captured at channel creation.
///
/// Corresponds to `VideoCommon::ChannelInfo` (channel_state_cache.h).
/// Engine addresses remain live for the lifetime of the owning `ChannelState`;
/// GPU memory uses shared ownership.
pub struct ChannelInfo {
    /// Channel-bound 3D engine reference.
    ///
    /// Upstream: `Tegra::Engines::Maxwell3D& maxwell3d`.
    pub maxwell3d: usize,
    /// Channel-bound compute engine reference.
    ///
    /// Upstream: `Tegra::Engines::KeplerCompute& kepler_compute`.
    pub kepler_compute: usize,
    /// Index into the owning `ChannelState`'s GPU memory manager.
    /// Upstream: `Tegra::MemoryManager& gpu_memory`.
    pub gpu_memory_index: usize,
    /// Channel-bound GPU memory manager reference.
    ///
    /// Upstream: `Tegra::MemoryManager& gpu_memory`.
    pub gpu_memory: Option<Arc<Mutex<MemoryManager>>>,
    /// Program ID copied from the channel state.
    pub program_id: u64,
}

impl ChannelInfo {
    /// Construct from a `ChannelState`.
    ///
    /// Corresponds to `ChannelInfo::ChannelInfo(Tegra::Control::ChannelState&)`.
    pub fn from_channel_state(channel_state: &ChannelState) -> Self {
        // Upstream dereferences the unique_ptrs; we capture indices/IDs for
        // later resolution.  When real engine types exist these will hold
        // proper references or Arc handles.
        let gpu_memory = channel_state.memory_manager.as_ref().map(Arc::clone);
        let gpu_memory_index = gpu_memory
            .as_ref()
            .map(|memory_manager| memory_manager.lock().get_id())
            .unwrap_or(0);
        Self {
            maxwell3d: channel_state
                .maxwell_3d
                .as_ref()
                .map(|engine| (&**engine as *const _) as usize)
                .unwrap_or(0),
            kepler_compute: channel_state
                .kepler_compute
                .as_ref()
                .map(|engine| (&**engine as *const _) as usize)
                .unwrap_or(0),
            gpu_memory_index,
            gpu_memory,
            program_id: channel_state.program_id,
        }
    }
}

// ---------------------------------------------------------------------------
// AddressSpaceRef — inner bookkeeping struct
// ---------------------------------------------------------------------------

/// Tracks reference-counted address-space registrations.
///
/// Corresponds to `ChannelSetupCaches<P>::AddressSpaceRef`.
struct AddressSpaceRef {
    ref_count: usize,
    storage_id: usize,
    gpu_memory: Arc<Mutex<MemoryManager>>,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Sentinel value for an unbound channel.
///
/// Corresponds to `ChannelSetupCaches<P>::UNSET_CHANNEL`.
const UNSET_CHANNEL: usize = usize::MAX;

// ---------------------------------------------------------------------------
// ChannelSetupCaches<P>
// ---------------------------------------------------------------------------

/// Generic per-channel cache container.
///
/// Corresponds to `VideoCommon::ChannelSetupCaches<P>` (header + `.inc`).
///
/// The type parameter `P` is the per-channel cache payload (e.g.
/// `ChannelInfo`).  Upstream instantiates this as
/// `ChannelSetupCaches<ChannelInfo>`.
pub struct ChannelSetupCaches<P> {
    // -- "current" state (updated by bind_to_channel) ---------------------
    /// Stable address of the currently bound per-channel state.
    ///
    /// Upstream stores `P* channel_state` pointing into a `std::deque<P>`.
    /// Rust's `VecDeque` does not preserve element addresses when it grows, so
    /// `channel_storage` owns boxed elements and this field stores the pointee
    /// address rather than repeatedly resolving `current_channel_id`.
    channel_state: Option<usize>,
    current_channel_id: usize,
    current_address_space: usize,

    /// Cached Maxwell3D owner for the currently bound channel.
    pub maxwell3d: Option<usize>,
    /// Cached KeplerCompute owner for the currently bound channel.
    pub kepler_compute: Option<usize>,
    /// Cached GPU memory owner for the currently bound channel.
    pub gpu_memory: Option<Arc<Mutex<MemoryManager>>>,
    /// Program ID of the currently bound channel.
    pub program_id: u64,

    // -- storage ----------------------------------------------------------
    channel_storage: VecDeque<Box<P>>,
    free_channel_ids: VecDeque<usize>,
    channel_map: HashMap<i32, usize>,
    active_channel_ids: Vec<usize>,
    address_spaces: HashMap<usize, AddressSpaceRef>,
}

impl<P> ChannelSetupCaches<P> {
    /// Create an empty cache set.
    pub fn new() -> Self {
        Self {
            channel_state: None,
            current_channel_id: UNSET_CHANNEL,
            current_address_space: 0,
            maxwell3d: None,
            kepler_compute: None,
            gpu_memory: None,
            program_id: 0,
            channel_storage: VecDeque::new(),
            free_channel_ids: VecDeque::new(),
            channel_map: HashMap::new(),
            active_channel_ids: Vec::new(),
            address_spaces: HashMap::new(),
        }
    }

    /// Create channel state.
    ///
    /// Corresponds to `ChannelSetupCaches<P>::CreateChannel` (channel_state_cache.inc).
    pub fn create_channel(&mut self, channel: &ChannelState)
    where
        P: FromChannelState,
    {
        self.create_channel_with_on_gpu_as_register(channel, |_| {});
    }

    /// Create channel state and run the derived-cache address-space hook when
    /// a new GPU address space is registered.
    ///
    /// Corresponds to `ChannelSetupCaches<P>::CreateChannel` calling virtual
    /// `OnGPUASRegister(map_id)` before returning.
    pub fn create_channel_with_on_gpu_as_register(
        &mut self,
        channel: &ChannelState,
        mut on_gpu_as_register: impl FnMut(usize),
    ) where
        P: FromChannelState,
    {
        assert!(
            !self.channel_map.contains_key(&channel.bind_id) && channel.bind_id >= 0,
            "duplicate or negative bind_id in create_channel"
        );

        let new_id = if let Some(id) = self.free_channel_ids.pop_front() {
            self.channel_storage[id] = Box::new(P::from_channel_state(channel));
            id
        } else {
            self.channel_storage
                .push_back(Box::new(P::from_channel_state(channel)));
            self.channel_storage.len() - 1
        };

        self.channel_map.insert(channel.bind_id, new_id);

        if self.current_channel_id != UNSET_CHANNEL {
            self.channel_state =
                Some((&mut *self.channel_storage[self.current_channel_id] as *mut P) as usize);
        }

        self.active_channel_ids.push(new_id);

        // Address-space bookkeeping.
        if let Some(ref mm) = channel.memory_manager {
            let mm_id = mm.lock().get_id();
            if let Some(entry) = self.address_spaces.get_mut(&mm_id) {
                entry.ref_count += 1;
                return;
            }
            let storage_id = self.address_spaces.len();
            self.address_spaces.insert(
                mm_id,
                AddressSpaceRef {
                    ref_count: 1,
                    storage_id,
                    gpu_memory: Arc::clone(mm),
                },
            );
            on_gpu_as_register(mm_id);
        }
    }

    /// Bind a channel for execution.
    ///
    /// Corresponds to `ChannelSetupCaches<P>::BindToChannel` (channel_state_cache.inc).
    pub fn bind_to_channel(&mut self, id: i32)
    where
        P: ChannelCacheAccessor,
    {
        let &storage_id = self
            .channel_map
            .get(&id)
            .expect("bind_to_channel: unknown channel id");
        assert!(id >= 0, "bind_to_channel: negative id");

        self.current_channel_id = storage_id;
        self.channel_state = Some((&mut *self.channel_storage[storage_id] as *mut P) as usize);

        let state = &*self.channel_storage[storage_id];
        self.maxwell3d = Some(state.maxwell3d_ref());
        self.kepler_compute = Some(state.kepler_compute_ref());
        self.gpu_memory = state.gpu_memory_arc();
        self.program_id = state.program_id_val();
        self.current_address_space = state.gpu_memory_id();
    }

    /// Erase channel's state.
    ///
    /// Corresponds to `ChannelSetupCaches<P>::EraseChannel` (channel_state_cache.inc).
    pub fn erase_channel(&mut self, id: i32) {
        let &storage_id = self
            .channel_map
            .get(&id)
            .expect("erase_channel: unknown channel id");
        assert!(id >= 0, "erase_channel: negative id");

        self.free_channel_ids.push_back(storage_id);
        self.channel_map.remove(&id);

        if storage_id == self.current_channel_id {
            self.current_channel_id = UNSET_CHANNEL;
            self.channel_state = None;
            self.maxwell3d = None;
            self.kepler_compute = None;
            self.gpu_memory = None;
            self.program_id = 0;
        } else if self.current_channel_id != UNSET_CHANNEL {
            self.channel_state =
                Some((&mut *self.channel_storage[self.current_channel_id] as *mut P) as usize);
        }

        if let Some(pos) = self
            .active_channel_ids
            .iter()
            .position(|&x| x == storage_id)
        {
            self.active_channel_ids.remove(pos);
        }
    }

    /// Look up the `MemoryManager` belonging to an address-space map id.
    ///
    /// Corresponds to `ChannelSetupCaches<P>::GetFromID`.
    pub fn get_from_id(&self, id: usize) -> Arc<Mutex<MemoryManager>> {
        Arc::clone(
            &self
                .address_spaces
                .get(&id)
                .expect("get_from_id: unknown address space")
                .gpu_memory,
        )
    }

    /// Look up the storage id for an address-space map id.
    ///
    /// Corresponds to `ChannelSetupCaches<P>::getStorageID`.
    pub fn get_storage_id(&self, id: usize) -> Option<usize> {
        self.address_spaces.get(&id).map(|r| r.storage_id)
    }

    pub fn current_channel_state(&self) -> Option<&P> {
        let channel_state = self.channel_state?;
        // `channel_storage` owns boxed entries, so insertion or deque growth
        // cannot move this pointee. Bind/erase keeps the address synchronized.
        Some(unsafe { &*(channel_state as *const P) })
    }

    pub fn has_current_channel_state(&self) -> bool {
        self.channel_state.is_some()
    }

    pub fn current_channel_state_mut(&mut self) -> Option<&mut P> {
        let channel_state = self.channel_state?;
        // `&mut self` excludes every other cache access while the returned
        // reference exists; the boxed pointee remains stable across growth.
        Some(unsafe { &mut *(channel_state as *mut P) })
    }

    pub fn channel_state_by_bind_id(&self, id: i32) -> Option<&P> {
        let &storage_id = self.channel_map.get(&id)?;
        self.channel_storage.get(storage_id).map(Box::as_ref)
    }

    pub fn channel_state_by_bind_id_mut(&mut self, id: i32) -> Option<&mut P> {
        let &storage_id = self.channel_map.get(&id)?;
        self.channel_storage.get_mut(storage_id).map(Box::as_mut)
    }

    pub fn for_each_active_channel_state_mut(&mut self, mut f: impl FnMut(&mut P)) {
        let active_channel_ids = self.active_channel_ids.clone();
        for id in active_channel_ids {
            if let Some(state) = self.channel_storage.get_mut(id) {
                f(state.as_mut());
            }
        }
    }
}

impl<P> Default for ChannelSetupCaches<P> {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Helper traits — replace C++ template parameter interface
// ---------------------------------------------------------------------------

/// Construct a `P` from a `ChannelState`.
///
/// Mirrors the upstream pattern where `ChannelSetupCaches<P>::CreateChannel`
/// placement-news a `P` from a `ChannelState&`.
pub trait FromChannelState {
    fn from_channel_state(state: &ChannelState) -> Self;
}

/// Accessor trait for reading cached engine references out of a `P`.
///
/// Mirrors the member accesses in `ChannelSetupCaches<P>::BindToChannel`.
pub trait ChannelCacheAccessor {
    fn maxwell3d_ref(&self) -> usize;
    fn kepler_compute_ref(&self) -> usize;
    fn gpu_memory_id(&self) -> usize;
    fn gpu_memory_arc(&self) -> Option<Arc<Mutex<MemoryManager>>>;
    fn program_id_val(&self) -> u64;
}

// ---------------------------------------------------------------------------
// Trait implementations for ChannelInfo
// ---------------------------------------------------------------------------

impl FromChannelState for ChannelInfo {
    fn from_channel_state(state: &ChannelState) -> Self {
        ChannelInfo::from_channel_state(state)
    }
}

impl ChannelCacheAccessor for ChannelInfo {
    fn maxwell3d_ref(&self) -> usize {
        self.maxwell3d
    }

    fn kepler_compute_ref(&self) -> usize {
        self.kepler_compute
    }

    fn gpu_memory_id(&self) -> usize {
        self.gpu_memory_index
    }

    fn gpu_memory_arc(&self) -> Option<Arc<Mutex<MemoryManager>>> {
        self.gpu_memory.as_ref().map(Arc::clone)
    }

    fn program_id_val(&self) -> u64 {
        self.program_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unset_channel_sentinel() {
        assert_eq!(UNSET_CHANNEL, usize::MAX);
    }

    #[test]
    fn test_channel_setup_caches_new() {
        let caches: ChannelSetupCaches<ChannelInfo> = ChannelSetupCaches::new();
        assert_eq!(caches.current_channel_id, UNSET_CHANNEL);
        assert!(caches.channel_map.is_empty());
        assert!(caches.active_channel_ids.is_empty());
    }

    #[test]
    fn test_create_and_erase_channel() {
        use crate::memory_manager::MemoryManager;
        use parking_lot::Mutex;
        use std::sync::Arc;

        let mut caches: ChannelSetupCaches<ChannelInfo> = ChannelSetupCaches::new();

        let mut cs = ChannelState::new(7);
        cs.program_id = 0x1234;
        cs.memory_manager = Some(Arc::new(Mutex::new(MemoryManager::new(99))));

        caches.create_channel(&cs);
        assert!(caches.channel_map.contains_key(&7));
        assert_eq!(caches.active_channel_ids.len(), 1);

        caches.erase_channel(7);
        assert!(!caches.channel_map.contains_key(&7));
        assert!(caches.active_channel_ids.is_empty());
    }

    #[test]
    fn free_channel_slots_are_reused_in_fifo_order() {
        let mut caches: ChannelSetupCaches<ChannelInfo> = ChannelSetupCaches::new();
        for bind_id in 1..=3 {
            let mut channel = ChannelState::new(bind_id);
            channel.program_id = bind_id as u64;
            caches.create_channel(&channel);
        }

        caches.erase_channel(1);
        caches.erase_channel(2);

        let mut fourth = ChannelState::new(4);
        fourth.program_id = 40;
        caches.create_channel(&fourth);
        let mut fifth = ChannelState::new(5);
        fifth.program_id = 50;
        caches.create_channel(&fifth);

        assert_eq!(caches.channel_map[&4], 0);
        assert_eq!(caches.channel_map[&5], 1);
        assert_eq!(caches.channel_storage.len(), 3);
        assert_eq!(caches.channel_storage[0].program_id, 40);
        assert_eq!(caches.channel_storage[1].program_id, 50);
    }

    #[test]
    fn test_bind_to_channel() {
        use crate::memory_manager::MemoryManager;
        use parking_lot::Mutex;
        use std::sync::Arc;

        let mut caches: ChannelSetupCaches<ChannelInfo> = ChannelSetupCaches::new();

        let mut cs = ChannelState::new(3);
        cs.program_id = 0xABCD;
        cs.memory_manager = Some(Arc::new(Mutex::new(MemoryManager::new(42))));

        caches.create_channel(&cs);
        caches.bind_to_channel(3);

        assert_eq!(caches.program_id, 0xABCD);
        assert!(caches.channel_state.is_some());
        let bound = caches
            .current_channel_state()
            .and_then(ChannelCacheAccessor::gpu_memory_arc)
            .expect("bound gpu memory");
        assert_eq!(bound.lock().get_id(), 42);
    }

    #[test]
    fn bound_channel_state_address_survives_storage_growth() {
        let mut caches: ChannelSetupCaches<ChannelInfo> = ChannelSetupCaches::new();

        let mut bound_channel = ChannelState::new(1);
        bound_channel.program_id = 0xCAFE;
        caches.create_channel(&bound_channel);
        caches.bind_to_channel(1);
        let bound_address = caches.channel_state.expect("bound channel address");

        for bind_id in 2..258 {
            let mut channel = ChannelState::new(bind_id);
            channel.program_id = bind_id as u64;
            caches.create_channel(&channel);
        }

        assert_eq!(caches.channel_state, Some(bound_address));
        assert_eq!(
            caches
                .current_channel_state()
                .expect("bound channel after storage growth")
                .program_id,
            0xCAFE
        );
    }

    #[test]
    fn create_channel_runs_gpu_as_register_once_per_address_space() {
        use crate::memory_manager::MemoryManager;
        use parking_lot::Mutex;
        use std::sync::Arc;

        let shared = Arc::new(Mutex::new(MemoryManager::new(42)));
        let separate = Arc::new(Mutex::new(MemoryManager::new(77)));
        let mut first = ChannelState::new(3);
        first.memory_manager = Some(Arc::clone(&shared));
        let mut second = ChannelState::new(4);
        second.memory_manager = Some(Arc::clone(&shared));
        let mut third = ChannelState::new(5);
        third.memory_manager = Some(Arc::clone(&separate));

        let mut registrations = Vec::new();
        let mut caches: ChannelSetupCaches<ChannelInfo> = ChannelSetupCaches::new();
        caches.create_channel_with_on_gpu_as_register(&first, |memory_id| {
            registrations.push(memory_id);
        });
        caches.create_channel_with_on_gpu_as_register(&second, |memory_id| {
            registrations.push(memory_id);
        });
        caches.create_channel_with_on_gpu_as_register(&third, |memory_id| {
            registrations.push(memory_id);
        });

        assert_eq!(registrations, vec![42, 77]);
        assert!(Arc::ptr_eq(&caches.get_from_id(42), &shared));
        assert!(Arc::ptr_eq(&caches.get_from_id(77), &separate));
    }

    #[test]
    fn test_channel_info_captures_live_engine_addresses() {
        use crate::engines::kepler_compute::KeplerCompute;
        use crate::engines::maxwell_3d::Maxwell3D;
        use crate::memory_manager::MemoryManager;
        use parking_lot::Mutex;
        use std::sync::Arc;

        let mm = Arc::new(Mutex::new(MemoryManager::new(77)));
        let mut cs = ChannelState::new(11);
        cs.program_id = 0xCAFE;
        cs.memory_manager = Some(Arc::clone(&mm));
        cs.maxwell_3d = Some(Box::new(Maxwell3D::new()));
        cs.kepler_compute = Some(Box::new(KeplerCompute::new(Arc::clone(&mm))));

        let info = ChannelInfo::from_channel_state(&cs);
        assert_ne!(info.maxwell3d, 0);
        assert_ne!(info.kepler_compute, 0);
        assert_eq!(info.gpu_memory_index, 77);
        assert_eq!(info.program_id, 0xCAFE);
    }
}
