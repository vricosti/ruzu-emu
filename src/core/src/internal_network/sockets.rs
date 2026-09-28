// SPDX-FileCopyrightText: Copyright 2020 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of Eden src/core/internal_network/sockets.h and the Socket/transport
//! portions of network.cpp. Rust keeps those definitions with their declared
//! socket owner; network.rs owns network initialization and shared types.

#[cfg(windows)]
use winapi::{shared::ws2def, um::winsock2 as ws};

/// Upstream SocketBase::SOCKET is pointer-sized on Windows and int on Unix.
#[cfg(windows)]
pub type NativeSocket = ws::SOCKET;
#[cfg(not(windows))]
pub type NativeSocket = i32;
#[cfg(windows)]
pub(crate) const INVALID_SOCKET: NativeSocket = ws::INVALID_SOCKET;
#[cfg(not(windows))]
pub(crate) const INVALID_SOCKET: NativeSocket = -1;

#[cfg(any(unix, windows))]
use crate::internal_network::network::get_interrupt_socket;
#[cfg(any(unix, windows))]
use crate::internal_network::network::PollEvents as NetworkPollEvents;
use crate::internal_network::network::{
    Domain, Errno, Protocol, ProxyPacket, ShutdownHow, SockAddrIn, Type,
};

/// Socket base trait.
///
/// Corresponds to upstream `Network::SocketBase`.
pub trait SocketBase: Send + Sync {
    fn initialize(&mut self, domain: Domain, type_: Type, protocol: Protocol) -> Errno;
    fn close(&mut self) -> Errno;

    fn accept(&mut self) -> (AcceptResult, Errno);
    fn connect(&mut self, addr_in: SockAddrIn) -> Errno;

    fn get_peer_name(&self) -> (SockAddrIn, Errno);
    fn get_sock_name(&self) -> (SockAddrIn, Errno);

    fn bind(&mut self, addr: SockAddrIn) -> Errno;
    fn listen(&mut self, backlog: i32) -> Errno;
    fn shutdown(&mut self, how: ShutdownHow) -> Errno;

    fn recv(&mut self, flags: i32, message: &mut [u8]) -> (i32, Errno);
    fn recv_from(
        &mut self,
        flags: i32,
        message: &mut [u8],
        addr: Option<&mut SockAddrIn>,
    ) -> (i32, Errno);
    fn send(&mut self, message: &[u8], flags: i32) -> (i32, Errno);
    fn send_to(&mut self, flags: u32, message: &[u8], addr: Option<&SockAddrIn>) -> (i32, Errno);

    fn set_linger(&mut self, enable: bool, linger: u32) -> Errno;
    fn set_reuse_addr(&mut self, enable: bool) -> Errno;
    fn set_keep_alive(&mut self, enable: bool) -> Errno;
    fn set_broadcast(&mut self, enable: bool) -> Errno;
    fn set_snd_buf(&mut self, value: u32) -> Errno;
    fn set_rcv_buf(&mut self, value: u32) -> Errno;
    fn set_snd_timeo(&mut self, value: u32) -> Errno;
    fn set_rcv_timeo(&mut self, value: u32) -> Errno;
    fn set_non_block(&mut self, enable: bool) -> Errno;

    fn get_pending_error(&self) -> (Errno, Errno);
    fn is_opened(&self) -> bool;
    fn handle_proxy_packet(&mut self, packet: &ProxyPacket);

    fn get_fd(&self) -> NativeSocket;
}

/// Accept result.
///
/// Corresponds to upstream `SocketBase::AcceptResult`.
pub struct AcceptResult {
    pub socket: Option<Box<dyn SocketBase>>,
    pub sockaddr_in: SockAddrIn,
}

impl Default for AcceptResult {
    fn default() -> Self {
        Self {
            socket: None,
            sockaddr_in: SockAddrIn::default(),
        }
    }
}

/// Native socket implementation.
///
/// Corresponds to upstream `Network::Socket`.
pub struct Socket {
    fd: NativeSocket,
    is_non_blocking: bool,
}


/// Convert our SockAddrIn to libc::sockaddr_in.
#[cfg(windows)]
fn to_sockaddr_in(addr: &SockAddrIn) -> ws2def::SOCKADDR_IN {
    let mut native: ws2def::SOCKADDR_IN = unsafe { std::mem::zeroed() };
    native.sin_family = ws2def::AF_INET as u16;
    native.sin_port = addr.portno.to_be();
    unsafe { *native.sin_addr.S_un.S_addr_mut() = u32::from_ne_bytes(addr.ip); }
    native
}

#[cfg(windows)]
fn from_sockaddr_in(addr: &ws2def::SOCKADDR_IN) -> SockAddrIn {
    SockAddrIn { family: Some(if addr.sin_family == 0 { Domain::Unspecified } else { Domain::INET }), portno: u16::from_be(addr.sin_port),
        ip: unsafe { *addr.sin_addr.S_un.S_addr() }.to_ne_bytes() }
}

/// Windows branch of upstream TranslateNativeError(CallType).
#[cfg(windows)]
fn translate_windows_error(error: i32, send: bool) -> Errno {
    match error {
        0 => Errno::Success,
        ws::WSAEBADF => Errno::Badf,
        ws::WSAEINVAL => Errno::Inval,
        ws::WSAEMFILE => Errno::Mfile,
        ws::WSAENOTCONN => Errno::Notconn,
        ws::WSAEWOULDBLOCK => Errno::Again,
        ws::WSAECONNREFUSED => Errno::Connrefused,
        ws::WSAECONNABORTED if send => Errno::Pipe,
        ws::WSAECONNABORTED => Errno::Connaborted,
        ws::WSAECONNRESET => Errno::Connreset,
        ws::WSAEHOSTUNREACH => Errno::Hostunreach,
        ws::WSAENETDOWN => Errno::Netdown,
        ws::WSAENETUNREACH => Errno::Netunreach,
        ws::WSAEMSGSIZE => Errno::Msgsize,
        ws::WSAETIMEDOUT => Errno::Timedout,
        ws::WSAEINPROGRESS => Errno::Inprogress,
        ws::WSAEISCONN => Errno::Isconn,
        _ => { log::warn!("Unimplemented Winsock error {error}"); Errno::Other }
    }
}

#[cfg(windows)]
fn get_last_error() -> Errno {
    translate_windows_error(unsafe { ws::WSAGetLastError() }, false)
}

#[cfg(windows)]
fn get_last_send_error() -> Errno {
    translate_windows_error(unsafe { ws::WSAGetLastError() }, true)
}

