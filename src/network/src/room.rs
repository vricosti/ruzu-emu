// SPDX-FileCopyrightText: Copyright 2017 Citra Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of Eden src/network/room.h and room.cpp
//!
//! Implements the Room (server) for network multiplayer games.

use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use parking_lot::{Mutex, RwLock};
use rusty_enet as enet;

use common::announce_multiplayer_room::{GameInfo, IPv4Address, Member, RoomInformation};

use crate::packet::Packet;
use crate::verify_user;

type Server = enet::Host<UdpSocket>;

// ---------------------------------------------------------------------------
// Constants (from room.h)
// ---------------------------------------------------------------------------

/// The version of this Room and RoomMember.
pub const NETWORK_VERSION: u32 = 1;

/// Default port for room connections.
pub const DEFAULT_ROOM_PORT: u16 = 24872;

/// Maximum chat message size.
pub const MAX_MESSAGE_SIZE: u32 = 500;

/// Maximum number of concurrent connections allowed to this room.
pub const MAX_CONCURRENT_CONNECTIONS: u32 = 254;

/// Number of channels used for the connection.
pub const NUM_CHANNELS: usize = 1;

/// A special IP address that tells the room to assign one automatically.
pub const NO_PREFERRED_IP: IPv4Address = [0xFF, 0xFF, 0xFF, 0xFF];

// ---------------------------------------------------------------------------
// Room message types (from room.h)
// ---------------------------------------------------------------------------

/// The different types of messages that can be sent. The first byte of each
/// packet defines the type.
/// Maps to C++ `Network::RoomMessageTypes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RoomMessageTypes {
    IdJoinRequest = 1,
    IdJoinSuccess,
    IdRoomInformation,
    IdSetGameInfo,
    IdProxyPacket,
    IdLdnPacket,
    IdChatMessage,
    IdNameCollision,
    IdIpCollision,
    IdVersionMismatch,
    IdWrongPassword,
    IdCloseRoom,
    IdRoomIsFull,
    IdStatusMessage,
    IdHostKicked,
    IdHostBanned,
    /// Moderation requests
    IdModKick,
    IdModBan,
    IdModUnban,
    IdModGetBanList,
    /// Moderation responses
    IdModBanListResponse,
    IdModPermissionDenied,
    IdModNoSuchUser,
    IdJoinSuccessAsMod,
}

impl RoomMessageTypes {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::IdJoinRequest),
            2 => Some(Self::IdJoinSuccess),
            3 => Some(Self::IdRoomInformation),
            4 => Some(Self::IdSetGameInfo),
            5 => Some(Self::IdProxyPacket),
            6 => Some(Self::IdLdnPacket),
            7 => Some(Self::IdChatMessage),
            8 => Some(Self::IdNameCollision),
            9 => Some(Self::IdIpCollision),
            10 => Some(Self::IdVersionMismatch),
            11 => Some(Self::IdWrongPassword),
            12 => Some(Self::IdCloseRoom),
            13 => Some(Self::IdRoomIsFull),
            14 => Some(Self::IdStatusMessage),
            15 => Some(Self::IdHostKicked),
            16 => Some(Self::IdHostBanned),
            17 => Some(Self::IdModKick),
            18 => Some(Self::IdModBan),
            19 => Some(Self::IdModUnban),
            20 => Some(Self::IdModGetBanList),
            21 => Some(Self::IdModBanListResponse),
            22 => Some(Self::IdModPermissionDenied),
            23 => Some(Self::IdModNoSuchUser),
            24 => Some(Self::IdJoinSuccessAsMod),
            _ => None,
        }
    }
}

/// Types of system status messages.
/// Maps to C++ `Network::StatusMessageTypes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum StatusMessageTypes {
    /// Member joining.
    IdMemberJoin = 1,
    /// Member leaving.
    IdMemberLeave,
    /// A member is kicked from the room.
    IdMemberKicked,
    /// A member is banned from the room.
    IdMemberBanned,
    /// A username / ip address is unbanned from the room.
    IdAddressUnbanned,
}

impl StatusMessageTypes {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::IdMemberJoin),
            2 => Some(Self::IdMemberLeave),
            3 => Some(Self::IdMemberKicked),
            4 => Some(Self::IdMemberBanned),
            5 => Some(Self::IdAddressUnbanned),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Ban list types
// ---------------------------------------------------------------------------

pub type UsernameBanList = Vec<String>;
pub type IpBanList = Vec<String>;
pub type BanList = (UsernameBanList, IpBanList);

// ---------------------------------------------------------------------------
// Room::State
// ---------------------------------------------------------------------------

/// The state of a Room.
/// Maps to C++ `Network::Room::State`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RoomState {
    /// The room is open and ready to accept connections.
    Open = 0,
    /// The room is not opened and can not accept connections.
    Closed = 1,
}

// ---------------------------------------------------------------------------
// RoomImpl (internal state)
// ---------------------------------------------------------------------------

/// Byte-preserving form of the member's upstream GameInfo.
#[derive(Default)]
struct MemberGameInfo {
    // The server forwards std::string bytes unchanged, while the public
    // frontend GameInfo is UTF-8 text. Convert only in GetRoomMemberList.
    name: Vec<u8>,
    id: u64,
    version: Vec<u8>,
}

/// Internal member data, corresponding to C++ Room::RoomImpl::Member.
struct RoomMemberEntry {
    nickname: String,
    game_info: MemberGameInfo,
    fake_ip: IPv4Address,
    user_data: verify_user::UserData,
    peer: enet::PeerID,
}

/// Internal room implementation.
/// Maps to C++ `Room::RoomImpl`.
struct RoomImpl {
    state: AtomicU8,
    room_information: RwLock<RoomInformation>,

