// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/server_manager.h and server_manager.cpp
//!
//! Contains:
//! - ServerManager: manages server ports and sessions for HLE services
//! - Session: wrapper pairing a KServerSession with a SessionRequestManager
//!
//! Upstream uses MultiWait/MultiWaitHolder for the event loop.
//! The Rust port still approximates that structure, but now blocks the guest
//! service thread on a kernel-readable wakeup event instead of keeping it
//! runnable in a round-robin yield loop.
//!
//! IPC dispatch follows upstream's `KClientSession::SendSyncRequest` →
//! `KServerSession::OnRequest` → `ServerManager::OnSessionEvent` flow.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::core::SystemRef;
use crate::hle::kernel::k_event::KEvent;
use crate::hle::kernel::k_port::KPort;
use crate::hle::kernel::k_process::ProcessLock;
use crate::hle::kernel::k_readable_event::KReadableEvent;
use crate::hle::kernel::k_server_session::KServerSession;
use crate::hle::kernel::svc::svc_results::RESULT_SESSION_CLOSED as KERNEL_RESULT_SESSION_CLOSED;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{
    self, HLERequestContext, PendingRegistrationQueue, SessionRequestHandlerFactory,
    SessionRequestHandlerPtr, SessionRequestManager,
};
use crate::hle::service::os::event::Event;
use crate::hle::service::os::multi_wait::MultiWait;
use crate::hle::service::os::multi_wait_holder::MultiWaitHolder;
use crate::hle::service::sm::sm::ServiceManager;

/// Tag for MultiWaitHolder user data, matching upstream `UserDataTag`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
enum UserDataTag {
    Port = 0,
    Session = 1,
    DeferEvent = 2,
}

/// Session wrapper pairing a KServerSession with its SessionRequestManager.
///
/// Matches upstream `Service::Session` (server_manager.cpp).
/// Upstream also stores an HLERequestContext for in-flight requests.
struct Session {
    id: u64,
    holder: Box<MultiWaitHolder>,
    server_session: Arc<Mutex<KServerSession>>,
    manager: Arc<Mutex<SessionRequestManager>>,
    /// Stored context for in-flight requests.
    /// Upstream: `HLERequestContext context` stored per-session.
    context: Option<HLERequestContext>,
}

#[derive(Default)]
struct LoopStats {
    wakeup_hits: u64,
    deferral_hits: u64,
    port_hits: u64,
    session_hits: u64,
    idle_timeouts: u64,
}

struct SharedSessionEvent {
    session_id: u64,
    server_session: Arc<Mutex<KServerSession>>,
    manager: Arc<Mutex<SessionRequestManager>>,
    service_manager: Option<Arc<Mutex<ServiceManager>>>,
    server_name: String,
}

enum SelectedSharedEvent {
    Session(SharedSessionEvent),
    Port(usize),
    Deferral,
}

fn ipc_phase_timer() -> Option<Instant> {
    std::env::var_os("RUZU_PROFILE_IPC_PHASES")
        .is_some()
        .then(Instant::now)
}

fn record_ipc_phase(label: &'static str, last: &mut Option<Instant>) {
    if let Some(start) = last {
        crate::hle::kernel::svc::svc_ipc::record_ipc_phase(label, start.elapsed());
        *last = Some(Instant::now());
    }
}

fn assert_receive_request_hle_result(server_name: &str, session_id: u64, result: u32) -> ! {
    panic!(
        "ServerManager({}): unexpected ReceiveRequestHLE result for session {}: {:#x}",
        server_name, session_id, result
    );
}

impl Session {
    fn new(
        id: u64,
        server_session: Arc<Mutex<KServerSession>>,
        manager: Arc<Mutex<SessionRequestManager>>,
    ) -> Self {
        let mut holder = Box::new(MultiWaitHolder::from_server_session(server_session.clone()));
        holder.set_user_data(UserDataTag::Session as usize);
        Self {
            id,
            holder,
            server_session,
            manager,
            context: None,
        }
    }

    fn holder_ptr(&self) -> *const MultiWaitHolder {
        &*self.holder as *const MultiWaitHolder
    }
}

/// Port wrapper pairing a waited server port with its handler factory.
///
/// Matches upstream `Service::Port` ownership in `server_manager.cpp`.
struct Port {
    holder: Box<MultiWaitHolder>,
    port: Arc<Mutex<KPort>>,
    server_port_object_id: Option<u64>,
    named_client_port_object_id: Option<u64>,
    registered_in_process: bool,
    handler_factory: SessionRequestHandlerFactory,
}

impl Port {
    fn new(
        port: Arc<Mutex<KPort>>,
        server_port_object_id: Option<u64>,
        named_client_port_object_id: Option<u64>,
        handler_factory: SessionRequestHandlerFactory,
    ) -> Self {
        let mut holder = Box::new(MultiWaitHolder::from_server_port(
            port.clone(),
            server_port_object_id,
        ));
        holder.set_user_data(UserDataTag::Port as usize);
        Self {
            holder,
            port,
            server_port_object_id,
            named_client_port_object_id,
            registered_in_process: false,
            handler_factory,
        }
    }

    fn holder_ptr(&self) -> *const MultiWaitHolder {
        &*self.holder as *const MultiWaitHolder
    }

    fn create_handler(&self) -> SessionRequestHandlerPtr {
        (self.handler_factory)()
    }
}

/// Manages server ports and sessions for HLE services.
///
/// Port of upstream `Service::ServerManager`.
/// Upstream uses MultiWait for the event loop (WaitSignaled → Process).
/// We implement the same pattern with our MultiWait/MultiWaitHolder.
pub struct ServerManager {
    /// Reference to the System, matching upstream `Core::System& m_system`.
    system: SystemRef,

    /// Service name for thread identification.
    name: String,

    /// Ensures only one host thread selects a `MultiWaitHolder` at a time.
    /// Upstream: `Mutex m_selection_mutex`.
    selection_mutex: Arc<Mutex<()>>,

    /// Active managed server ports.
    ports: Vec<Port>,

    /// Active sessions.
    sessions: Vec<Session>,

    /// Stable Rust equivalent of upstream `Session*` identity.
    next_session_id: u64,

    /// Wakeup event — signaled to wake the event loop when new items are linked.
    /// Upstream: `Kernel::KEvent* m_wakeup_event`.
    wakeup_event: Arc<Event>,

    /// Deferral event — signaled when deferred requests should be retried.
    /// Upstream: `Kernel::KEvent* m_deferral_event`.
    deferral_event: Option<Arc<Mutex<KEvent>>>,

    /// The main multi-wait for the event loop.
    /// Upstream: `MultiWait m_multi_wait`.
    multi_wait: MultiWait,

    /// Deferred list — items to be linked into multi_wait on next iteration.
    /// Upstream: `MultiWait m_deferred_list` + `std::mutex m_deferred_list_mutex`.
    deferred_list: Mutex<MultiWait>,

    /// Deferred sessions awaiting retry.
    /// Upstream: `std::list<Session*> m_deferred_sessions`.
    deferred_sessions: Vec<u64>,

    /// Wakeup holder in the multi-wait.
    /// Upstream: `std::optional<MultiWaitHolder> m_wakeup_holder`.
    wakeup_holder: Option<Box<MultiWaitHolder>>,

    /// Deferral holder in the multi-wait.
    /// Upstream: `std::optional<MultiWaitHolder> m_deferral_holder`.
    deferral_holder: Option<Box<MultiWaitHolder>>,

    /// Shared queue of pending session registrations. External handlers
    /// push to this Arc instead of locking `Mutex<ServerManager>` (which is
    /// held for the lifetime of `loop_process` by the host thread). The host
    /// thread drains the queue at the start of `wait_and_process_impl`.
    ///
    /// Forward-compatible workaround for sm.rs / sm_controller.rs /
    /// ipc_helpers.rs that previously called `register_session` directly on a
    /// locked ServerManager (deadlock). Once ServerManager's master mutex is
    /// fully refactored into per-field locks (upstream pattern), this queue
    /// can be removed in favor of direct calls.
    pending_registrations: PendingRegistrationQueue,
    pending_session_closures: Arc<Mutex<Vec<u64>>>,

    /// Stop flag. Upstream: `std::stop_source m_stop_source`.
    stop_requested: Arc<AtomicBool>,

    /// Whether the server has been stopped.
    stopped: AtomicBool,

    /// Whether `LoopProcess` has entered the event loop at least once.
    loop_started: AtomicBool,

    /// Additional host threads requested by upstream call sites such as
    /// `Sockets::LoopProcess`.
    ///
    /// Upstream: `std::vector<std::jthread> m_threads` is populated by
    /// `StartAdditionalHostThreads(...)`. Rust records the requested threads
    /// here first, then activates them after `loop_process_shared(...)` has
    /// prepared holder linkage on the final shared owner.
    pending_additional_host_threads: Vec<(String, usize)>,

    /// Owned handles for additional host threads.
    /// Upstream: `m_threads`.
    ///
    /// This has its own mutex because shutdown may need to join the host
    /// threads after guest CPU fibers have stopped while holding the enclosing
    /// `ServerManager` mutex.
    host_threads: Arc<Mutex<Vec<JoinHandle<()>>>>,

    /// Shared self-owner used where upstream passes `*this` into
    /// `SessionRequestManager(server_manager)`.
    self_reference: Option<Weak<Mutex<ServerManager>>>,

    /// Bounded local instrumentation for diagnosing service-thread spin loops.
    /// Disabled unless `RUZU_SM_SPIN_TRACE` is set.
    loop_stats: LoopStats,
}

impl ServerManager {
    /// Read the parent id without retaining the Rust endpoint mutex.
    ///
    /// Upstream passes a direct `KServerSession*` and has no equivalent host
    /// mutex. Keeping the guard alive while resolving the parent `KSession`
    /// would invert the close path's parent -> server order.
    fn server_session_parent_id(server_session: &Arc<Mutex<KServerSession>>) -> Option<u64> {
        server_session.lock().unwrap().get_parent_id()
    }

    fn trace_ipc(&self, stage: &str) {
        if std::env::var_os("RUZU_TRACE_SERVER_MANAGER_IPC").is_some() {
            eprintln!("[SERVER_MANAGER_IPC] manager={} stage={stage}", self.name);
        }
    }

    fn trace_ipc_counts(&self, stage: &str) {
        if std::env::var_os("RUZU_TRACE_SERVER_MANAGER_IPC").is_some() {
            let holders = self.multi_wait.holders_snapshot();
            let signaled = holders
                .iter()
                .filter(|holder| unsafe { (*(**holder)).is_signaled() })
                .count();
            if signaled == 0 {
                return;
            }
            eprintln!(
                "[SERVER_MANAGER_IPC] manager={} stage={stage} holders={} signaled={}",
                self.name,
                holders.len(),
                signaled
            );
        }
    }

