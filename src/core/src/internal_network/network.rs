// SPDX-FileCopyrightText: Copyright 2020 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/internal_network/network.h and network.cpp
//! Network types and utilities.

use bitflags::bitflags;
use std::net::Ipv4Addr;
#[cfg(windows)]
use winapi::um::winsock2 as winsock;

/// IPv4 address as a 4-byte array.
pub type IPv4Address = [u8; 4];

/// Socket domain types.
///
/// Corresponds to upstream `Network::Domain` (from common/socket_types.h).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Domain {
    Unspecified,
    INET,
}

/// Socket types.
///
/// Corresponds to upstream `Network::Type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    Unspecified,
    STREAM,
    DGRAM,
    RAW,
    SEQPACKET,
}

/// Protocol types.
///
/// Corresponds to upstream `Network::Protocol`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Unspecified,
    ICMP,
    TCP,
    UDP,
}

/// Shutdown modes.
///
/// Corresponds to upstream `Network::ShutdownHow` (from common/socket_types.h).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownHow {
    RD,
    WR,
    RDWR,
}

/// Socket address structure.
///
/// Corresponds to upstream `Network::SockAddrIn` (from common/socket_types.h).
#[derive(Debug, Clone, Default)]
pub struct SockAddrIn {
    pub family: Option<Domain>,
    pub ip: IPv4Address,
    pub portno: u16,
}

/// Address info structure.
///
/// Corresponds to upstream `Network::AddrInfo` (from common/socket_types.h).
#[derive(Debug, Clone)]
pub struct AddrInfo {
    pub family: Domain,
    pub socket_type: Type,
    pub protocol: Protocol,
    pub addr: SockAddrIn,
    pub canon_name: Option<String>,
}

/// Proxy packet for network tunneling.
///
/// Corresponds to upstream `Network::ProxyPacket` (from common/socket_types.h).
#[derive(Debug, Clone)]
pub struct ProxyPacket {
    pub local_endpoint: SockAddrIn,
    pub remote_endpoint: SockAddrIn,
    pub protocol: Protocol,
    pub broadcast: bool,
    pub data: Vec<u8>,
}

/// Flag for MSG_PEEK in recv operations.
pub const FLAG_MSG_PEEK: i32 = 0x2;

/// Error code for network functions.
///
/// Corresponds to upstream `Network::Errno`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Errno {
    Success,
    Badf,
    Inval,
    Mfile,
    Pipe,
    Notconn,
    Again,
    Connrefused,
    Connreset,
    Connaborted,
    Hostunreach,
    Netdown,
    Netunreach,
    Timedout,
    Msgsize,
    Inprogress,
    Isconn,
    Other,
}

/// Error codes for getaddrinfo.
///
/// Corresponds to upstream `Network::GetAddrInfoError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GetAddrInfoError {
    Success,
    Addrfamily,
    Again,
    Badflags,
    Fail,
    Family,
    Memory,
    Nodata,
    Noname,
    Service,
    Socktype,
    System,
    Badhints,
    Protocol,
    Overflow,
    Other,
}

bitflags! {
    /// Cross-platform poll event flags.
    ///
    /// Corresponds to upstream `Network::PollEvents`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct PollEvents: u16 {
        const IN      = 1 << 0;
        const PRI     = 1 << 1;
        const OUT     = 1 << 2;
        const ERR     = 1 << 3;
        const HUP     = 1 << 4;
        const NVAL    = 1 << 5;
        const RD_NORM = 1 << 6;
        const RD_BAND = 1 << 7;
        const WR_BAND = 1 << 8;
    }
}

/// Cross-platform poll fd structure.
///
/// Corresponds to upstream `Network::PollFD`.
/// Note: upstream uses a SocketBase pointer; here we use a file descriptor.
pub struct PollFD {
    pub fd: super::sockets::NativeSocket, // Upstream uses SocketBase*; we use fd directly.
    pub events: PollEvents,
    pub revents: PollEvents,
}

/// Network instance for platform initialization/cleanup.
///
/// Corresponds to upstream `Network::NetworkInstance`.
pub struct NetworkInstance {
    _private: (),
}

impl NetworkInstance {
    pub fn new() -> Self {
        initialize();
        Self { _private: () }
    }
}

impl Drop for NetworkInstance {
    fn drop(&mut self) {
        finalize();
    }
}

