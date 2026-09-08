// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/ssl/ssl.h
//! Port of zuyu/src/core/hle/service/ssl/ssl.cpp

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::core::SystemRef;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};
use crate::hle::service::sockets::bsd::Bsd;
use crate::hle::service::sockets::sockets::Errno as BsdErrno;
use crate::internal_network::network::Errno as NetworkErrno;

use super::cert_store::CertStore;
use super::ssl_backend::{
    SslConnectionBackend, RESULT_INTERNAL_ERROR, RESULT_INVALID_SOCKET, RESULT_NO_SOCKET,
};
use super::ssl_backend_openssl::create_ssl_connection_backend;
use super::ssl_types::CaCertificateId;

/// nn::ssl::sf::CertificateFormat
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertificateFormat {
    Pem = 1,
    Der = 2,
}

/// nn::ssl::sf::ContextOption
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextOption {
    None = 0,
    CrlImportDateCheckEnable = 1,
}

/// nn::ssl::Connection::IoMode
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoMode {
    Blocking = 1,
    NonBlocking = 2,
}

/// nn::ssl::sf::OptionType
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionType {
    DoNotCloseSocket = 0,
    GetServerCertChain = 1,
}

/// nn::ssl::sf::SslVersion
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SslVersion {
    pub raw: u32,
}

impl SslVersion {
    pub fn tls_auto(&self) -> bool {
        (self.raw & 1) != 0
    }

    pub fn tls_v10(&self) -> bool {
        (self.raw >> 3) & 1 != 0
    }

    pub fn tls_v11(&self) -> bool {
        (self.raw >> 4) & 1 != 0
    }

    pub fn tls_v12(&self) -> bool {
        (self.raw >> 5) & 1 != 0
    }

    pub fn tls_v13(&self) -> bool {
        (self.raw >> 6) & 1 != 0
    }

    pub fn api_version(&self) -> u32 {
        (self.raw >> 24) & 0x7F
    }
}

pub struct SslContextSharedData {
    pub connection_count: u32,
}

