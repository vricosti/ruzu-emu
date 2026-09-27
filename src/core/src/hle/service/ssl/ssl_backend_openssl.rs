// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/ssl/ssl_backend_openssl.cpp
//!
//! OpenSSL-backed SSL connection backend. Uses the `openssl` crate to provide
//! TLS client connections, matching the upstream OpenSSL backend's behavior.
//!
//! Key differences from upstream:
//! - Uses the Rust `openssl` crate's safe wrappers instead of raw C OpenSSL API.
//! - Uses `SslStream<SocketAdapter>` for BIO callbacks; its Read/Write bridge
//!   forwards to the same shared Network::SocketBase as upstream.
//! - SSLKEYLOGFILE support uses `SslContextBuilder::set_keylog_callback`.

use std::io::{self, Read as IoRead, Write as IoWrite};
use std::sync::{Arc, Mutex, OnceLock};

use openssl::ssl::{
    HandshakeError, MidHandshakeSslStream, SslConnector, SslMethod, SslStream, SslVerifyMode,
};

use crate::hle::result::ResultCode;
use crate::internal_network::sockets::SocketBase;
use crate::internal_network::network::Errno as NetworkErrno;

use super::ssl_backend::{SslConnectionBackend, RESULT_INTERNAL_ERROR, RESULT_WOULD_BLOCK};

// =========================================================================
// One-time initialization
// =========================================================================

static SSL_CONNECTOR: OnceLock<Result<SslConnector, String>> = OnceLock::new();

/// Build the process-wide OpenSSL context.
///
/// Corresponds to upstream `OneTimeInit()`, which creates one shared
/// `SSL_CTX` and configures the host trust store once.
fn create_connector() -> Result<SslConnector, String> {
    let mut builder = SslConnector::builder(SslMethod::tls_client()).map_err(|e| e.to_string())?;
    builder.set_verify(SslVerifyMode::PEER);
    builder
        .set_default_verify_paths()
        .map_err(|e| e.to_string())?;

    if let Ok(logfile) = std::env::var("SSLKEYLOGFILE") {
        builder.set_keylog_callback(move |_ssl, line| {
            use std::io::Write;
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&logfile)
            {
                if writeln!(file, "{}", line).is_err() || file.flush().is_err() {
                    log::error!("Failed to write to SSLKEYLOGFILE");
                }
            } else {
                log::error!("Failed to write to SSLKEYLOGFILE");
            }
            log::debug!("Wrote to SSLKEYLOGFILE: {}", line);
        });
    }

    Ok(builder.build())
}

fn shared_connector() -> Result<SslConnector, ResultCode> {
    match SSL_CONNECTOR.get_or_init(create_connector) {
        Ok(connector) => Ok(connector.clone()),
        Err(error) => {
            log::error!(
                "Can't create SSL connection because OpenSSL one-time initialization failed: {}",
                error
            );
            Err(RESULT_INTERNAL_ERROR)
        }
    }
}

// Rust Read/Write bridge for upstream's BIO ReadCallback/WriteCallback.
// Retains the same shared SocketBase; never duplicates or takes ownership of
// its native descriptor and never bypasses ProxySocket.
struct SocketAdapter {
    socket: Arc<Mutex<Box<dyn SocketBase>>>,
    got_read_eof: bool,
}

impl IoRead for SocketAdapter {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let (actual, error) = self.socket.lock().unwrap().recv(0, buf);
        match error {
            NetworkErrno::Success => {
                if actual == 0 { self.got_read_eof = true; }
                Ok(actual as usize)
            }
            NetworkErrno::Again => Err(io::ErrorKind::WouldBlock.into()),
            _ => Err(io::Error::new(io::ErrorKind::Other, format!("Socket recv returned {error:?}"))),
        }
    }
}

impl IoWrite for SocketAdapter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let (actual, error) = self.socket.lock().unwrap().send(buf, 0);
        match error {
            NetworkErrno::Success => Ok(actual as usize),
            NetworkErrno::Again => Err(io::ErrorKind::WouldBlock.into()),
            _ => Err(io::Error::new(io::ErrorKind::Other, format!("Socket send returned {error:?}"))),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        // Upstream BIO_CTRL_FLUSH has nothing to flush.
        Ok(())
    }
}

