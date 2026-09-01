// SPDX-FileCopyrightText: Copyright 2020 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/sockets/sockets_translate.h
//! Port of zuyu/src/core/hle/service/sockets/sockets_translate.cpp
//!
//! Translation utilities between BSD/guest socket types and internal network types.

use super::sockets::{
    Domain, Errno, GetAddrInfoError, PollEvents, Protocol, ShutdownHow, SockAddrIn, Type,
};
use crate::internal_network::network::{
    Domain as NetDomain, Errno as NetErrno, GetAddrInfoError as NetGetAddrInfoError,
    PollEvents as NetPollEvents, Protocol as NetProtocol, ShutdownHow as NetShutdownHow,
    SockAddrIn as NetSockAddrIn, Type as NetType,
};

/// Corresponds to `Translate(Network::Errno)` in upstream sockets_translate.cpp.
pub fn translate_errno(value: NetErrno) -> Errno {
    match value {
        NetErrno::Success => Errno::SUCCESS,
        NetErrno::Badf => Errno::BADF,
        NetErrno::Again => Errno::AGAIN,
        NetErrno::Inval => Errno::INVAL,
        NetErrno::Mfile => Errno::MFILE,
        NetErrno::Pipe => Errno::PIPE,
        NetErrno::Connrefused => Errno::CONNREFUSED,
        NetErrno::Notconn => Errno::NOTCONN,
        NetErrno::Timedout => Errno::TIMEDOUT,
        NetErrno::Connaborted => Errno::CONNABORTED,
        NetErrno::Connreset => Errno::CONNRESET,
        NetErrno::Inprogress => Errno::INPROGRESS,
        NetErrno::Isconn => Errno::ISCONN,
        _ => {
            log::warn!("Unimplemented errno={value:?}");
            Errno::SUCCESS
        }
    }
}

/// Corresponds to `Translate(std::pair<s32, Network::Errno>)` in upstream.
pub fn translate_result(value: (i32, NetErrno)) -> (i32, Errno) {
    (value.0, translate_errno(value.1))
}

/// Corresponds to `Translate(Network::GetAddrInfoError)` in upstream.
pub fn translate_get_addr_info_error(error: NetGetAddrInfoError) -> GetAddrInfoError {
    match error {
        NetGetAddrInfoError::Success => GetAddrInfoError::SUCCESS,
        NetGetAddrInfoError::Addrfamily => GetAddrInfoError::ADDRFAMILY,
        NetGetAddrInfoError::Again => GetAddrInfoError::AGAIN,
        NetGetAddrInfoError::Badflags => GetAddrInfoError::BADFLAGS,
        NetGetAddrInfoError::Fail => GetAddrInfoError::FAIL,
        NetGetAddrInfoError::Family => GetAddrInfoError::FAMILY,
        NetGetAddrInfoError::Memory => GetAddrInfoError::MEMORY,
        NetGetAddrInfoError::Nodata => GetAddrInfoError::NODATA,
        NetGetAddrInfoError::Noname => GetAddrInfoError::NONAME,
        NetGetAddrInfoError::Service => GetAddrInfoError::SERVICE,
        NetGetAddrInfoError::Socktype => GetAddrInfoError::SOCKTYPE,
        NetGetAddrInfoError::System => GetAddrInfoError::SYSTEM,
        NetGetAddrInfoError::Badhints => GetAddrInfoError::BADHINTS,
        NetGetAddrInfoError::Protocol => GetAddrInfoError::PROTOCOL,
        NetGetAddrInfoError::Overflow => GetAddrInfoError::OVERFLOW,
        NetGetAddrInfoError::Other => GetAddrInfoError::OTHER,
    }
}

