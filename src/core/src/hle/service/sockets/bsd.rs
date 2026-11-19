// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/sockets/bsd.h
//! Port of zuyu/src/core/hle/service/sockets/bsd.cpp
//!
//! BSD socket service -- "bsd:u" and "bsd:s".

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::sockets::{
    Domain, Errno, FcntlCmd, Linger, OptName, PollEvents, PollFD, Protocol, ShutdownHow,
    SockAddrIn, SocketLevel, Type,
};
use super::sockets_translate::{
    translate_domain, translate_errno, translate_poll_events, translate_poll_events_from_network,
    translate_protocol, translate_result, translate_shutdown_how, translate_sockaddr_from_network,
    translate_sockaddr_to_network, translate_type,
};
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};
use crate::internal_network::network::{
    Errno as NetErrno, PollEvents as NetPollEvents, SockAddrIn as NetSockAddrIn,
};
use crate::internal_network::sockets::{self as net_sockets, Socket, SocketBase};

/// Maximum number of file descriptors.
///
/// Corresponds to `MAX_FD` in upstream bsd.h.
pub const MAX_FD: usize = 128;

/// Non-blocking socket flag.
///
/// Corresponds to `Network::FLAG_O_NONBLOCK` used in upstream bsd.cpp.
pub const FLAG_O_NONBLOCK: i32 = 0x800;

/// MSG_DONTWAIT flag.
///
/// Corresponds to `Network::FLAG_MSG_DONTWAIT` used in upstream bsd.cpp.
pub const FLAG_MSG_DONTWAIT: u32 = 0x80;

/// IPC command table for BSD.
///
/// Corresponds to the function table in upstream bsd.cpp constructor.
pub mod commands {
    pub const REGISTER_CLIENT: u32 = 0;
    pub const START_MONITORING: u32 = 1;
    pub const SOCKET: u32 = 2;
    pub const SOCKET_EXEMPT: u32 = 3;
    pub const OPEN: u32 = 4;
    pub const SELECT: u32 = 5;
    pub const POLL: u32 = 6;
    pub const SYSCTL: u32 = 7;
    pub const RECV: u32 = 8;
    pub const RECV_FROM: u32 = 9;
    pub const SEND: u32 = 10;
    pub const SEND_TO: u32 = 11;
    pub const ACCEPT: u32 = 12;
    pub const BIND: u32 = 13;
    pub const CONNECT: u32 = 14;
    pub const GET_PEER_NAME: u32 = 15;
    pub const GET_SOCK_NAME: u32 = 16;
    pub const GET_SOCK_OPT: u32 = 17;
    pub const LISTEN: u32 = 18;
    pub const IOCTL: u32 = 19;
    pub const FCNTL: u32 = 20;
    pub const SET_SOCK_OPT: u32 = 21;
    pub const SHUTDOWN: u32 = 22;
    pub const SHUTDOWN_ALL_SOCKETS: u32 = 23;
    pub const WRITE: u32 = 24;
    pub const READ: u32 = 25;
    pub const CLOSE: u32 = 26;
    pub const DUPLICATE_SOCKET: u32 = 27;
    pub const GET_RESOURCE_STATISTICS: u32 = 28;
    pub const RECV_MMSG: u32 = 29;
    pub const SEND_MMSG: u32 = 30;
    pub const EVENT_FD: u32 = 31;
    pub const REGISTER_RESOURCE_STATISTICS_NAME: u32 = 32;
    pub const INITIALIZE2: u32 = 33;
}

/// BSDCFG IPC command table.
///
/// Corresponds to the function table in upstream bsd.cpp `BSDCFG` constructor.
pub mod bsdcfg_commands {
    pub const SET_IF_UP: u32 = 0;
    pub const SET_IF_UP_WITH_EVENT: u32 = 1;
    pub const CANCEL_IF: u32 = 2;
    pub const SET_IF_DOWN: u32 = 3;
    pub const GET_IF_STATE: u32 = 4;
    pub const DHCP_RENEW: u32 = 5;
    pub const ADD_STATIC_ARP_ENTRY: u32 = 6;
    pub const REMOVE_ARP_ENTRY: u32 = 7;
    pub const LOOKUP_ARP_ENTRY: u32 = 8;
    pub const LOOKUP_ARP_ENTRY2: u32 = 9;
    pub const CLEAR_ARP_ENTRIES: u32 = 10;
    pub const CLEAR_ARP_ENTRIES2: u32 = 11;
    pub const PRINT_ARP_ENTRIES: u32 = 12;
    pub const UNKNOWN13: u32 = 13;
    pub const UNKNOWN14: u32 = 14;
    pub const UNKNOWN15: u32 = 15;
}

/// Helper: check if a socket type is connection-based.
///
/// Corresponds to `IsConnectionBased` in upstream bsd.cpp.
fn is_connection_based(ty: Type) -> bool {
    match ty {
        Type::STREAM => true,
        Type::DGRAM => false,
        _ => {
            log::warn!("Unimplemented socket type={:?}", ty);
            false
        }
    }
}

fn socket_blocked_by_airplane_mode(airplane_mode: bool, connection_based: bool) -> bool {
    airplane_mode && connection_based
}