    verify_uid: Mutex<String>,
    password: Mutex<String>,

    members: RwLock<Vec<RoomMemberEntry>>,
    // Upstream protects both lists with the same ban_list_mutex.
    ban_list: Mutex<BanList>,

    verify_backend: Mutex<Option<Box<dyn verify_user::Backend>>>,
}

impl RoomImpl {
    fn new() -> Self {
        Self {
            state: AtomicU8::new(RoomState::Closed as u8),
            room_information: RwLock::new(RoomInformation::default()),
            verify_uid: Mutex::new(String::new()),
            password: Mutex::new(String::new()),
            members: RwLock::new(Vec::new()),
            ban_list: Mutex::new((Vec::new(), Vec::new())),
            verify_backend: Mutex::new(None),
        }
    }

    fn get_state(&self) -> RoomState {
        match self.state.load(Ordering::SeqCst) {
            0 => RoomState::Open,
            _ => RoomState::Closed,
        }
    }

    fn set_state(&self, state: RoomState) {
        self.state.store(state as u8, Ordering::SeqCst);
    }

    fn start_loop(self: &Arc<Self>, mut server: Server) -> std::io::Result<JoinHandle<()>> {
        let room = Arc::clone(self);
        std::thread::Builder::new()
            .name("Room".into())
            .spawn(move || {
                while room.get_state() != RoomState::Closed {
                    match server.service().map(|event| event.map(enet::Event::no_ref)) {
                        Ok(Some(enet::EventNoRef::Receive { peer, packet, .. })) => {
                            use RoomMessageTypes::*;
                            let bytes = packet.data();
                            let command =
                                bytes.first().and_then(|id| RoomMessageTypes::from_u8(*id));
                            match command {
                                Some(IdJoinRequest) => {
                                    room.handle_join_request(&mut server, peer, bytes)
                                }
                                Some(IdSetGameInfo) => {
                                    room.handle_game_info_packet(&mut server, peer, bytes)
                                }
                                Some(IdProxyPacket) => {
                                    room.handle_proxy_packet(&mut server, peer, bytes)
                                }
                                Some(IdLdnPacket) => {
                                    room.handle_ldn_packet(&mut server, peer, bytes)
                                }
                                Some(IdChatMessage) => {
                                    room.handle_chat_packet(&mut server, peer, bytes)
                                }
                                Some(IdModKick) => {
                                    room.handle_mod_kick_packet(&mut server, peer, bytes)
                                }
                                Some(IdModBan) => {
                                    room.handle_mod_ban_packet(&mut server, peer, bytes)
                                }
                                Some(IdModUnban) => {
                                    room.handle_mod_unban_packet(&mut server, peer, bytes)
                                }
                                Some(IdModGetBanList) => {
                                    room.handle_mod_get_ban_list_packet(&mut server, peer)
                                }
                                _ => {}
                            }
                        }
                        Ok(Some(enet::EventNoRef::Disconnect { peer, .. })) => {
                            room.handle_client_disconnection(&mut server, peer);
                        }
                        Ok(Some(enet::EventNoRef::Connect { .. })) => {}
                        // rusty_enet service is nonblocking. Only wait when drained,
                        // unlike sleeping after every packet (which limits throughput).
                        // Upstream's enet_host_service timeout is 5 milliseconds.
                        Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                        Err(error) => {
                            log::error!("Room socket service failed: {error}");
                            room.set_state(RoomState::Closed);
                        }
                    }
                }
                room.send_close_message(&mut server);
            })
    }