/// Convert our SockAddrIn to libc::sockaddr_in.
#[cfg(unix)]
fn to_sockaddr_in(addr: &SockAddrIn) -> libc::sockaddr_in {
    let ip = addr.ip;
    // Initialize through libc's platform-specific definition: BSD sockaddr_in
    // has sin_len while Linux sockaddr_in does not.
    let mut sa: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    #[cfg(any(
        target_vendor = "apple",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    ))]
    {
        sa.sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
    }
    sa.sin_family = libc::AF_INET as libc::sa_family_t;
    sa.sin_port = addr.portno.to_be();
    sa.sin_addr = libc::in_addr {
        // `s_addr` is stored in network byte order. Building the native-endian
        // integer from the four bytes gives it that exact in-memory layout on
        // both little- and big-endian hosts, matching upstream's byte shifts.
        s_addr: u32::from_ne_bytes(ip),
    };
    sa
}

/// Convert libc::sockaddr_in to our SockAddrIn.
#[cfg(unix)]
fn from_sockaddr_in(addr: &libc::sockaddr_in) -> SockAddrIn {
    SockAddrIn {
        family: Some(Domain::INET),
        ip: addr.sin_addr.s_addr.to_ne_bytes(),
        portno: u16::from_be(addr.sin_port),
    }
}

/// Translate a native socket error to the cross-platform network errno.
///
/// Corresponds to upstream `TranslateNativeError` in network.cpp.
#[cfg(unix)]
fn translate_native_error(err: i32) -> Errno {
    match err {
        0 => Errno::Success,
        value if value == libc::EWOULDBLOCK || value == libc::EAGAIN => Errno::Again,
        libc::EMFILE => Errno::Mfile,
        libc::ECONNREFUSED => Errno::Connrefused,
        libc::ECONNRESET => Errno::Connreset,
        libc::ECONNABORTED => Errno::Connaborted,
        libc::EINPROGRESS => Errno::Inprogress,
        libc::ENOTCONN => Errno::Notconn,
        libc::ETIMEDOUT => Errno::Timedout,
        libc::EBADF => Errno::Badf,
        libc::EINVAL => Errno::Inval,
        libc::EPIPE => Errno::Pipe,
        libc::EMSGSIZE => Errno::Msgsize,
        libc::EHOSTUNREACH => Errno::Hostunreach,
        libc::ENETDOWN => Errno::Netdown,
        libc::ENETUNREACH => Errno::Netunreach,
        libc::EISCONN => Errno::Isconn,
        _ => {
            log::warn!("Unmapped socket errno: {}", err);
            Errno::Other
        }
    }
}

/// Get and translate the last native socket error.
#[cfg(unix)]
fn get_last_error() -> Errno {
    let native_error = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    let error = translate_native_error(native_error);
    if matches!(error, Errno::Again | Errno::Timedout | Errno::Inprogress) {
        log::debug!(
            "Socket operation error: {}",
            std::io::Error::from_raw_os_error(native_error)
        );
    } else {
        log::error!(
            "Socket operation error: {}",
            std::io::Error::from_raw_os_error(native_error)
        );
    }
    error
}

#[cfg(unix)]
fn translate_poll_events(mut events: NetworkPollEvents) -> i16 {
    let mut result = 0;
    macro_rules! translate {
        ($event:ident, $native:ident) => {
            if events.contains(NetworkPollEvents::$event) {
                events.remove(NetworkPollEvents::$event);
                result |= libc::$native;
            }
        };
    }

    translate!(IN, POLLIN);
    translate!(PRI, POLLPRI);
    translate!(OUT, POLLOUT);
    translate!(ERR, POLLERR);
    translate!(HUP, POLLHUP);
    translate!(NVAL, POLLNVAL);
    translate!(RD_NORM, POLLRDNORM);
    translate!(RD_BAND, POLLRDBAND);
    translate!(WR_BAND, POLLWRBAND);

    if !events.is_empty() {
        log::warn!("Unhandled poll events={:#x}", events.bits());
    }
    result
}

#[cfg(unix)]
fn translate_poll_revents(mut revents: i16) -> NetworkPollEvents {
    let mut result = NetworkPollEvents::empty();
    macro_rules! translate {
        ($native:ident, $event:ident) => {
            if revents & libc::$native != 0 {
                revents &= !libc::$native;
                result.insert(NetworkPollEvents::$event);
            }
        };
    }

    translate!(POLLIN, IN);
    translate!(POLLPRI, PRI);
    translate!(POLLOUT, OUT);
    translate!(POLLERR, ERR);
    translate!(POLLHUP, HUP);
    translate!(POLLNVAL, NVAL);
    translate!(POLLRDNORM, RD_NORM);
    translate!(POLLRDBAND, RD_BAND);
    translate!(POLLWRBAND, WR_BAND);

    if revents != 0 {
        log::warn!("Unhandled host poll revents={revents:#x}");
    }
    result
}

#[cfg(windows)]
fn translate_poll_events(mut events: NetworkPollEvents) -> i16 {
    let mut result = 0;
    macro_rules! translate {
        ($event:ident, $native:ident) => {
            if events.contains(NetworkPollEvents::$event) {
                events.remove(NetworkPollEvents::$event);
                result |= ws::$native;
            }
        };
    }

    translate!(IN, POLLIN);
    translate!(PRI, POLLPRI);
    translate!(OUT, POLLOUT);
    translate!(ERR, POLLERR);
    translate!(HUP, POLLHUP);
    translate!(NVAL, POLLNVAL);
    translate!(RD_NORM, POLLRDNORM);
    translate!(RD_BAND, POLLRDBAND);
    translate!(WR_BAND, POLLWRBAND);

    if !events.is_empty() {
        log::warn!("Unhandled poll events={:#x}", events.bits());
    }
    // WSAPoll rejects other input flags, unlike POSIX poll.
    result & (ws::POLLRDBAND | ws::POLLRDNORM | ws::POLLWRNORM)
}

