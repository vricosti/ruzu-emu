//! Port of zuyu/src/core/hle/kernel/k_port.h / k_port.cpp
//! Status: Ported (structural parity with upstream)
//! Derniere synchro: 2026-03-19
//!
//! KPort: a kernel port object containing server and client endpoints.
//!
//! Upstream `KPort` owns embedded `KServerPort m_server` and `KClientPort m_client`
//! members. Methods delegate to these sub-objects. State transitions are guarded
//! by KScopedSchedulerLock (stubbed here until scheduler integration).

use super::k_client_port::KClientPort;
use super::k_server_port::KServerPort;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};

/// Port state.
/// Matches upstream `KPort::State` (k_port.h).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortState {
    Invalid = 0,
    Normal = 1,
    ClientClosed = 2,
    ServerClosed = 3,
}

impl Default for PortState {
    fn default() -> Self {
        Self::Invalid
    }
}

/// ResultPortClosed — returned when enqueueing on a closed port.
/// Matches upstream `Svc::ResultPortClosed`.
pub const RESULT_PORT_CLOSED: ResultCode =
    ResultCode::from_module_description(crate::hle::result::ErrorModule::Kernel, 132);

/// The kernel port object.
///
/// Matches upstream `KPort` class (k_port.h):
/// ```cpp
/// class KPort final : public KAutoObjectWithSlabHeapAndContainer<KPort, KAutoObjectWithList> {
///     KServerPort m_server;
///     KClientPort m_client;
///     uintptr_t m_name;
///     State m_state{State::Invalid};
///     bool m_is_light{};
/// };
/// ```
pub struct KPort {
    /// The server endpoint of this port.
    /// Upstream: `KServerPort m_server`.
    pub server: KServerPort,
    /// The client endpoint of this port.
    /// Upstream: `KClientPort m_client`.
    pub client: KClientPort,
    /// Port name (opaque tag, often 0 for service ports).
    /// Upstream: `uintptr_t m_name`.
    pub name: usize,
    /// Current state.
    /// Upstream: `State m_state{State::Invalid}`.
    pub state: PortState,
    /// Whether this is a light port.
    /// Upstream: `bool m_is_light{}`.
    pub is_light: bool,
}

impl KPort {
    pub fn new() -> Self {
        Self {
            server: KServerPort::new(),
            client: KClientPort::new(),
            name: 0,
            state: PortState::Invalid,
            is_light: false,
        }
    }

    /// Initialize the port.
    ///
    /// Matches upstream `KPort::Initialize(s32 max_sessions, bool is_light, uintptr_t name)`:
    /// ```cpp
    /// void KPort::Initialize(s32 max_sessions, bool is_light, uintptr_t name) {
    ///     this->Open();
    ///     KAutoObject::Create(std::addressof(m_server));
    ///     KAutoObject::Create(std::addressof(m_client));
    ///     m_server.Initialize(this);
    ///     m_client.Initialize(this, max_sessions);
    ///     m_is_light = is_light;
    ///     m_name = name;
    ///     m_state = State::Normal;
    /// }
    /// ```
    pub fn initialize(&mut self, max_sessions: i32, is_light: bool, name: usize) {
        // In upstream, server/client Initialize takes a pointer to `this` (the parent KPort).
        // We don't store back-pointers yet — the parent relationship is implicit
        // since KPort owns both endpoints.
        self.server.initialize();
        self.client.initialize(max_sessions);

        self.is_light = is_light;
        self.name = name;
        self.state = PortState::Normal;
    }

    pub fn get_name(&self) -> usize {
        self.name
    }

    pub fn is_light(&self) -> bool {
        self.is_light
    }

    /// Check if the server side is closed.
    ///
    /// Matches upstream `KPort::IsServerClosed()`.
    /// Upstream uses KScopedSchedulerLock; we omit it for now.
    pub fn is_server_closed(&self) -> bool {
        self.state == PortState::ServerClosed
    }

    /// Access the server port.
    /// Matches upstream `KPort::GetServerPort()`.
    pub fn get_server_port(&self) -> &KServerPort {
        &self.server
    }

    /// Access the server port mutably.
    pub fn get_server_port_mut(&mut self) -> &mut KServerPort {
        &mut self.server
    }

    /// Access the client port.
    /// Matches upstream `KPort::GetClientPort()`.
    pub fn get_client_port(&self) -> &KClientPort {
        &self.client
    }

    /// Access the client port mutably.
    pub fn get_client_port_mut(&mut self) -> &mut KClientPort {
        &mut self.client
    }

