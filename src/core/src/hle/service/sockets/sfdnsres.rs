// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/sockets/sfdnsres.h
//! Port of zuyu/src/core/hle/service/sockets/sfdnsres.cpp
//!
//! SFDNSRES service — DNS resolution ("sfdnsres").

use std::collections::BTreeMap;

use super::sockets::{Domain, Errno, GetAddrInfoError, SockAddrIn};
use super::sockets_translate::get_addr_info_error_string;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command table for SFDNSRES.
///
/// Corresponds to the function table in upstream sfdnsres.cpp.
pub mod commands {
    pub const SET_DNS_ADDRESSES_PRIVATE_REQUEST: u32 = 0;
    pub const GET_DNS_ADDRESS_PRIVATE_REQUEST: u32 = 1;
    pub const GET_HOST_BY_NAME_REQUEST: u32 = 2;
    pub const GET_HOST_BY_ADDR_REQUEST: u32 = 3;
    pub const GET_HOST_STRING_ERROR_REQUEST: u32 = 4;
    pub const GET_GAI_STRING_ERROR_REQUEST: u32 = 5;
    pub const GET_ADDR_INFO_REQUEST: u32 = 6;
    pub const GET_NAME_INFO_REQUEST: u32 = 7;
    pub const REQUEST_CANCEL_HANDLE_REQUEST: u32 = 8;
    pub const CANCEL_REQUEST: u32 = 9;
    pub const GET_HOST_BY_NAME_REQUEST_WITH_OPTIONS: u32 = 10;
    pub const GET_HOST_BY_ADDR_REQUEST_WITH_OPTIONS: u32 = 11;
    pub const GET_ADDR_INFO_REQUEST_WITH_OPTIONS: u32 = 12;
    pub const GET_NAME_INFO_REQUEST_WITH_OPTIONS: u32 = 13;
    pub const RESOLVER_SET_OPTION_REQUEST: u32 = 14;
    pub const RESOLVER_GET_OPTION_REQUEST: u32 = 15;
}

/// NetDbError codes.
///
/// Corresponds to `NetDbError` in upstream sfdnsres.cpp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
enum NetDbError {
    Success = 0,
    HostNotFound = 1,
    TryAgain = 2,
}

const BLOCKED_DOMAINS: &[&str] = &[
    "nintendo.net",
    "srv.nintendo.net",
    "nintendo.es",
    "nintendowifi.net",
    "nintendo-europe.com",
    "nintendo.com.hk",
    "nintendo.com.au",
    "nintendo.co.kr",
    "nintendo.co.uk",
    "nintendo.co.jp",
    "nintendo.co.nz",
    "nintendo.co.za",
    "nintendo.com",
    "nintendo.jp",
    "nintendo.tw",
    "nintendo.at",
    "nintendo.be",
    "nintendo.dk",
    "nintendo.de",
    "nintendo.fi",
    "nintendo.fr",
    "nintendo.gr",
    "nintendo.hu",
    "nintendo.it",
    "nintendo.nl",
    "nintendo.no",
    "nintendo.pt",
    "nintendo.ru",
    "nintendo.ch",
    "nintendo.se",
    "nintendoswitch.com.cn",
    "nintendoswitch.com",
    "sun.hac.lp1.d4c.nintendo.net",
    "phoenix-api.wbagora.com",
    "battle.net",
    "microsoft.com",
    "mojang.com",
    "xboxlive.com",
    "api.epicgames.dev",
    "minecraftservices.com",
    "508223012e5a5ff19f30a391b2bdadc0.my.2k.com",
];

fn is_blocked_host(host: &str) -> bool {
    BLOCKED_DOMAINS.iter().any(|domain| host.contains(domain))
}

/// NSD resolution expands Nintendo service identifiers and environment placeholders before DNS.
/// Until that resolver is implemented, fail those requests before host DNS resolution. Reporting
/// a successful loopback resolution would incorrectly move the guest into its connection path.
fn should_block_nsd_resolution(use_nsd_resolve: u8) -> bool {
    use_nsd_resolve != 0
}