    fn is_valid_nickname(&self, nickname: &str) -> bool {
        (4..=20).contains(&nickname.len())
            && nickname
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b" ._-".contains(&c))
            && self
                .members
                .read()
                .iter()
                .all(|member| member.nickname != nickname)
    }

    fn is_valid_fake_ip_address(&self, address: IPv4Address) -> bool {
        self.members
            .read()
            .iter()
            .all(|member| member.fake_ip != address)
    }

    fn generate_fake_ip_address(&self) -> IPv4Address {
        let members = self.members.read();
        for i in 1..255 {
            for j in 1..255 {
                let address = [192, 168, i, j];
                if members.iter().all(|member| member.fake_ip != address) {
                    return address;
                }
            }
        }
        log::error!("All room addresses are taken");
        [192, 168, 0, 0]
    }

    fn has_mod_permission(&self, peer: enet::PeerID) -> bool {
        let members = self.members.read();
        let Some(member) = members.iter().find(|member| member.peer == peer) else {
            return false;
        };
        let info = self.room_information.read();
        member.user_data.moderator
            || (!info.host_username.is_empty() && member.user_data.username == info.host_username)
    }

    fn handle_join_request(&self, server: &mut Server, peer: enet::PeerID, bytes: &[u8]) {
        if self.members.read().len() >= self.room_information.read().member_slots as usize {
            self.send_room_is_full(server, peer);
            return;
        }
        let mut packet = Packet::new();
        packet.append(bytes);
        packet.ignore_bytes(1);
        let Some(nickname) = packet.read_string() else {
            return;
        };
        let Some(mut fake_ip) = packet.read_array() else {
            return;
        };
        let Some(version) = packet.read_u32() else {
            return;
        };
        let Some(password) = packet.read_string_bytes() else {
            return;
        };
        let Some(token) = packet.read_string() else {
            return;
        };
        if password != self.password.lock().as_bytes() {
            self.send_wrong_password(server, peer);
            return;
        }
        if !self.is_valid_nickname(&nickname) {
            self.send_name_collision(server, peer);
            return;
        }
        if fake_ip != NO_PREFERRED_IP {
            if !self.is_valid_fake_ip_address(fake_ip) {
                self.send_ip_collision(server, peer);
                return;
            }
        } else {
            fake_ip = self.generate_fake_ip_address();
        }
        if version != NETWORK_VERSION {
            self.send_version_mismatch(server, peer);
            return;
        }
        let uid = self.verify_uid.lock().clone();
        let mut user_data = self
            .verify_backend
            .lock()
            .as_ref()
            .map(|backend| backend.load_user_data(&uid, &token))
            .unwrap_or_default();
        if nickname == self.room_information.read().host_username {
            user_data.moderator = true;
        }
        let ip = server
            .peer(peer)
            .address()
            .map(|address| address.ip().to_string())
            .unwrap_or_default();
        let banned = {
            let (usernames, ips) = &*self.ban_list.lock();
            (!user_data.username.is_empty() && usernames.contains(&user_data.username))
                || ips.contains(&ip)
        };
        if banned {
            self.send_user_banned(server, peer);
            return;
        }
        self.send_status_message(
            server,
            StatusMessageTypes::IdMemberJoin,
            &nickname,
            &user_data.username,
            &ip,
        );
        self.members.write().push(RoomMemberEntry {
            nickname,
            game_info: MemberGameInfo::default(),
            fake_ip,
            user_data,
            peer,
        });
        self.broadcast_room_information(server);
        if self.has_mod_permission(peer) {
            self.send_join_success_as_mod(server, peer, fake_ip);
        } else {
            self.send_join_success(server, peer, fake_ip);
        }
    }

    // Mechanical Rust equivalent of enet_packet_create/reliable + peer_send +
    // host_flush repeated by the upstream Send* methods. No dispatch ownership.
    fn send(server: &mut Server, peer: enet::PeerID, packet: &Packet) {
        if let Err(error) = server
            .peer_mut(peer)
            .send(0, &enet::Packet::reliable(packet.get_data()))
        {
            log::error!("Room packet send failed: {error:?}");
        }
        server.flush();
    }

    fn send_name_collision(&self, server: &mut Server, peer: enet::PeerID) {
        let mut packet = Packet::new();
        packet.write_u8(RoomMessageTypes::IdNameCollision as u8);
        Self::send(server, peer, &packet);
    }
    fn send_ip_collision(&self, server: &mut Server, peer: enet::PeerID) {
        let mut packet = Packet::new();
        packet.write_u8(RoomMessageTypes::IdIpCollision as u8);
        Self::send(server, peer, &packet);
    }
    fn send_wrong_password(&self, server: &mut Server, peer: enet::PeerID) {
        let mut packet = Packet::new();
        packet.write_u8(RoomMessageTypes::IdWrongPassword as u8);
        Self::send(server, peer, &packet);
    }
    fn send_room_is_full(&self, server: &mut Server, peer: enet::PeerID) {
        let mut packet = Packet::new();
        packet.write_u8(RoomMessageTypes::IdRoomIsFull as u8);
        Self::send(server, peer, &packet);
    }
    fn send_version_mismatch(&self, server: &mut Server, peer: enet::PeerID) {
        let mut packet = Packet::new();
        packet.write_u8(RoomMessageTypes::IdVersionMismatch as u8);
        packet.write_u32(NETWORK_VERSION);
        Self::send(server, peer, &packet);
    }
    fn send_join_success(&self, server: &mut Server, peer: enet::PeerID, ip: IPv4Address) {
        let mut packet = Packet::new();
        packet.write_u8(RoomMessageTypes::IdJoinSuccess as u8);
        packet.write_array(&ip);
        Self::send(server, peer, &packet);
    }
    fn send_join_success_as_mod(&self, server: &mut Server, peer: enet::PeerID, ip: IPv4Address) {
        let mut packet = Packet::new();
        packet.write_u8(RoomMessageTypes::IdJoinSuccessAsMod as u8);
        packet.write_array(&ip);
        Self::send(server, peer, &packet);
    }
    fn send_user_kicked(&self, server: &mut Server, peer: enet::PeerID) {
        let mut packet = Packet::new();
        packet.write_u8(RoomMessageTypes::IdHostKicked as u8);
        Self::send(server, peer, &packet);
    }
    fn send_user_banned(&self, server: &mut Server, peer: enet::PeerID) {
        let mut packet = Packet::new();
        packet.write_u8(RoomMessageTypes::IdHostBanned as u8);
        Self::send(server, peer, &packet);
    }
    fn send_mod_permission_denied(&self, server: &mut Server, peer: enet::PeerID) {
        let mut packet = Packet::new();
        packet.write_u8(RoomMessageTypes::IdModPermissionDenied as u8);
        Self::send(server, peer, &packet);
    }
    fn send_mod_no_such_user(&self, server: &mut Server, peer: enet::PeerID) {
        let mut packet = Packet::new();
        packet.write_u8(RoomMessageTypes::IdModNoSuchUser as u8);
        Self::send(server, peer, &packet);
    }

    fn send_mod_ban_list_response(&self, server: &mut Server, peer: enet::PeerID) {
        let mut packet = Packet::new();
        packet.write_u8(RoomMessageTypes::IdModBanListResponse as u8);
        {
            let bans = self.ban_list.lock();
            packet.write_vec_string(&bans.0);
            packet.write_vec_string(&bans.1);
        }
        Self::send(server, peer, &packet);
    }

    fn send_close_message(&self, server: &mut Server) {
        let packet = enet::Packet::reliable(&[RoomMessageTypes::IdCloseRoom as u8][..]);
        let members = self.members.read();
        for member in members.iter() {
            let _ = server.peer_mut(member.peer).send(0, &packet);
        }
        server.flush();
        for member in members.iter() {
            server.peer_mut(member.peer).disconnect(0);
        }
    }

    fn send_status_message(
        &self,
        server: &mut Server,
        kind: StatusMessageTypes,
        nickname: &str,
        username: &str,
        ip: &str,
    ) {
        let mut packet = Packet::new();
        packet.write_u8(RoomMessageTypes::IdStatusMessage as u8);
        packet.write_u8(kind as u8);
        packet.write_string(nickname);
        packet.write_string(username);
        let packet = enet::Packet::reliable(packet.get_data());
        for member in self.members.read().iter() {
            let _ = server.peer_mut(member.peer).send(0, &packet);
        }
        server.flush();
        log::info!("Room status {kind:?}: {nickname} ({username}) [{ip}]");
    }

    fn broadcast_room_information(&self, server: &mut Server) {
        let mut packet = Packet::new();
        let info = self.room_information.read();
        packet.write_u8(RoomMessageTypes::IdRoomInformation as u8);
        packet.write_string(&info.name);
        packet.write_string(&info.description);
        packet.write_u32(info.member_slots);
        packet.write_u16(info.port);
        packet.write_string(&info.preferred_game.name);
        packet.write_string(&info.host_username);
        let members = self.members.read();
        packet.write_u32(members.len() as u32);
        for member in members.iter() {
            packet.write_string(&member.nickname);
            packet.write_array(&member.fake_ip);
            packet.write_string_bytes(&member.game_info.name);
            packet.write_u64(member.game_info.id);
            packet.write_string_bytes(&member.game_info.version);
            packet.write_string(&member.user_data.username);
            packet.write_string(&member.user_data.display_name);
            packet.write_string(&member.user_data.avatar_url);
        }
        server.broadcast(0, &enet::Packet::reliable(packet.get_data()));
        server.flush();
    }
}