    /// Called when the client side is closed.
    ///
    /// Matches upstream `KPort::OnClientClosed()`:
    /// ```cpp
    /// void KPort::OnClientClosed() {
    ///     KScopedSchedulerLock sl{m_kernel};
    ///     if (m_state == State::Normal) { m_state = State::ClientClosed; }
    /// }
    /// ```
    pub fn on_client_closed(&mut self) {
        // Upstream: KScopedSchedulerLock sl{m_kernel};
        if self.state == PortState::Normal {
            self.state = PortState::ClientClosed;
        }
    }

    /// Called when the server side is closed.
    ///
    /// Matches upstream `KPort::OnServerClosed()`:
    /// ```cpp
    /// void KPort::OnServerClosed() {
    ///     KScopedSchedulerLock sl{m_kernel};
    ///     if (m_state == State::Normal) { m_state = State::ServerClosed; }
    /// }
    /// ```
    pub fn on_server_closed(&mut self) {
        // Upstream: KScopedSchedulerLock sl{m_kernel};
        if self.state == PortState::Normal {
            self.state = PortState::ServerClosed;
        }
    }

    /// Enqueue a session on the server port.
    ///
    /// Matches upstream `KPort::EnqueueSession(KServerSession*)`:
    /// ```cpp
    /// Result KPort::EnqueueSession(KServerSession* session) {
    ///     KScopedSchedulerLock sl{m_kernel};
    ///     R_UNLESS(m_state == State::Normal, ResultPortClosed);
    ///     m_server.EnqueueSession(session);
    ///     R_SUCCEED();
    /// }
    /// ```
    fn enqueue_session(&mut self, session_id: u64) -> ResultCode {
        // Upstream: KScopedSchedulerLock sl{m_kernel};
        if self.state != PortState::Normal {
            return RESULT_PORT_CLOSED;
        }
        self.server.enqueue_session(session_id);
        RESULT_SUCCESS
    }

    /// Enqueue a light session on the server port.
    ///
    /// Matches upstream `KPort::EnqueueSession(KLightServerSession*)`.
    fn enqueue_light_session(&mut self, session_id: u64) -> ResultCode {
        // Upstream: KScopedSchedulerLock sl{m_kernel};
        if self.state != PortState::Normal {
            return RESULT_PORT_CLOSED;
        }
        self.server.enqueue_light_session(session_id);
        RESULT_SUCCESS
    }

    /// Shared-owner counterpart of EnqueueSession. Eden's KPort has no wrapper
    /// mutex; drop Rust's mutex before scheduler unlock can switch a fiber.
    pub fn enqueue_session_arc(port: &std::sync::Arc<std::sync::Mutex<Self>>, session_id: u64) -> ResultCode {
        let mut port = port.lock().unwrap();
        let _scheduler_guard = super::kernel::scheduler_lock()
            .map(super::k_scheduler_lock::KScopedSchedulerLock::new);
        let result = port.enqueue_session(session_id);
        drop(port);
        result
    }

    /// The light-session overload has the same wrapper/scheduler lifetime.
    pub fn enqueue_light_session_arc(port: &std::sync::Arc<std::sync::Mutex<Self>>, session_id: u64) -> ResultCode {
        let mut port = port.lock().unwrap();
        let _scheduler_guard = super::kernel::scheduler_lock()
            .map(super::k_scheduler_lock::KScopedSchedulerLock::new);
        let result = port.enqueue_light_session(session_id);
        drop(port);
        result
    }
}

impl Default for KPort {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_port_state_values() {
        assert_eq!(PortState::Invalid as u8, 0);
        assert_eq!(PortState::Normal as u8, 1);
        assert_eq!(PortState::ClientClosed as u8, 2);
        assert_eq!(PortState::ServerClosed as u8, 3);
    }

    #[test]
    fn test_port_initialize() {
        let mut port = KPort::new();
        assert_eq!(port.state, PortState::Invalid);

        port.initialize(64, false, 0);
        assert_eq!(port.state, PortState::Normal);
        assert!(!port.is_light());
        assert_eq!(port.client.get_max_sessions(), 64);
    }

    #[test]
    fn test_port_lifecycle() {
        let mut port = KPort::new();
        port.initialize(64, false, 0);

        // Normal → can enqueue
        assert_eq!(port.enqueue_session(42), RESULT_SUCCESS);
        assert!(port.server.is_signaled());

        // Accept the session
        assert_eq!(port.server.accept_session(), Some(42));

        // Close server side
        port.on_server_closed();
        assert!(port.is_server_closed());
        assert_eq!(port.state, PortState::ServerClosed);

        // Can no longer enqueue
        assert_eq!(port.enqueue_session(99), RESULT_PORT_CLOSED);
    }

    #[test]
    fn test_port_client_closed() {
        let mut port = KPort::new();
        port.initialize(64, false, 0);

        port.on_client_closed();
        assert_eq!(port.state, PortState::ClientClosed);
        assert!(!port.is_server_closed());
    }
}