/// Corresponds to `Translate(GetAddrInfoError)` in upstream.
pub fn get_addr_info_error_string(error: GetAddrInfoError) -> &'static str {
    match error {
        GetAddrInfoError::SUCCESS => "Success",
        GetAddrInfoError::ADDRFAMILY => "Address family for hostname not supported",
        GetAddrInfoError::AGAIN => "Temporary failure in name resolution",
        GetAddrInfoError::BADFLAGS => "Invalid value for ai_flags",
        GetAddrInfoError::FAIL => "Non-recoverable failure in name resolution",
        GetAddrInfoError::FAMILY => "ai_family not supported",
        GetAddrInfoError::MEMORY => "Memory allocation failure",
        GetAddrInfoError::NODATA => "No address associated with hostname",
        GetAddrInfoError::NONAME => "hostname nor servname provided, or not known",
        GetAddrInfoError::SERVICE => "servname not supported for ai_socktype",
        GetAddrInfoError::SOCKTYPE => "ai_socktype not supported",
        GetAddrInfoError::SYSTEM => "System error returned in errno",
        GetAddrInfoError::BADHINTS => "Invalid value for hints",
        GetAddrInfoError::PROTOCOL => "Resolved protocol is unknown",
        GetAddrInfoError::OVERFLOW => "Argument buffer overflow",
        GetAddrInfoError::OTHER => "Unknown error",
    }
}

/// Corresponds to `Translate(Domain)` in upstream.
pub fn translate_domain(domain: Domain) -> NetDomain {
    match domain {
        Domain::Unspecified => NetDomain::Unspecified,
        Domain::INET => NetDomain::INET,
        _ => {
            log::warn!("Unimplemented domain={domain:?}");
            NetDomain::Unspecified
        }
    }
}

/// Corresponds to `Translate(Network::Domain)` in upstream.
pub fn translate_domain_from_network(domain: NetDomain) -> Domain {
    match domain {
        NetDomain::Unspecified => Domain::Unspecified,
        NetDomain::INET => Domain::INET,
    }
}

/// Corresponds to `Translate(Type)` in upstream.
pub fn translate_type(ty: Type) -> NetType {
    match ty {
        Type::Unspecified => NetType::Unspecified,
        Type::STREAM => NetType::STREAM,
        Type::DGRAM => NetType::DGRAM,
        Type::RAW => NetType::RAW,
        Type::SEQPACKET => NetType::SEQPACKET,
        _ => {
            log::warn!("Unimplemented type={ty:?}");
            NetType::Unspecified
        }
    }
}

/// Corresponds to `Translate(Network::Type)` in upstream.
pub fn translate_type_from_network(ty: NetType) -> Type {
    match ty {
        NetType::Unspecified => Type::Unspecified,
        NetType::STREAM => Type::STREAM,
        NetType::DGRAM => Type::DGRAM,
        NetType::RAW => Type::RAW,
        NetType::SEQPACKET => Type::SEQPACKET,
    }
}

/// Corresponds to `Translate(Protocol)` in upstream.
pub fn translate_protocol(protocol: Protocol) -> NetProtocol {
    match protocol {
        Protocol::Unspecified => NetProtocol::Unspecified,
        Protocol::TCP => NetProtocol::TCP,
        Protocol::UDP => NetProtocol::UDP,
        _ => {
            log::warn!("Unimplemented protocol={protocol:?}");
            NetProtocol::Unspecified
        }
    }
}

/// Corresponds to `Translate(Network::Protocol)` in upstream.
pub fn translate_protocol_from_network(protocol: NetProtocol) -> Protocol {
    match protocol {
        NetProtocol::Unspecified => Protocol::Unspecified,
        NetProtocol::TCP => Protocol::TCP,
        NetProtocol::UDP => Protocol::UDP,
        _ => {
            log::warn!("Unimplemented protocol={protocol:?}");
            Protocol::Unspecified
        }
    }
}

/// Corresponds to `Translate(PollEvents)` in upstream.
pub fn translate_poll_events(mut flags: PollEvents) -> NetPollEvents {
    let mut result = NetPollEvents::empty();
    macro_rules! translate {
        ($from:ident, $to:ident) => {
            if flags.contains(PollEvents::$from) {
                flags.remove(PollEvents::$from);
                result.insert(NetPollEvents::$to);
            }
        };
    }
    translate!(IN, IN);
    translate!(PRI, PRI);
    translate!(OUT, OUT);
    translate!(ERR, ERR);
    translate!(HUP, HUP);
    translate!(NVAL, NVAL);
    translate!(RD_NORM, RD_NORM);
    translate!(RD_BAND, RD_BAND);
    translate!(WR_BAND, WR_BAND);
    if !flags.is_empty() {
        log::warn!("Unimplemented poll flags={:#x}", flags.bits());
    }
    result
}