/// Interrupt pipe for cancelling blocking socket operations.
/// Upstream uses a pipe fd pair (Unix) or a UDP socket (Windows).
#[cfg(unix)]
struct InterruptPipeState {
    fds: [i32; 2],
    // Rust tests can own multiple Systems concurrently; keep upstream's
    // process-global pipe alive until the final NetworkInstance is dropped.
    owners: usize,
}

#[cfg(unix)]
static INTERRUPT_PIPE: std::sync::Mutex<InterruptPipeState> =
    std::sync::Mutex::new(InterruptPipeState {
        fds: [-1, -1],
        owners: 0,
    });

#[cfg(windows)]
struct InterruptSocketState {
    socket: winsock::SOCKET,
    owners: usize,
    // Retain the closed handle value for WSAPoll, as upstream does, but never
    // close it twice: Windows could have reused that value for another socket.
    cancelled: bool,
}

#[cfg(windows)]
static INTERRUPT_SOCKET: std::sync::Mutex<InterruptSocketState> =
    std::sync::Mutex::new(InterruptSocketState {
        socket: winsock::INVALID_SOCKET,
        owners: 0,
        cancelled: true,
    });

/// Platform-specific network initialization.
/// Port of upstream `Network::Initialize`.
fn initialize() {
    #[cfg(windows)]
    {
        let mut state = INTERRUPT_SOCKET.lock().unwrap();
        if state.owners == 0 {
            let mut data = unsafe { std::mem::zeroed() };
            let result = unsafe { winsock::WSAStartup(0x0202, &mut data) };
            assert_eq!(result, 0, "Winsock initialization failed");
            state.socket = unsafe {
                winsock::socket(winapi::shared::ws2def::AF_INET, winsock::SOCK_DGRAM,
                    winapi::shared::ws2def::IPPROTO_UDP as i32)
            };
            if state.socket == winsock::INVALID_SOCKET {
                let error = unsafe { winsock::WSAGetLastError() };
                unsafe { winsock::WSACleanup() };
                panic!("Failed to create interrupt socket: {error}");
            }
            state.cancelled = false;
        }
        state.owners += 1;
    }
    #[cfg(unix)]
    {
        let mut state = INTERRUPT_PIPE.lock().unwrap();
        if state.owners == 0 {
            let mut fds = [0i32; 2];
            if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
                log::error!("Failed to create interrupt pipe");
                return;
            }

            let flags = unsafe { libc::fcntl(fds[0], libc::F_GETFL) };
            assert!(flags >= 0, "Failed to get interrupt pipe flags");
            assert_eq!(
                unsafe { libc::fcntl(fds[0], libc::F_SETFL, flags | libc::O_NONBLOCK) },
                0,
                "Failed to set nonblocking state for interrupt pipe"
            );
            state.fds = fds;
        }
        state.owners += 1;
    }
}

/// Platform-specific network cleanup.
/// Port of upstream `Network::Finalize`.
fn finalize() {
    #[cfg(windows)]
    {
        let mut state = INTERRUPT_SOCKET.lock().unwrap();
        if state.owners == 0 {
            return;
        }
        state.owners -= 1;
        if state.owners != 0 {
            return;
        }
        if !state.cancelled {
            unsafe { winsock::closesocket(state.socket) };
        }
        state.socket = winsock::INVALID_SOCKET;
        state.cancelled = true;
        unsafe { winsock::WSACleanup() };
    }
    #[cfg(unix)]
    {
        let mut state = INTERRUPT_PIPE.lock().unwrap();
        if state.owners == 0 {
            return;
        }

        state.owners -= 1;
        if state.owners != 0 {
            return;
        }

        if state.fds[0] >= 0 {
            unsafe { libc::close(state.fds[0]) };
        }
        if state.fds[1] >= 0 {
            unsafe { libc::close(state.fds[1]) };
        }
        state.fds = [-1, -1];
    }
}

/// Return the read side of the socket-operation interrupt pipe.
#[cfg(unix)]
pub(crate) fn get_interrupt_socket() -> i32 {
    INTERRUPT_PIPE.lock().unwrap().fds[0]
}

#[cfg(windows)]
pub(crate) fn get_interrupt_socket() -> winsock::SOCKET {
    INTERRUPT_SOCKET.lock().unwrap().socket
}

