// SPDX-FileCopyrightText: Copyright 2022 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/debugger/debugger.h and debugger.cpp.
//! Top-level debugger server and connection lifecycle.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::debugger::debugger_interface::{DebuggerAction, DebuggerBackend, DebuggerFrontend};
use crate::debugger::gdbstub::GdbStub;
use crate::hle::kernel::k_process::{DebugWatchpoint, ProcessLock};
use crate::hle::kernel::k_thread::{KThreadLock, StepState, SuspendType};

#[derive(Debug, Clone)]
enum SignalType {
    Stopped,
    Watchpoint,
    ShuttingDown,
}

#[derive(Clone)]
struct SignalInfo {
    type_: SignalType,
    thread: Option<Arc<KThreadLock>>,
    watchpoint: Option<DebugWatchpoint>,
}

#[derive(Default)]
struct SharedConnectionState {
    connected: bool,
    stopped: bool,
}

struct ConnectionBackend {
    client_socket: TcpStream,
    client_data: [u8; 4096],
    active_thread: Option<Arc<KThreadLock>>,
    stop_requested: Arc<AtomicBool>,
}

impl ConnectionBackend {
    fn new(client_socket: TcpStream, stop_requested: Arc<AtomicBool>) -> io::Result<Self> {
        client_socket.set_nonblocking(true)?;
        Ok(Self {
            client_socket,
            client_data: [0; 4096],
            active_thread: None,
            stop_requested,
        })
    }

    fn read_available(&mut self) -> io::Result<Option<Vec<u8>>> {
        match self.client_socket.read(&mut self.client_data) {
            Ok(0) => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "debugger client disconnected",
            )),
            Ok(size) => Ok(Some(self.client_data[..size].to_vec())),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error),
        }
    }
}

impl DebuggerBackend for ConnectionBackend {
    fn read_from_client(&mut self) -> &[u8] {
        let size = loop {
            match self.client_socket.read(&mut self.client_data) {
                Ok(size) => break size,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if self.stop_requested.load(Ordering::Acquire) {
                        break 0;
                    }
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => {
                    log::error!("Debugger client read failed: {error}");
                    break 0;
                }
            }
        };
        &self.client_data[..size]
    }

    fn write_to_client(&mut self, data: &[u8]) {
        let mut remaining = data;
        while !remaining.is_empty() {
            match self.client_socket.write(remaining) {
                Ok(0) => return,
                Ok(size) => remaining = &remaining[size..],
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if self.stop_requested.load(Ordering::Acquire) {
                        return;
                    }
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => {
                    log::error!("Debugger client write failed: {error}");
                    return;
                }
            }
        }
    }

    fn get_active_thread(&self) -> Option<Arc<KThreadLock>> {
        self.active_thread.clone()
    }

    fn set_active_thread(&mut self, thread: Arc<KThreadLock>) {
        self.active_thread = Some(thread);
    }
}

struct DebuggerImpl {
    port: u16,
    signal_sender: Sender<SignalInfo>,
    connection_state: Arc<Mutex<SharedConnectionState>>,
    stop_requested: Arc<AtomicBool>,
    connection_thread: Option<JoinHandle<()>>,
}