    fn boot_trace_enabled(&self) -> bool {
        std::env::var_os("RUZU_APPLET_BOOT_TRACE")
            .is_some_and(|value| value != std::ffi::OsStr::new("0"))
    }

    fn current_process(&self) -> Option<Arc<ProcessLock>> {
        let Some(current_thread) = self.system.get().current_thread() else {
            return self.system.get().current_process_arc_opt();
        };
        let thread_guard = current_thread.lock().unwrap();
        thread_guard
            .parent
            .as_ref()
            .and_then(Weak::upgrade)
            .or_else(|| self.system.get().current_process_arc_opt())
    }

    fn signal_kernel_event(&self, event: &Arc<Mutex<KEvent>>) {
        let Some(process) = (!self.system.is_null())
            .then(|| self.current_process())
            .flatten()
        else {
            return;
        };
        KEvent::signal_arc(event, &process);
    }

    /// Creates a new ServerManager.
    /// Port of upstream `ServerManager::ServerManager(Core::System& system)`.
    fn new(system: SystemRef) -> Self {
        let wakeup_event = Arc::new(Event::new());

        let mut wakeup_holder = Box::new(MultiWaitHolder::from_event(wakeup_event.clone()));
        wakeup_holder.set_user_data(usize::MAX); // sentinel, not a real tag

        Self {
            system,
            name: String::new(),
            selection_mutex: Arc::new(Mutex::new(())),
            ports: Vec::new(),
            sessions: Vec::new(),
            next_session_id: 1,
            wakeup_event,
            deferral_event: None,
            multi_wait: MultiWait::new(),
            deferred_list: Mutex::new(MultiWait::new()),
            deferred_sessions: Vec::new(),
            wakeup_holder: Some(wakeup_holder),
            deferral_holder: None,
            stop_requested: Arc::new(AtomicBool::new(false)),
            stopped: AtomicBool::new(false),
            loop_started: AtomicBool::new(false),
            pending_registrations: Arc::new(Mutex::new(Vec::new())),
            pending_session_closures: Arc::new(Mutex::new(Vec::new())),
            pending_additional_host_threads: Vec::new(),
            host_threads: Arc::new(Mutex::new(Vec::new())),
            self_reference: None,
            loop_stats: LoopStats::default(),
        }
    }

    /// Creates a `ServerManager` in its final shared Rust owner before service
    /// registration.
    ///
    /// Upstream service `LoopProcess` functions construct a
    /// `std::unique_ptr<ServerManager>` before `RegisterNamedService` /
    /// `ManageNamedPort`, so callbacks can refer to a stable manager pointee
    /// while services are registered. This is the Rust counterpart for service
    /// loops; `new(...)` remains only the low-level constructor used before
    /// binding the shared owner.
    pub fn new_shared(system: SystemRef) -> Arc<Mutex<Self>> {
        let manager = Arc::new(Mutex::new(Self::new(system)));
        manager.lock().unwrap().bind_self_reference(&manager);
        manager
    }

    /// Return the system owner for service-dispatch helpers.
    ///
    /// Upstream `ServiceFrameworkBase` stores `Core::System&` directly; ruzu
    /// reaches the same owner through the `ServerManager` attached to the
    /// request context.
    pub fn system(&self) -> SystemRef {
        self.system
    }

    /// Returns a clone of the pending-registration queue Arc. Used by code
    /// that constructs a `SessionRequestManager` while it has direct access
    /// to `&self` (e.g., `on_port_event` inside the host thread).
    pub fn pending_registrations_arc(&self) -> PendingRegistrationQueue {
        Arc::clone(&self.pending_registrations)
    }

    /// Returns a clone of the wakeup_event Arc. Used for the same wiring as
    /// `pending_registrations_arc`.
    pub fn wakeup_event_arc(&self) -> Arc<Event> {
        Arc::clone(&self.wakeup_event)
    }