/// Counter behind an eventfd. Port of Fritchii BSD_USA::EventFdState.
/// The counter lock also serializes version changes with readiness snapshots.
#[derive(Default)]
struct EventFdState {
    inner: Mutex<EventFdInner>,
    version: std::sync::atomic::AtomicU64,
    status_flags: std::sync::atomic::AtomicI32,
    semaphore: bool,
}
#[derive(Default)]
struct EventFdInner { counter: u64, closed: bool }
impl EventFdState {
    const FLAG_SEMAPHORE: u32 = 1;
    const FLAG_NONBLOCK: u32 = 4;
    const MAX_COUNTER: u64 = u64::MAX - 1;
    fn is_non_blocking(&self) -> bool {
        self.status_flags.load(std::sync::atomic::Ordering::Relaxed) & FLAG_O_NONBLOCK != 0
    }
    fn read(&self, message: &mut [u8], blocked_version: &mut u64) -> (i32, Errno) {
        use std::sync::atomic::Ordering::Relaxed;
        if message.len() < 8 { return (-1, Errno::INVAL); }
        let mut inner = self.inner.lock().unwrap();
        if inner.closed { return (-1, Errno::BADF); }
        if inner.counter == 0 { *blocked_version = self.version.load(Relaxed); return (-1, Errno::AGAIN); }
        let value = if self.semaphore {1} else {inner.counter};
        inner.counter -= value; self.version.fetch_add(1, Relaxed);
        message[..8].copy_from_slice(&value.to_le_bytes()); (8, Errno::SUCCESS)
    }
    fn write(&self, message: &[u8], blocked_version: &mut u64) -> (i32, Errno) {
        use std::sync::atomic::Ordering::Relaxed;
        if message.len() < 8 { return (-1, Errno::INVAL); }
        let value = u64::from_le_bytes(message[..8].try_into().unwrap());
        if value == u64::MAX { return (-1, Errno::INVAL); }
        let mut inner = self.inner.lock().unwrap();
        if inner.closed { return (-1, Errno::BADF); }
        if value > Self::MAX_COUNTER - inner.counter {
            *blocked_version = self.version.load(Relaxed); return (-1, Errno::AGAIN);
        }
        inner.counter += value; self.version.fetch_add(1, Relaxed); (8, Errno::SUCCESS)
    }
    fn readiness(&self) -> (u64, bool, bool) {
        let inner = self.inner.lock().unwrap();
        (self.version.load(std::sync::atomic::Ordering::Relaxed), inner.counter != 0, inner.counter < Self::MAX_COUNTER)
    }
    fn descriptor_closed(&self) {
        let _inner = self.inner.lock().unwrap(); self.version.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    fn close(&self) { self.inner.lock().unwrap().closed = true; }
}

/// Per-file-descriptor state.
///
/// Corresponds to `BSD::FileDescriptor` in upstream bsd.h.
#[derive(Clone, Default)]
pub struct FileDescriptor {
    /// Platform socket (corresponds to upstream shared_ptr<SocketBase>).
    pub socket: Option<Arc<Mutex<Box<dyn SocketBase>>>>,
    event_fd: Option<Arc<EventFdState>>,
    socket_closed: bool,
    receive_timeout: u32,
    send_timeout: u32,
    pub flags: i32,
    pub is_connection_based: bool,
}

/// Fritchii BSD descriptor table, generations and mutex. Shared by BSD services
/// of one emulation session instead of a process-global static.
pub(super) struct DescriptorTable {
    entries: [Option<FileDescriptor>; MAX_FD],
    generations: [u64; MAX_FD],
}
impl Default for DescriptorTable {
    fn default() -> Self { Self { entries: std::array::from_fn(|_| None), generations: [0; MAX_FD] } }
}
impl DescriptorTable {
    fn find_free_file_descriptor_handle(&self) -> i32 {
        self.entries.iter().position(Option::is_none).map_or(-1, |fd| fd as i32)
    }
    fn find_open_descriptor(&self, fd: i32) -> Option<&FileDescriptor> {
        let descriptor = self.entries.get(usize::try_from(fd).ok()?)?.as_ref()?;
        (descriptor.event_fd.is_some() || (descriptor.socket.is_some() && !descriptor.socket_closed)).then_some(descriptor)
    }
}

// Fritchii SelectClock uses a monotonic clock. None represents time_point::max().
#[derive(Default)]
struct SelectRequest {
    nfds: usize,
    fd_sets: [Vec<u8>; 3],
    pollfds: Vec<PollFD>,
    deadline: Option<std::time::Instant>,
    file_descriptor_generation: u64,
}
#[derive(Default)]
struct SelectWait {
    entries: Vec<SelectWaitEntry>,
    recheck: Option<std::time::Instant>,
}
struct SelectWaitEntry {
    fd: i32,
    socket: Option<Arc<Mutex<Box<dyn SocketBase>>>>,
    event_fd: Option<Arc<EventFdState>>,
    event_fd_version: u64,
    generation: Option<u64>,
    events: NetPollEvents,
    emulated: bool,
}
struct DeferredSelect {
    request: SelectRequest,
    wait: SelectWait,
    id: u64,
    armed: bool,
}
struct SelectNotifierState {
    wake_socket: Socket,
    requests: BTreeMap<usize, DeferredSelect>,
    last_id: u64,
    polled_sockets: Vec<Arc<Mutex<Box<dyn SocketBase>>>>,
    poll_epoch: u64,
    polling: bool,
    wake_pending: bool,
    stopped: bool,
}
struct SelectNotifierInner {
    state: Mutex<SelectNotifierState>,
    poll_done: std::sync::Condvar,
    table: Arc<Mutex<DescriptorTable>>,
    signal: Arc<dyn Fn() + Send + Sync>,
}
/// Fritchii BSD_USA::SelectNotifier. The worker owns only Inner; the service-owned
/// outer object stops/joins it on Drop without an Arc cycle.
pub(super) struct SelectNotifier {
    inner: Arc<SelectNotifierInner>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}
fn fd_is_set(bytes: &[u8], fd: usize) -> bool {
    bytes.get(fd / 8).is_some_and(|byte| byte & (1 << (fd % 8)) != 0)
}
fn open_signal_socket(socket: &mut Socket) -> NetErrno {
    use crate::internal_network::network as net;
    let errno = socket.initialize(net::Domain::INET, net::Type::DGRAM, net::Protocol::UDP);
    if errno != NetErrno::Success { return errno; }
    let errno = socket.bind(NetSockAddrIn { family: Some(net::Domain::INET), ip: [127,0,0,1], portno: 0 });
    if errno != NetErrno::Success { return errno; }
    let (address, errno) = socket.get_sock_name();
    if errno != NetErrno::Success { return errno; }
    let errno = socket.connect(address);
    if errno != NetErrno::Success { return errno; }
    socket.set_non_block(true)
}
impl SelectNotifier {
    fn new(
        table: Arc<Mutex<DescriptorTable>>, signal: Arc<dyn Fn() + Send + Sync>,
        spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> std::thread::JoinHandle<()>,
    ) -> Arc<Self> {
        let mut wake_socket = Socket::new();
        let errno = open_signal_socket(&mut wake_socket);
        let stopped = errno != NetErrno::Success;
        if stopped { log::error!("Failed to open BSD Select wake socket: {errno:?}"); }
        let inner = Arc::new(SelectNotifierInner {
            state: Mutex::new(SelectNotifierState { wake_socket, requests: BTreeMap::new(), last_id: 0,
                polled_sockets: Vec::new(), poll_epoch: 0, polling: false, wake_pending: false, stopped }),
            poll_done: std::sync::Condvar::new(), table, signal,
        });
        let thread = if stopped { None } else {
            let worker = Arc::clone(&inner);
            Some(spawn(Box::new(move || worker.run())))
        };
        Arc::new(Self { inner, thread: Mutex::new(thread) })
    }
    fn take(&self, ctx: &HLERequestContext) -> Option<SelectRequest> {
        self.inner.state.lock().unwrap().requests.remove(&ctx.request_key()).map(|deferred| deferred.request)
    }
    fn defer(&self, ctx: &HLERequestContext, request: SelectRequest, wait: SelectWait) -> bool {
        let mut state = self.inner.state.lock().unwrap();
        if state.stopped { return false; }
        state.last_id = state.last_id.wrapping_add(1);
        let id = state.last_id;
        state.requests.insert(ctx.request_key(), DeferredSelect { request, wait, id, armed: true });
        SelectNotifierInner::wake(&mut state);
        true
    }
    fn cancel(&self, ctx: &HLERequestContext) {
        let mut state = self.inner.state.lock().unwrap();
        if state.requests.remove(&ctx.request_key()).is_some_and(|deferred| deferred.armed) {
            SelectNotifierInner::wake(&mut state);
        }
    }
    fn notify_change(&self, closing: Option<&Arc<Mutex<Box<dyn SocketBase>>>>) {
        let mut state = self.inner.state.lock().unwrap();
        if !state.polling { return; }
        let polls_closing = closing.is_some_and(|closing| state.polled_sockets.iter().any(|socket| Arc::ptr_eq(closing,socket)));
        if !polls_closing && !state.requests.values().any(|deferred| deferred.armed) { return; }
        let epoch = state.poll_epoch;
        SelectNotifierInner::wake(&mut state);
        while polls_closing && state.poll_epoch == epoch {
            state = self.inner.poll_done.wait_timeout(state, std::time::Duration::from_millis(50)).unwrap().0;
            SelectNotifierInner::wake(&mut state);
        }
    }
}
impl Drop for SelectNotifier {
    fn drop(&mut self) {
        {
            let mut state = self.inner.state.lock().unwrap();
            state.stopped = true;
            SelectNotifierInner::wake(&mut state);
        }
        if let Some(thread) = self.thread.lock().unwrap().take() { thread.join().expect("BSD Select notifier panicked"); }
        // Arc event/process references held by signal are released after the worker joins.
    }
}
impl SelectNotifierInner {
    fn wake(state: &mut SelectNotifierState) {
        if !state.polling || state.wake_pending { return; }
        let (_, errno) = state.wake_socket.send(&[0], 0);
        state.wake_pending = errno == NetErrno::Success || errno == NetErrno::Again;
        if !state.wake_pending { log::error!("Failed to wake BSD Select poll: {errno:?}"); }
    }
    fn is_current(table: &DescriptorTable, wait: &SelectWait) -> bool {
        wait.entries.iter().all(|entry| {
            let Some(descriptor) = table.find_open_descriptor(entry.fd) else { return false; };
            let same_socket = match (&descriptor.socket, &entry.socket) { (None,None) => true, (Some(a),Some(b)) => Arc::ptr_eq(a,b), _ => false };
            let same_event = match (&descriptor.event_fd, &entry.event_fd) { (None,None) => true, (Some(a),Some(b)) => Arc::ptr_eq(a,b), _ => false };
            same_socket && same_event
                && entry.generation.is_none_or(|generation| table.generations[entry.fd as usize] == generation)
                && entry.event_fd.as_ref().is_none_or(|event| event.version.load(std::sync::atomic::Ordering::Relaxed) == entry.event_fd_version)
        })
    }
    fn has_emulated_ready(wait: &SelectWait) -> bool {
        wait.entries.iter().any(|entry| entry.emulated && !entry.events.is_empty()
            && entry.socket.as_ref().is_some_and(|socket| socket.lock().unwrap().emulated_poll(entry.events).is_some_and(|events| !events.is_empty())))
    }
    fn run(&self) {
        use std::time::{Duration, Instant};
        let mut state = self.state.lock().unwrap();
        loop {
            if crate::internal_network::network::socket_operations_cancelled() { state.stopped = true; }
            let now = Instant::now();
            let stopped = state.stopped;
            let mut wake_at: Option<Instant> = None;
            let mut retry = false;
            let mut pollfds = Vec::new();
            let mut polled_for = Vec::new();
            let mut polled_sockets = Vec::new();
            state.polled_sockets.clear();
            {
                let table = self.table.lock().unwrap();
                for (&key, deferred) in &mut state.requests {
                    if !deferred.armed { continue; }
                    if stopped || deferred.request.deadline.is_some_and(|at| now >= at)
                        || deferred.wait.recheck.is_some_and(|at| now >= at)
                        || !Self::is_current(&table, &deferred.wait) || Self::has_emulated_ready(&deferred.wait) {
                        deferred.armed = false; retry = true; continue;
                    }
                    for at in [deferred.request.deadline, deferred.wait.recheck].into_iter().flatten() {
                        wake_at = Some(wake_at.map_or(at, |old| old.min(at)));
                    }
                    for entry in &deferred.wait.entries {
                        if entry.emulated || entry.events.is_empty() { continue; }
                        let socket = entry.socket.as_ref().expect("native wait has no socket");
                        pollfds.push(net_sockets::PollFD { fd: socket.lock().unwrap().get_fd(), events: entry.events.bits(), revents: 0 });
                        polled_for.push((key, deferred.id));
                        polled_sockets.push(Arc::clone(socket));
                    }
                }
            }
            if retry { drop(state); (self.signal)(); state = self.state.lock().unwrap(); continue; }
            if stopped { return; }
            state.polled_sockets = polled_sockets;
            pollfds.push(net_sockets::PollFD { fd: state.wake_socket.get_fd(), events: NetPollEvents::IN.bits(), revents: 0 });
            let timeout = wake_at.map_or(-1, |at| at.saturating_duration_since(now).as_nanos().div_ceil(1_000_000).min(i32::MAX as u128) as i32);
            state.polling = true;
            drop(state);
            let (polled, errno) = net_sockets::poll(&mut pollfds, timeout);
            state = self.state.lock().unwrap();
            state.polling = false;
            state.poll_epoch = state.poll_epoch.wrapping_add(1);
            state.polled_sockets.clear();
            self.poll_done.notify_all();
            let wake_revents = NetPollEvents::from_bits_retain(pollfds.last().unwrap().revents);
            if wake_revents.intersects(NetPollEvents::IN) {
                let mut sink = [0;16];
                while state.wake_socket.recv(0, &mut sink).0 > 0 {}
            }
            state.wake_pending = false;
            let failed = polled < 0 || wake_revents.intersects(NetPollEvents::ERR | NetPollEvents::HUP | NetPollEvents::NVAL);
            if failed { log::error!("BSD Select host poll failed: {errno:?}"); }
            for (index, (key, id)) in polled_for.into_iter().enumerate() {
                if !failed && pollfds[index].revents == 0 { continue; }
                if let Some(deferred) = state.requests.get_mut(&key) {
                    if deferred.armed && deferred.id == id { deferred.armed = false; retry = true; }
                }
            }
            if retry || failed {
                drop(state);
                if retry { (self.signal)(); }
                if failed { std::thread::sleep(Duration::from_millis(50)); }
                state = self.state.lock().unwrap();
            }
        }
    }
}

/// Work structs for async operations.
///
/// Corresponds to PollWork, AcceptWork, ConnectWork, RecvWork, RecvFromWork,
/// SendWork, SendToWork in upstream bsd.h.
/// In this port, work is executed synchronously (matching upstream ExecuteWork pattern).

/// BSD socket service.
///
/// Corresponds to `BSD` in upstream bsd.h / bsd.cpp.
pub struct Bsd {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
    file_descriptors: Arc<Mutex<DescriptorTable>>,
    select_notifier: Option<Arc<SelectNotifier>>,
    name: &'static str,
    is_user: bool,
}

impl Bsd {
    pub fn new(name: &'static str, is_user: bool) -> Self {
        Self::with_descriptor_table(name, is_user, Arc::new(Mutex::new(DescriptorTable::default())))
    }

    pub(super) fn with_descriptor_table(name: &'static str, is_user: bool, file_descriptors: Arc<Mutex<DescriptorTable>>) -> Self {
        let handlers = build_handler_map(&[
            (0, Some(Bsd::register_client_handler), "RegisterClient"),
            (1, Some(Bsd::start_monitoring_handler), "StartMonitoring"),
            (2, Some(Bsd::socket_handler), "Socket"),
            (3, Some(Bsd::socket_exempt_handler), "SocketExempt"),
            (4, None, "Open"),
            (5, Some(Bsd::select_handler), "Select"),
            (6, Some(Bsd::poll_handler), "Poll"),
            (7, None, "Sysctl"),
            (8, Some(Bsd::recv_handler), "Recv"),
            (9, Some(Bsd::recv_from_handler), "RecvFrom"),
            (10, Some(Bsd::send_handler), "Send"),
            (11, Some(Bsd::send_to_handler), "SendTo"),
            (12, Some(Bsd::accept_handler), "Accept"),
            (13, Some(Bsd::bind_handler), "Bind"),
            (14, Some(Bsd::connect_handler), "Connect"),
            (15, Some(Bsd::get_peer_name_handler), "GetPeerName"),
            (16, Some(Bsd::get_sock_name_handler), "GetSockName"),
            (17, Some(Bsd::get_sock_opt_handler), "GetSockOpt"),
            (18, Some(Bsd::listen_handler), "Listen"),
            (19, None, "Ioctl"),
            (20, Some(Bsd::fcntl_handler), "Fcntl"),
            (21, Some(Bsd::set_sock_opt_handler), "SetSockOpt"),
            (22, Some(Bsd::shutdown_handler), "Shutdown"),
            (23, None, "ShutdownAllSockets"),
            (24, Some(Bsd::write_handler), "Write"),
            (25, Some(Bsd::read_handler), "Read"),
            (26, Some(Bsd::close_handler), "Close"),
            (27, Some(Bsd::duplicate_socket_handler), "DuplicateSocket"),
            (28, None, "GetResourceStatistics"),
            (29, None, "RecvMMsg"),
            (30, None, "SendMMsg"),
            (31, Some(Bsd::event_fd_handler), "EventFd"),
            (32, None, "RegisterResourceStatisticsName"),
            (33, None, "Initialize2"),
            // Fritchii BSD_USA: NX2 registration has the same result + errno reply.
            (36, Some(Bsd::register_client_handler), "RegisterClientSwitch2"),
        ]);

        Self {
            handlers,
            handlers_tipc: BTreeMap::new(),
            file_descriptors,
            select_notifier: None,
            name,
            is_user,
        }
    }

    pub(super) fn create_select_notifier(system: crate::core::SystemRef, manager: &mut crate::hle::service::server_manager::ServerManager, table: Arc<Mutex<DescriptorTable>>) -> Arc<SelectNotifier> {
        let (result, event) = manager.manage_deferral();
        assert_eq!(result, RESULT_SUCCESS);
        let event = event.expect("BSD deferral event requires a process");
        let kernel = system.get().kernel().expect("BSD notifier requires kernel");
        let owner = event.lock().unwrap().owner_process_id.unwrap();
        let process = kernel.get_process_by_id(owner).expect("BSD deferral owner missing");
        let signal = Arc::new(move || { crate::hle::kernel::k_event::KEvent::signal_arc(&event, &process); });
        let notifier = SelectNotifier::new(table, signal, |task| kernel.run_on_host_core_thread("bsdsocket:select", task));
        let weak = Arc::downgrade(&notifier);
        manager.set_deferral_cancel_handler(Arc::new(move |ctx| { if let Some(notifier) = weak.upgrade() { notifier.cancel(ctx); } }));
        notifier
    }

    pub(super) fn with_notifier(name: &'static str, is_user: bool, table: Arc<Mutex<DescriptorTable>>, notifier: Arc<SelectNotifier>) -> Self {
        let mut bsd = Self::with_descriptor_table(name, is_user, table);
        bsd.select_notifier = Some(notifier);
        bsd
    }

    /// Returns whether this is a privileged (bsd:s) instance.
    pub fn is_privileged(&self) -> bool {
        !self.is_user
    }

    // --- Internal implementation methods ---

