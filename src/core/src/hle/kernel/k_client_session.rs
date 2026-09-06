//! Port of zuyu/src/core/hle/kernel/k_client_session.h / k_client_session.cpp
//! Status: Partial (structural port)
//! Derniere synchro: 2026-03-11
//!
//! KClientSession: the client endpoint of a session, used to send IPC requests.

use std::sync::{Arc, Mutex};

use crate::hle::kernel::k_process::KProcess;
use crate::hle::kernel::k_session::KSession;
use crate::hle::kernel::k_session_request::KSessionRequest;

/// The client session object.
/// Matches upstream `KClientSession` class (k_client_session.h).
pub struct KClientSession {
    /// Parent KSession ID.
    pub parent_id: Option<u64>,
}

impl KClientSession {
    pub fn new() -> Self {
        Self { parent_id: None }
    }

    /// Initialize with a parent session.
    pub fn initialize(&mut self, parent_id: u64) {
        self.parent_id = Some(parent_id);
    }

    pub fn get_parent_id(&self) -> Option<u64> {
        self.parent_id
    }

    /// Send a synchronous IPC request.
    /// Port of upstream `KClientSession::SendSyncRequest`.
    ///
    /// Rust still routes the active SVC path through `send_sync_request_with_process(...)`
    /// so the caller can reuse the already-held owner process. This fallback now
    /// creates a real `KSessionRequest` owner, but it still re-enters the process
    /// registry and should be migrated off the hot runtime path.
    pub fn send_sync_request(&mut self, address: usize, size: usize) -> u32 {
        let Some(parent_id) = self.parent_id else {
            return 1;
        };
        let Some(kernel) = crate::hle::kernel::kernel::get_kernel_ref() else {
            return 1;
        };
        let Some(owner_process_id) = kernel.get_session_owner_process_id(parent_id) else {
            return 1;
        };
        let Some(process_arc) = kernel.get_process_by_id(owner_process_id) else {
            return 1;
        };
        let mut process = process_arc.lock().unwrap();
        self.send_sync_request_with_process(&mut process, address, size)
    }

    /// Port of upstream `KClientSession::SendSyncRequest` when the caller
    /// already holds the owning `KProcess`.
    ///
    /// Rust now creates a real `KSessionRequest` owner in `k_session_request.rs`
    /// before forwarding to the parent `KSession`. The upstream slab lifecycle
    /// is still approximated with `Arc<Mutex<_>>`.
    pub fn send_sync_request_with_process(
        &mut self,
        process: &mut KProcess,
        address: usize,
        size: usize,
    ) -> u32 {
        let Some(parent_id) = self.parent_id else {
            return 1;
        };
        let Some(parent_session) = process.get_session_by_object_id(parent_id) else {
            return 1;
        };
        Self::send_sync_request_to_parent_with_process(process, parent_session, address, size)
    }

    /// Rust equivalent of upstream `m_parent->OnRequest(request)` once the
    /// parent `KSession` has already been resolved by the caller.
    pub fn send_sync_request_to_parent_with_process(
        process: &mut KProcess,
        parent_session: Arc<Mutex<KSession>>,
        address: usize,
        size: usize,
    ) -> u32 {
        Self::send_request_to_parent_with_process(process, parent_session, None, address, size)
    }

    /// Send an asynchronous IPC request.
    /// Port of upstream `KClientSession::SendAsyncRequest`.
    pub fn send_async_request(&mut self, _event_id: u64, _address: usize, _size: usize) -> u32 {
        let Some(parent_id) = self.parent_id else {
            return 1;
        };
        let Some(kernel) = crate::hle::kernel::kernel::get_kernel_ref() else {
            return 1;
        };
        let Some(owner_process_id) = kernel.get_session_owner_process_id(parent_id) else {
            return 1;
        };
        let Some(process_arc) = kernel.get_process_by_id(owner_process_id) else {
            return 1;
        };
        let mut process = process_arc.lock().unwrap();
        self.send_async_request_with_process(&mut process, _event_id, _address, _size)
    }