// ---------------------------------------------------------------------------
// Room (public API)
// ---------------------------------------------------------------------------

/// This is what a server (person creating a server) would use.
/// Maps to C++ `Network::Room`.
pub struct Room {
    room_impl: Arc<RoomImpl>,
    // Serializes Create/Destroy. The worker owns its Host exclusively, and
    // only retains RoomImpl, so joining never depends on this mutex or Room.
    room_thread: Mutex<Option<JoinHandle<()>>>,
}

impl Room {
    pub fn new() -> Self {
        Self {
            room_impl: Arc::new(RoomImpl::new()),
            room_thread: Mutex::new(None),
        }
    }

    /// Gets the current state of the room.
    pub fn get_state(&self) -> RoomState {
        self.room_impl.get_state()
    }

    /// Gets the room information of the room.
    pub fn get_room_information(&self) -> RoomInformation {
        self.room_impl.room_information.read().clone()
    }

    /// Gets the verify UID of this room.
    pub fn get_verify_uid(&self) -> String {
        self.room_impl.verify_uid.lock().clone()
    }

    /// Gets a list of the members connected to the room.
    pub fn get_room_member_list(&self) -> Vec<Member> {
        let members = self.room_impl.members.read();
        members
            .iter()
            .map(|m| Member {
                nickname: m.nickname.clone(),
                username: m.user_data.username.clone(),
                display_name: m.user_data.display_name.clone(),
                avatar_url: m.user_data.avatar_url.clone(),
                fake_ip: m.fake_ip,
                game: GameInfo {
                    name: String::from_utf8_lossy(&m.game_info.name).into_owned(),
                    id: m.game_info.id,
                    version: String::from_utf8_lossy(&m.game_info.version).into_owned(),
                },
            })
            .collect()
    }

    /// Checks if the room is password protected.
    pub fn has_password(&self) -> bool {
        !self.room_impl.password.lock().is_empty()
    }

    /// Creates the socket for this room.
    ///
    /// The worker exclusively owns the ENet host until Destroy joins it.
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        &self,
        name: &str,
        description: &str,
        server: &str,
        server_port: u16,
        password: &str,
        max_connections: u32,
        host_username: &str,
        preferred_game: GameInfo,
        verify_backend: Option<Box<dyn verify_user::Backend>>,
        ban_list: &BanList,
        enable_yuzu_mods: bool,
    ) -> bool {
        let mut thread = self.room_thread.lock();
        if thread.is_some() {
            return false;
        }
        let address = if server.is_empty() {
            SocketAddr::from((Ipv4Addr::UNSPECIFIED, server_port))
        } else {
            let Some(address) = (server, server_port)
                .to_socket_addrs()
                .ok()
                .and_then(|mut addresses| addresses.find(SocketAddr::is_ipv4))
            else {
                return false;
            };
            address
        };
        let Ok(socket) = UdpSocket::bind(address) else {
            return false;
        };
        // The extra transport slot lets us send IdRoomIsFull to a client
        // rather than letting ENet silently reject its connection.
        let Some(peer_limit) = max_connections.checked_add(1) else {
            return false;
        };
        let Ok(host) = Server::new(
            socket,
            enet::HostSettings {
                peer_limit: peer_limit as usize,
                channel_limit: NUM_CHANNELS,
                ..Default::default()
            },
        ) else {
            return false;
        };

        {
            let mut info = self.room_impl.room_information.write();
            info.name = name.to_string();
            info.description = description.to_string();
            info.member_slots = max_connections;
            info.port = server_port;
            info.preferred_game = preferred_game;
            info.host_username = host_username.to_string();
            info.enable_yuzu_mods = enable_yuzu_mods;
        }

        *self.room_impl.password.lock() = password.to_string();
        *self.room_impl.verify_backend.lock() = verify_backend;
        *self.room_impl.ban_list.lock() = ban_list.clone();

        self.room_impl.set_state(RoomState::Open);
        match self.room_impl.start_loop(host) {
            Ok(handle) => {
                *thread = Some(handle);
                true
            }
            Err(error) => {
                log::error!("Could not start room server: {error}");
                self.room_impl.set_state(RoomState::Closed);
                *self.room_impl.room_information.write() = RoomInformation::default();
                false
            }
        }
    }

    /// Sets the verification GUID of the room.
    pub fn set_verify_uid(&self, uid: &str) {
        *self.room_impl.verify_uid.lock() = uid.to_string();
    }

    /// Gets the ban list (including banned forum usernames and IPs) of the room.
    pub fn get_ban_list(&self) -> BanList {
        self.room_impl.ban_list.lock().clone()
    }

    /// Destroys the room.
    pub fn destroy(&self) {
        let mut thread = self.room_thread.lock();
        self.room_impl.set_state(RoomState::Closed);
        if let Some(handle) = thread.take() {
            if handle.join().is_err() {
                log::error!("Room server worker panicked");
            }
        }
        {
            let mut info = self.room_impl.room_information.write();
            *info = RoomInformation::default();
        }
        self.room_impl.members.write().clear();
    }
}

