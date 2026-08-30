// SPDX-FileCopyrightText: 2022 yuzu Emulator Project
// SPDX-FileCopyrightText: 2022 Skyline Team and Contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/core/hle/service/nvdrv/core/nvmap.h
//! Port of zuyu/src/core/hle/service/nvdrv/core/nvmap.cpp

use std::collections::{HashMap, LinkedList};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use crate::hle::service::nvdrv::core::container::{ContainerSessionStore, SessionId};
use crate::hle::service::nvdrv::nvdata::NvResult;

const PAGE_SIZE: u64 = 0x1000;

/// Handle flags mirroring the C++ BitField union.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct HandleFlags {
    pub raw: u32,
}

impl HandleFlags {
    pub fn map_uncached(&self) -> bool {
        self.raw & 1 != 0
    }

    pub fn set_map_uncached(&mut self, val: bool) {
        if val {
            self.raw |= 1;
        } else {
            self.raw &= !1;
        }
    }

    pub fn keep_uncached_after_free(&self) -> bool {
        (self.raw >> 2) & 1 != 0
    }

    pub fn set_keep_uncached_after_free(&mut self, val: bool) {
        if val {
            self.raw |= 1 << 2;
        } else {
            self.raw &= !(1 << 2);
        }
    }
}

pub type HandleId = u32;

/// A handle to a contiguous block of memory in an application's address space.
///
/// Interior mutability is achieved via `Mutex<HandleInner>` since many operations
/// need to modify handle state behind shared references.
pub struct Handle {
    pub id: HandleId,
    orig_size: AtomicU64,
    inner: Mutex<HandleInner>,
}

/// Mutable state of a Handle, protected by the Handle's mutex.
pub struct HandleInner {
    pub align: u64,
    pub size: u64,
    pub aligned_size: u64,
    pub dupes: i32,
    pub internal_dupes: i32,
    pub pins: i64,
    pub pin_virt_address: u32,
    pub flags: HandleFlags,
    pub address: u64,
    pub is_shared_mem_mapped: bool,
    pub kind: u8,
    pub allocated: bool,
    pub in_heap: bool,
    pub session_id: SessionId,
    pub d_address: u64,
}

impl Handle {
    pub fn new(size: u64, id: HandleId) -> Self {
        Self {
            id,
            orig_size: AtomicU64::new(size),
            inner: Mutex::new(HandleInner {
                align: 0,
                size,
                aligned_size: size,
                dupes: 1,
                internal_dupes: 0,
                pins: 0,
                pin_virt_address: 0,
                flags: HandleFlags { raw: 0 },
                address: 0,
                is_shared_mem_mapped: false,
                kind: 0,
                allocated: false,
                in_heap: false,
                session_id: SessionId { id: 0 },
                d_address: 0,
            }),
        }
    }

    /// Sets up the handle with the given memory config, can allocate memory from the tmem
    /// if a 0 address is passed.
    pub fn alloc(
        &self,
        p_flags: HandleFlags,
        p_align: u32,
        p_kind: u8,
        p_address: u64,
        p_session_id: SessionId,
    ) -> NvResult {
        let mut inner = self.inner.lock().unwrap();

        // Handles cannot be allocated twice
        if inner.allocated {
            return NvResult::AccessDenied;
        }

        inner.flags = p_flags;
        inner.kind = p_kind;
        inner.align = if (p_align as u64) < PAGE_SIZE {
            PAGE_SIZE
        } else {
            p_align as u64
        };
        inner.session_id = p_session_id;

        // This flag is only applicable for handles with an address passed
        if p_address != 0 {
            inner.flags.set_keep_uncached_after_free(false);
        } else {
            log::error!("Mapping nvmap handles without a CPU side address is unimplemented!");
        }

        inner.size = (inner.size + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        inner.aligned_size = (inner.size + inner.align - 1) & !(inner.align - 1);
        inner.address = p_address;
        inner.allocated = true;

        NvResult::Success
    }

    /// Increases the dupe counter of the handle for the given session.
    pub fn duplicate(&self, internal_session: bool) -> NvResult {
        let mut inner = self.inner.lock().unwrap();

        // Unallocated handles cannot be duplicated as duplication requires memory accounting (in HOS)
        if !inner.allocated {
            return NvResult::BadValue;
        }

        // If we internally use FromId the duplication tracking of handles won't work accurately due
        // to us not implementing per-process handle refs.
        if internal_session {
            inner.internal_dupes += 1;
        } else {
            inner.dupes += 1;
        }

        NvResult::Success
    }

    pub fn orig_size(&self) -> u64 {
        self.orig_size.load(Ordering::Relaxed)
    }

    pub fn set_orig_size(&self, size: u64) {
        self.orig_size.store(size, Ordering::Relaxed);
    }

    /// Locks the inner state and returns the guard for direct access.
    pub fn lock_inner(&self) -> std::sync::MutexGuard<'_, HandleInner> {
        self.inner.lock().unwrap()
    }
}