// =========================================================================
// SSLConnectionBackendOpenSSL
// =========================================================================

enum TlsStreamState {
    Socket(SocketAdapter),
    Handshaking(MidHandshakeSslStream<SocketAdapter>),
    Connected(SslStream<SocketAdapter>),
}

/// OpenSSL-backed SSL connection.
///
/// Corresponds to `SSLConnectionBackendOpenSSL` in upstream
/// ssl_backend_openssl.cpp.
struct SslConnectionBackendOpenSsl {
    /// Clone of the process-wide connector/`SSL_CTX`.
    connector: SslConnector,
    /// Persistent socket and SSL handshake state.
    ///
    /// Upstream keeps one `SSL*` and calls `SSL_do_handshake` repeatedly after
    /// `SSL_ERROR_WANT_READ`/`SSL_ERROR_WANT_WRITE`. Keeping the
    /// `MidHandshakeSslStream` is the equivalent openssl-crate contract.
    stream_state: Option<TlsStreamState>,
    /// Hostname for SNI and verification.
    hostname: Option<String>,
}

impl SslConnectionBackendOpenSsl {
    fn new() -> Result<Self, ResultCode> {
        Ok(Self {
            connector: shared_connector()?,
            stream_state: None,
            hostname: None,
        })
    }

    // Same decision table as upstream HandleReturn, including WANT_WRITE
    // during a read and protocol errors (which must not be reported as EOF).
    fn handle_return(&self, result: Result<usize, openssl::ssl::Error>) -> Result<usize, ResultCode> {
        use openssl::ssl::ErrorCode;
        match result {
            Ok(actual) => Ok(actual),
            Err(error) => match error.code() {
                ErrorCode::ZERO_RETURN => Ok(0),
                ErrorCode::WANT_READ | ErrorCode::WANT_WRITE => Err(RESULT_WOULD_BLOCK),
                ErrorCode::SYSCALL if matches!(self.stream_state.as_ref(),
                    Some(TlsStreamState::Connected(stream)) if stream.get_ref().got_read_eof) => Ok(0),
                _ => {
                    log::error!("SSL I/O failed: {error}");
                    Err(RESULT_INTERNAL_ERROR)
                }
            },
        }
    }

    fn finish_handshake(
        &mut self,
        result: Result<SslStream<SocketAdapter>, HandshakeError<SocketAdapter>>,
    ) -> ResultCode {
        match result {
            Ok(stream) => {
                log::info!(
                    "SSL handshake succeeded for {}",
                    self.hostname.as_deref().unwrap_or("localhost")
                );
                self.stream_state = Some(TlsStreamState::Connected(stream));
                ResultCode(0)
            }
            Err(HandshakeError::WouldBlock(mid)) => {
                self.stream_state = Some(TlsStreamState::Handshaking(mid));
                log::debug!("SSL handshake would block");
                RESULT_WOULD_BLOCK
            }
            Err(HandshakeError::Failure(mid)) => {
                log::error!("SSL handshake failed: {}", mid.error());
                // Upstream retains the SSL object and socket after errors too.
                self.stream_state = Some(TlsStreamState::Handshaking(mid));
                RESULT_INTERNAL_ERROR
            }
            Err(HandshakeError::SetupFailure(error)) => {
                log::error!("SSL handshake setup failure: {}", error);
                RESULT_INTERNAL_ERROR
            }
        }
    }
}

impl SslConnectionBackend for SslConnectionBackendOpenSsl {
    fn pending(&self) -> i32 {
        // SSL_pending counts decrypted application bytes, not bytes waiting on
        // the underlying socket. Before creating SSL there cannot be any.
        match self.stream_state.as_ref() {
            Some(TlsStreamState::Connected(stream)) => stream.ssl().pending() as i32,
            Some(TlsStreamState::Handshaking(stream)) => stream.ssl().pending() as i32,
            Some(TlsStreamState::Socket(_)) | None => 0,
        }
    }

    /// SetSocket.
    ///
    /// Corresponds to `SSLConnectionBackendOpenSSL::SetSocket` in upstream.
    fn set_socket(&mut self, socket: Arc<Mutex<Box<dyn SocketBase>>>) {
        self.stream_state = Some(TlsStreamState::Socket(SocketAdapter {
            socket,
            got_read_eof: false,
        }));
    }