    /// Port of upstream `KClientSession::SendAsyncRequest` when the caller
    /// already holds the owning `KProcess`.
    pub fn send_async_request_with_process(
        &mut self,
        process: &mut KProcess,
        event_id: u64,
        address: usize,
        size: usize,
    ) -> u32 {
        let Some(parent_id) = self.parent_id else {
            return 1;
        };
        let Some(parent_session) = process.get_session_by_object_id(parent_id) else {
            return 1;
        };
        Self::send_async_request_to_parent_with_process(
            process,
            parent_session,
            event_id,
            address,
            size,
        )
    }

    /// Rust equivalent of upstream `m_parent->OnRequest(request)` for async
    /// IPC once the parent `KSession` has already been resolved.
    pub fn send_async_request_to_parent_with_process(
        process: &mut KProcess,
        parent_session: Arc<Mutex<KSession>>,
        event_id: u64,
        address: usize,
        size: usize,
    ) -> u32 {
        Self::send_request_to_parent_with_process(
            process,
            parent_session,
            Some(event_id),
            address,
            size,
        )
    }

    fn send_request_to_parent_with_process(
        process: &mut KProcess,
        parent_session: Arc<Mutex<KSession>>,
        event_id: Option<u64>,
        address: usize,
        size: usize,
    ) -> u32 {
        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        request
            .lock()
            .unwrap()
            .initialize_with_process(process, event_id, address, size);
        let (server_endpoint, trace_object_id) = {
            let session = parent_session.lock().unwrap();
            (
                Arc::clone(session.get_server_session()),
                session.name as u64,
            )
        };
        KSession::on_server_request_defer_scheduler_unlock(
            &server_endpoint,
            trace_object_id,
            request,
        )
    }

    /// Called when the server side is closed.
    /// Port of upstream `KClientSession::OnServerClosed`.
    /// Upstream is a no-op.
    pub fn on_server_closed(&mut self) {
        // Upstream: empty body.
    }

    /// Destroy the client session.
    /// Port of upstream `KClientSession::Destroy`.
    pub fn destroy(&mut self) {
        let Some(parent_id) = self.parent_id else {
            return;
        };
        let Some(kernel) = crate::hle::kernel::kernel::get_kernel_ref() else {
            self.parent_id = None;
            return;
        };
        let Some(owner_process_id) = kernel.get_session_owner_process_id(parent_id) else {
            self.parent_id = None;
            return;
        };
        let Some(process_arc) = kernel.get_process_by_id(owner_process_id) else {
            self.parent_id = None;
            return;
        };
        let mut process = process_arc.lock().unwrap();
        self.destroy_with_process(&mut process);
    }

    /// Port of upstream `KClientSession::Destroy` when the owning process is
    /// already held by the caller.
    ///
    /// Upstream calls `m_parent->OnClientClosed(); m_parent->Close();`. Rust
    /// represents the `Close()` refcount through handle/Arc registry cleanup,
    /// but the parent close notification must still run before the endpoint is
    /// removed from `KProcess`.
    pub fn destroy_with_process(&mut self, process: &mut KProcess) {
        let Some(parent_id) = self.parent_id.take() else {
            return;
        };

        let should_finalize =
            if let Some(parent_session) = process.get_session_by_object_id(parent_id) {
                KSession::on_client_closed(&parent_session);
                let mut parent = parent_session.lock().unwrap();
                parent.close_client_endpoint()
            } else {
                false
            };

        if should_finalize {
            process.unregister_session_object_by_object_id(parent_id);
        }
    }
}

impl Default for KClientSession {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::kernel::k_process::KProcess;
    use crate::hle::kernel::k_session::KSession;
    use crate::hle::kernel::k_thread::{KThread, KThreadLock, ThreadState};
    use std::sync::{Arc, Mutex};