/// Encapsulates the result of a FreeHandle operation.
pub struct FreeInfo {
    pub address: u64,
    pub size: u64,
    pub was_uncached: bool,
    pub can_unlock: bool,
}

/// Each new handle ID is an increment of 4 from the previous.
const HANDLE_ID_INCREMENT: u32 = 4;

/// The nvmap core class holds the global state for nvmap and provides methods to manage handles.
pub struct NvMap {
    handles: Mutex<HashMap<HandleId, Arc<Handle>>>,
    unmap_queue: Mutex<LinkedList<Arc<Handle>>>,
    next_handle_id: AtomicU32,
    /// `SystemRef` used by `pin_handle` to reach `Host1x` and allocate/map
    /// SMMU addresses. Mirrors upstream `NvMap`'s `host1x` reference member
    /// passed through the constructor.
    system: crate::core::SystemRef,
    /// Runtime NvMap instances are built by `Container` and always carry this
    /// session store, matching upstream's `Container& core` member. `None` is
    /// reserved for reduced test construction through `NvMap::new()`.
    sessions: Option<Arc<ContainerSessionStore>>,
}

impl NvMap {
    #[cfg(test)]
    pub fn new() -> Self {
        Self {
            handles: Mutex::new(HashMap::new()),
            unmap_queue: Mutex::new(LinkedList::new()),
            next_handle_id: AtomicU32::new(HANDLE_ID_INCREMENT),
            system: crate::core::SystemRef::null(),
            sessions: None,
        }
    }

    pub(crate) fn new_with_system_and_sessions(
        system: crate::core::SystemRef,
        sessions: Arc<ContainerSessionStore>,
    ) -> Self {
        Self {
            handles: Mutex::new(HashMap::new()),
            unmap_queue: Mutex::new(LinkedList::new()),
            next_handle_id: AtomicU32::new(HANDLE_ID_INCREMENT),
            system,
            sessions: Some(sessions),
        }
    }

    fn host1x(&self) -> Option<&dyn crate::host1x_core::Host1xCoreInterface> {
        if self.system.is_null() {
            return None;
        }
        self.system.get().host1x_core()
    }

    /// Creates an unallocated handle of the given size.
    pub fn create_handle(&self, size: u64) -> Result<Arc<Handle>, NvResult> {
        if size == 0 {
            return Err(NvResult::BadValue);
        }

        let id = self
            .next_handle_id
            .fetch_add(HANDLE_ID_INCREMENT, Ordering::Relaxed);
        let handle = Arc::new(Handle::new(size, id));
        self.add_handle(handle.clone());
        Ok(handle)
    }

    pub fn get_handle(&self, handle_id: HandleId) -> Option<Arc<Handle>> {
        let handles = self.handles.lock().unwrap();
        handles.get(&handle_id).cloned()
    }

    pub fn get_handle_address(&self, handle_id: HandleId) -> u64 {
        let handles = self.handles.lock().unwrap();
        handles
            .get(&handle_id)
            .map(|h| {
                let inner = h.lock_inner();
                inner.d_address
            })
            .unwrap_or(0)
    }

    fn unmap_mapped_handle(&self, handle_id: HandleId, inner: &mut HandleInner) {
        if inner.d_address == 0 {
            return;
        }
        if inner.pin_virt_address != 0 {
            if let Some(host1x) = self.host1x() {
                host1x.host1x_gmmu_unmap_low(inner.pin_virt_address, inner.aligned_size as usize);
            }
        }
        if inner.in_heap {
            let map_size = inner.aligned_size as usize;
            let unmapped = self.sessions.as_ref().is_some_and(|sessions| {
                sessions.unmap_preallocated_area(inner.session_id, inner.address, map_size)
            });
            if !unmapped {
                log::warn!(
                    "nvmap handle 0x{:X}: failed to unmap heap-backed range session={} vaddr=0x{:X} size=0x{:X}",
                    handle_id,
                    inner.session_id.id,
                    inner.address,
                    map_size
                );
            }
        } else if let Some(host1x) = self.host1x() {
            const BIG_PAGE_SIZE: usize = PAGE_SIZE as usize * 16;
            let map_size = inner.aligned_size as usize;
            let aligned_up = (map_size + BIG_PAGE_SIZE - 1) & !(BIG_PAGE_SIZE - 1);
            host1x.smmu_unmap(inner.d_address, map_size);
            host1x.smmu_free(inner.d_address, aligned_up);
        }
        inner.pin_virt_address = 0;
        inner.d_address = 0;
        inner.in_heap = false;
    }