#[cfg(windows)]
fn translate_poll_revents(mut revents: i16) -> NetworkPollEvents {
    let mut result = NetworkPollEvents::empty();
    macro_rules! translate {
        ($native:ident, $event:ident) => {
            if revents & ws::$native != 0 {
                revents &= !ws::$native;
                result.insert(NetworkPollEvents::$event);
            }
        };
    }

    translate!(POLLIN, IN);
    translate!(POLLPRI, PRI);
    translate!(POLLOUT, OUT);
    translate!(POLLERR, ERR);
    translate!(POLLHUP, HUP);
    translate!(POLLNVAL, NVAL);
    translate!(POLLRDNORM, RD_NORM);
    translate!(POLLRDBAND, RD_BAND);
    translate!(POLLWRBAND, WR_BAND);

    if revents != 0 {
        log::warn!("Unhandled host poll revents={revents:#x}");
    }
    result
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::internal_network::network::{
        cancel_pending_socket_operations, restart_socket_operations, NetworkInstance,
    };
    use std::sync::{mpsc, Mutex};
    use std::time::Duration;

    static INTERRUPT_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn sockaddr_in_preserves_network_order_bytes() {
        let guest = SockAddrIn {
            family: Some(Domain::INET),
            ip: [192, 0, 2, 7],
            portno: 0x1234,
        };

        let native = to_sockaddr_in(&guest);
        assert_eq!(native.sin_addr.s_addr.to_ne_bytes(), guest.ip);
        assert_eq!(native.sin_port.to_ne_bytes(), guest.portno.to_be_bytes());

        let round_trip = from_sockaddr_in(&native);
        assert_eq!(round_trip.family, guest.family);
        assert_eq!(round_trip.ip, guest.ip);
        assert_eq!(round_trip.portno, guest.portno);
    }

    #[test]
    fn poll_event_translation_uses_native_values() {
        assert_eq!(
            translate_poll_events(NetworkPollEvents::WR_BAND),
            libc::POLLWRBAND
        );
        assert_eq!(
            translate_poll_revents(libc::POLLWRBAND),
            NetworkPollEvents::WR_BAND
        );
    }

    #[test]
    fn native_error_translation_includes_already_connected() {
        assert_eq!(translate_native_error(libc::EISCONN), Errno::Isconn);
    }

    #[test]
    fn socket_options_are_forwarded_to_the_host_socket() {
        let mut socket = Socket::new();
        assert_eq!(
            socket.initialize(Domain::INET, Type::STREAM, Protocol::TCP),
            Errno::Success
        );

        assert_eq!(socket.set_linger(true, 1), Errno::Success);
        assert_eq!(socket.set_reuse_addr(true), Errno::Success);
        assert_eq!(socket.set_keep_alive(true), Errno::Success);
        assert_eq!(socket.set_broadcast(true), Errno::Success);
        assert_eq!(socket.set_snd_buf(32 * 1024), Errno::Success);
        assert_eq!(socket.set_rcv_buf(32 * 1024), Errno::Success);
        assert_eq!(socket.get_pending_error(), (Errno::Success, Errno::Success));
    }

    #[test]
    fn pending_error_reports_getsockopt_failure() {
        let socket = Socket::new();
        assert_eq!(socket.get_pending_error(), (Errno::Success, Errno::Badf));
    }

    #[test]
    fn pending_poll_is_cancelled_by_network_interrupt() {
        let _test_guard = INTERRUPT_TEST_LOCK.lock().unwrap();
        let first_instance = NetworkInstance::new();
        let _second_instance = NetworkInstance::new();
        restart_socket_operations();

        // Dropping one Rust System must not close the process-global pipe while
        // another System still owns its NetworkInstance.
        drop(first_instance);

        let (sender, receiver) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let mut pollfds = [];
            sender.send(poll(&mut pollfds, -1)).unwrap();
        });

        cancel_pending_socket_operations();
        let result = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("network interrupt did not release poll");
        thread.join().unwrap();
        restart_socket_operations();

        assert_eq!(result, (1, Errno::Success));
    }

    #[test]
    fn blocking_accept_is_cancelled_by_network_interrupt() {
        let _test_guard = INTERRUPT_TEST_LOCK.lock().unwrap();
        let _network_instance = NetworkInstance::new();
        restart_socket_operations();

        let mut listener = Socket::new();
        assert_eq!(
            listener.initialize(Domain::INET, Type::STREAM, Protocol::TCP),
            Errno::Success
        );
        assert_eq!(
            listener.bind(SockAddrIn {
                family: Some(Domain::INET),
                ip: [127, 0, 0, 1],
                portno: 0,
            }),
            Errno::Success
        );
        assert_eq!(listener.listen(1), Errno::Success);

        let (sender, receiver) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let (accepted, error) = listener.accept();
            sender.send((accepted.socket.is_none(), error)).unwrap();
        });

        cancel_pending_socket_operations();
        let result = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("network interrupt did not release accept");
        thread.join().unwrap();
        restart_socket_operations();

        assert_eq!(result, (true, Errno::Again));
    }
}

impl Socket {
    #[cfg(windows)]
    fn set_sock_opt<T>(&self, option: i32, value: &T) -> Errno {
        let result = unsafe { ws::setsockopt(self.fd, ws::SOL_SOCKET, option,
            (value as *const T).cast(), std::mem::size_of::<T>() as i32) };
        if result == ws::SOCKET_ERROR { get_last_error() } else { Errno::Success }
    }

    #[cfg(windows)]
    fn get_sock_opt<T: Default>(&self, option: i32) -> (T, Errno) {
        let mut value = T::default();
        let mut size = std::mem::size_of::<T>() as i32;
        let result = unsafe { ws::getsockopt(self.fd, ws::SOL_SOCKET, option,
            (&mut value as *mut T).cast(), &mut size) };
        if result == ws::SOCKET_ERROR { (value, get_last_error()) } else {
            assert_eq!(size as usize, std::mem::size_of::<T>());
            (value, Errno::Success)
        }
    }

    pub fn new() -> Self {
        Self {
            fd: INVALID_SOCKET,
            is_non_blocking: false,
        }
    }

    pub fn from_fd(fd: NativeSocket) -> Self {
        Self {
            fd,
            is_non_blocking: false,
        }
    }

    /// Corresponds to upstream `Socket::SetSockOpt` in network.cpp.
    #[cfg(unix)]
    fn set_sock_opt<T>(&self, option: i32, value: &T) -> Errno {
        let result = unsafe {
            libc::setsockopt(
                self.fd,
                libc::SOL_SOCKET,
                option,
                value as *const T as *const libc::c_void,
                std::mem::size_of::<T>() as libc::socklen_t,
            )
        };
        if result == 0 {
            Errno::Success
        } else {
            get_last_error()
        }
    }