impl Default for SslContextSharedData {
    fn default() -> Self {
        Self {
            connection_count: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CreateContextParameters {
    ssl_version: SslVersion,
    _padding: u32,
    pid_placeholder: u64,
}

const _: () = assert!(std::mem::size_of::<CreateContextParameters>() == 0x10);

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ContextOptionParameters {
    option: u32,
    value: i32,
}

const _: () = assert!(std::mem::size_of::<ContextOptionParameters>() == 0x8);

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ConnectionOptionParameters {
    option: u32,
    value: i32,
}

const _: () = assert!(std::mem::size_of::<ConnectionOptionParameters>() == 0x8);

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct HandshakeCertOutputParameters {
    certs_size: u32,
    certs_count: u32,
}

const _: () = assert!(std::mem::size_of::<HandshakeCertOutputParameters>() == 0x8);

struct SslConnectionState {
    backend: Box<dyn SslConnectionBackend>,
    fd_to_close: Option<i32>,
    do_not_close_socket: bool,
    get_server_cert_chain: bool,
    socket_fd: Option<i32>,
    did_handshake: bool,
}

pub struct ISslConnection {
    system: SystemRef,
    ssl_version: SslVersion,
    shared_data: Arc<Mutex<SslContextSharedData>>,
    state: Mutex<SslConnectionState>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ISslConnection {
    fn new(
        system: SystemRef,
        ssl_version: SslVersion,
        shared_data: Arc<Mutex<SslContextSharedData>>,
        backend: Box<dyn SslConnectionBackend>,
    ) -> Self {
        shared_data.lock().unwrap().connection_count += 1;

        Self {
            system,
            ssl_version,
            shared_data,
            state: Mutex::new(SslConnectionState {
                backend,
                fd_to_close: None,
                do_not_close_socket: false,
                get_server_cert_chain: false,
                socket_fd: None,
                did_handshake: false,
            }),
            handlers: build_handler_map(&[
                (
                    0,
                    Some(ISslConnection::set_socket_descriptor_handler),
                    "SetSocketDescriptor",
                ),
                (
                    1,
                    Some(ISslConnection::set_host_name_handler),
                    "SetHostName",
                ),
                (
                    2,
                    Some(ISslConnection::set_verify_option_handler),
                    "SetVerifyOption",
                ),
                (3, Some(ISslConnection::set_io_mode_handler), "SetIoMode"),
                (4, None, "GetSocketDescriptor"),
                (5, None, "GetHostName"),
                (6, None, "GetVerifyOption"),
                (7, None, "GetIoMode"),
                (8, Some(ISslConnection::do_handshake_handler), "DoHandshake"),
                (
                    9,
                    Some(ISslConnection::do_handshake_get_server_cert_handler),
                    "DoHandshakeGetServerCert",
                ),
                (10, Some(ISslConnection::read_handler), "Read"),
                (11, Some(ISslConnection::write_handler), "Write"),
                (12, Some(ISslConnection::pending_handler), "Pending"),
                (13, None, "Peek"),
                (14, None, "Poll"),
                (15, None, "GetVerifyCertError"),
                (16, None, "GetNeededServerCertBufferSize"),
                (
                    17,
                    Some(ISslConnection::set_session_cache_mode_handler),
                    "SetSessionCacheMode",
                ),
                (18, None, "GetSessionCacheMode"),
                (19, None, "FlushSessionCache"),
                (20, None, "SetRenegotiationMode"),
                (21, None, "GetRenegotiationMode"),
                (22, Some(ISslConnection::set_option_handler), "SetOption"),
                (23, None, "GetOption"),
                (24, None, "GetVerifyCertErrors"),
                (25, None, "GetCipherInfo"),
                (26, None, "SetNextAlpnProto"),
                (27, None, "GetNextAlpnProto"),
                (28, None, "SetDtlsSocketDescriptor"),
                (29, None, "GetDtlsHandshakeTimeout"),
                (30, None, "SetPrivateOption"),
                (31, None, "SetSrtpCiphers"),
                (32, None, "GetSrtpCipher"),
                (33, None, "ExportKeyingMaterial"),
                (34, None, "SetIoTimeout"),
                (35, None, "GetIoTimeout"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn with_bsd<R>(&self, f: impl FnOnce(&mut Bsd) -> R) -> Option<R> {
        if self.system.is_null() {
            return None;
        }
        let service_manager = self.system.get().service_manager()?;
        let handler = service_manager.lock().unwrap().get_service("bsd:u")?;
        let bsd = handler.as_any().downcast_ref::<Mutex<Bsd>>()?;
        let mut bsd = bsd.lock().unwrap();
        Some(f(&mut bsd))
    }

    fn set_socket_descriptor_impl(&self, mut fd: i32) -> Result<i32, ResultCode> {
        log::debug!("ISslConnection::SetSocketDescriptor called, fd={}", fd);

        let mut state = self.state.lock().unwrap();
        assert!(!state.did_handshake);

        let Some(result) = self.with_bsd(|bsd| {
            let out_fd = if state.do_not_close_socket {
                match bsd.duplicate_socket_impl(fd) {
                    Ok(duplicated_fd) => {
                        fd = duplicated_fd;
                        state.fd_to_close = Some(fd);
                        fd
                    }
                    Err(_) => {
                        log::error!("Failed to duplicate socket with fd {}", fd);
                        return Err(RESULT_INVALID_SOCKET);
                    }
                }
            } else {
                -1
            };

            let Some(host_fd) = bsd.get_socket(fd).map(|socket| socket.get_fd()) else {
                log::error!("Invalid socket fd {}", fd);
                return Err(RESULT_INVALID_SOCKET);
            };

            state.socket_fd = Some(fd);
            state.backend.set_socket(host_fd);
            Ok(out_fd)
        }) else {
            log::error!("Unable to resolve shared bsd:u service");
            return Err(RESULT_INTERNAL_ERROR);
        };

        result
    }

    fn set_host_name_impl(&self, hostname: &str) -> ResultCode {
        log::debug!("ISslConnection::SetHostName called, hostname={}", hostname);
        let mut state = self.state.lock().unwrap();
        assert!(!state.did_handshake);
        state.backend.set_host_name(hostname)
    }

    fn set_verify_option_impl(&self, option: u32) -> ResultCode {
        let state = self.state.lock().unwrap();
        assert!(!state.did_handshake);
        drop(state);
        log::warn!(
            "ISslConnection::SetVerifyOption (STUBBED) called, option={}",
            option
        );
        RESULT_SUCCESS
    }

    fn set_io_mode_impl(&self, input_mode: u32) -> ResultCode {
        assert!(input_mode == IoMode::Blocking as u32 || input_mode == IoMode::NonBlocking as u32);

        let socket_fd = self.state.lock().unwrap().socket_fd;
        let Some(socket_fd) = socket_fd else {
            return RESULT_NO_SOCKET;
        };
        let non_block = input_mode == IoMode::NonBlocking as u32;
        let Some(error) = self.with_bsd(|bsd| {
            bsd.get_socket_mut(socket_fd)
                .map(|socket| socket.set_non_block(non_block))
        }) else {
            return RESULT_NO_SOCKET;
        };
        let Some(error) = error else {
            return RESULT_NO_SOCKET;
        };
        if error != NetworkErrno::Success {
            log::error!(
                "Failed to set native socket non-block flag to {}: {:?}",
                non_block,
                error
            );
        }
        RESULT_SUCCESS
    }

    fn set_session_cache_mode_impl(&self, mode: u32) -> ResultCode {
        let state = self.state.lock().unwrap();
        assert!(!state.did_handshake);
        drop(state);
        log::warn!(
            "ISslConnection::SetSessionCacheMode (STUBBED) called, value={}",
            mode
        );
        RESULT_SUCCESS
    }

    fn do_handshake_impl(&self) -> ResultCode {
        log::debug!(
            "ISslConnection::DoHandshake called, api_version={}",
            self.ssl_version.api_version()
        );
        let mut state = self.state.lock().unwrap();
        if state.did_handshake || state.socket_fd.is_none() {
            return RESULT_NO_SOCKET;
        }
        let result = state.backend.do_handshake();
        state.did_handshake = result.is_success();
        result
    }

    fn read_impl(&self, output: &mut Vec<u8>) -> ResultCode {
        let mut state = self.state.lock().unwrap();
        if !state.did_handshake {
            return RESULT_INTERNAL_ERROR;
        }
        match state.backend.read(output) {
            Ok(actual_size) => {
                output.truncate(actual_size);
                RESULT_SUCCESS
            }
            Err(result) => result,
        }
    }

    fn write_impl(&self, data: &[u8]) -> Result<usize, ResultCode> {
        let mut state = self.state.lock().unwrap();
        if !state.did_handshake {
            return Err(RESULT_INTERNAL_ERROR);
        }
        state.backend.write(data)
    }

    fn pending_impl(&self) -> (ResultCode, i32) {
        log::warn!("ISslConnection::Pending (STUBBED) called");
        (RESULT_SUCCESS, 0)
    }

    fn set_socket_descriptor_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISslConnection) };
        let mut rp = RequestParser::new(ctx);
        let fd = rp.pop_i32();
        let (result, out_fd) = match service.set_socket_descriptor_impl(fd) {
            Ok(out_fd) => (RESULT_SUCCESS, out_fd),
            Err(result) => (result, -1),
        };
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_i32(out_fd);
    }

    fn set_host_name_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISslConnection) };
        let hostname = common::string_util::string_from_buffer(&ctx.read_buffer(0));
        let result = service.set_host_name_impl(&hostname);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    fn set_verify_option_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISslConnection) };
        let mut rp = RequestParser::new(ctx);
        let result = service.set_verify_option_impl(rp.pop_u32());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    fn set_io_mode_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISslConnection) };
        let mut rp = RequestParser::new(ctx);
        let result = service.set_io_mode_impl(rp.pop_u32());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    fn do_handshake_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISslConnection) };
        let result = service.do_handshake_impl();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    fn do_handshake_get_server_cert_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISslConnection) };
        let mut result = service.do_handshake_impl();
        let mut output = HandshakeCertOutputParameters::default();

        if result.is_success() {
            let state = service.state.lock().unwrap();
            match state.backend.get_server_certs() {
                Ok(certs) => {
                    let certs_buffer = serialize_server_certs(&certs, state.get_server_cert_chain);
                    ctx.write_buffer(&certs_buffer, 0);
                    output.certs_count = certs.len() as u32;
                    output.certs_size = certs_buffer.len() as u32;
                }
                Err(error) => result = error,
            }
        }

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(result);
        rb.push_raw(&output);
    }

    fn read_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISslConnection) };
        let mut output = vec![0u8; ctx.get_write_buffer_size(0)];
        let result = service.read_impl(&mut output);
        if result.is_success() {
            ctx.write_buffer(&output, 0);
        }
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_u32(if result.is_success() {
            output.len() as u32
        } else {
            0
        });
    }

    fn write_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISslConnection) };
        let data = ctx.read_buffer(0);
        let (result, write_size) = match service.write_impl(&data) {
            Ok(write_size) => (RESULT_SUCCESS, write_size),
            Err(result) => (result, 0),
        };
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_u32(write_size as u32);
    }

    fn pending_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISslConnection) };
        let (result, pending_size) = service.pending_impl();
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_i32(pending_size);
    }

    fn set_session_cache_mode_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISslConnection) };
        let mut rp = RequestParser::new(ctx);
        let result = service.set_session_cache_mode_impl(rp.pop_u32());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    fn set_option_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISslConnection) };
        let mut rp = RequestParser::new(ctx);
        let parameters = rp.pop_raw::<ConnectionOptionParameters>();
        let mut state = service.state.lock().unwrap();
        match parameters.option {
            option if option == OptionType::DoNotCloseSocket as u32 => {
                state.do_not_close_socket = parameters.value != 0;
            }
            option if option == OptionType::GetServerCertChain as u32 => {
                state.get_server_cert_chain = parameters.value != 0;
            }
            option => log::warn!(
                "ISslConnection::SetOption unknown option={}, value={}",
                option,
                parameters.value
            ),
        }
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }
}