/// Cancel pending socket operations by writing to the interrupt pipe.
/// Port of upstream `Network::CancelPendingSocketOperations`.
pub fn cancel_pending_socket_operations() {
    #[cfg(windows)]
    {
        let mut state = INTERRUPT_SOCKET.lock().unwrap();
        if state.owners != 0 && !state.cancelled {
            unsafe { winsock::closesocket(state.socket) };
            state.cancelled = true;
        }
    }
    #[cfg(unix)]
    {
        let state = INTERRUPT_PIPE.lock().unwrap();
        if state.fds[1] < 0 {
            return;
        }

        let value = 0u8;
        let written = unsafe {
            libc::write(
                state.fds[1],
                &value as *const u8 as *const libc::c_void,
                std::mem::size_of_val(&value),
            )
        };
        if written != 1 {
            log::error!("Failed to interrupt pending socket operations");
        }
    }
}

/// Restart socket operations after cancellation.
/// Port of upstream `Network::RestartSocketOperations`.
pub fn restart_socket_operations() {
    #[cfg(windows)]
    {
        let mut state = INTERRUPT_SOCKET.lock().unwrap();
        if state.owners == 0 || !state.cancelled {
            return;
        }
        state.socket = unsafe {
            winsock::socket(winapi::shared::ws2def::AF_INET, winsock::SOCK_DGRAM,
                winapi::shared::ws2def::IPPROTO_UDP as i32)
        };
        assert_ne!(state.socket, winsock::INVALID_SOCKET, "Failed to recreate interrupt socket");
        state.cancelled = false;
    }
    #[cfg(unix)]
    {
        let state = INTERRUPT_PIPE.lock().unwrap();
        if state.fds[0] < 0 {
            return;
        }

        let mut value = 0u8;
        let read = unsafe {
            libc::read(
                state.fds[0],
                &mut value as *mut u8 as *mut libc::c_void,
                std::mem::size_of_val(&value),
            )
        };
        if read != 1 {
            let error = std::io::Error::last_os_error();
            let raw_error = error.raw_os_error();
            if raw_error != Some(libc::EAGAIN) && raw_error != Some(libc::EWOULDBLOCK) {
                log::error!("Failed to acknowledge interrupt on shutdown: {error}");
            }
        }
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn winsock_lifecycle_retains_shared_interrupt_and_restarts_after_cancel() {
        const CHILD: &str = "RUZU_TEST_WINSOCK_LIFECYCLE";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "internal_network::network::windows_tests::winsock_lifecycle_retains_shared_interrupt_and_restarts_after_cancel", "--nocapture"])
                .env(CHILD, "1").status().unwrap();
            assert!(status.success());
            return;
        }
        fn socket_type(socket: winsock::SOCKET) -> Result<i32, i32> {
            let mut value = 0i32;
            let mut size = std::mem::size_of_val(&value) as i32;
            let result = unsafe { winsock::getsockopt(socket, winsock::SOL_SOCKET,
                winsock::SO_TYPE, &mut value as *mut i32 as *mut i8, &mut size) };
            if result == 0 { Ok(value) } else { Err(unsafe { winsock::WSAGetLastError() }) }
        }
        assert_eq!(get_interrupt_socket(), winsock::INVALID_SOCKET);
        cancel_pending_socket_operations();
        restart_socket_operations();
        assert_eq!(get_interrupt_socket(), winsock::INVALID_SOCKET);
        let first = NetworkInstance::new();
        let second = NetworkInstance::new();
        let initial = get_interrupt_socket();
        assert_eq!(socket_type(initial), Ok(winsock::SOCK_DGRAM));
        restart_socket_operations();
        assert_eq!(get_interrupt_socket(), initial);
        drop(first);
        assert_eq!(socket_type(initial), Ok(winsock::SOCK_DGRAM));
        cancel_pending_socket_operations();
        assert_eq!(get_interrupt_socket(), initial);
        assert_eq!(socket_type(initial), Err(winsock::WSAENOTSOCK));
        // A new socket may reuse the cancelled handle. Repeated cancellation
        // must not accidentally close this unrelated socket.
        let unrelated = unsafe { winsock::socket(winapi::shared::ws2def::AF_INET,
            winsock::SOCK_DGRAM, winapi::shared::ws2def::IPPROTO_UDP as i32) };
        assert_ne!(unrelated, winsock::INVALID_SOCKET);
        cancel_pending_socket_operations();
        assert_eq!(socket_type(unrelated), Ok(winsock::SOCK_DGRAM));
        restart_socket_operations();
        assert_eq!(socket_type(get_interrupt_socket()), Ok(winsock::SOCK_DGRAM));
        assert_eq!(unsafe { winsock::closesocket(unrelated) }, 0);
        drop(second);
        assert_eq!(get_interrupt_socket(), winsock::INVALID_SOCKET);
        // Reinitialization after the last owner is released is supported.
        let third = NetworkInstance::new();
        assert_eq!(socket_type(get_interrupt_socket()), Ok(winsock::SOCK_DGRAM));
        cancel_pending_socket_operations();
        drop(third);
        assert_eq!(get_interrupt_socket(), winsock::INVALID_SOCKET);
    }
}