    /// Corresponds to upstream `Socket::GetSockOpt` in network.cpp.
    #[cfg(unix)]
    fn get_sock_opt<T: Default>(&self, option: i32) -> (T, Errno) {
        let mut value = T::default();
        let mut length = std::mem::size_of::<T>() as libc::socklen_t;
        let result = unsafe {
            libc::getsockopt(
                self.fd,
                libc::SOL_SOCKET,
                option,
                &mut value as *mut T as *mut libc::c_void,
                &mut length,
            )
        };
        if result == 0 {
            assert_eq!(length as usize, std::mem::size_of::<T>());
            (value, Errno::Success)
        } else {
            (value, get_last_error())
        }
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        if self.fd == INVALID_SOCKET {
            return;
        }
        #[cfg(unix)]
        unsafe {
            libc::close(self.fd);
        }
        #[cfg(windows)]
        unsafe { ws::closesocket(self.fd); }
        self.fd = INVALID_SOCKET;
    }
}

impl SocketBase for Socket {
    fn initialize(&mut self, domain: Domain, type_: Type, protocol: Protocol) -> Errno {
        #[cfg(unix)]
        {
            let native_domain = match domain {
                Domain::Unspecified => 0,
                Domain::INET => libc::AF_INET,
            };
            let native_type = match type_ {
                Type::Unspecified => 0,
                Type::STREAM => libc::SOCK_STREAM,
                Type::DGRAM => libc::SOCK_DGRAM,
                Type::RAW => libc::SOCK_RAW,
                Type::SEQPACKET => libc::SOCK_SEQPACKET,
            };
            let native_proto = match protocol {
                Protocol::Unspecified => 0,
                Protocol::ICMP => libc::IPPROTO_ICMP,
                Protocol::TCP => libc::IPPROTO_TCP,
                Protocol::UDP => libc::IPPROTO_UDP,
            };
            self.fd = unsafe { libc::socket(native_domain, native_type, native_proto) };
            if self.fd != INVALID_SOCKET {
                return Errno::Success;
            }
            return get_last_error();
        }
        #[cfg(windows)]
        {
            let domain = match domain { Domain::Unspecified => 0, Domain::INET => ws2def::AF_INET };
            let kind = match type_ { Type::Unspecified => 0, Type::STREAM => ws::SOCK_STREAM,
                Type::DGRAM => ws::SOCK_DGRAM, Type::RAW => ws::SOCK_RAW, Type::SEQPACKET => ws::SOCK_SEQPACKET };
            let protocol = match protocol { Protocol::Unspecified => 0, Protocol::ICMP => ws2def::IPPROTO_ICMP as i32,
                Protocol::TCP => ws2def::IPPROTO_TCP as i32, Protocol::UDP => ws2def::IPPROTO_UDP as i32 };
            self.fd = unsafe { ws::socket(domain, kind, protocol) };
            if self.fd == INVALID_SOCKET { get_last_error() } else { Errno::Success }
        }
        #[cfg(not(any(unix, windows)))]
        {
            // TODO: Windows socket creation
            let _ = (domain, type_, protocol);
            Errno::Other
        }
    }

    fn close(&mut self) -> Errno {
        if self.fd != INVALID_SOCKET {
            #[cfg(unix)]
            if unsafe { libc::close(self.fd) } != 0 {
                log::warn!("close failed, socket may already be closed");
            }
            #[cfg(windows)]
            if unsafe { ws::closesocket(self.fd) } != 0 {
                log::warn!("closesocket failed, socket may already be closed");
            }
            self.fd = INVALID_SOCKET;
        }
        Errno::Success
    }