impl Drop for ISslConnection {
    fn drop(&mut self) {
        self.shared_data.lock().unwrap().connection_count -= 1;

        let (fd_to_close, do_not_close_socket) = {
            let state = self.state.lock().unwrap();
            (state.fd_to_close, state.do_not_close_socket)
        };
        if let Some(fd) = fd_to_close {
            if !do_not_close_socket {
                log::error!("do_not_close_socket was changed after setting socket; is this right?");
            } else if let Some(error) = self.with_bsd(|bsd| bsd.close_impl(fd)) {
                if error != BsdErrno::SUCCESS {
                    log::error!("Failed to close duplicated socket {}: {:?}", fd, error);
                }
            }
        }
    }
}

impl SessionRequestHandler for ISslConnection {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "ISslConnection"
    }
}

impl ServiceFramework for ISslConnection {
    fn get_service_name(&self) -> &str {
        "ISslConnection"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct ISslContext {
    system: SystemRef,
    ssl_version: SslVersion,
    shared_data: Arc<Mutex<SslContextSharedData>>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ISslContext {
    pub fn new(system: SystemRef, ssl_version: SslVersion) -> Self {
        Self {
            system,
            ssl_version,
            shared_data: Arc::new(Mutex::new(SslContextSharedData::default())),
            handlers: build_handler_map(&[
                (0, Some(ISslContext::set_option_handler), "SetOption"),
                (1, None, "GetOption"),
                (
                    2,
                    Some(ISslContext::create_connection_handler),
                    "CreateConnection",
                ),
                (
                    3,
                    Some(ISslContext::get_connection_count_handler),
                    "GetConnectionCount",
                ),
                (
                    4,
                    Some(ISslContext::import_server_pki_handler),
                    "ImportServerPki",
                ),
                (
                    5,
                    Some(ISslContext::import_client_pki_handler),
                    "ImportClientPki",
                ),
                (6, None, "RemoveServerPki"),
                (7, None, "RemoveClientPki"),
                (8, None, "RegisterInternalPki"),
                (9, None, "AddPolicyOid"),
                (10, None, "ImportCrl"),
                (11, None, "RemoveCrl"),
                (12, None, "ImportClientCertKeyPki"),
                (13, None, "GeneratePrivateKeyAndCert"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn set_option(&self, option: u32, value: i32) {
        log::warn!(
            "ISslContext::SetOption (STUBBED) called, option={}, value={}",
            option,
            value
        );
    }

    fn connection_count(&self) -> u32 {
        self.shared_data.lock().unwrap().connection_count
    }

    fn create_connection(&self) -> Result<ISslConnection, ResultCode> {
        log::warn!("ISslContext::CreateConnection called");
        let backend = create_ssl_connection_backend()?;
        Ok(ISslConnection::new(
            self.system,
            self.ssl_version,
            Arc::clone(&self.shared_data),
            backend,
        ))
    }

    fn set_option_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISslContext) };
        let mut rp = RequestParser::new(ctx);
        let params = rp.pop_raw::<ContextOptionParameters>();
        service.set_option(params.option, params.value);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn create_connection_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISslContext) };
        let connection = service.create_connection();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        match connection {
            Ok(connection) => {
                rb.push_result(RESULT_SUCCESS);
                rb.push_ipc_interface(Arc::new(connection));
            }
            Err(result) => rb.push_result(result),
        }
    }

    fn get_connection_count_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISslContext) };
        log::debug!(
            "ISslContext::GetConnectionCount connection_count={}",
            service.connection_count()
        );
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(service.connection_count());
    }