    /// Find the first free file descriptor slot.
    ///
    /// Corresponds to `BSD::FindFreeFileDescriptorHandle` in upstream bsd.cpp.
    fn copy_file_descriptor(&self, fd: i32) -> Option<(FileDescriptor, u64)> {
        let table = self.file_descriptors.lock().unwrap();
        Some((table.find_open_descriptor(fd)?.clone(), table.generations[fd as usize]))
    }

    fn copy_descriptor(&self, fd: i32) -> Option<FileDescriptor> {
        let (descriptor, _) = self.copy_file_descriptor(fd)?;
        descriptor.socket.as_ref()?;
        Some(descriptor)
    }

    /// Build a standard errno response.
    ///
    /// Corresponds to `BSD::BuildErrnoResponse` in upstream bsd.cpp.
    /// Returns (ret, errno) where ret is 0 on success, -1 on error.
    pub fn build_errno_response(bsd_errno: Errno) -> (i32, Errno) {
        let ret = if bsd_errno == Errno::SUCCESS { 0 } else { -1 };
        (ret, bsd_errno)
    }

    /// SocketImpl -- create a new socket.
    ///
    /// Corresponds to `BSD::SocketImpl` in upstream bsd.cpp.
    pub fn socket_impl(
        &mut self,
        domain: Domain,
        mut ty: Type,
        protocol: Protocol,
    ) -> (i32, Errno) {
        if self.is_user && (ty == Type::SEQPACKET || ty == Type::RAW) {
            if !(ty == Type::RAW && domain == Domain::INET && protocol == Protocol::ICMP) {
                return (-1, Errno::INVAL);
            }
        }

        // Check and strip unknown flag (bit 29)
        let raw_type = ty.0;
        let unk_flag = (raw_type & 0x20000000) != 0;
        if unk_flag {
            log::warn!("Unknown flag in type");
        }
        ty = Type(raw_type & !0x20000000);

        if socket_blocked_by_airplane_mode(
            *common::settings::values().airplane_mode.get_value(), is_connection_based(ty)) {
            return (-1, Errno::NOTCONN);
        }
        let mut socket = Socket::new();
        let errno = socket.initialize(
            translate_domain(domain),
            translate_type(ty),
            translate_protocol(protocol),
        );
        if errno != NetErrno::Success {
            return (-1, translate_errno(errno));
        }

        let mut table = self.file_descriptors.lock().unwrap();
        let fd = table.find_free_file_descriptor_handle();
        if fd < 0 { return (-1, Errno::MFILE); }
        table.entries[fd as usize] = Some(FileDescriptor {
            socket: Some(Arc::new(Mutex::new(Box::new(socket)))),
            is_connection_based: is_connection_based(ty),
            ..FileDescriptor::default()
        });
        (fd, Errno::SUCCESS)
    }

    /// PollImpl -- poll file descriptors.
    ///
    /// Corresponds to `BSD::PollImpl` in upstream bsd.cpp.
    fn poll_impl(&self, ctx: &mut HLERequestContext, output: &mut Vec<u8>, input: &[u8], nfds: i32, timeout: i32) -> Option<(i32, Errno)> {
        use std::time::{Duration, Instant};
        let mut request = self.select_notifier.as_ref().and_then(|notifier| notifier.take(ctx));
        if request.is_none() {
            if nfds <= 0 { return Some((-1, Errno::SUCCESS)); }
            let size = nfds as usize * 8;
            if input.len() < size || output.len() < size || timeout < -1 { return Some((-1, Errno::INVAL)); }
            let fds = input[..size].chunks_exact(8).map(|bytes| PollFD {
                fd: i32::from_le_bytes(bytes[..4].try_into().unwrap()), events: u16::from_le_bytes(bytes[4..6].try_into().unwrap()), revents: 0,
            }).collect();
            request = Some(SelectRequest { pollfds: fds, deadline: (timeout >= 0).then(|| Instant::now()+Duration::from_millis(timeout as u64)), ..SelectRequest::default() });
        }
        let request = request.unwrap();
        let mut fds = request.pollfds.clone();
        let mut wait = SelectWait::default();
        let mut recheck = false;
        let mut host = Vec::new();
        let mut indices = Vec::new();
        {
            let table = self.file_descriptors.lock().unwrap();
            for (index, fd) in fds.iter_mut().enumerate() {
                fd.revents = 0;
                if fd.fd < 0 { continue; }
                let Some(descriptor) = table.find_open_descriptor(fd.fd) else { fd.revents = PollEvents::NVAL.bits(); continue; };
                let events = translate_poll_events(PollEvents::from_bits_retain(fd.events));
                let mut entry = SelectWaitEntry { fd: fd.fd, socket: descriptor.socket.clone(), event_fd: descriptor.event_fd.clone(), event_fd_version: 0,
                    generation: None, events: NetPollEvents::empty(), emulated: false };
                if let Some(event) = &descriptor.event_fd {
                    let (version, readable, writable) = event.readiness(); entry.event_fd_version = version;
                    let mut ready = NetPollEvents::empty();
                    if readable { ready |= events & (NetPollEvents::IN | NetPollEvents::RD_NORM); }
                    if writable { ready |= events & NetPollEvents::OUT; }
                    fd.revents = translate_poll_events_from_network(ready).bits();
                } else {
                    let socket = descriptor.socket.as_ref().unwrap().lock().unwrap();
                    if let Some(ready) = socket.emulated_poll(events) {
                        entry.events = events; entry.emulated = true;
                        fd.revents = translate_poll_events_from_network(ready).bits();
                    } else if !socket.is_opened() { fd.revents = PollEvents::NVAL.bits(); continue; }
                    else {
                        entry.events = events; recheck |= events.is_empty();
                        indices.push((index, wait.entries.len()));
                        host.push(net_sockets::PollFD { fd: socket.get_fd(), events: events.bits(), revents: 0 });
                    }
                }
                wait.entries.push(entry);
            }
            if !host.is_empty() {
                let (ret, errno) = net_sockets::poll(&mut host, 0);
                if ret < 0 { return Some((-1, if crate::internal_network::network::socket_operations_cancelled() { Errno::INTR } else { translate_errno(errno) })); }
            }
        }
        let always = PollEvents::ERR | PollEvents::HUP | PollEvents::NVAL;
        for (host, (index, wait_index)) in host.iter().zip(indices) {
            let reported = translate_poll_events_from_network(NetPollEvents::from_bits_retain(host.revents));
            fds[index].revents = reported.bits() & (fds[index].events | always.bits());
            if !reported.is_empty() && fds[index].revents == 0 { wait.entries[wait_index].events = NetPollEvents::empty(); recheck = true; }
        }
        let count = fds.iter().filter(|fd| fd.revents != 0).count() as i32;
        let now = Instant::now();
        if count == 0 && request.deadline.is_none_or(|at| now < at) {
            if recheck { wait.recheck = Some(now + Duration::from_millis(50)); }
            if crate::internal_network::network::socket_operations_cancelled()
                || !self.select_notifier.as_ref().is_some_and(|notifier| notifier.defer(ctx, request, wait)) { return Some((-1, Errno::INTR)); }
            ctx.set_is_deferred_value(true); return None;
        }
        output.clear();
        for fd in fds { output.extend(fd.fd.to_le_bytes()); output.extend(fd.events.to_le_bytes()); output.extend(fd.revents.to_le_bytes()); }
        Some((count, Errno::SUCCESS))
    }

    fn select_impl(&self, request: &SelectRequest, ready_sets: &mut [Vec<u8>;3], wait: &mut SelectWait) -> Option<(i32, Errno)> {
        let mut wanted = Vec::new(); let mut ready = Vec::new(); let mut host = Vec::new(); let mut indices = Vec::new();
        wait.entries.clear();
        {
            let table = self.file_descriptors.lock().unwrap();
            for fd in 0..request.nfds {
                let asked: [bool;3] = std::array::from_fn(|set| fd_is_set(&request.fd_sets[set], fd));
                if asked == [false;3] { continue; }
                let Some(descriptor) = table.find_open_descriptor(fd as i32) else { return Some((-1, Errno::BADF)); };
                let mut entry = SelectWaitEntry { fd: fd as i32, socket: descriptor.socket.clone(), event_fd: descriptor.event_fd.clone(), event_fd_version: 0,
                    generation: None, events: NetPollEvents::empty(), emulated: false };
                let mut found = [false;3];
                if let Some(event) = &descriptor.event_fd {
                    let (version, readable, writable) = event.readiness(); entry.event_fd_version = version;
                    found = [asked[0] && readable, asked[1] && writable, false];
                } else {
                    for (asked, event) in asked.into_iter().zip([NetPollEvents::RD_NORM, NetPollEvents::OUT, NetPollEvents::PRI]) { if asked { entry.events |= event; } }
                    let socket = descriptor.socket.as_ref().unwrap().lock().unwrap();
                    if let Some(events) = socket.emulated_poll(entry.events) {
                        entry.emulated = true;
                        found = [events.intersects(NetPollEvents::RD_NORM), events.intersects(NetPollEvents::OUT), events.intersects(NetPollEvents::PRI)];
                    } else if !socket.is_opened() { return Some((-1, Errno::BADF)); }
                    else { indices.push(wait.entries.len()); host.push(net_sockets::PollFD { fd: socket.get_fd(), events: entry.events.bits(), revents: 0 }); }
                }
                wanted.push(asked); ready.push(found); wait.entries.push(entry);
            }
            if !host.is_empty() {
                let (ret, errno) = net_sockets::poll(&mut host, 0);
                if ret < 0 { return Some((-1, if crate::internal_network::network::socket_operations_cancelled() { Errno::INTR } else { translate_errno(errno) })); }
            }
        }
        let now = std::time::Instant::now(); wait.recheck = None;
        for (host, index) in host.iter().zip(indices) {
            let events = NetPollEvents::from_bits_retain(host.revents);
            if events.intersects(NetPollEvents::NVAL) { return Some((-1, Errno::BADF)); }
            ready[index] = [wanted[index][0] && events.intersects(NetPollEvents::RD_NORM | NetPollEvents::HUP | NetPollEvents::ERR),
                wanted[index][1] && events.intersects(NetPollEvents::OUT | NetPollEvents::HUP | NetPollEvents::ERR),
                wanted[index][2] && events.intersects(NetPollEvents::PRI | NetPollEvents::ERR)];
            if !events.is_empty() && ready[index] == [false;3] { wait.entries[index].events = NetPollEvents::empty(); wait.recheck = Some(now + std::time::Duration::from_millis(50)); }
        }
        let count = ready.iter().flatten().filter(|&&bit| bit).count() as i32;
        if count == 0 && request.deadline.is_none_or(|at| now < at) {
            return if crate::internal_network::network::socket_operations_cancelled() { Some((-1, Errno::INTR)) } else { None };
        }
        for (output, input) in ready_sets.iter_mut().zip(&request.fd_sets) { *output = vec![0; input.len()]; }
        for (entry, found) in wait.entries.iter().zip(ready) {
            for (set, found) in ready_sets.iter_mut().zip(found) {
                if found { set[entry.fd as usize / 8] |= 1 << (entry.fd as usize % 8); }
            }
        }
        Some((count, Errno::SUCCESS))
    }

    /// AcceptImpl -- accept a connection.
    ///
    /// Corresponds to `BSD::AcceptImpl` in upstream bsd.cpp.
    pub fn accept_impl(&mut self, fd: i32, write_buffer: &mut Vec<u8>) -> (i32, Errno) {
        let Some(descriptor) = self.copy_descriptor(fd) else {
            return (-1, Errno::BADF);
        };

        let (result, bsd_errno) = descriptor.socket.as_ref().unwrap().lock().unwrap().accept();
        if bsd_errno != NetErrno::Success {
            return (-1, translate_errno(bsd_errno));
        }

        let accept_result = result;
        let guest_addr_in = translate_sockaddr_from_network(&accept_result.sockaddr_in);

        // Write the guest address to the write buffer
        let addr_size = std::mem::size_of::<SockAddrIn>();
        write_buffer.resize(addr_size, 0);
        unsafe {
            std::ptr::copy_nonoverlapping(
                &guest_addr_in as *const SockAddrIn as *const u8,
                write_buffer.as_mut_ptr(),
                addr_size,
            );
        }

        let mut table = self.file_descriptors.lock().unwrap();
        let new_fd = table.find_free_file_descriptor_handle();
        if new_fd < 0 { return (-1, Errno::MFILE); }
        table.entries[new_fd as usize] = Some(FileDescriptor {
            socket: accept_result.socket.map(|socket| Arc::new(Mutex::new(socket))),
            is_connection_based: descriptor.is_connection_based,
            receive_timeout: descriptor.receive_timeout,
            send_timeout: descriptor.send_timeout,
            ..FileDescriptor::default()
        });

        (new_fd, Errno::SUCCESS)
    }

