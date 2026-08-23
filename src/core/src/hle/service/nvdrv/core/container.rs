// SPDX-FileCopyrightText: 2022 yuzu Emulator Project
// SPDX-FileCopyrightText: 2022 Skyline Team and Contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/core/hle/service/nvdrv/core/container.h
//! Port of zuyu/src/core/hle/service/nvdrv/core/container.cpp

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use super::heap_mapper::HeapMapper;
use super::nvmap::NvMap;
use super::syncpoint_manager::SyncpointManager;
use crate::core::SystemRef;
use crate::hle::kernel::k_memory_block::KMemoryState;
use crate::hle::kernel::k_process::ProcessLock;

const INVALID_ASID: u32 = u32::MAX;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionId {
    pub id: usize,
}

pub struct Session {
    pub id: SessionId,
    /// Matches upstream `Kernel::KProcess* process`.
    pub process: Option<Weak<ProcessLock>>,
    /// Host1x SMMU address-space identifier returned by
    /// `DeviceMemoryManager::RegisterProcess`.
    pub asid: u32,
    /// Process identity used for session reuse. This mirrors the upstream
    /// pointer-identity comparison in a Rust-friendly form.
    pub process_identity: usize,
    pub has_preallocated_area: bool,
    pub mapper: Option<HeapMapper>,
    pub is_active: bool,
    pub ref_count: i32,
}

impl Session {
    pub fn new(id: SessionId, process: &std::sync::Arc<ProcessLock>, asid: u32) -> Self {
        let process_identity = std::sync::Arc::as_ptr(process) as usize;
        Self {
            id,
            process: Some(std::sync::Arc::downgrade(process)),
            asid,
            process_identity,
            has_preallocated_area: false,
            mapper: None,
            is_active: false,
            ref_count: 0,
        }
    }
}

pub struct Host1xDeviceFileData {
    pub syncpts_accumulated: VecDeque<u32>,
}

impl Default for Host1xDeviceFileData {
    fn default() -> Self {
        Self {
            syncpts_accumulated: VecDeque::new(),
        }
    }
}

/// Inner state protected by a single mutex, matching the C++ ContainerImpl pattern.
pub(crate) struct ContainerInner {
    sessions: Vec<Session>,
    new_ids: usize,
    id_pool: VecDeque<usize>,
}

pub(crate) struct ContainerSessionStore {
    inner: Mutex<ContainerInner>,
}

impl ContainerSessionStore {
    fn new() -> Self {
        Self {
            inner: Mutex::new(ContainerInner {
                sessions: Vec::new(),
                new_ids: 0,
                id_pool: VecDeque::new(),
            }),
        }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, ContainerInner> {
        self.inner.lock().unwrap()
    }

    pub(crate) fn map_preallocated_area(
        &self,
        session_id: SessionId,
        address: u64,
        size: usize,
    ) -> Option<u64> {
        let inner = self.lock();
        let session = inner.sessions.get(session_id.id)?;
        if !session.is_active || !session.has_preallocated_area {
            return None;
        }
        let mapper = session.mapper.as_ref()?;
        if !mapper.is_in_bounds(address, size) {
            return None;
        }
        Some(mapper.map(address, size))
    }

    pub(crate) fn session_asid(&self, session_id: SessionId) -> Option<u32> {
        let inner = self.lock();
        let session = inner.sessions.get(session_id.id)?;
        if session.is_active && session.asid != INVALID_ASID {
            Some(session.asid)
        } else {
            None
        }
    }

    pub(crate) fn unmap_preallocated_area(
        &self,
        session_id: SessionId,
        address: u64,
        size: usize,
    ) -> bool {
        let inner = self.lock();
        Self::unmap_preallocated_area_locked(&inner, session_id, address, size)
    }

    pub(crate) fn unmap_preallocated_area_locked(
        inner: &ContainerInner,
        session_id: SessionId,
        address: u64,
        size: usize,
    ) -> bool {
        let Some(session) = inner.sessions.get(session_id.id) else {
            return false;
        };
        if !session.is_active || !session.has_preallocated_area {
            return false;
        }
        let Some(mapper) = session.mapper.as_ref() else {
            return false;
        };
        if !mapper.is_in_bounds(address, size) {
            return false;
        }
        mapper.unmap(address, size);
        true
    }
}

/// Container manages syncpoints on the host and provides access to NvMap and SyncpointManager.
#[derive(Clone)]
pub struct Container {
    file: Arc<NvMap>,
    manager: Arc<SyncpointManager>,
    device_file_data: Arc<Mutex<Host1xDeviceFileData>>,
    sessions: Arc<ContainerSessionStore>,
    system: SystemRef,
}

impl Container {
    #[cfg(test)]
    pub fn new() -> Self {
        Self::new_with_system(SystemRef::null())
    }

