// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/hle_ipc.h and hle_ipc.cpp
//! Status: Structural port with stubs for kernel-dependent methods
//!
//! Contains:
//! - SessionRequestHandler: trait for HLE session handlers
//! - SessionRequestManager: manages domain state and handler dispatch
//! - HLERequestContext: in-flight IPC request context

use std::cell::RefCell;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::core::SystemRef;
use crate::hle::ipc;

// Temporary investigation: per-thread slot holding the service name + cmd of the
// current IPC dispatch, so the IPC_REPLY hex-dump in
// write_to_outgoing_command_buffer can attribute each reply to (service, cmd).
// The third element is an optional ioctl number (non-zero means nvdrv Ioctl1/2/3).
thread_local! {
    pub(crate) static IPC_TRACE_CURRENT: RefCell<(String, u32, u32)> =
        RefCell::new((String::new(), 0, 0));
}

fn ipc_trace_ring_limit() -> usize {
    static LIMIT: OnceLock<usize> = OnceLock::new();
    *LIMIT.get_or_init(|| {
        std::env::var("RUZU_TRACE_IPC_RING_LIMIT")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(4000)
    })
}
use crate::hle::kernel::k_readable_event::KReadableEvent;
use crate::hle::kernel::k_thread::KThreadLock;

/// Reference to a kernel object for IPC handle translation.
///
/// Upstream stores raw `KAutoObject*` in `outgoing_copy_objects` / `outgoing_move_objects`.
/// Handle creation is deferred to `WriteToOutgoingCommandBuffer()` where
/// `handle_table.Add(object)` translates objects to handles.
///
/// We store either:
/// - `ObjectId(u64)` — a kernel object ID needing `handle_table.Add()` translation
/// - `Handle(u32)` — a pre-resolved handle (for services that already have one)
#[derive(Clone, Copy, Debug)]
pub enum KAutoObjectRef {
    /// A kernel object ID that needs handle_table.Add() translation.
    ObjectId(u64),
    /// A pre-resolved handle value.
    Handle(u32),
}

impl KAutoObjectRef {
    /// Returns the pre-resolved handle value when this reference already owns
    /// a handle.
    pub fn as_handle(&self) -> Option<u32> {
        match self {
            KAutoObjectRef::ObjectId(_) => None,
            KAutoObjectRef::Handle(h) => Some(*h),
        }
    }
}
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::sm::sm::ServiceManager;

/// Handle type alias matching upstream `Kernel::Handle`.
pub type Handle = u32;

/// Trait implemented by HLE session handlers.
///
/// Corresponds to upstream `SessionRequestHandler`. This can be provided to a ServerSession
/// in order to hook into several relevant events (such as a new connection or a SyncRequest)
/// so they can be implemented in the emulator.
pub trait SessionRequestHandler: Send + Sync {
    /// Handles a sync request from the emulated application.
    fn handle_sync_request(&self, context: &mut HLERequestContext) -> ResultCode;

    /// Returns the service name, for logging.
    fn service_name(&self) -> &str {
        std::any::type_name::<Self>()
    }

    /// Downcast support for service-to-service direct handler access.
    /// Matches upstream `ServiceManager::GetService<T>()` which returns
    /// a typed `shared_ptr<T>` to the handler object.
    /// Default implementation panics — override for types that need downcasting.
    fn as_any(&self) -> &dyn std::any::Any {
        panic!("as_any() not implemented for {}", self.service_name())
    }
}

pub type SessionRequestHandlerPtr = Arc<dyn SessionRequestHandler>;
pub type SessionRequestHandlerWeakPtr = Weak<dyn SessionRequestHandler>;
pub type SessionRequestHandlerFactory = Box<dyn Fn() -> SessionRequestHandlerPtr + Send + Sync>;

/// One queued session-registration request. Pushed by external handlers and
/// drained by the owning `ServerManager`'s host thread at the start of each
/// `wait_and_process_impl` iteration. Lets `sm.rs` / `sm_controller.rs` /
/// `ipc_helpers.rs::push_ipc_interface` register new sessions without locking
/// `Mutex<ServerManager>` — which is held for the lifetime of `loop_process`
/// by the host thread itself, so external `lock()` would deadlock.
pub type PendingRegistration = (
    Arc<Mutex<crate::hle::kernel::k_server_session::KServerSession>>,
    Arc<Mutex<SessionRequestManager>>,
);

pub type PendingRegistrationQueue = Arc<Mutex<Vec<PendingRegistration>>>;

/// Manages the underlying HLE requests for a session, and whether (or not) the session should be
/// treated as a domain. This is managed separately from server sessions, as this state is shared
/// when objects are cloned.
///
/// Corresponds to upstream `SessionRequestManager`.
pub struct SessionRequestManager {
    convert_to_domain: bool,
    is_domain: bool,
    is_initialized_for_sm: bool,
    session_handler: Option<SessionRequestHandlerPtr>,
    domain_handlers: Vec<Option<SessionRequestHandlerPtr>>,
    /// Reference to the owning ServerManager.
    /// Matches upstream: `Service::ServerManager& server_manager`.
    server_manager: Option<Weak<Mutex<super::server_manager::ServerManager>>>,
    /// Cloned Arc to the ServerManager's pending-registration queue. Lets
    /// handlers register new sessions without locking `Mutex<ServerManager>`
    /// (which would deadlock — the host thread holds it for `loop_process`'s
    /// lifetime). Cascades through `push_ipc_interface` / `clone_current_object`
    /// so child SessionRequestManagers see the same queue as their parent.
    pending_registrations: Option<PendingRegistrationQueue>,
    /// Cloned Arc to the ServerManager's wakeup_event. Signaled after
    /// pushing to `pending_registrations` so the host thread reacts in µs
    /// instead of its 100 ms idle timeout.
    server_wakeup: Option<Arc<super::os::event::Event>>,
}

#[derive(Clone)]
enum PreparedSyncRequest {
    Session(SessionRequestHandlerPtr),
    Domain(SessionRequestHandlerPtr),
    CloseVirtualHandle(usize),
    StubSuccess,
}

impl SessionRequestManager {
    pub fn new() -> Self {
        Self {
            convert_to_domain: false,
            is_domain: false,
            is_initialized_for_sm: false,
            session_handler: None,
            domain_handlers: Vec::new(),
            server_manager: None,
            pending_registrations: None,
            server_wakeup: None,
        }
    }

    /// Create with a ServerManager reference, matching upstream constructor:
    /// `SessionRequestManager(KernelCore& kernel, ServerManager& server_manager)`.
    ///
    /// This variant does NOT attempt to extract the pending-registration queue
    /// from `server_manager` — that would require locking `Mutex<ServerManager>`,
    /// which deadlocks if the host thread is in `loop_process`. Callers that
    /// also have access to the parent SessionRequestManager (the common path
    /// for nested IPC interfaces) should prefer `new_with_server_manager_full`
    /// which propagates the queue and wakeup from the parent.
    pub fn new_with_server_manager(
        server_manager: Arc<Mutex<super::server_manager::ServerManager>>,
    ) -> Self {
        Self {
            convert_to_domain: false,
            is_domain: false,
            is_initialized_for_sm: false,
            session_handler: None,
            domain_handlers: Vec::new(),
            server_manager: Some(Arc::downgrade(&server_manager)),
            pending_registrations: None,
            server_wakeup: None,
        }
    }

    /// Create with full ServerManager wiring: the manager Arc plus pre-extracted
    /// `pending_registrations` queue and `wakeup_event`. Callers extract these
    /// once (either while already holding a lock, or directly from `&mut self`
    /// inside `ServerManager`) and pass them in, avoiding a second
    /// `Mutex<ServerManager>` lock.
    pub fn new_with_server_manager_full(
        server_manager: Arc<Mutex<super::server_manager::ServerManager>>,
        pending_registrations: PendingRegistrationQueue,
        wakeup: Arc<super::os::event::Event>,
    ) -> Self {
        Self {
            convert_to_domain: false,
            is_domain: false,
            is_initialized_for_sm: false,
            session_handler: None,
            domain_handlers: Vec::new(),
            server_manager: Some(Arc::downgrade(&server_manager)),
            pending_registrations: Some(pending_registrations),
            server_wakeup: Some(wakeup),
        }
    }

    /// Build the closest upstream-shaped manager from a parent manager's
    /// already-extracted owner endpoints.
    ///
    /// Upstream `SessionRequestManager` always stores `ServerManager&`, so
    /// Rust only preserves the pending-registration queue and wakeup event when
    /// the corresponding `ServerManager` owner is also available.
    pub fn from_parent_server_manager_endpoints(
        server_manager: Option<Arc<Mutex<super::server_manager::ServerManager>>>,
        pending_registrations: Option<PendingRegistrationQueue>,
        wakeup: Option<Arc<super::os::event::Event>>,
    ) -> Self {
        match (server_manager, pending_registrations, wakeup) {
            (Some(sm), Some(queue), Some(wakeup)) => {
                Self::new_with_server_manager_full(sm, queue, wakeup)
            }
            (Some(sm), _, _) => Self::new_with_server_manager(sm),
            _ => Self::new(),
        }
    }

    /// Get the pending-registration queue for this session's owning
    /// ServerManager. Used by handlers that need to register a freshly-created
    /// session (push_ipc_interface, clone_current_object, sm::GetService)
    /// without locking `Mutex<ServerManager>`.
    pub fn pending_registrations(&self) -> Option<&PendingRegistrationQueue> {
        self.pending_registrations.as_ref()
    }

    /// Get the wakeup_event of this session's owning ServerManager.
    pub fn server_wakeup(&self) -> Option<&Arc<super::os::event::Event>> {
        self.server_wakeup.as_ref()
    }

    /// Get the ServerManager reference. Matches upstream `GetServerManager()`.
    pub fn get_server_manager(&self) -> Option<Arc<Mutex<super::server_manager::ServerManager>>> {
        self.server_manager.as_ref().and_then(Weak::upgrade)
    }

    pub fn is_domain(&self) -> bool {
        self.is_domain
    }

    pub fn convert_to_domain(&mut self) {
        self.domain_handlers = vec![self.session_handler.clone()];
        self.is_domain = true;
    }

    pub fn convert_to_domain_on_request_end(&mut self) {
        self.convert_to_domain = true;
    }

    pub fn domain_handler_count(&self) -> usize {
        self.domain_handlers.len()
    }

    pub fn has_session_handler(&self) -> bool {
        self.session_handler.is_some()
    }

    pub fn session_handler(&self) -> Option<&SessionRequestHandlerPtr> {
        self.session_handler.as_ref()
    }

    pub fn close_domain_handler(&mut self, index: usize) {
        if index < self.domain_handler_count() {
            self.domain_handlers[index] = None;
        } else {
            log::error!("Unexpected handler index {}", index);
        }
    }

    pub fn domain_handler(&self, index: usize) -> Option<&SessionRequestHandlerPtr> {
        assert!(
            index < self.domain_handler_count(),
            "Unexpected handler index {}",
            index
        );
        self.domain_handlers[index].as_ref()
    }

    pub fn append_domain_handler(&mut self, handler: SessionRequestHandlerPtr) {
        let trace = std::env::var_os("RUZU_DOMAIN_TRACE").is_some();
        let next_index = self.domain_handlers.len() + 1;
        if trace {
            log::info!("DOMAIN_SLOT append index={}", next_index);
        } else {
            log::debug!("AppendDomainHandler: index={}", next_index);
        }
        self.domain_handlers.push(Some(handler));
    }

    pub fn set_session_handler(&mut self, handler: SessionRequestHandlerPtr) {
        self.session_handler = Some(handler);
    }

    pub fn has_session_request_handler(&self, context: &HLERequestContext) -> bool {
        if self.is_domain() && context.has_domain_message_header() {
            let message_header = context.get_domain_message_header().unwrap();
            let object_id = message_header.object_id() as usize;

            if object_id > self.domain_handler_count() {
                log::error!("object_id {} is too big!", object_id);
                return false;
            }
            self.domain_handlers
                .get(object_id - 1)
                .map_or(false, |h| h.is_some())
        } else {
            self.session_handler.is_some()
        }
    }