    /// BindImpl -- bind a socket to an address.
    ///
    /// Corresponds to `BSD::BindImpl` in upstream bsd.cpp.
    pub fn bind_impl(&mut self, fd: i32, addr: &[u8]) -> Errno {
        let Some(descriptor) = self.copy_descriptor(fd) else {
            return Errno::BADF;
        };
        assert!(addr.len() >= std::mem::size_of::<SockAddrIn>());

        let mut guest_addr = SockAddrIn::default();
        unsafe {
            std::ptr::copy_nonoverlapping(
                addr.as_ptr(),
                &mut guest_addr as *mut SockAddrIn as *mut u8,
                std::mem::size_of::<SockAddrIn>(),
            );
        }
        let net_addr = translate_sockaddr_to_network(&guest_addr);

        let result = translate_errno(descriptor.socket.as_ref().unwrap().lock().unwrap().bind(net_addr));
        result
    }

    /// ConnectImpl -- connect to remote address.
    ///
    /// Corresponds to `BSD::ConnectImpl` in upstream bsd.cpp.
    pub fn connect_impl(&mut self, fd: i32, addr: &[u8]) -> Errno {
        let Some(descriptor) = self.copy_descriptor(fd) else {
            return Errno::BADF;
        };
        assert!(addr.len() >= std::mem::size_of::<SockAddrIn>());

        let mut guest_addr = SockAddrIn::default();
        unsafe {
            std::ptr::copy_nonoverlapping(
                addr.as_ptr(),
                &mut guest_addr as *mut SockAddrIn as *mut u8,
                std::mem::size_of::<SockAddrIn>(),
            );
        }
        let net_addr = translate_sockaddr_to_network(&guest_addr);

        let result = translate_errno(descriptor.socket.as_ref().unwrap().lock().unwrap().connect(net_addr));
        if result == Errno::ISCONN {
            log::debug!("returned ISCONN - socket already connected");
            return Errno::SUCCESS;
        }
        result
    }

    /// GetPeerNameImpl
    ///
    /// Corresponds to `BSD::GetPeerNameImpl` in upstream bsd.cpp.
    pub fn get_peer_name_impl(&self, fd: i32, write_buffer: &mut Vec<u8>) -> Errno {
        let Some(descriptor) = self.copy_descriptor(fd) else {
            return Errno::BADF;
        };

        let (addr_in, bsd_errno) = descriptor.socket.as_ref().unwrap().lock().unwrap().get_peer_name();
        if bsd_errno != NetErrno::Success {
            return translate_errno(bsd_errno);
        }

        let guest_addr_in = translate_sockaddr_from_network(&addr_in);
        let addr_size = std::mem::size_of::<SockAddrIn>();
        assert!(write_buffer.len() >= addr_size);
        write_buffer.resize(addr_size, 0);
        unsafe {
            std::ptr::copy_nonoverlapping(
                &guest_addr_in as *const SockAddrIn as *const u8,
                write_buffer.as_mut_ptr(),
                addr_size,
            );
        }
        translate_errno(bsd_errno)
    }

    /// GetSockNameImpl
    ///
    /// Corresponds to `BSD::GetSockNameImpl` in upstream bsd.cpp.
    pub fn get_sock_name_impl(&self, fd: i32, write_buffer: &mut Vec<u8>) -> Errno {
        let Some(descriptor) = self.copy_descriptor(fd) else {
            return Errno::BADF;
        };

        let (addr_in, bsd_errno) = descriptor.socket.as_ref().unwrap().lock().unwrap().get_sock_name();
        if bsd_errno != NetErrno::Success {
            return translate_errno(bsd_errno);
        }

        let guest_addr_in = translate_sockaddr_from_network(&addr_in);
        let addr_size = std::mem::size_of::<SockAddrIn>();
        assert!(write_buffer.len() >= addr_size);
        write_buffer.resize(addr_size, 0);
        unsafe {
            std::ptr::copy_nonoverlapping(
                &guest_addr_in as *const SockAddrIn as *const u8,
                write_buffer.as_mut_ptr(),
                addr_size,
            );
        }
        translate_errno(bsd_errno)
    }

    /// ListenImpl -- listen on a socket.
    ///
    /// Corresponds to `BSD::ListenImpl` in upstream bsd.cpp.
    pub fn listen_impl(&mut self, fd: i32, backlog: i32) -> Errno {
        let Some(descriptor) = self.copy_descriptor(fd) else {
            return Errno::BADF;
        };
        let result = translate_errno(descriptor.socket.as_ref().unwrap().lock().unwrap().listen(backlog));
        result
    }

    /// FcntlImpl -- file control operations.
    ///
    /// Corresponds to `BSD::FcntlImpl` in upstream bsd.cpp.
    pub fn fcntl_impl(&mut self, fd: i32, cmd: FcntlCmd, arg: i32) -> (i32, Errno) {
        use std::sync::atomic::Ordering::Relaxed;
        let Some((descriptor, _)) = self.copy_file_descriptor(fd) else { return (-1, Errno::BADF); };
        match cmd {
            FcntlCmd::GETFL => {
                assert_eq!(arg, 0);
                (descriptor.event_fd.as_ref().map_or(descriptor.flags, |event| event.status_flags.load(Relaxed)), Errno::SUCCESS)
            }
            FcntlCmd::SETFL => {
                if let Some(event) = descriptor.event_fd { event.status_flags.store(arg, Relaxed); return (0, Errno::SUCCESS); }
                let mut table = self.file_descriptors.lock().unwrap();
                let socket = descriptor.socket.as_ref().unwrap();
                let errno = translate_errno(socket.lock().unwrap().set_non_block(arg & FLAG_O_NONBLOCK != 0));
                if errno != Errno::SUCCESS { return (-1, errno); }
                if let Some(entry) = &mut table.entries[fd as usize] {
                    if entry.socket.as_ref().is_some_and(|other| Arc::ptr_eq(socket, other)) { entry.flags = arg; }
                }
                (0, Errno::SUCCESS)
            }
            _ => (-1, Errno::SUCCESS),
        }
    }

    /// GetSockOptImpl
    ///
    /// Corresponds to `BSD::GetSockOptImpl` in upstream bsd.cpp.
    pub fn get_sock_opt_impl(
        &self,
        fd: i32,
        level: u32,
        optname: OptName,
        optval: &mut Vec<u8>,
    ) -> Errno {
        let Some(descriptor) = self.copy_descriptor(fd) else {
            return Errno::BADF;
        };

        if level != SocketLevel::SOCKET as u32 {
            log::warn!("Unknown getsockopt level={}", level);
            return Errno::SUCCESS;
        }


        match optname {
            OptName::ERROR => {
                let (pending_err, getsockopt_err) = descriptor.socket.as_ref().unwrap().lock().unwrap().get_pending_error();
                if getsockopt_err == NetErrno::Success {
                    let translated_pending_err = translate_errno(pending_err);
                    if optval.len() != std::mem::size_of::<Errno>() {
                        return Errno::INVAL;
                    }
                    optval.resize(std::mem::size_of::<Errno>(), 0);
                    let err_val = translated_pending_err as u32;
                    optval[..4].copy_from_slice(&err_val.to_ne_bytes());
                }
                translate_errno(getsockopt_err)
            }
            _ => {
                log::warn!("Unimplemented optname={:?}", optname);
                Errno::SUCCESS
            }
        }
    }

    /// SetSockOptImpl
    ///
    /// Corresponds to `BSD::SetSockOptImpl` in upstream bsd.cpp.
    pub fn set_sock_opt_impl(
        &mut self,
        fd: i32,
        level: u32,
        optname: OptName,
        optval: &[u8],
    ) -> Errno {
        let Some(descriptor) = self.copy_descriptor(fd) else {
            return Errno::BADF;
        };

        if level != SocketLevel::SOCKET as u32 {
            log::warn!("Unknown setsockopt level={}", level);
            return Errno::SUCCESS;
        }


        if optname == OptName::LINGER {
            if optval.len() < std::mem::size_of::<Linger>() { return Errno::INVAL; }
            let mut linger = Linger::default();
            unsafe {
                std::ptr::copy_nonoverlapping(
                    optval.as_ptr(),
                    &mut linger as *mut Linger as *mut u8,
                    std::mem::size_of::<Linger>(),
                );
            }
            assert!(linger.onoff == 0 || linger.onoff == 1);
            return translate_errno(
                descriptor
                    .socket.as_ref().unwrap()
                    .lock().unwrap().set_linger(linger.onoff != 0, linger.linger),
            );
        }

        if optval.len() < std::mem::size_of::<u32>() { return Errno::INVAL; }
        if optname == OptName::SNDTIMEO || optname == OptName::RCVTIMEO {
            if optval.len() < 16 { return Errno::INVAL; }
            let seconds = i64::from_le_bytes(optval[..8].try_into().unwrap());
            let microseconds = i64::from_le_bytes(optval[8..16].try_into().unwrap());
            if seconds < 0 || !(0..1_000_000).contains(&microseconds) { return Errno::DOM; }
            let max = u32::MAX as i64;
            let timeout = if seconds >= max / 1000 { max } else { max.min(seconds * 1000 + (microseconds + 999) / 1000) } as u32;
            let receive = optname == OptName::RCVTIMEO;
            let socket = descriptor.socket.as_ref().unwrap();
            let result = {
                let mut host = socket.lock().unwrap();
                translate_errno(if receive { host.set_rcv_timeo(timeout) } else { host.set_snd_timeo(timeout) })
            };
            if result == Errno::SUCCESS {
                for entry in self.file_descriptors.lock().unwrap().entries.iter_mut().flatten() {
                    if entry.socket.as_ref().is_some_and(|other| Arc::ptr_eq(socket, other)) {
                        if receive { entry.receive_timeout = timeout; } else { entry.send_timeout = timeout; }
                    }
                }
            }
            return result;
        }
        let value = u32::from_ne_bytes([optval[0], optval[1], optval[2], optval[3]]);

        match optname {
            OptName::REUSEADDR => {
                assert!(value == 0 || value == 1);
                translate_errno(descriptor.socket.as_ref().unwrap().lock().unwrap().set_reuse_addr(value != 0))
            }
            OptName::KEEPALIVE => {
                assert!(value == 0 || value == 1);
                translate_errno(descriptor.socket.as_ref().unwrap().lock().unwrap().set_keep_alive(value != 0))
            }
            OptName::BROADCAST => {
                assert!(value == 0 || value == 1);
                translate_errno(descriptor.socket.as_ref().unwrap().lock().unwrap().set_broadcast(value != 0))
            }
            OptName::SNDBUF => translate_errno(descriptor.socket.as_ref().unwrap().lock().unwrap().set_snd_buf(value)),
            OptName::RCVBUF => translate_errno(descriptor.socket.as_ref().unwrap().lock().unwrap().set_rcv_buf(value)),
            OptName::NOSIGPIPE => {
                log::warn!("(STUBBED) setting NOSIGPIPE to {}", value);
                Errno::SUCCESS
            }
            _ => {
                log::warn!("Unimplemented optname={:?}", optname);
                Errno::SUCCESS
            }
        }
    }

    /// ShutdownImpl
    ///
    /// Corresponds to `BSD::ShutdownImpl` in upstream bsd.cpp.
    pub fn shutdown_impl(&mut self, fd: i32, how: i32) -> Errno {
        let Some(descriptor) = self.copy_descriptor(fd) else {
            return Errno::BADF;
        };
        let host_how = translate_shutdown_how(ShutdownHow(how));
        let result = translate_errno(descriptor.socket.as_ref().unwrap().lock().unwrap().shutdown(host_how));
        result
    }

    /// RecvImpl
    ///
    /// Corresponds to `BSD::RecvImpl` in upstream bsd.cpp.
    pub fn recv_impl(&mut self, fd: i32, mut flags: u32, message: &mut Vec<u8>) -> (i32, Errno) {
        let Some(descriptor) = self.copy_descriptor(fd) else {
            return (-1, Errno::BADF);
        };

        let mut socket = descriptor.socket.as_ref().unwrap().lock().unwrap();

        // Apply MSG_DONTWAIT flag
        if (flags & FLAG_MSG_DONTWAIT) != 0 {
            flags &= !FLAG_MSG_DONTWAIT;
            if (descriptor.flags & FLAG_O_NONBLOCK) == 0 {
                socket.set_non_block(true);
            }
        }

        let (ret, bsd_errno) =
            translate_result(socket.recv(flags as i32, message.as_mut_slice()));

        // Restore original state
        if (descriptor.flags & FLAG_O_NONBLOCK) == 0 {
            socket.set_non_block(false);
        }

        (ret, bsd_errno)
    }