impl RoomImpl {
    fn handle_mod_kick_packet(&self, server: &mut Server, peer: enet::PeerID, bytes: &[u8]) {
        if !self.has_mod_permission(peer) {
            self.send_mod_permission_denied(server, peer);
            return;
        }
        let mut packet = Packet::new();
        packet.append(bytes);
        packet.ignore_bytes(1);
        let Some(nickname) = packet.read_string() else {
            return;
        };
        let (username, ip) = {
            let mut members = self.members.write();
            let Some(index) = members.iter().position(|m| m.nickname == nickname) else {
                self.send_mod_no_such_user(server, peer);
                return;
            };
            let target = &members[index];
            self.send_user_kicked(server, target.peer);
            let username = target.user_data.username.clone();
            let ip = server
                .peer(target.peer)
                .address()
                .map(|a| a.ip().to_string())
                .unwrap_or_default();
            server.peer_mut(target.peer).disconnect(0);
            members.remove(index);
            (username, ip)
        };
        self.send_status_message(
            server,
            StatusMessageTypes::IdMemberKicked,
            &nickname,
            &username,
            &ip,
        );
        self.broadcast_room_information(server);
    }

    fn handle_mod_ban_packet(&self, server: &mut Server, peer: enet::PeerID, bytes: &[u8]) {
        if !self.has_mod_permission(peer) {
            self.send_mod_permission_denied(server, peer);
            return;
        }
        let mut packet = Packet::new();
        packet.append(bytes);
        packet.ignore_bytes(1);
        let Some(nickname) = packet.read_string() else {
            return;
        };
        let (username, ip) = {
            let mut members = self.members.write();
            let Some(index) = members.iter().position(|m| m.nickname == nickname) else {
                self.send_mod_no_such_user(server, peer);
                return;
            };
            let target = &members[index];
            self.send_user_banned(server, target.peer);
            let username = target.user_data.username.clone();
            let ip = server
                .peer(target.peer)
                .address()
                .map(|a| a.ip().to_string())
                .unwrap_or_default();
            server.peer_mut(target.peer).disconnect(0);
            members.remove(index);
            (username, ip)
        };
        {
            let mut bans = self.ban_list.lock();
            let (usernames, ips) = &mut *bans;
            if !username.is_empty() && !usernames.contains(&username) {
                usernames.push(username.clone());
            }
            if !ips.contains(&ip) {
                ips.push(ip.clone());
            }
        }
        self.send_status_message(
            server,
            StatusMessageTypes::IdMemberBanned,
            &nickname,
            &username,
            &ip,
        );
        self.broadcast_room_information(server);
    }

    fn handle_mod_unban_packet(&self, server: &mut Server, peer: enet::PeerID, bytes: &[u8]) {
        if !self.has_mod_permission(peer) {
            self.send_mod_permission_denied(server, peer);
            return;
        }
        let mut packet = Packet::new();
        packet.append(bytes);
        packet.ignore_bytes(1);
        let Some(address) = packet.read_string() else {
            return;
        };
        let mut unbanned = false;
        {
            let mut bans = self.ban_list.lock();
            let (usernames, ips) = &mut *bans;
            for entries in [usernames, ips] {
                if let Some(index) = entries.iter().position(|entry| entry == &address) {
                    entries.remove(index);
                    unbanned = true;
                }
            }
        }
        if unbanned {
            self.send_status_message(
                server,
                StatusMessageTypes::IdAddressUnbanned,
                &address,
                "",
                "",
            );
        } else {
            self.send_mod_no_such_user(server, peer);
        }
    }

    fn handle_mod_get_ban_list_packet(&self, server: &mut Server, peer: enet::PeerID) {
        if !self.has_mod_permission(peer) {
            self.send_mod_permission_denied(server, peer);
            return;
        }
        self.send_mod_ban_list_response(server, peer);
    }

    fn handle_proxy_packet(&self, server: &mut Server, peer: enet::PeerID, bytes: &[u8]) {
        let mut packet = Packet::new();
        packet.append(bytes);
        packet.ignore_bytes(1 + 1 + 4 + 2 + 1);
        let Some(remote_ip) = packet.read_array::<4>() else {
            return;
        };
        packet.ignore_bytes(2 + 1);
        let Some(broadcast) = packet.read_bool() else {
            return;
        };
        let packet = enet::Packet::reliable(bytes);
        let members = self.members.read();
        if broadcast {
            for member in members.iter().filter(|member| member.peer != peer) {
                let _ = server.peer_mut(member.peer).send(0, &packet);
            }
        } else if let Some(member) = members.iter().find(|member| member.fake_ip == remote_ip) {
            let _ = server.peer_mut(member.peer).send(0, &packet);
        } else {
            log::error!("Attempting to send to unknown IP address: {remote_ip:?}");
        }
        server.flush();
    }

