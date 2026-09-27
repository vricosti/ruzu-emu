// SPDX-FileCopyrightText: Copyright 2022 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/internal_network/socket_proxy.h and socket_proxy.cpp
//! Socket proxy for network tunneling via room network.

use std::collections::VecDeque;
use std::sync::Mutex;

use crate::internal_network::network::{
    Domain, Errno, Protocol, ProxyPacket, ShutdownHow, SockAddrIn, Type, FLAG_MSG_PEEK,
};
use crate::internal_network::sockets::{AcceptResult, NativeSocket, SocketBase, INVALID_SOCKET};

/// Proxy socket for room-based network tunneling.
///
/// Corresponds to upstream `Network::ProxySocket`.
pub struct ProxySocket {
    broadcast: bool,
    closed: bool,
    send_timeout: u32,
    receive_timeout: u32,
    is_bound: bool,
    local_endpoint: SockAddrIn,
    blocking: bool,
    received_packets: Mutex<VecDeque<ProxyPacket>>,
    protocol: Protocol,
    // TODO: room_network: &RoomNetwork,
}

impl ProxySocket {
    pub fn new(/* room_network: &RoomNetwork */) -> Self {
        Self {
            broadcast: false,
            closed: false,
            send_timeout: 0,
            receive_timeout: 0,
            is_bound: false,
            local_endpoint: SockAddrIn::default(),
            blocking: true,
            received_packets: Mutex::new(VecDeque::new()),
            protocol: Protocol::Unspecified,
        }
    }
}

impl SocketBase for ProxySocket {
    fn initialize(&mut self, _domain: Domain, _type_: Type, socket_protocol: Protocol) -> Errno {
        self.protocol = socket_protocol;
        Errno::Success
    }

    fn close(&mut self) -> Errno {
        self.closed = true;
        Errno::Success
    }

    fn accept(&mut self) -> (AcceptResult, Errno) {
        log::warn!("ProxySocket::accept (STUBBED)");
        (AcceptResult::default(), Errno::Success)
    }

    fn connect(&mut self, _addr_in: SockAddrIn) -> Errno {
        log::warn!("ProxySocket::connect (STUBBED)");
        Errno::Success
    }

    fn get_peer_name(&self) -> (SockAddrIn, Errno) {
        log::warn!("ProxySocket::get_peer_name (STUBBED)");
        (SockAddrIn::default(), Errno::Success)
    }

    fn get_sock_name(&self) -> (SockAddrIn, Errno) {
        log::warn!("ProxySocket::get_sock_name (STUBBED)");
        (SockAddrIn::default(), Errno::Success)
    }

    fn bind(&mut self, addr: SockAddrIn) -> Errno {
        if self.is_bound {
            log::warn!("Rebinding Socket is unimplemented!");
            return Errno::Success;
        }
        self.local_endpoint = addr;
        self.is_bound = true;
        Errno::Success
    }

    fn listen(&mut self, _backlog: i32) -> Errno {
        log::warn!("ProxySocket::listen (STUBBED)");
        Errno::Success
    }

    fn shutdown(&mut self, _how: ShutdownHow) -> Errno {
        log::warn!("ProxySocket::shutdown (STUBBED)");
        Errno::Success
    }

    fn recv(&mut self, _flags: i32, _message: &mut [u8]) -> (i32, Errno) {
        log::warn!("ProxySocket::recv (STUBBED)");
        (0, Errno::Success)
    }

    fn recv_from(
        &mut self,
        flags: i32,
        message: &mut [u8],
        addr: Option<&mut SockAddrIn>,
    ) -> (i32, Errno) {
        // When receive_timeout is zero, use 5000ms to prevent hangs
        let timeout = if self.receive_timeout == 0 {
            5000
        } else {
            self.receive_timeout
        };

        let start = std::time::Instant::now();

        loop {
            {
                let mut packets = self.received_packets.lock().unwrap();
                if !packets.is_empty() {
                    return self.receive_packet_inner(
                        &mut packets,
                        flags,
                        message,
                        addr,
                        message.len(),
                    );
                }
            }

            if !self.blocking {
                return (-1, Errno::Again);
            }

            std::thread::yield_now();

            if start.elapsed().as_millis() > timeout as u128 {
                return (-1, Errno::Timedout);
            }
        }
    }