    /// RecvFromImpl
    ///
    /// Corresponds to `BSD::RecvFromImpl` in upstream bsd.cpp.
    pub fn recv_from_impl(
        &mut self,
        fd: i32,
        mut flags: u32,
        message: &mut Vec<u8>,
        addr: &mut Vec<u8>,
    ) -> (i32, Errno) {
        let Some(descriptor) = self.copy_descriptor(fd) else {
            return (-1, Errno::BADF);
        };

        let mut socket = descriptor.socket.as_ref().unwrap().lock().unwrap();

        let mut addr_in = NetSockAddrIn::default();
        let use_addr = if descriptor.is_connection_based {
            // Connection based file descriptors (e.g. TCP) zero addr
            addr.clear();
            false
        } else {
            true
        };

        // Apply MSG_DONTWAIT flag
        if (flags & FLAG_MSG_DONTWAIT) != 0 {
            flags &= !FLAG_MSG_DONTWAIT;
            if (descriptor.flags & FLAG_O_NONBLOCK) == 0 {
                socket.set_non_block(true);
            }
        }

        let p_addr_in = if use_addr { Some(&mut addr_in) } else { None };

        let (ret, bsd_errno) = translate_result(socket.recv_from(
            flags as i32,
            message.as_mut_slice(),
            p_addr_in,
        ));

        // Restore original state
        if (descriptor.flags & FLAG_O_NONBLOCK) == 0 {
            socket.set_non_block(false);
        }

        if use_addr {
            if ret < 0 {
                addr.clear();
            } else {
                assert!(addr.len() == std::mem::size_of::<SockAddrIn>());
                let guest_addr = translate_sockaddr_from_network(&addr_in);
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        &guest_addr as *const SockAddrIn as *const u8,
                        addr.as_mut_ptr(),
                        std::mem::size_of::<SockAddrIn>(),
                    );
                }
            }
        }