    fn handle_ldn_packet(&self, server: &mut Server, peer: enet::PeerID, bytes: &[u8]) {
        let mut packet = Packet::new();
        packet.append(bytes);
        packet.ignore_bytes(1 + 1 + 4);
        let Some(remote_ip) = packet.read_array::<4>() else {
            return;
        };
        let Some(broadcast) = packet.read_bool() else {
            return;
        };
        let packet = enet::Packet::reliable(bytes);
        let members = self.members.read();
        if broadcast {
            for member in members.iter().filter(|member| member.peer != peer) {
                let _ = server.peer_mut(member.peer).send(0, &packet);
            }
        } else if let Some(member) = members.iter().find(|member| member.fake_ip == remote_ip) {
            let _ = server.peer_mut(member.peer).send(0, &packet);
        } else {
            log::error!("Attempting to send to unknown IP address: {remote_ip:?}");
        }
        server.flush();
    }

    fn handle_chat_packet(&self, server: &mut Server, peer: enet::PeerID, bytes: &[u8]) {
        let mut packet = Packet::new();
        packet.append(bytes);
        packet.ignore_bytes(1);
        let Some(mut message) = packet.read_string_bytes() else {
            return;
        };
        let members = self.members.read();
        let Some(sender) = members.iter().find(|member| member.peer == peer) else {
            return;
        };
        message.truncate(MAX_MESSAGE_SIZE as usize);
        let mut packet = Packet::new();
        packet.write_u8(RoomMessageTypes::IdChatMessage as u8);
        packet.write_string(&sender.nickname);
        packet.write_string(&sender.user_data.username);
        packet.write_string_bytes(&message);
        let packet = enet::Packet::reliable(packet.get_data());
        for member in members.iter().filter(|member| member.peer != peer) {
            let _ = server.peer_mut(member.peer).send(0, &packet);
        }
        server.flush();
        log::info!(
            "{} ({}): {}",
            sender.nickname,
            sender.user_data.username,
            String::from_utf8_lossy(&message)
        );
    }

    fn handle_game_info_packet(&self, server: &mut Server, peer: enet::PeerID, bytes: &[u8]) {
        let mut packet = Packet::new();
        packet.append(bytes);
        packet.ignore_bytes(1);
        let Some(name) = packet.read_string_bytes() else {
            return;
        };
        let Some(id) = packet.read_u64() else {
            return;
        };
        let Some(version) = packet.read_string_bytes() else {
            return;
        };
        {
            let mut members = self.members.write();
            if let Some(member) = members.iter_mut().find(|member| member.peer == peer) {
                member.game_info = MemberGameInfo { name, id, version };
                log::info!(
                    "{} is playing {} ({})",
                    member.nickname,
                    String::from_utf8_lossy(&member.game_info.name),
                    String::from_utf8_lossy(&member.game_info.version)
                );
            }
        }
        self.broadcast_room_information(server);
    }

    fn handle_client_disconnection(&self, server: &mut Server, peer: enet::PeerID) {
        let removed = {
            let mut members = self.members.write();
            members
                .iter()
                .position(|member| member.peer == peer)
                .map(|index| members.remove(index))
        };
        let ip = server
            .peer(peer)
            .address()
            .map(|address| address.ip().to_string())
            .unwrap_or_default();
        server.peer_mut(peer).disconnect(0);
        if let Some(member) = removed {
            if !member.nickname.is_empty() {
                self.send_status_message(
                    server,
                    StatusMessageTypes::IdMemberLeave,
                    &member.nickname,
                    &member.user_data.username,
                    &ip,
                );
            }
        }
        self.broadcast_room_information(server);
    }
}

impl Drop for Room {
    fn drop(&mut self) {
        self.destroy();
    }
}

impl Default for Room {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_room(slots: u32) -> (Room, u16) {
        // Reserve a candidate, then retry if another process binds between
        // releasing the probe and Create. All sockets are loopback-only.
        for _ in 0..16 {
            let probe = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let port = probe.local_addr().unwrap().port();
            drop(probe);
            let room = Room::new();
            if room.create(
                "Synthetic room",
                "",
                "127.0.0.1",
                port,
                "secret",
                slots,
                "Host",
                GameInfo::default(),
                None,
                &(vec![], vec![]),
                false,
            ) {
                return (room, port);
            }
        }
        panic!("could not bind a local test room");
    }

