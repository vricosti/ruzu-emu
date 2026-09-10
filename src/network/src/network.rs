// SPDX-FileCopyrightText: Copyright 2017 Citra Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/network/network.h and network.cpp
//!
//! Provides the `RoomNetwork` struct which owns and coordinates the Room
//! (server) and RoomMember (client) for network games.

use std::sync::{Arc, LazyLock, Mutex, Weak};

use crate::room::{Room, RoomState};
use crate::room_member::RoomMember;

#[derive(Default)]
struct NetworkState {
    room_member: Option<Arc<RoomMember>>,
    room: Option<Arc<Room>>,
}

static NETWORK_STATE: LazyLock<Mutex<NetworkState>> =
    LazyLock::new(|| Mutex::new(NetworkState::default()));

fn create_network_state() -> (Arc<Room>, Arc<RoomMember>) {
    (Arc::new(Room::new()), Arc::new(RoomMember::new()))
}

fn install_network_state(room: Arc<Room>, room_member: Arc<RoomMember>) {
    *NETWORK_STATE.lock().unwrap() = NetworkState {
        room: Some(room),
        room_member: Some(room_member),
    };
}

/// Initializes the process-global room and room-member owners.
///
/// Maps to upstream `Network::Init`.
pub fn init() -> bool {
    let (room, room_member) = create_network_state();
    install_network_state(room, room_member);
    log::debug!("initialized OK");
    true
}

/// Returns the process-global room handle.
///
/// Maps to upstream `Network::GetRoom`.
pub fn get_room() -> Weak<Room> {
    NETWORK_STATE
        .lock()
        .unwrap()
        .room
        .as_ref()
        .map(Arc::downgrade)
        .unwrap_or_default()
}

/// Returns the process-global room-member handle.
///
/// Maps to upstream `Network::GetRoomMember`.
pub fn get_room_member() -> Weak<RoomMember> {
    NETWORK_STATE
        .lock()
        .unwrap()
        .room_member
        .as_ref()
        .map(Arc::downgrade)
        .unwrap_or_default()
}

/// Tears down the process-global network owners.
///
/// Maps to upstream `Network::Shutdown`.
pub fn shutdown() {
    let (room_member, room) = {
        let mut state = NETWORK_STATE.lock().unwrap();
        (state.room_member.take(), state.room.take())
    };

    if let Some(room_member) = room_member {
        if room_member.is_connected() {
            room_member.leave();
        }
    }
    if let Some(room) = room {
        if room.get_state() == RoomState::Open {
            room.destroy();
        }
    }
    log::debug!("shutdown OK");
}

/// Owns the Room and RoomMember handles for the network subsystem.
/// Maps to C++ `Network::RoomNetwork`.
pub struct RoomNetwork {
    /// RoomMember (Client) for network games.
    m_room_member: Arc<RoomMember>,
    /// Room (Server) for network games.
    m_room: Arc<Room>,
}

impl RoomNetwork {
    pub fn new() -> Self {
        let (m_room, m_room_member) = create_network_state();
        install_network_state(Arc::clone(&m_room), Arc::clone(&m_room_member));
        Self {
            m_room_member,
            m_room,
        }
    }

    /// Initializes and registers the network device, the room, and the room
    /// member.
    ///
    /// NOTE: `enet_initialize()` is not ported; ENet is not used in Rust.
    /// This method re-creates the Room and RoomMember instances.
    pub fn init(&mut self) -> bool {
        // NOTE: enet_initialize() call omitted; no ENet in Rust port.
        self.m_room = Arc::new(Room::new());
        self.m_room_member = Arc::new(RoomMember::new());
        install_network_state(Arc::clone(&self.m_room), Arc::clone(&self.m_room_member));
        log::debug!("initialized OK");
        true
    }

    /// Returns a weak pointer to the room handle.
    pub fn get_room(&self) -> Weak<Room> {
        Arc::downgrade(&self.m_room)
    }

    /// Returns a weak pointer to the room member handle.
    pub fn get_room_member(&self) -> Weak<RoomMember> {
        Arc::downgrade(&self.m_room_member)
    }

    /// Unregisters the network device, the room, and the room member and shuts
    /// them down.
    pub fn shutdown(&mut self) {
        shutdown();
    }
}

impl Default for RoomNetwork {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_room_network_init_and_shutdown() {
        // Other tests legitimately install their own global RoomNetwork.
        // Verify global identity/teardown in a process with no competing test.
        const CHILD: &str = "RUZU_TEST_NETWORK_GLOBAL_LIFETIME";
        if std::env::var_os(CHILD).is_none() {
            assert!(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "network::tests::test_room_network_init_and_shutdown"])
                .env(CHILD, "1")
                .status().unwrap().success());
            return;
        }
        let mut rn = RoomNetwork::new();
        assert!(rn.init());
        let room = rn.get_room().upgrade().unwrap();
        let room_member = rn.get_room_member().upgrade().unwrap();
        assert!(Arc::ptr_eq(&room, &get_room().upgrade().unwrap()));
        assert!(Arc::ptr_eq(
            &room_member,
            &get_room_member().upgrade().unwrap()
        ));
        rn.shutdown();
        assert!(get_room().upgrade().is_none());
        assert!(get_room_member().upgrade().is_none());
    }
}