        (ret, bsd_errno)
    }

    /// SendImpl
    ///
    /// Corresponds to `BSD::SendImpl` in upstream bsd.cpp.
    pub fn send_impl(&mut self, fd: i32, flags: u32, message: &[u8]) -> (i32, Errno) {
        let Some(descriptor) = self.copy_descriptor(fd) else {
            return (-1, Errno::BADF);
        };
        let result = translate_result(descriptor.socket.as_ref().unwrap().lock().unwrap().send(message, flags as i32));
        result
    }

    /// SendToImpl
    ///
    /// Corresponds to `BSD::SendToImpl` in upstream bsd.cpp.
    pub fn send_to_impl(
        &mut self,
        fd: i32,
        flags: u32,
        message: &[u8],
        addr: &[u8],
    ) -> (i32, Errno) {
        let Some(descriptor) = self.copy_descriptor(fd) else {
            return (-1, Errno::BADF);
        };

        let p_addr_in = if !addr.is_empty() {
            assert!(addr.len() == std::mem::size_of::<SockAddrIn>());
            let mut guest_addr = SockAddrIn::default();
            unsafe {
                std::ptr::copy_nonoverlapping(
                    addr.as_ptr(),
                    &mut guest_addr as *mut SockAddrIn as *mut u8,
                    std::mem::size_of::<SockAddrIn>(),
                );
            }
            Some(translate_sockaddr_to_network(&guest_addr))
        } else {
            None
        };

        let result = translate_result(
            descriptor
                .socket.as_ref().unwrap()
                .lock().unwrap().send_to(flags, message, p_addr_in.as_ref()),
        );
        result
    }

    /// CloseImpl -- close a file descriptor.
    ///
    /// Corresponds to `BSD::CloseImpl` in upstream bsd.cpp.
    pub fn close_impl(&mut self, fd: i32) -> Errno {
        let mut event_shared = false;
        let descriptor = {
            let mut table = self.file_descriptors.lock().unwrap();
            let Some(slot) = usize::try_from(fd).ok().and_then(|fd| table.entries.get_mut(fd)) else { return Errno::BADF; };
            let Some(descriptor) = slot.take() else { return Errno::BADF; };
            table.generations[fd as usize] = table.generations[fd as usize].wrapping_add(1);
            if let Some(event) = &descriptor.event_fd {
                event.descriptor_closed();
                event_shared = table.entries.iter().flatten().any(|entry| entry.event_fd.as_ref().is_some_and(|other| Arc::ptr_eq(event, other)));
            }
            if let Some(socket) = &descriptor.socket {
                for other in table.entries.iter_mut().flatten() {
                    if other.socket.as_ref().is_some_and(|other| Arc::ptr_eq(socket, other)) { other.socket_closed = true; }
                }
            }
            descriptor
        };
        if let Some(event) = descriptor.event_fd {
            if let Some(notifier) = &self.select_notifier { notifier.notify_change(None); }
            if !event_shared { event.close(); }
            return Errno::SUCCESS;
        }
        if let Some(notifier) = &self.select_notifier { notifier.notify_change(descriptor.socket.as_ref()); }
        if descriptor.socket_closed { return Errno::SUCCESS; }
        descriptor.socket.map_or(Errno::SUCCESS, |socket| translate_errno(socket.lock().unwrap().close()))
    }

    pub fn duplicate_socket_impl(&mut self, fd: i32) -> Result<i32, Errno> {
        let mut table = self.file_descriptors.lock().unwrap();
        let descriptor = table.find_open_descriptor(fd).ok_or(Errno::BADF)?.clone();
        let new_fd = table.find_free_file_descriptor_handle();
        if new_fd < 0 { return Err(Errno::MFILE); }
        table.entries[new_fd as usize] = Some(descriptor);
        Ok(new_fd)
    }

    pub fn get_socket(&self, fd: i32) -> Option<Arc<Mutex<Box<dyn SocketBase>>>> {
        self.copy_descriptor(fd)?.socket
    }

    pub fn event_fd_impl(&self, initval: u64, flags: u32) -> (i32, Errno) {
        if initval > EventFdState::MAX_COUNTER { return (-1, Errno::INVAL); }
        let unknown = flags & !(EventFdState::FLAG_SEMAPHORE | EventFdState::FLAG_NONBLOCK);
        if unknown != 0 { log::warn!("Ignoring unknown eventfd flags={unknown:#x}"); }
        let event = EventFdState {
            inner: Mutex::new(EventFdInner { counter: initval, closed: false }),
            semaphore: flags & EventFdState::FLAG_SEMAPHORE != 0,
            status_flags: std::sync::atomic::AtomicI32::new(if flags & EventFdState::FLAG_NONBLOCK != 0 { FLAG_O_NONBLOCK } else { 0 }),
            ..EventFdState::default()
        };
        let mut table = self.file_descriptors.lock().unwrap();
        let fd = table.find_free_file_descriptor_handle();
        if fd < 0 { return (-1, Errno::MFILE); }
        table.entries[fd as usize] = Some(FileDescriptor { event_fd: Some(Arc::new(event)), ..FileDescriptor::default() });
        (fd, Errno::SUCCESS)
    }

    fn event_fd_io_impl(&self, ctx: &mut HLERequestContext, fd: i32, descriptor: &FileDescriptor, generation: u64,
        operation: impl FnOnce(&mut u64) -> (i32, Errno)) -> Option<(i32, Errno)> {
        let mut version = 0;
        let result = operation(&mut version);
        if result.1 == Errno::SUCCESS {
            if let Some(notifier) = &self.select_notifier { notifier.notify_change(None); }
            return Some(result);
        }
        if result.1 != Errno::AGAIN || descriptor.event_fd.as_ref().unwrap().is_non_blocking() { return Some(result); }
        let request = SelectRequest { file_descriptor_generation: generation, ..SelectRequest::default() };
        let wait = SelectWait { entries: vec![SelectWaitEntry { fd, socket: descriptor.socket.clone(), event_fd: descriptor.event_fd.clone(),
            event_fd_version: version, generation: Some(generation), events: NetPollEvents::empty(), emulated: false }], recheck: None };
        if !self.select_notifier.as_ref().is_some_and(|notifier| notifier.defer(ctx, request, wait)) { return Some((-1, Errno::INTR)); }
        ctx.set_is_deferred_value(true);
        None
    }

    fn is_blocking(&self, fd: i32, descriptor: &FileDescriptor) -> bool {
        let table = self.file_descriptors.lock().unwrap();
        let socket = descriptor.socket.as_ref().unwrap();
        let flags = table.entries[fd as usize].as_ref()
            .filter(|entry| entry.socket.as_ref().is_some_and(|other| Arc::ptr_eq(socket, other)))
            .map_or(descriptor.flags, |entry| entry.flags);
        flags & FLAG_O_NONBLOCK == 0
    }

    fn defer_socket_wait(&self, ctx: &mut HLERequestContext, fd: i32, descriptor: &FileDescriptor,
        generation: u64, send: bool, mut request: SelectRequest) -> bool {
        if crate::internal_network::network::socket_operations_cancelled() { return false; }
        let socket = descriptor.socket.as_ref().unwrap();
        let events = if send { NetPollEvents::OUT } else { NetPollEvents::RD_NORM };
        request.file_descriptor_generation = generation;
        let emulated = socket.lock().unwrap().emulated_poll(events).is_some();
        let wait = SelectWait { entries: vec![SelectWaitEntry { fd, socket: Some(Arc::clone(socket)), event_fd: None, event_fd_version: 0,
            generation: Some(generation), events, emulated }], recheck: None };
        if !self.select_notifier.as_ref().is_some_and(|notifier| notifier.defer(ctx, request, wait)) { return false; }
        ctx.set_is_deferred_value(true);
        true
    }

    fn read_impl(&self, ctx: &mut HLERequestContext, fd: i32, message: &mut [u8]) -> Option<(i32, Errno)> {
        let retried = self.select_notifier.as_ref().and_then(|notifier| notifier.take(ctx));
        let Some((descriptor, generation)) = self.copy_file_descriptor(fd) else { return Some((-1, Errno::BADF)); };
        if retried.as_ref().is_some_and(|request| request.file_descriptor_generation != generation) { return Some((-1, Errno::BADF)); }
        if let Some(event) = &descriptor.event_fd {
            return self.event_fd_io_impl(ctx, fd, &descriptor, generation, |version| event.read(message, version));
        }
        let socket = descriptor.socket.as_ref().unwrap();
        let blocking = self.is_blocking(fd, &descriptor);
        let result = translate_result(socket.lock().unwrap().recv(crate::internal_network::network::FLAG_MSG_DONTWAIT, message));
        if !blocking || result.1 != Errno::AGAIN { return Some(result); }
        let deadline = retried.map(|request| request.deadline).unwrap_or_else(|| (descriptor.receive_timeout != 0).then(|| std::time::Instant::now() + std::time::Duration::from_millis(descriptor.receive_timeout as u64)));
        if deadline.is_some_and(|at| std::time::Instant::now() >= at) { return Some(result); }
        let request = SelectRequest { deadline, file_descriptor_generation: generation, ..SelectRequest::default() };
        if !self.defer_socket_wait(ctx, fd, &descriptor, generation, false, request) { return Some((-1, Errno::INTR)); }
        None
    }

    fn write_impl(&self, ctx: &mut HLERequestContext, fd: i32, message: &[u8]) -> Option<(i32, Errno)> {
        let retried = self.select_notifier.as_ref().and_then(|notifier| notifier.take(ctx));
        let Some((descriptor, generation)) = self.copy_file_descriptor(fd) else { return Some((-1, Errno::BADF)); };
        if retried.is_some_and(|request| request.file_descriptor_generation != generation) { return Some((-1, Errno::BADF)); }
        if let Some(event) = &descriptor.event_fd {
            return self.event_fd_io_impl(ctx, fd, &descriptor, generation, |version| event.write(message, version));
        }
        let result = translate_result(descriptor.socket.as_ref().unwrap().lock().unwrap().send(message, 0));
        Some(result)
    }

    /// OnProxyPacketReceived -- handle incoming proxy packet.
    ///
    /// Corresponds to `BSD::OnProxyPacketReceived` in upstream bsd.cpp.
    pub fn on_proxy_packet_received(
        &mut self,
        packet: &crate::internal_network::network::ProxyPacket,
    ) {
        let sockets: Vec<_> = self.file_descriptors.lock().unwrap().entries.iter()
            .flatten().filter(|entry| !entry.socket_closed)
            .filter_map(|entry| entry.socket.clone()).collect();
        let mut unique = Vec::new();
        for socket in sockets {
            if unique.iter().any(|other| Arc::ptr_eq(&socket, other)) { continue; }
            unique.push(Arc::clone(&socket));
            socket.lock().unwrap().handle_proxy_packet(packet);
        }
    }

    /// Build a standard errno IPC response.
    ///
    /// Corresponds to `BSD::BuildErrnoResponse(HLERequestContext&, Errno)` in upstream.
    fn build_errno_response_ipc(ctx: &mut HLERequestContext, bsd_errno: Errno) {
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(if bsd_errno == Errno::SUCCESS { 0 } else { -1 });
        rb.push_u32(bsd_errno as u32);
    }

    fn register_client_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) BSD::RegisterClient called");
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(0); // bsd errno
    }

    fn start_monitoring_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) BSD::StartMonitoring called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn socket_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let bsd = unsafe { &mut *(std::ptr::addr_of!(*this).cast::<Bsd>().cast_mut()) };
        let mut rp = RequestParser::new(ctx);
        let domain = rp.pop_u32();
        let ty = rp.pop_u32();
        let protocol = rp.pop_u32();

        let (fd, bsd_errno) = bsd.socket_impl(Domain(domain), Type(ty), Protocol(protocol));

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(fd);
        rb.push_u32(bsd_errno as u32);
    }

    fn socket_exempt_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let bsd = unsafe { &mut *(std::ptr::addr_of!(*this).cast::<Bsd>().cast_mut()) };
        let mut rp = RequestParser::new(ctx);
        let domain = rp.pop_u32();
        let ty = rp.pop_u32();
        let protocol = rp.pop_u32();

        let (fd, mut bsd_errno) = bsd.socket_impl(Domain(domain), Type(ty), Protocol(protocol));
        if bsd_errno == Errno::SUCCESS {
            bsd_errno = bsd.shutdown_impl(fd, 0);
        }

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(fd);
        rb.push_u32(bsd_errno as u32);
    }

    fn select_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let bsd = unsafe { &*(std::ptr::addr_of!(*this).cast::<Bsd>()) };
        let reply = |ctx: &mut HLERequestContext, ret: i32, errno: Errno| {
            let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
            rb.push_result(RESULT_SUCCESS); rb.push_i32(ret); rb.push_u32(errno as u32);
        };
        let mut request = bsd.select_notifier.as_ref().and_then(|notifier| notifier.take(ctx));
        if request.is_none() {
            let mut rp = RequestParser::new(ctx);
            let nfds = rp.pop_i32(); let _reserved = rp.pop_u32();
            let seconds = rp.pop_i64(); let micros = rp.pop_i64(); let is_null = rp.pop_u8() != 0;
            if nfds < 0 { reply(ctx, -1, Errno::INVAL); return; }
            let mut parsed = SelectRequest { nfds: (nfds as usize).min(MAX_FD), ..SelectRequest::default() };
            let size = parsed.nfds.div_ceil(64) * 8;
            for set in 0..3 {
                if !ctx.can_read_buffer(set) { continue; }
                let bytes = ctx.read_buffer(set);
                parsed.fd_sets[set] = bytes[..size.min(bytes.len())].to_vec();
                if (MAX_FD..(nfds as usize).min(bytes.len()*8)).any(|fd| fd_is_set(&bytes, fd)) { reply(ctx, -1, Errno::BADF); return; }
            }
            if !is_null {
                if seconds < 0 || !(0..1_000_000).contains(&micros) { reply(ctx, -1, Errno::INVAL); return; }
                if seconds <= i32::MAX as i64 { parsed.deadline = Some(std::time::Instant::now() + std::time::Duration::from_secs(seconds as u64) + std::time::Duration::from_micros(micros as u64)); }
            }
            request = Some(parsed);
        }
        let request = request.unwrap(); let mut ready = std::array::from_fn(|_| Vec::new()); let mut wait = SelectWait::default();
        let result = bsd.select_impl(&request, &mut ready, &mut wait);
        let (ret, errno) = match result {
            Some(result) => result,
            None => {
                if bsd.select_notifier.as_ref().is_some_and(|notifier| notifier.defer(ctx, request, wait)) { ctx.set_is_deferred_value(true); return; }
                (-1, Errno::INTR)
            }
        };
        if ret >= 0 {
            for (index, bytes) in ready.iter().enumerate() {
                if ctx.can_write_buffer(index) { let size = bytes.len().min(ctx.get_write_buffer_size(index)); if size > 0 { ctx.write_buffer(&bytes[..size], index); } }
            }
        }
        reply(ctx, ret, errno);
    }

    fn poll_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let bsd = unsafe { &mut *(std::ptr::addr_of!(*this).cast::<Bsd>().cast_mut()) };
        let mut rp = RequestParser::new(ctx);
        let nfds = rp.pop_i32();
        let timeout = rp.pop_i32();

        log::debug!("BSD::Poll called. nfds={} timeout={}", nfds, timeout);

        let read_buffer = ctx.read_buffer(0);
        let write_size = ctx.get_write_buffer_size(0);
        let mut write_buffer = vec![0u8; write_size];

        let Some((ret, bsd_errno)) = bsd.poll_impl(ctx, &mut write_buffer, &read_buffer, nfds, timeout) else { return; };

        if !write_buffer.is_empty() {
            ctx.write_buffer(&write_buffer, 0);
        }
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(ret);
        rb.push_u32(bsd_errno as u32);
    }

    fn accept_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let bsd = unsafe { &mut *(std::ptr::addr_of!(*this).cast::<Bsd>().cast_mut()) };
        let mut rp = RequestParser::new(ctx);
        let fd = rp.pop_i32();

        let mut write_buffer = vec![0u8; ctx.get_write_buffer_size(0)];
        let (ret, bsd_errno) = bsd.accept_impl(fd, &mut write_buffer);

        ctx.write_buffer(&write_buffer, 0);
        let mut rb = ResponseBuilder::new(ctx, 5, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(ret);
        rb.push_u32(bsd_errno as u32);
        rb.push_u32(write_buffer.len() as u32);
    }

    fn bind_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let bsd = unsafe { &mut *(std::ptr::addr_of!(*this).cast::<Bsd>().cast_mut()) };
        let mut rp = RequestParser::new(ctx);
        let fd = rp.pop_i32();
        let addr = ctx.read_buffer(0);
        let bsd_errno = bsd.bind_impl(fd, &addr);
        Bsd::build_errno_response_ipc(ctx, bsd_errno);
    }

    fn connect_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let bsd = unsafe { &mut *(std::ptr::addr_of!(*this).cast::<Bsd>().cast_mut()) };
        let mut rp = RequestParser::new(ctx);
        let fd = rp.pop_i32();
        let addr = ctx.read_buffer(0);
        let bsd_errno = bsd.connect_impl(fd, &addr);
        Bsd::build_errno_response_ipc(ctx, bsd_errno);
    }

    fn get_peer_name_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let bsd = unsafe { &*(this as *const dyn ServiceFramework as *const Bsd) };
        let mut rp = RequestParser::new(ctx);
        let fd = rp.pop_i32();

        let mut write_buffer = vec![0u8; ctx.get_write_buffer_size(0)];
        let bsd_errno = bsd.get_peer_name_impl(fd, &mut write_buffer);

        ctx.write_buffer(&write_buffer, 0);
        let mut rb = ResponseBuilder::new(ctx, 5, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(if bsd_errno != Errno::SUCCESS { -1 } else { 0 });
        rb.push_u32(bsd_errno as u32);
        rb.push_u32(write_buffer.len() as u32);
    }

    fn get_sock_name_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let bsd = unsafe { &*(this as *const dyn ServiceFramework as *const Bsd) };
        let mut rp = RequestParser::new(ctx);
        let fd = rp.pop_i32();

        let mut write_buffer = vec![0u8; ctx.get_write_buffer_size(0)];
        let bsd_errno = bsd.get_sock_name_impl(fd, &mut write_buffer);

        ctx.write_buffer(&write_buffer, 0);
        let mut rb = ResponseBuilder::new(ctx, 5, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(if bsd_errno != Errno::SUCCESS { -1 } else { 0 });
        rb.push_u32(bsd_errno as u32);
        rb.push_u32(write_buffer.len() as u32);
    }

    fn get_sock_opt_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let bsd = unsafe { &*(this as *const dyn ServiceFramework as *const Bsd) };
        let mut rp = RequestParser::new(ctx);
        let fd = rp.pop_i32();
        let level = rp.pop_u32();
        let optname_raw = rp.pop_u32();
        let optname = OptName(optname_raw);

        let mut optval = vec![0u8; ctx.get_write_buffer_size(0)];
        let err = bsd.get_sock_opt_impl(fd, level, optname, &mut optval);

        ctx.write_buffer(&optval, 0);
        let mut rb = ResponseBuilder::new(ctx, 5, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(if err == Errno::SUCCESS { 0 } else { -1 });
        rb.push_u32(err as u32);
        rb.push_u32(optval.len() as u32);
    }

    fn listen_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let bsd = unsafe { &mut *(std::ptr::addr_of!(*this).cast::<Bsd>().cast_mut()) };
        let mut rp = RequestParser::new(ctx);
        let fd = rp.pop_i32();
        let backlog = rp.pop_i32();
        let bsd_errno = bsd.listen_impl(fd, backlog);
        Bsd::build_errno_response_ipc(ctx, bsd_errno);
    }

    fn fcntl_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let bsd = unsafe { &mut *(std::ptr::addr_of!(*this).cast::<Bsd>().cast_mut()) };
        let mut rp = RequestParser::new(ctx);
        let fd = rp.pop_i32();
        let cmd = rp.pop_i32();
        let arg = rp.pop_i32();

        let (ret, bsd_errno) = bsd.fcntl_impl(fd, FcntlCmd(cmd), arg);

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(ret);
        rb.push_u32(bsd_errno as u32);
    }

    fn set_sock_opt_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let bsd = unsafe { &mut *(std::ptr::addr_of!(*this).cast::<Bsd>().cast_mut()) };
        let mut rp = RequestParser::new(ctx);
        let fd = rp.pop_i32();
        let level = rp.pop_u32();
        let optname_raw = rp.pop_u32();
        let optname = OptName(optname_raw);
        let optval = ctx.read_buffer(0);
        let bsd_errno = bsd.set_sock_opt_impl(fd, level, optname, &optval);
        Bsd::build_errno_response_ipc(ctx, bsd_errno);
    }

    fn shutdown_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let bsd = unsafe { &mut *(std::ptr::addr_of!(*this).cast::<Bsd>().cast_mut()) };
        let mut rp = RequestParser::new(ctx);
        let fd = rp.pop_i32();
        let how = rp.pop_i32();
        let bsd_errno = bsd.shutdown_impl(fd, how);
        Bsd::build_errno_response_ipc(ctx, bsd_errno);
    }

    fn recv_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let bsd = unsafe { &mut *(std::ptr::addr_of!(*this).cast::<Bsd>().cast_mut()) };
        let mut rp = RequestParser::new(ctx);
        let fd = rp.pop_i32();
        let flags = rp.pop_u32();

        let mut message = vec![0u8; ctx.get_write_buffer_size(0)];
        let (ret, bsd_errno) = bsd.recv_impl(fd, flags, &mut message);

        ctx.write_buffer(&message, 0);
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(ret);
        rb.push_u32(bsd_errno as u32);
    }

    fn recv_from_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let bsd = unsafe { &mut *(std::ptr::addr_of!(*this).cast::<Bsd>().cast_mut()) };
        let mut rp = RequestParser::new(ctx);
        let fd = rp.pop_i32();
        let flags = rp.pop_u32();

        let mut message = vec![0u8; ctx.get_write_buffer_size(0)];
        let mut addr = vec![0u8; ctx.get_write_buffer_size(1)];
        let (ret, bsd_errno) = bsd.recv_from_impl(fd, flags, &mut message, &mut addr);

        ctx.write_buffer(&message, 0);
        ctx.write_buffer(&addr, 1);
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(ret);
        rb.push_u32(bsd_errno as u32);
    }

    fn send_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let bsd = unsafe { &mut *(std::ptr::addr_of!(*this).cast::<Bsd>().cast_mut()) };
        let mut rp = RequestParser::new(ctx);
        let fd = rp.pop_i32();
        let flags = rp.pop_u32();

        let message = ctx.read_buffer(0);
        let (ret, bsd_errno) = bsd.send_impl(fd, flags, &message);

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(ret);
        rb.push_u32(bsd_errno as u32);
    }

    fn send_to_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let bsd = unsafe { &mut *(std::ptr::addr_of!(*this).cast::<Bsd>().cast_mut()) };
        let mut rp = RequestParser::new(ctx);
        let fd = rp.pop_i32();
        let flags = rp.pop_u32();

        let message = ctx.read_buffer(0);
        let addr = ctx.read_buffer(1);
        let (ret, bsd_errno) = bsd.send_to_impl(fd, flags, &message, &addr);

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(ret);
        rb.push_u32(bsd_errno as u32);
    }

    fn write_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let bsd = unsafe { &mut *(std::ptr::addr_of!(*this).cast::<Bsd>().cast_mut()) };
        let mut rp = RequestParser::new(ctx);
        let fd = rp.pop_i32();

        let message = ctx.read_buffer(0);
        let Some((ret, bsd_errno)) = bsd.write_impl(ctx, fd, &message) else { return; };

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(ret);
        rb.push_u32(bsd_errno as u32);
    }

    fn read_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let bsd = unsafe { &*(std::ptr::addr_of!(*this).cast::<Bsd>()) };
        let fd = RequestParser::new(ctx).pop_i32();
        let mut message = vec![0; ctx.get_write_buffer_size(0)];
        let Some((ret, errno)) = bsd.read_impl(ctx, fd, &mut message) else { return; };
        if ret > 0 { message.truncate(ret as usize); ctx.write_buffer(&message, 0); }
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS); rb.push_i32(ret); rb.push_u32(errno as u32);
    }

    fn close_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let bsd = unsafe { &mut *(std::ptr::addr_of!(*this).cast::<Bsd>().cast_mut()) };
        let mut rp = RequestParser::new(ctx);
        let fd = rp.pop_i32();
        let bsd_errno = bsd.close_impl(fd);
        Bsd::build_errno_response_ipc(ctx, bsd_errno);
    }

    fn duplicate_socket_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let bsd = unsafe { &mut *(std::ptr::addr_of!(*this).cast::<Bsd>().cast_mut()) };
        let mut rp = RequestParser::new(ctx);
        let fd = rp.pop_i32();
        let _reserved = rp.pop_u64();

        let (ret, bsd_errno) = if bsd.is_user {
            (0, Errno::INVAL)
        } else {
            match bsd.duplicate_socket_impl(fd) {
                Ok(new_fd) => (new_fd, Errno::SUCCESS),
                Err(err) => (0, err),
            }
        };

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(ret);
        rb.push_u32(bsd_errno as u32);
    }

    fn event_fd_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let bsd = unsafe { &*(std::ptr::addr_of!(*this).cast::<Bsd>()) };
        let mut rp = RequestParser::new(ctx);
        let initval = rp.pop_u64(); let flags = rp.pop_u32();
        let (ret, errno) = bsd.event_fd_impl(initval, flags);
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS); rb.push_i32(ret); rb.push_u32(errno as u32);
    }

}