    struct Client {
        host: Server,
        peer: enet::PeerID,
    }
    impl Client {
        fn connect(port: u16) -> Self {
            let mut host = Server::new(
                UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
                enet::HostSettings {
                    peer_limit: 1,
                    channel_limit: 1,
                    ..Default::default()
                },
            )
            .unwrap();
            let peer = host
                .connect(SocketAddr::from((Ipv4Addr::LOCALHOST, port)), 1, 0)
                .unwrap()
                .id();
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                if matches!(host.service().unwrap(), Some(enet::Event::Connect { .. })) {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "local ENet connection timeout"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            Self { host, peer }
        }
        fn send(&mut self, packet: &Packet) {
            self.host
                .peer_mut(self.peer)
                .send(0, &enet::Packet::reliable(packet.get_data()))
                .unwrap();
            self.host.flush();
        }
        fn join(&mut self, name: &str, ip: IPv4Address, password: &str, version: u32) {
            let mut packet = Packet::new();
            packet.write_u8(RoomMessageTypes::IdJoinRequest as u8);
            packet.write_string(name);
            packet.write_array(&ip);
            packet.write_u32(version);
            packet.write_string(password);
            packet.write_string("");
            self.send(&packet);
        }
        fn receive(&mut self, kind: RoomMessageTypes) -> Packet {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                if let Some(enet::Event::Receive { packet, .. }) = self.host.service().unwrap() {
                    if packet.data().first() == Some(&(kind as u8)) {
                        let mut result = Packet::new();
                        result.append(packet.data());
                        result.ignore_bytes(1);
                        return result;
                    }
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "missing local room reply {kind:?}"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        fn command(&mut self, kind: RoomMessageTypes, text: &str) {
            let mut packet = Packet::new();
            packet.write_u8(kind as u8);
            packet.write_string(text);
            self.send(&packet);
        }
        fn disconnected(&mut self) {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                if matches!(
                    self.host.service().unwrap(),
                    Some(enet::Event::Disconnect { .. })
                ) {
                    return;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "local peer was not disconnected"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    #[test]
    fn server_bind_failure_does_not_publish_open_and_destroy_releases_socket() {
        let occupied = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = occupied.local_addr().unwrap().port();
        let room = Room::new();
        assert!(!room.create(
            "Test",
            "",
            "127.0.0.1",
            port,
            "",
            4,
            "Host",
            GameInfo::default(),
            None,
            &(vec![], vec![]),
            false
        ));
        assert_eq!(room.get_state(), RoomState::Closed);
        assert!(room.get_room_information().name.is_empty());
        drop(occupied);
        assert!(room.create(
            "Test",
            "",
            "127.0.0.1",
            port,
            "",
            4,
            "Host",
            GameInfo::default(),
            None,
            &(vec![], vec![]),
            false
        ));
        room.destroy();
        assert!(UdpSocket::bind((Ipv4Addr::LOCALHOST, port)).is_ok());
        assert!(room.create(
            "Again",
            "",
            "127.0.0.1",
            port,
            "",
            4,
            "Host",
            GameInfo::default(),
            None,
            &(vec![], vec![]),
            false
        ));
        drop(room);
        assert!(UdpSocket::bind((Ipv4Addr::LOCALHOST, port)).is_ok());
    }

    #[test]
    fn server_join_validation_matches_upstream_order() {
        use RoomMessageTypes::*;
        for (name, password, version, expected) in [
            ("!", "wrong", 0, IdWrongPassword),
            ("!", "secret", 0, IdNameCollision),
            ("Guest", "secret", 0, IdVersionMismatch),
        ] {
            let (_room, port) = local_room(4);
            let mut client = Client::connect(port);
            client.join(name, NO_PREFERRED_IP, password, version);
            let mut reply = client.receive(expected);
            if expected == IdVersionMismatch {
                assert_eq!(reply.read_u32(), Some(NETWORK_VERSION));
            }
            assert!(reply.end_of_packet());
        }
        for (slots, name, ip, expected) in [
            (1, "Host", [192, 168, 1, 1], IdRoomIsFull),
            (4, "Host", [192, 168, 1, 1], IdNameCollision),
            (4, "Guest", [192, 168, 1, 1], IdIpCollision),
        ] {
            let (_room, port) = local_room(slots);
            let mut host = Client::connect(port);
            host.join("Host", NO_PREFERRED_IP, "secret", NETWORK_VERSION);
            assert_eq!(
                host.receive(IdJoinSuccessAsMod).read_array::<4>(),
                Some([192, 168, 1, 1])
            );
            let mut guest = Client::connect(port);
            guest.join(name, ip, "secret", NETWORK_VERSION);
            guest.receive(expected);
        }
    }

    #[test]
    fn server_relays_raw_chat_and_routes_ldn_and_proxy_packets() {
        use RoomMessageTypes::*;
        let (room, port) = local_room(4);
        let mut host = Client::connect(port);
        host.join("Host", NO_PREFERRED_IP, "secret", NETWORK_VERSION);
        host.receive(IdJoinSuccessAsMod);
        let mut guest = Client::connect(port);
        guest.join("Guest", NO_PREFERRED_IP, "secret", NETWORK_VERSION);
        let guest_ip = guest.receive(IdJoinSuccess).read_array::<4>().unwrap();
        assert_eq!(guest_ip, [192, 168, 1, 2]);
        assert_eq!(room.get_room_member_list().len(), 2);

        // Drain the initial join broadcast before testing the update. Validate
        // the complete upstream field order, including arbitrary string bytes.
        host.receive(IdRoomInformation);
        let mut metadata = Packet::new();
        metadata.write_u8(IdSetGameInfo as u8);
        metadata.write_string_bytes(b"Synthetic\0\xff");
        metadata.write_u64(0);
        metadata.write_string_bytes(b"1.0\xc3");
        guest.send(&metadata);
        let mut info = host.receive(IdRoomInformation);
        assert_eq!(info.read_string().as_deref(), Some("Synthetic room"));
        assert_eq!(info.read_string().as_deref(), Some(""));
        assert_eq!(info.read_u32(), Some(4));
        assert_eq!(info.read_u16(), Some(port));
        assert_eq!(info.read_string().as_deref(), Some(""));
        assert_eq!(info.read_string().as_deref(), Some("Host"));
        assert_eq!(info.read_u32(), Some(2));
        for (name, ip, game, version) in [
            ("Host", [192, 168, 1, 1], &b""[..], &b""[..]),
            ("Guest", guest_ip, &b"Synthetic\0\xff"[..], &b"1.0\xc3"[..]),
        ] {
            assert_eq!(info.read_string().as_deref(), Some(name));
            assert_eq!(info.read_array::<4>(), Some(ip));
            assert_eq!(info.read_string_bytes().as_deref(), Some(game));
            assert_eq!(info.read_u64(), Some(0));
            assert_eq!(info.read_string_bytes().as_deref(), Some(version));
            for _ in 0..3 {
                assert_eq!(info.read_string().as_deref(), Some(""));
            }
        }
        assert!(info.end_of_packet());

        let mut message = vec![b'a'; 499];
        message.extend_from_slice("é".as_bytes());
        let mut packet = Packet::new();
        packet.write_u8(IdChatMessage as u8);
        packet.write_string_bytes(&message);
        host.send(&packet);
        let mut received = guest.receive(IdChatMessage);
        assert_eq!(received.read_string().as_deref(), Some("Host"));
        assert_eq!(received.read_string().as_deref(), Some(""));
        assert_eq!(received.read_string_bytes().unwrap(), message[..500]);

        for kind in [IdLdnPacket, IdProxyPacket] {
            for broadcast in [false, true] {
                let mut packet = Packet::new();
                packet.write_u8(kind as u8);
                if kind == IdLdnPacket {
                    packet.write_u8(0);
                    packet.write_array(&[192, 168, 1, 1]);
                    packet.write_array(&guest_ip);
                } else {
                    packet.write_u8(2);
                    packet.write_array(&[192, 168, 1, 1]);
                    packet.write_u16(1234);
                    packet.write_u8(2);
                    packet.write_array(&guest_ip);
                    packet.write_u16(5678);
                    packet.write_u8(1);
                }
                packet.write_bool(broadcast);
                packet.write_vec_u8(&[0, 0xff, 3, 4]);
                host.send(&packet);
                assert_eq!(guest.receive(kind).get_data(), packet.get_data());
            }
        }
        room.destroy();
        host.receive(IdCloseRoom);
        guest.receive(IdCloseRoom);
    }

    #[test]
    fn server_moderation_updates_ban_lists_and_enforces_permissions() {
        use RoomMessageTypes::*;
        let (room, port) = local_room(4);
        let mut host = Client::connect(port);
        host.join("Host", NO_PREFERRED_IP, "secret", NETWORK_VERSION);
        host.receive(IdJoinSuccessAsMod);
        let mut guest = Client::connect(port);
        guest.join("Guest", NO_PREFERRED_IP, "secret", NETWORK_VERSION);
        guest.receive(IdJoinSuccess);
        guest.command(IdModBan, "Host");
        guest.receive(IdModPermissionDenied);
        host.command(IdModKick, "Missing");
        host.receive(IdModNoSuchUser);
        // Upstream flushes the notification then immediately disconnects.
        // ENet's disconnect resets pending receive queues on the client, so
        // observing that notification is not guaranteed. Assert removal and
        // the ban list, not delivery after the peer has already disconnected.
        host.command(IdModBan, "Guest");
        guest.disconnected();
        host.command(IdModGetBanList, "");
        let mut bans = host.receive(IdModBanListResponse);
        assert!(bans.read_vec_string().unwrap().is_empty());
        assert_eq!(bans.read_vec_string().unwrap(), ["127.0.0.1"]);
        host.command(IdModUnban, "127.0.0.1");
        host.command(IdModGetBanList, "");
        let mut bans = host.receive(IdModBanListResponse);
        assert!(bans.read_vec_string().unwrap().is_empty());
        assert!(bans.read_vec_string().unwrap().is_empty());
        assert!(room.get_ban_list().1.is_empty());
    }

    #[test]
    fn production_room_member_joins_leaves_and_rejoins_local_server() {
        use crate::room_member::{RoomMember, RoomMemberState};
        let (room, port) = local_room(4);
        let member = RoomMember::new();
        for _ in 0..3 {
            member.join("Host", "127.0.0.1", port, 0, &NO_PREFERRED_IP, "secret", "");
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while member.get_state() != RoomMemberState::Moderator {
                assert!(
                    std::time::Instant::now() < deadline,
                    "client failed to join: {:?}",
                    member.get_state()
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(member.get_room_information().name, "Synthetic room");
            member.leave();
            while !room.get_room_member_list().is_empty() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "server retained disconnected member"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        room.destroy();
    }

    #[test]
    fn test_room_default_state_is_closed() {
        let room = Room::new();
        assert_eq!(room.get_state(), RoomState::Closed);
    }

    #[test]
    fn test_room_create_and_destroy() {
        let room = Room::new();
        let result = room.create(
            "Test Room",
            "A test room",
            "127.0.0.1",
            0,
            "secret",
            4,
            "host",
            GameInfo::default(),
            None,
            &(vec![], vec![]),
            false,
        );
        assert!(result);
        assert_eq!(room.get_state(), RoomState::Open);
        assert!(room.has_password());

        let info = room.get_room_information();
        assert_eq!(info.name, "Test Room");
        assert_eq!(info.member_slots, 4);
        assert_eq!(info.port, 0);

        room.destroy();
        assert_eq!(room.get_state(), RoomState::Closed);
    }

    #[test]
    fn test_room_message_types_roundtrip() {
        assert_eq!(
            RoomMessageTypes::from_u8(RoomMessageTypes::IdJoinRequest as u8),
            Some(RoomMessageTypes::IdJoinRequest)
        );
        assert_eq!(
            RoomMessageTypes::from_u8(RoomMessageTypes::IdJoinSuccessAsMod as u8),
            Some(RoomMessageTypes::IdJoinSuccessAsMod)
        );
        assert_eq!(RoomMessageTypes::from_u8(0), None);
        assert_eq!(RoomMessageTypes::from_u8(255), None);
    }

    #[test]
    fn test_status_message_types_roundtrip() {
        assert_eq!(
            StatusMessageTypes::from_u8(1),
            Some(StatusMessageTypes::IdMemberJoin)
        );
        assert_eq!(
            StatusMessageTypes::from_u8(5),
            Some(StatusMessageTypes::IdAddressUnbanned)
        );
        assert_eq!(StatusMessageTypes::from_u8(0), None);
    }

    #[test]
    fn test_no_preferred_ip() {
        assert_eq!(NO_PREFERRED_IP, [0xFF, 0xFF, 0xFF, 0xFF]);
    }
}