    fn prepare_sync_request(&self, context: &HLERequestContext) -> PreparedSyncRequest {
        if !self.has_session_request_handler(context) {
            let (is_dom, obj_id, cmd) = if self.is_domain() && context.has_domain_message_header() {
                let h = context.get_domain_message_header().unwrap();
                (true, h.object_id(), format!("{:?}", h.command()))
            } else {
                (false, 0, String::new())
            };
            log::error!(
                "Session handler is invalid! is_domain={} object_id={} dom_cmd={} handler_count={}",
                is_dom,
                obj_id,
                cmd,
                self.domain_handler_count()
            );
            return PreparedSyncRequest::StubSuccess;
        }

        if self.is_domain() && context.has_domain_message_header() {
            let domain_message_header = context.get_domain_message_header().unwrap();
            let object_id = domain_message_header.object_id() as usize;
            let command = domain_message_header.command();
            let trace_dom = std::env::var_os("RUZU_DOMAIN_TRACE").is_some();

            match command {
                ipc::DomainCommandType::SendMessage => {
                    if object_id > self.domain_handler_count() {
                        if trace_dom {
                            log::info!(
                                "DOMAIN_SLOT stub_oor object_id={} handler_count={} cmd={} reason=out_of_range",
                                object_id,
                                self.domain_handler_count(),
                                context.get_command()
                            );
                        }
                        log::error!(
                            "object_id {} is too big! This probably means a recent service call \
                             needed to return a new interface!",
                            object_id
                        );
                        return PreparedSyncRequest::StubSuccess;
                    }
                    if let Some(Some(handler)) = self.domain_handlers.get(object_id - 1) {
                        if trace_dom {
                            log::info!(
                                "DOMAIN_SLOT dispatch object_id={} cmd={}",
                                object_id,
                                context.get_command()
                            );
                        }
                        log::debug!(
                            "HandleDomainSyncRequest: object_id={} cmd={}",
                            object_id,
                            context.get_command()
                        );
                        return PreparedSyncRequest::Domain(handler.clone());
                    }
                    if trace_dom {
                        log::info!(
                            "DOMAIN_SLOT stub_null object_id={} handler_count={} cmd={} reason=null_slot",
                            object_id,
                            self.domain_handler_count(),
                            context.get_command()
                        );
                    }
                    log::error!("Domain handler at index {} is null", object_id - 1);
                    return PreparedSyncRequest::StubSuccess;
                }
                ipc::DomainCommandType::CloseVirtualHandle => {
                    log::debug!("CloseVirtualHandle, object_id=0x{:08X}", object_id);
                    return PreparedSyncRequest::CloseVirtualHandle(object_id - 1);
                }
            }
        }

        // If domain but no domain header, fall through to session handler.
        // Matches upstream hle_ipc.cpp:62-63: "If there is no domain header,
        // the regular session handler is used"
        // This is normal for Control commands and also happens for certain
        // Request messages that the session handler dispatches directly.

        match &self.session_handler {
            Some(handler) => PreparedSyncRequest::Session(handler.clone()),
            None => PreparedSyncRequest::StubSuccess,
        }
    }

    fn finish_sync_request(&mut self) {
        if self.convert_to_domain {
            assert!(
                !self.is_domain(),
                "ServerSession is already a domain instance."
            );
            self.convert_to_domain();
            self.convert_to_domain = false;
        }
    }

    pub fn get_is_initialized_for_sm(&self) -> bool {
        self.is_initialized_for_sm
    }

    pub fn set_is_initialized_for_sm(&mut self) {
        self.is_initialized_for_sm = true;
    }
}

/// `OUT_BUF`: log the bytes the HLE handler wrote into a B/C-descriptor
/// buffer (the IPC output the guest reads back). Gated by
/// `RUZU_TRACE_OUT_BUF=1` (NOT by RUZU_SVC_TRACE — keep this opt-in
/// because some handlers write large buffers that would flood the log).
///
/// Output format matches `scripts/svc_diff.py::OUT_BUF_RE`:
///   `[T.TTTTTT] OUT_BUF [0xADDR] size=0xSZ: XXXXXXXX XXXXXXXX ...`
/// Bytes are dumped as space-separated 4-byte little-endian-as-stored
/// hex words (matching the `TLS_REQ` / `TLS_RSP_*` formatting), so the
/// diff script reads both sides identically.
///
/// Mirror of zuyu's `OUT_BUF_XDESC` in WriteToOutgoingCommandBuffer:
/// dump the bytes at X-descriptor[0] address. Used to confirm whether
/// ruzu's IPC pipeline writes back to the X-desc input the same way
/// zuyu's does (RW ioctls expect the input struct to be updated in
/// place even if the formal output is in B-desc).
impl HLERequestContext {
    pub(super) fn trace_out_buf_xdesc(&self) {
        if std::env::var_os("RUZU_TRACE_OUT_BUF").is_none() {
            return;
        }
        // Mirror zuyu's OUT_BUF_XDESC: log raw[0]/raw[1] of each X-descriptor.
        for (i, x) in self.buffer_x_descriptors.iter().enumerate() {
            eprintln!(
                "[{:>10.6}] OUT_BUF_XDESC[{}] raw[0]=0x{:08x} raw[1]=0x{:08x} addr=0x{:x} size=0x{:x}",
                crate::hle::kernel::trace_format::elapsed_secs(),
                i,
                x.raw,
                x.address_bits_0_31,
                x.address(),
                x.size(),
            );
        }
        let Some(x) = self.buffer_x_descriptors.first() else {
            return;
        };
        let addr = x.address();
        let size = x.size() as usize;
        if addr == 0 || size == 0 || size > 0x100 {
            return;
        }
        let bytes = self.read_guest_memory(addr, size);
        if bytes.is_empty() {
            return;
        }
        let mut payload = String::with_capacity(bytes.len() * 2 + bytes.len() / 4);
        for chunk in bytes.chunks(4) {
            for byte in chunk {
                use std::fmt::Write;
                let _ = write!(payload, "{:02x}", byte);
            }
            for _ in chunk.len()..4 {
                payload.push_str("00");
            }
            payload.push(' ');
        }
        eprintln!(
            "[{:>10.6}] OUT_BUF [0x{:x}] size=0x{:x}: {}",
            crate::hle::kernel::trace_format::elapsed_secs(),
            addr,
            size,
            payload.trim_end()
        );
    }
}

fn trace_out_buf(address: u64, data: &[u8]) {
    if std::env::var_os("RUZU_TRACE_OUT_BUF").is_none() {
        return;
    }
    if data.is_empty() {
        return;
    }
    // Cap to first 256 bytes (64 words) to avoid log flooding on
    // multi-KB ioctl outputs. Diff script compares strings so both
    // ruzu and zuyu must use the same cap. 256 is plenty for the
    // nvdrv ioctl output structures we care about.
    const MAX_BYTES: usize = 256;
    let n = data.len().min(MAX_BYTES);
    let mut payload = String::with_capacity(n * 2 + n / 4);
    for chunk in data[..n].chunks(4) {
        for byte in chunk {
            use std::fmt::Write;
            let _ = write!(payload, "{:02x}", byte);
        }
        // pad short tail chunk so word boundary stays consistent
        for _ in chunk.len()..4 {
            payload.push_str("00");
        }
        payload.push(' ');
    }
    eprintln!(
        "[{:>10.6}] OUT_BUF [0x{:x}] size=0x{:x}: {}",
        crate::hle::kernel::trace_format::elapsed_secs(),
        address,
        data.len(),
        payload.trim_end()
    );
}

/// Per-(service_name, cmd) wall-clock aggregator. Populated by
/// `RUZU_PROFILE_IPC_PHASES=1` (we piggyback on the same flag because both
/// profilers answer the same question: "where does IPC time go"). Dumped
/// by `dump_hle_handler_profile()`.
static HLE_HANDLER_PROFILE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<(String, u32), HleHandlerAgg>>,
> = std::sync::OnceLock::new();

#[derive(Default, Clone)]
struct HleHandlerAgg {
    count: u64,
    total_ns: u64,
    max_ns: u64,
}

fn record_hle_handler_profile(service_name: &str, cmd: u32, elapsed: std::time::Duration) {
    let map =
        HLE_HANDLER_PROFILE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let ns = elapsed.as_nanos() as u64;
    let mut guard = map.lock().unwrap();
    let entry = guard.entry((service_name.to_string(), cmd)).or_default();
    entry.count += 1;
    entry.total_ns = entry.total_ns.saturating_add(ns);
    if ns > entry.max_ns {
        entry.max_ns = ns;
    }
}

pub fn dump_hle_handler_profile() {
    let Some(map) = HLE_HANDLER_PROFILE.get() else {
        return;
    };
    let entries: Vec<((String, u32), HleHandlerAgg)> = {
        let g = map.lock().unwrap();
        g.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    };
    if entries.is_empty() {
        return;
    }
    let mut entries = entries;
    entries.sort_by_key(|(_, e)| std::cmp::Reverse(e.total_ns));
    eprintln!("[HLE_HANDLER_PROFILE] top (service, cmd) by total time:");
    for ((svc, cmd), e) in entries.iter().take(20) {
        eprintln!(
            "[HLE_HANDLER_PROFILE]   service={:<48} cmd={:<4} count={:<6} total={:>9.2}ms avg={:>9.2}us max={:>9.2}us",
            svc,
            cmd,
            e.count,
            e.total_ns as f64 / 1e6,
            e.total_ns as f64 / e.count as f64 / 1e3,
            e.max_ns as f64 / 1e3,
        );
    }
}

pub fn complete_sync_request(
    manager: &Arc<Mutex<SessionRequestManager>>,
    context: &mut HLERequestContext,
) -> ResultCode {
    let profile_phases = std::env::var_os("RUZU_PROFILE_IPC_PHASES").is_some();
    let p_t0 = if profile_phases {
        Some(std::time::Instant::now())
    } else {
        None
    };
    let dispatch = {
        let guard = manager.lock().unwrap();
        guard.prepare_sync_request(context)
    };
    let p_after_prepare = p_t0.map(|t0| {
        let elapsed = t0.elapsed();
        crate::hle::kernel::svc::svc_ipc::record_ipc_phase("06a_prepare_sync_request", elapsed);
        std::time::Instant::now()
    });

    let trace_dispatch = std::env::var_os("RUZU_HLE_DISPATCH_TRACE").is_some();

    let result = match dispatch {
        PreparedSyncRequest::Session(handler) | PreparedSyncRequest::Domain(handler) => {
            if trace_dispatch {
                let tid = context
                    .thread
                    .as_ref()
                    .map(|t| t.lock().unwrap().get_thread_id())
                    .unwrap_or(0);
                log::warn!(
                    "HLE dispatch enter tid={} service={} cmd={} tipc={}",
                    tid,
                    handler.service_name(),
                    context.get_command(),
                    context.is_tipc()
                );
            }
            IPC_TRACE_CURRENT.with(|c| {
                *c.borrow_mut() = (handler.service_name().to_string(), context.get_command(), 0);
            });
            // Snapshot the request words so we can emit IPC_REQUEST after the
            // handler runs (handler will overwrite cmd_buf with the reply).
            // Only paid when the IPC_REQUEST category is enabled.
            let req_words = if common::trace::is_enabled(common::trace::cat::IPC_REQUEST) {
                let n = ipc::COMMAND_BUFFER_LENGTH.min(16);
                let mut buf = [0u32; 16];
                buf[..n].copy_from_slice(&context.cmd_buf[..n]);
                Some(buf)
            } else {
                None
            };
            let handler_t0 = p_after_prepare.map(|_| std::time::Instant::now());
            let result = handler.handle_sync_request(context);
            if let Some(buf) = req_words {
                use std::sync::atomic::{AtomicUsize, Ordering};
                static REQ_SEQ: AtomicUsize = AtomicUsize::new(0);
                let seq = REQ_SEQ.fetch_add(1, Ordering::Relaxed);
                if seq < ipc_trace_ring_limit() {
                    let (svc, cmd, ioctl) = IPC_TRACE_CURRENT.with(|c| c.borrow().clone());
                    let svc_str = if svc.is_empty() { "?" } else { svc.as_str() };
                    let svc_id = common::trace::intern_service(svc_str);
                    let tid = context
                        .thread
                        .as_ref()
                        .map(|t| t.lock().unwrap().get_thread_id())
                        .unwrap_or(0);
                    let mut args: [u64; 14] = [0; 14];
                    args[0] = seq as u64;
                    args[1] = svc_id as u64;
                    args[2] = cmd as u64;
                    args[3] = ioctl as u64;
                    args[4] = tid;
                    const PAYLOAD_WORDS: usize = 9; // 14 - 5 fixed
                    for i in 0..PAYLOAD_WORDS {
                        args[5 + i] = buf[i] as u64;
                    }
                    common::trace::emit_raw(
                        common::trace::cat::IPC_REQUEST,
                        &args[..5 + PAYLOAD_WORDS],
                    );
                }
            }
            if let Some(t0) = handler_t0 {
                let elapsed = t0.elapsed();
                crate::hle::kernel::svc::svc_ipc::record_ipc_phase(
                    "06b_handle_sync_request_dispatch",
                    elapsed,
                );
                if std::env::var_os("RUZU_PROFILE_IPC_PHASES").is_some() {
                    record_hle_handler_profile(
                        handler.service_name(),
                        context.get_command(),
                        elapsed,
                    );
                }
            }
            if trace_dispatch {
                log::warn!(
                    "HLE dispatch leave service={} cmd={} result={:#x}",
                    handler.service_name(),
                    context.get_command(),
                    result.get_inner_value()
                );
            }
            result
        }
        PreparedSyncRequest::CloseVirtualHandle(index) => {
            manager.lock().unwrap().close_domain_handler(index);
            IPC_TRACE_CURRENT.with(|c| {
                *c.borrow_mut() = ("__close_virtual_handle__".to_string(), index as u32, 0);
            });
            let mut rb = super::ipc_helpers::ResponseBuilder::new(context, 2, 0, 0);
            rb.push_result(RESULT_SUCCESS);
            RESULT_SUCCESS
        }
        PreparedSyncRequest::StubSuccess => {
            IPC_TRACE_CURRENT.with(|c| {
                *c.borrow_mut() = ("__stub_success__".to_string(), context.get_command(), 0);
            });
            let mut rb = super::ipc_helpers::ResponseBuilder::new(context, 2, 0, 0);
            rb.push_result(RESULT_SUCCESS);
            context.write_to_outgoing_command_buffer();
            RESULT_SUCCESS
        }
    };

    manager.lock().unwrap().finish_sync_request();
    result
}