    fn import_server_pki_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let _service = unsafe { &*(this as *const dyn ServiceFramework as *const ISslContext) };
        let mut rp = RequestParser::new(ctx);
        let certificate_format = rp.pop_u32();
        let _pkcs_12_certificates = ctx.read_buffer(0);
        log::warn!(
            "ISslContext::ImportServerPki (STUBBED) called, certificate_format={}",
            certificate_format
        );

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(0);
    }

    fn import_client_pki_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let _service = unsafe { &*(this as *const dyn ServiceFramework as *const ISslContext) };
        let _pkcs_12_certificate = ctx.read_buffer(0);
        let _ascii_password = if ctx.can_read_buffer(1) {
            ctx.read_buffer(1)
        } else {
            Vec::new()
        };
        log::warn!("ISslContext::ImportClientPki (STUBBED) called");

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(0);
    }
}

impl SessionRequestHandler for ISslContext {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "ISslContext"
    }
}

impl ServiceFramework for ISslContext {
    fn get_service_name(&self) -> &str {
        "ISslContext"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct ISslService {
    system: SystemRef,
    cert_store: CertStore,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ISslService {
    pub fn new(system: SystemRef) -> Self {
        Self {
            system,
            cert_store: CertStore::new(system),
            handlers: build_handler_map(&[
                (
                    0,
                    Some(ISslService::create_context_handler),
                    "CreateContext",
                ),
                (1, None, "GetContextCount"),
                (
                    2,
                    Some(ISslService::get_certificates_handler),
                    "GetCertificates",
                ),
                (
                    3,
                    Some(ISslService::get_certificate_buf_size_handler),
                    "GetCertificateBufSize",
                ),
                (4, None, "DebugIoctl"),
                (
                    5,
                    Some(ISslService::set_interface_version_handler),
                    "SetInterfaceVersion",
                ),
                (6, None, "FlushSessionCache"),
                (7, None, "SetDebugOption"),
                (8, None, "GetDebugOption"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn create_context(&self, ssl_version: SslVersion, pid_placeholder: u64) -> ISslContext {
        log::warn!(
            "ISslService::CreateContext (STUBBED) called, api_version={}, pid_placeholder={}",
            ssl_version.api_version(),
            pid_placeholder
        );
        ISslContext::new(self.system, ssl_version)
    }

    fn set_interface_version(&self, ssl_version: u32) {
        log::debug!(
            "ISslService::SetInterfaceVersion called, ssl_version={}",
            ssl_version
        );
    }

    fn create_context_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISslService) };
        let mut rp = RequestParser::new(ctx);
        let params = rp.pop_raw::<CreateContextParameters>();
        let context = service.create_context(params.ssl_version, params.pid_placeholder);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(Arc::new(context));
    }

    fn set_interface_version_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISslService) };
        let mut rp = RequestParser::new(ctx);
        service.set_interface_version(rp.pop_u32());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_certificate_buf_size_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISslService) };
        log::info!("ISslService::GetCertificateBufSize called");
        let certificate_ids = parse_certificate_ids(&ctx.read_buffer(0));
        let (result, size) = match service
            .cert_store
            .get_certificate_buf_size(&certificate_ids)
        {
            Ok((size, _num_entries)) => (RESULT_SUCCESS, size),
            Err(result) => (result, 0),
        };
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_u32(size);
    }