/// Corresponds to `GetAddrInfoErrorToNetDbError` in upstream sfdnsres.cpp.
fn get_addr_info_error_to_netdb_error(result: GetAddrInfoError) -> NetDbError {
    match result {
        GetAddrInfoError::SUCCESS => NetDbError::Success,
        GetAddrInfoError::AGAIN => NetDbError::TryAgain,
        GetAddrInfoError::NODATA => NetDbError::HostNotFound,
        GetAddrInfoError::SERVICE => NetDbError::Success,
        _ => NetDbError::HostNotFound,
    }
}

/// Corresponds to `GetAddrInfoErrorToErrno` in upstream sfdnsres.cpp.
fn get_addr_info_error_to_errno(result: GetAddrInfoError) -> Errno {
    match result {
        GetAddrInfoError::SUCCESS | GetAddrInfoError::AGAIN | GetAddrInfoError::NODATA => {
            Errno::SUCCESS
        }
        GetAddrInfoError::SERVICE => Errno::INVAL,
        _ => Errno::SUCCESS,
    }
}

/// Input parameters for GetHostByNameRequest / GetAddrInfoRequest.
///
/// Corresponds to `InputParameters` struct in upstream GetHostByNameRequestImpl / GetAddrInfoRequestImpl.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct DnsInputParameters {
    pub use_nsd_resolve: u8,
    pub _pad: [u8; 3],
    pub cancel_handle: u32,
    pub process_id: u64,
}

/// Output from GetHostByNameRequest.
///
/// Corresponds to `OutputParameters` in `GetHostByNameRequest` upstream.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct GetHostByNameOutput {
    pub netdb_error: i32,
    pub bsd_errno: u32,
    pub data_size: u32,
}

/// Output from GetHostByNameRequestWithOptions.
///
/// Corresponds to `OutputParameters` in `GetHostByNameRequestWithOptions` upstream.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct GetHostByNameWithOptionsOutput {
    pub data_size: u32,
    pub netdb_error: i32,
    pub bsd_errno: u32,
}

/// Output from GetAddrInfoRequest.
///
/// Corresponds to `OutputParameters` in `GetAddrInfoRequest` upstream.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct GetAddrInfoOutput {
    pub bsd_errno: u32,
    pub gai_error: i32,
    pub data_size: u32,
}

/// Output from GetAddrInfoRequestWithOptions.
///
/// Corresponds to `OutputParameters` in `GetAddrInfoRequestWithOptions` upstream.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct GetAddrInfoWithOptionsOutput {
    pub data_size: u32,
    pub gai_error: i32,
    pub netdb_error: i32,
    pub bsd_errno: u32,
}