    pub fn stop_requested_arc(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop_requested)
    }

    pub fn host_threads_arc(&self) -> Arc<Mutex<Vec<JoinHandle<()>>>> {
        Arc::clone(&self.host_threads)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    fn service_owner_weak(&self) -> Weak<Mutex<ServerManager>> {
        self.self_reference.as_ref().cloned().unwrap_or_default()
    }

    /// Drain queued session registrations (called by host thread inside
    /// `wait_and_process_impl`). External handlers can push to the queue
    /// without locking `Mutex<ServerManager>`, then this drain runs them.
    fn drain_pending_registrations(&mut self) {
        let drained: Vec<_> = {
            let mut queue = self.pending_registrations.lock().unwrap();
            std::mem::take(&mut *queue)
        };
        if !drained.is_empty() {
            self.trace_ipc("drain_pending_registrations");
        }
        for (server_session, manager) in drained {
            let _ = self.register_session(server_session, manager);
        }
    }

    /// Consume endpoint-close notifications without taking the ServerManager
    /// mutex from the notifying thread. Upstream observes the same closures
    /// directly through `MultiWait`.
    fn drain_pending_session_closures(&mut self) {
        let closed_parent_ids = {
            let mut queue = self.pending_session_closures.lock().unwrap();
            std::mem::take(&mut *queue)
        };
        for parent_id in closed_parent_ids {
            if let Some(index) = self.sessions.iter().position(|session| {
                Self::server_session_parent_id(&session.server_session) == Some(parent_id)
            }) {
                self.destroy_session(index);
            }
        }
    }

    fn spin_trace_enabled(&self) -> bool {
        std::env::var_os("RUZU_SM_SPIN_TRACE").is_some()
    }

    fn log_loop_stats_if_needed(&self, reason: &str) {
        if !self.spin_trace_enabled() {
            return;
        }

        let total = self.loop_stats.wakeup_hits
            + self.loop_stats.deferral_hits
            + self.loop_stats.port_hits
            + self.loop_stats.session_hits
            + self.loop_stats.idle_timeouts;
        if total == 0 || total % 10 != 0 {
            return;
        }

        log::warn!(
            "ServerManager({}): spin stats reason={} wakeup={} deferral={} port={} session={} idle={}",
            self.name,
            reason,
            self.loop_stats.wakeup_hits,
            self.loop_stats.deferral_hits,
            self.loop_stats.port_hits,
            self.loop_stats.session_hits,
            self.loop_stats.idle_timeouts
        );
    }

    pub fn bind_self_reference(&mut self, manager: &Arc<Mutex<ServerManager>>) {
        self.self_reference = Some(Arc::downgrade(manager));
        self.rebuild_wait_holder_linkage_after_move();
    }

    /// Rebuild intrusive wait-list linkage after the manager has reached its
    /// final shared owner.
    ///
    /// Upstream constructs `ServerManager` behind a stable pointee, so
    /// `MultiWaitHolder` keeps valid `MultiWait*` backlinks for its lifetime.
    /// Rust moves `ServerManager` into `Arc<Mutex<_>>` in
    /// `KernelCore::run_server(...)`, which changes the address of
    /// `m_multi_wait` / `m_deferred_list` after some ports/deferral holders may
    /// already have been linked. Clear the stale backlinks and rebuild the
    /// current lists before the event loop starts.
    fn rebuild_wait_holder_linkage_after_move(&mut self) {
        self.multi_wait.holders.clear();
        let deferred_list = self.deferred_list.get_mut().unwrap();
        deferred_list.holders.clear();

        if let Some(holder) = self.wakeup_holder.as_deref_mut() {
            holder.reset_multi_wait_linkage_for_owner_move();
        }
        if let Some(holder) = self.deferral_holder.as_deref_mut() {
            holder.reset_multi_wait_linkage_for_owner_move();
            holder.link_to_multi_wait(deferred_list as *mut MultiWait);
        }
        for port in &mut self.ports {
            port.holder.reset_multi_wait_linkage_for_owner_move();
            port.holder
                .link_to_multi_wait(deferred_list as *mut MultiWait);
        }
        for session in &mut self.sessions {
            session.holder.reset_multi_wait_linkage_for_owner_move();
            session
                .holder
                .link_to_multi_wait(deferred_list as *mut MultiWait);
        }
    }

    /// Get the service manager from System.
    fn service_manager(&self) -> Option<Arc<Mutex<ServiceManager>>> {
        if self.system.is_null() {
            return None;
        }
        self.system.get().service_manager()
    }

    /// Registers a session with a manager.
    /// Port of upstream `ServerManager::RegisterSession`.
    pub fn register_session(
        &mut self,
        server_session: Arc<Mutex<KServerSession>>,
        manager: Arc<Mutex<SessionRequestManager>>,
    ) -> ResultCode {
        log::debug!("ServerManager({}): register_session", self.name);
        // Idempotent: skip if this server_session is already registered.
        // Drain callers (drain_pending_registrations) can push the same session
        // multiple times if `svc_ipc.rs`'s host-thread routing path enqueues
        // it on every IPC without checking whether it's already in
        // self.sessions. Without this guard, the session would appear N times
        // in `multi_wait` and `on_session_event` would be invoked N times for
        // a single IPC arrival.
        if self
            .sessions
            .iter()
            .any(|s| Arc::ptr_eq(&s.server_session, &server_session))
        {
            return RESULT_SUCCESS;
        }
        // Mirror the session into the host fiber's KProcess so the kernel-
        // backed `MultiWait::wait_any` can resolve its parent_id. Sessions are
        // typically created via `push_ipc_interface` /
        // `create_session_with_manager_object_id`, which registers the new
        // `KSession` in the GUEST process (the caller's process). When the
        // owning ServerManager's host fiber later calls `wait_any`, the
        // resolver looks the parent_id up in *its* process — the host service
        // KProcess — and gets `RESULT_INVALID_HANDLE` because the session is
        // not registered there. The result is that `wait_any` returns `None`,
        // the loop spins, and every other guest-service fiber sharing the
        // same guest core starves.
        //
        // Upstream's `MultiWait::WaitAny` works on native `KSession*`
        // pointers and is process-agnostic; the ruzu port resolves through
        // object ids per-process. The least-invasive bridge is to also
        // register the session in the current host fiber's KProcess so the
        // kernel-backed wait succeeds.
        if !self.system.is_null() {
            // Do not put the lock expression directly in the `if let`
            // scrutinee: Rust extends that temporary guard through the whole
            // body, producing server -> parent while CloseHandle uses parent
            // -> server.
            let parent_id = Self::server_session_parent_id(&server_session);
            if let Some(parent_id) = parent_id {
                if let Some(current_thread) = self.system.get().current_thread() {
                    let process = current_thread
                        .lock()
                        .unwrap()
                        .parent
                        .as_ref()
                        .and_then(|parent| parent.upgrade());
                    if let Some(process) = process {
                        let already_known = {
                            let process_guard = process.lock().unwrap();
                            process_guard
                                .get_server_session_by_object_id(parent_id)
                                .is_some()
                        };
                        if !already_known {
                            // We need the owning KSession Arc (not just the
                            // server-side). Find which process registered this
                            // session object id, then pull the KSession Arc out
                            // of that process so we can mirror it here.
                            let kernel_session = self.system.get().kernel().and_then(|kernel| {
                                kernel
                                    .get_session_owner_process_id(parent_id)
                                    .and_then(|owner_id| kernel.get_process_by_id(owner_id))
                                    .and_then(|owner_process| {
                                        owner_process
                                            .lock()
                                            .unwrap()
                                            .get_session_by_object_id(parent_id)
                                    })
                            });
                            if let Some(ksession) = kernel_session {
                                process
                                    .lock()
                                    .unwrap()
                                    .register_session_object(parent_id, ksession);
                            }
                        }
                    }
                }
            }
        }
        let session_id = self.next_session_id;
        self.next_session_id = self.next_session_id.wrapping_add(1).max(1);
        let mut session = Session::new(session_id, server_session, manager);
        session
            .server_session
            .lock()
            .unwrap()
            .set_manager_close_notification(
                Arc::downgrade(&self.pending_session_closures),
                Arc::downgrade(&self.wakeup_event),
            );
        session
            .server_session
            .lock()
            .unwrap()
            .set_manager_wakeup(Arc::downgrade(&self.wakeup_event));
        self.link_to_deferred_list_holder(&mut session.holder);
        self.sessions.push(session);
        RESULT_SUCCESS
    }

    /// Borrow the wakeup event as a Weak so that owners of `KServerSession`
    /// can wire reactive wakeup without an Arc clone leak.
    pub fn wakeup_event_weak(&self) -> std::sync::Weak<Event> {
        Arc::downgrade(&self.wakeup_event)
    }

    /// Registers a named service with the global ServiceManager.
    /// Port of upstream `ServerManager::RegisterNamedService`.
    pub fn register_named_service(
        &mut self,
        service_name: &str,
        handler_factory: SessionRequestHandlerFactory,
        max_sessions: u32,
    ) -> ResultCode {
        if self.name.is_empty() {
            self.name = service_name.to_string();
        }

        let sm = match self.service_manager() {
            Some(sm) => sm,
            None => return RESULT_SUCCESS,
        };

        let server_manager = self.service_owner_weak();
        let (port, deferral_event) = {
            let mut sm_guard = sm.lock().unwrap();
            let result = sm_guard.register_service_with_port(
                service_name.to_string(),
                max_sessions,
                handler_factory,
            );
            let deferral_event = sm_guard.deferral_event_clone();
            let port = match result {
                Ok(port) => port,
                Err(result) => {
                    log::warn!(
                        "ServerManager({}): failed to register '{}': {:#x}",
                        self.name,
                        service_name,
                        result.get_inner_value()
                    );
                    return result;
                }
            };
            if server_manager.upgrade().is_some() {
                sm_guard.set_service_ownership(
                    service_name,
                    crate::hle::service::sm::sm::ServiceOwnership {
                        queue: self.pending_registrations_arc(),
                        wakeup: self.wakeup_event_arc(),
                        server_manager,
                    },
                );
            }
            (port, deferral_event)
        };
        if let Some(event) = deferral_event {
            self.signal_kernel_event(&event);
        }

        let (client_port_object_id, server_port_object_id) = (!self.system.is_null())
            .then(|| self.system.get().kernel())
            .flatten()
            .map(|kernel| {
                let client_object_id = kernel.create_new_object_id() as u64;
                kernel.register_kernel_object(client_object_id);
                let object_id = kernel.create_new_object_id() as u64;
                kernel.register_kernel_object(object_id);
                (Some(client_object_id), Some(object_id))
            })
            .unwrap_or((None, None));
        if let Some(client_port_object_id) = client_port_object_id {
            sm.lock()
                .unwrap()
                .set_service_port_client_port_object_id(service_name, client_port_object_id);
        }

        let sm_for_handler = Arc::clone(&sm);
        let service_name_owned = service_name.to_string();
        let handler_factory: SessionRequestHandlerFactory = Box::new(move || {
            sm_for_handler
                .lock()
                .unwrap()
                .get_service(&service_name_owned)
                .expect("registered service must resolve to a handler")
        });
        let mut server = Port::new(port, server_port_object_id, None, handler_factory);
        self.link_to_deferred_list_holder(&mut server.holder);
        self.ports.push(server);

        RESULT_SUCCESS
    }

    /// Registers a named service with a shared handler instance.
    pub fn register_named_service_handler(
        &mut self,
        service_name: &str,
        handler: SessionRequestHandlerPtr,
        max_sessions: u32,
    ) -> ResultCode {
        let handler_clone = handler.clone();
        let factory: SessionRequestHandlerFactory = Box::new(move || handler_clone.clone());
        self.register_named_service(service_name, factory, max_sessions)
    }

    /// Manages a named port (standalone, not registered with SM).
    /// Port of upstream `ServerManager::ManageNamedPort`.
    pub fn manage_named_port(
        &mut self,
        port_name: &str,
        handler_factory: SessionRequestHandlerFactory,
        max_sessions: u32,
    ) -> ResultCode {
        let Some(kernel) = (!self.system.is_null())
            .then(|| self.system.get().kernel())
            .flatten()
        else {
            return RESULT_SUCCESS;
        };

        let port = Arc::new(Mutex::new(KPort::new()));
        port.lock()
            .unwrap()
            .initialize(max_sessions as i32, false, 0);

        let client_port_object_id = kernel.create_new_object_id() as u64;
        kernel.register_kernel_object(client_port_object_id);

        if let Some(gd) = kernel.object_name_global_data() {
            if gd
                .new_from_name(client_port_object_id as usize, port_name)
                .is_err()
            {
                kernel.unregister_kernel_object(client_port_object_id);
                return crate::hle::kernel::svc::svc_results::RESULT_INVALID_STATE;
            }
        }

        let server_port_object_id = kernel.create_new_object_id() as u64;
        kernel.register_kernel_object(server_port_object_id);

        let server_manager = self.service_owner_weak();
        if server_manager.upgrade().is_some() {
            if let Some(sm) = self.service_manager() {
                sm.lock().unwrap().set_service_ownership(
                    port_name,
                    crate::hle::service::sm::sm::ServiceOwnership {
                        queue: self.pending_registrations_arc(),
                        wakeup: self.wakeup_event_arc(),
                        server_manager,
                    },
                );
            }
        }

        let mut server = Port::new(
            port,
            Some(server_port_object_id),
            Some(client_port_object_id),
            handler_factory,
        );
        self.link_to_deferred_list_holder(&mut server.holder);
        self.ports.push(server);
        RESULT_SUCCESS
    }

    pub(crate) fn ensure_kernel_port_registrations(&mut self) {
        if self.system.is_null() {
            return;
        }

        let Some(process) = self.current_process() else {
            return;
        };

        self.ensure_kernel_port_registrations_for_process(process);
    }

    pub(crate) fn ensure_kernel_port_registrations_for_process(
        &mut self,
        process: Arc<ProcessLock>,
    ) {
        let mut process = process.lock().unwrap();
        for port in &mut self.ports {
            if port.registered_in_process {
                continue;
            }
            if let Some(server_port_object_id) = port.server_port_object_id {
                process.register_server_port_object(server_port_object_id, Arc::clone(&port.port));
            }
            if let Some(client_port_object_id) = port.named_client_port_object_id {
                process.register_client_port_object(client_port_object_id, Arc::clone(&port.port));
            }
            port.registered_in_process = true;
        }
    }

    /// Manages deferral events.
    /// Port of upstream `ServerManager::ManageDeferral(KEvent**)`.
    pub fn manage_deferral(&mut self) -> (ResultCode, Option<Arc<Mutex<KEvent>>>) {
        let Some(kernel) = (!self.system.is_null())
            .then(|| self.system.get().kernel())
            .flatten()
        else {
            return (RESULT_SUCCESS, None);
        };
        let Some(process) = self.current_process() else {
            return (RESULT_SUCCESS, None);
        };

        let event_object_id = kernel.create_new_object_id() as u64;
        let readable_event_object_id = kernel.create_new_object_id() as u64;

        let event = Arc::new(Mutex::new(KEvent::new()));
        event.lock().unwrap().initialize(
            process.lock().unwrap().get_process_id(),
            readable_event_object_id,
        );

        let readable_event = Arc::new(Mutex::new(KReadableEvent::new()));
        readable_event
            .lock()
            .unwrap()
            .initialize(event_object_id, readable_event_object_id);

        {
            let mut process = process.lock().unwrap();
            process.register_event_object(event_object_id, Arc::clone(&event));
            process.register_readable_event_object(
                readable_event_object_id,
                Arc::clone(&readable_event),
            );
        }
        kernel.register_kernel_object(event_object_id);
        kernel.register_kernel_object(readable_event_object_id);

        self.deferral_event = Some(Arc::clone(&event));

        let mut holder = Box::new(MultiWaitHolder::from_readable_event(readable_event));
        holder.set_user_data(UserDataTag::DeferEvent as usize);

        self.link_to_deferred_list_holder(&mut holder);
        self.deferral_holder = Some(holder);

        (RESULT_SUCCESS, Some(event))
    }

    /// Link a holder to the deferred list and signal the wakeup event.
    /// Port of upstream `ServerManager::LinkToDeferredList`.
    fn link_to_deferred_list_holder(&self, holder: &mut MultiWaitHolder) {
        let mut deferred_list = self.deferred_list.lock().unwrap();
        holder.link_to_multi_wait(&mut *deferred_list as *mut MultiWait);
        self.signal_wakeup_event();
    }

    /// Link a holder pointer to the deferred list and wake the event loop.
    ///
    /// This mirrors upstream `LinkToDeferredList(MultiWaitHolder*)` when the
    /// caller only has a stable holder pointer and cannot keep a Rust borrow of
    /// the owning `Session`/`Port` across the deferred-list lock.
    fn link_holder_ptr_to_deferred_list(&self, holder: *mut MultiWaitHolder) {
        let mut deferred_list = self.deferred_list.lock().unwrap();
        unsafe {
            (*holder).link_to_multi_wait(&mut *deferred_list as *mut MultiWait);
        }
        self.signal_wakeup_event();
    }

    /// Move all items from the deferred list to the main multi-wait.
    /// Port of upstream `ServerManager::LinkDeferred`.
    fn link_deferred(&mut self) {
        let mut deferred_list = self.deferred_list.lock().unwrap();
        self.multi_wait.move_all(&mut deferred_list);
        drop(deferred_list);
        self.trace_ipc_counts("link_deferred");
    }

    /// Runs `LoopProcess` from the shared owner without holding the
    /// `ServerManager` mutex for the lifetime of the event loop.
    pub fn loop_process_shared(manager: &Arc<Mutex<ServerManager>>) -> ResultCode {
        manager.lock().unwrap().prepare_loop_process();
        Self::activate_additional_host_threads(manager);
        let result = Self::loop_process_impl_shared(manager);
        manager.lock().unwrap().finish_loop_process();
        result
    }

    /// Port of upstream `ServerManager::LoopProcessImpl` for the shared Rust owner.
    ///
    /// Additional host threads call this directly, matching upstream
    /// `StartAdditionalHostThreads(... [&] { this->LoopProcessImpl(); })`.
    /// Only the main `LoopProcess` caller runs the prepare/finish wrapper.
    fn loop_process_impl_shared(manager: &Arc<Mutex<ServerManager>>) -> ResultCode {
        loop {
            let Some(selected_holder) = Self::wait_signaled_shared(manager) else {
                if manager
                    .lock()
                    .unwrap()
                    .stop_requested
                    .load(Ordering::Relaxed)
                {
                    return RESULT_SUCCESS;
                }
                continue;
            };
            let selected = manager
                .lock()
                .unwrap()
                .prepare_shared_event(selected_holder);
            Self::process_shared_event(manager, selected);
        }
    }

    fn process_shared_event(manager: &Arc<Mutex<ServerManager>>, selected: SelectedSharedEvent) {
        match selected {
            SelectedSharedEvent::Session(event) => {
                Self::process_session_event_shared(manager, event);
            }
            SelectedSharedEvent::Port(port_index) => {
                manager.lock().unwrap().on_port_event(port_index);
            }
            SelectedSharedEvent::Deferral => {
                Self::process_deferral_event_shared(manager);
            }
        }
    }

    /// Drive the real shared event loop without blocking.
    ///
    /// Unit-test systems do not start host fibers. Their SVC adapter calls
    /// this after enqueueing a request so tests still exercise the sole
    /// ServerManager dispatch transaction rather than a second inline
    /// implementation in `svc_ipc.rs`.
    #[cfg(test)]
    pub(crate) fn process_available_events_for_test(manager: &Arc<Mutex<ServerManager>>) -> usize {
        if !manager.lock().unwrap().loop_started.load(Ordering::Acquire) {
            manager.lock().unwrap().prepare_loop_process();
        }

        let mut processed = 0;
        loop {
            let selected = {
                let mut owner = manager.lock().unwrap();
                owner.ensure_kernel_port_registrations();
                owner.drain_pending_registrations();
                owner.drain_pending_session_closures();
                owner.link_deferred();

                let Some(selected) = owner.multi_wait.try_wait_any_local() else {
                    break;
                };
                unsafe {
                    (*selected).unlink_from_multi_wait();
                }
                if owner.is_wakeup_holder(selected) {
                    owner.consume_wakeup_holder(selected);
                    continue;
                }
                owner.prepare_shared_event(selected)
            };

            Self::process_shared_event(manager, selected);
            processed += 1;
        }
        processed
    }

    /// Shared-owner adaptation of upstream `ServerManager::WaitSignaled`.
    ///
    /// `m_selection_mutex` serializes selection, but upstream does not hold a
    /// mutex covering the whole `ServerManager` while `MultiWait::WaitAny`
    /// blocks. Request workers must remain able to send their reply and link
    /// the processed session to `m_deferred_list` during that wait.
    fn wait_signaled_shared(manager: &Arc<Mutex<ServerManager>>) -> Option<*mut MultiWaitHolder> {
        let selection_mutex = Arc::clone(&manager.lock().unwrap().selection_mutex);
        let _selection_guard = selection_mutex.lock().unwrap();

        loop {
            let (multi_wait, kernel, wakeup_event) = {
                let mut owner = manager.lock().unwrap();

                if std::env::var_os("RUZU_TRACE_SERVER_MANAGER_LOOP").is_some() {
                    let pending = owner.pending_registrations.lock().unwrap().len();
                    eprintln!(
                        "[SERVER_MANAGER_LOOP] manager={} stage=iter pending_q={} wakeup_signaled={}",
                        owner.name,
                        pending,
                        owner.wakeup_event.is_signaled()
                    );
                }

                owner.ensure_kernel_port_registrations();
                owner.drain_pending_registrations();
                owner.drain_pending_session_closures();
                owner.link_deferred();

                if owner.stop_requested.load(Ordering::Relaxed) {
                    return None;
                }

                let kernel = if owner.system.is_null() {
                    None
                } else {
                    owner
                        .system
                        .get()
                        .kernel()
                        .map(|kernel| kernel as *const crate::hle::kernel::kernel::KernelCore)
                };
                (
                    &owner.multi_wait as *const MultiWait,
                    kernel,
                    Arc::clone(&owner.wakeup_event),
                )
            };

            // The manager is stable behind Arc<Mutex<_>> and selection_mutex
            // prevents another waiter from mutating m_multi_wait. Other workers
            // only add holders to m_deferred_list while this call is blocked.
            let selected = if let Some(kernel) = kernel {
                unsafe { (&*multi_wait).wait_any(&*kernel) }
            } else {
                let selected = unsafe { (&*multi_wait).try_wait_any_local() };
                if selected.is_some() {
                    selected
                } else {
                    wakeup_event.wait_timeout(Duration::from_millis(100));
                    continue;
                }
            };
            let Some(selected) = selected else {
                continue;
            };

            let mut owner = manager.lock().unwrap();
            unsafe {
                (*selected).unlink_from_multi_wait();
            }
            if owner.is_wakeup_holder(selected) {
                owner.consume_wakeup_holder(selected);
                continue;
            }
            return Some(selected);
        }
    }

    fn prepare_loop_process(&mut self) {
        if self.spin_trace_enabled() && !self.system.is_null() {
            if let Some(kernel) = self.system.get().kernel() {
                log::warn!(
                    "ServerManager({}): is_guest_core={}",
                    self.name,
                    kernel.is_current_thread_guest_core()
                );
            }
        }
        self.ensure_kernel_event_bridge(&self.wakeup_event);
        self.ensure_kernel_port_registrations();

        // Link the permanent wakeup holder into `multi_wait` before entering
        // the loop. Upstream constructs the wakeup holder already bound to the
        // wait list; our split construction requires doing it here so that
        // `wait_any()` has at least one waitable even before any session or
        // port has been registered. Without this, service managers with no
        // sessions yet hit `holders.is_empty()` in `timed_wait_impl`, which
        // returns `None` immediately, and `loop_process` spins at 100% CPU
        // with zero voluntary context switches.
        if let Some(holder) = self.wakeup_holder.as_deref_mut() {
            holder.link_to_multi_wait(&mut self.multi_wait as *mut MultiWait);
        }

        log::info!("ServerManager({}): entering event loop", self.name);
        self.loop_started.store(true, Ordering::Release);
    }

    fn finish_loop_process(&mut self) {
        self.stopped.store(true, Ordering::Release);
        log::info!("ServerManager({}): event loop exited", self.name);
    }

    fn is_wakeup_holder(&self, selected: *mut MultiWaitHolder) -> bool {
        self.wakeup_holder
            .as_ref()
            .is_some_and(|holder| std::ptr::eq(&**holder as *const MultiWaitHolder, selected))
    }

    fn session_index_by_id(&self, session_id: u64) -> Option<usize> {
        self.sessions
            .iter()
            .position(|session| session.id == session_id)
    }

    /// Destroy a managed session.
    ///
    /// Port of upstream `ServerManager::DestroySession(Session*)`.
    fn destroy_session(&mut self, session_index: usize) {
        if session_index >= self.sessions.len() {
            return;
        }

        // `pending_session_closures` is a Rust-side bridge with no direct
        // upstream equivalent: it can reach this ownership boundary without
        // the holder first being selected by `WaitSignaled`. Eden's selected
        // session is already unlinked before `DestroySession`; preserve that
        // invariant here before freeing the boxed intrusive-list node.
        self.sessions[session_index].holder.unlink_from_multi_wait();

        let session = self.sessions.remove(session_index);
        let session_id = session.id;

        // Deleting upstream's Session wrapper closes its native
        // KServerSession. Rust's Arc does not run that kernel-object close, so
        // perform KServerSession::Destroy explicitly before dropping the
        // wrapper. This also finalizes the parent KSession once its client end
        // has closed and releases the parent KClientPort session slot.
        let parent_id = session.server_session.lock().unwrap().get_parent_id();
        if let Some(parent_id) = parent_id {
            let owner_process = (!self.system.is_null())
                .then(|| self.system.get().kernel())
                .flatten()
                .and_then(|kernel| kernel.get_session_owner_process_id(parent_id))
                .and_then(|process_id| {
                    self.system
                        .get()
                        .kernel()
                        .and_then(|kernel| kernel.get_process_by_id(process_id))
                });
            if let Some(owner_process) = owner_process {
                let mut process = owner_process.lock().unwrap();
                session
                    .server_session
                    .lock()
                    .unwrap()
                    .destroy_with_process(&mut process);
            } else {
                session.server_session.lock().unwrap().destroy();
            }
        } else {
            session.server_session.lock().unwrap().destroy();
        }

        // Upstream stores raw `Session*` entries in `m_deferred_sessions`; a
        // destroyed session pointer must no longer be retried. Rust stores
        // stable ids, so remove matching deferred ids at the ownership boundary.
        self.deferred_sessions.retain(|&id| id != session_id);
    }

    fn consume_wakeup_holder(&mut self, selected: *mut MultiWaitHolder) {
        self.loop_stats.wakeup_hits += 1;
        self.trace_ipc("wait_signaled_wakeup");
        self.log_loop_stats_if_needed("wakeup");
        self.wakeup_event.clear();
        unsafe {
            (*selected).link_to_multi_wait(&mut self.multi_wait as *mut MultiWait);
        }
    }

    fn prepare_shared_event(&mut self, selected: *mut MultiWaitHolder) -> SelectedSharedEvent {
        let selected_const = selected as *const MultiWaitHolder;
        let user_data = unsafe { (*selected_const).get_user_data() };

        match user_data {
            tag if tag == UserDataTag::Session as usize => {
                let Some(session_index) = self
                    .sessions
                    .iter()
                    .position(|session| std::ptr::eq(session.holder_ptr(), selected_const))
                else {
                    panic!(
                        "ServerManager({}): session holder was not registered",
                        self.name
                    );
                };
                self.loop_stats.session_hits += 1;
                self.log_loop_stats_if_needed("session");
                let session = &self.sessions[session_index];
                SelectedSharedEvent::Session(SharedSessionEvent {
                    session_id: session.id,
                    server_session: Arc::clone(&session.server_session),
                    manager: Arc::clone(&session.manager),
                    service_manager: self.service_manager(),
                    server_name: self.name.clone(),
                })
            }
            tag if tag == UserDataTag::Port as usize => {
                let Some(port_index) = self
                    .ports
                    .iter()
                    .position(|port| std::ptr::eq(port.holder_ptr(), selected_const))
                else {
                    panic!(
                        "ServerManager({}): port holder was not registered",
                        self.name
                    );
                };
                self.loop_stats.port_hits += 1;
                self.log_loop_stats_if_needed("port");
                SelectedSharedEvent::Port(port_index)
            }
            tag if tag == UserDataTag::DeferEvent as usize => {
                self.loop_stats.deferral_hits += 1;
                self.log_loop_stats_if_needed("deferral");
                SelectedSharedEvent::Deferral
            }
            _ => panic!(
                "ServerManager({}): unknown MultiWaitHolder user data {:#x}",
                self.name, user_data
            ),
        }
    }

    fn process_session_event_shared(
        manager_owner: &Arc<Mutex<ServerManager>>,
        event: SharedSessionEvent,
    ) -> bool {
        let mut phase_last = ipc_phase_timer();
        let result = event
            .server_session
            .lock()
            .unwrap()
            .receive_request_hle(Arc::clone(&event.manager));
        record_ipc_phase("server_03_receive_request_hle", &mut phase_last);

        let (context, _, _) = match result {
            Ok(result) => result,
            Err(result) => {
                if result == KERNEL_RESULT_SESSION_CLOSED.get_inner_value() {
                    let mut owner = manager_owner.lock().unwrap();
                    if let Some(session_index) = owner.session_index_by_id(event.session_id) {
                        owner.destroy_session(session_index);
                    }
                    return true;
                }

                log::warn!(
                    "ServerManager({}): session {} receive_request_hle failed (result={:#x})",
                    event.server_name,
                    event.session_id,
                    result
                );
                assert_receive_request_hle_result(&event.server_name, event.session_id, result);
            }
        };

        record_ipc_phase("server_04_store_context", &mut phase_last);
        Self::complete_sync_request_shared(manager_owner, event, context);
        record_ipc_phase("server_05_complete_sync_request", &mut phase_last);
        true
    }

    /// Port of upstream `ServerManager::OnDeferralEvent` for the shared Rust
    /// owner. Deferred contexts are detached while the owner is locked, then
    /// retried through the same `complete_sync_request_shared` transaction as
    /// newly received requests.
    fn process_deferral_event_shared(manager_owner: &Arc<Mutex<ServerManager>>) -> bool {
        // Eden clears the event before taking the deferred-session list. Keep
        // that ordering, but do not retain Rust's enclosing manager mutex while
        // KEvent::Clear enters the scheduler.
        let clear_target = {
            let owner = manager_owner.lock().unwrap();
            owner.deferral_event.as_ref().and_then(|event| {
                owner
                    .current_process()
                    .map(|process| (Arc::clone(event), process))
            })
        };
        if let Some((event, process)) = clear_target {
            KEvent::clear_arc(&event, &process);
        }

        let (pending, wakeup_after_relink) = {
            let mut owner = manager_owner.lock().unwrap();
            let deferred = std::mem::take(&mut owner.deferred_sessions);
            log::debug!(
                "ServerManager({}): retrying {} deferred sessions",
                owner.name,
                deferred.len()
            );

            let deferral_holder_ptr = owner
                .deferral_holder
                .as_deref_mut()
                .map(|holder| holder as *mut MultiWaitHolder);
            let mut wakeup_after_relink = None;
            if let Some(deferral_holder_ptr) = deferral_holder_ptr {
                {
                    let mut deferred_list = owner.deferred_list.lock().unwrap();
                    unsafe {
                        (*deferral_holder_ptr)
                            .link_to_multi_wait(&mut *deferred_list as *mut MultiWait);
                    }
                }
                wakeup_after_relink = Some(owner.wakeup_event_arc());
            }

            let mut pending = Vec::with_capacity(deferred.len());
            for session_id in deferred {
                let Some(session_index) = owner.session_index_by_id(session_id) else {
                    continue;
                };
                let Some(context) = owner.sessions[session_index].context.take() else {
                    log::warn!(
                        "ServerManager({}): deferred session {} missing HLE request context",
                        owner.name,
                        session_id
                    );
                    continue;
                };
                let session = &owner.sessions[session_index];
                pending.push((
                    SharedSessionEvent {
                        session_id,
                        server_session: Arc::clone(&session.server_session),
                        manager: Arc::clone(&session.manager),
                        service_manager: owner.service_manager(),
                        server_name: owner.name.clone(),
                    },
                    context,
                ));
            }
            (pending, wakeup_after_relink)
        };

        // `Event::signal` can wake kernel waiters and reschedule. Perform it
        // only after the global manager guard has been destroyed.
        if let Some(wakeup) = wakeup_after_relink {
            wakeup.signal();
        }

        for (event, context) in pending {
            Self::complete_sync_request_shared(manager_owner, event, context);
        }
        true
    }

    /// Complete one sync request for both initial and deferred dispatch.
    ///
    /// This is the sole Rust owner of upstream
    /// `ServerManager::CompleteSyncRequest`. The enclosing `ServerManager`
    /// mutex is deliberately not held while the service callback or
    /// `SendReplyHLE` runs.
    fn complete_sync_request_shared(
        manager_owner: &Arc<Mutex<ServerManager>>,
        event: SharedSessionEvent,
        mut context: HLERequestContext,
    ) -> bool {
        let mut phase_last = ipc_phase_timer();
        context.set_session_request_manager(Arc::clone(&event.manager));
        if let Some(sm) = event.service_manager {
            context.set_service_manager(sm);
        }
        context.set_is_deferred_value(false);
        record_ipc_phase("server_07_prepare_context", &mut phase_last);

        if std::env::var_os("RUZU_TRACE_HOST_THREAD_IPC").is_some() {
            let (svc, dom) = {
                let g = event.manager.lock().unwrap();
                let svc = g
                    .session_handler()
                    .map(|h| h.service_name().to_string())
                    .unwrap_or_else(|| "<none>".to_string());
                (svc, g.is_domain())
            };
            eprintln!(
                "[HOST_THREAD_IPC] dispatch manager={} service={} cmd={} dom={}",
                event.server_name,
                svc,
                context.get_command(),
                dom
            );
        }

        let service_profile =
            crate::hle::kernel::svc::svc_ipc::ipc_service_profile_enabled().then(Instant::now);
        let service_profile_key = service_profile.as_ref().map(|_| {
            let manager = event.manager.lock().unwrap();
            let service_name = manager
                .session_handler()
                .map(|handler| handler.service_name().to_string())
                .unwrap_or_else(|| "<none>".to_string());
            (service_name, context.get_command())
        });
        let service_result = hle_ipc::complete_sync_request(&event.manager, &mut context);
        if let (Some(start), Some((service_name, command))) = (service_profile, service_profile_key)
        {
            crate::hle::kernel::svc::svc_ipc::record_ipc_service_profile(
                &service_name,
                command,
                start.elapsed(),
            );
        }
        record_ipc_phase("server_08_hle_complete_sync_request", &mut phase_last);

        if context.get_is_deferred() {
            log::debug!(
                "ServerManager({}): session {} deferred",
                event.server_name,
                event.session_id
            );
            let mut owner = manager_owner.lock().unwrap();
            if let Some(session_index) = owner.session_index_by_id(event.session_id) {
                owner.sessions[session_index].context = Some(context);
                owner.deferred_sessions.push(event.session_id);
            }
            record_ipc_phase("server_09_deferred", &mut phase_last);
            return true;
        }

        let reply_result =
            crate::hle::kernel::k_server_session::KServerSession::send_reply_hle_unlocked(
                &event.server_session,
            );
        record_ipc_phase("server_10_send_reply_hle", &mut phase_last);

        let wakeup_after_relink = {
            let mut owner = manager_owner.lock().unwrap();
            let Some(session_index) = owner.session_index_by_id(event.session_id) else {
                return true;
            };

            if reply_result == KERNEL_RESULT_SESSION_CLOSED.get_inner_value()
                || service_result == crate::hle::service::ipc_helpers::RESULT_SESSION_CLOSED
            {
                log::debug!(
                    "ServerManager({}): session {} closed after dispatch",
                    owner.name,
                    event.session_id
                );
                owner.destroy_session(session_index);
                record_ipc_phase("server_11_session_closed", &mut phase_last);
                return true;
            }

            assert_eq!(
                reply_result,
                RESULT_SUCCESS.get_inner_value(),
                "ServerManager({}): unexpected SendReplyHLE result for session {}: {:#x}",
                owner.name,
                event.session_id,
                reply_result
            );
            assert_eq!(
                service_result,
                RESULT_SUCCESS,
                "ServerManager({}): unexpected service dispatch result for session {}: {:#x}",
                owner.name,
                event.session_id,
                service_result.get_inner_value()
            );

            log::trace!(
                "ServerManager({}): session {} completed (service_result={:#x}, reply_result={:#x})",
                owner.name,
                event.session_id,
                service_result.get_inner_value(),
                reply_result
            );

            let holder_ptr = {
                let holder = &mut *owner.sessions[session_index].holder as *mut MultiWaitHolder;
                unsafe {
                    if (*holder).is_linked() {
                        std::ptr::null_mut()
                    } else {
                        holder
                    }
                }
            };
            if holder_ptr.is_null() {
                None
            } else {
                {
                    let mut deferred_list = owner.deferred_list.lock().unwrap();
                    unsafe {
                        (*holder_ptr).link_to_multi_wait(&mut *deferred_list as *mut MultiWait);
                    }
                }
                Some(owner.wakeup_event_arc())
            }
        };

        // Relinking is manager-owned; waking the kernel waiters is not done
        // until the manager guard has been released.
        if let Some(wakeup) = wakeup_after_relink {
            wakeup.signal();
        }
        record_ipc_phase("server_12_relink_session", &mut phase_last);
        true
    }

    pub fn signal_wakeup_event(&self) {
        self.wakeup_event.signal();
    }

    fn ensure_kernel_event_bridge(&self, event: &Arc<Event>) {
        if event.kernel_object_id().is_some() || self.system.is_null() {
            return;
        }

        let Some(current_thread) = self.system.get().current_thread() else {
            if self.boot_trace_enabled() {
                log::info!(
                    "ServerManager({}): ensure_kernel_event_bridge skipped (no current_thread)",
                    self.name
                );
            }
            return;
        };
        let process = {
            let thread_guard = current_thread.lock().unwrap();
            let process = thread_guard
                .parent
                .as_ref()
                .and_then(|parent| parent.upgrade());
            let Some(process) = process else {
                if self.boot_trace_enabled() {
                    log::info!(
                        "ServerManager({}): ensure_kernel_event_bridge skipped (process missing)",
                        self.name
                    );
                }
                return;
            };
            process
        };

        let Some(kernel) = self.system.get().kernel() else {
            if self.boot_trace_enabled() {
                log::info!(
                    "ServerManager({}): ensure_kernel_event_bridge skipped (no kernel)",
                    self.name
                );
            }
            return;
        };

        let object_id = kernel.create_new_object_id() as u64;
        let mut readable_event = KReadableEvent::new();
        readable_event.initialize(0, object_id);
        let readable_event = Arc::new(Mutex::new(readable_event));

        process
            .lock()
            .unwrap()
            .register_readable_event_object(object_id, Arc::clone(&readable_event));
        kernel.register_kernel_object(object_id);
        event.attach_kernel_event(readable_event, process);
        if self.boot_trace_enabled() {
            log::info!(
                "ServerManager({}): attached kernel bridge object_id={}",
                self.name,
                object_id
            );
        }
    }

    /// Handle a server-port event (incoming connection).
    /// Port of upstream `ServerManager::OnPortEvent`.
    fn on_port_event(&mut self, port_index: usize) {
        if port_index >= self.ports.len() {
            return;
        }

        let server_session_object_id = {
            let mut port_guard = self.ports[port_index].port.lock().unwrap();
            let Some(server_session_object_id) = port_guard.server.accept_session() else {
                let holder_ptr = self.ports[port_index].holder_ptr() as *mut MultiWaitHolder;
                self.link_holder_ptr_to_deferred_list(holder_ptr);
                return;
            };
            server_session_object_id
        };

        let Some(kernel) = (!self.system.is_null())
            .then(|| self.system.get().kernel())
            .flatten()
        else {
            return;
        };
        let Some(owner_process_id) = kernel.get_session_owner_process_id(server_session_object_id)
        else {
            return;
        };
        let Some(process_arc) = kernel.get_process_by_id(owner_process_id) else {
            return;
        };

        let server_session = {
            let process = process_arc.lock().unwrap();
            match process.get_server_session_by_object_id(server_session_object_id) {
                Some(server_session) => server_session,
                None => return,
            }
        };

        let existing_manager = server_session.lock().unwrap().get_manager().cloned();
        let manager = if let Some(manager) = existing_manager {
            manager
        } else {
            let handler = self.ports[port_index].create_handler();
            // Use the *_full constructor and pass our own pending-registration
            // queue + wakeup_event Arcs directly. We're inside `&mut self` on
            // the host thread, so we can read these fields without locking
            // anything. This makes child SessionRequestManagers propagate the
            // queue down through push_ipc_interface / clone / sm::GetService.
            let queue = self.pending_registrations_arc();
            let wakeup = self.wakeup_event_arc();
            let manager = self
                .self_reference
                .as_ref()
                .and_then(Weak::upgrade)
                .map(|sm_arc| {
                    SessionRequestManager::new_with_server_manager_full(sm_arc, queue, wakeup)
                })
                .map(|manager| Arc::new(Mutex::new(manager)))
                .unwrap_or_else(|| Arc::new(Mutex::new(SessionRequestManager::new())));
            manager.lock().unwrap().set_session_handler(handler);
            server_session.lock().unwrap().set_manager(manager.clone());
            manager
        };
        let _ = self.register_session(server_session, manager);

        let holder_ptr = self.ports[port_index].holder_ptr() as *mut MultiWaitHolder;
        self.link_holder_ptr_to_deferred_list(holder_ptr);
    }

    /// Starts additional host threads for processing.
    /// Port of upstream `ServerManager::StartAdditionalHostThreads`.
    pub fn start_additional_host_threads(&mut self, name: &str, num_threads: usize) {
        log::debug!(
            "ServerManager({}): start_additional_host_threads({}, {})",
            self.name,
            name,
            num_threads
        );
        self.pending_additional_host_threads
            .push((name.to_string(), num_threads));
    }

    /// Activate pending additional host threads once the manager has a shared owner.
    ///
    /// Port of upstream `StartAdditionalHostThreads`: each requested thread
    /// runs the same manager's `LoopProcessImpl`.
    pub fn activate_additional_host_threads(manager: &Arc<Mutex<ServerManager>>) {
        let (system, pending, host_threads) = {
            let mut guard = manager.lock().unwrap();
            let pending = std::mem::take(&mut guard.pending_additional_host_threads);
            (guard.system, pending, Arc::clone(&guard.host_threads))
        };

        if system.is_null() {
            return;
        }

        let Some(kernel) = system.get().kernel() else {
            return;
        };

        for (name, num_threads) in pending {
            for i in 0..num_threads {
                let thread_name = format!("{}:{}", name, i + 1);
                let manager_for_thread = Arc::clone(manager);
                let handle = kernel.run_on_host_core_thread(
                    &thread_name,
                    Box::new(move || {
                        let _ = ServerManager::loop_process_impl_shared(&manager_for_thread);
                    }),
                );
                host_threads.lock().unwrap().push(handle);
            }
        }
    }

    /// Request the server to stop.
    pub fn request_stop(&self) {
        self.stop_requested.store(true, Ordering::Release);
        for handle in self.host_threads.lock().unwrap().iter() {
            handle.thread().unpark();
        }
        self.signal_wakeup_event();
    }

    pub fn loop_started(&self) -> bool {
        self.loop_started.load(Ordering::Acquire)
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    /// Join the additional host threads without retaining the manager lock.
    ///
    /// Upstream clears `m_threads` from the destructor without an enclosing
    /// manager mutex. In Rust, an additional thread must reacquire the manager
    /// mutex to observe `stop_requested` and leave `LoopProcessImpl`, so joining
    /// while holding that mutex would deadlock shutdown.
    pub fn join_host_threads(host_threads: &Arc<Mutex<Vec<JoinHandle<()>>>>, name: &str) {
        let host_threads = std::mem::take(&mut *host_threads.lock().unwrap());

        for handle in host_threads {
            if let Err(err) = handle.join() {
                log::warn!(
                    "ServerManager({}): additional host thread join failed: {:?}",
                    name,
                    err
                );
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn stop_requested_for_test(&self) -> bool {
        self.stop_requested.load(Ordering::Acquire)
    }

    /// Runs a manager that was already constructed in its final shared owner.
    ///
    /// This lets migrated service loops register services after
    /// `bind_self_reference(...)`, matching upstream's stable manager pointee
    /// during service registration.
    pub fn run_server_shared(manager: Arc<Mutex<ServerManager>>) {
        let system_ref = {
            let guard = manager.lock().unwrap();
            if guard.system.is_null() {
                log::warn!("ServerManager::run_server_shared called with null system");
                return;
            }
            guard.system
        };
        system_ref.get().run_server_shared(manager);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct TestSessionHandler;

    impl crate::hle::service::hle_ipc::SessionRequestHandler for TestSessionHandler {
        fn handle_sync_request(&self, _context: &mut HLERequestContext) -> ResultCode {
            RESULT_SUCCESS
        }
    }

    struct FailingSessionHandler;

    impl crate::hle::service::hle_ipc::SessionRequestHandler for FailingSessionHandler {
        fn handle_sync_request(&self, _context: &mut HLERequestContext) -> ResultCode {
            crate::hle::kernel::svc::svc_results::RESULT_INVALID_STATE
        }
    }

    struct ClearCurrentRequestHandler {
        server_session: Arc<Mutex<KServerSession>>,
    }

    impl crate::hle::service::hle_ipc::SessionRequestHandler for ClearCurrentRequestHandler {
        fn handle_sync_request(&self, _context: &mut HLERequestContext) -> ResultCode {
            self.server_session.lock().unwrap().current_request = None;
            RESULT_SUCCESS
        }
    }

    struct CloseClientSessionHandler {
        server_session: Arc<Mutex<KServerSession>>,
    }

    impl crate::hle::service::hle_ipc::SessionRequestHandler for CloseClientSessionHandler {
        fn handle_sync_request(&self, _context: &mut HLERequestContext) -> ResultCode {
            self.server_session.lock().unwrap().on_client_closed();
            RESULT_SUCCESS
        }
    }

    struct RecordingDispatchHandler {
        owner: Weak<Mutex<ServerManager>>,
        server_session: Arc<Mutex<KServerSession>>,
        calls: AtomicUsize,
        manager_was_unlocked: AtomicBool,
        request_addresses: Mutex<Vec<u64>>,
        defer_first: bool,
    }

    impl crate::hle::service::hle_ipc::SessionRequestHandler for RecordingDispatchHandler {
        fn handle_sync_request(&self, context: &mut HLERequestContext) -> ResultCode {
            let manager_is_unlocked = self
                .owner
                .upgrade()
                .is_some_and(|owner| owner.try_lock().is_ok());
            self.manager_was_unlocked
                .fetch_and(manager_is_unlocked, Ordering::Relaxed);
            assert!(
                self.server_session
                    .lock()
                    .unwrap()
                    .get_current_request()
                    .is_some(),
                "callback must run after ReceiveRequestHLE and before SendReplyHLE"
            );
            self.request_addresses
                .lock()
                .unwrap()
                .push(context.tls_address());

            let call = self.calls.fetch_add(1, Ordering::Relaxed);
            if self.defer_first && call == 0 {
                context.set_is_deferred();
            }
            RESULT_SUCCESS
        }
    }

    fn setup_recording_dispatch(
        defer_first: bool,
    ) -> (
        Arc<Mutex<ServerManager>>,
        Arc<Mutex<KServerSession>>,
        Arc<Mutex<SessionRequestManager>>,
        Arc<RecordingDispatchHandler>,
    ) {
        let owner = ServerManager::new_shared(SystemRef::null());
        let server_session = Arc::new(Mutex::new(KServerSession::new()));
        server_session.lock().unwrap().initialize(0x1000);
        let handler = Arc::new(RecordingDispatchHandler {
            owner: Arc::downgrade(&owner),
            server_session: Arc::clone(&server_session),
            calls: AtomicUsize::new(0),
            manager_was_unlocked: AtomicBool::new(true),
            request_addresses: Mutex::new(Vec::new()),
            defer_first,
        });
        let request_manager = Arc::new(Mutex::new(SessionRequestManager::new()));
        request_manager
            .lock()
            .unwrap()
            .set_session_handler(handler.clone());
        assert_eq!(
            owner
                .lock()
                .unwrap()
                .register_session(Arc::clone(&server_session), Arc::clone(&request_manager),),
            RESULT_SUCCESS
        );
        (owner, server_session, request_manager, handler)
    }

    fn enqueue_waiting_request(
        server_session: &Arc<Mutex<KServerSession>>,
        thread_id: u64,
        address: u64,
    ) -> Arc<crate::hle::kernel::k_thread::KThreadLock> {
        let client_thread = Arc::new(crate::hle::kernel::k_thread::KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = client_thread.lock().unwrap();
            thread.thread_id = thread_id;
            thread.begin_wait();
        }
        let mut request = crate::hle::kernel::k_session_request::KSessionRequest::new();
        request.thread = Some(Arc::downgrade(&client_thread));
        request.thread_id = Some(thread_id);
        request.address = address as usize;
        server_session
            .lock()
            .unwrap()
            .request_list
            .push_back(Arc::new(Mutex::new(request)));
        client_thread
    }

    fn first_shared_session_event(owner: &Arc<Mutex<ServerManager>>) -> SharedSessionEvent {
        let owner = owner.lock().unwrap();
        let session = &owner.sessions[0];
        SharedSessionEvent {
            session_id: session.id,
            server_session: Arc::clone(&session.server_session),
            manager: Arc::clone(&session.manager),
            service_manager: None,
            server_name: "test".to_string(),
        }
    }

    fn unlink_first_session_holder(owner: &Arc<Mutex<ServerManager>>) {
        owner.lock().unwrap().sessions[0]
            .holder
            .unlink_from_multi_wait();
    }

    #[test]
    fn initial_dispatch_replies_then_relinks_without_manager_locking_callback() {
        let (owner, server_session, _request_manager, handler) = setup_recording_dispatch(false);
        let client_thread = enqueue_waiting_request(&server_session, 1, 0x1000);
        unlink_first_session_holder(&owner);

        assert!(ServerManager::process_session_event_shared(
            &owner,
            first_shared_session_event(&owner),
        ));

        assert_eq!(handler.calls.load(Ordering::Relaxed), 1);
        assert!(handler.manager_was_unlocked.load(Ordering::Relaxed));
        assert!(server_session
            .lock()
            .unwrap()
            .get_current_request()
            .is_none());
        assert_eq!(
            client_thread.lock().unwrap().get_wait_result(),
            RESULT_SUCCESS.get_inner_value()
        );
        let owner = owner.lock().unwrap();
        assert!(owner.sessions[0].holder.is_linked());
        assert_eq!(owner.deferred_list.lock().unwrap().holders.len(), 1);
    }

    #[test]
    fn deferred_dispatch_reuses_the_same_transaction_and_replies_on_retry() {
        let (owner, server_session, _request_manager, handler) = setup_recording_dispatch(true);
        let client_thread = enqueue_waiting_request(&server_session, 1, 0x2000);
        unlink_first_session_holder(&owner);

        assert!(ServerManager::process_session_event_shared(
            &owner,
            first_shared_session_event(&owner),
        ));
        {
            let owner = owner.lock().unwrap();
            assert_eq!(owner.deferred_sessions, vec![owner.sessions[0].id]);
            assert!(owner.sessions[0].context.is_some());
            assert!(!owner.sessions[0].holder.is_linked());
        }
        assert!(server_session
            .lock()
            .unwrap()
            .get_current_request()
            .is_some());

        assert!(ServerManager::process_deferral_event_shared(&owner));

        assert_eq!(handler.calls.load(Ordering::Relaxed), 2);
        assert!(handler.manager_was_unlocked.load(Ordering::Relaxed));
        assert_eq!(
            handler.request_addresses.lock().unwrap().as_slice(),
            &[0x2000, 0x2000]
        );
        assert!(server_session
            .lock()
            .unwrap()
            .get_current_request()
            .is_none());
        assert_eq!(
            client_thread.lock().unwrap().get_wait_result(),
            RESULT_SUCCESS.get_inner_value()
        );
        let owner = owner.lock().unwrap();
        assert!(owner.deferred_sessions.is_empty());
        assert!(owner.sessions[0].context.is_none());
        assert!(owner.sessions[0].holder.is_linked());
    }

    #[test]
    fn shared_session_dispatch_preserves_request_fifo() {
        let (owner, server_session, _request_manager, handler) = setup_recording_dispatch(false);
        let first_thread = enqueue_waiting_request(&server_session, 1, 0x3000);
        let second_thread = enqueue_waiting_request(&server_session, 2, 0x4000);

        for expected_calls in 1..=2 {
            unlink_first_session_holder(&owner);
            assert!(ServerManager::process_session_event_shared(
                &owner,
                first_shared_session_event(&owner),
            ));
            assert_eq!(handler.calls.load(Ordering::Relaxed), expected_calls);
        }

        assert_eq!(
            handler.request_addresses.lock().unwrap().as_slice(),
            &[0x3000, 0x4000]
        );
        assert_eq!(
            first_thread.lock().unwrap().get_wait_result(),
            RESULT_SUCCESS.get_inner_value()
        );
        assert_eq!(
            second_thread.lock().unwrap().get_wait_result(),
            RESULT_SUCCESS.get_inner_value()
        );
        assert!(server_session.lock().unwrap().request_list.is_empty());
    }

    #[test]
    fn request_stop_sets_flag_and_signals_wakeup_event() {
        let manager = ServerManager::new(SystemRef::null());

        assert!(!manager.stop_requested_for_test());
        assert!(!manager.wakeup_event.is_signaled());
        manager.request_stop();

        assert!(manager.stop_requested_for_test());
        assert!(manager.wakeup_event.is_signaled());
    }

    #[test]
    fn joining_additional_threads_does_not_hold_the_manager_lock() {
        let manager = ServerManager::new_shared(SystemRef::null());
        let host_threads = manager.lock().unwrap().host_threads_arc();
        let (start_tx, start_rx) = std::sync::mpsc::channel();
        let (worker_done_tx, worker_done_rx) = std::sync::mpsc::channel();
        let worker_manager = Arc::clone(&manager);
        let worker = std::thread::spawn(move || {
            start_rx.recv().unwrap();
            let _manager = worker_manager.lock().unwrap();
            worker_done_tx.send(()).unwrap();
        });
        host_threads.lock().unwrap().push(worker);

        let (join_done_tx, join_done_rx) = std::sync::mpsc::channel();
        let join_host_threads = Arc::clone(&host_threads);
        std::thread::spawn(move || {
            ServerManager::join_host_threads(&join_host_threads, "test");
            join_done_tx.send(()).unwrap();
        });

        start_tx.send(()).unwrap();
        worker_done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("worker could not reacquire the manager during join");
        join_done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("additional host thread join did not complete");
    }

    #[test]
    fn shared_wait_does_not_hold_the_manager_lock() {
        let manager = ServerManager::new_shared(SystemRef::null());
        manager.lock().unwrap().prepare_loop_process();

        let waiter_manager = Arc::clone(&manager);
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            ServerManager::wait_signaled_shared(&waiter_manager).is_none()
        });
        started_rx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(20));

        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let locker_manager = Arc::clone(&manager);
        std::thread::spawn(move || {
            let owner = locker_manager.lock().unwrap();
            owner.request_stop();
            locked_tx.send(()).unwrap();
        });

        locked_rx
            .recv_timeout(Duration::from_millis(50))
            .expect("shared WaitAny retained Mutex<ServerManager>");
        assert!(waiter.join().unwrap());
    }

    #[test]
    fn start_additional_host_threads_records_pending_threads() {
        let mut manager = ServerManager::new(SystemRef::null());

        manager.start_additional_host_threads("bsdsocket", 2);

        assert_eq!(
            manager.pending_additional_host_threads,
            vec![("bsdsocket".to_string(), 2)]
        );
    }

    #[test]
    fn server_manager_holders_use_upstream_process_tags() {
        let server_session = Arc::new(Mutex::new(KServerSession::new()));
        server_session.lock().unwrap().initialize(0x1000);
        let request_manager = Arc::new(Mutex::new(SessionRequestManager::new()));
        let session = Session::new(1, server_session, request_manager);
        assert_eq!(
            session.holder.get_user_data(),
            UserDataTag::Session as usize
        );

        let mut port = KPort::new();
        port.initialize(1, false, 0);
        let port = Port::new(
            Arc::new(Mutex::new(port)),
            Some(0x2000),
            Some(0x2001),
            Box::new(|| Arc::new(TestSessionHandler)),
        );
        assert_eq!(port.holder.get_user_data(), UserDataTag::Port as usize);
    }

    #[test]
    #[should_panic(expected = "unknown MultiWaitHolder user data")]
    fn prepare_shared_event_panics_on_unknown_wait_holder_tag() {
        let mut manager = ServerManager::new(SystemRef::null());
        let event = Arc::new(Event::new());
        let mut holder = MultiWaitHolder::from_event(event);
        holder.set_user_data(0xdead);

        manager.prepare_shared_event(&mut holder as *mut MultiWaitHolder);
    }

    #[test]
    #[should_panic(expected = "session holder was not registered")]
    fn prepare_shared_event_panics_on_unregistered_session_holder() {
        let mut manager = ServerManager::new(SystemRef::null());
        let event = Arc::new(Event::new());
        let mut holder = MultiWaitHolder::from_event(event);
        holder.set_user_data(UserDataTag::Session as usize);

        manager.prepare_shared_event(&mut holder as *mut MultiWaitHolder);
    }

    #[test]
    fn register_session_links_session_holder_and_wakeup() {
        let mut manager = ServerManager::new(SystemRef::null());
        let server_session = Arc::new(Mutex::new(KServerSession::new()));
        server_session.lock().unwrap().initialize(0x1000);
        let request_manager = Arc::new(Mutex::new(SessionRequestManager::new()));

        assert_eq!(
            manager.register_session(Arc::clone(&server_session), request_manager),
            RESULT_SUCCESS
        );

        assert_eq!(manager.sessions.len(), 1);
        assert!(manager.sessions[0].holder.is_linked());
        assert!(server_session.lock().unwrap().manager_wakeup.is_some());
    }

    #[test]
    fn session_close_returns_to_server_manager_for_destruction() {
        let mut manager = ServerManager::new(SystemRef::null());
        let server_session = Arc::new(Mutex::new(KServerSession::new()));
        server_session.lock().unwrap().initialize(0x1000);
        let request_manager = Arc::new(Mutex::new(SessionRequestManager::new()));

        assert_eq!(
            manager.register_session(Arc::clone(&server_session), request_manager),
            RESULT_SUCCESS
        );
        assert!(manager.sessions[0].holder.is_linked());
        assert_eq!(manager.deferred_list.lock().unwrap().holders.len(), 1);

        server_session.lock().unwrap().on_client_closed();
        assert!(manager.wakeup_event.is_signaled());
        assert_eq!(*manager.pending_session_closures.lock().unwrap(), [0x1000]);

        manager.drain_pending_session_closures();
        assert!(manager.sessions.is_empty());
        assert!(manager.deferred_list.lock().unwrap().holders.is_empty());
        assert!(manager.pending_session_closures.lock().unwrap().is_empty());
    }

    #[test]
    fn session_close_unlinks_holder_from_main_wait_before_destruction() {
        let mut manager = ServerManager::new(SystemRef::null());
        let server_session = Arc::new(Mutex::new(KServerSession::new()));
        server_session.lock().unwrap().initialize(0x1000);
        let request_manager = Arc::new(Mutex::new(SessionRequestManager::new()));

        assert_eq!(
            manager.register_session(Arc::clone(&server_session), request_manager),
            RESULT_SUCCESS
        );
        manager.link_deferred();
        assert_eq!(manager.multi_wait.holders.len(), 1);
        assert!(manager.deferred_list.lock().unwrap().holders.is_empty());

        server_session.lock().unwrap().on_client_closed();
        manager.drain_pending_session_closures();

        assert!(manager.sessions.is_empty());
        assert!(manager.multi_wait.holders.is_empty());
    }

    #[test]
    fn shared_dispatch_accepts_kernel_session_closed_reply() {
        let manager = Arc::new(Mutex::new(ServerManager::new(SystemRef::null())));
        let server_session = Arc::new(Mutex::new(KServerSession::new()));
        server_session.lock().unwrap().initialize(0x1000);
        let request_manager = Arc::new(Mutex::new(SessionRequestManager::new()));
        request_manager
            .lock()
            .unwrap()
            .set_session_handler(Arc::new(CloseClientSessionHandler {
                server_session: Arc::clone(&server_session),
            }));
        {
            let mut manager = manager.lock().unwrap();
            assert_eq!(
                manager
                    .register_session(Arc::clone(&server_session), Arc::clone(&request_manager),),
                RESULT_SUCCESS
            );
        }

        let client_thread = Arc::new(crate::hle::kernel::k_thread::KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        let mut request = crate::hle::kernel::k_session_request::KSessionRequest::new();
        request.thread = Some(Arc::downgrade(&client_thread));
        request.thread_id = Some(1);
        server_session
            .lock()
            .unwrap()
            .request_list
            .push_back(Arc::new(Mutex::new(request)));

        assert!(ServerManager::process_session_event_shared(
            &manager,
            SharedSessionEvent {
                session_id: 1,
                server_session,
                manager: request_manager,
                service_manager: None,
                server_name: "test".to_string(),
            },
        ));
        assert!(manager.lock().unwrap().sessions.is_empty());
    }

    #[test]
    fn reading_server_session_parent_id_releases_endpoint_lock() {
        let server_session = Arc::new(Mutex::new(KServerSession::new()));
        server_session.lock().unwrap().initialize(0x1000);

        assert_eq!(
            ServerManager::server_session_parent_id(&server_session),
            Some(0x1000)
        );
        assert!(server_session.try_lock().is_ok());
    }

    #[test]
    fn deferred_session_ids_survive_session_vector_removal() {
        let mut manager = ServerManager::new(SystemRef::null());
        for object_id in [0x1000, 0x1001] {
            let server_session = Arc::new(Mutex::new(KServerSession::new()));
            server_session.lock().unwrap().initialize(object_id);
            let request_manager = Arc::new(Mutex::new(SessionRequestManager::new()));
            assert_eq!(
                manager.register_session(server_session, request_manager),
                RESULT_SUCCESS
            );
        }

        let deferred_id = manager.sessions[1].id;
        manager.deferred_sessions.push(deferred_id);
        manager.destroy_session(0);

        assert_eq!(manager.session_index_by_id(deferred_id), Some(0));
        assert_eq!(manager.deferred_sessions, vec![deferred_id]);
    }

    #[test]
    fn destroy_session_removes_matching_deferred_id() {
        let mut manager = ServerManager::new(SystemRef::null());
        let server_session = Arc::new(Mutex::new(KServerSession::new()));
        server_session.lock().unwrap().initialize(0x1000);
        let request_manager = Arc::new(Mutex::new(SessionRequestManager::new()));
        assert_eq!(
            manager.register_session(server_session, request_manager),
            RESULT_SUCCESS
        );
        manager.wakeup_event.clear();

        let destroyed_id = manager.sessions[0].id;
        manager.deferred_sessions.push(destroyed_id);
        assert!(!manager.wakeup_event.is_signaled());
        manager.destroy_session(0);

        assert!(manager.sessions.is_empty());
        assert!(manager.deferred_sessions.is_empty());
        assert!(!manager.wakeup_event.is_signaled());
    }

    #[test]
    #[should_panic(expected = "unexpected service dispatch result")]
    fn complete_sync_request_asserts_unexpected_service_error() {
        let manager = Arc::new(Mutex::new(ServerManager::new(SystemRef::null())));
        let server_session = Arc::new(Mutex::new(KServerSession::new()));
        server_session.lock().unwrap().initialize(0x1000);
        let request_manager = Arc::new(Mutex::new(SessionRequestManager::new()));
        request_manager
            .lock()
            .unwrap()
            .set_session_handler(Arc::new(FailingSessionHandler));
        manager
            .lock()
            .unwrap()
            .register_session(Arc::clone(&server_session), Arc::clone(&request_manager));

        let client_thread = Arc::new(crate::hle::kernel::k_thread::KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        let mut request = crate::hle::kernel::k_session_request::KSessionRequest::new();
        request.thread = Some(Arc::downgrade(&client_thread));
        request.thread_id = Some(1);
        server_session.lock().unwrap().current_request = Some(Arc::new(Mutex::new(request)));

        ServerManager::complete_sync_request_shared(
            &manager,
            SharedSessionEvent {
                session_id: 1,
                server_session,
                manager: request_manager,
                service_manager: None,
                server_name: "test".to_string(),
            },
            HLERequestContext::new(),
        );
    }

    #[test]
    #[should_panic(expected = "unexpected ReceiveRequestHLE result")]
    fn shared_session_event_asserts_unexpected_receive_error() {
        let manager = Arc::new(Mutex::new(ServerManager::new(SystemRef::null())));
        let server_session = Arc::new(Mutex::new(KServerSession::new()));
        server_session.lock().unwrap().initialize(0x1000);
        let request_manager = Arc::new(Mutex::new(SessionRequestManager::new()));
        {
            let mut guard = manager.lock().unwrap();
            assert_eq!(
                guard.register_session(Arc::clone(&server_session), Arc::clone(&request_manager)),
                RESULT_SUCCESS
            );
        }

        ServerManager::process_session_event_shared(
            &manager,
            SharedSessionEvent {
                session_id: 1,
                server_session,
                manager: request_manager,
                service_manager: None,
                server_name: "test".to_string(),
            },
        );
    }

    #[test]
    #[should_panic(expected = "unexpected service dispatch result")]
    fn shared_session_event_asserts_unexpected_service_error() {
        let manager = Arc::new(Mutex::new(ServerManager::new(SystemRef::null())));
        let server_session = Arc::new(Mutex::new(KServerSession::new()));
        server_session.lock().unwrap().initialize(0x1000);
        let request_manager = Arc::new(Mutex::new(SessionRequestManager::new()));
        request_manager
            .lock()
            .unwrap()
            .set_session_handler(Arc::new(FailingSessionHandler));
        {
            let mut guard = manager.lock().unwrap();
            assert_eq!(
                guard.register_session(Arc::clone(&server_session), Arc::clone(&request_manager)),
                RESULT_SUCCESS
            );
        }

        let client_thread = Arc::new(crate::hle::kernel::k_thread::KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        let mut request = crate::hle::kernel::k_session_request::KSessionRequest::new();
        request.thread = Some(Arc::downgrade(&client_thread));
        request.thread_id = Some(1);
        {
            server_session
                .lock()
                .unwrap()
                .request_list
                .push_back(Arc::new(Mutex::new(request)));
        }

        ServerManager::process_session_event_shared(
            &manager,
            SharedSessionEvent {
                session_id: 1,
                server_session,
                manager: request_manager,
                service_manager: None,
                server_name: "test".to_string(),
            },
        );
    }

    #[test]
    #[should_panic(expected = "unexpected SendReplyHLE result")]
    fn shared_session_event_asserts_unexpected_reply_error() {
        let manager = Arc::new(Mutex::new(ServerManager::new(SystemRef::null())));
        let server_session = Arc::new(Mutex::new(KServerSession::new()));
        server_session.lock().unwrap().initialize(0x1000);
        let request_manager = Arc::new(Mutex::new(SessionRequestManager::new()));
        request_manager
            .lock()
            .unwrap()
            .set_session_handler(Arc::new(ClearCurrentRequestHandler {
                server_session: Arc::clone(&server_session),
            }));
        {
            let mut guard = manager.lock().unwrap();
            assert_eq!(
                guard.register_session(Arc::clone(&server_session), Arc::clone(&request_manager)),
                RESULT_SUCCESS
            );
        }

        let client_thread = Arc::new(crate::hle::kernel::k_thread::KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        let mut request = crate::hle::kernel::k_session_request::KSessionRequest::new();
        request.thread = Some(Arc::downgrade(&client_thread));
        request.thread_id = Some(1);
        {
            server_session
                .lock()
                .unwrap()
                .request_list
                .push_back(Arc::new(Mutex::new(request)));
        }

        ServerManager::process_session_event_shared(
            &manager,
            SharedSessionEvent {
                session_id: 1,
                server_session,
                manager: request_manager,
                service_manager: None,
                server_name: "test".to_string(),
            },
        );
    }

    #[test]
    fn wait_holder_rebuild_relinks_session_holder() {
        let mut manager = ServerManager::new(SystemRef::null());
        let server_session = Arc::new(Mutex::new(KServerSession::new()));
        server_session.lock().unwrap().initialize(0x1000);
        let request_manager = Arc::new(Mutex::new(SessionRequestManager::new()));

        assert_eq!(
            manager.register_session(Arc::clone(&server_session), request_manager),
            RESULT_SUCCESS
        );
        manager.rebuild_wait_holder_linkage_after_move();

        assert_eq!(manager.sessions.len(), 1);
        assert!(manager.sessions[0].holder.is_linked());
    }

    #[test]
    fn service_owner_weak_is_live_after_manager_is_bound() {
        let manager = Arc::new(Mutex::new(ServerManager::new(SystemRef::null())));
        manager.lock().unwrap().bind_self_reference(&manager);

        let owner = manager.lock().unwrap().service_owner_weak();

        assert!(owner.upgrade().is_some());
    }

    #[test]
    fn new_shared_binds_service_owner_before_registration() {
        let manager = ServerManager::new_shared(SystemRef::null());

        let owner = manager.lock().unwrap().service_owner_weak();

        let upgraded = owner
            .upgrade()
            .expect("new_shared should bind a live ServerManager owner");
        assert!(Arc::ptr_eq(&upgraded, &manager));
    }
}