    #[test]
    fn client_close_releases_parent_before_waiting_for_server() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let session = Arc::new(Mutex::new(KSession::new()));
        let (client, server) = {
            let mut parent = session.lock().unwrap();
            parent.initialize(None, 0);
            parent.client.lock().unwrap().initialize(0x1000);
            (parent.client.clone(), parent.server.clone())
        };
        let server_guard = server.lock().unwrap();
        let closing_session = session.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let close_thread = std::thread::spawn(move || {
            let mut process = KProcess::new();
            process.register_session_object(0x1000, closing_session);
            started_tx.send(()).unwrap();
            client.lock().unwrap().destroy_with_process(&mut process);
            process.get_session_by_object_id(0x1000).is_none()
        });
        started_rx.recv().unwrap();

        // Hold the server endpoint as the concurrent server destroy does.
        // Even while its notification is blocked, client close must release
        // the parent and retain its parent reference until notification ends.
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut parent_available = false;
        let mut finalized_early = false;
        while Instant::now() < deadline {
            if let Ok(mut parent) = session.try_lock() {
                if parent.is_client_closed() {
                    // Both closed predicates mean non-Normal upstream. Here
                    // only the client destroy can have made that transition.
                    parent_available = true;
                    parent.on_server_closed();
                    finalized_early = parent.close_server_endpoint();
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }

        // Always unblock and join before asserting, including on the old bug.
        drop(server_guard);
        let finalized_after_notification = close_thread.join().unwrap();
        assert!(
            parent_available,
            "client close held parent while waiting for server"
        );
        assert!(
            !finalized_early,
            "client reference released before notification"
        );
        assert!(server.lock().unwrap().client_closed);
        assert!(finalized_after_notification);
    }

    fn install_test_current_thread(thread_id: u64) -> Arc<KThreadLock> {
        let current_thread = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut thread = current_thread.lock().unwrap();
            thread.thread_id = thread_id;
            thread.set_state(ThreadState::RUNNABLE);
        }
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&current_thread));
        current_thread
    }

    #[test]
    fn send_sync_request_with_process_enqueues_real_session_request() {
        let mut process = KProcess::new();
        let session = Arc::new(Mutex::new(KSession::new()));
        {
            let mut session_guard = session.lock().unwrap();
            session_guard.initialize(None, 0);
            session_guard.client.lock().unwrap().initialize(0x1000);
            session_guard.server.lock().unwrap().initialize(0x1000);
        }
        process.register_session_object(0x1000, Arc::clone(&session));

        let _current_thread = install_test_current_thread(1);

        let client_session = session.lock().unwrap().get_client_session().clone();
        assert_eq!(
            client_session
                .lock()
                .unwrap()
                .send_sync_request_with_process(&mut process, 0x2395000, 0x80),
            0
        );
        crate::hle::kernel::kernel::set_current_emu_thread(None);

        let server_session = session.lock().unwrap().get_server_session().clone();
        assert_eq!(server_session.lock().unwrap().receive_request(), 0);
        let current_request = server_session
            .lock()
            .unwrap()
            .get_current_request()
            .expect("queued request");
        let current_request = current_request.lock().unwrap();
        assert_eq!(current_request.get_address(), 0x2395000);
        assert_eq!(current_request.get_size(), 0x80);
    }

    #[test]
    fn destroy_with_process_notifies_parent_server_session() {
        let mut process = KProcess::new();
        let session = Arc::new(Mutex::new(KSession::new()));
        {
            let mut session_guard = session.lock().unwrap();
            session_guard.initialize(None, 0);
            session_guard.client.lock().unwrap().initialize(0x1000);
            session_guard.server.lock().unwrap().initialize(0x1000);
        }
        let client_session = session.lock().unwrap().get_client_session().clone();
        let server_session = session.lock().unwrap().get_server_session().clone();
        process.register_session_object(0x1000, Arc::clone(&session));

        client_session
            .lock()
            .unwrap()
            .destroy_with_process(&mut process);

        let server = server_session.lock().unwrap();
        assert!(server.client_closed);
        assert!(server.is_signaled());
    }

    #[test]
    fn closing_last_client_session_handle_notifies_parent_server_session() {
        let mut process = KProcess::new();
        assert_eq!(process.initialize_handle_table(), 0);

        let session = Arc::new(Mutex::new(KSession::new()));
        {
            let mut session_guard = session.lock().unwrap();
            session_guard.initialize(None, 0);
            session_guard.client.lock().unwrap().initialize(0x1000);
            session_guard.server.lock().unwrap().initialize(0x1000);
        }
        let client_session = session.lock().unwrap().get_client_session().clone();
        let server_session = session.lock().unwrap().get_server_session().clone();
        process.register_session_object(0x1000, Arc::clone(&session));
        process.register_client_session_object(0x2000, client_session, 0x1000);

        let handle = process.handle_table.add(0x2000).unwrap();
        assert!(process.remove_handle(handle));

        assert!(process.get_client_session_by_object_id(0x2000).is_none());
        let server = server_session.lock().unwrap();
        assert!(server.client_closed);
        assert!(server.is_signaled());
    }

    #[test]
    fn send_async_request_with_process_enqueues_request_with_event() {
        let mut process = KProcess::new();
        let session = Arc::new(Mutex::new(KSession::new()));
        {
            let mut session_guard = session.lock().unwrap();
            session_guard.initialize(None, 0);
            session_guard.client.lock().unwrap().initialize(0x1000);
            session_guard.server.lock().unwrap().initialize(0x1000);
        }
        process.register_session_object(0x1000, Arc::clone(&session));

        let _current_thread = install_test_current_thread(1);
        let client_session = session.lock().unwrap().get_client_session().clone();
        assert_eq!(
            client_session
                .lock()
                .unwrap()
                .send_async_request_with_process(&mut process, 0x2222, 0x2395000, 0x80),
            0
        );

        let server_session = session.lock().unwrap().get_server_session().clone();
        assert_eq!(server_session.lock().unwrap().receive_request(), 0);
        let current_request = server_session
            .lock()
            .unwrap()
            .get_current_request()
            .expect("queued request");
        let current_request = current_request.lock().unwrap();
        assert_eq!(current_request.get_event_id(), Some(0x2222));
        assert_eq!(current_request.get_address(), 0x2395000);
        crate::hle::kernel::kernel::set_current_emu_thread(None);
    }

    #[test]
    fn enqueue_helper_uses_resolved_parent_without_locking_client_session() {
        let mut process = KProcess::new();
        let session = Arc::new(Mutex::new(KSession::new()));
        {
            let mut session_guard = session.lock().unwrap();
            session_guard.initialize(None, 0);
            session_guard.client.lock().unwrap().initialize(0x1000);
            session_guard.server.lock().unwrap().initialize(0x1000);
        }
        process.register_session_object(0x1000, Arc::clone(&session));

        let current_thread = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut thread = current_thread.lock().unwrap();
            thread.thread_id = 2;
            thread.set_state(ThreadState::RUNNABLE);
        }
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&current_thread));

        let client_session = session.lock().unwrap().get_client_session().clone();
        let _client_guard = client_session.lock().unwrap();

        assert_eq!(
            KClientSession::send_sync_request_to_parent_with_process(
                &mut process,
                Arc::clone(&session),
                0x2395100,
                0x40,
            ),
            0
        );
        crate::hle::kernel::kernel::set_current_emu_thread(None);

        let server_session = session.lock().unwrap().get_server_session().clone();
        assert_eq!(server_session.lock().unwrap().receive_request(), 0);
        let current_request = server_session
            .lock()
            .unwrap()
            .get_current_request()
            .expect("queued request");
        let current_request = current_request.lock().unwrap();
        assert_eq!(current_request.get_address(), 0x2395100);
        assert_eq!(current_request.get_size(), 0x40);
    }
}