    /// Maps a handle into the SMMU address space.
    /// This operation is refcounted, the number of calls to this must eventually match the
    /// number of calls to `unpin_handle`.
    ///
    /// Mirrors upstream `NvMap::PinHandle`: handle refcounting, unmap-queue
    /// reuse, preallocated heap mapping, non-heap SMMU mapping, and optional
    /// low-area Host1x GMMU pinning all stay owned by this file.
    pub fn pin_handle(&self, handle_id: HandleId, low_area_pin: bool) -> u64 {
        let handle = match self.get_handle(handle_id) {
            Some(h) => h,
            None => return 0,
        };

        let mut inner = handle.lock_inner();

        let map_low_area = |inner: &mut HandleInner| {
            if inner.pin_virt_address == 0 {
                let Some(host1x) = self.host1x() else {
                    return;
                };
                inner.pin_virt_address =
                    host1x.host1x_gmmu_map_low(inner.d_address, inner.aligned_size as usize);
                if std::env::var_os("RUZU_TRACE_NVMAP_PIN").is_some() {
                    eprintln!(
                        "[NVMAP_LOW_PIN] handle=0x{:X} d_address=0x{:X} size=0x{:X} -> gpu=0x{:X}",
                        handle_id, inner.d_address, inner.aligned_size, inner.pin_virt_address
                    );
                }
            }
        };

        if inner.pins == 0 {
            // If we're in the unmap queue we can just remove ourselves and return since we're
            // already mapped
            {
                let mut unmap_queue = self.unmap_queue.lock().unwrap();
                // Check if this handle is in the unmap queue and remove it
                let mut found = false;
                let mut new_queue = LinkedList::new();
                while let Some(queued) = unmap_queue.pop_front() {
                    if queued.id == handle_id && !found {
                        found = true;
                        // Skip this entry - removing from queue
                    } else {
                        new_queue.push_back(queued);
                    }
                }
                *unmap_queue = new_queue;

                if found {
                    if low_area_pin {
                        map_low_area(&mut inner);
                    }
                    inner.pins += 1;
                    return if low_area_pin {
                        inner.pin_virt_address as u64
                    } else {
                        inner.d_address
                    };
                }
            }

            if inner.d_address == 0 {
                let map_size = inner.aligned_size as usize;
                if let Some(d_address) = self.sessions.as_ref().and_then(|sessions| {
                    sessions.map_preallocated_area(inner.session_id, inner.address, map_size)
                }) {
                    inner.d_address = d_address;
                    inner.in_heap = true;
                    if std::env::var_os("RUZU_TRACE_NVMAP_PIN").is_some() {
                        eprintln!(
                            "[NVMAP_PIN] handle=0x{:X} vaddr=0x{:X} size=0x{:X} -> prealloc d_address=0x{:X}",
                            handle_id, inner.address, map_size, d_address
                        );
                    }
                } else {
                    // Upstream fallback: allocate SMMU address space via
                    // `host1x.MemoryManager().Allocate(AlignUp(aligned_size,
                    // BIG_PAGE_SIZE))` and map through the process/asid-backed
                    // device memory manager.
                    let Some(host1x) = self.host1x() else {
                        log::error!(
                            "nvmap handle 0x{:X}: cannot pin without Host1x memory manager",
                            handle_id
                        );
                        return 0;
                    };
                    const BIG_PAGE_SIZE: usize = PAGE_SIZE as usize * 16;
                    let aligned_up = (map_size + BIG_PAGE_SIZE - 1) & !(BIG_PAGE_SIZE - 1);
                    let mut d_address = host1x.smmu_allocate(aligned_up);
                    while d_address == 0 {
                        let queued = self.unmap_queue.lock().unwrap().pop_front();
                        if let Some(queued) = queued {
                            let mut queued_inner = queued.lock_inner();
                            self.unmap_mapped_handle(queued.id, &mut queued_inner);
                        } else {
                            log::error!("Ran out of SMMU address space!");
                            return 0;
                        }
                        d_address = host1x.smmu_allocate(aligned_up);
                    }
                    let Some(sessions) = self.sessions.as_ref() else {
                        log::error!(
                            "nvmap handle 0x{:X}: runtime SMMU map missing Container session store",
                            handle_id
                        );
                        host1x.smmu_free(d_address, aligned_up);
                        inner.d_address = 0;
                        return 0;
                    };
                    let Some(asid) = sessions.session_asid(inner.session_id) else {
                        log::error!(
                            "nvmap handle 0x{:X}: active session {} has no registered SMMU ASID",
                            handle_id,
                            inner.session_id.id
                        );
                        host1x.smmu_free(d_address, aligned_up);
                        inner.d_address = 0;
                        return 0;
                    };
                    host1x.smmu_map(d_address, inner.address, map_size, asid, true);
                    inner.d_address = d_address;
                    inner.in_heap = false;
                    if std::env::var_os("RUZU_TRACE_NVMAP_PIN").is_some() {
                        eprintln!(
                        "[NVMAP_PIN] handle=0x{:X} vaddr=0x{:X} size=0x{:X} -> asid-backed d_address=0x{:X}",
                        handle_id, inner.address, map_size, d_address
                    );
                    }
                }
            }
        }

        if low_area_pin {
            map_low_area(&mut inner);
        }
        inner.pins += 1;
        if low_area_pin {
            inner.pin_virt_address as u64
        } else {
            inner.d_address
        }
    }