/// Class containing information about an in-flight IPC request being handled by an HLE service
/// implementation.
///
/// Corresponds to upstream `HLERequestContext`.
///
/// Upstream stores `KThread* thread`, `Core::Memory::Memory& memory`, `KernelCore& kernel`,
/// and `KServerSession* server_session`. We store the thread, memory, the kernel's
/// non-owning system reference, and the TLS address to match the upstream access
/// pattern (thread→process→handle_table, memory write-back).
pub struct HLERequestContext {
    /// IPC command buffer.
    pub cmd_buf: [u32; ipc::COMMAND_BUFFER_LENGTH],

    /// The requesting thread. Matches upstream `KThread* thread`.
    thread: Option<Arc<KThreadLock>>,
    /// Guest memory bridge. Matches upstream `Core::Memory::Memory& memory`.
    memory: Option<Arc<std::sync::Mutex<crate::memory::memory::Memory>>>,
    /// Non-owning system owner obtained from upstream's `KernelCore& kernel`.
    system: SystemRef,
    /// TLS address for command buffer read/write.
    /// Upstream derives this from `thread->GetTlsAddress()`.
    tls_address: u64,

    command_header: Option<ipc::CommandHeader>,
    handle_descriptor_header: Option<ipc::HandleDescriptorHeader>,
    data_payload_header: Option<ipc::DataPayloadHeader>,
    domain_message_header: Option<ipc::DomainMessageHeader>,

    buffer_x_descriptors: Vec<ipc::BufferDescriptorX>,
    buffer_a_descriptors: Vec<ipc::BufferDescriptorABW>,
    buffer_b_descriptors: Vec<ipc::BufferDescriptorABW>,
    buffer_w_descriptors: Vec<ipc::BufferDescriptorABW>,
    buffer_c_descriptors: Vec<ipc::BufferDescriptorC>,

    incoming_move_handles: Vec<Handle>,
    incoming_copy_handles: Vec<Handle>,

    pub(crate) outgoing_move_objects: Vec<KAutoObjectRef>,
    pub(crate) outgoing_copy_objects: Vec<KAutoObjectRef>,
    outgoing_domain_objects: Vec<Option<SessionRequestHandlerPtr>>,

    command: u32,
    pid: u64,
    pub write_size: u32,
    pub data_payload_offset: u32,
    pub handles_offset: u32,
    pub domain_offset: u32,

    manager: Option<Arc<Mutex<SessionRequestManager>>>,
    service_manager: Option<Arc<Mutex<ServiceManager>>>,
    is_deferred: bool,

    /// Temporary: holds the server session from the last create_session_with_manager call,
    /// so that push_ipc_interface can register it with the ServerManager.
    pub(crate) last_created_server_session:
        Option<Arc<Mutex<crate::hle::kernel::k_server_session::KServerSession>>>,
}

impl HLERequestContext {
    fn owner_process_memory(
        thread: &Arc<KThreadLock>,
    ) -> Option<Arc<std::sync::Mutex<crate::memory::memory::Memory>>> {
        let parent = {
            let thread_guard = thread.lock().unwrap();
            thread_guard.parent.as_ref()?.clone()
        }
        .upgrade()?;
        let process = parent.lock().unwrap();
        process.get_memory()
    }

    /// Create a context with thread/memory access, matching upstream constructor.
    ///
    /// Upstream: `HLERequestContext(KernelCore&, Memory&, KServerSession*, KThread*)`
    pub fn new_with_thread(thread: Arc<KThreadLock>, tls_address: u64) -> Self {
        let memory = Self::owner_process_memory(&thread);
        let system = crate::hle::kernel::kernel::get_kernel_ref()
            .map(|kernel| kernel.system())
            .unwrap_or_else(SystemRef::null);
        Self {
            cmd_buf: [0u32; ipc::COMMAND_BUFFER_LENGTH],
            thread: Some(thread),
            memory,
            system,
            tls_address,
            command_header: None,
            handle_descriptor_header: None,
            data_payload_header: None,
            domain_message_header: None,
            buffer_x_descriptors: Vec::new(),
            buffer_a_descriptors: Vec::new(),
            buffer_b_descriptors: Vec::new(),
            buffer_w_descriptors: Vec::new(),
            buffer_c_descriptors: Vec::new(),
            incoming_move_handles: Vec::new(),
            incoming_copy_handles: Vec::new(),
            outgoing_move_objects: Vec::new(),
            outgoing_copy_objects: Vec::new(),
            outgoing_domain_objects: Vec::new(),
            command: 0,
            pid: 0,
            write_size: 0,
            data_payload_offset: 0,
            handles_offset: 0,
            domain_offset: 0,
            manager: None,
            service_manager: None,
            is_deferred: false,
            last_created_server_session: None,
        }
    }

    /// Create a context without thread/memory (for tests or contexts where
    /// the caller manages TLS read/write externally).
    pub fn new() -> Self {
        Self {
            cmd_buf: [0u32; ipc::COMMAND_BUFFER_LENGTH],
            thread: None,
            memory: None,
            system: SystemRef::null(),
            tls_address: 0,
            command_header: None,
            handle_descriptor_header: None,
            data_payload_header: None,
            domain_message_header: None,
            buffer_x_descriptors: Vec::new(),
            buffer_a_descriptors: Vec::new(),
            buffer_b_descriptors: Vec::new(),
            buffer_w_descriptors: Vec::new(),
            buffer_c_descriptors: Vec::new(),
            incoming_move_handles: Vec::new(),
            incoming_copy_handles: Vec::new(),
            outgoing_move_objects: Vec::new(),
            outgoing_copy_objects: Vec::new(),
            outgoing_domain_objects: Vec::new(),
            command: 0,
            pid: 0,
            write_size: 0,
            data_payload_offset: 0,
            handles_offset: 0,
            domain_offset: 0,
            manager: None,
            service_manager: None,
            is_deferred: false,
            last_created_server_session: None,
        }
    }

    /// Returns a pointer to the IPC command buffer for this request.
    pub fn command_buffer(&self) -> &[u32; ipc::COMMAND_BUFFER_LENGTH] {
        &self.cmd_buf
    }

    /// Returns the requesting thread.
    ///
    /// Matches upstream ownership where `HLERequestContext` carries `KThread* thread`.
    pub fn get_thread(&self) -> Option<Arc<KThreadLock>> {
        self.thread.clone()
    }

    /// Return the system associated with upstream's request `KernelCore&`.
    pub fn get_system(&self) -> Option<SystemRef> {
        (!self.system.is_null()).then_some(self.system)
    }

    pub fn tls_address(&self) -> u64 {
        self.tls_address
    }

    /// Returns a mutable pointer to the IPC command buffer.
    pub fn command_buffer_mut(&mut self) -> &mut [u32; ipc::COMMAND_BUFFER_LENGTH] {
        &mut self.cmd_buf
    }

    pub fn get_hipc_command(&self) -> u32 {
        self.command
    }

    pub fn get_tipc_command(&self) -> u32 {
        if let Some(ref header) = self.command_header {
            let raw_type = header.command_type_raw();
            raw_type.wrapping_sub(ipc::CommandType::TipcCommandRegion as u32)
        } else {
            0
        }
    }

    pub fn get_command(&self) -> u32 {
        if let Some(ref header) = self.command_header {
            if header.is_tipc() {
                self.get_tipc_command()
            } else {
                self.get_hipc_command()
            }
        } else {
            self.get_hipc_command()
        }
    }

    pub fn is_tipc(&self) -> bool {
        self.command_header.as_ref().map_or(false, |h| h.is_tipc())
    }

    pub fn get_command_type(&self) -> ipc::CommandType {
        self.command_header
            .as_ref()
            .map_or(ipc::CommandType::Invalid, |h| h.command_type())
    }

    pub fn get_pid(&self) -> u64 {
        self.pid
    }

    pub fn get_data_payload_offset(&self) -> u32 {
        self.data_payload_offset
    }

    pub fn buffer_descriptor_x(&self) -> &[ipc::BufferDescriptorX] {
        &self.buffer_x_descriptors
    }

    pub fn buffer_descriptor_a(&self) -> &[ipc::BufferDescriptorABW] {
        &self.buffer_a_descriptors
    }

    pub fn buffer_descriptor_b(&self) -> &[ipc::BufferDescriptorABW] {
        &self.buffer_b_descriptors
    }

    #[cfg(test)]
    pub fn set_buffer_b_descriptors_for_test(
        &mut self,
        descriptors: Vec<ipc::BufferDescriptorABW>,
    ) {
        self.buffer_b_descriptors = descriptors;
    }

    pub fn buffer_descriptor_c(&self) -> &[ipc::BufferDescriptorC] {
        &self.buffer_c_descriptors
    }

    pub fn get_domain_message_header(&self) -> Option<&ipc::DomainMessageHeader> {
        self.domain_message_header.as_ref()
    }

    pub fn has_domain_message_header(&self) -> bool {
        self.domain_message_header.is_some()
    }

    pub fn get_copy_handle(&self, index: usize) -> Handle {
        self.incoming_copy_handles.get(index).copied().unwrap_or(0)
    }

    pub fn get_move_handle(&self, index: usize) -> Handle {
        self.incoming_move_handles.get(index).copied().unwrap_or(0)
    }

    pub fn add_move_object(&mut self, obj: KAutoObjectRef) {
        self.outgoing_move_objects.push(obj);
    }

    pub fn add_copy_object(&mut self, obj: KAutoObjectRef) {
        self.outgoing_copy_objects.push(obj);
    }

    /// Convenience: add a move object from a raw handle.
    pub fn add_move_handle(&mut self, handle: Handle) {
        self.outgoing_move_objects
            .push(KAutoObjectRef::Handle(handle));
    }

