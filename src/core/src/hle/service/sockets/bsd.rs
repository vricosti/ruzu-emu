// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/sockets/bsd.h
//! Port of zuyu/src/core/hle/service/sockets/bsd.cpp
//!
//! BSD socket service -- "bsd:u" and "bsd:s".

use std::collections::BTreeMap;

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

/// Per-file-descriptor state.
///
/// Corresponds to `BSD::FileDescriptor` in upstream bsd.h.
pub struct FileDescriptor {
    /// Platform socket (corresponds to upstream shared_ptr<SocketBase>).
    pub socket: Box<dyn SocketBase>,
    pub flags: i32,
    pub is_connection_based: bool,
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
    file_descriptors: [Option<FileDescriptor>; MAX_FD],
    name: &'static str,
    is_user: bool,
}

impl Bsd {
    pub fn new(name: &'static str, is_user: bool) -> Self {
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
        ]);

        Self {
            handlers,
            handlers_tipc: BTreeMap::new(),
            file_descriptors: std::array::from_fn(|_| None),
            name,
            is_user,
        }
    }

    /// Returns whether this is a privileged (bsd:s) instance.
    pub fn is_privileged(&self) -> bool {
        !self.is_user
    }

    // --- Internal implementation methods ---

    /// Find the first free file descriptor slot.
    ///
    /// Corresponds to `BSD::FindFreeFileDescriptorHandle` in upstream bsd.cpp.
    fn find_free_file_descriptor_handle(&self) -> i32 {
        for (i, fd) in self.file_descriptors.iter().enumerate() {
            if fd.is_none() {
                return i as i32;
            }
        }
        -1
    }

    /// Check if a file descriptor index is valid.
    ///
    /// Corresponds to `BSD::IsFileDescriptorValid` in upstream bsd.cpp.
    fn is_file_descriptor_valid(&self, fd: i32) -> bool {
        if fd > MAX_FD as i32 || fd < 0 {
            log::error!("Invalid file descriptor handle={}", fd);
            return false;
        }
        if self.file_descriptors[fd as usize].is_none() {
            log::error!("File descriptor handle={} is not allocated", fd);
            return false;
        }
        true
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

        let fd = self.find_free_file_descriptor_handle();
        if fd < 0 {
            log::error!("No more file descriptors available");
            return (-1, Errno::MFILE);
        }

        log::info!("New socket fd={}", fd);

        let mut socket = Socket::new();
        let errno = socket.initialize(
            translate_domain(domain),
            translate_type(ty),
            translate_protocol(protocol),
        );
        if errno != NetErrno::Success {
            return (-1, translate_errno(errno));
        }

        self.file_descriptors[fd as usize] = Some(FileDescriptor {
            socket: Box::new(socket),
            flags: 0,
            is_connection_based: is_connection_based(ty),
        });

        if socket_blocked_by_airplane_mode(
            *common::settings::values().airplane_mode.get_value(),
            self.file_descriptors[fd as usize]
                .as_ref()
                .is_some_and(|descriptor| descriptor.is_connection_based),
        ) {
            log::error!("Airplane mode is enabled, cannot create socket");
            return (-1, Errno::NOTCONN);
        }

        (fd, Errno::SUCCESS)
    }

    /// PollImpl -- poll file descriptors.
    ///
    /// Corresponds to `BSD::PollImpl` in upstream bsd.cpp.
    pub fn poll_impl(
        &self,
        write_buffer: &mut [u8],
        read_buffer: &[u8],
        nfds: i32,
        timeout: i32,
    ) -> (i32, Errno) {
        if nfds <= 0 {
            // When no entries are provided, -1 is returned with errno zero
            return (-1, Errno::SUCCESS);
        }

        let poll_fd_size = std::mem::size_of::<PollFD>();
        if read_buffer.len() < nfds as usize * poll_fd_size {
            return (-1, Errno::INVAL);
        }
        if write_buffer.len() < nfds as usize * poll_fd_size {
            return (-1, Errno::INVAL);
        }

        // Validate timeout
        if timeout >= 0 {
            let seconds = timeout as i64 / 1000;
            let nanoseconds = (timeout as u64 % 1000) * 1_000_000;
            if seconds < 0 {
                return (-1, Errno::INVAL);
            }
            if nanoseconds > 999_999_999 {
                return (-1, Errno::INVAL);
            }
        } else if timeout != -1 {
            return (-1, Errno::INVAL);
        }

        // Parse poll fds from read buffer
        let mut fds = vec![PollFD::default(); nfds as usize];
        unsafe {
            std::ptr::copy_nonoverlapping(
                read_buffer.as_ptr(),
                fds.as_mut_ptr() as *mut u8,
                nfds as usize * poll_fd_size,
            );
        }

        // Validate fds
        for pollfd in fds.iter_mut() {
            if pollfd.fd > MAX_FD as i32 || pollfd.fd < 0 {
                log::error!("File descriptor handle={} is invalid", pollfd.fd);
                pollfd.revents = 0;
                return (0, Errno::SUCCESS);
            }

            if self.file_descriptors[pollfd.fd as usize].is_none() {
                log::trace!("File descriptor handle={} is not allocated", pollfd.fd);
                pollfd.revents = PollEvents::NVAL.bits();
                return (0, Errno::SUCCESS);
            }
        }

        // Build host poll fds using the real socket file descriptors
        let mut host_pollfds: Vec<net_sockets::PollFD> = fds
            .iter()
            .map(|pollfd| {
                let descriptor = self.file_descriptors[pollfd.fd as usize].as_ref().unwrap();
                net_sockets::PollFD {
                    fd: descriptor.socket.get_fd(),
                    events: translate_poll_events(PollEvents::from_bits_retain(pollfd.events))
                        .bits(),
                    revents: 0,
                }
            })
            .collect();

        let result = net_sockets::poll(&mut host_pollfds, timeout);

        // Copy revents back
        for (i, host_pollfd) in host_pollfds.iter().enumerate() {
            fds[i].revents = translate_poll_events_from_network(NetPollEvents::from_bits_retain(
                host_pollfd.revents,
            ))
            .bits();
        }

        unsafe {
            std::ptr::copy_nonoverlapping(
                fds.as_ptr() as *const u8,
                write_buffer.as_mut_ptr(),
                nfds as usize * poll_fd_size,
            );
        }

        translate_result(result)
    }

    /// AcceptImpl -- accept a connection.
    ///
    /// Corresponds to `BSD::AcceptImpl` in upstream bsd.cpp.
    pub fn accept_impl(&mut self, fd: i32, write_buffer: &mut Vec<u8>) -> (i32, Errno) {
        if !self.is_file_descriptor_valid(fd) {
            return (-1, Errno::BADF);
        }

        let new_fd = self.find_free_file_descriptor_handle();
        if new_fd < 0 {
            log::error!("No more file descriptors available");
            return (-1, Errno::MFILE);
        }

        let is_conn_based = self.file_descriptors[fd as usize]
            .as_ref()
            .unwrap()
            .is_connection_based;

        let (result, bsd_errno) = self.file_descriptors[fd as usize]
            .as_mut()
            .unwrap()
            .socket
            .accept();

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

        self.file_descriptors[new_fd as usize] = Some(FileDescriptor {
            socket: accept_result.socket.unwrap(),
            flags: 0,
            is_connection_based: is_conn_based,
        });

        (new_fd, Errno::SUCCESS)
    }

    /// BindImpl -- bind a socket to an address.
    ///
    /// Corresponds to `BSD::BindImpl` in upstream bsd.cpp.
    pub fn bind_impl(&mut self, fd: i32, addr: &[u8]) -> Errno {
        if !self.is_file_descriptor_valid(fd) {
            return Errno::BADF;
        }
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

        let descriptor = self.file_descriptors[fd as usize].as_mut().unwrap();
        translate_errno(descriptor.socket.bind(net_addr))
    }

    /// ConnectImpl -- connect to remote address.
    ///
    /// Corresponds to `BSD::ConnectImpl` in upstream bsd.cpp.
    pub fn connect_impl(&mut self, fd: i32, addr: &[u8]) -> Errno {
        if !self.is_file_descriptor_valid(fd) {
            return Errno::BADF;
        }
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

        let descriptor = self.file_descriptors[fd as usize].as_mut().unwrap();
        let result = translate_errno(descriptor.socket.connect(net_addr));
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
        if !self.is_file_descriptor_valid(fd) {
            return Errno::BADF;
        }

        let descriptor = self.file_descriptors[fd as usize].as_ref().unwrap();
        let (addr_in, bsd_errno) = descriptor.socket.get_peer_name();
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
        if !self.is_file_descriptor_valid(fd) {
            return Errno::BADF;
        }

        let descriptor = self.file_descriptors[fd as usize].as_ref().unwrap();
        let (addr_in, bsd_errno) = descriptor.socket.get_sock_name();
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
        if !self.is_file_descriptor_valid(fd) {
            return Errno::BADF;
        }
        let descriptor = self.file_descriptors[fd as usize].as_mut().unwrap();
        translate_errno(descriptor.socket.listen(backlog))
    }

    /// FcntlImpl -- file control operations.
    ///
    /// Corresponds to `BSD::FcntlImpl` in upstream bsd.cpp.
    pub fn fcntl_impl(&mut self, fd: i32, cmd: FcntlCmd, arg: i32) -> (i32, Errno) {
        if !self.is_file_descriptor_valid(fd) {
            return (-1, Errno::BADF);
        }

        let descriptor = self.file_descriptors[fd as usize].as_mut().unwrap();

        match cmd {
            FcntlCmd::GETFL => {
                assert!(arg == 0);
                (descriptor.flags, Errno::SUCCESS)
            }
            FcntlCmd::SETFL => {
                let enable = (arg & FLAG_O_NONBLOCK) != 0;
                let bsd_errno = translate_errno(descriptor.socket.set_non_block(enable));
                if bsd_errno != Errno::SUCCESS {
                    return (-1, bsd_errno);
                }
                descriptor.flags = arg;
                (0, Errno::SUCCESS)
            }
            _ => {
                log::warn!("Unimplemented cmd={cmd:?}");
                (-1, Errno::SUCCESS)
            }
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
        if !self.is_file_descriptor_valid(fd) {
            return Errno::BADF;
        }

        if level != SocketLevel::SOCKET as u32 {
            log::warn!("Unknown getsockopt level={}", level);
            return Errno::SUCCESS;
        }

        let descriptor = self.file_descriptors[fd as usize].as_ref().unwrap();

        match optname {
            OptName::ERROR => {
                let (pending_err, getsockopt_err) = descriptor.socket.get_pending_error();
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
        if !self.is_file_descriptor_valid(fd) {
            return Errno::BADF;
        }

        if level != SocketLevel::SOCKET as u32 {
            log::warn!("Unknown setsockopt level={}", level);
            return Errno::SUCCESS;
        }

        let descriptor = self.file_descriptors[fd as usize].as_mut().unwrap();

        if optname == OptName::LINGER {
            assert!(optval.len() == std::mem::size_of::<Linger>());
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
                    .socket
                    .set_linger(linger.onoff != 0, linger.linger),
            );
        }

        assert!(optval.len() == std::mem::size_of::<u32>());
        let value = u32::from_ne_bytes([optval[0], optval[1], optval[2], optval[3]]);

        match optname {
            OptName::REUSEADDR => {
                assert!(value == 0 || value == 1);
                translate_errno(descriptor.socket.set_reuse_addr(value != 0))
            }
            OptName::KEEPALIVE => {
                assert!(value == 0 || value == 1);
                translate_errno(descriptor.socket.set_keep_alive(value != 0))
            }
            OptName::BROADCAST => {
                assert!(value == 0 || value == 1);
                translate_errno(descriptor.socket.set_broadcast(value != 0))
            }
            OptName::SNDBUF => translate_errno(descriptor.socket.set_snd_buf(value)),
            OptName::RCVBUF => translate_errno(descriptor.socket.set_rcv_buf(value)),
            OptName::SNDTIMEO => translate_errno(descriptor.socket.set_snd_timeo(value)),
            OptName::RCVTIMEO => translate_errno(descriptor.socket.set_rcv_timeo(value)),
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
        if !self.is_file_descriptor_valid(fd) {
            return Errno::BADF;
        }
        let host_how = translate_shutdown_how(ShutdownHow(how));
        let descriptor = self.file_descriptors[fd as usize].as_mut().unwrap();
        translate_errno(descriptor.socket.shutdown(host_how))
    }

    /// RecvImpl
    ///
    /// Corresponds to `BSD::RecvImpl` in upstream bsd.cpp.
    pub fn recv_impl(&mut self, fd: i32, mut flags: u32, message: &mut Vec<u8>) -> (i32, Errno) {
        if !self.is_file_descriptor_valid(fd) {
            return (-1, Errno::BADF);
        }

        let descriptor = self.file_descriptors[fd as usize].as_mut().unwrap();

        // Apply MSG_DONTWAIT flag
        if (flags & FLAG_MSG_DONTWAIT) != 0 {
            flags &= !FLAG_MSG_DONTWAIT;
            if (descriptor.flags & FLAG_O_NONBLOCK) == 0 {
                descriptor.socket.set_non_block(true);
            }
        }

        let (ret, bsd_errno) =
            translate_result(descriptor.socket.recv(flags as i32, message.as_mut_slice()));

        // Restore original state
        if (descriptor.flags & FLAG_O_NONBLOCK) == 0 {
            descriptor.socket.set_non_block(false);
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
        if !self.is_file_descriptor_valid(fd) {
            return (-1, Errno::BADF);
        }

        let descriptor = self.file_descriptors[fd as usize].as_mut().unwrap();

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
                descriptor.socket.set_non_block(true);
            }
        }

        let p_addr_in = if use_addr { Some(&mut addr_in) } else { None };

        let (ret, bsd_errno) = translate_result(descriptor.socket.recv_from(
            flags as i32,
            message.as_mut_slice(),
            p_addr_in,
        ));

        // Restore original state
        if (descriptor.flags & FLAG_O_NONBLOCK) == 0 {
            descriptor.socket.set_non_block(false);
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
        if !self.is_file_descriptor_valid(fd) {
            return (-1, Errno::BADF);
        }
        let descriptor = self.file_descriptors[fd as usize].as_mut().unwrap();
        translate_result(descriptor.socket.send(message, flags as i32))
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
        if !self.is_file_descriptor_valid(fd) {
            return (-1, Errno::BADF);
        }

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

        let descriptor = self.file_descriptors[fd as usize].as_mut().unwrap();
        translate_result(
            descriptor
                .socket
                .send_to(flags, message, p_addr_in.as_ref()),
        )
    }

    /// CloseImpl -- close a file descriptor.
    ///
    /// Corresponds to `BSD::CloseImpl` in upstream bsd.cpp.
    pub fn close_impl(&mut self, fd: i32) -> Errno {
        if !self.is_file_descriptor_valid(fd) {
            return Errno::BADF;
        }

        let bsd_errno = translate_errno(
            self.file_descriptors[fd as usize]
                .as_mut()
                .unwrap()
                .socket
                .close(),
        );
        if bsd_errno != Errno::SUCCESS {
            return bsd_errno;
        }

        log::info!("Close socket fd={}", fd);
        self.file_descriptors[fd as usize] = None;
        bsd_errno
    }

    /// DuplicateSocketImpl -- duplicate a file descriptor.
    ///
    /// Corresponds to `BSD::DuplicateSocketImpl` in upstream bsd.cpp.
    pub fn duplicate_socket_impl(&mut self, fd: i32) -> Result<i32, Errno> {
        if !self.is_file_descriptor_valid(fd) {
            return Err(Errno::BADF);
        }

        let new_fd = self.find_free_file_descriptor_handle();
        if new_fd < 0 {
            log::error!("No more file descriptors available");
            return Err(Errno::MFILE);
        }

        // Upstream copies the shared_ptr (shared ownership). In Rust we create a new Socket
        // wrapping the same underlying fd via Socket::from_fd. Note: this means the two
        // FileDescriptors share the same OS fd, matching upstream shared_ptr semantics.
        let src = self.file_descriptors[fd as usize].as_ref().unwrap();
        #[cfg(unix)]
        let src_fd_val = src.socket.get_fd();
        // Duplicate the OS-level file descriptor so both can close independently
        #[cfg(unix)]
        let new_os_fd = unsafe { libc::dup(src_fd_val) };
        #[cfg(not(unix))]
        let new_os_fd = -1;

        if new_os_fd < 0 {
            log::error!("Failed to dup socket fd");
            return Err(Errno::BADF);
        }

        let src_flags = src.flags;
        let src_is_conn = src.is_connection_based;

        self.file_descriptors[new_fd as usize] = Some(FileDescriptor {
            socket: Box::new(Socket::from_fd(new_os_fd)),
            flags: src_flags,
            is_connection_based: src_is_conn,
        });

        Ok(new_fd)
    }

    /// GetSocket -- get a socket reference by fd.
    ///
    /// Corresponds to `BSD::GetSocket` in upstream bsd.cpp.
    /// Used by SSL service to access BSD sockets.
    pub fn get_socket(&self, fd: i32) -> Option<&dyn SocketBase> {
        if !self.is_file_descriptor_valid(fd) {
            return None;
        }
        Some(
            self.file_descriptors[fd as usize]
                .as_ref()
                .unwrap()
                .socket
                .as_ref(),
        )
    }

    /// Mutable Rust counterpart to upstream `BSD::GetSocket`.
    ///
    /// Upstream returns a shared socket object whose mutating methods are used
    /// by SSL. Rust keeps the descriptor table behind the shared BSD service
    /// mutex and exposes the equivalent mutable borrow while that lock is held.
    pub fn get_socket_mut(&mut self, fd: i32) -> Option<&mut dyn SocketBase> {
        if !self.is_file_descriptor_valid(fd) {
            return None;
        }
        Some(
            self.file_descriptors[fd as usize]
                .as_mut()
                .unwrap()
                .socket
                .as_mut(),
        )
    }

    /// EventFd -- create event fd (stubbed).
    ///
    /// Corresponds to `BSD::EventFd` in upstream bsd.cpp.
    pub fn event_fd(&self, initval: u64, flags: u32) {
        log::warn!(
            "(STUBBED) BSD::EventFd called, initval={}, flags={}",
            initval,
            flags
        );
    }

    /// OnProxyPacketReceived -- handle incoming proxy packet.
    ///
    /// Corresponds to `BSD::OnProxyPacketReceived` in upstream bsd.cpp.
    pub fn on_proxy_packet_received(
        &mut self,
        packet: &crate::internal_network::network::ProxyPacket,
    ) {
        for optional_descriptor in self.file_descriptors.iter_mut() {
            if let Some(descriptor) = optional_descriptor {
                descriptor.socket.handle_proxy_packet(packet);
            }
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

    fn select_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("(STUBBED) BSD::Select called");
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(0); // ret
        rb.push_u32(0); // bsd errno
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

        let (ret, bsd_errno) = bsd.poll_impl(&mut write_buffer, &read_buffer, nfds, timeout);

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
        let (ret, bsd_errno) = bsd.send_impl(fd, 0, &message);

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(ret);
        rb.push_u32(bsd_errno as u32);
    }

    fn read_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) BSD::Read called");
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(0); // ret
        rb.push_u32(0); // bsd errno
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

    fn event_fd_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let initval = rp.pop_u64();
        let flags = rp.pop_u32();
        log::warn!(
            "(STUBBED) BSD::EventFd called, initval={}, flags={}",
            initval,
            flags
        );
        Bsd::build_errno_response_ipc(ctx, Errno::SUCCESS);
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
    use super::*;

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