impl DebuggerImpl {
    fn new(
        debug_process: Arc<ProcessLock>,
        port: u16,
        shutdown_requested: Arc<AtomicBool>,
    ) -> Option<Self> {
        log::info!("Starting debugger server on port {port}...");
        let listener = match TcpListener::bind(("0.0.0.0", port)) {
            Ok(listener) => listener,
            Err(error) => {
                log::error!("Stopping debugger server: {error}");
                return None;
            }
        };
        if let Err(error) = listener.set_nonblocking(true) {
            log::error!("Stopping debugger server: {error}");
            return None;
        }
        let actual_port = listener.local_addr().map_or(port, |address| address.port());

        let (signal_sender, signal_receiver) = mpsc::channel();
        let connection_state = Arc::new(Mutex::new(SharedConnectionState::default()));
        let thread_connection_state = Arc::clone(&connection_state);
        let stop_requested = Arc::new(AtomicBool::new(false));
        let thread_stop_requested = Arc::clone(&stop_requested);
        let connection_thread = thread::Builder::new()
            .name("Debugger".to_owned())
            .spawn(move || {
                run_server(
                    listener,
                    debug_process,
                    signal_receiver,
                    thread_connection_state,
                    thread_stop_requested,
                    shutdown_requested,
                );
            })
            .ok()?;

        Some(Self {
            port: actual_port,
            signal_sender,
            connection_state,
            stop_requested,
            connection_thread: Some(connection_thread),
        })
    }

    fn signal_debugger(&self, signal_info: SignalInfo) -> bool {
        let mut state = self.connection_state.lock().unwrap();
        if state.stopped || !state.connected {
            return false;
        }
        state.stopped = true;
        if self.signal_sender.send(signal_info).is_err() {
            state.connected = false;
            return false;
        }
        true
    }
}

impl Drop for DebuggerImpl {
    fn drop(&mut self) {
        self.stop_requested.store(true, Ordering::Release);
        if let Some(thread) = self.connection_thread.take() {
            let _ = thread.join();
        }
        log::info!("Shut down debugger server on port {}", self.port);
    }
}

fn process_threads(process: &Arc<ProcessLock>) -> Vec<Arc<KThreadLock>> {
    let process = process.lock().unwrap();
    process
        .thread_list
        .iter()
        .filter_map(|thread_id| process.get_thread_by_thread_id(*thread_id))
        .collect()
}

fn pause_emulation(process: &Arc<ProcessLock>) {
    for thread in process_threads(process) {
        thread.lock().unwrap().request_suspend(SuspendType::Debug);
    }
}

fn resume_emulation(process: &Arc<ProcessLock>, except_thread_id: Option<u64>) {
    for thread in process_threads(process) {
        let mut thread = thread.lock().unwrap();
        if except_thread_id == Some(thread.get_thread_id()) {
            continue;
        }
        thread.set_step_state(StepState::NotStepping);
        thread.resume(SuspendType::Debug);
    }
}

fn resume_threads(threads: Vec<Arc<KThreadLock>>, except_thread_id: Option<u64>) {
    for thread in threads {
        let mut thread = thread.lock().unwrap();
        if except_thread_id == Some(thread.get_thread_id()) {
            continue;
        }
        thread.set_step_state(StepState::NotStepping);
        thread.resume(SuspendType::Debug);
    }
}

fn update_active_thread(backend: &mut ConnectionBackend, process: &Arc<ProcessLock>) {
    let threads = process_threads(process);
    if let Some(active) = backend.active_thread.as_ref() {
        let active_id = active.lock().unwrap().get_thread_id();
        if threads
            .iter()
            .any(|thread| thread.lock().unwrap().get_thread_id() == active_id)
        {
            return;
        }
    }
    backend.active_thread = threads.into_iter().next();
}

fn mark_resumed(state: &Arc<Mutex<SharedConnectionState>>) {
    state.lock().unwrap().stopped = false;
}