/// SFDNSRES service.
///
/// Corresponds to `SFDNSRES` in upstream sfdnsres.h / sfdnsres.cpp.
pub struct Sfdnsres {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl Sfdnsres {
    pub fn new() -> Self {
        let handlers = build_handler_map(&[
            (0, None, "SetDnsAddressesPrivateRequest"),
            (1, None, "GetDnsAddressPrivateRequest"),
            (
                2,
                Some(Sfdnsres::get_host_by_name_request_handler),
                "GetHostByNameRequest",
            ),
            (3, None, "GetHostByAddrRequest"),
            (4, None, "GetHostStringErrorRequest"),
            (
                5,
                Some(Sfdnsres::get_gai_string_error_handler),
                "GetGaiStringErrorRequest",
            ),
            (
                6,
                Some(Sfdnsres::get_addr_info_request_handler),
                "GetAddrInfoRequest",
            ),
            (7, None, "GetNameInfoRequest"),
            (8, None, "RequestCancelHandleRequest"),
            (9, None, "CancelRequest"),
            (
                10,
                Some(Sfdnsres::get_host_by_name_request_with_options_handler),
                "GetHostByNameRequestWithOptions",
            ),
            (11, None, "GetHostByAddrRequestWithOptions"),
            (
                12,
                Some(Sfdnsres::get_addr_info_request_with_options_handler),
                "GetAddrInfoRequestWithOptions",
            ),
            (13, None, "GetNameInfoRequestWithOptions"),
            (
                14,
                Some(Sfdnsres::resolver_set_option_request_handler),
                "ResolverSetOptionRequest",
            ),
            (15, None, "ResolverGetOptionRequest"),
        ]);

        Self {
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// Append a value as raw bytes to a buffer.
    fn append_raw<T: Copy>(buf: &mut Vec<u8>, value: T) {
        let ptr = &value as *const T as *const u8;
        let bytes = unsafe { std::slice::from_raw_parts(ptr, std::mem::size_of::<T>()) };
        buf.extend_from_slice(bytes);
    }

    /// Append a NUL-terminated string to a buffer.
    fn append_nul_terminated(buf: &mut Vec<u8>, s: &str) {
        buf.extend_from_slice(s.as_bytes());
        buf.push(0);
    }

    /// Serialize addrinfo results as a hostent-style buffer.
    ///
    /// Corresponds to `SerializeAddrInfoAsHostEnt` in upstream sfdnsres.cpp.
    fn serialize_addr_info_as_host_ent(addrs: &[[u8; 4]], host: &str) -> Vec<u8> {
        let mut data = Vec::new();

        // h_name: input hostname (NUL-terminated).
        Self::append_nul_terminated(&mut data, host);

        // h_aliases: empty.
        Self::append_raw(&mut data, 0u32.to_be()); // count of aliases = 0

        // h_addrtype
        Self::append_raw(&mut data, (Domain::INET.0 as u16).to_be());
        // h_length
        Self::append_raw(&mut data, 4u16.to_be()); // sizeof(IPv4Address)

        // h_addr_list count.
        Self::append_raw(&mut data, (addrs.len() as u32).to_be());

        for addr in addrs {
            // On the Switch, this is passed through htonl despite already being
            // big-endian, so it ends up as little-endian.
            let ip_int = u32::from_be_bytes(*addr);
            Self::append_raw(&mut data, ip_int.to_le());

            log::info!(
                "Resolved host '{}' to IPv4 address {}.{}.{}.{}",
                host,
                addr[0],
                addr[1],
                addr[2],
                addr[3]
            );
        }

        data
    }

    /// Common implementation for GetHostByNameRequest / GetHostByNameRequestWithOptions.
    ///
    /// Corresponds to `GetHostByNameRequestImpl` in upstream sfdnsres.cpp.
    fn get_host_by_name_request_impl(ctx: &mut HLERequestContext) -> (u32, GetAddrInfoError) {
        let mut rp = RequestParser::new(ctx);
        let parameters: DnsInputParameters = rp.pop_raw();

        log::warn!(
            "called with ignored parameters: cancel_handle={}, process_id={}; use_nsd_resolve={}",
            parameters.cancel_handle,
            parameters.process_id,
            parameters.use_nsd_resolve
        );

        let host_buffer = ctx.read_buffer(0);
        let host = common::string_util::string_from_buffer(&host_buffer);
        // For now, ignore options, which are in input buffer 1 for GetHostByNameRequestWithOptions.

        if should_block_nsd_resolution(parameters.use_nsd_resolve) {
            log::warn!(
                "Blocking unresolved NSD hostname request {:?}, returning EAI_AGAIN",
                host
            );
            return (0, GetAddrInfoError::AGAIN);
        }

        // Prevent direct resolution of Nintendo servers.
        if is_blocked_host(&host) {
            log::warn!(
                "Resolution of hostname {:?} requested, returning EAI_AGAIN",
                host
            );
            return (0, GetAddrInfoError::AGAIN);
        }

        // Attempt DNS resolution via std::net.
        use std::net::ToSocketAddrs;
        let lookup = format!("{}:0", host);
        match lookup.to_socket_addrs() {
            Ok(addrs) => {
                let ipv4_addrs: Vec<[u8; 4]> = addrs
                    .filter_map(|a| match a {
                        std::net::SocketAddr::V4(v4) => Some(v4.ip().octets()),
                        _ => None,
                    })
                    .collect();

                if ipv4_addrs.is_empty() {
                    return (0, GetAddrInfoError::NODATA);
                }

                let data = Self::serialize_addr_info_as_host_ent(&ipv4_addrs, &host);
                let data_size = data.len() as u32;
                ctx.write_buffer(&data, 0);
                (data_size, GetAddrInfoError::SUCCESS)
            }
            Err(e) => {
                log::warn!("DNS resolution failed for '{}': {}", host, e);
                (0, GetAddrInfoError::NONAME)
            }
        }
    }

    /// Common implementation for GetAddrInfoRequest / GetAddrInfoRequestWithOptions.
    ///
    /// Corresponds to `GetAddrInfoRequestImpl` in upstream sfdnsres.cpp.
    fn get_addr_info_request_impl(ctx: &mut HLERequestContext) -> (u32, GetAddrInfoError) {
        let mut rp = RequestParser::new(ctx);
        let parameters: DnsInputParameters = rp.pop_raw();

        log::warn!(
            "called with ignored parameters: cancel_handle={}, process_id={}; use_nsd_resolve={}",
            parameters.cancel_handle,
            parameters.process_id,
            parameters.use_nsd_resolve
        );

        // Upstream passes NSD service identifiers through NSD::Resolve before DNS lookup.
        // The Rust port does not yet expand those identifiers; the safety path below handles
        // them without exposing an official hostname to the host resolver.

        let host_buffer = ctx.read_buffer(0);
        let host = common::string_util::string_from_buffer(&host_buffer);

        let service: Option<String> = if ctx.can_read_buffer(1) {
            let service_buffer = ctx.read_buffer(1);
            Some(common::string_util::string_from_buffer(&service_buffer))
        } else {
            None
        };

        if should_block_nsd_resolution(parameters.use_nsd_resolve) {
            log::warn!(
                "Blocking unresolved NSD addrinfo request {:?}, returning EAI_AGAIN",
                host
            );
            return (0, GetAddrInfoError::AGAIN);
        }

        // Prevent direct resolution of Nintendo servers.
        if is_blocked_host(&host) {
            log::warn!(
                "Resolution of hostname {:?} requested, returning EAI_AGAIN",
                host
            );
            return (0, GetAddrInfoError::AGAIN);
        }

        // Serialized hints are also passed in a buffer, but are ignored for now.

        // Attempt DNS resolution.
        use std::net::ToSocketAddrs;
        let lookup = if let Some(ref svc) = service {
            format!("{}:{}", host, svc)
        } else {
            format!("{}:0", host)
        };
        match lookup.to_socket_addrs() {
            Ok(addrs) => {
                let ipv4_addrs: Vec<std::net::SocketAddrV4> = addrs
                    .filter_map(|a| match a {
                        std::net::SocketAddr::V4(v4) => Some(v4),
                        _ => None,
                    })
                    .collect();

                if ipv4_addrs.is_empty() {
                    return (0, GetAddrInfoError::NODATA);
                }

                let data = Self::serialize_addr_info(&ipv4_addrs, &host);
                let data_size = data.len() as u32;
                ctx.write_buffer(&data, 0);
                (data_size, GetAddrInfoError::SUCCESS)
            }
            Err(e) => {
                log::warn!("DNS resolution failed for '{}': {}", host, e);
                (0, GetAddrInfoError::NONAME)
            }
        }
    }

    /// Serialize addrinfo results in the Switch addrinfo format.
    ///
    /// Corresponds to `SerializeAddrInfo` in upstream sfdnsres.cpp.
    fn serialize_addr_info(addrs: &[std::net::SocketAddrV4], host: &str) -> Vec<u8> {
        let mut data = Vec::new();

        for addr in addrs {
            let ip = addr.ip().octets();
            let port = addr.port();

            // Magic.
            Self::append_raw(&mut data, 0xBEEFCAFEu32.to_be());
            // ai_flags = 0.
            Self::append_raw(&mut data, 0u32.to_be());
            // ai_family = INET.
            Self::append_raw(&mut data, (Domain::INET.0 as u32).to_be());
            // ai_socktype = STREAM (default).
            Self::append_raw(&mut data, 1u32.to_be());
            // ai_protocol = TCP (default).
            Self::append_raw(&mut data, 6u32.to_be());
            // ai_addrlen = sizeof(SockAddrIn).
            Self::append_raw(
                &mut data,
                (std::mem::size_of::<SockAddrIn>() as u32).to_be(),
            );

            // ai_addr: SockAddrIn fields.
            // sin_family.
            Self::append_raw(&mut data, (Domain::INET.0 as u16).to_be());
            // sin_port (LE due to Switch double-swap).
            Self::append_raw(&mut data, port.to_le());
            // sin_addr (LE due to Switch double-swap).
            let ip_int = u32::from_be_bytes(ip);
            Self::append_raw(&mut data, ip_int.to_le());
            // sin_zero (8 bytes).
            data.extend_from_slice(&[0u8; 8]);

            // Canon name (empty NUL-terminated string).
            data.push(0);

            log::info!(
                "Resolved host '{}' to IPv4 address {}.{}.{}.{}",
                host,
                ip[0],
                ip[1],
                ip[2],
                ip[3]
            );
        }

        // 4-byte sentinel value.
        data.extend_from_slice(&[0u8; 4]);

        data
    }

    /// GetGaiStringErrorRequest (cmd 5).
    ///
    /// Corresponds to `SFDNSRES::GetGaiStringErrorRequest` in upstream.
    pub fn get_gai_string_error(gai_errno: i32) -> &'static str {
        let error = match gai_errno {
            0 => GetAddrInfoError::SUCCESS,
            1 => GetAddrInfoError::ADDRFAMILY,
            2 => GetAddrInfoError::AGAIN,
            3 => GetAddrInfoError::BADFLAGS,
            4 => GetAddrInfoError::FAIL,
            5 => GetAddrInfoError::FAMILY,
            6 => GetAddrInfoError::MEMORY,
            7 => GetAddrInfoError::NODATA,
            8 => GetAddrInfoError::NONAME,
            9 => GetAddrInfoError::SERVICE,
            10 => GetAddrInfoError::SOCKTYPE,
            11 => GetAddrInfoError::SYSTEM,
            12 => GetAddrInfoError::BADHINTS,
            13 => GetAddrInfoError::PROTOCOL,
            14 => GetAddrInfoError::OVERFLOW,
            _ => GetAddrInfoError::OTHER,
        };
        get_addr_info_error_string(error)
    }

    /// ResolverSetOptionRequest (cmd 14).
    ///
    /// Corresponds to `SFDNSRES::ResolverSetOptionRequest` in upstream.
    pub fn resolver_set_option_request(&self) -> i32 {
        log::warn!("ResolverSetOptionRequest (STUBBED) called");
        0 // bsd errno = success
    }

    // -----------------------------------------------------------------------
    // IPC handler bridge functions
    // -----------------------------------------------------------------------

    /// GetHostByNameRequest (cmd 2).
    ///
    /// Corresponds to `SFDNSRES::GetHostByNameRequest` in upstream sfdnsres.cpp.
    fn get_host_by_name_request_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let (data_size, emu_gai_err) = Sfdnsres::get_host_by_name_request_impl(ctx);

        let output = GetHostByNameOutput {
            netdb_error: get_addr_info_error_to_netdb_error(emu_gai_err) as i32,
            bsd_errno: get_addr_info_error_to_errno(emu_gai_err) as u32,
            data_size,
        };

        let mut rb = ResponseBuilder::new(ctx, 5, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_raw(&output);
    }

    /// GetHostByNameRequestWithOptions (cmd 10).
    ///
    /// Corresponds to `SFDNSRES::GetHostByNameRequestWithOptions` in upstream sfdnsres.cpp.
    fn get_host_by_name_request_with_options_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let (data_size, emu_gai_err) = Sfdnsres::get_host_by_name_request_impl(ctx);

        let output = GetHostByNameWithOptionsOutput {
            data_size,
            netdb_error: get_addr_info_error_to_netdb_error(emu_gai_err) as i32,
            bsd_errno: get_addr_info_error_to_errno(emu_gai_err) as u32,
        };

        let mut rb = ResponseBuilder::new(ctx, 5, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_raw(&output);
    }

    /// GetAddrInfoRequest (cmd 6).
    ///
    /// Corresponds to `SFDNSRES::GetAddrInfoRequest` in upstream sfdnsres.cpp.
    fn get_addr_info_request_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let (data_size, emu_gai_err) = Sfdnsres::get_addr_info_request_impl(ctx);

        let output = GetAddrInfoOutput {
            bsd_errno: get_addr_info_error_to_errno(emu_gai_err) as u32,
            gai_error: emu_gai_err as i32,
            data_size,
        };

        let mut rb = ResponseBuilder::new(ctx, 5, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_raw(&output);
    }

    /// GetAddrInfoRequestWithOptions (cmd 12).
    ///
    /// Corresponds to `SFDNSRES::GetAddrInfoRequestWithOptions` in upstream sfdnsres.cpp.
    fn get_addr_info_request_with_options_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        // Additional options are ignored.
        let (data_size, emu_gai_err) = Sfdnsres::get_addr_info_request_impl(ctx);

        let output = GetAddrInfoWithOptionsOutput {
            data_size,
            gai_error: emu_gai_err as i32,
            netdb_error: get_addr_info_error_to_netdb_error(emu_gai_err) as i32,
            bsd_errno: get_addr_info_error_to_errno(emu_gai_err) as u32,
        };

        let mut rb = ResponseBuilder::new(ctx, 6, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_raw(&output);
    }

    fn get_gai_string_error_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let gai_errno = rp.pop_i32();

        let result = Sfdnsres::get_gai_string_error(gai_errno);
        ctx.write_buffer(result.as_bytes(), 0);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn resolver_set_option_request_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = unsafe { &*(this as *const dyn ServiceFramework as *const Sfdnsres) };
        let bsd_errno = svc.resolver_set_option_request();

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(bsd_errno);
    }
}

impl SessionRequestHandler for Sfdnsres {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "sfdnsres"
    }
}

