// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/ssl/ssl_backend.h

use crate::hle::result::{ErrorModule, ResultCode};
use crate::internal_network::sockets::SocketBase;
use std::sync::{Arc, Mutex};

pub const RESULT_NO_SOCKET: ResultCode =
    ResultCode::from_module_description(ErrorModule::SSLSrv, 103);
pub const RESULT_INVALID_SOCKET: ResultCode =
    ResultCode::from_module_description(ErrorModule::SSLSrv, 106);
pub const RESULT_TIMEOUT: ResultCode =
    ResultCode::from_module_description(ErrorModule::SSLSrv, 205);
/// ResultWouldBlock is returned from Read and Write, and oddly, DoHandshake,
/// with no way in the latter case to distinguish whether the client should poll
/// for read or write.
pub const RESULT_WOULD_BLOCK: ResultCode =
    ResultCode::from_module_description(ErrorModule::SSLSrv, 204);
pub const RESULT_INTERNAL_ERROR: ResultCode =
    ResultCode::from_module_description(ErrorModule::SSLSrv, 999);

/// SSL connection backend trait.
///
/// In upstream C++, SSLConnectionBackend is an abstract class with virtual methods.
/// Concrete implementations include OpenSSL-based backends.
pub trait SslConnectionBackend: Send {
    fn set_socket(&mut self, socket: Arc<Mutex<Box<dyn SocketBase>>>);
    fn set_host_name(&mut self, hostname: &str) -> ResultCode;
    fn do_handshake(&mut self) -> ResultCode;
    fn read(&mut self, data: &mut [u8]) -> Result<usize, ResultCode>;
    fn write(&mut self, data: &[u8]) -> Result<usize, ResultCode>;
    fn pending(&self) -> i32;
    fn get_server_certs(&self) -> Result<Vec<Vec<u8>>, ResultCode>;
}