    fn get_certificates_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const ISslService) };
        log::info!("ISslService::GetCertificates called");
        let certificate_ids = parse_certificate_ids(&ctx.read_buffer(0));
        let mut output = vec![0u8; ctx.get_write_buffer_size(0)];
        let (result, num_entries) = match service
            .cert_store
            .get_certificates(&mut output, &certificate_ids)
        {
            Ok(num_entries) => {
                ctx.write_buffer(&output, 0);
                (RESULT_SUCCESS, num_entries)
            }
            Err(result) => (result, 0),
        };
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_u32(num_entries);
    }
}

fn parse_certificate_ids(bytes: &[u8]) -> Vec<CaCertificateId> {
    bytes
        .chunks_exact(std::mem::size_of::<i32>())
        .map(|chunk| CaCertificateId::from_raw(i32::from_le_bytes(chunk.try_into().unwrap())))
        .collect()
}

impl SessionRequestHandler for ISslService {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "ssl"
    }
}

impl ServiceFramework for ISslService {
    fn get_service_name(&self) -> &str {
        "ssl"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// Port of ISslServiceForSystem in upstream ssl.cpp.
/// Upstream currently returns only ResultSuccess for every command, including
/// CreateContextForSystem. This interface must not alias the ordinary TLS service.
pub struct ISslServiceForSystem {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ISslServiceForSystem {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, Some(Self::create_context), "CreateContext"),
                (1, Some(Self::get_context_count), "GetContextCount"),
                (2, Some(Self::get_certificates), "GetCertificates"),
                (3, Some(Self::get_certificate_buf_size), "GetCertificateBufSize"),
                (4, Some(Self::debug_ioctl), "DebugIoctl"),
                (5, Some(Self::set_interface_version), "SetInterfaceVersion"),
                (6, Some(Self::flush_session_cache), "FlushSessionCache"),
                (7, Some(Self::set_debug_option), "SetDebugOption"),
                (8, Some(Self::get_debug_option), "GetDebugOption"),
                (9, Some(Self::clear_tls12_fallback_flag), "ClearTls12FallbackFlag"),
                (100, Some(Self::create_context_for_system), "CreateContextForSystem"),
                (101, Some(Self::set_thread_core_mask), "SetThreadCoreMask"),
                (102, Some(Self::get_thread_core_mask), "GetThreadCoreMask"),
                (103, Some(Self::verify_signature), "VerifySignature"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn create_context(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("ISslServiceForSystem::CreateContext (STUBBED) called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_context_count(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("ISslServiceForSystem::GetContextCount (STUBBED) called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_certificates(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("ISslServiceForSystem::GetCertificates (STUBBED) called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_certificate_buf_size(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("ISslServiceForSystem::GetCertificateBufSize (STUBBED) called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn debug_ioctl(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("ISslServiceForSystem::DebugIoctl (STUBBED) called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn set_interface_version(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("ISslServiceForSystem::SetInterfaceVersion (STUBBED) called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn flush_session_cache(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("ISslServiceForSystem::FlushSessionCache (STUBBED) called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn set_debug_option(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("ISslServiceForSystem::SetDebugOption (STUBBED) called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_debug_option(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("ISslServiceForSystem::GetDebugOption (STUBBED) called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn clear_tls12_fallback_flag(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("ISslServiceForSystem::ClearTls12FallbackFlag (STUBBED) called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn create_context_for_system(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("ISslServiceForSystem::CreateContextForSystem (STUBBED) called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn set_thread_core_mask(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("ISslServiceForSystem::SetThreadCoreMask (STUBBED) called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_thread_core_mask(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("ISslServiceForSystem::GetThreadCoreMask (STUBBED) called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn verify_signature(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("ISslServiceForSystem::VerifySignature (STUBBED) called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }
}

impl SessionRequestHandler for ISslServiceForSystem {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "ssl:s"
    }
}

impl ServiceFramework for ISslServiceForSystem {
    fn get_service_name(&self) -> &str {
        "ssl:s"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// Registers the "ssl" and "ssl:s" services.
///
/// Corresponds to `Service::SSL::LoopProcess` in upstream `ssl.cpp`.
pub fn loop_process(system: crate::core::SystemRef) {
    use crate::hle::service::server_manager::ServerManager;

    let server_manager = ServerManager::new_shared(system);
    {
        let mut server_manager = server_manager.lock().unwrap();
        server_manager.register_named_service_handler(
            "ssl",
            Arc::new(ISslService::new(system)),
            64,
        );
        server_manager.register_named_service_handler(
            "ssl:s",
            Arc::new(ISslServiceForSystem::new()),
            64,
        );
    }
    ServerManager::run_server_shared(server_manager);
}

/// Serialize server certificate chain for DoHandshakeGetServerCert.
///
/// If get_server_cert_chain is false, returns just the first cert.
/// Otherwise returns a structured buffer with magic header.
pub fn serialize_server_certs(certs: &[Vec<u8>], get_server_cert_chain: bool) -> Vec<u8> {
    if !get_server_cert_chain {
        return match certs.first() {
            Some(cert) => cert.clone(),
            None => {
                log::error!("Should be at least one server cert");
                Vec::new()
            }
        };
    }

    let mut ret = Vec::new();

    // Header: magic (8 bytes) + count (4 bytes) + pad (4 bytes)
    let magic: u64 = 0x4E4D684374726543;
    ret.extend_from_slice(&magic.to_le_bytes());
    ret.extend_from_slice(&(certs.len() as u32).to_le_bytes());
    ret.extend_from_slice(&0u32.to_le_bytes());

    // Entry headers: size (4 bytes) + offset (4 bytes) each
    let header_size = 16 + certs.len() * 8;
    let mut data_offset = header_size;
    for cert in certs {
        ret.extend_from_slice(&(cert.len() as u32).to_le_bytes());
        ret.extend_from_slice(&(data_offset as u32).to_le_bytes());
        data_offset += cert.len();
    }

    // Certificate data
    for cert in certs {
        ret.extend_from_slice(cert);
    }

    ret
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestSslBackend;

    impl SslConnectionBackend for TestSslBackend {
        fn set_socket(&mut self, _socket_fd: i32) {}

        fn set_host_name(&mut self, _hostname: &str) -> ResultCode {
            RESULT_SUCCESS
        }

        fn do_handshake(&mut self) -> ResultCode {
            RESULT_SUCCESS
        }

        fn read(&mut self, _data: &mut [u8]) -> Result<usize, ResultCode> {
            Ok(0)
        }

        fn write(&mut self, data: &[u8]) -> Result<usize, ResultCode> {
            Ok(data.len())
        }

        fn get_server_certs(&self) -> Result<Vec<Vec<u8>>, ResultCode> {
            Ok(vec![vec![1, 2, 3]])
        }
    }

    #[test]
    fn ssl_service_handler_table_matches_upstream_slice() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let system = Box::new(crate::core::System::new_for_test());
                let service = ISslService::new(SystemRef::from_ref(&system));
                assert_eq!(service.handlers().len(), 9);
                assert!(service.handlers().contains_key(&0));
                assert!(service.handlers().contains_key(&5));
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn system_ssl_commands_match_upstream_result_only_responses() {
        let service = ISslServiceForSystem::new();
        assert_eq!(service.get_service_name(), "ssl:s");
        let expected = [
            (0, "CreateContext"),
            (1, "GetContextCount"),
            (2, "GetCertificates"),
            (3, "GetCertificateBufSize"),
            (4, "DebugIoctl"),
            (5, "SetInterfaceVersion"),
            (6, "FlushSessionCache"),
            (7, "SetDebugOption"),
            (8, "GetDebugOption"),
            (9, "ClearTls12FallbackFlag"),
            (100, "CreateContextForSystem"),
            (101, "SetThreadCoreMask"),
            (102, "GetThreadCoreMask"),
            (103, "VerifySignature"),
        ];
        assert_eq!(service.handlers().len(), expected.len());
        for (id, name) in expected {
            let handler = &service.handlers()[&id];
            assert_eq!(handler.name, name);
            let mut ctx = HLERequestContext::new();
            ctx.command_buffer_mut().fill(0xdead_beef);
            (handler.handler_callback.unwrap())(&service, &mut ctx);
            assert_eq!(ctx.command_buffer()[6], 0, "command {id}");
            assert_eq!(ctx.command_buffer()[7], 0, "command {id}");
            // No handle descriptor: even CreateContextForSystem has no output
            // interface in the upstream implementation.
            assert_eq!(ctx.command_buffer()[1] >> 31, 0, "command {id}");
        }
    }

    #[test]
    fn ssl_context_handler_table_matches_upstream_slice() {
        let context = ISslContext::new(SystemRef::null(), SslVersion::default());
        assert_eq!(context.handlers().len(), 14);
        assert!(context.handlers().contains_key(&0));
        assert!(context.handlers()[&2].handler_callback.is_some());
        assert!(context.handlers().contains_key(&5));
    }

    #[test]
    fn ssl_connection_handler_table_matches_upstream() {
        let shared_data = Arc::new(Mutex::new(SslContextSharedData::default()));
        let connection = ISslConnection::new(
            SystemRef::null(),
            SslVersion::default(),
            shared_data,
            Box::new(TestSslBackend),
        );
        assert_eq!(connection.handlers().len(), 36);
        for command in [0, 1, 2, 3, 8, 9, 10, 11, 12, 17, 22] {
            assert!(
                connection.handlers()[&command].handler_callback.is_some(),
                "command {} must be implemented",
                command
            );
        }
    }

    #[test]
    fn ssl_connection_lifetime_updates_context_count() {
        let shared_data = Arc::new(Mutex::new(SslContextSharedData::default()));
        let connection = ISslConnection::new(
            SystemRef::null(),
            SslVersion::default(),
            Arc::clone(&shared_data),
            Box::new(TestSslBackend),
        );
        assert_eq!(shared_data.lock().unwrap().connection_count, 1);
        drop(connection);
        assert_eq!(shared_data.lock().unwrap().connection_count, 0);
    }

    #[test]
    fn ssl_create_context_payload_layout_matches_upstream() {
        assert_eq!(std::mem::size_of::<CreateContextParameters>(), 0x10);
        assert_eq!(std::mem::size_of::<ContextOptionParameters>(), 0x8);
        assert_eq!(std::mem::size_of::<ConnectionOptionParameters>(), 0x8);
        assert_eq!(std::mem::size_of::<HandshakeCertOutputParameters>(), 0x8);
    }

    #[test]
    fn empty_server_certificate_list_matches_upstream_guard() {
        assert!(serialize_server_certs(&[], false).is_empty());
    }
}
