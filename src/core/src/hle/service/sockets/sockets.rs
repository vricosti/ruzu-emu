// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/sockets/sockets.h
//! Port of zuyu/src/core/hle/service/sockets/sockets.cpp
//!
//! Socket service registration and common types.

use super::bsd::{Bsd, BsdCfg, BsdNu};
use super::nsd::Nsd;
use super::sfdnsres::{DnsPriv, Sfdnsres};
use std::collections::BTreeMap;

use crate::hle::result::ResultCode;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

pub struct EthcC {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl EthcC {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "Initialize"),
                (1, None, "Cancel"),
                (2, None, "GetResult"),
                (3, None, "GetMediaList"),
                (4, None, "SetMediaType"),
                (5, None, "GetMediaType"),
                (6, None, "GetMacAddress"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for EthcC {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "ethc:c"
    }
}

impl ServiceFramework for EthcC {
    fn get_service_name(&self) -> &str {
        "ethc:c"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct EthcI {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl EthcI {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "GetReadableHandle"),
                (1, None, "Cancel"),
                (2, None, "GetResult"),
                (3, None, "GetInterfaceList"),
                (4, None, "GetInterfaceCount"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for EthcI {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "ethc:i"
    }
}

impl ServiceFramework for EthcI {
    fn get_service_name(&self) -> &str {
        "ethc:i"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct ISfDriverServiceCreator {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ISfDriverServiceCreator {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[(0, None, "CreateDriverService")]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for ISfDriverServiceCreator {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "eth:nd"
    }
}

impl ServiceFramework for ISfDriverServiceCreator {
    fn get_service_name(&self) -> &str {
        "eth:nd"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// Errno values matching upstream.
///
/// Corresponds to `Errno` in upstream sockets.h.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Errno {
    SUCCESS = 0,
    BADF = 9,
    AGAIN = 11,
    INVAL = 22,
    MFILE = 24,
    PIPE = 32,
    MSGSIZE = 90,
    CONNABORTED = 103,
    CONNRESET = 104,
    NOTCONN = 107,
    TIMEDOUT = 110,
    CONNREFUSED = 111,
    INPROGRESS = 115,
}

/// GetAddrInfoError codes matching upstream.
///
/// Corresponds to `GetAddrInfoError` in upstream sockets.h.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum GetAddrInfoError {
    SUCCESS = 0,
    ADDRFAMILY = 1,
    AGAIN = 2,
    BADFLAGS = 3,
    FAIL = 4,
    FAMILY = 5,
    MEMORY = 6,
    NODATA = 7,
    NONAME = 8,
    SERVICE = 9,
    SOCKTYPE = 10,
    SYSTEM = 11,
    BADHINTS = 12,
    PROTOCOL = 13,
    OVERFLOW = 14,
    OTHER = 15,
}

/// Declares the Rust counterpart of a C++ scoped enumeration that the guest
/// supplies raw values for.
///
/// Upstream writes these as `enum class X : u32 { ... }` and builds them from
/// an IPC word with `static_cast<X>(word)`. In C++ that cast is value
/// preserving for *every* value of the underlying type — an enumeration with a
/// fixed underlying type has the same range as that type — and each `switch`
/// that consumes one carries a `default:` arm for the unnamed values. A Rust
/// `enum` cannot hold an unnamed discriminant, so these types are newtypes over
/// the underlying integer with the upstream enumerators as associated
/// constants. Deriving `PartialEq`/`Eq` keeps them usable in `match` patterns,
/// so the ported `switch` statements read the same as upstream, and the manual
/// `Debug` prints the enumerator name exactly like upstream's fmt formatter.
macro_rules! guest_enum {
    (
        $(#[$meta:meta])*
        pub struct $name:ident : $repr:ty {
            $($variant:ident = $value:expr),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[repr(transparent)]
        #[derive(Clone, Copy, PartialEq, Eq)]
        pub struct $name(pub $repr);

        #[allow(non_upper_case_globals)]
        impl $name {
            $(pub const $variant: Self = Self($value);)*
        }

        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match *self {
                    $(Self::$variant => f.write_str(stringify!($variant)),)*
                    Self(value) => write!(f, concat!(stringify!($name), "({:#x})"), value),
                }
            }
        }
    };
}

guest_enum! {
    /// Domain (address family).
    ///
    /// Corresponds to `Domain` in upstream sockets.h.
    pub struct Domain: u32 {
        Unspecified = 0,
        INET = 2,
    }
}

guest_enum! {
    /// Type (socket type).
    ///
    /// Corresponds to `Type` in upstream sockets.h.
    pub struct Type: u32 {
        Unspecified = 0,
        STREAM = 1,
        DGRAM = 2,
        RAW = 3,
        SEQPACKET = 5,
    }
}

guest_enum! {
    /// Protocol.
    ///
    /// Corresponds to `Protocol` in upstream sockets.h.
    pub struct Protocol: u32 {
        Unspecified = 0,
        ICMP = 1,
        TCP = 6,
        UDP = 17,
    }
}

/// Socket level for setsockopt/getsockopt.
///
/// Corresponds to `SocketLevel` in upstream sockets.h.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketLevel {
    SOCKET = 0xffff, // SOL_SOCKET
}

guest_enum! {
    /// Socket option names.
    ///
    /// Corresponds to `OptName` in upstream sockets.h.
    pub struct OptName: u32 {
        REUSEADDR = 0x4,
        KEEPALIVE = 0x8,
        BROADCAST = 0x20,
        LINGER = 0x80,
        SNDBUF = 0x1001,
        RCVBUF = 0x1002,
        SNDTIMEO = 0x1005,
        RCVTIMEO = 0x1006,
        ERROR = 0x1007,
        NOSIGPIPE = 0x800, // at least according to libnx
    }
}

guest_enum! {
    /// ShutdownHow modes.
    ///
    /// Corresponds to `ShutdownHow` in upstream sockets.h.
    pub struct ShutdownHow: i32 {
        RD = 0,
        WR = 1,
        RDWR = 2,
    }
}

guest_enum! {
    /// Fcntl command codes.
    ///
    /// Corresponds to `FcntlCmd` in upstream sockets.h.
    pub struct FcntlCmd: i32 {
        GETFL = 3,
        SETFL = 4,
    }
}

/// Guest socket address structure.
///
/// Corresponds to `SockAddrIn` in upstream sockets.h.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct SockAddrIn {
    pub len: u8,
    pub family: u8,
    pub portno: u16,
    pub ip: [u8; 4],
    pub zeroes: [u8; 8],
}

bitflags::bitflags! {
    /// PollEvents flags.
    ///
    /// Corresponds to `PollEvents` in upstream sockets.h.
    /// Uses DECLARE_ENUM_FLAG_OPERATORS in C++.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct PollEvents: u16 {
        const IN = 1 << 0;
        const PRI = 1 << 1;
        const OUT = 1 << 2;
        const ERR = 1 << 3;
        const HUP = 1 << 4;
        const NVAL = 1 << 5;
        const RD_NORM = 1 << 6;
        const RD_BAND = 1 << 7;
        const WR_BAND = 1 << 8;
    }
}

/// PollFD structure for poll operations.
///
/// Corresponds to `PollFD` in upstream sockets.h.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct PollFD {
    pub fd: i32,
    pub events: u16,  // PollEvents
    pub revents: u16, // PollEvents
}

/// Linger structure for SO_LINGER option.
///
/// Corresponds to `Linger` in upstream sockets.h.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct Linger {
    pub onoff: u32,
    pub linger: u32,
}

/// LoopProcess -- registers "bsd:u", "bsd:s", "bsdcfg", "nsd:u", "nsd:a", "sfdnsres" services.
///
/// Corresponds to `Service::Sockets::LoopProcess` in upstream sockets.cpp.
pub fn loop_process(system: crate::core::SystemRef) {
    use crate::hle::service::hle_ipc::SessionRequestHandlerPtr;
    use crate::hle::service::server_manager::ServerManager;
    use std::sync::{Arc, Mutex};

    let server_manager = ServerManager::new_shared(system);

    {
        let mut server_manager = server_manager.lock().unwrap();
        let bsd_s: SessionRequestHandlerPtr = Arc::new(Mutex::new(Bsd::new("bsd:s", false)));
        let bsd_u: SessionRequestHandlerPtr = Arc::new(Mutex::new(Bsd::new("bsd:u", true)));
        let bsd_a: SessionRequestHandlerPtr = Arc::new(Mutex::new(Bsd::new("bsd:a", true)));
        let bsd_nu: SessionRequestHandlerPtr = Arc::new(BsdNu::new());
        let bsdcfg: SessionRequestHandlerPtr = Arc::new(BsdCfg::new("bsdcfg"));
        let ifcfg: SessionRequestHandlerPtr = Arc::new(BsdCfg::new("ifcfg"));
        let nsd_a: SessionRequestHandlerPtr = Arc::new(Nsd::new("nsd:a"));
        let nsd_u: SessionRequestHandlerPtr = Arc::new(Nsd::new("nsd:u"));
        let sfdnsres: SessionRequestHandlerPtr = Arc::new(Sfdnsres::new());

        server_manager.register_named_service("ethc:c", Box::new(|| Arc::new(EthcC::new())), 64);
        server_manager.register_named_service("ethc:i", Box::new(|| Arc::new(EthcI::new())), 64);
        server_manager.register_named_service_handler("bsd:s", bsd_s, 64);
        server_manager.register_named_service_handler("bsd:u", bsd_u, 64);
        server_manager.register_named_service_handler("bsd:a", bsd_a, 64);
        server_manager.register_named_service_handler("bsd:nu", bsd_nu, 64);
        server_manager.register_named_service_handler("bsdcfg", bsdcfg, 64);
        server_manager.register_named_service_handler("ifcfg", ifcfg, 64);
        server_manager.register_named_service(
            "dns:priv",
            Box::new(|| Arc::new(DnsPriv::new())),
            64,
        );
        server_manager.register_named_service(
            "eth:nd",
            Box::new(|| Arc::new(ISfDriverServiceCreator::new())),
            64,
        );
        server_manager.register_named_service_handler("nsd:a", nsd_a, 64);
        server_manager.register_named_service_handler("nsd:u", nsd_u, 64);
        server_manager.register_named_service_handler("sfdnsres", sfdnsres, 64);
    }

    // Wait for the main thread to finish spawning all initial services before
    // calling start_additional_host_threads. Without this gate, ruzu's
    // bsdsocket host thread can race ahead and allocate its 2 dummy tids
    // between host services and sm — placing them at tid 23/25 instead of
    while !crate::hle::service::services::SERVICES_INIT_DONE
        .load(std::sync::atomic::Ordering::Acquire)
    {
        std::thread::yield_now();
    }

    {
        let mut server_manager = server_manager.lock().unwrap();
        server_manager.start_additional_host_threads("bsdsocket", 2);
    }

    ServerManager::run_server_shared(server_manager);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Upstream builds these from an IPC word with `static_cast<X>(word)`,
    /// which is value preserving for every value of the underlying type. A
    /// value the guest sends that names no enumerator must survive the
    /// conversion so the consuming `switch` can reach its `default:` arm —
    /// Super Tux Kart calls `setsockopt` with `optname=0x1`, which used to
    /// abort the emulator when the port modelled `OptName` as a Rust `enum`.
    #[test]
    fn a_guest_word_that_names_no_enumerator_survives_the_conversion() {
        assert_eq!(OptName(0x1).0, 0x1);
        assert_ne!(OptName(0x1), OptName::REUSEADDR);
        assert_eq!(Domain(0xdead).0, 0xdead);
        assert_eq!(Type(0x2000_0001).0, 0x2000_0001);
        assert_eq!(Protocol(99).0, 99);
        assert_eq!(FcntlCmd(-1).0, -1);
        assert_eq!(ShutdownHow(7).0, 7);
    }

    /// The enumerator values are the ones in upstream `sockets.h`.
    #[test]
    fn the_enumerators_keep_their_upstream_values() {
        assert_eq!(std::mem::size_of::<Domain>(), std::mem::size_of::<u32>());
        assert_eq!(std::mem::size_of::<Type>(), std::mem::size_of::<u32>());
        assert_eq!(std::mem::size_of::<Protocol>(), std::mem::size_of::<u32>());
        assert_eq!(std::mem::size_of::<OptName>(), std::mem::size_of::<u32>());
        assert_eq!(
            std::mem::size_of::<ShutdownHow>(),
            std::mem::size_of::<i32>()
        );
        assert_eq!(std::mem::size_of::<FcntlCmd>(), std::mem::size_of::<i32>());
        assert_eq!(Domain::Unspecified.0, 0);
        assert_eq!(Domain::INET.0, 2);
        assert_eq!(Type::STREAM.0, 1);
        assert_eq!(Type::SEQPACKET.0, 5);
        assert_eq!(Protocol::TCP.0, 6);
        assert_eq!(Protocol::UDP.0, 17);
        assert_eq!(OptName::REUSEADDR.0, 0x4);
        assert_eq!(OptName::LINGER.0, 0x80);
        assert_eq!(OptName::NOSIGPIPE.0, 0x800);
        assert_eq!(OptName::ERROR.0, 0x1007);
        assert_eq!(FcntlCmd::GETFL.0, 3);
        assert_eq!(FcntlCmd::SETFL.0, 4);
        assert_eq!(ShutdownHow::RDWR.0, 2);
    }

    /// Upstream's fmt formatter prints the enumerator name; the log messages in
    /// the `default:` arms rely on unnamed values still being readable.
    #[test]
    fn debug_names_the_enumerator_and_falls_back_to_the_raw_value() {
        assert_eq!(format!("{:?}", OptName::RCVTIMEO), "RCVTIMEO");
        assert_eq!(format!("{:?}", OptName(0x1)), "OptName(0x1)");
        assert_eq!(format!("{:?}", Domain::INET), "INET");
        assert_eq!(format!("{:?}", FcntlCmd(9)), "FcntlCmd(0x9)");
    }
}