    /// SetHostName.
    ///
    /// Corresponds to upstream `SSL_set1_host` (for verification) and
    /// `SSL_set_tlsext_host_name` (for SNI). Both are handled by the
    /// `openssl` crate's `SslConnector::connect(hostname, ...)`.
    fn set_host_name(&mut self, hostname: &str) -> ResultCode {
        log::debug!("SSLConnectionBackendOpenSSL::SetHostName: {}", hostname);
        self.hostname = Some(hostname.to_string());
        ResultCode(0) // RESULT_SUCCESS
    }

    /// DoHandshake.
    ///
    /// Corresponds to `SSLConnectionBackendOpenSSL::DoHandshake` in upstream.
    /// Upstream sequence:
    ///   SSL_set_verify_result(ssl, X509_V_OK)
    ///   SSL_do_handshake(ssl)
    ///   SSL_get_verify_result(ssl) — check cert verification
    ///   HandleReturn("SSL_do_handshake", 0, ret)
    fn do_handshake(&mut self) -> ResultCode {
        log::debug!("SSLConnectionBackendOpenSSL::DoHandshake called");

        let stream_state = match self.stream_state.take() {
            Some(state) => state,
            None => {
                log::error!("DoHandshake called without a socket");
                return RESULT_INTERNAL_ERROR;
            }
        };

        let result = match stream_state {
            TlsStreamState::Socket(tcp_adapter) => {
                let hostname = self.hostname.as_deref().unwrap_or("localhost");
                self.connector.connect(hostname, tcp_adapter)
            }
            TlsStreamState::Handshaking(mid) => mid.handshake(),
            TlsStreamState::Connected(stream) => {
                self.stream_state = Some(TlsStreamState::Connected(stream));
                return ResultCode(0);
            }
        };

        self.finish_handshake(result)
    }

    /// Read.
    ///
    /// Corresponds to `SSLConnectionBackendOpenSSL::Read` in upstream.
    /// Upstream: SSL_read_ex(ssl, data, size, &actual) then HandleReturn.
    fn read(&mut self, data: &mut [u8]) -> Result<usize, ResultCode> {
        let Some(TlsStreamState::Connected(stream)) = self.stream_state.as_mut() else {
            return Err(RESULT_INTERNAL_ERROR);
        };

        let result = stream.ssl_read(data);
        self.handle_return(result)
    }

    /// Write.
    ///
    /// Corresponds to `SSLConnectionBackendOpenSSL::Write` in upstream.
    /// Upstream: SSL_write_ex(ssl, data, size, &actual) then HandleReturn.
    fn write(&mut self, data: &[u8]) -> Result<usize, ResultCode> {
        let Some(TlsStreamState::Connected(stream)) = self.stream_state.as_mut() else {
            return Err(RESULT_INTERNAL_ERROR);
        };

        let result = stream.ssl_write(data);
        self.handle_return(result)
    }

    /// GetServerCerts.
    ///
    /// Corresponds to `SSLConnectionBackendOpenSSL::GetServerCerts` in upstream.
    /// Upstream: SSL_get_peer_cert_chain, then i2d_X509 for each cert to get
    /// DER-encoded bytes.
    fn get_server_certs(&self) -> Result<Vec<Vec<u8>>, ResultCode> {
        let Some(TlsStreamState::Connected(stream)) = self.stream_state.as_ref() else {
            return Err(RESULT_INTERNAL_ERROR);
        };

        let ssl = stream.ssl();
        let chain = ssl.peer_cert_chain().ok_or_else(|| {
            log::error!("SSL_get_peer_cert_chain returned None");
            RESULT_INTERNAL_ERROR
        })?;

        let mut out_certs = Vec::new();
        for cert in chain.iter() {
            match cert.to_der() {
                Ok(der) => out_certs.push(der),
                Err(e) => {
                    log::error!("Failed to DER-encode certificate: {}", e);
                    // Upstream: ASSERT_OR_EXECUTE(... , { continue; })
                    continue;
                }
            }
        }

        Ok(out_certs)
    }
}