impl SessionRequestHandler for Bsd {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        self.name
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl SessionRequestHandler for std::sync::Mutex<Bsd> {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        let service = self.lock().unwrap();
        ServiceFramework::handle_sync_request_impl(&*service, ctx)
    }

    fn service_name(&self) -> &str {
        self.lock().unwrap().name
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl ServiceFramework for Bsd {
    fn get_service_name(&self) -> &str {
        &self.name
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// BSDCFG service.
///
/// Corresponds to `BSDCFG` in upstream bsd.h / bsd.cpp.
/// All commands are nullptr (unimplemented) in upstream.
pub struct BsdCfg {
    name: String,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl BsdCfg {
    pub fn new(name: &str) -> Self {
        let handlers = build_handler_map(&[
            (0, None, "SetIfUp"),
            (1, None, "SetIfUpWithEvent"),
            (2, None, "CancelIf"),
            (3, None, "SetIfDown"),
            (4, None, "GetIfState"),
            (5, None, "DhcpRenew"),
            (6, None, "AddStaticArpEntry"),
            (7, None, "RemoveArpEntry"),
            (8, None, "LookupArpEntry"),
            (9, None, "LookupArpEntry2"),
            (10, None, "ClearArpEntries"),
            (11, None, "ClearArpEntries2"),
            (12, None, "PrintArpEntries"),
            (13, None, "Unknown13"),
            (14, None, "Unknown14"),
            (15, None, "Unknown15"),
        ]);

        Self {
            name: name.to_owned(),
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for BsdCfg {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        &self.name
    }
}

impl ServiceFramework for BsdCfg {
    fn get_service_name(&self) -> &str {
        &self.name
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct BsdNu {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl BsdNu {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[(0, None, "CreateUserService")]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for BsdNu {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }
    fn service_name(&self) -> &str {
        "bsd:nu"
    }
}
impl ServiceFramework for BsdNu {
    fn get_service_name(&self) -> &str {
        "bsd:nu"
    }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn eventfd_descriptors_share_flags_and_select_counts_each_set() {
        let mut bsd = Bsd::new("bsd:s", false);
        let (fd, errno) = bsd.event_fd_impl(2, EventFdState::FLAG_NONBLOCK);
        assert_eq!((fd, errno), (0, Errno::SUCCESS));
        assert!(bsd.get_socket(fd).is_none());
        let duplicate = bsd.duplicate_socket_impl(fd).unwrap();
        assert_eq!(bsd.fcntl_impl(duplicate, FcntlCmd::GETFL, 0), (FLAG_O_NONBLOCK, Errno::SUCCESS));
        bsd.fcntl_impl(duplicate, FcntlCmd::SETFL, 0);
        assert_eq!(bsd.fcntl_impl(fd, FcntlCmd::GETFL, 0), (0, Errno::SUCCESS));
        let request = SelectRequest { nfds: 1, fd_sets: [vec![1;8], vec![1;8], vec![1;8]], deadline: Some(std::time::Instant::now()), ..SelectRequest::default() };
        let mut ready = std::array::from_fn(|_| Vec::new());
        assert_eq!(bsd.select_impl(&request, &mut ready, &mut SelectWait::default()), Some((2, Errno::SUCCESS)));
        assert_eq!(ready[0], vec![1,0,0,0,0,0,0,0]); assert_eq!(ready[1], ready[0]); assert_eq!(ready[2], vec![0;8]);
        assert_eq!(bsd.close_impl(fd), Errno::SUCCESS);
        let mut bytes = [0;8];
        assert_eq!(bsd.read_impl(&mut HLERequestContext::new(), duplicate, &mut bytes), Some((8,Errno::SUCCESS)));
        assert_eq!(u64::from_le_bytes(bytes),2);
        let event = bsd.copy_file_descriptor(duplicate).unwrap().0.event_fd.unwrap();
        assert_eq!(bsd.close_impl(duplicate),Errno::SUCCESS);
        assert_eq!(event.read(&mut bytes, &mut 0),(-1,Errno::BADF));
        assert_eq!(bsd.event_fd_impl(u64::MAX, 0),(-1,Errno::INVAL));
    }

    #[test]
    fn notifier_retries_eventfd_native_reads_deadlines_and_cancellation() {
        const CHILD: &str = "RUZU_TEST_BSD_NOTIFIER";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "hle::service::sockets::bsd::tests::notifier_retries_eventfd_native_reads_deadlines_and_cancellation", "--nocapture"])
                .env(CHILD,"1").status().unwrap();
            assert!(status.success()); return;
        }
        use std::time::{Duration, Instant};
        use crate::internal_network::network as net;
        let _network = net::NetworkInstance::new(); net::restart_socket_operations();
        let table = Arc::new(Mutex::new(DescriptorTable::default()));
        let (tx, rx) = std::sync::mpsc::channel();
        let notifier = SelectNotifier::new(Arc::clone(&table), Arc::new(move || { tx.send(()).unwrap(); }), |task| std::thread::spawn(task));
        let mut bsd = Bsd::with_notifier("bsd:s", false, Arc::clone(&table), Arc::clone(&notifier));
        let (fd, _) = bsd.event_fd_impl(0, 0); let mut bytes = [0;8]; let mut read = HLERequestContext::new();
        assert_eq!(bsd.read_impl(&mut read, fd, &mut bytes),None); assert!(read.get_is_deferred());
        assert_eq!(bsd.write_impl(&mut HLERequestContext::new(),fd,&2u64.to_le_bytes()),Some((8,Errno::SUCCESS)));
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(bsd.read_impl(&mut read, fd, &mut bytes),Some((8,Errno::SUCCESS))); assert_eq!(u64::from_le_bytes(bytes),2);
        let duplicate = bsd.duplicate_socket_impl(fd).unwrap();
        assert_eq!(bsd.read_impl(&mut read,fd,&mut bytes),None);
        assert_eq!(bsd.close_impl(fd),Errno::SUCCESS); assert_eq!(bsd.duplicate_socket_impl(duplicate),Ok(fd));
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(bsd.read_impl(&mut read,fd,&mut bytes),Some((-1,Errno::BADF)));
        assert_eq!(bsd.read_impl(&mut read,fd,&mut bytes),None); notifier.cancel(&read);
        assert!(!notifier.inner.state.lock().unwrap().requests.contains_key(&read.request_key()));
        while rx.try_recv().is_ok() {}
        let mut timeout_ctx = HLERequestContext::new(); let mut output = Vec::new();
        let request = SelectRequest { deadline: Some(Instant::now()+Duration::from_millis(20)), ..SelectRequest::default() };
        assert!(notifier.defer(&timeout_ctx, request, SelectWait::default()));
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let request = notifier.take(&timeout_ctx).unwrap(); assert!(Instant::now() >= request.deadline.unwrap());
        let mut receiver = Socket::new(); assert_eq!(receiver.initialize(net::Domain::INET,net::Type::DGRAM,net::Protocol::UDP),NetErrno::Success);
        assert_eq!(receiver.bind(NetSockAddrIn{family:Some(net::Domain::INET),ip:[127,0,0,1],portno:0}),NetErrno::Success);
        let address = receiver.get_sock_name().0;
        table.lock().unwrap().entries[2] = Some(FileDescriptor{ socket:Some(Arc::new(Mutex::new(Box::new(receiver)))), ..FileDescriptor::default() });
        assert_eq!(bsd.read_impl(&mut read,2,&mut bytes),None);
        let mut sender = Socket::new(); sender.initialize(net::Domain::INET,net::Type::DGRAM,net::Protocol::UDP);
        assert_eq!(sender.send_to(0,&[42],Some(&address)),(1,NetErrno::Success));
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(bsd.read_impl(&mut read,2,&mut bytes),Some((1,Errno::SUCCESS))); assert_eq!(bytes[0],42);
        let input = [fd.to_le_bytes().as_slice(), &PollEvents::OUT.bits().to_le_bytes(), &[0,0]].concat(); output.resize(8,0);
        assert_eq!(bsd.poll_impl(&mut timeout_ctx,&mut output,&input,1,0),Some((1,Errno::SUCCESS)));
        assert_eq!(bsd.read_impl(&mut read,2,&mut bytes),None);
        // Closing a host socket waits for the notifier's poll to release it.
        assert_eq!(bsd.close_impl(2),Errno::SUCCESS);
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(bsd.read_impl(&mut read,2,&mut bytes),Some((-1,Errno::BADF)));
        assert_eq!(bsd.read_impl(&mut read,fd,&mut bytes),None);
        net::cancel_pending_socket_operations();
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(bsd.read_impl(&mut read,fd,&mut bytes),Some((-1,Errno::INTR)));
        drop(bsd); drop(notifier); net::restart_socket_operations();
    }

    #[test]
    fn descriptor_table_shares_ownership_and_distinguishes_reused_slots() {
        use crate::internal_network::socket_proxy::ProxySocket;
        let table = Arc::new(Mutex::new(DescriptorTable::default()));
        let first = Bsd::with_descriptor_table("bsd:s", false, Arc::clone(&table));
        let mut second = Bsd::with_descriptor_table("bsd:u", true, Arc::clone(&table));
        table.lock().unwrap().entries[0] = Some(FileDescriptor {
            socket: Some(Arc::new(Mutex::new(Box::new(ProxySocket::new())))),
            ..FileDescriptor::default()
        });
        let (retained, generation) = first.copy_file_descriptor(0).unwrap();
        let duplicate = second.duplicate_socket_impl(0).unwrap();
        assert!(Arc::ptr_eq(retained.socket.as_ref().unwrap(), second.get_socket(duplicate).as_ref().unwrap()));
        assert_eq!(second.close_impl(0), Errno::SUCCESS);
        assert!(first.get_socket(duplicate).is_none());
        assert!(first.copy_file_descriptor(0).is_none());
        table.lock().unwrap().entries[0] = Some(FileDescriptor {
            socket: Some(Arc::new(Mutex::new(Box::new(ProxySocket::new())))),
            ..FileDescriptor::default()
        });
        let (replacement, next_generation) = first.copy_file_descriptor(0).unwrap();
        assert_eq!(next_generation, generation.wrapping_add(1));
        assert!(!Arc::ptr_eq(retained.socket.as_ref().unwrap(), replacement.socket.as_ref().unwrap()));
        assert_eq!(second.close_impl(duplicate), Errno::SUCCESS);
        assert!(first.get_socket(0).unwrap().lock().unwrap().is_opened());
    }

    #[test]
    fn socket_timeval_validation_and_shared_timeout_match_upstream() {
        use crate::internal_network::socket_proxy::ProxySocket;
        let mut bsd = Bsd::new("bsd:s", false);
        bsd.file_descriptors.lock().unwrap().entries[0] = Some(FileDescriptor {
            socket: Some(Arc::new(Mutex::new(Box::new(ProxySocket::new())))),
            ..FileDescriptor::default()
        });
        let duplicate = bsd.duplicate_socket_impl(0).unwrap();
        let level = SocketLevel::SOCKET as u32;
        assert_eq!(bsd.set_sock_opt_impl(0, level, OptName::RCVTIMEO, &[0; 4]), Errno::INVAL);
        for (seconds, micros, expected) in [(-1i64,0i64,Errno::DOM),(0,1_000_000,Errno::DOM),(0,1,Errno::SUCCESS),(1,999,Errno::SUCCESS)] {
            let bytes: Vec<_> = seconds.to_le_bytes().into_iter().chain(micros.to_le_bytes()).collect();
            assert_eq!(bsd.set_sock_opt_impl(0, level, OptName::RCVTIMEO, &bytes), expected);
            if expected == Errno::SUCCESS {
                assert_eq!(bsd.copy_descriptor(duplicate).unwrap().receive_timeout, (seconds*1000+(micros+999)/1000) as u32);
            }
        }
    }

    #[test]
    fn eventfd_counter_limits_readiness_and_semaphore_match_upstream() {
        let state = EventFdState::default(); let mut version = u64::MAX; let mut bytes = [0xaa;16];
        assert_eq!(state.readiness(), (0,false,true));
        assert_eq!(state.read(&mut bytes,&mut version),(-1,Errno::AGAIN)); assert_eq!(version,0);
        assert_eq!(bytes,[0xaa;16]);
        assert_eq!(state.write(&u64::MAX.to_le_bytes(),&mut version),(-1,Errno::INVAL));
        assert_eq!(state.write(&EventFdState::MAX_COUNTER.to_le_bytes(),&mut version),(8,Errno::SUCCESS));
        assert_eq!(state.readiness(),(1,true,false));
        assert_eq!(state.write(&1u64.to_le_bytes(),&mut version),(-1,Errno::AGAIN)); assert_eq!(version,1);
        assert_eq!(state.read(&mut bytes,&mut version),(8,Errno::SUCCESS));
        assert_eq!(u64::from_le_bytes(bytes[..8].try_into().unwrap()),EventFdState::MAX_COUNTER);
        assert_eq!(&bytes[8..],&[0xaa;8]);
        assert_eq!(state.readiness(),(2,false,true));
        let semaphore = EventFdState { semaphore: true, ..EventFdState::default() };
        semaphore.write(&2u64.to_le_bytes(),&mut version);
        assert_eq!(semaphore.read(&mut bytes,&mut version),(8,Errno::SUCCESS));
        assert_eq!(u64::from_le_bytes(bytes[..8].try_into().unwrap()),1);
        assert_eq!(semaphore.readiness(),(2,true,true));
        semaphore.descriptor_closed(); assert_eq!(semaphore.readiness().0,3);
        semaphore.close();
        assert_eq!(semaphore.read(&mut bytes,&mut version),(-1,Errno::BADF));
        assert_eq!(semaphore.write(&0u64.to_le_bytes(),&mut version),(-1,Errno::BADF));
        assert_eq!(semaphore.read(&mut bytes[..7],&mut version),(-1,Errno::INVAL));
    }

    #[test]
    fn nx2_register_client_clears_errno_in_response() {
        let bsd = Bsd::new("bsd:s", false);
        let mut ctx = HLERequestContext::new();
        ctx.command_buffer_mut().fill(0xdead_beef);
        ctx.command_buffer_mut()[2] = 0;
        ctx.command_buffer_mut()[3] = 0;
        (bsd.handlers[&36].handler_callback.unwrap())(&bsd, &mut ctx);
        assert_eq!(ctx.command_buffer()[6], RESULT_SUCCESS.get_inner_value());
        assert_eq!(ctx.command_buffer()[7], 0);
        assert_eq!(ctx.command_buffer()[8], 0);
    }

    use super::*;

    #[test]
    fn duplicate_socket_preserves_shared_object_and_descriptor_local_flags() {
        use crate::internal_network::socket_proxy::ProxySocket;

        let mut bsd = Bsd::new("bsd:s", false);
        bsd.file_descriptors.lock().unwrap().entries[0] = Some(FileDescriptor {
            socket: Some(Arc::new(Mutex::new(Box::new(ProxySocket::new())))),
            flags: FLAG_O_NONBLOCK,
            is_connection_based: false,
            ..FileDescriptor::default()
        });
        let source = bsd.get_socket(0).unwrap();
        let weak = Arc::downgrade(&source);
        // ProxySocket has no OS descriptor: duplication must not call dup().
        assert_eq!(source.lock().unwrap().get_fd(), net_sockets::INVALID_SOCKET);
        let duplicate_fd = bsd.duplicate_socket_impl(0).unwrap();
        assert_eq!(duplicate_fd, 1);
        let duplicate = bsd.get_socket(duplicate_fd).unwrap();
        assert!(Arc::ptr_eq(&source, &duplicate));
        assert_eq!(bsd.fcntl_impl(1, FcntlCmd::GETFL, 0), (FLAG_O_NONBLOCK, Errno::SUCCESS));
        assert_eq!(bsd.fcntl_impl(1, FcntlCmd::SETFL, 0), (0, Errno::SUCCESS));
        assert_eq!(bsd.fcntl_impl(0, FcntlCmd::GETFL, 0), (FLAG_O_NONBLOCK, Errno::SUCCESS));
        assert!(!bsd.file_descriptors.lock().unwrap().entries[1].as_ref().unwrap().is_connection_based);
        assert!(source.lock().unwrap().is_opened());
        assert_eq!(bsd.close_impl(1), Errno::SUCCESS);
        assert!(bsd.get_socket(1).is_none());
        // Eden explicitly closes the shared socket, including all aliases.
        assert!(!source.lock().unwrap().is_opened());
        assert!(bsd.get_socket(0).is_none());
        assert_eq!(bsd.close_impl(0), Errno::SUCCESS);
        drop(bsd);
        assert!(weak.upgrade().is_some());
        drop(source);
        assert!(weak.upgrade().is_some());
        drop(duplicate);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn duplicate_socket_validates_bounds_and_table_capacity() {
        use crate::internal_network::socket_proxy::ProxySocket;
        let mut bsd = Bsd::new("bsd:s", false);
        for fd in [-1, 0, MAX_FD as i32, i32::MAX] {
            assert!(bsd.get_socket(fd).is_none());
            assert_eq!(bsd.duplicate_socket_impl(fd), Err(Errno::BADF));
        }
        bsd.file_descriptors.lock().unwrap().entries[0] = Some(FileDescriptor {
            socket: Some(Arc::new(Mutex::new(Box::new(ProxySocket::new())))),
            flags: 0,
            is_connection_based: true,
            ..FileDescriptor::default()
        });
        for fd in 1..MAX_FD {
            assert_eq!(bsd.duplicate_socket_impl(0), Ok(fd as i32));
            assert!(bsd.file_descriptors.lock().unwrap().entries[fd].as_ref().unwrap().is_connection_based);
        }
        assert_eq!(bsd.duplicate_socket_impl(0), Err(Errno::MFILE));
        let input = PollFD { fd: MAX_FD as i32, events: 0, revents: 0 };
        let bytes = unsafe { std::slice::from_raw_parts(
            &input as *const PollFD as *const u8, std::mem::size_of::<PollFD>(),
        ) };
        let mut output = vec![0; bytes.len()];
        assert_eq!(bsd.poll_impl(&mut HLERequestContext::new(), &mut output, bytes, 1, 0), Some((1, Errno::SUCCESS)));
        assert_eq!(u16::from_le_bytes(output[6..8].try_into().unwrap()), PollEvents::NVAL.bits());
    }

    #[test]
    fn airplane_mode_blocks_only_connection_based_sockets() {
        assert!(socket_blocked_by_airplane_mode(true, true));
        assert!(!socket_blocked_by_airplane_mode(true, false));
        assert!(!socket_blocked_by_airplane_mode(false, true));
    }
    use crate::hle::service::hle_ipc::SessionRequestHandlerPtr;
    use std::sync::{Arc, Mutex};

    #[test]
    fn unknown_set_sock_opt_name_reaches_upstream_default_case() {
        let _network = crate::internal_network::network::NetworkInstance::new();
        let mut bsd = Bsd::new("bsd:u", true);
        let (fd, errno) = bsd.socket_impl(Domain::INET, Type::DGRAM, Protocol::UDP);
        assert_eq!(errno, Errno::SUCCESS);
        assert!(fd >= 0);

        let optval = 0u32.to_ne_bytes();
        assert_eq!(
            bsd.set_sock_opt_impl(fd, SocketLevel::SOCKET as u32, OptName(0x1), &optval),
            Errno::SUCCESS
        );
        assert_eq!(bsd.close_impl(fd), Errno::SUCCESS);
    }

    #[cfg(unix)]
    #[test]
    fn connecting_an_already_connected_socket_matches_upstream_success() {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let guest_address = SockAddrIn {
            len: std::mem::size_of::<SockAddrIn>() as u8,
            family: Domain::INET.0 as u8,
            portno: port.swap_bytes(),
            ip: std::net::Ipv4Addr::LOCALHOST.octets(),
            zeroes: [0; 8],
        };
        let address = unsafe {
            std::slice::from_raw_parts(
                &guest_address as *const SockAddrIn as *const u8,
                std::mem::size_of::<SockAddrIn>(),
            )
        };

        let mut bsd = Bsd::new("bsd:u", true);
        let (fd, error) = bsd.socket_impl(Domain::INET, Type::STREAM, Protocol::TCP);
        assert_eq!(error, Errno::SUCCESS);
        assert_eq!(bsd.connect_impl(fd, address), Errno::SUCCESS);
        assert_eq!(bsd.connect_impl(fd, address), Errno::SUCCESS);
        assert_eq!(bsd.close_impl(fd), Errno::SUCCESS);
    }

    #[test]
    fn shared_bsd_handler_exposes_one_descriptor_table() {
        let _network = crate::internal_network::network::NetworkInstance::new();
        let handler: SessionRequestHandlerPtr = Arc::new(Mutex::new(Bsd::new("bsd:u", true)));
        let first = handler
            .as_any()
            .downcast_ref::<Mutex<Bsd>>()
            .expect("shared BSD handler");
        let (fd, errno) =
            first
                .lock()
                .unwrap()
                .socket_impl(Domain::INET, Type::DGRAM, Protocol::UDP);
        assert_eq!(errno, Errno::SUCCESS);

        let cloned_handler = Arc::clone(&handler);
        let second = cloned_handler
            .as_any()
            .downcast_ref::<Mutex<Bsd>>()
            .expect("same shared BSD handler");
        assert!(second.lock().unwrap().get_socket(fd).is_some());
    }

    #[test]
    fn zero_descriptor_poll_returns_without_an_output_buffer() {
        let bsd = Bsd::new("bsd:u", true);
        let mut ctx = HLERequestContext::new();
        ctx.command_buffer_mut()[2] = 0;
        ctx.command_buffer_mut()[3] = 0;

        Bsd::poll_handler(&bsd, &mut ctx);

        assert_eq!(ctx.command_buffer()[6], RESULT_SUCCESS.get_inner_value());
        assert_eq!(ctx.command_buffer()[8], (-1_i32) as u32);
        assert_eq!(ctx.command_buffer()[9], Errno::SUCCESS as u32);
    }
}