fn execute_actions(
    frontend: &mut GdbStub,
    backend: &mut ConnectionBackend,
    process: &Arc<ProcessLock>,
    state: &Arc<Mutex<SharedConnectionState>>,
    shutdown_requested: &AtomicBool,
    actions: Vec<DebuggerAction>,
) {
    for action in actions {
        match action {
            DebuggerAction::Interrupt => {
                state.lock().unwrap().stopped = true;
                pause_emulation(process);
                update_active_thread(backend, process);
                if let Some(thread) = backend.get_active_thread() {
                    frontend.stopped(backend, thread);
                }
            }
            DebuggerAction::Continue => {
                mark_resumed(state);
                resume_emulation(process, None);
            }
            DebuggerAction::ContinueThreads => {
                mark_resumed(state);
                resume_threads(std::mem::take(&mut frontend.resume_threads), None);
            }
            DebuggerAction::StepThread => {
                let active = backend.get_active_thread();
                let active_id = active
                    .as_ref()
                    .map(|thread| thread.lock().unwrap().get_thread_id());
                mark_resumed(state);
                if let Some(thread) = active {
                    let mut thread = thread.lock().unwrap();
                    thread.set_step_state(StepState::StepPending);
                    thread.resume(SuspendType::Debug);
                }
                resume_threads(std::mem::take(&mut frontend.resume_threads), active_id);
            }
            DebuggerAction::ShutdownEmulation => {
                shutdown_requested.store(true, Ordering::Release);
            }
        }
    }
}

fn handle_signal(
    signal: SignalInfo,
    frontend: &mut GdbStub,
    backend: &mut ConnectionBackend,
    process: &Arc<ProcessLock>,
) -> bool {
    match signal.type_ {
        SignalType::Stopped | SignalType::Watchpoint => {
            pause_emulation(process);
            backend.active_thread = signal.thread.clone();
            update_active_thread(backend, process);
            if let Some(thread) = backend.get_active_thread() {
                if let Some(watchpoint) = signal.watchpoint {
                    frontend.watchpoint(backend, thread, watchpoint);
                } else {
                    frontend.stopped(backend, thread);
                }
            }
            true
        }
        SignalType::ShuttingDown => {
            frontend.shutting_down(backend);
            let _ = backend.client_socket.shutdown(Shutdown::Both);
            false
        }
    }
}