    pub fn new_with_system(system: SystemRef) -> Self {
        let sessions = Arc::new(ContainerSessionStore::new());
        Self {
            file: Arc::new(NvMap::new_with_system_and_sessions(
                system,
                Arc::clone(&sessions),
            )),
            manager: Arc::new(SyncpointManager::new_with_system(system)),
            device_file_data: Arc::new(Mutex::new(Host1xDeviceFileData::default())),
            sessions,
            system,
        }
    }

    fn host1x(&self) -> Option<&dyn crate::host1x_core::Host1xCoreInterface> {
        if self.system.is_null() {
            return None;
        }
        self.system.get().host1x_core()
    }

    /// Opens a session. If the same process already has an active session,
    /// increments its ref count and returns the existing session ID.
    ///
    /// Matches upstream `Container::OpenSession(KProcess* process)` for the
    /// session reuse, ASID registration, and preallocated heap ownership
    /// ordering. The ASID is registered with the process `Memory` owner,
    /// matching upstream `process->GetMemory()` storage in DeviceMemoryManager.
    pub fn open_session(&self, process: &std::sync::Arc<ProcessLock>) -> SessionId {
        let mut inner = self.sessions.lock();
        let process_identity = std::sync::Arc::as_ptr(process) as usize;

        for session in &mut inner.sessions {
            if !session.is_active {
                continue;
            }
            if session.process_identity == process_identity {
                session.ref_count += 1;
                return session.id;
            }
        }

        let process_memory = process.lock().unwrap().get_memory();
        let asid = if let Some(host1x) = self.host1x() {
            host1x.smmu_register_process(process_memory)
        } else {
            log::error!(
                "nvdrv::Container::open_session without Host1x; using invalid ASID for reduced construction"
            );
            INVALID_ASID
        };

        let new_id;
        if let Some(recycled_id) = inner.id_pool.pop_front() {
            new_id = recycled_id;
            if new_id < inner.sessions.len() {
                inner.sessions[new_id] = Session::new(SessionId { id: new_id }, process, asid);
            }
        } else {
            new_id = inner.new_ids;
            inner.new_ids += 1;
            inner
                .sessions
                .push(Session::new(SessionId { id: new_id }, process, asid));
        }

        let session = &mut inner.sessions[new_id];
        session.is_active = true;
        session.ref_count = 1;
        self.try_create_preallocated_area(session, process);

        SessionId { id: new_id }
    }

    fn try_create_preallocated_area(
        &self,
        session: &mut Session,
        process: &std::sync::Arc<ProcessLock>,
    ) {
        const MIN_PREALLOCATED_AREA_SIZE: usize = 32 * 1024 * 1024;

        let Some(host1x) = self.host1x() else {
            return;
        };

        let process = process.lock().unwrap();
        if !process.is_application() {
            return;
        }

        let page_table = &process.page_table;
        let heap_start = page_table.get_heap_region_start().get() as usize;
        let mut cur_addr = heap_start;
        let mut region_start = 0usize;
        let mut region_size = 0usize;

        loop {
            let Some(info) = page_table.query_info(cur_addr) else {
                break;
            };
            let svc_state = info.get_state() & KMemoryState::MASK;
            if svc_state == (KMemoryState::NORMAL & KMemoryState::MASK) {
                if region_start.wrapping_add(region_size) == info.get_address() {
                    region_size = region_size.wrapping_add(info.get_size());
                } else if info.get_size() > region_size {
                    region_start = info.get_address();
                    region_size = info.get_size();
                }
            }

            let next_addr = info.get_address().wrapping_add(info.get_size());
            if next_addr <= cur_addr {
                break;
            }
            cur_addr = next_addr;
        }

        session.has_preallocated_area = false;
        if region_size < MIN_PREALLOCATED_AREA_SIZE {
            return;
        }

        let start_region = host1x.smmu_allocate(region_size);
        if start_region == 0 {
            return;
        }

        session.mapper = Some(HeapMapper::new(
            region_start as u64,
            start_region,
            region_size,
            session.asid,
            self.system,
        ));
        host1x.smmu_track_continuity_registered(
            start_region,
            region_start as u64,
            region_size,
            session.asid,
        );
        session.has_preallocated_area = true;

        if std::env::var_os("RUZU_TRACE_NVMAP_PREALLOC").is_some() {
            log::info!(
                "nvdrv::Container preallocated heap session={} vaddr=0x{:X} daddr=0x{:X} size=0x{:X}",
                session.id.id,
                region_start,
                start_region,
                region_size
            );
        }
    }

