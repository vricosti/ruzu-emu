//! Port of zuyu/src/core/hle/kernel/k_server_port.h / k_server_port.cpp
//! Status: Partial (structural port)
//! Derniere synchro: 2026-03-19
//!
//! KServerPort: the server endpoint of a port, accepts sessions.
//!
//! Upstream uses intrusive lists for session queues. We use VecDeque as a
//! simpler equivalent (same push-back/pop-front semantics).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::k_synchronization_object::SynchronizationObjectState;

/// The server port object.
///
/// Matches upstream `KServerPort` class (k_server_port.h):
/// ```cpp
/// class KServerPort final : public KSynchronizationObject {
///     using SessionList = boost::intrusive::list<KServerSession, ...>;
///     using LightSessionList = boost::intrusive::list<KLightServerSession, ...>;
///     SessionList m_session_list{};
///     LightSessionList m_light_session_list{};
///     KPort* m_parent{};
/// };
/// ```
pub struct KServerPort {
    /// Pending session list (KServerSession IDs).
    /// Upstream: `SessionList m_session_list`.
    pub session_list: VecDeque<u64>,
    /// Pending light session list (KLightServerSession IDs).
    /// Upstream: `LightSessionList m_light_session_list`.
    pub light_session_list: VecDeque<u64>,
    /// Waiter state inherited from upstream `KSynchronizationObject`.
    pub sync_object: SynchronizationObjectState,
    // Note: upstream has `KPort* m_parent` for back-navigation.
    // Omitted because KPort owns KServerPort inline.
}

impl KServerPort {
    pub fn new() -> Self {
        Self {
            session_list: VecDeque::new(),
            light_session_list: VecDeque::new(),
            sync_object: SynchronizationObjectState::new(),
        }
    }

    /// Initialize the server port.
    ///
    /// Matches upstream `KServerPort::Initialize(KPort* parent)`.
    /// We omit the parent pointer since KPort owns us inline.
    pub fn initialize(&mut self) {
        // Nothing to do beyond construction — lists are already empty.
        self.sync_object = SynchronizationObjectState::new();
    }

    /// Enqueue a session.
    ///
    /// Matches upstream `KServerPort::EnqueueSession(KServerSession*)`.
    pub fn enqueue_session(&mut self, session_id: u64) {
        let _scheduler_guard = super::kernel::scheduler_lock()
            .map(super::k_scheduler_lock::KScopedSchedulerLock::new);
        self.session_list.push_back(session_id);
        if self.session_list.len() == 1 {
            // KServerPort has no numeric object-ID member in the Rust port.
            // Wait selection uses native state identity, not this trace ID.
            unsafe { super::k_synchronization_object::notify_waiters_on_state(
                &self.sync_object, 0, crate::hle::result::RESULT_SUCCESS.get_inner_value()); }
        }
    }

    /// Enqueue a light session.
    ///
    /// Matches upstream `KServerPort::EnqueueSession(KLightServerSession*)`.
    pub fn enqueue_light_session(&mut self, session_id: u64) {
        let _scheduler_guard = super::kernel::scheduler_lock()
            .map(super::k_scheduler_lock::KScopedSchedulerLock::new);
        self.light_session_list.push_back(session_id);
        if self.light_session_list.len() == 1 {
            unsafe { super::k_synchronization_object::notify_waiters_on_state(
                &self.sync_object, 0, crate::hle::result::RESULT_SUCCESS.get_inner_value()); }
        }
    }

    /// Accept a session (dequeue).
    ///
    /// Matches upstream `KServerPort::AcceptSession()`.
    pub fn accept_session(&mut self) -> Option<u64> {
        let _scheduler_guard = super::kernel::scheduler_lock()
            .map(super::k_scheduler_lock::KScopedSchedulerLock::new);
        self.session_list.pop_front()
    }

    /// Accept a light session (dequeue).
    ///
    /// Matches upstream `KServerPort::AcceptLightSession()`.
    pub fn accept_light_session(&mut self) -> Option<u64> {
        let _scheduler_guard = super::kernel::scheduler_lock()
            .map(super::k_scheduler_lock::KScopedSchedulerLock::new);
        self.light_session_list.pop_front()
    }

    /// Release the enclosing Rust KPort mutex before scheduler unlock can
    /// switch fibers. The queue operation remains owned by KServerPort.
    pub fn accept_session_arc(port: &Arc<Mutex<super::k_port::KPort>>) -> Option<u64> {
        let mut port = port.lock().unwrap();
        let _scheduler_guard = super::kernel::scheduler_lock()
            .map(super::k_scheduler_lock::KScopedSchedulerLock::new);
        let session = port.server.accept_session();
        drop(port);
        session
    }

    pub fn accept_light_session_arc(port: &Arc<Mutex<super::k_port::KPort>>) -> Option<u64> {
        let mut port = port.lock().unwrap();
        let _scheduler_guard = super::kernel::scheduler_lock()
            .map(super::k_scheduler_lock::KScopedSchedulerLock::new);
        let session = port.server.accept_light_session();
        drop(port);
        session
    }

    /// Is the server port signaled (has pending sessions)?
    ///
    /// Matches upstream `KServerPort::IsSignaled()`.
    pub fn is_signaled(&self) -> bool {
        !self.session_list.is_empty() || !self.light_session_list.is_empty()
    }

    /// Clean up sessions.
    ///
    /// Matches upstream `KServerPort::CleanupSessions()`.
    /// Upstream iterates and closes each session under KScopedSchedulerLock.
    pub fn cleanup_sessions(&mut self) {
        self.session_list.clear();
        self.light_session_list.clear();
    }

    /// Destroy the server port.
    ///
    /// Matches upstream `KServerPort::Destroy()`.
    pub fn destroy(&mut self) {
        // Upstream: calls OnServerClosed on parent, then CleanupSessions.
        self.cleanup_sessions();
    }
}

impl Default for KServerPort {
    fn default() -> Self {
        Self::new()
    }
}