    /// When this has been called an equal number of times to `pin_handle` for the supplied
    /// handle it will be added to a list of handles to be freed when necessary.
    pub fn unpin_handle(&self, handle_id: HandleId) {
        let handle = match self.get_handle(handle_id) {
            Some(h) => h,
            None => return,
        };

        let mut inner = handle.lock_inner();
        inner.pins -= 1;

        if inner.pins < 0 {
            log::warn!("Pin count imbalance detected!");
        } else if inner.pins == 0 {
            // Add to the unmap queue allowing this handle's memory to be freed if needed
            // while the handle stays locked. Upstream holds Handle::mutex while
            // acquiring unmap_queue_lock, so PinHandle cannot observe pins == 0
            // before the queue entry exists and leave a live handle queued for
            // reclamation.
            let mut unmap_queue = self.unmap_queue.lock().unwrap();
            unmap_queue.push_back(Arc::clone(&handle));
        }
    }

    /// Tries to duplicate a handle.
    pub fn duplicate_handle(&self, handle_id: HandleId, internal_session: bool) {
        let handle = match self.get_handle(handle_id) {
            Some(h) => h,
            None => {
                log::error!("Unregistered handle!");
                return;
            }
        };

        let result = handle.duplicate(internal_session);
        if result != NvResult::Success {
            log::error!("Could not duplicate handle!");
        }
    }

    /// Tries to free a handle and remove a single dupe.
    /// If a handle has no dupes left and has no other users a FreeInfo struct will be returned
    /// describing the prior state of the handle.
    pub fn free_handle(&self, handle_id: HandleId, internal_session: bool) -> Option<FreeInfo> {
        // We use a weak ptr here so we can tell when the handle has been freed and report that
        // back to guest
        let handle_weak: Weak<Handle>;
        let free_info: FreeInfo;

        {
            let handle = match self.get_handle(handle_id) {
                Some(h) => h,
                None => return None,
            };
            handle_weak = Arc::downgrade(&handle);

            let mut inner = handle.lock_inner();

            if internal_session {
                inner.internal_dupes -= 1;
                if inner.internal_dupes < 0 {
                    log::warn!("Internal duplicate count imbalance detected!");
                }
            } else {
                inner.dupes -= 1;
                if inner.dupes < 0 {
                    log::warn!("User duplicate count imbalance detected!");
                } else if inner.dupes == 0 {
                    // Force unmap the handle
                    if inner.d_address != 0 {
                        // In the C++ code, this calls UnmapHandle which unmaps from SMMU.
                        let mut unmap_queue = self.unmap_queue.lock().unwrap();
                        let mut new_queue = LinkedList::new();
                        while let Some(queued) = unmap_queue.pop_front() {
                            if queued.id != handle_id {
                                new_queue.push_back(queued);
                            }
                        }
                        *unmap_queue = new_queue;

                        self.unmap_mapped_handle(handle_id, &mut inner);
                    }

                    inner.pins = 0;
                }
            }

            // Try to remove the shared ptr to the handle from the map
            if inner.dupes == 0 && inner.internal_dupes == 0 {
                let mut handles = self.handles.lock().unwrap();
                handles.remove(&handle_id);
                log::debug!("Removed nvmap handle: {}", handle_id);
            } else {
                log::debug!(
                    "Tried to free nvmap handle: {} but didn't as it still has duplicates",
                    handle_id
                );
            }

            free_info = FreeInfo {
                address: inner.address,
                size: inner.size,
                was_uncached: inner.flags.map_uncached(),
                can_unlock: true,
            };
        }

        // If the handle hasn't been freed from memory, mark that
        if handle_weak.strong_count() > 0 {
            log::debug!(
                "nvmap handle: {} wasn't freed as it is still in use",
                handle_id
            );
            return Some(FreeInfo {
                can_unlock: false,
                ..free_info
            });
        }

        Some(free_info)
    }