    /// Convenience: add a copy object from a raw handle.
    pub fn add_copy_handle(&mut self, handle: Handle) {
        self.outgoing_copy_objects
            .push(KAutoObjectRef::Handle(handle));
    }

    pub fn add_domain_object(&mut self, object: SessionRequestHandlerPtr) {
        self.outgoing_domain_objects.push(Some(object));
    }

    /// Add a null domain object entry. Upstream writes domain object ID = 0 for nullptr
    /// OutInterface parameters (e.g., when a command returns an error but the method
    /// signature has Out<SharedPointer<T>>). The response layout is compile-time fixed,
    /// so the slot must always be present.
    pub fn add_null_domain_object(&mut self) {
        self.outgoing_domain_objects.push(None);
    }

    /// Set the Memory bridge for TLS reads/writes.
    /// Matches upstream `Core::Memory::Memory& memory` passed to the HLERequestContext constructor.
    pub fn set_memory(&mut self, memory: Arc<std::sync::Mutex<crate::memory::memory::Memory>>) {
        self.memory = Some(memory);
    }

    pub fn set_session_request_manager(&mut self, manager: Arc<Mutex<SessionRequestManager>>) {
        self.manager = Some(manager);
    }

    pub fn get_manager(&self) -> Option<&Arc<Mutex<SessionRequestManager>>> {
        self.manager.as_ref()
    }

    pub fn set_service_manager(&mut self, service_manager: Arc<Mutex<ServiceManager>>) {
        self.service_manager = Some(service_manager);
    }

    pub fn get_service_manager(&self) -> Option<&Arc<Mutex<ServiceManager>>> {
        self.service_manager.as_ref()
    }

    /// Creates a full KSession (server + client) for a service handler.
    /// Registers the client session in the process handle table and links
    /// the server session to the manager.
    ///
    /// Matches upstream flow:
    /// 1. KSession::Create(kernel) → creates session with server + client
    /// 2. session->Initialize(nullptr, 0)
    /// 3. ServerManager::RegisterSession(server_session, manager)
    /// 4. handle_table.Add(client_session)
    pub fn create_session_for_service(
        &mut self,
        handler: SessionRequestHandlerPtr,
    ) -> Option<Handle> {
        // Inherit ServerManager + pending-registration queue + wakeup_event
        // from the parent's SessionRequestManager, matching upstream
        // `PushIpcInterface` / `CloneCurrentObject` semantics:
        //   auto next_manager = make_shared<SessionRequestManager>(
        //       kernel, manager->GetServerManager());
        // Propagating the queue + wakeup along with the ServerManager Arc
        // lets descendant sessions register themselves without locking
        // `Mutex<ServerManager>` (which would deadlock against the host
        // thread's `loop_process`).
        let (parent_server_manager, parent_queue, parent_wakeup) = self
            .manager
            .as_ref()
            .map(|m| {
                let guard = m.lock().unwrap();
                (
                    guard.get_server_manager(),
                    guard.pending_registrations().cloned(),
                    guard.server_wakeup().cloned(),
                )
            })
            .unwrap_or((None, None, None));
        let manager = Arc::new(std::sync::Mutex::new(
            SessionRequestManager::from_parent_server_manager_endpoints(
                parent_server_manager,
                parent_queue,
                parent_wakeup,
            ),
        ));
        manager.lock().unwrap().set_session_handler(handler);
        let handle = self.create_session_with_manager(manager.clone())?;
        self.register_last_created_session_with_manager(manager);
        Some(handle)
    }

    /// Register the server endpoint produced by the most recent session
    /// creation with its ServerManager owner.
    ///
    /// Upstream does this before exposing the client endpoint:
    /// `manager->GetServerManager().RegisterSession(&session->GetServerSession(), next_manager)`.
    /// Ruzu's host-thread manager owns its mutex while looping, so registration
    /// is delivered through the manager's pending queue and wakeup event.
    pub(crate) fn register_last_created_session_with_manager(
        &mut self,
        manager: Arc<std::sync::Mutex<SessionRequestManager>>,
    ) -> bool {
        let (queue, wakeup) = {
            let guard = manager.lock().unwrap();
            (
                guard.pending_registrations().cloned(),
                guard.server_wakeup().cloned(),
            )
        };

        let Some(server_session) = self.last_created_server_session.take() else {
            return false;
        };

        if let Some(queue) = queue {
            queue.lock().unwrap().push((server_session, manager));
            if let Some(wakeup) = wakeup {
                wakeup.signal();
            }
            true
        } else {
            false
        }
    }

    /// Creates a full KSession that shares an existing SessionRequestManager,
    /// returning the client-session object ID instead of pre-resolving a
    /// process handle.
    ///
    /// This matches the upstream ownership model used by
    /// `Controller::CloneCurrentObject` and `IPC::ResponseBuilder::PushIpcInterface`,
    /// where the response stores the client-session object and final handle
    /// translation happens later in `WriteToOutgoingCommandBuffer()`.
    pub fn create_session_with_manager_object_id(
        &mut self,
        manager: Arc<std::sync::Mutex<SessionRequestManager>>,
    ) -> Option<u64> {
        log::info!("HLERequestContext::create_session_with_manager_object_id: begin");
        let thread = self.thread.as_ref()?;
        let thread_guard = thread.lock().unwrap();
        let parent = thread_guard.parent.as_ref()?.upgrade()?;
        let mut process = parent.lock().unwrap();

        static NEXT_SESSION_OBJECT_ID: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0x1000_0000);
        let object_id = NEXT_SESSION_OBJECT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let session = Arc::new(std::sync::Mutex::new(
            crate::hle::kernel::k_session::KSession::new(),
        ));
        {
            let mut ksession = session.lock().unwrap();
            ksession.initialize(None, 0);
            ksession.server.lock().unwrap().initialize(object_id);
            ksession.client.lock().unwrap().initialize(object_id);
        }
        log::info!(
            "HLERequestContext::create_session_with_manager_object_id: endpoints initialized object_id={:#x}",
            object_id
        );

        let server_session = session.lock().unwrap().get_server_session().clone();
        let client_session = session.lock().unwrap().get_client_session().clone();

        log::info!(
            "HLERequestContext::create_session_with_manager_object_id: registering session endpoints"
        );
        server_session.lock().unwrap().set_manager(manager.clone());
        client_session.lock().unwrap().initialize(object_id);

        self.last_created_server_session = Some(server_session.clone());

        log::info!(
            "HLERequestContext::create_session_with_manager_object_id: registering process objects"
        );
        process.register_session_object(object_id, session);
        process.register_client_session_object(object_id, client_session, object_id);