/// Corresponds to `Translate(Network::PollEvents)` in upstream.
pub fn translate_poll_events_from_network(mut flags: NetPollEvents) -> PollEvents {
    let mut result = PollEvents::empty();
    macro_rules! translate {
        ($from:ident, $to:ident) => {
            if flags.contains(NetPollEvents::$from) {
                flags.remove(NetPollEvents::$from);
                result.insert(PollEvents::$to);
            }
        };
    }
    translate!(IN, IN);
    translate!(PRI, PRI);
    translate!(OUT, OUT);
    translate!(ERR, ERR);
    translate!(HUP, HUP);
    translate!(NVAL, NVAL);
    translate!(RD_NORM, RD_NORM);
    translate!(RD_BAND, RD_BAND);
    translate!(WR_BAND, WR_BAND);
    if !flags.is_empty() {
        log::warn!("Unimplemented network poll flags={:#x}", flags.bits());
    }
    result
}

/// Corresponds to `Translate(SockAddrIn)` in upstream.
pub fn translate_sockaddr_to_network(value: &SockAddrIn) -> NetSockAddrIn {
    NetSockAddrIn {
        family: Some(translate_domain(Domain(value.family as u32))),
        ip: value.ip,
        portno: value.portno.swap_bytes(),
    }
}

/// Corresponds to `Translate(Network::SockAddrIn)` in upstream.
pub fn translate_sockaddr_from_network(value: &NetSockAddrIn) -> SockAddrIn {
    SockAddrIn {
        len: std::mem::size_of::<SockAddrIn>() as u8,
        family: value
            .family
            .map(translate_domain_from_network)
            .unwrap_or(Domain::Unspecified)
            .0 as u8,
        portno: value.portno.swap_bytes(),
        ip: value.ip,
        zeroes: [0; 8],
    }
}

/// Corresponds to `Translate(ShutdownHow)` in upstream.
pub fn translate_shutdown_how(how: ShutdownHow) -> NetShutdownHow {
    match how {
        ShutdownHow::RD => NetShutdownHow::RD,
        ShutdownHow::WR => NetShutdownHow::WR,
        ShutdownHow::RDWR => NetShutdownHow::RDWR,
        _ => {
            log::warn!("Unimplemented how={how:?}");
            NetShutdownHow::RD
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_guest_values_use_upstream_defaults() {
        assert_eq!(translate_domain(Domain(u32::MAX)), NetDomain::Unspecified);
        assert_eq!(translate_type(Type(u32::MAX)), NetType::Unspecified);
        assert_eq!(translate_protocol(Protocol::ICMP), NetProtocol::Unspecified);
        assert_eq!(
            translate_shutdown_how(ShutdownHow(i32::MAX)),
            NetShutdownHow::RD
        );
    }

    #[test]
    fn already_connected_errno_matches_upstream() {
        assert_eq!(translate_errno(NetErrno::Isconn), Errno::ISCONN);
    }

    #[test]
    fn sockaddr_translation_preserves_upstream_layout_and_byte_order() {
        let guest = SockAddrIn {
            len: 16,
            family: Domain::INET.0 as u8,
            portno: 0x3412,
            ip: [127, 0, 0, 1],
            zeroes: [0; 8],
        };
        let network = translate_sockaddr_to_network(&guest);
        assert_eq!(network.family, Some(NetDomain::INET));
        assert_eq!(network.portno, 0x1234);
        assert_eq!(translate_sockaddr_from_network(&network).portno, 0x3412);
    }

    #[test]
    fn sockaddr_translation_accepts_every_length_like_upstream() {
        let mut guest = SockAddrIn {
            family: Domain::INET.0 as u8,
            ..SockAddrIn::default()
        };
        for length in u8::MIN..=u8::MAX {
            guest.len = length;
            assert_eq!(
                translate_sockaddr_to_network(&guest).family,
                Some(NetDomain::INET)
            );
        }
    }
}