/// Create an OpenSSL-backed SSL connection backend.
///
/// Corresponds to `CreateSSLConnectionBackend` in upstream ssl_backend_openssl.cpp.
pub fn create_ssl_connection_backend() -> Result<Box<dyn SslConnectionBackend>, ResultCode> {
    let backend = SslConnectionBackendOpenSsl::new()?;
    Ok(Box::new(backend))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;
    use crate::internal_network::sockets::Socket;

    fn shared_tcp_socket(stream: TcpStream) -> Arc<Mutex<Box<dyn SocketBase>>> {
        #[cfg(unix)]
        let handle = { use std::os::fd::IntoRawFd; stream.into_raw_fd() };
        #[cfg(windows)]
        let handle = { use std::os::windows::io::IntoRawSocket; stream.into_raw_socket() as usize };
        Arc::new(Mutex::new(Box::new(Socket::from_fd(handle))))
    }

    #[test]
    fn shared_transport_keeps_identity_propagates_close_and_tracks_raw_eof() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
        let (peer, _) = listener.accept().unwrap();
        let socket = shared_tcp_socket(client);
        let weak = Arc::downgrade(&socket);
        let mut adapter = SocketAdapter { socket: Arc::clone(&socket), got_read_eof: false };
        assert_eq!(socket.lock().unwrap().set_non_block(true), NetworkErrno::Success);
        assert_eq!(adapter.read(&mut [0; 1]).unwrap_err().kind(), io::ErrorKind::WouldBlock);
        assert!(!adapter.got_read_eof);
        peer.shutdown(std::net::Shutdown::Write).unwrap();
        assert_eq!(socket.lock().unwrap().set_non_block(false), NetworkErrno::Success);
        assert_eq!(adapter.read(&mut [0; 1]).unwrap(), 0);
        assert!(adapter.got_read_eof);
        assert_eq!(socket.lock().unwrap().close(), NetworkErrno::Success);
        assert!(adapter.write(b"x").is_err());
        drop(socket);
        assert!(weak.upgrade().is_some());
        drop(adapter);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn shared_socket_nonblocking_handshake_preserves_ssl_state() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_peer, _) = listener.accept().unwrap();
        let socket = shared_tcp_socket(client);
        assert_eq!(socket.lock().unwrap().set_non_block(true), NetworkErrno::Success);
        let mut backend = SslConnectionBackendOpenSsl::new().unwrap();
        backend.set_socket(Arc::clone(&socket));
        backend.set_host_name("localhost");
        for _ in 0..2 {
            assert_eq!(backend.do_handshake(), RESULT_WOULD_BLOCK);
            let Some(TlsStreamState::Handshaking(stream)) = backend.stream_state.as_ref() else {
                panic!("handshake state discarded");
            };
            assert!(Arc::ptr_eq(&stream.get_ref().socket, &socket));
        }
        let weak = Arc::downgrade(&socket);
        drop(socket);
        assert!(weak.upgrade().is_some());
        drop(backend);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn pending_counts_decrypted_bytes_without_consuming_them() {
        use openssl::asn1::Asn1Time;
        use openssl::hash::MessageDigest;
        use openssl::pkey::PKey;
        use openssl::rsa::Rsa;
        use openssl::ssl::SslAcceptor;
        use openssl::x509::{X509NameBuilder, X509};
        use std::net::TcpListener;
        use std::time::Duration;

        let mut backend = SslConnectionBackendOpenSsl::new().unwrap();
        assert_eq!(backend.pending(), 0);
        let key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
        let mut name = X509NameBuilder::new().unwrap();
        name.append_entry_by_text("CN", "localhost").unwrap();
        let name = name.build();
        let mut cert = X509::builder().unwrap();
        cert.set_version(2).unwrap();
        cert.set_subject_name(&name).unwrap();
        cert.set_issuer_name(&name).unwrap();
        cert.set_pubkey(&key).unwrap();
        cert.set_not_before(&Asn1Time::days_from_now(0).unwrap()).unwrap();
        cert.set_not_after(&Asn1Time::days_from_now(1).unwrap()).unwrap();
        cert.sign(&key, MessageDigest::sha256()).unwrap();
        let mut acceptor = SslAcceptor::mozilla_intermediate(SslMethod::tls()).unwrap();
        acceptor.set_private_key(&key).unwrap();
        acceptor.set_certificate(&cert.build()).unwrap();
        let acceptor = acceptor.build();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (peer, _) = listener.accept().unwrap();
        for stream in [&socket, &peer] {
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            stream.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
        }
        let server = std::thread::spawn(move || {
            let mut stream = acceptor.accept(peer).unwrap();
            // One TLS record: reading one byte leaves five decrypted bytes.
            assert_eq!(stream.ssl_write(b"abcdef").unwrap(), 6);
            let mut ack = [0];
            stream.read_exact(&mut ack).unwrap();
            assert_eq!(ack, [1]);
            // Deliberately violate TLS framing after a valid exchange. This
            // must be an SSL protocol error, not a successful EOF.
            stream.get_mut().write_all(b"HTTP/1.1 200 OK\r\n\r\n").unwrap();
        });
        let shared_socket = shared_tcp_socket(socket);
        backend.set_socket(Arc::clone(&shared_socket));
        assert_eq!(backend.pending(), 0);
        let Some(TlsStreamState::Socket(adapter)) = backend.stream_state.take() else {
            unreachable!();
        };
        let mut config = backend.connector.configure().unwrap();
        // The local fixture is self-signed; production verification is unchanged.
        config.set_verify(SslVerifyMode::NONE);
        let stream = config.connect("localhost", adapter).unwrap_or_else(|_| panic!("local TLS handshake failed"));
        backend.stream_state = Some(TlsStreamState::Connected(stream));
        let mut first = [0];
        assert_eq!(backend.read(&mut first).unwrap(), 1);
        assert_eq!(first, [b'a']);
        assert_eq!(backend.pending(), 5);
        assert_eq!(backend.pending(), 5);
        let mut rest = [0; 5];
        assert_eq!(backend.read(&mut rest).unwrap(), 5);
        assert_eq!(&rest, b"bcdef");
        assert_eq!(backend.pending(), 0);
        assert_eq!(backend.write(&[1]).unwrap(), 1);
        assert_eq!(backend.read(&mut first), Err(RESULT_INTERNAL_ERROR));
        server.join().unwrap();
    }

    #[test]
    fn openssl_backend_creates_successfully() {
        let result = create_ssl_connection_backend();
        assert!(result.is_ok(), "OpenSSL backend should create successfully");
    }

    #[test]
    fn set_hostname_succeeds() {
        let mut backend = create_ssl_connection_backend().unwrap();
        let rc = backend.set_host_name("example.com");
        assert!(rc.is_success());
    }

    #[test]
    fn handshake_without_socket_returns_error() {
        let mut backend = create_ssl_connection_backend().unwrap();
        backend.set_host_name("example.com");
        let rc = backend.do_handshake();
        assert!(rc.is_error());
    }

    #[test]
    fn read_without_handshake_returns_error() {
        let mut backend = create_ssl_connection_backend().unwrap();
        let mut buf = [0u8; 16];
        let result = backend.read(&mut buf);
        assert!(result.is_err());
    }

    #[test]
    fn get_server_certs_without_handshake_returns_error() {
        let backend = create_ssl_connection_backend().unwrap();
        let result = backend.get_server_certs();
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn nonblocking_handshake_retains_mid_handshake_state() {
        let mut sockets = [-1; 2];
        assert_eq!(
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sockets.as_mut_ptr(),) },
            0
        );

        let flags = unsafe { libc::fcntl(sockets[0], libc::F_GETFL) };
        assert_ne!(flags, -1);
        assert_eq!(
            unsafe { libc::fcntl(sockets[0], libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );

        let mut backend = SslConnectionBackendOpenSsl::new().unwrap();
        backend.set_socket(Arc::new(Mutex::new(Box::new(Socket::from_fd(sockets[0])))));
        backend.set_host_name("localhost");

        assert_eq!(backend.do_handshake(), RESULT_WOULD_BLOCK);
        assert!(matches!(
            backend.stream_state,
            Some(TlsStreamState::Handshaking(_))
        ));
        assert_eq!(backend.do_handshake(), RESULT_WOULD_BLOCK);
        assert!(matches!(
            backend.stream_state,
            Some(TlsStreamState::Handshaking(_))
        ));

        unsafe {
            libc::close(sockets[1]);
        }
    }
}