        log::info!(
            "HLERequestContext::create_session_with_manager_object_id: done object_id={:#x}",
            object_id
        );
        Some(object_id)
    }

    /// Creates a full KSession that shares an existing SessionRequestManager.
    /// Used by CloneCurrentObject to replicate upstream behavior where the clone
    /// is registered with the SAME manager as the parent session.
    ///
    /// Matches upstream `Controller::CloneCurrentObject`:
    /// ```cpp
    /// session_manager->GetServerManager().RegisterSession(
    ///     &session->GetServerSession(), session_manager);
    /// ```
    pub fn create_session_with_manager(
        &mut self,
        manager: Arc<std::sync::Mutex<SessionRequestManager>>,
    ) -> Option<Handle> {
        let object_id = self.create_session_with_manager_object_id(manager)?;
        let thread = self.thread.as_ref()?;
        let thread_guard = thread.lock().unwrap();
        let parent = thread_guard.parent.as_ref()?.upgrade()?;
        let mut process = parent.lock().unwrap();
        let handle = process.handle_table.add(object_id).ok()?;
        log::info!(
            "HLERequestContext::create_session_with_manager: done handle={:#x}",
            handle
        );

        Some(handle)
    }

    /// Creates a KReadableEvent and registers it in the current process object table.
    ///
    /// Matches the ownership of upstream `ServiceContext::CreateEvent` without
    /// pre-allocating an IPC copy handle. The final handle is allocated when
    /// `ResponseBuilder::push_copy_object_id(...)` is written back.
    pub fn create_readable_event_object(
        &self,
        signaled: bool,
    ) -> Option<(u64, Arc<Mutex<KReadableEvent>>)> {
        let thread = self.thread.as_ref()?;
        let thread_guard = thread.lock().unwrap();
        let parent = thread_guard.parent.as_ref()?.upgrade()?;
        let mut process = parent.lock().unwrap();

        static NEXT_EVENT_ID: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0x2000_0000);
        let object_id = NEXT_EVENT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // Create and register a KReadableEvent.
        let readable = Arc::new(Mutex::new(KReadableEvent::new()));
        {
            let mut re = readable.lock().unwrap();
            re.initialize(0, object_id);
            if signaled {
                re.is_signaled
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
        process.register_readable_event_object(object_id, readable.clone());

        Some((object_id, readable))
    }

    /// Creates a KReadableEvent and registers it in the current process handle table.
    ///
    /// Matches older Rust service paths that still pre-resolve process handles
    /// before writing IPC responses. Prefer `create_readable_event_object(...)`
    /// for upstream-shaped `OutCopyHandle<KReadableEvent>` serialization.
    pub fn create_readable_event(
        &self,
        signaled: bool,
    ) -> Option<(Handle, Arc<Mutex<KReadableEvent>>)> {
        let (object_id, readable) = self.create_readable_event_object(signaled)?;
        let thread = self.thread.as_ref()?;
        let thread_guard = thread.lock().unwrap();
        let parent = thread_guard.parent.as_ref()?.upgrade()?;
        let mut process = parent.lock().unwrap();

        let handle = process.handle_table.add(object_id).ok()?;
        Some((handle, readable))
    }

    /// Creates a signaled KReadableEvent and registers it in the process
    /// handle table. Returns the handle for use in copy handle responses.
    ///
    /// Matches upstream `ServiceContext::CreateEvent` + signal pattern.
    pub fn create_readable_event_handle(&self, signaled: bool) -> Option<Handle> {
        self.create_readable_event(signaled)
            .map(|(handle, _)| handle)
    }

    /// Installs an existing readable event object into the current process handle table.
    pub fn copy_handle_for_readable_event(
        &self,
        readable_event: Arc<Mutex<KReadableEvent>>,
    ) -> Option<Handle> {
        let thread = self.thread.as_ref()?;
        let thread_guard = thread.lock().unwrap();
        let parent = thread_guard.parent.as_ref()?.upgrade()?;
        let mut process = parent.lock().unwrap();

        if process.ensure_handle_table_initialized() != RESULT_SUCCESS.get_inner_value() {
            return None;
        }

        let object_id = readable_event.lock().unwrap().object_id;
        process.register_readable_event_object(object_id, readable_event);
        let handle = process.handle_table.add(object_id).ok();
        if std::env::var_os("RUZU_TRACE_EVENTS").is_some() {
            let (svc, cmd, _ioctl) = IPC_TRACE_CURRENT.with(|c| c.borrow().clone());
            log::info!(
                "EVENT_HANDOUT object_id={} handle={:?} svc={} cmd={}",
                object_id,
                handle,
                svc,
                cmd
            );
        }
        handle
    }

    /// Installs an existing readable-event object into the current process object table,
    /// returning the object ID without pre-resolving a process handle.
    ///
    /// This matches upstream `OutCopyHandle<KReadableEvent>` ownership more closely:
    /// the response stores the object, and `WriteToOutgoingCommandBuffer()` performs
    /// final `handle_table.Add(...)` translation.
    pub fn register_readable_event_object(
        &self,
        readable_event: Arc<Mutex<KReadableEvent>>,
    ) -> Option<u64> {
        let thread = self.thread.as_ref()?;
        let thread_guard = thread.lock().unwrap();
        let parent = thread_guard.parent.as_ref()?.upgrade()?;
        let mut process = parent.lock().unwrap();

        if process.ensure_handle_table_initialized() != RESULT_SUCCESS.get_inner_value() {
            return None;
        }

        let object_id = readable_event.lock().unwrap().object_id;
        process.register_readable_event_object(object_id, readable_event);
        Some(object_id)
    }

    pub fn owner_process_arc(&self) -> Option<Arc<crate::hle::kernel::k_process::ProcessLock>> {
        let thread = self.thread.as_ref()?;
        let thread_guard = thread.lock().unwrap();
        thread_guard.parent.as_ref()?.upgrade()
    }

    pub fn get_is_deferred(&self) -> bool {
        self.is_deferred
    }

    pub fn set_is_deferred(&mut self) {
        self.is_deferred = true;
    }

    pub fn set_is_deferred_value(&mut self, value: bool) {
        self.is_deferred = value;
    }

    /// Helper function to get the size of the input buffer.
    pub fn get_read_buffer_size(&self, buffer_index: usize) -> usize {
        let is_buffer_a = self.buffer_a_descriptors.len() > buffer_index
            && self.buffer_a_descriptors[buffer_index].size() > 0;
        if is_buffer_a {
            self.buffer_a_descriptors[buffer_index].size() as usize
        } else if self.buffer_x_descriptors.len() > buffer_index {
            self.buffer_x_descriptors[buffer_index].size() as usize
        } else {
            0
        }
    }

    /// Helper function to get the size of the output buffer.
    pub fn get_write_buffer_size(&self, buffer_index: usize) -> usize {
        let is_buffer_b = self.buffer_b_descriptors.len() > buffer_index
            && self.buffer_b_descriptors[buffer_index].size() > 0;
        if is_buffer_b {
            self.buffer_b_descriptors[buffer_index].size() as usize
        } else if self.buffer_c_descriptors.len() > buffer_index {
            self.buffer_c_descriptors[buffer_index].size() as usize
        } else {
            0
        }
    }

    /// Helper function to test whether the input buffer at buffer_index can be read.
    pub fn can_read_buffer(&self, buffer_index: usize) -> bool {
        let is_buffer_a = self.buffer_a_descriptors.len() > buffer_index
            && self.buffer_a_descriptors[buffer_index].size() > 0;
        if is_buffer_a {
            self.buffer_a_descriptors.len() > buffer_index
        } else {
            self.buffer_x_descriptors.len() > buffer_index
        }
    }

    /// Helper function to test whether the output buffer at buffer_index can be written.
    pub fn can_write_buffer(&self, buffer_index: usize) -> bool {
        let is_buffer_b = self.buffer_b_descriptors.len() > buffer_index
            && self.buffer_b_descriptors[buffer_index].size() > 0;
        if is_buffer_b {
            self.buffer_b_descriptors.len() > buffer_index
        } else {
            self.buffer_c_descriptors.len() > buffer_index
        }
    }

    // -- ReadBuffer / WriteBuffer --
    //
    // Matches upstream methods on HLERequestContext (hle_ipc.h lines 261-328,
    // hle_ipc.cpp lines 317-506).
    //
    // Upstream uses `Core::Memory::Memory& memory` (page-table-aware) to
    // read/write guest buffers. Ruzu uses the same `Memory` bridge.
    //
    // Upstream `ReadBuffer` returns `std::span<const u8>` (zero-copy).
    // We return `Vec<u8>` (copy) because the Memory bridge is behind a Mutex.
    // See DIFF_WITH_YUZU.md for rationale.

    /// Returns a reference to the Memory bridge.
    ///
    /// Matches upstream `Core::Memory::Memory& GetMemory() const`.
    pub fn get_memory(&self) -> Option<&Arc<std::sync::Mutex<crate::memory::memory::Memory>>> {
        self.memory.as_ref()
    }

    /// Read from buffer descriptor A at the given index.
    ///
    /// Matches upstream `HLERequestContext::ReadBufferA(size_t buffer_index)`.
    pub fn read_buffer_a(&self, buffer_index: usize) -> Vec<u8> {
        if buffer_index >= self.buffer_a_descriptors.len() {
            log::error!("BufferDescriptorA invalid buffer_index {}", buffer_index);
            return Vec::new();
        }
        let size = self.buffer_a_descriptors[buffer_index].size() as usize;
        let address = self.buffer_a_descriptors[buffer_index].address();
        self.read_guest_memory(address, size)
    }

    /// Read from buffer descriptor X at the given index.
    ///
    /// Matches upstream `HLERequestContext::ReadBufferX(size_t buffer_index)`.
    pub fn read_buffer_x(&self, buffer_index: usize) -> Vec<u8> {
        if buffer_index >= self.buffer_x_descriptors.len() {
            log::error!("BufferDescriptorX invalid buffer_index {}", buffer_index);
            return Vec::new();
        }
        let size = self.buffer_x_descriptors[buffer_index].size() as usize;
        let address = self.buffer_x_descriptors[buffer_index].address();
        self.read_guest_memory(address, size)
    }

    /// Read from the appropriate buffer descriptor (A if available, else X).
    ///
    /// Matches upstream `HLERequestContext::ReadBuffer(size_t buffer_index)`.
    /// Returns a copy (see `ReadBufferCopy` in upstream for equivalent semantics).
    pub fn read_buffer(&self, buffer_index: usize) -> Vec<u8> {
        let is_buffer_a = self.buffer_a_descriptors.len() > buffer_index
            && self.buffer_a_descriptors[buffer_index].size() > 0;
        let is_buffer_x = self.buffer_x_descriptors.len() > buffer_index
            && self.buffer_x_descriptors[buffer_index].size() > 0;

        if is_buffer_a && is_buffer_x {
            log::warn!(
                "ReadBuffer: both A and X descriptors available, a.size={}, x.size={}",
                self.buffer_a_descriptors[buffer_index].size(),
                self.buffer_x_descriptors[buffer_index].size()
            );
        }

        if is_buffer_a {
            self.read_buffer_a(buffer_index)
        } else {
            self.read_buffer_x(buffer_index)
        }
    }

    /// Write to the appropriate buffer descriptor (B if available, else C).
    ///
    /// Matches upstream `HLERequestContext::WriteBuffer(const void*, size_t, size_t)`.
    /// Returns the number of bytes written.
    pub fn write_buffer(&self, data: &[u8], buffer_index: usize) -> usize {
        if data.is_empty() {
            IPC_TRACE_CURRENT.with(|current| {
                let current = current.borrow();
                log::warn!(
                    "WriteBuffer: skip empty buffer write service={} cmd={} ioctl=0x{:X} index={}",
                    current.0,
                    current.1,
                    current.2,
                    buffer_index
                );
            });
            return 0;
        }

        let is_buffer_b = self.buffer_b_descriptors.len() > buffer_index
            && self.buffer_b_descriptors[buffer_index].size() > 0;
        let buffer_size = self.get_write_buffer_size(buffer_index);
        let mut size = data.len();
        if size > buffer_size {
            log::error!(
                "WriteBuffer: size ({:#x}) > buffer_size ({:#x})",
                size,
                buffer_size
            );
            size = buffer_size;
        }

        if is_buffer_b {
            if self.buffer_b_descriptors.len() <= buffer_index
                || (self.buffer_b_descriptors[buffer_index].size() as usize) < size
            {
                log::error!(
                    "BufferDescriptorB is invalid, index={}, size={}",
                    buffer_index,
                    size
                );
                return 0;
            }
            self.write_buffer_b(&data[..size], buffer_index)
        } else {
            if self.buffer_c_descriptors.len() <= buffer_index
                || (self.buffer_c_descriptors[buffer_index].size() as usize) < size
            {
                log::error!(
                    "BufferDescriptorC is invalid, index={}, size={}",
                    buffer_index,
                    size
                );
                return 0;
            }
            self.write_buffer_c(&data[..size], buffer_index)
        }
    }

    /// Write to buffer descriptor B at the given index.
    ///
    /// Matches upstream `HLERequestContext::WriteBufferB(const void*, size_t, size_t)`.
    pub fn write_buffer_b(&self, data: &[u8], buffer_index: usize) -> usize {
        if buffer_index >= self.buffer_b_descriptors.len() || data.is_empty() {
            return 0;
        }
        let buffer_size = self.buffer_b_descriptors[buffer_index].size() as usize;
        let size = data.len().min(buffer_size);
        let address = self.buffer_b_descriptors[buffer_index].address();
        self.write_guest_memory(address, &data[..size]);
        trace_out_buf(address, &data[..size]);
        size
    }

    /// Write to buffer descriptor C at the given index.
    ///
    /// Matches upstream `HLERequestContext::WriteBufferC(const void*, size_t, size_t)`.
    pub fn write_buffer_c(&self, data: &[u8], buffer_index: usize) -> usize {
        if buffer_index >= self.buffer_c_descriptors.len() || data.is_empty() {
            return 0;
        }
        let buffer_size = self.buffer_c_descriptors[buffer_index].size() as usize;
        let size = data.len().min(buffer_size);
        let address = self.buffer_c_descriptors[buffer_index].address();
        self.write_guest_memory(address, &data[..size]);
        trace_out_buf(address, &data[..size]);
        size
    }

    /// Read `size` bytes from guest virtual address.
    /// Matches upstream `memory.ReadBlock(address, buffer, size)`.
    /// With per-process Memory, `self.memory` has its own stable
    /// `current_page_table` — no pinning needed.
    fn read_guest_memory(&self, address: u64, size: usize) -> Vec<u8> {
        if size == 0 || address == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u8; size];
        if let Some(ref memory) = self.memory {
            let accessible = memory.lock().unwrap().read_block_checked(address, &mut buf);
            if accessible {
                return buf;
            }
            log::warn!(
                "read_guest_memory: checked read filled inaccessible range addr={:#x} size={:#x}",
                address,
                size
            );
            return buf;
        }
        log::error!(
            "read_guest_memory: no Memory available for addr={:#x} size={:#x}",
            address,
            size
        );
        buf
    }

    /// Write bytes to guest virtual address.
    /// Matches upstream `memory.WriteBlock(address, buffer, size)`.
    fn write_guest_memory(&self, address: u64, data: &[u8]) {
        if data.is_empty() || address == 0 {
            return;
        }
        if self.trace_write_block_target(address, data.len()) {
            IPC_TRACE_CURRENT.with(|current| {
                let current = current.borrow();
                eprintln!(
                    "[IPC_WRITE_BLOCK_AT] service={} cmd={} ioctl=0x{:X} addr=0x{:016X} size={:#x}",
                    current.0,
                    current.1,
                    current.2,
                    address,
                    data.len()
                );
            });
        }
        if let Some(ref memory) = self.memory {
            // Upstream HLE writes call Core::Memory::Memory::WriteBlock, not
            // a rasterizer-bypassing path. Use the checked variant so
            // protected host pages report EFAULT instead of SIGSEGV while
            // preserving WriteBlock's rasterizer-write side effect.
            let accessible = memory.lock().unwrap().write_block_checked(address, data);
            if accessible {
                return;
            }
            log::warn!(
                "write_guest_memory: WriteBlock reported inaccessible range addr={:#x} size={:#x}",
                address,
                data.len()
            );
        } else {
            log::error!(
                "write_guest_memory: no Memory available for addr={:#x} size={:#x}",
                address,
                data.len()
            );
        }
    }

    fn trace_write_block_target(&self, address: u64, size: usize) -> bool {
        let Ok(spec) = std::env::var("RUZU_TRACE_WRITE_BLOCK_AT") else {
            return false;
        };
        let trimmed = spec.trim();
        let hex = trimmed
            .strip_prefix("0x")
            .or_else(|| trimmed.strip_prefix("0X"))
            .unwrap_or(trimmed);
        let Ok(target) = u64::from_str_radix(hex, 16) else {
            return false;
        };
        address <= target && target < address.saturating_add(size as u64)
    }

    /// Populates this context from the thread's TLS command buffer.
    ///
    /// Matches upstream `PopulateFromIncomingCommandBuffer` which reads from
    /// `thread->GetTlsAddress()`. When constructed with `new_with_thread`,
    /// reads directly from guest memory. Falls back to the provided buffer
    /// if no memory is available (test path).
    pub fn populate_from_incoming_command_buffer(&mut self, src_cmdbuf: &[u32]) {
        if !src_cmdbuf.is_empty() {
            let len = src_cmdbuf.len().min(ipc::COMMAND_BUFFER_LENGTH);
            self.cmd_buf[..len].copy_from_slice(&src_cmdbuf[..len]);
            if len < ipc::COMMAND_BUFFER_LENGTH {
                self.cmd_buf[len..].fill(0);
            }
        } else if let Some(ref memory) = self.memory {
            let m = memory.lock().unwrap();
            for i in 0..ipc::COMMAND_BUFFER_LENGTH {
                self.cmd_buf[i] = m.read_32(self.tls_address + (i as u64 * 4));
            }
        } else {
            // Test/fallback path: copy from provided buffer.
            let len = src_cmdbuf.len().min(ipc::COMMAND_BUFFER_LENGTH);
            self.cmd_buf[..len].copy_from_slice(&src_cmdbuf[..len]);
        }
        self.parse_command_buffer(true);
    }

    /// Parses the command buffer header fields.
    fn parse_command_buffer(&mut self, incoming: bool) {
        let mut index: usize = 0;

        // Parse command header (2 words).
        if index + 1 >= ipc::COMMAND_BUFFER_LENGTH {
            return;
        }
        let header = ipc::CommandHeader {
            raw_low: self.cmd_buf[index],
            raw_high: self.cmd_buf[index + 1],
        };
        index += 2;
        self.command_header = Some(header);

        if header.is_close_command() {
            return;
        }

        // Handle descriptor header.
        if header.enable_handle_descriptor() {
            if index >= ipc::COMMAND_BUFFER_LENGTH {
                return;
            }
            let hdh = ipc::HandleDescriptorHeader {
                raw: self.cmd_buf[index],
            };
            index += 1;
            self.handle_descriptor_header = Some(hdh);

            if hdh.send_current_pid() {
                // Upstream sets the client PID from the requesting thread's owner process
                // when the handle descriptor requests SendCurrentPid, then skips the
                // placeholder words in the incoming command buffer.
                self.pid = self
                    .thread
                    .as_ref()
                    .and_then(|thread| {
                        thread
                            .lock()
                            .unwrap()
                            .parent
                            .as_ref()
                            .and_then(Weak::upgrade)
                    })
                    .map(|process| process.lock().unwrap().get_process_id())
                    .unwrap_or(0);
                index += 2;
            }

            if incoming {
                let num_copy = hdh.num_handles_to_copy() as usize;
                let num_move = hdh.num_handles_to_move() as usize;
                self.incoming_copy_handles.clear();
                self.incoming_move_handles.clear();

                for _ in 0..num_copy {
                    if index < ipc::COMMAND_BUFFER_LENGTH {
                        self.incoming_copy_handles.push(self.cmd_buf[index]);
                        index += 1;
                    }
                }
                for _ in 0..num_move {
                    if index < ipc::COMMAND_BUFFER_LENGTH {
                        self.incoming_move_handles.push(self.cmd_buf[index]);
                        index += 1;
                    }
                }
            } else {
                let num_copy = hdh.num_handles_to_copy() as usize;
                let num_move = hdh.num_handles_to_move() as usize;
                index += num_copy + num_move;
            }
        }

        // Buffer descriptors X.
        let num_x = header.num_buf_x_descriptors() as usize;
        self.buffer_x_descriptors.clear();
        for _ in 0..num_x {
            if index + 1 < ipc::COMMAND_BUFFER_LENGTH {
                self.buffer_x_descriptors.push(ipc::BufferDescriptorX {
                    raw: self.cmd_buf[index],
                    address_bits_0_31: self.cmd_buf[index + 1],
                });
                index += 2;
            }
        }

        // Buffer descriptors A.
        let num_a = header.num_buf_a_descriptors() as usize;
        self.buffer_a_descriptors.clear();
        for _ in 0..num_a {
            if index + 2 < ipc::COMMAND_BUFFER_LENGTH {
                self.buffer_a_descriptors.push(ipc::BufferDescriptorABW {
                    size_bits_0_31: self.cmd_buf[index],
                    address_bits_0_31: self.cmd_buf[index + 1],
                    raw_word2: self.cmd_buf[index + 2],
                });
                index += 3;
            }
        }

        // Buffer descriptors B.
        let num_b = header.num_buf_b_descriptors() as usize;
        self.buffer_b_descriptors.clear();
        for _ in 0..num_b {
            if index + 2 < ipc::COMMAND_BUFFER_LENGTH {
                self.buffer_b_descriptors.push(ipc::BufferDescriptorABW {
                    size_bits_0_31: self.cmd_buf[index],
                    address_bits_0_31: self.cmd_buf[index + 1],
                    raw_word2: self.cmd_buf[index + 2],
                });
                index += 3;
            }
        }

        // Buffer descriptors W.
        let num_w = header.num_buf_w_descriptors() as usize;
        self.buffer_w_descriptors.clear();
        for _ in 0..num_w {
            if index + 2 < ipc::COMMAND_BUFFER_LENGTH {
                self.buffer_w_descriptors.push(ipc::BufferDescriptorABW {
                    size_bits_0_31: self.cmd_buf[index],
                    address_bits_0_31: self.cmd_buf[index + 1],
                    raw_word2: self.cmd_buf[index + 2],
                });
                index += 3;
            }
        }

        let buffer_c_offset = index + header.data_size() as usize;

        if !header.is_tipc() {
            // Upstream IPC::RequestParser::AlignWithPadding (ipc_helpers.h) zero-fills
            // the alignment padding directly in guest TLS via Skip(n, set_to_null=true).
            // Ruzu parses a local cmd_buf copy, so we mirror the side effect into TLS.
            if index & 3 != 0 {
                let pad_count = 4 - (index & 3);
                if incoming {
                    for i in 0..pad_count {
                        let target = index + i;
                        if target < ipc::COMMAND_BUFFER_LENGTH {
                            self.cmd_buf[target] = 0;
                        }
                    }
                    if let Some(ref memory) = self.memory {
                        let m = memory.lock().unwrap();
                        for i in 0..pad_count {
                            m.write_32_no_rasterizer(
                                self.tls_address + ((index + i) as u64 * 4),
                                0,
                            );
                        }
                    }
                }
                index += pad_count;
            }

            // Domain message header check.
            // For simplicity, we check the manager if available.
            let is_manager_domain = self
                .manager
                .as_ref()
                .map_or(false, |m| m.lock().unwrap().is_domain());

            if is_manager_domain
                && ((header.command_type() == ipc::CommandType::Request
                    || header.command_type() == ipc::CommandType::RequestWithContext)
                    || !incoming)
            {
                if incoming || self.domain_message_header.is_some() {
                    if index + 3 < ipc::COMMAND_BUFFER_LENGTH {
                        self.domain_message_header = Some(ipc::DomainMessageHeader {
                            raw: [
                                self.cmd_buf[index],
                                self.cmd_buf[index + 1],
                                self.cmd_buf[index + 2],
                                self.cmd_buf[index + 3],
                            ],
                        });
                        index += 4;
                    }
                }
            }

            // Data payload header (2 words).
            if index + 1 < ipc::COMMAND_BUFFER_LENGTH {
                self.data_payload_header = Some(ipc::DataPayloadHeader {
                    magic: self.cmd_buf[index],
                    _padding: self.cmd_buf[index + 1],
                });
                index += 2;
            }

            self.data_payload_offset = index as u32;

            // Check for CloseVirtualHandle (no further data).
            if let Some(ref dmh) = self.domain_message_header {
                if dmh.command() == ipc::DomainCommandType::CloseVirtualHandle {
                    return;
                }
            }
        }

        // Buffer descriptors C.
        let mut c_index = buffer_c_offset;
        self.buffer_c_descriptors.clear();
        let c_flags_raw = bit_field_extract(header.raw_high, 10, 4);
        if c_flags_raw > ipc::BufferDescriptorCFlag::InlineDescriptor as u32 {
            if c_flags_raw == ipc::BufferDescriptorCFlag::OneDescriptor as u32 {
                if c_index + 1 < ipc::COMMAND_BUFFER_LENGTH {
                    self.buffer_c_descriptors.push(ipc::BufferDescriptorC {
                        address_bits_0_31: self.cmd_buf[c_index],
                        raw_word1: self.cmd_buf[c_index + 1],
                    });
                }
            } else {
                // Number of C descriptors = flags - 2, matching upstream
                // `static_cast<u32>(buf_c_descriptor_flags.Value()) - 2`.
                let num_c = c_flags_raw.wrapping_sub(2) as usize;
                if num_c < 14 {
                    for _ in 0..num_c {
                        if c_index + 1 < ipc::COMMAND_BUFFER_LENGTH {
                            self.buffer_c_descriptors.push(ipc::BufferDescriptorC {
                                address_bits_0_31: self.cmd_buf[c_index],
                                raw_word1: self.cmd_buf[c_index + 1],
                            });
                            c_index += 2;
                        }
                    }
                }
            }
        }

        // Parse the command ID from the data payload.
        if !header.is_tipc() {
            let dp_offset = self.data_payload_offset as usize;
            if dp_offset < ipc::COMMAND_BUFFER_LENGTH {
                self.command = self.cmd_buf[dp_offset];
                // Skip 1 more word (command is u64, high part unused).
            }
        }
    }

    /// Writes data from this context back to the requesting thread's TLS.
    ///
    /// Matches upstream `HLERequestContext::WriteToOutgoingCommandBuffer()`:
    /// 1. Translate outgoing copy objects → handles via handle_table.Add()
    /// 2. Translate outgoing move objects → handles (+ close original)
    /// 3. Write domain object IDs into the buffer
    /// 4. Write the command buffer back to guest TLS memory
    pub fn write_to_outgoing_command_buffer(&mut self) -> ResultCode {
        let mut current_offset = self.handles_offset as usize;

        // Translate outgoing copy objects to handles.
        // Upstream: handle_table.Add(object) for each, writes handle to cmd_buf.
        for obj_ref in self.outgoing_copy_objects.clone() {
            if current_offset < ipc::COMMAND_BUFFER_LENGTH {
                let handle = self.resolve_ipc_object_handle(obj_ref).unwrap_or(0);
                self.cmd_buf[current_offset] = handle;
                current_offset += 1;
            }
        }

        // Translate outgoing move objects to handles.
        // Upstream: handle_table.Add(object) + object->Close() for each.
        for obj_ref in self.outgoing_move_objects.clone() {
            if current_offset < ipc::COMMAND_BUFFER_LENGTH {
                let handle = self.resolve_ipc_object_handle(obj_ref).unwrap_or(0);
                self.cmd_buf[current_offset] = handle;
                current_offset += 1;
            }
        }

        // Write domain objects to the command buffer (after raw untranslated data).
        // Matches upstream: domain objects go at domain_offset - count.
        let is_domain = self
            .manager
            .as_ref()
            .map_or(false, |m| m.lock().unwrap().is_domain());

        if is_domain {
            let num_domain_objects = self.outgoing_domain_objects.len();
            current_offset = self.domain_offset as usize - num_domain_objects;

            let domain_objects = std::mem::take(&mut self.outgoing_domain_objects);
            for object in domain_objects {
                if current_offset < ipc::COMMAND_BUFFER_LENGTH {
                    // Matches upstream hle_ipc.cpp:301-307:
                    // if (object) { AppendDomainHandler; cmd_buf = count; }
                    // else { cmd_buf = 0; }
                    if let Some(handler) = object {
                        if let Some(manager) = &self.manager {
                            manager.lock().unwrap().append_domain_handler(handler);
                            let count = manager.lock().unwrap().domain_handler_count();
                            self.cmd_buf[current_offset] = count as u32;
                            // RUZU_IPC_DOMAIN_OUT=1 — log domain object ID
                            // emission. If the client (libnx) thinks the
                            // session is non-domain, it will read this `count`
                            // value as a kernel handle (typically a small
                            // integer like 1, 2, 3) — directly producing the
                            // `handle 0x1 not in handle table` failure mode.
                            // domain_offset is the offset PAST the raw data
                            // section, so a non-domain client reads the value
                            // as the move/copy handle slot.
                            if common::trace::is_enabled(common::trace::cat::IPC_DOMAIN_OUT) {
                                let (svc, cmd, _ioctl) =
                                    IPC_TRACE_CURRENT.with(|c| c.borrow().clone());
                                let svc_str = if svc.is_empty() { "?" } else { svc.as_str() };
                                let svc_id = common::trace::intern_service(svc_str);
                                common::trace::emit_raw(
                                    common::trace::cat::IPC_DOMAIN_OUT,
                                    &[
                                        svc_id as u64,
                                        cmd as u64,
                                        current_offset as u64,
                                        count as u64,
                                    ],
                                );
                            }
                        } else {
                            self.cmd_buf[current_offset] = 0;
                        }
                    } else {
                        // Null domain object — write 0 as the domain object ID.
                        self.cmd_buf[current_offset] = 0;
                    }
                    current_offset += 1;
                }
            }
        }

        // Write the command buffer back to guest TLS memory.
        // Matches upstream: memory.WriteBlock(thread->GetTlsAddress(), cmd_buf.data(), write_size * sizeof(u32))
        let write_words = (self.write_size as usize).min(ipc::COMMAND_BUFFER_LENGTH);

        // IPC reply hex-dump routed through the non-blocking trace ring. Cap is
        // configurable so long-run investigations can include late IPC traffic.
        if common::trace::is_enabled(common::trace::cat::IPC_REPLY) {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static SEQ: AtomicUsize = AtomicUsize::new(0);
            let seq = SEQ.fetch_add(1, Ordering::Relaxed);
            if seq < ipc_trace_ring_limit() {
                let (svc, cmd, ioctl) = IPC_TRACE_CURRENT.with(|c| c.borrow().clone());
                let svc_str = if svc.is_empty() { "?" } else { svc.as_str() };
                let svc_id = common::trace::intern_service(svc_str);
                let n = write_words.min(9); // 14 - 5 fixed args = 9 payload words
                let mut args: [u64; 14] = [0; 14];
                args[0] = seq as u64;
                args[1] = svc_id as u64;
                args[2] = cmd as u64;
                args[3] = ioctl as u64;
                args[4] = write_words as u64;
                for i in 0..n {
                    args[5 + i] = self.cmd_buf[i] as u64;
                }
                let used = 5 + n;
                common::trace::emit_raw(common::trace::cat::IPC_REPLY, &args[..used]);
            }
        }

        if let Some(ref memory) = self.memory {
            let m = memory.lock().unwrap();
            for i in 0..write_words {
                m.write_32_no_rasterizer(self.tls_address + (i as u64 * 4), self.cmd_buf[i]);
            }
        }

        // Diagnostic mirror of zuyu's OUT_BUF_XDESC: dump the bytes at
        // X-desc[0] address after the response has been written to TLS.
        // Matches the trigger point in zuyu's `WriteToOutgoingCommandBuffer`.
        self.trace_out_buf_xdesc();

        RESULT_SUCCESS
    }

    fn resolve_ipc_object_handle(&self, obj_ref: KAutoObjectRef) -> Option<u32> {
        if let Some(handle) = obj_ref.as_handle() {
            // Silent provenance record for the InvalidHandle boot-abort
            // forensics (task #123) — no I/O, see handle_forensics.rs.
            if crate::hle::kernel::handle_forensics::enabled() {
                let (svc, cmd, _ioctl) = IPC_TRACE_CURRENT.with(|c| c.borrow().clone());
                crate::hle::kernel::handle_forensics::record_handle_out(
                    handle,
                    format!("raw service={} cmd={}", svc, cmd),
                );
            }
            // RUZU_IPC_HANDLE_OUT=1 — log every raw-handle path that bypasses
            // handle_table.Add. The handle is written verbatim to the client
            // IPC buffer; if it's not actually present in the client's table,
            // the client will see `handle 0x... not in handle table` on the
            // next SendSyncRequest. Surfacing all raw-handle outputs lets us
            // see which service pushed a bad value (or whether the bug is
            // elsewhere — e.g. domain object ID or raw payload mis-parsed).
            if common::trace::is_enabled(common::trace::cat::IPC_HANDLE_OUT) {
                let valid = self
                    .owner_process_arc()
                    .map(|p| p.lock().unwrap().handle_table.is_valid_handle(handle))
                    .unwrap_or(false);
                let (svc, cmd, _ioctl) = IPC_TRACE_CURRENT.with(|c| c.borrow().clone());
                let svc_str = if svc.is_empty() { "?" } else { svc.as_str() };
                let svc_id = common::trace::intern_service(svc_str);
                common::trace::emit_raw(
                    common::trace::cat::IPC_HANDLE_OUT,
                    &[
                        svc_id as u64,
                        cmd as u64,
                        handle as u64,
                        valid as u64,
                        0, // kind = raw
                        0,
                    ],
                );
            }
            return Some(handle);
        }

        let KAutoObjectRef::ObjectId(object_id) = obj_ref else {
            return None;
        };

        let process = self.owner_process_arc()?;
        let mut process = process.lock().unwrap();
        if process.ensure_handle_table_initialized() != RESULT_SUCCESS.get_inner_value() {
            return None;
        }
        let result = process.handle_table.add(object_id).ok();
        if crate::hle::kernel::handle_forensics::enabled() {
            if let Some(h) = result {
                let (svc, cmd, _ioctl) = IPC_TRACE_CURRENT.with(|c| c.borrow().clone());
                crate::hle::kernel::handle_forensics::record_handle_out(
                    h,
                    format!(
                        "add service={} cmd={} object_id=0x{:X}",
                        svc, cmd, object_id
                    ),
                );
            }
        }
        if common::trace::is_enabled(common::trace::cat::IPC_HANDLE_OUT) {
            let (svc, cmd, _ioctl) = IPC_TRACE_CURRENT.with(|c| c.borrow().clone());
            let svc_str = if svc.is_empty() { "?" } else { svc.as_str() };
            let svc_id = common::trace::intern_service(svc_str);
            let (new_handle, present) = match result {
                Some(h) => (h as u64, 1u64),
                None => (0u64, 0u64),
            };
            common::trace::emit_raw(
                common::trace::cat::IPC_HANDLE_OUT,
                &[
                    svc_id as u64,
                    cmd as u64,
                    object_id,
                    new_handle,
                    1, // kind = add
                    present,
                ],
            );
        }
        result
    }

    /// Returns a description of the current IPC command for debugging.
    pub fn description(&self) -> String {
        match &self.command_header {
            None => "No command header available".to_string(),
            Some(header) => {
                format!(
                    "IPC::CommandHeader: Type:{}, X(Pointer):{}, A(Send):{}, B(Receive):{}, \
                     C(ReceiveList):{}, data_size:{}",
                    header.command_type_raw(),
                    header.num_buf_x_descriptors(),
                    header.num_buf_a_descriptors(),
                    header.num_buf_b_descriptors(),
                    self.buffer_c_descriptors.len(),
                    header.data_size(),
                )
            }
        }
    }
}