    fn accept(&mut self) -> (AcceptResult, Errno) {
        #[cfg(unix)]
        {
            let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
            let mut addrlen: libc::socklen_t = std::mem::size_of::<libc::sockaddr_in>() as u32;

            if !self.is_non_blocking {
                let mut host_pollfds = [
                    libc::pollfd {
                        fd: self.fd,
                        events: libc::POLLIN,
                        revents: 0,
                    },
                    libc::pollfd {
                        fd: get_interrupt_socket(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                ];

                loop {
                    let poll_result = unsafe {
                        libc::poll(
                            host_pollfds.as_mut_ptr(),
                            host_pollfds.len() as libc::nfds_t,
                            -1,
                        )
                    };
                    if host_pollfds[1].revents != 0 {
                        return (AcceptResult::default(), Errno::Again);
                    }
                    if poll_result > 0 {
                        break;
                    }
                }
            }

            let new_fd = unsafe {
                libc::accept(
                    self.fd,
                    &mut addr as *mut libc::sockaddr_in as *mut libc::sockaddr,
                    &mut addrlen,
                )
            };
            if new_fd < 0 {
                return (AcceptResult::default(), get_last_error());
            }
            let mut new_socket = Socket::new();
            new_socket.fd = new_fd;
            (
                AcceptResult {
                    socket: Some(Box::new(new_socket)),
                    sockaddr_in: from_sockaddr_in(&addr),
                },
                Errno::Success,
            )
        }
        #[cfg(windows)]
        {
            let mut addr: ws2def::SOCKADDR_IN = unsafe { std::mem::zeroed() };
            let mut len = std::mem::size_of_val(&addr) as i32;
            if !self.is_non_blocking {
                let mut fds = [ws::WSAPOLLFD { fd: self.fd, events: ws::POLLIN, revents: 0 },
                    ws::WSAPOLLFD { fd: get_interrupt_socket(), events: ws::POLLIN, revents: 0 }];
                loop {
                    let result = unsafe { ws::WSAPoll(fds.as_mut_ptr(), 2, -1) };
                    if fds[1].revents != 0 { return (AcceptResult::default(), Errno::Again); }
                    if result > 0 { break; }
                }
            }
            let fd = unsafe { ws::accept(self.fd, (&mut addr as *mut ws2def::SOCKADDR_IN).cast(), &mut len) };
            if fd == INVALID_SOCKET { return (AcceptResult::default(), get_last_error()); }
            (AcceptResult { socket: Some(Box::new(Socket::from_fd(fd))), sockaddr_in: from_sockaddr_in(&addr) }, Errno::Success)
        }
        #[cfg(not(any(unix, windows)))]
        {
            (AcceptResult::default(), Errno::Other)
        }
    }

    fn connect(&mut self, addr_in: SockAddrIn) -> Errno {
        #[cfg(unix)]
        {
            let addr = to_sockaddr_in(&addr_in);
            let result = unsafe {
                libc::connect(
                    self.fd,
                    &addr as *const libc::sockaddr_in as *const libc::sockaddr,
                    std::mem::size_of::<libc::sockaddr_in>() as u32,
                )
            };
            if result == 0 {
                Errno::Success
            } else {
                get_last_error()
            }
        }
        #[cfg(windows)]
        {
            let addr = to_sockaddr_in(&addr_in);
            if unsafe { ws::connect(self.fd, (&addr as *const ws2def::SOCKADDR_IN).cast(), std::mem::size_of_val(&addr) as i32) } == 0 {
                Errno::Success
            } else { get_last_error() }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = addr_in;
            Errno::Other
        }
    }

    fn get_peer_name(&self) -> (SockAddrIn, Errno) {
        #[cfg(unix)]
        {
            let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
            let mut addrlen: libc::socklen_t = std::mem::size_of::<libc::sockaddr_in>() as u32;
            if unsafe {
                libc::getpeername(
                    self.fd,
                    &mut addr as *mut libc::sockaddr_in as *mut libc::sockaddr,
                    &mut addrlen,
                )
            } != 0
            {
                return (SockAddrIn::default(), get_last_error());
            }
            (from_sockaddr_in(&addr), Errno::Success)
        }
        #[cfg(windows)]
        {
            let mut addr: ws2def::SOCKADDR_IN = unsafe { std::mem::zeroed() };
            let mut len = std::mem::size_of_val(&addr) as i32;
            if unsafe { ws::getpeername(self.fd, (&mut addr as *mut ws2def::SOCKADDR_IN).cast(), &mut len) } == ws::SOCKET_ERROR {
                return (SockAddrIn::default(), get_last_error());
            }
            (from_sockaddr_in(&addr), Errno::Success)
        }
        #[cfg(not(any(unix, windows)))]
        {
            (SockAddrIn::default(), Errno::Other)
        }
    }

    fn get_sock_name(&self) -> (SockAddrIn, Errno) {
        #[cfg(unix)]
        {
            let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
            let mut addrlen: libc::socklen_t = std::mem::size_of::<libc::sockaddr_in>() as u32;
            if unsafe {
                libc::getsockname(
                    self.fd,
                    &mut addr as *mut libc::sockaddr_in as *mut libc::sockaddr,
                    &mut addrlen,
                )
            } != 0
            {
                return (SockAddrIn::default(), get_last_error());
            }
            (from_sockaddr_in(&addr), Errno::Success)
        }
        #[cfg(windows)]
        {
            let mut addr: ws2def::SOCKADDR_IN = unsafe { std::mem::zeroed() };
            let mut len = std::mem::size_of_val(&addr) as i32;
            if unsafe { ws::getsockname(self.fd, (&mut addr as *mut ws2def::SOCKADDR_IN).cast(), &mut len) } == ws::SOCKET_ERROR {
                return (SockAddrIn::default(), get_last_error());
            }
            (from_sockaddr_in(&addr), Errno::Success)
        }
        #[cfg(not(any(unix, windows)))]
        {
            (SockAddrIn::default(), Errno::Other)
        }
    }

    fn bind(&mut self, addr: SockAddrIn) -> Errno {
        #[cfg(unix)]
        {
            let addr_in = to_sockaddr_in(&addr);
            if unsafe {
                libc::bind(
                    self.fd,
                    &addr_in as *const libc::sockaddr_in as *const libc::sockaddr,
                    std::mem::size_of::<libc::sockaddr_in>() as u32,
                )
            } == 0
            {
                Errno::Success
            } else {
                get_last_error()
            }
        }
        #[cfg(windows)]
        {
            let addr = to_sockaddr_in(&addr);
            if unsafe { ws::bind(self.fd, (&addr as *const ws2def::SOCKADDR_IN).cast(), std::mem::size_of_val(&addr) as i32) } == 0 {
                Errno::Success
            } else { get_last_error() }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = addr;
            Errno::Other
        }
    }

    fn listen(&mut self, backlog: i32) -> Errno {
        #[cfg(unix)]
        {
            if unsafe { libc::listen(self.fd, backlog) } == 0 {
                Errno::Success
            } else {
                get_last_error()
            }
        }
        #[cfg(windows)]
        {
            if unsafe { ws::listen(self.fd, backlog) } == 0 { Errno::Success } else { get_last_error() }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = backlog;
            Errno::Other
        }
    }

    fn shutdown(&mut self, how: ShutdownHow) -> Errno {
        #[cfg(unix)]
        {
            let host_how = match how {
                ShutdownHow::RD => libc::SHUT_RD,
                ShutdownHow::WR => libc::SHUT_WR,
                ShutdownHow::RDWR => libc::SHUT_RDWR,
            };
            if unsafe { libc::shutdown(self.fd, host_how) } == 0 {
                Errno::Success
            } else {
                get_last_error()
            }
        }
        #[cfg(windows)]
        {
            let how = match how { ShutdownHow::RD => ws::SD_RECEIVE, ShutdownHow::WR => ws::SD_SEND, ShutdownHow::RDWR => ws::SD_BOTH };
            if unsafe { ws::shutdown(self.fd, how) } == 0 { Errno::Success } else { get_last_error() }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = how;
            Errno::Other
        }
    }

    fn recv(&mut self, flags: i32, message: &mut [u8]) -> (i32, Errno) {
        #[cfg(unix)]
        {
            assert_eq!(flags, 0);
            let result = unsafe {
                libc::recv(
                    self.fd,
                    message.as_mut_ptr() as *mut libc::c_void,
                    message.len(),
                    flags,
                )
            };
            if result >= 0 {
                (result as i32, Errno::Success)
            } else {
                (-1, get_last_error())
            }
        }
        #[cfg(windows)]
        {
            assert_eq!(flags, 0);
            assert!(message.len() < i32::MAX as usize);
            let result = unsafe { ws::recv(self.fd, message.as_mut_ptr().cast(), message.len() as i32, 0) };
            if result == ws::SOCKET_ERROR { (-1, get_last_error()) } else { (result, Errno::Success) }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (flags, message);
            (-1, Errno::Other)
        }
    }

    fn recv_from(
        &mut self,
        flags: i32,
        message: &mut [u8],
        addr: Option<&mut SockAddrIn>,
    ) -> (i32, Errno) {
        #[cfg(unix)]
        {
            assert_eq!(flags, 0);
            let mut addr_in: libc::sockaddr_in = unsafe { std::mem::zeroed() };
            let mut addrlen: libc::socklen_t = std::mem::size_of::<libc::sockaddr_in>() as u32;
            let (p_addr, p_addrlen) = if addr.is_some() {
                (
                    &mut addr_in as *mut libc::sockaddr_in as *mut libc::sockaddr,
                    &mut addrlen as *mut libc::socklen_t,
                )
            } else {
                (std::ptr::null_mut(), std::ptr::null_mut())
            };

            let result = unsafe {
                libc::recvfrom(
                    self.fd,
                    message.as_mut_ptr() as *mut libc::c_void,
                    message.len(),
                    flags,
                    p_addr,
                    p_addrlen,
                )
            };
            if result >= 0 {
                if let Some(out_addr) = addr {
                    *out_addr = from_sockaddr_in(&addr_in);
                }
                (result as i32, Errno::Success)
            } else {
                (-1, get_last_error())
            }
        }
        #[cfg(windows)]
        {
            assert_eq!(flags, 0);
            assert!(message.len() < i32::MAX as usize);
            let mut native: ws2def::SOCKADDR_IN = unsafe { std::mem::zeroed() };
            let mut len = std::mem::size_of_val(&native) as i32;
            let (address, length) = if addr.is_some() { ((&mut native as *mut ws2def::SOCKADDR_IN).cast(), &mut len as *mut i32) }
                else { (std::ptr::null_mut(), std::ptr::null_mut()) };
            let result = unsafe { ws::recvfrom(self.fd, message.as_mut_ptr().cast(), message.len() as i32, 0, address, length) };
            if result == ws::SOCKET_ERROR { return (-1, get_last_error()); }
            if let Some(addr) = addr { *addr = from_sockaddr_in(&native); }
            (result, Errno::Success)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (flags, message, addr);
            (-1, Errno::Other)
        }
    }

    fn send(&mut self, message: &[u8], flags: i32) -> (i32, Errno) {
        #[cfg(unix)]
        {
            assert_eq!(flags, 0);
            let result = unsafe {
                libc::send(
                    self.fd,
                    message.as_ptr() as *const libc::c_void,
                    message.len(),
                    libc::MSG_NOSIGNAL,
                )
            };
            if result >= 0 {
                (result as i32, Errno::Success)
            } else {
                (-1, get_last_error())
            }
        }
        #[cfg(windows)]
        {
            assert_eq!(flags, 0);
            assert!(message.len() < i32::MAX as usize);
            let result = unsafe { ws::send(self.fd, message.as_ptr().cast(), message.len() as i32, 0) };
            if result == ws::SOCKET_ERROR { (-1, get_last_send_error()) } else { (result, Errno::Success) }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (message, flags);
            (-1, Errno::Other)
        }
    }

    fn send_to(&mut self, flags: u32, message: &[u8], addr: Option<&SockAddrIn>) -> (i32, Errno) {
        #[cfg(unix)]
        {
            assert_eq!(flags, 0);
            let addr_in = addr.map(to_sockaddr_in);
            let p_addr = addr_in.as_ref().map_or(std::ptr::null(), |value| {
                value as *const libc::sockaddr_in as *const libc::sockaddr
            });
            let addrlen = if addr_in.is_some() {
                std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t
            } else {
                0
            };

            let result = unsafe {
                libc::sendto(
                    self.fd,
                    message.as_ptr() as *const libc::c_void,
                    message.len(),
                    0,
                    p_addr,
                    addrlen,
                )
            };

            if result >= 0 {
                (result as i32, Errno::Success)
            } else {
                (-1, get_last_error())
            }
        }
        #[cfg(windows)]
        {
            assert_eq!(flags, 0);
            let native = addr.map(to_sockaddr_in);
            let address = native.as_ref().map_or(std::ptr::null(), |a| (a as *const ws2def::SOCKADDR_IN).cast());
            let len = if native.is_some() { std::mem::size_of::<ws2def::SOCKADDR_IN>() as i32 } else { 0 };
            let result = unsafe { ws::sendto(self.fd, message.as_ptr().cast(), message.len() as i32, 0, address, len) };
            if result == ws::SOCKET_ERROR { (-1, get_last_send_error()) } else { (result, Errno::Success) }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (flags, message, addr);
            (-1, Errno::Other)
        }
    }

    fn set_linger(&mut self, enable: bool, linger: u32) -> Errno {
        #[cfg(unix)]
        {
            let value = libc::linger {
                l_onoff: i32::from(enable),
                l_linger: linger as i32,
            };
            self.set_sock_opt(libc::SO_LINGER, &value)
        }
        #[cfg(windows)]
        {
            assert!(linger <= u16::MAX as u32);
            self.set_sock_opt(ws::SO_LINGER, &ws::LINGER { l_onoff: u16::from(enable), l_linger: linger as u16 })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (enable, linger);
            Errno::Other
        }
    }
    fn set_reuse_addr(&mut self, enable: bool) -> Errno {
        #[cfg(unix)]
        {
            self.set_sock_opt(libc::SO_REUSEADDR, &u32::from(enable))
        }
        #[cfg(windows)]
        {
            self.set_sock_opt(ws::SO_REUSEADDR, &u32::from(enable))
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = enable;
            Errno::Other
        }
    }
    fn set_keep_alive(&mut self, enable: bool) -> Errno {
        #[cfg(unix)]
        {
            self.set_sock_opt(libc::SO_KEEPALIVE, &u32::from(enable))
        }
        #[cfg(windows)]
        {
            self.set_sock_opt(ws::SO_KEEPALIVE, &u32::from(enable))
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = enable;
            Errno::Other
        }
    }
    fn set_broadcast(&mut self, enable: bool) -> Errno {
        #[cfg(unix)]
        {
            self.set_sock_opt(libc::SO_BROADCAST, &u32::from(enable))
        }
        #[cfg(windows)]
        {
            self.set_sock_opt(ws::SO_BROADCAST, &u32::from(enable))
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = enable;
            Errno::Other
        }
    }
    fn set_snd_buf(&mut self, value: u32) -> Errno {
        #[cfg(unix)]
        {
            self.set_sock_opt(libc::SO_SNDBUF, &value)
        }
        #[cfg(windows)]
        {
            self.set_sock_opt(ws::SO_SNDBUF, &value)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = value;
            Errno::Other
        }
    }
    fn set_rcv_buf(&mut self, value: u32) -> Errno {
        #[cfg(unix)]
        {
            self.set_sock_opt(libc::SO_RCVBUF, &value)
        }
        #[cfg(windows)]
        {
            self.set_sock_opt(ws::SO_RCVBUF, &value)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = value;
            Errno::Other
        }
    }
    fn set_snd_timeo(&mut self, value: u32) -> Errno {
        #[cfg(unix)]
        {
            self.set_sock_opt(libc::SO_SNDTIMEO, &value)
        }
        #[cfg(windows)]
        {
            self.set_sock_opt(ws::SO_SNDTIMEO, &value)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = value;
            Errno::Other
        }
    }
    fn set_rcv_timeo(&mut self, value: u32) -> Errno {
        #[cfg(unix)]
        {
            self.set_sock_opt(libc::SO_RCVTIMEO, &value)
        }
        #[cfg(windows)]
        {
            self.set_sock_opt(ws::SO_RCVTIMEO, &value)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = value;
            Errno::Other
        }
    }

    fn set_non_block(&mut self, enable: bool) -> Errno {
        #[cfg(unix)]
        {
            let flags = unsafe { libc::fcntl(self.fd, libc::F_GETFL) };
            if flags == -1 {
                return Errno::Other;
            }
            let new_flags = if enable {
                flags | libc::O_NONBLOCK
            } else {
                flags & !libc::O_NONBLOCK
            };
            if unsafe { libc::fcntl(self.fd, libc::F_SETFL, new_flags) } == 0 {
                self.is_non_blocking = enable;
                return Errno::Success;
            }
            return get_last_error();
        }
        #[cfg(windows)]
        {
            let mut value = u32::from(enable);
            if unsafe { ws::ioctlsocket(self.fd, ws::FIONBIO, &mut value) } == 0 {
                self.is_non_blocking = enable;
                Errno::Success
            } else { get_last_error() }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = enable;
            Errno::Other
        }
    }

    fn get_pending_error(&self) -> (Errno, Errno) {
        #[cfg(unix)]
        {
            let (pending_error, get_sock_opt_error) = self.get_sock_opt::<i32>(libc::SO_ERROR);
            (translate_native_error(pending_error), get_sock_opt_error)
        }
        #[cfg(windows)]
        {
            let (error, result) = self.get_sock_opt::<i32>(ws::SO_ERROR);
            (translate_windows_error(error, false), result)
        }
        #[cfg(not(any(unix, windows)))]
        {
            (Errno::Success, Errno::Other)
        }
    }

    fn is_opened(&self) -> bool {
        self.fd != INVALID_SOCKET
    }

    fn handle_proxy_packet(&mut self, _packet: &ProxyPacket) {
        log::warn!("ProxyPacket received, but not in Proxy mode!");
    }

    fn get_fd(&self) -> NativeSocket {
        self.fd
    }
}

/// Poll a set of file descriptors.
///
/// Corresponds to upstream `Network::Poll`.
pub fn poll(pollfds: &mut [PollFD], timeout: i32) -> (i32, Errno) {
    #[cfg(unix)]
    {
        let num = pollfds.len();
        let mut fds: Vec<libc::pollfd> = pollfds
            .iter()
            .map(|pfd| libc::pollfd {
                fd: pfd.fd,
                events: translate_poll_events(NetworkPollEvents::from_bits_retain(pfd.events)),
                revents: 0,
            })
            .collect();
        fds.push(libc::pollfd {
            fd: get_interrupt_socket(),
            events: libc::POLLIN,
            revents: 0,
        });

        let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };

        if result == 0 {
            assert!(fds.iter().all(|fd| fd.revents == 0));
            return (0, Errno::Success);
        }

        for (i, fd) in fds.iter().take(num).enumerate() {
            pollfds[i].revents = translate_poll_revents(fd.revents).bits();
        }
        if result > 0 {
            (result, Errno::Success)
        } else {
            (-1, get_last_error())
        }
    }
    #[cfg(windows)]
    {
        let num = pollfds.len();
        let mut fds: Vec<ws::WSAPOLLFD> = pollfds
            .iter()
            .map(|pfd| ws::WSAPOLLFD {
                fd: pfd.fd,
                events: translate_poll_events(NetworkPollEvents::from_bits_retain(pfd.events)),
                revents: 0,
            })
            .collect();
        fds.push(ws::WSAPOLLFD {
            fd: get_interrupt_socket(),
            events: ws::POLLIN,
            revents: 0,
        });

        let result = unsafe { ws::WSAPoll(fds.as_mut_ptr(), fds.len() as u32, timeout) };

        if result == 0 {
            assert!(fds.iter().all(|fd| fd.revents == 0));
            return (0, Errno::Success);
        }

        for (i, fd) in fds.iter().take(num).enumerate() {
            pollfds[i].revents = translate_poll_revents(fd.revents).bits();
        }
        if result > 0 {
            (result, Errno::Success)
        } else {
            (-1, get_last_error())
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (pollfds, timeout);
        (0, Errno::Success)
    }
}

/// Poll file descriptor entry (simplified).
#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use crate::internal_network::network::NetworkInstance;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream, UdpSocket};

    fn loopback(portno: u16) -> SockAddrIn {
        SockAddrIn { family: Some(Domain::INET), ip: [127, 0, 0, 1], portno }
    }

    #[test]
    fn windows_blocking_operations_cancel_and_restart() {
        use crate::internal_network::network::{
            cancel_pending_socket_operations, restart_socket_operations,
        };
        use std::sync::mpsc;
        use std::time::Duration;

        // Cancellation is process-global. Do not interrupt unrelated socket tests.
        const CHILD: &str = "RUZU_TEST_WINDOWS_SOCKET_CANCELLATION";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "internal_network::sockets::windows_tests::windows_blocking_operations_cancel_and_restart", "--nocapture"])
                .env(CHILD, "1")
                .status()
                .unwrap();
            assert!(status.success(), "isolated cancellation test: {status}");
            return;
        }

        let _network = NetworkInstance::new();
        for _ in 0..3 {
            restart_socket_operations();
            let mut listener = Socket::new();
            assert_eq!(listener.initialize(Domain::INET, Type::STREAM, Protocol::TCP), Errno::Success);
            assert_eq!(listener.bind(loopback(0)), Errno::Success);
            assert_eq!(listener.listen(1), Errno::Success);
            let (sender, receiver) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                let (accepted, error) = listener.accept();
                sender.send((accepted.socket.is_none(), error)).unwrap();
            });
            assert!(matches!(receiver.recv_timeout(Duration::from_millis(30)), Err(mpsc::RecvTimeoutError::Timeout)));
            cancel_pending_socket_operations();
            assert_eq!(receiver.recv_timeout(Duration::from_secs(3)).unwrap(), (true, Errno::Again));
            worker.join().unwrap();

            restart_socket_operations();
            let (sender, receiver) = mpsc::channel();
            let worker = std::thread::spawn(move || sender.send(poll(&mut [], -1)).unwrap());
            assert!(matches!(receiver.recv_timeout(Duration::from_millis(30)), Err(mpsc::RecvTimeoutError::Timeout)));
            cancel_pending_socket_operations();
            assert_eq!(receiver.recv_timeout(Duration::from_secs(3)).unwrap(), (1, Errno::Success));
            worker.join().unwrap();
        }
        restart_socket_operations();
        assert_eq!(poll(&mut [], 0), (0, Errno::Success));
    }

    #[test]
    fn native_socket_width_addresses_and_error_mapping_match_winsock() {
        assert_eq!(std::mem::size_of::<NativeSocket>(), std::mem::size_of::<usize>());
        let address = loopback(0x1234);
        let native = to_sockaddr_in(&address);
        assert_eq!(native.sin_port.to_ne_bytes(), [0x12, 0x34]);
        assert_eq!(unsafe { *native.sin_addr.S_un.S_addr() }.to_ne_bytes(), address.ip);
        assert_eq!(from_sockaddr_in(&native).portno, address.portno);
        assert_eq!(from_sockaddr_in(&native).ip, address.ip);
        assert_eq!(from_sockaddr_in(&unsafe { std::mem::zeroed() }).family, Some(Domain::Unspecified));
        assert_eq!(translate_windows_error(ws::WSAECONNABORTED, false), Errno::Connaborted);
        assert_eq!(translate_windows_error(ws::WSAECONNABORTED, true), Errno::Pipe);
        assert_eq!(translate_windows_error(ws::WSAEWOULDBLOCK, false), Errno::Again);
        assert_eq!(translate_windows_error(ws::WSAEISCONN, false), Errno::Isconn);
        assert_eq!(translate_poll_events(NetworkPollEvents::WR_BAND), 0);
        assert_eq!(translate_poll_revents(ws::POLLWRBAND), NetworkPollEvents::WR_BAND);
    }

    #[test]
    fn windows_tcp_connect_and_accept_transfer_data() {
        let _network = NetworkInstance::new();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = Socket::new();
        assert_eq!(client.initialize(Domain::INET, Type::STREAM, Protocol::TCP), Errno::Success);
        assert_eq!(client.set_linger(false, 0), Errno::Success);
        assert_eq!(client.set_keep_alive(true), Errno::Success);
        assert_eq!(client.set_rcv_timeo(2000), Errno::Success);
        assert_eq!(client.set_snd_timeo(2000), Errno::Success);
        assert_eq!(client.connect(loopback(listener.local_addr().unwrap().port())), Errno::Success);
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
        assert_eq!(client.get_peer_name().0.portno, listener.local_addr().unwrap().port());
        assert_eq!(client.send(b"abc", 0), (3, Errno::Success));
        let mut buf = [0; 3];
        peer.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"abc");
        peer.write_all(b"def").unwrap();
        assert_eq!(client.recv(0, &mut buf), (3, Errno::Success));
        assert_eq!(&buf, b"def");
        assert_eq!(client.shutdown(ShutdownHow::WR), Errno::Success);
        assert_eq!(peer.read(&mut buf).unwrap(), 0);
        assert_eq!(client.close(), Errno::Success);
        assert!(!client.is_opened());

        let mut server = Socket::new();
        assert_eq!(server.initialize(Domain::INET, Type::STREAM, Protocol::TCP), Errno::Success);
        assert_eq!(server.bind(loopback(0)), Errno::Success);
        assert_eq!(server.listen(1), Errno::Success);
        let port = server.get_sock_name().0.portno;
        let mut peer = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).unwrap();
        let (accepted, error) = server.accept();
        assert_eq!(error, Errno::Success);
        assert_eq!(accepted.sockaddr_in.portno, peer.local_addr().unwrap().port());
        let mut accepted = accepted.socket.unwrap();
        assert_eq!(accepted.set_rcv_timeo(2000), Errno::Success);
        peer.write_all(b"xyz").unwrap();
        assert_eq!(accepted.recv(0, &mut buf), (3, Errno::Success));
        assert_eq!(&buf, b"xyz");
    }

    #[test]
    fn windows_udp_poll_nonblocking_and_options() {
        let _network = NetworkInstance::new();
        let mut socket = Socket::new();
        assert_eq!(socket.initialize(Domain::INET, Type::DGRAM, Protocol::UDP), Errno::Success);
        assert_eq!(socket.set_reuse_addr(true), Errno::Success);
        assert_eq!(socket.set_broadcast(true), Errno::Success);
        assert_eq!(socket.set_snd_buf(32768), Errno::Success);
        assert_eq!(socket.set_rcv_buf(32768), Errno::Success);
        assert_eq!(socket.set_rcv_timeo(2000), Errno::Success);
        assert_eq!(socket.bind(loopback(0)), Errno::Success);
        assert_eq!(socket.get_pending_error(), (Errno::Success, Errno::Success));
        assert_eq!(socket.set_non_block(true), Errno::Success);
        let mut data = [0; 4];
        assert_eq!(socket.recv_from(0, &mut data, None), (-1, Errno::Again));
        assert_eq!(socket.set_non_block(false), Errno::Success);
        let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
        peer.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
        peer.send_to(b"ping", (std::net::Ipv4Addr::LOCALHOST, socket.get_sock_name().0.portno)).unwrap();
        let mut fds = [PollFD { fd: socket.get_fd(), events: NetworkPollEvents::IN.bits(), revents: 0 }];
        let (ready, error) = poll(&mut fds, 2000);
        assert_eq!(error, Errno::Success);
        assert!(ready > 0);
        assert_ne!(fds[0].revents & NetworkPollEvents::IN.bits(), 0);
        let mut address = SockAddrIn::default();
        assert_eq!(socket.recv_from(0, &mut data, Some(&mut address)), (4, Errno::Success));
        assert_eq!(&data, b"ping");
        assert_eq!(address.portno, peer.local_addr().unwrap().port());
        assert_eq!(socket.send_to(0, b"pong", Some(&address)), (4, Errno::Success));
        assert_eq!(peer.recv_from(&mut data).unwrap().0, 4);
        assert_eq!(&data, b"pong");
    }
}

/// Poll file descriptor entry (simplified).
pub struct PollFD {
    pub fd: NativeSocket,
    pub events: u16,
    pub revents: u16,
}
