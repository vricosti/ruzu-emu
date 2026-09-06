// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/os/multi_wait_holder.h
//! Port of zuyu/src/core/hle/service/os/multi_wait_holder.cpp
//!
//! MultiWaitHolder — holds a synchronization object for MultiWait.

use std::ptr::NonNull;
use std::sync::{Arc, Mutex};

use crate::hle::kernel::k_port::KPort;
use crate::hle::kernel::k_readable_event::KReadableEvent;
use crate::hle::kernel::k_server_session::KServerSession;

use super::event::Event;
use super::multi_wait::MultiWait;
use crate::hle::kernel::k_process::ProcessLock;
use crate::hle::kernel::k_synchronization_object::WaitableObject;

enum WaitableHandle {
    None,
    Event(Arc<Event>),
    ReadableEvent(Arc<Mutex<KReadableEvent>>),
    Process(Arc<ProcessLock>),
    ServerPort {
        port: Arc<Mutex<KPort>>,
        object_id: Option<u64>,
    },
    ServerSession(Arc<Mutex<KServerSession>>),
}

/// MultiWaitHolder — holds a native synchronization object handle.
///
/// Upstream stores a native kernel handle and linkage into the parent
/// MultiWait's intrusive list. We store an optional Event reference
/// and a linked flag.
pub struct MultiWaitHolder {
    user_data: usize,
    multi_wait: Option<NonNull<MultiWait>>,
    native_handle: WaitableHandle,
}

// Matches the ownership model used throughout the service layer: the raw
// `multi_wait` pointer is only linkage metadata, while the waited objects are
// owned through `Arc` and synchronized independently.
unsafe impl Send for MultiWaitHolder {}
unsafe impl Sync for MultiWaitHolder {}

impl MultiWaitHolder {
    pub fn new() -> Self {
        Self {
            user_data: 0,
            multi_wait: None,
            native_handle: WaitableHandle::None,
        }
    }

    /// Create a holder wrapping an Event.
    pub fn from_event(event: Arc<Event>) -> Self {
        Self {
            user_data: 0,
            multi_wait: None,
            native_handle: WaitableHandle::Event(event),
        }
    }

    /// Create a holder wrapping a kernel readable event directly.
    pub fn from_readable_event(readable_event: Arc<Mutex<KReadableEvent>>) -> Self {
        Self {
            user_data: 0,
            multi_wait: None,
            native_handle: WaitableHandle::ReadableEvent(readable_event),
        }
    }

    /// Create a holder wrapping a process synchronization object.
    pub fn from_process(process: Arc<ProcessLock>) -> Self {
        Self {
            user_data: 0,
            multi_wait: None,
            native_handle: WaitableHandle::Process(process),
        }
    }

    /// Create a holder wrapping a server-port synchronization object.
    pub fn from_server_port(server_port: Arc<Mutex<KPort>>, object_id: Option<u64>) -> Self {
        Self {
            user_data: 0,
            multi_wait: None,
            native_handle: WaitableHandle::ServerPort {
                port: server_port,
                object_id,
            },
        }
    }

    /// Create a holder wrapping a server-session synchronization object.
    pub fn from_server_session(server_session: Arc<Mutex<KServerSession>>) -> Self {
        Self {
            user_data: 0,
            multi_wait: None,
            native_handle: WaitableHandle::ServerSession(server_session),
        }
    }

    /// Set user data associated with this holder.
    pub fn set_user_data(&mut self, data: usize) {
        self.user_data = data;
    }

    /// Get user data associated with this holder.
    pub fn get_user_data(&self) -> usize {
        self.user_data
    }

    /// Check if the held object is signaled.
    pub fn is_signaled(&self) -> bool {
        match &self.native_handle {
            WaitableHandle::None => false,
            WaitableHandle::Event(event) => event.is_signaled(),
            WaitableHandle::ReadableEvent(event) => event.lock().unwrap().is_signaled(),
            WaitableHandle::Process(process) => process.lock().unwrap().is_signaled(),
            WaitableHandle::ServerPort { port, .. } => port.lock().unwrap().server.is_signaled(),
            WaitableHandle::ServerSession(server_session) => {
                server_session.lock().unwrap().is_signaled()
            }
        }
    }

    pub fn object_id(&self) -> Option<u64> {
        match &self.native_handle {
            WaitableHandle::None => None,
            WaitableHandle::Event(event) => event.kernel_object_id(),
            WaitableHandle::ReadableEvent(event) => Some(event.lock().unwrap().object_id),
            WaitableHandle::Process(process) => Some(process.lock().unwrap().get_process_id()),
            WaitableHandle::ServerPort { object_id, .. } => *object_id,
            WaitableHandle::ServerSession(server_session) => {
                server_session.lock().unwrap().get_parent_id()
            }
        }
    }