/// Translate an IPv4 address from platform representation.
///
/// Corresponds to upstream `Network::TranslateIPv4`.
pub fn translate_ipv4(addr: Ipv4Addr) -> IPv4Address {
    addr.octets()
}

/// Returns host's IPv4 address.
///
/// Corresponds to upstream `Network::GetHostIPv4Address`.
pub fn get_host_ipv4_address() -> Option<IPv4Address> {
    let iface = super::network_interface::get_selected_network_interface()?;
    Some(iface.ip_address.octets())
}

/// Convert IPv4 address to string.
///
/// Corresponds to upstream `Network::IPv4AddressToString`.
pub fn ipv4_address_to_string(ip_addr: IPv4Address) -> String {
    format!(
        "{}.{}.{}.{}",
        ip_addr[0], ip_addr[1], ip_addr[2], ip_addr[3]
    )
}

/// Convert IPv4 address to integer (big-endian / network order).
///
/// Corresponds to upstream `Network::IPv4AddressToInteger`.
pub fn ipv4_address_to_integer(ip_addr: IPv4Address) -> u32 {
    (ip_addr[0] as u32) << 24
        | (ip_addr[1] as u32) << 16
        | (ip_addr[2] as u32) << 8
        | (ip_addr[3] as u32)
}

/// Get address info for a host.
///
/// Corresponds to upstream `Network::GetAddressInfo`.
pub fn get_address_info(
    host: &str,
    service: Option<&str>,
) -> Result<Vec<AddrInfo>, GetAddrInfoError> {
    #[cfg(unix)]
    {
        use std::ffi::CString;

        let c_host = CString::new(host).map_err(|_| GetAddrInfoError::Fail)?;
        let c_service = service.map(|s| CString::new(s).ok()).flatten();

        let mut result_ptr: *mut libc::addrinfo = std::ptr::null_mut();
        let service_ptr = c_service
            .as_ref()
            .map(|s| s.as_ptr())
            .unwrap_or(std::ptr::null());

        let ret = unsafe {
            libc::getaddrinfo(
                c_host.as_ptr(),
                service_ptr,
                std::ptr::null(),
                &mut result_ptr,
            )
        };

        if ret != 0 {
            return Err(GetAddrInfoError::Fail);
        }

        let mut results = Vec::new();
        let mut cur = result_ptr;
        while !cur.is_null() {
            let info = unsafe { &*cur };
            let domain = match info.ai_family {
                libc::AF_INET => Domain::INET,
                _ => {
                    cur = info.ai_next;
                    continue;
                }
            };

            let mut addr = SockAddrIn::default();
            if !info.ai_addr.is_null()
                && info.ai_addrlen >= std::mem::size_of::<libc::sockaddr_in>() as u32
            {
                let sa = unsafe { &*(info.ai_addr as *const libc::sockaddr_in) };
                let ip_bytes = u32::from_be(sa.sin_addr.s_addr).to_ne_bytes();
                addr.family = Some(Domain::INET);
                addr.ip = ip_bytes;
                addr.portno = u16::from_be(sa.sin_port);
            }

            results.push(AddrInfo {
                family: domain,
                socket_type: match info.ai_socktype {
                    libc::SOCK_STREAM => Type::STREAM,
                    libc::SOCK_DGRAM => Type::DGRAM,
                    _ => Type::STREAM,
                },
                protocol: match info.ai_protocol {
                    libc::IPPROTO_TCP => Protocol::TCP,
                    libc::IPPROTO_UDP => Protocol::UDP,
                    _ => Protocol::TCP,
                },
                addr,
                canon_name: None,
            });

            cur = info.ai_next;
        }

        unsafe {
            libc::freeaddrinfo(result_ptr);
        }

        Ok(results)
    }
    #[cfg(not(unix))]
    {
        let _ = (host, service);
        Err(GetAddrInfoError::Fail)
    }
}