impl ServiceFramework for Sfdnsres {
    fn get_service_name(&self) -> &str {
        "sfdnsres"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct DnsPriv {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl DnsPriv {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[(0, None, "Cmd0"), (1, None, "Cmd1"), (2, None, "Cmd2")]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for DnsPriv {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }
    fn service_name(&self) -> &str {
        "dns:priv"
    }
}
impl ServiceFramework for DnsPriv {
    fn get_service_name(&self) -> &str {
        "dns:priv"
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
    fn netdb_mapping_matches_console_verified_upstream_cases() {
        assert_eq!(
            get_addr_info_error_to_netdb_error(GetAddrInfoError::SUCCESS),
            NetDbError::Success
        );
        assert_eq!(
            get_addr_info_error_to_netdb_error(GetAddrInfoError::AGAIN),
            NetDbError::TryAgain
        );
        assert_eq!(
            get_addr_info_error_to_netdb_error(GetAddrInfoError::NODATA),
            NetDbError::HostNotFound
        );
        assert_eq!(
            get_addr_info_error_to_netdb_error(GetAddrInfoError::SERVICE),
            NetDbError::Success
        );
        assert_eq!(
            get_addr_info_error_to_netdb_error(GetAddrInfoError::FAIL),
            NetDbError::HostNotFound
        );
    }

    #[test]
    fn blocked_host_matching_uses_edens_substring_policy() {
        assert!(is_blocked_host("sun.hac.lp1.d4c.nintendo.net"));
        assert!(is_blocked_host("accounts.nintendo.net"));
        assert!(is_blocked_host("subdomain.nintendo.com.example.org"));
        assert!(is_blocked_host("api.epicgames.dev"));
        assert!(!is_blocked_host("libretro.com"));
    }

    #[test]
    fn nsd_resolution_is_blocked_before_host_dns() {
        assert!(should_block_nsd_resolution(1));
        assert!(should_block_nsd_resolution(u8::MAX));
        assert!(!should_block_nsd_resolution(0));
    }
}