    /// Closes a session by decrementing its ref count.
    /// When the ref count reaches zero, unmaps all handles and cleans up.
    pub fn close_session(&self, session_id: SessionId) {
        {
            let mut inner = self.sessions.lock();
            if session_id.id >= inner.sessions.len() {
                return;
            }

            let session = &mut inner.sessions[session_id.id];
            session.ref_count -= 1;
            if session.ref_count > 0 {
                return;
            }
        }

        // Upstream calls `NvMap::UnmapAllHandles` while the session and its
        // HeapMapper are still active, then tears the session down. Do not hold
        // the Rust session mutex here; `NvMap::UnmapHandle` reaches back into
        // the same session store for `session->mapper->Unmap(...)`.
        self.file.unmap_all_handles(session_id);

        let mut inner = self.sessions.lock();
        let session = &mut inner.sessions[session_id.id];
        let preallocated_region = session.mapper.as_ref().map(|mapper| {
            (
                session.has_preallocated_area,
                mapper.get_region_start(),
                mapper.get_region_size(),
            )
        });
        let asid = session.asid;
        if let Some((true, region_start, region_size)) = preallocated_region {
            session.mapper = None;
            session.has_preallocated_area = false;
            if let Some(host1x) = self.host1x() {
                host1x.smmu_free(region_start, region_size);
            }
        }

        session.is_active = false;
        session.process = None;
        session.process_identity = 0;
        if asid != INVALID_ASID {
            if let Some(host1x) = self.host1x() {
                host1x.smmu_unregister_process(asid);
            }
        }
        inner.id_pool.push_front(session_id.id);
    }

    /// Gets a reference to a session by ID.
    /// In the C++ code, this uses an atomic_thread_fence(acquire) before returning.
    pub fn get_session(&self, session_id: SessionId) -> Option<SessionId> {
        let inner = self.sessions.lock();
        if session_id.id < inner.sessions.len() && inner.sessions[session_id.id].is_active {
            Some(session_id)
        } else {
            None
        }
    }

    /// Returns the process owner for an active session.
    ///
    /// This is the Rust counterpart of upstream `GetSession(session_id)->process`.
    pub fn get_session_process(
        &self,
        session_id: SessionId,
    ) -> Option<std::sync::Arc<ProcessLock>> {
        let inner = self.sessions.lock();
        let session = inner.sessions.get(session_id.id)?;
        if !session.is_active {
            return None;
        }
        session.process.as_ref()?.upgrade()
    }

    pub fn get_nv_map_file(&self) -> &NvMap {
        self.file.as_ref()
    }

    pub fn get_nv_map_file_handle(&self) -> Arc<NvMap> {
        Arc::clone(&self.file)
    }

    pub fn get_syncpoint_manager(&self) -> &SyncpointManager {
        self.manager.as_ref()
    }

    pub fn get_syncpoint_manager_handle(&self) -> Arc<SyncpointManager> {
        Arc::clone(&self.manager)
    }

    pub fn take_accumulated_syncpoint(&self) -> Option<u32> {
        self.device_file_data
            .lock()
            .unwrap()
            .syncpts_accumulated
            .pop_front()
    }

    pub fn recycle_syncpoint(&self, syncpoint: u32) {
        self.device_file_data
            .lock()
            .unwrap()
            .syncpts_accumulated
            .push_back(syncpoint);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::Container;
    use crate::hle::kernel::k_process::{KProcess, ProcessLock};

    #[test]
    fn open_session_reuses_active_session_for_same_process() {
        let container = Container::new();
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));

        let first = container.open_session(&process);
        let second = container.open_session(&process);

        assert_eq!(first, second);

        container.close_session(first);
        let third = container.open_session(&process);

        assert_eq!(third, first);
    }

    #[test]
    fn ownerless_container_does_not_expose_valid_smmu_asid() {
        let container = Container::new();
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));

        let session = container.open_session(&process);

        assert_eq!(container.sessions.session_asid(session), None);
    }

    #[test]
    fn cloned_container_handle_preserves_session_ownership() {
        let container = Container::new();
        let shared_handle = container.clone();
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let session = container.open_session(&process);

        let resolved = shared_handle
            .get_session_process(session)
            .expect("the shared container handle must see the active session");

        assert!(Arc::ptr_eq(&resolved, &process));
    }
}