    /// Unmaps all handles belonging to a given session.
    pub fn unmap_all_handles(&self, session_id: SessionId) {
        // Take a snapshot of handles to avoid holding the lock during free
        let handles_copy: Vec<(HandleId, Arc<Handle>)> = {
            let handles = self.handles.lock().unwrap();
            handles.iter().map(|(&id, h)| (id, h.clone())).collect()
        };

        for (id, handle) in handles_copy {
            let should_free = {
                let inner = handle.lock_inner();
                inner.session_id.id == session_id.id && inner.dupes > 0
            };
            if should_free {
                self.free_handle(id, false);
            }
        }
    }

    fn add_handle(&self, handle: Arc<Handle>) {
        let mut handles = self.handles.lock().unwrap();
        handles.insert(handle.id, handle);
    }
}

#[cfg(test)]
mod tests {
    use super::{HandleFlags, NvMap};
    use crate::hle::service::nvdrv::core::container::SessionId;

    #[test]
    fn free_heap_backed_handle_clears_mapping_state() {
        let nvmap = NvMap::new();
        let handle = nvmap.create_handle(0x3000).unwrap();
        let session_id = SessionId { id: 7 };
        assert_eq!(
            handle.alloc(HandleFlags::default(), 0x1000, 0, 0x8000, session_id),
            crate::hle::service::nvdrv::nvdata::NvResult::Success
        );

        {
            let mut inner = handle.lock_inner();
            inner.d_address = 0x20000;
            inner.in_heap = true;
            inner.pins = 1;
        }

        let free_info = nvmap.free_handle(handle.id, false);

        assert!(free_info.is_some());
        let inner = handle.lock_inner();
        assert_eq!(inner.d_address, 0);
        assert!(!inner.in_heap);
        assert_eq!(inner.pins, 0);
    }

    #[test]
    fn pin_without_host1x_does_not_fabricate_device_address() {
        let nvmap = NvMap::new();
        let handle = nvmap.create_handle(0x3000).unwrap();
        let session_id = SessionId { id: 7 };
        assert_eq!(
            handle.alloc(HandleFlags::default(), 0x1000, 0, 0x8000, session_id),
            crate::hle::service::nvdrv::nvdata::NvResult::Success
        );

        assert_eq!(nvmap.pin_handle(handle.id, false), 0);

        let inner = handle.lock_inner();
        assert_eq!(inner.d_address, 0);
        assert_eq!(inner.pins, 0);
    }

    #[test]
    fn pin_zero_cpu_address_still_attempts_mapping_like_upstream() {
        let nvmap = NvMap::new();
        let handle = nvmap.create_handle(0x3000).unwrap();
        let session_id = SessionId { id: 7 };
        assert_eq!(
            handle.alloc(HandleFlags::default(), 0x1000, 0, 0, session_id),
            crate::hle::service::nvdrv::nvdata::NvResult::Success
        );

        assert_eq!(nvmap.pin_handle(handle.id, false), 0);

        let inner = handle.lock_inner();
        assert_eq!(inner.d_address, 0);
        assert_eq!(inner.pins, 0);
    }

    #[test]
    fn repin_removes_the_zero_pin_handle_from_the_unmap_queue() {
        let nvmap = NvMap::new();
        let handle = nvmap.create_handle(0x3000).unwrap();
        {
            let mut inner = handle.lock_inner();
            inner.d_address = 0x20000;
            inner.pins = 1;
        }

        nvmap.unpin_handle(handle.id);
        assert_eq!(nvmap.unmap_queue.lock().unwrap().len(), 1);

        assert_eq!(nvmap.pin_handle(handle.id, false), 0x20000);
        assert!(nvmap.unmap_queue.lock().unwrap().is_empty());
        assert_eq!(handle.lock_inner().pins, 1);
    }
}