    fn send(&mut self, _message: &[u8], _flags: i32) -> (i32, Errno) {
        log::warn!("ProxySocket::send (STUBBED)");
        (0, Errno::Success)
    }

    fn send_to(&mut self, _flags: u32, message: &[u8], addr: Option<&SockAddrIn>) -> (i32, Errno) {
        if !self.is_bound {
            log::error!("ProxySocket is not bound!");
            return (message.len() as i32, Errno::Success);
        }

        // TODO: Check room_member connection and send packet
        // Upstream compresses data with ZSTD before sending
        let _packet = ProxyPacket {
            local_endpoint: self.local_endpoint.clone(),
            remote_endpoint: addr.cloned().unwrap_or_default(),
            protocol: self.protocol,
            broadcast: self.broadcast && addr.map(|a| a.ip[3] == 255).unwrap_or(false),
            data: message.to_vec(),
        };

        // TODO: Send via room network
        (message.len() as i32, Errno::Success)
    }

    fn set_linger(&mut self, _enable: bool, _linger: u32) -> Errno {
        Errno::Success
    }

    fn set_reuse_addr(&mut self, _enable: bool) -> Errno {
        Errno::Success
    }

    fn set_keep_alive(&mut self, _enable: bool) -> Errno {
        Errno::Success
    }

    fn set_broadcast(&mut self, enable: bool) -> Errno {
        self.broadcast = enable;
        Errno::Success
    }

    fn set_snd_buf(&mut self, _value: u32) -> Errno {
        Errno::Success
    }

    fn set_rcv_buf(&mut self, _value: u32) -> Errno {
        Errno::Success
    }

    fn set_snd_timeo(&mut self, value: u32) -> Errno {
        self.send_timeout = value;
        Errno::Success
    }

    fn set_rcv_timeo(&mut self, value: u32) -> Errno {
        self.receive_timeout = value;
        Errno::Success
    }

    fn set_non_block(&mut self, enable: bool) -> Errno {
        self.blocking = !enable;
        Errno::Success
    }

    fn get_pending_error(&self) -> (Errno, Errno) {
        (Errno::Success, Errno::Success)
    }

    fn is_opened(&self) -> bool {
        !self.closed
    }

    fn handle_proxy_packet(&mut self, packet: &ProxyPacket) {
        if self.protocol != packet.protocol
            || self.local_endpoint.portno != packet.remote_endpoint.portno
            || self.closed
        {
            return;
        }

        if !self.broadcast && packet.broadcast {
            log::info!("Received broadcast packet, but not configured for broadcast mode");
            return;
        }

        // TODO: Decompress with ZSTD
        let decompressed = packet.clone();

        let mut packets = self.received_packets.lock().unwrap();
        packets.push_back(decompressed);
    }

    fn get_fd(&self) -> NativeSocket {
        INVALID_SOCKET // ProxySocket doesn't use a real fd
    }
}

impl ProxySocket {
    /// Internal helper for receiving a packet from the queue.
    ///
    /// Corresponds to upstream `ProxySocket::ReceivePacket`.
    fn receive_packet_inner(
        &self,
        packets: &mut VecDeque<ProxyPacket>,
        flags: i32,
        message: &mut [u8],
        addr: Option<&mut SockAddrIn>,
        max_length: usize,
    ) -> (i32, Errno) {
        let packet = match packets.front() {
            Some(p) => p,
            None => return (-1, Errno::Again),
        };

        if let Some(addr) = addr {
            addr.family = Some(Domain::INET);
            addr.ip = packet.local_endpoint.ip;
            addr.portno = packet.local_endpoint.portno;
        }

        let peek = (flags & FLAG_MSG_PEEK) != 0;

        if packet.data.len() > max_length {
            let read_bytes = max_length;
            message[..max_length].copy_from_slice(&packet.data[..max_length]);

            if self.protocol == Protocol::UDP {
                if !peek {
                    packets.pop_front();
                }
                return (-1, Errno::Msgsize);
            } else if self.protocol == Protocol::TCP {
                let remaining = packet.data[max_length..].to_vec();
                if let Some(front) = packets.front_mut() {
                    front.data = remaining;
                }
            }
            (read_bytes as i32, Errno::Success)
        } else {
            let read_bytes = packet.data.len();
            message[..read_bytes].copy_from_slice(&packet.data);
            if !peek {
                packets.pop_front();
            }
            (read_bytes as i32, Errno::Success)
        }
    }
}