    /// Host counterpart of GetNativeHandle for the local Event wait path.
    pub(super) fn host_event(&self) -> Option<&Event> {
        match &self.native_handle {
            WaitableHandle::Event(event) => Some(event),
            _ => None,
        }
    }

    /// Return the native synchronization object held by this holder.
    ///
    /// Mirrors upstream `MultiWaitHolder::GetNativeHandle()`: `MultiWait`
    /// already owns the synchronization object and must pass it directly to
    /// `KSynchronizationObject::Wait`, without resolving its numeric identity
    /// through the current process object table.
    pub(crate) fn native_waitable_object(&self) -> Option<(u64, WaitableObject)> {
        match &self.native_handle {
            WaitableHandle::None => None,
            WaitableHandle::Event(event) => {
                let readable_event = event.readable_event()?;
                let object_id = readable_event.lock().unwrap().object_id;
                Some((
                    object_id,
                    WaitableObject::from_readable_event(readable_event),
                ))
            }
            WaitableHandle::ReadableEvent(event) => {
                let object_id = event.lock().unwrap().object_id;
                Some((
                    object_id,
                    WaitableObject::from_readable_event(Arc::clone(event)),
                ))
            }
            WaitableHandle::Process(process) => {
                let object_id = process.lock().unwrap().get_process_id();
                Some((object_id, WaitableObject::from_process(Arc::clone(process))))
            }
            WaitableHandle::ServerPort { port, object_id } => Some((
                (*object_id)?,
                WaitableObject::from_server_port(Arc::clone(port)),
            )),
            WaitableHandle::ServerSession(server_session) => {
                let object_id = server_session.lock().unwrap().get_parent_id()?;
                Some((
                    object_id,
                    WaitableObject::from_server_session(Arc::clone(server_session)),
                ))
            }
        }
    }

    pub fn kind_name(&self) -> &'static str {
        match &self.native_handle {
            WaitableHandle::None => "none",
            WaitableHandle::Event(_) => "evt",
            WaitableHandle::ReadableEvent(_) => "rev",
            WaitableHandle::Process(_) => "prc",
            WaitableHandle::ServerPort { .. } => "prt",
            WaitableHandle::ServerSession(_) => "ses",
        }
    }

    /// Link this holder to a MultiWait.
    pub fn link_to_multi_wait(&mut self, multi_wait: *mut MultiWait) {
        assert!(self.multi_wait.is_none(), "holder already linked");
        self.multi_wait = NonNull::new(multi_wait);
        unsafe {
            (*multi_wait).holders.push(self as *mut MultiWaitHolder);
        }
    }

    /// Unlink this holder from its MultiWait.
    pub fn unlink_from_multi_wait(&mut self) {
        let Some(mut multi_wait) = self.multi_wait.take() else {
            return;
        };
        unsafe {
            multi_wait
                .as_mut()
                .holders
                .retain(|&holder| !std::ptr::eq(holder, self as *mut MultiWaitHolder));
        }
    }

    /// Check if currently linked.
    pub fn is_linked(&self) -> bool {
        self.multi_wait.is_some()
    }

    /// Clear the stored intrusive-owner pointer without touching the old list.
    ///
    /// Rust-specific ownership repair for move-sensitive service owners such as
    /// `ServerManager`: upstream stores the manager behind a stable pointee
    /// (`unique_ptr`), while Rust can move the owner struct into an `Arc`,
    /// invalidating the raw `MultiWait*` kept here. Callers must rebuild the
    /// destination list explicitly after resetting this linkage.
    pub fn reset_multi_wait_linkage_for_owner_move(&mut self) {
        self.multi_wait = None;
    }
}

impl Default for MultiWaitHolder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_readable_event_does_not_require_process_table_resolution() {
        let mut readable_event = KReadableEvent::new();
        readable_event.initialize(0, 0x1234);
        assert_eq!(
            readable_event.signal(),
            crate::hle::result::RESULT_SUCCESS.get_inner_value()
        );
        let holder = MultiWaitHolder::from_readable_event(Arc::new(Mutex::new(readable_event)));

        let (object_id, waitable) = holder
            .native_waitable_object()
            .expect("holder must retain its native readable event");

        assert_eq!(object_id, 0x1234);
        assert!(waitable.is_signaled());
    }
}