fn run_connection(
    listener: &TcpListener,
    peer: TcpStream,
    debug_process: &Arc<ProcessLock>,
    signal_receiver: &Receiver<SignalInfo>,
    connection_state: &Arc<Mutex<SharedConnectionState>>,
    stop_requested: &Arc<AtomicBool>,
    shutdown_requested: &AtomicBool,
) -> Option<TcpStream> {
    log::info!("Accepting new debugger peer connection");
    pause_emulation(debug_process);
    {
        let mut state = connection_state.lock().unwrap();
        state.connected = true;
        state.stopped = true;
    }

    let mut backend = match ConnectionBackend::new(peer, Arc::clone(stop_requested)) {
        Ok(backend) => backend,
        Err(error) => {
            log::error!("Failed to initialize debugger connection: {error}");
            connection_state.lock().unwrap().connected = false;
            return None;
        }
    };
    update_active_thread(&mut backend, debug_process);
    let mut frontend = GdbStub::new(Arc::clone(debug_process));
    frontend.connected(&mut backend);

    while !stop_requested.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((next_peer, _)) => {
                let _ = backend.client_socket.shutdown(Shutdown::Both);
                return Some(next_peer);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => log::error!("Debugger accept failed: {error}"),
        }

        loop {
            match signal_receiver.try_recv() {
                Ok(signal) => {
                    if !handle_signal(signal, &mut frontend, &mut backend, debug_process) {
                        connection_state.lock().unwrap().connected = false;
                        return None;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return None,
            }
        }

        match backend.read_available() {
            Ok(Some(data)) => {
                let actions = frontend.client_data(&mut backend, &data);
                execute_actions(
                    &mut frontend,
                    &mut backend,
                    debug_process,
                    connection_state,
                    shutdown_requested,
                    actions,
                );
            }
            Ok(None) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => {
                log::error!("Debugger client read failed: {error}");
                break;
            }
        }

        thread::sleep(Duration::from_millis(1));
    }

    connection_state.lock().unwrap().connected = false;
    None
}

fn run_server(
    listener: TcpListener,
    debug_process: Arc<ProcessLock>,
    signal_receiver: Receiver<SignalInfo>,
    connection_state: Arc<Mutex<SharedConnectionState>>,
    stop_requested: Arc<AtomicBool>,
    shutdown_requested: Arc<AtomicBool>,
) {
    let mut pending_peer = None;
    while !stop_requested.load(Ordering::Acquire) {
        let peer = match pending_peer.take() {
            Some(peer) => peer,
            None => match listener.accept() {
                Ok((peer, _)) => peer,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(error) => {
                    log::error!("Stopping debugger server: {error}");
                    return;
                }
            },
        };

        pending_peer = run_connection(
            &listener,
            peer,
            &debug_process,
            &signal_receiver,
            &connection_state,
            &stop_requested,
            &shutdown_requested,
        );
    }
}

/// Top-level debugger that manages debug connections.
///
/// Corresponds to upstream `Core::Debugger`.
pub struct Debugger {
    impl_: Option<DebuggerImpl>,
}

impl Debugger {
    pub fn new(
        debug_process: Arc<ProcessLock>,
        server_port: u16,
        shutdown_requested: Arc<AtomicBool>,
    ) -> Self {
        let impl_ = DebuggerImpl::new(debug_process, server_port, shutdown_requested);
        Self { impl_ }
    }

    pub fn is_initialized(&self) -> bool {
        self.impl_.is_some()
    }

    pub fn notify_thread_stopped(&self, thread: Arc<KThreadLock>) -> bool {
        self.impl_.as_ref().is_some_and(|implementation| {
            implementation.signal_debugger(SignalInfo {
                type_: SignalType::Stopped,
                thread: Some(thread),
                watchpoint: None,
            })
        })
    }

    pub fn notify_shutdown(&self) {
        if let Some(implementation) = self.impl_.as_ref() {
            let _ = implementation.signal_debugger(SignalInfo {
                type_: SignalType::ShuttingDown,
                thread: None,
                watchpoint: None,
            });
        }
    }

    pub fn notify_thread_watchpoint(
        &self,
        thread: Arc<KThreadLock>,
        watchpoint: DebugWatchpoint,
    ) -> bool {
        self.impl_.as_ref().is_some_and(|implementation| {
            implementation.signal_debugger(SignalInfo {
                type_: SignalType::Watchpoint,
                thread: Some(thread),
                watchpoint: Some(watchpoint),
            })
        })
    }

    #[cfg(test)]
    fn port(&self) -> Option<u16> {
        self.impl_
            .as_ref()
            .map(|implementation| implementation.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::kernel::k_process::KProcess;
    use std::net::SocketAddr;

    #[test]
    fn debugger_refuses_a_port_that_is_already_bound() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind occupied port");
        let port = listener.local_addr().expect("local address").port();
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let debugger = Debugger::new(process, port, Arc::new(AtomicBool::new(false)));

        assert!(!debugger.is_initialized());
    }

    #[test]
    fn debugger_binds_an_ephemeral_port_and_stops_on_drop() {
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let debugger = Debugger::new(process, 0, Arc::new(AtomicBool::new(false)));

        assert!(debugger.is_initialized());
        assert_ne!(debugger.port(), Some(0));
    }

    #[test]
    fn debugger_routes_remote_packets_to_the_gdb_frontend() {
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let debugger = Debugger::new(process, 0, Arc::new(AtomicBool::new(false)));
        let port = debugger.port().expect("debugger listener port");
        let mut client = TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], port)))
            .expect("connect debugger client");
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("set read timeout");
        client
            .write_all(b"$qSupported#37")
            .expect("write qSupported packet");

        let mut response = Vec::new();
        while !response.windows(3).any(|bytes| bytes[0] == b'#') {
            let mut chunk = [0u8; 512];
            let size = client.read(&mut chunk).expect("read qSupported reply");
            response.extend_from_slice(&chunk[..size]);
        }
        let response = std::str::from_utf8(&response).expect("utf8 GDB response");

        assert!(response.starts_with("+$PacketSize=4000;"), "{response:?}");
    }
}