/// Helper to extract unsigned bits from a value. Mirrors common::bit_field::extract_unsigned.
fn bit_field_extract(value: u32, position: usize, bits: usize) -> u32 {
    (value >> position) & ((1u32 << bits) - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::SystemRef;
    use crate::device_memory::DeviceMemory;
    use crate::hle::kernel::k_process::{KProcess, ProcessLock};
    use crate::hle::kernel::k_thread::{KThread, KThreadLock};
    use crate::memory::memory::Memory;
    use common::page_table::{PageTable, PageType};

    struct MappedTestMemory {
        _device_memory: Box<DeviceMemory>,
        _page_table: Box<PageTable>,
        memory: Arc<Mutex<Memory>>,
    }

    impl MappedTestMemory {
        fn new(mapped_start: u64, mapped_size: usize) -> Self {
            const PAGE_BITS: usize = 12;
            const PAGE_SIZE: usize = 1 << PAGE_BITS;

            let device_memory = Box::new(DeviceMemory::new());
            let buffer_ptr = &device_memory.buffer as *const common::host_memory::HostMemory;
            let memory = Arc::new(Mutex::new(unsafe {
                Memory::new(
                    SystemRef::null(),
                    device_memory.as_ref() as *const _,
                    buffer_ptr,
                )
            }));

            let mut page_table = Box::new(PageTable::new());
            page_table.resize(32, PAGE_BITS);
            let base_page = (mapped_start as usize) / PAGE_SIZE;
            let num_pages = mapped_size.div_ceil(PAGE_SIZE);
            let host_base = device_memory.buffer.backing_base_pointer() as usize;
            for page in base_page..base_page + num_pages {
                let target = (page * PAGE_SIZE) as u64;
                page_table.map_pages(
                    page,
                    1,
                    target,
                    PageType::Memory,
                    host_base + target as usize,
                );
            }

            memory
                .lock()
                .unwrap()
                .set_current_page_table(page_table.as_mut() as *mut PageTable, true);

            Self {
                _device_memory: device_memory,
                _page_table: page_table,
                memory,
            }
        }
    }

    #[test]
    fn test_session_request_manager_default() {
        let mgr = SessionRequestManager::new();
        assert!(!mgr.is_domain());
        assert!(!mgr.has_session_handler());
        assert!(!mgr.get_is_initialized_for_sm());
        assert_eq!(mgr.domain_handler_count(), 0);
    }

    #[test]
    fn test_hle_request_context_default() {
        let ctx = HLERequestContext::new();
        assert_eq!(ctx.cmd_buf[0], 0);
        assert!(!ctx.is_tipc());
        assert!(!ctx.has_domain_message_header());
        assert!(!ctx.get_is_deferred());
    }

    #[test]
    fn test_send_current_pid_uses_requesting_thread_process_id() {
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        process.lock().unwrap().process_id = 0x51;

        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));

        let tls_address = 0x2000;
        let mut ctx = HLERequestContext::new_with_thread(thread, tls_address);
        // No Memory bridge in this test — use the cmdbuf fallback path.
        ctx.populate_from_incoming_command_buffer(&[
            ipc::CommandType::Request as u32,
            1u32 << 31, // has_special_header
            1,          // send_pid = true
            0,
            0,
        ]);

        assert_eq!(ctx.get_pid(), 0x51);
    }

    #[test]
    fn new_with_thread_uses_owner_process_memory() {
        let fixture = MappedTestMemory::new(0x2000, 0x1000);
        let memory = fixture.memory.clone();

        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        process
            .lock()
            .unwrap()
            .page_table
            .set_memory(memory.clone());

        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));

        let ctx = HLERequestContext::new_with_thread(thread, 0x2000);

        assert!(ctx.memory.is_some());
        assert!(Arc::ptr_eq(ctx.memory.as_ref().unwrap(), &memory));
    }

    #[test]
    fn read_guest_memory_uses_memory_bridge() {
        let fixture = MappedTestMemory::new(0x3000, 0x1000);
        let memory = fixture.memory.clone();

        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        process
            .lock()
            .unwrap()
            .page_table
            .set_memory(memory.clone());

        memory.lock().unwrap().write_8(0x3000, 0x7a);

        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));

        let ctx = HLERequestContext::new_with_thread(thread, 0x2000);

        assert_eq!(ctx.read_guest_memory(0x3000, 1), vec![0x7a]);
    }

    #[test]
    fn write_guest_memory_uses_memory_ipc_write_block() {
        let fixture = MappedTestMemory::new(0x3000, 0x1000);
        let memory = fixture.memory.clone();

        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        process
            .lock()
            .unwrap()
            .page_table
            .set_memory(memory.clone());

        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));

        let ctx = HLERequestContext::new_with_thread(thread, 0x2000);
        ctx.write_guest_memory(0x3000, &[0x11, 0x22, 0x33, 0x44]);

        let mut out = [0u8; 4];
        memory.lock().unwrap().read_block(0x3000, &mut out);
        assert_eq!(out, [0x11, 0x22, 0x33, 0x44]);
    }

    #[test]
    fn write_buffer_prefers_b_descriptor_and_clamps_like_upstream() {
        let fixture = MappedTestMemory::new(0x3000, 0x1000);
        let memory = fixture.memory.clone();

        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        process
            .lock()
            .unwrap()
            .page_table
            .set_memory(memory.clone());

        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));

        let mut ctx = HLERequestContext::new_with_thread(thread, 0x2000);
        ctx.buffer_b_descriptors = vec![ipc::BufferDescriptorABW {
            size_bits_0_31: 2,
            address_bits_0_31: 0x3000,
            raw_word2: 0,
        }];
        ctx.buffer_c_descriptors = vec![ipc::BufferDescriptorC {
            address_bits_0_31: 0x3010,
            raw_word1: 4 << 16,
        }];

        assert_eq!(ctx.write_buffer(&[0xaa, 0xbb, 0xcc, 0xdd], 0), 2);

        let mut b_out = [0u8; 4];
        memory.lock().unwrap().read_block(0x3000, &mut b_out);
        assert_eq!(b_out, [0xaa, 0xbb, 0, 0]);

        let mut c_out = [0u8; 4];
        memory.lock().unwrap().read_block(0x3010, &mut c_out);
        assert_eq!(c_out, [0, 0, 0, 0]);
    }

    #[test]
    fn write_buffer_returns_zero_when_selected_c_descriptor_is_missing() {
        let mut ctx = HLERequestContext::new();
        ctx.buffer_b_descriptors.clear();
        ctx.buffer_c_descriptors.clear();

        assert_eq!(ctx.write_buffer(&[0x55], 0), 0);
    }

    #[test]
    fn cmif_out_array_write_back_zeroes_buffer_even_when_count_is_zero() {
        let fixture = MappedTestMemory::new(0x3000, 0x1000);
        let memory = fixture.memory.clone();

        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        process
            .lock()
            .unwrap()
            .page_table
            .set_memory(memory.clone());

        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));

        memory.lock().unwrap().write_block(0x3000, &[0xFF; 16]);

        let mut ctx = HLERequestContext::new_with_thread(thread, 0x2000);
        ctx.buffer_b_descriptors = vec![ipc::BufferDescriptorABW {
            size_bits_0_31: 16,
            address_bits_0_31: 0x3000,
            raw_word2: 0,
        }];

        let mut out_storage = crate::hle::service::cmif_serialization::CmifOutArrayBuffer::<
            u64,
            { crate::hle::service::cmif_types::buffer_attr::BufferAttr_HipcAutoSelect },
        >::from_ctx(&ctx, 0);
        let mut out = out_storage.as_out_array();
        out[0] = 0;
        out_storage.write_back(&ctx, 0, 0);

        let mut out_bytes = [0xFFu8; 16];
        memory.lock().unwrap().read_block(0x3000, &mut out_bytes);
        assert_eq!(out_bytes, [0; 16]);
    }

    #[test]
    fn create_session_for_service_queues_server_manager_registration() {
        let mut process = KProcess::new();
        process.initialize_handle_table();
        let process = Arc::new(ProcessLock::from_value(process));
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));

        let server_manager =
            crate::hle::service::server_manager::ServerManager::new_shared(SystemRef::null());
        let (queue, wakeup) = {
            let guard = server_manager.lock().unwrap();
            (guard.pending_registrations_arc(), guard.wakeup_event_arc())
        };
        let manager = Arc::new(Mutex::new(
            SessionRequestManager::new_with_server_manager_full(
                server_manager.clone(),
                queue.clone(),
                wakeup,
            ),
        ));

        let mut ctx = HLERequestContext::new_with_thread(thread, 0x2000);
        ctx.set_session_request_manager(manager);

        let handle = ctx
            .create_session_for_service(Arc::new(DummyHandler))
            .expect("client session handle");

        assert!(process.lock().unwrap().handle_table.is_valid_handle(handle));
        assert_eq!(queue.lock().unwrap().len(), 1);
    }

    #[test]
    fn session_request_manager_drops_incomplete_server_manager_endpoints() {
        let queue: PendingRegistrationQueue = Arc::new(Mutex::new(Vec::new()));
        let wakeup = Arc::new(crate::hle::service::os::event::Event::new());

        let manager = SessionRequestManager::from_parent_server_manager_endpoints(
            None,
            Some(queue),
            Some(wakeup),
        );

        assert!(manager.get_server_manager().is_none());
        assert!(manager.pending_registrations().is_none());
        assert!(manager.server_wakeup().is_none());
    }

    #[test]
    fn write_to_outgoing_command_buffer_only_writes_write_size_words() {
        let fixture = MappedTestMemory::new(0x3000, 0x1000);
        let memory = fixture.memory.clone();

        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        process
            .lock()
            .unwrap()
            .page_table
            .set_memory(memory.clone());

        let tls_address = 0x3000u64;
        memory
            .lock()
            .unwrap()
            .write_32(tls_address + 16, 0xdead_beef);

        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));

        let mut ctx = HLERequestContext::new_with_thread(thread, tls_address);
        ctx.write_size = 4;
        ctx.cmd_buf[0] = 0x1111_1111;
        ctx.cmd_buf[1] = 0x2222_2222;
        ctx.cmd_buf[2] = 0x3333_3333;
        ctx.cmd_buf[3] = 0x4444_4444;
        ctx.cmd_buf[4] = 0;

        assert_eq!(ctx.write_to_outgoing_command_buffer(), RESULT_SUCCESS);

        let m = memory.lock().unwrap();
        assert_eq!(m.read_32(tls_address), 0x1111_1111);
        assert_eq!(m.read_32(tls_address + 4), 0x2222_2222);
        assert_eq!(m.read_32(tls_address + 8), 0x3333_3333);
        assert_eq!(m.read_32(tls_address + 12), 0x4444_4444);
        assert_eq!(m.read_32(tls_address + 16), 0xdead_beef);
    }

    #[test]
    fn parse_command_buffer_keeps_multi_descriptor_c_receive_list() {
        let mut ctx = HLERequestContext::new();
        let mut cmd_buf = [0u32; ipc::COMMAND_BUFFER_LENGTH];

        let mut raw_high = 0u32;
        raw_high |= 4; // data_size
        raw_high |= 5 << 10; // 3 C descriptors encoded as flags raw value 5

        cmd_buf[0] = ipc::CommandType::Request as u32;
        cmd_buf[1] = raw_high;
        cmd_buf[4] = 0x4943_4653; // SFCI
        cmd_buf[6] = 0x1111_2222;
        cmd_buf[7] = (0x30u32 << 16) | 0x0003;
        cmd_buf[8] = 0x3333_4444;
        cmd_buf[9] = (0x40u32 << 16) | 0x0005;
        cmd_buf[10] = 0x5555_6666;
        cmd_buf[11] = (0x50u32 << 16) | 0x0007;

        ctx.populate_from_incoming_command_buffer(&cmd_buf);

        assert_eq!(ctx.buffer_descriptor_c().len(), 3);
        assert_eq!(ctx.buffer_descriptor_c()[0].address(), 0x0003_1111_2222);
        assert_eq!(ctx.buffer_descriptor_c()[0].size(), 0x30);
        assert_eq!(ctx.buffer_descriptor_c()[1].address(), 0x0005_3333_4444);
        assert_eq!(ctx.buffer_descriptor_c()[1].size(), 0x40);
        assert_eq!(ctx.buffer_descriptor_c()[2].address(), 0x0007_5555_6666);
        assert_eq!(ctx.buffer_descriptor_c()[2].size(), 0x50);
    }

    #[test]
    fn parse_tipc_command_buffer_keeps_c_receive_descriptor() {
        let mut ctx = HLERequestContext::new();
        let mut cmd_buf = [0u32; ipc::COMMAND_BUFFER_LENGTH];

        cmd_buf[0] = ipc::CommandType::TipcCommandRegion as u32;
        cmd_buf[1] = (ipc::BufferDescriptorCFlag::OneDescriptor as u32) << 10;
        cmd_buf[2] = 0x1111_2222;
        cmd_buf[3] = (0x30u32 << 16) | 0x0003;

        ctx.populate_from_incoming_command_buffer(&cmd_buf);

        assert!(ctx.is_tipc());
        assert_eq!(ctx.get_command(), 0);
        assert_eq!(ctx.buffer_descriptor_c().len(), 1);
        assert_eq!(ctx.buffer_descriptor_c()[0].address(), 0x0003_1111_2222);
        assert_eq!(ctx.buffer_descriptor_c()[0].size(), 0x30);
    }

    struct DummyHandler;

    impl SessionRequestHandler for DummyHandler {
        fn handle_sync_request(&self, _context: &mut HLERequestContext) -> ResultCode {
            RESULT_SUCCESS
        }

        fn service_name(&self) -> &str {
            "DummyHandler"
        }
    }

    struct LockingNameHandler {
        gate: Arc<Mutex<()>>,
    }

    impl SessionRequestHandler for LockingNameHandler {
        fn handle_sync_request(&self, _context: &mut HLERequestContext) -> ResultCode {
            RESULT_SUCCESS
        }

        fn service_name(&self) -> &str {
            let _guard = self.gate.lock().unwrap();
            "LockingNameHandler"
        }
    }

    #[test]
    fn domain_dispatch_selection_does_not_lock_the_service_handler() {
        let gate = Arc::new(Mutex::new(()));
        let gate_guard = gate.lock().unwrap();
        let handler: SessionRequestHandlerPtr = Arc::new(LockingNameHandler {
            gate: Arc::clone(&gate),
        });
        let manager = Arc::new(Mutex::new(SessionRequestManager::new()));
        {
            let mut manager_guard = manager.lock().unwrap();
            manager_guard.set_session_handler(handler);
            manager_guard.convert_to_domain();
        }

        let mut context = HLERequestContext::new();
        let mut domain_header = ipc::DomainMessageHeader::default();
        domain_header.set_command(ipc::DomainCommandType::SendMessage);
        domain_header.set_object_id(1);
        context.domain_message_header = Some(domain_header);

        let (completed_tx, completed_rx) = std::sync::mpsc::channel();
        let manager_for_thread = Arc::clone(&manager);
        let worker = std::thread::spawn(move || {
            let dispatch = manager_for_thread
                .lock()
                .unwrap()
                .prepare_sync_request(&context);
            completed_tx
                .send(matches!(dispatch, PreparedSyncRequest::Domain(_)))
                .unwrap();
        });

        let completed_without_service_lock = completed_rx
            .recv_timeout(std::time::Duration::from_millis(250))
            .unwrap_or(false);
        drop(gate_guard);
        worker.join().unwrap();

        assert!(
            completed_without_service_lock,
            "selecting a domain handler while holding the session-manager lock must not call into the service"
        );
    }

    #[test]
    fn close_virtual_handle_does_not_write_back_tls() {
        let fixture = MappedTestMemory::new(0x3000, 0x1000);
        let memory = fixture.memory.clone();

        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        process
            .lock()
            .unwrap()
            .page_table
            .set_memory(memory.clone());

        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));

        let tls_address = 0x3000u64;
        let request_words = [
            0x0000_0004u32,
            0x0000_0008,
            0,
            0,
            0x0000_0002,
            0x0000_0002,
            0,
            0,
            0x4f43_4653,
            0,
            0x0007_d402,
            0,
            0,
            0,
            0,
            0,
        ];
        {
            let mem = memory.lock().unwrap();
            for (i, word) in request_words.iter().copied().enumerate() {
                mem.write_32(tls_address + (i as u64 * 4), word);
            }
        }

        let manager = Arc::new(Mutex::new(SessionRequestManager::new()));
        {
            let mut guard = manager.lock().unwrap();
            guard.set_session_handler(Arc::new(DummyHandler));
            guard.convert_to_domain();
            guard.append_domain_handler(Arc::new(DummyHandler));
            guard.append_domain_handler(Arc::new(DummyHandler));
        }

        let mut ctx = HLERequestContext::new_with_thread(thread, tls_address);
        ctx.set_session_request_manager(manager.clone());
        ctx.populate_from_incoming_command_buffer(&request_words);

        assert_eq!(complete_sync_request(&manager, &mut ctx), RESULT_SUCCESS);

        let mem = memory.lock().unwrap();
        for (i, word) in request_words.iter().copied().enumerate() {
            assert_eq!(mem.read_32(tls_address + (i as u64 * 4)), word);
        }
        assert!(manager.lock().unwrap().domain_handler(1).is_none());
    }
}
