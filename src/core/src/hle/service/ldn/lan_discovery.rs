// SPDX-FileCopyrightText: Copyright 2022 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/ldn/lan_discovery.h
//! Port of zuyu/src/core/hle/service/ldn/lan_discovery.cpp
//!
//! LANDiscovery: manages LAN-based local communication discovery, network creation,
//! scanning, and station management.
//!
//! Packet transport uses the shared RoomMember. The service owns this backend
//! through Arc<Mutex<_>>; scan releases that ownership during the reply window.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::ldn_results::{
    RESULT_ACCESS_POINT_CONNECTION_FAILED, RESULT_ADVERTISE_DATA_TOO_LARGE, RESULT_BAD_STATE,
    RESULT_CONNECTION_FAILED, RESULT_INVALID_BUFFER_COUNT, RESULT_INVALID_NODE_COUNT,
    RESULT_NO_IP_ADDRESS,
};
use super::ldn_types::*;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use network::room_member::{LdnPacket, LdnPacketType};

/// Fake SSID used for LAN discovery (matches upstream `fake_ssid`).
pub const FAKE_SSID: &str = "YuzuFakeSsidForLdn";

/// LanStation represents a single station connected to the LAN network.
/// Rust uses node_id to index the parent's nodes instead of retaining C++'s
/// self-referential node_info/discovery pointers. UpdateNodes applies OverrideInfo,
/// and ReceivePacket performs OnClose's Reset followed by the parent update.
pub struct LanStation {
    pub node_id: i8,
    pub status: NodeStatus,
}

impl LanStation {
    pub fn new(node_id: i8) -> Self {
        Self {
            node_id,
            status: NodeStatus::Disconnected,
        }
    }

    pub fn reset(&mut self) {
        self.status = NodeStatus::Disconnected;
    }

    pub fn get_status(&self) -> NodeStatus {
        self.status
    }
}

/// LANDiscovery manages the state machine for local area network communication.
pub struct LANDiscovery {
    inited: bool,
    packet_mutex: Arc<Mutex<()>>,
    stations: [LanStation; STATION_COUNT_MAX],
    node_changes: [NodeLatestUpdate; NODE_COUNT_MAX],
    node_last_states: [u8; NODE_COUNT_MAX],
    scan_results: HashMap<MacAddress, NetworkInfo>,
    node_info: NodeInfo,
    network_info: NetworkInfo,
    state: State,
    disconnect_reason: DisconnectReason,
    connected_clients: Vec<Ipv4Address>,
    host_ip: Option<Ipv4Address>,
    lan_event: Arc<dyn Fn() + Send + Sync>,
}

impl LANDiscovery {
    pub fn new() -> Self {
        Self {
            inited: false,
            packet_mutex: Arc::new(Mutex::new(())),
            stations: core::array::from_fn(|i| LanStation::new((i + 1) as i8)),
            node_changes: [NodeLatestUpdate::default(); NODE_COUNT_MAX],
            node_last_states: [0u8; NODE_COUNT_MAX],
            scan_results: HashMap::new(),
            node_info: NodeInfo::default(),
            network_info: NetworkInfo::default(),
            state: State::None,
            disconnect_reason: DisconnectReason::None,
            connected_clients: Vec::new(),
            host_ip: None,
            lan_event: Arc::new(|| {}),
        }
    }

    pub fn get_state(&self) -> State {
        self.state
    }

    pub fn set_state(&mut self, new_state: State) {
        self.state = new_state;
    }

    pub fn get_disconnect_reason(&self) -> DisconnectReason {
        self.disconnect_reason
    }

    pub fn get_network_info(&self) -> Result<NetworkInfo, ResultCode> {
        if matches!(
            self.state,
            State::AccessPointCreated | State::StationConnected
        ) {
            Ok(self.network_info)
        } else {
            Err(RESULT_BAD_STATE)
        }
    }

    pub fn get_network_info_latest_update(
        &mut self,
        update_count: usize,
    ) -> Result<(NetworkInfo, Vec<NodeLatestUpdate>), ResultCode> {
        if update_count > NODE_COUNT_MAX {
            return Err(RESULT_INVALID_BUFFER_COUNT);
        }
        if !matches!(
            self.state,
            State::AccessPointCreated | State::StationConnected
        ) {
            return Err(RESULT_BAD_STATE);
        }

        let mut updates = Vec::with_capacity(update_count);
        for node_update in self.node_changes.iter_mut().take(update_count) {
            updates.push(*node_update);
            node_update.state_change = NodeStateChange::None;
        }
        Ok((self.network_info, updates))
    }

    pub fn open_access_point(&mut self) -> ResultCode {
        let packet_mutex = Arc::clone(&self.packet_mutex);
        let _lock = packet_mutex.lock().unwrap();
        self.disconnect_reason = DisconnectReason::None;
        if self.state == State::None {
            return RESULT_BAD_STATE;
        }
        self.reset_stations();
        self.state = State::AccessPointOpened;
        RESULT_SUCCESS
    }

    pub fn close_access_point(&mut self) -> ResultCode {
        let packet_mutex = Arc::clone(&self.packet_mutex);
        let _lock = packet_mutex.lock().unwrap();
        if self.state == State::None {
            return RESULT_BAD_STATE;
        }
        if self.state == State::AccessPointCreated {
            self.destroy_network_impl();
        }
        self.reset_stations();
        self.state = State::Initialized;
        RESULT_SUCCESS
    }

    pub fn open_station(&mut self) -> ResultCode {
        let packet_mutex = Arc::clone(&self.packet_mutex);
        let _lock = packet_mutex.lock().unwrap();
        self.disconnect_reason = DisconnectReason::None;
        if self.state == State::None {
            return RESULT_BAD_STATE;
        }
        self.reset_stations();
        self.state = State::StationOpened;
        RESULT_SUCCESS
    }

    pub fn close_station(&mut self) -> ResultCode {
        let packet_mutex = Arc::clone(&self.packet_mutex);
        let _lock = packet_mutex.lock().unwrap();
        if self.state == State::None {
            return RESULT_BAD_STATE;
        }
        if self.state == State::StationConnected {
            self.disconnect_impl();
        }
        self.reset_stations();
        self.state = State::Initialized;
        RESULT_SUCCESS
    }

    pub fn create_network(
        &mut self,
        security_config: &SecurityConfig,
        user_config: &UserConfig,
        network_config: &NetworkConfig,
    ) -> ResultCode {
        let packet_mutex = Arc::clone(&self.packet_mutex);
        let _lock = packet_mutex.lock().unwrap();
        if self.state != State::AccessPointOpened {
            return RESULT_BAD_STATE;
        }

        self.init_network_info();
        self.network_info.ldn.node_count_max = network_config.node_count_max;
        self.network_info.ldn.security_mode = security_config.security_mode;
        self.network_info.common.channel = if network_config.channel == WifiChannel::Default {
            WifiChannel::Wifi24_6
        } else {
            network_config.channel
        };

        let mut random = common::random::Mt19937::new(5489);
        let mut next_u64 = || (u64::from(random.next_u32()) << 32) | u64::from(random.next_u32());
        self.network_info.network_id.session_id.high = next_u64();
        self.network_info.network_id.session_id.low = next_u64();
        self.network_info.network_id.intent_id = network_config.intent_id;

        // Upstream updates node0 in place; retain its reserved bytes as well.
        let mut node = self.network_info.ldn.nodes[0];
        if self.get_node_info(
            &mut node,
            user_config,
            network_config.local_communication_version,
        ) != RESULT_SUCCESS
        {
            return RESULT_ACCESS_POINT_CONNECTION_FAILED;
        }
        self.network_info.ldn.nodes[0] = node;
        self.state = State::AccessPointCreated;
        self.init_node_state_change();
        self.network_info.ldn.nodes[0].is_connected = 1;
        self.update_nodes();
        RESULT_SUCCESS
    }

    pub fn destroy_network(&mut self) -> ResultCode {
        self.destroy_network_impl()
    }

    fn destroy_network_impl(&mut self) -> ResultCode {
        for client_ip in self.connected_clients.clone() {
            self.send_packet(LdnPacketType::DestroyNetwork, client_ip);
        }
        self.reset_stations();
        self.state = State::AccessPointOpened;
        (self.lan_event)();
        RESULT_SUCCESS
    }

    pub fn connect(
        &mut self,
        network_info: &NetworkInfo,
        user_config: &UserConfig,
        local_communication_version: u16,
    ) -> ResultCode {
        let packet_mutex = Arc::clone(&self.packet_mutex);
        let _lock = packet_mutex.lock().unwrap();
        if network_info.ldn.node_count == 0 {
            return RESULT_INVALID_NODE_COUNT;
        }

        let mut node_info = self.node_info;
        if self.get_node_info(&mut node_info, user_config, local_communication_version)
            != RESULT_SUCCESS
        {
            return RESULT_CONNECTION_FAILED;
        }
        self.node_info = node_info;

        let mut host_ip = network_info.ldn.nodes[0].ipv4_address;
        host_ip.reverse();
        self.host_ip = Some(host_ip);
        self.send_packet_with_data(LdnPacketType::Connect, &node_info, host_ip);
        self.init_node_state_change();
        std::thread::sleep(Duration::from_secs(1));
        RESULT_SUCCESS
    }

    pub fn disconnect(&mut self) -> ResultCode {
        self.disconnect_impl()
    }

    fn disconnect_impl(&mut self) -> ResultCode {
        if let Some(host_ip) = self.host_ip {
            self.send_packet_with_data(LdnPacketType::Disconnect, &self.node_info, host_ip);
        }
        self.state = State::StationOpened;
        (self.lan_event)();
        RESULT_SUCCESS
    }

    pub fn initialize(
        &mut self,
        lan_event: Arc<dyn Fn() + Send + Sync>,
        _listening: bool,
    ) -> ResultCode {
        let packet_mutex = Arc::clone(&self.packet_mutex);
        let _lock = packet_mutex.lock().unwrap();
        if self.inited {
            return RESULT_SUCCESS;
        }
        self.reset_stations();
        self.lan_event = lan_event;
        self.state = State::Initialized;
        self.inited = true;
        RESULT_SUCCESS
    }

    pub fn finalize(&mut self) -> ResultCode {
        let packet_mutex = Arc::clone(&self.packet_mutex);
        let _lock = packet_mutex.lock().unwrap();
        if self.inited {
            if self.state == State::AccessPointCreated {
                self.destroy_network_impl();
            }
            if self.state == State::StationConnected {
                self.disconnect_impl();
            }
            self.reset_stations();
            self.inited = false;
        }
        self.state = State::None;
        RESULT_SUCCESS
    }

    pub fn scan(discovery: &Mutex<Self>, filter: &ScanFilter, capacity: usize) -> Vec<NetworkInfo> {
        {
            let mut discovery = discovery.lock().unwrap();
            let packet_mutex = Arc::clone(&discovery.packet_mutex);
            let _lock = packet_mutex.lock().unwrap();
            discovery.scan_results.clear();
            discovery.send_broadcast(LdnPacketType::Scan);
        }
        log::info!("Waiting for scan replies");
        std::thread::sleep(Duration::from_secs(1));

        let discovery = discovery.lock().unwrap();
        let packet_mutex = Arc::clone(&discovery.packet_mutex);
        let _lock = packet_mutex.lock().unwrap();
        discovery
            .scan_results
            .values()
            .filter(|info| {
                if Self::is_flag_set(filter.flag, ScanFilterFlag::LOCAL_COMMUNICATION_ID)
                    && filter.network_id.intent_id.local_communication_id
                        != info.network_id.intent_id.local_communication_id
                {
                    return false;
                }
                if Self::is_flag_set(filter.flag, ScanFilterFlag::SESSION_ID)
                    && filter.network_id.session_id != info.network_id.session_id
                {
                    return false;
                }
                if Self::is_flag_set(filter.flag, ScanFilterFlag::NETWORK_TYPE)
                    && filter.network_type as u32 != info.common.network_type as u32
                {
                    return false;
                }
                if Self::is_flag_set(filter.flag, ScanFilterFlag::SSID)
                    && filter.ssid != info.common.ssid
                {
                    return false;
                }
                if Self::is_flag_set(filter.flag, ScanFilterFlag::SCENE_ID)
                    && filter.network_id.intent_id.scene_id != info.network_id.intent_id.scene_id
                {
                    return false;
                }
                true
            })
            // Upstream compares its signed s16 count to a cast of the capacity.
            .take((capacity as i16).max(0) as usize)
            .copied()
            .collect()
    }

    pub fn set_advertise_data(&mut self, data: &[u8]) -> ResultCode {
        let packet_mutex = Arc::clone(&self.packet_mutex);
        let _lock = packet_mutex.lock().unwrap();
        if data.len() > ADVERTISE_DATA_SIZE_MAX {
            return RESULT_ADVERTISE_DATA_TOO_LARGE;
        }
        self.network_info.ldn.advertise_data[..data.len()].copy_from_slice(data);
        self.network_info.ldn.advertise_data_size = data.len() as u16;
        self.update_nodes();
        RESULT_SUCCESS
    }

    fn reset_stations(&mut self) {
        for station in self.stations.iter_mut() {
            station.reset();
        }
        self.connected_clients.clear();
    }

    fn init_node_state_change(&mut self) {
        for change in self.node_changes.iter_mut() {
            *change = NodeLatestUpdate::default();
        }
        for state in self.node_last_states.iter_mut() {
            *state = 0;
        }
    }

    fn init_network_info(&mut self) {
        self.network_info.common.bssid = self.get_fake_mac();
        self.network_info.common.channel = WifiChannel::Wifi24_6;
        self.network_info.common.link_level = LinkLevel::Good;
        self.network_info.common.network_type = PackedNetworkType::Ldn;
        self.network_info.common.ssid = Ssid::from_str(FAKE_SSID);
        for (index, node) in self.network_info.ldn.nodes.iter_mut().enumerate() {
            node.node_id = index as i8;
            node.is_connected = 0;
        }
    }

    fn update_nodes(&mut self) {
        let mut count = 0;
        for station in &self.stations {
            let connected = station.get_status() == NodeStatus::Connected;
            if connected {
                count += 1;
            }
            let node = &mut self.network_info.ldn.nodes[station.node_id as usize];
            node.node_id = station.node_id;
            node.is_connected = u8::from(connected);
        }
        self.network_info.ldn.node_count = count + 1;
        for local_ip in self.connected_clients.clone() {
            self.send_packet_with_data(LdnPacketType::SyncNetwork, &self.network_info, local_ip);
        }
        self.on_network_info_changed();
    }

    fn on_sync_network(&mut self, info: NetworkInfo) {
        self.network_info = info;
        if self.state == State::StationOpened {
            self.state = State::StationConnected;
        }
        self.on_network_info_changed();
    }

    fn on_disconnect_from_host(&mut self) {
        log::info!("OnDisconnectFromHost state: {:?}", self.state);
        self.host_ip = None;
        if self.state == State::StationConnected {
            self.state = State::StationOpened;
            (self.lan_event)();
        }
    }

    fn on_network_info_changed(&mut self) {
        if self.is_node_state_changed() {
            (self.lan_event)();
        }
    }

    fn get_local_ip(&self) -> Ipv4Address {
        network::network::get_room_member()
            .upgrade()
            .filter(|member| member.is_connected())
            .map(|member| member.get_fake_ip_address())
            .unwrap_or([0xFF; 4])
    }

    fn send_packet_with_data<T: Copy>(
        &self,
        packet_type: LdnPacketType,
        data: &T,
        remote_ip: Ipv4Address,
    ) {
        let bytes = unsafe {
            std::slice::from_raw_parts(data as *const T as *const u8, std::mem::size_of::<T>())
        };
        self.send_ldn_packet(LdnPacket {
            packet_type,
            broadcast: false,
            local_ip: self.get_local_ip(),
            remote_ip,
            data: bytes.to_vec(),
        });
    }

    fn send_packet(&self, packet_type: LdnPacketType, remote_ip: Ipv4Address) {
        self.send_ldn_packet(LdnPacket {
            packet_type,
            broadcast: false,
            local_ip: self.get_local_ip(),
            remote_ip,
            data: Vec::new(),
        });
    }

    fn send_broadcast(&self, packet_type: LdnPacketType) {
        self.send_ldn_packet(LdnPacket {
            packet_type,
            broadcast: true,
            local_ip: self.get_local_ip(),
            remote_ip: [0; 4],
            data: Vec::new(),
        });
    }

    fn send_ldn_packet(&self, packet: LdnPacket) {
        if let Some(room_member) = network::network::get_room_member().upgrade() {
            if room_member.is_connected() {
                room_member.send_ldn_packet(&packet);
            }
        }
    }

    pub fn receive_packet(&mut self, packet: &LdnPacket) {
        let packet_mutex = Arc::clone(&self.packet_mutex);
        let _lock = packet_mutex.lock().unwrap();
        match packet.packet_type {
            LdnPacketType::Scan => {
                if self.state == State::AccessPointCreated {
                    self.send_packet_with_data(
                        LdnPacketType::ScanResp,
                        &self.network_info,
                        packet.local_ip,
                    );
                }
            }
            LdnPacketType::ScanResp => {
                if let Some(info) = read_network_info(&packet.data) {
                    self.scan_results.entry(info.common.bssid).or_insert(info);
                }
            }
            LdnPacketType::Connect => {
                if let Some(info) = read_node_info(&packet.data) {
                    self.connected_clients.push(packet.local_ip);
                    if let Some(station) = self
                        .stations
                        .iter_mut()
                        .find(|station| station.status != NodeStatus::Connected)
                    {
                        self.network_info.ldn.nodes[station.node_id as usize] = info;
                        station.status = NodeStatus::Connected;
                    }
                    self.update_nodes();
                }
            }
            LdnPacketType::Disconnect => {
                self.connected_clients.retain(|ip| *ip != packet.local_ip);
                if let Some(info) = read_node_info(&packet.data) {
                    if let Some(station) = self.stations.iter_mut().find(|station| {
                        station.status == NodeStatus::Connected
                            && self.network_info.ldn.nodes[station.node_id as usize].mac_address
                                == info.mac_address
                    }) {
                        station.reset();
                        self.update_nodes();
                    }
                }
            }
            LdnPacketType::DestroyNetwork => {
                self.reset_stations();
                self.on_disconnect_from_host();
            }
            LdnPacketType::SyncNetwork => {
                if matches!(self.state, State::StationOpened | State::StationConnected) {
                    if let Some(info) = read_network_info(&packet.data) {
                        self.on_sync_network(info);
                    }
                }
            }
        }
    }

    fn is_node_state_changed(&mut self) -> bool {
        let mut changed = false;
        for index in 0..NODE_COUNT_MAX {
            let connected = self.network_info.ldn.nodes[index].is_connected;
            if connected != self.node_last_states[index] {
                let change = if connected != 0 {
                    NodeStateChange::Connect
                } else {
                    NodeStateChange::Disconnect
                };
                self.node_changes[index].state_change =
                    match self.node_changes[index].state_change as u8 | change as u8 {
                        1 => NodeStateChange::Connect,
                        2 => NodeStateChange::Disconnect,
                        3 => NodeStateChange::DisconnectAndConnect,
                        _ => NodeStateChange::None,
                    };
                self.node_last_states[index] = connected;
                changed = true;
            }
        }
        changed
    }

    fn is_flag_set(flag: ScanFilterFlag, search_flag: ScanFilterFlag) -> bool {
        (flag.bits() & search_flag.bits()) == search_flag.bits()
    }

    fn get_fake_mac(&self) -> MacAddress {
        let ip = self.get_local_ip();
        MacAddress {
            raw: [0x02, 0x00, ip[0], ip[1], ip[2], ip[3]],
        }
    }

    fn get_node_info(
        &self,
        node: &mut NodeInfo,
        user_config: &UserConfig,
        local_communication_version: u16,
    ) -> ResultCode {
        if crate::internal_network::network_interface::get_selected_network_interface().is_none() {
            log::error!("No network interface available");
            return RESULT_NO_IP_ADDRESS;
        }
        node.mac_address = self.get_fake_mac();
        node.is_connected = 1;
        node.user_name.copy_from_slice(&user_config.user_name);
        node.local_communication_version = local_communication_version as i16;
        let mut current_address = self.get_local_ip();
        current_address.reverse();
        node.ipv4_address = current_address;
        RESULT_SUCCESS
    }
}

impl Drop for LANDiscovery {
    fn drop(&mut self) {
        if self.inited {
            let result = self.finalize();
            log::info!("Finalize: {}", result.get_inner_value());
        }
    }
}

fn read_copy_payload<T: Copy>(data: &[u8]) -> Option<T> {
    if data.len() < std::mem::size_of::<T>() {
        return None;
    }
    Some(unsafe { std::ptr::read_unaligned(data.as_ptr() as *const T) })
}

fn read_node_info(data: &[u8]) -> Option<NodeInfo> {
    // `NodeInfo` contains only integer and byte-array fields, so every bit
    // pattern is a valid Rust value.
    read_copy_payload(data)
}

pub(super) fn read_network_info(data: &[u8]) -> Option<NetworkInfo> {
    if data.len() < std::mem::size_of::<NetworkInfo>() {
        return None;
    }

    let common = std::mem::offset_of!(NetworkInfo, common);
    let ldn = std::mem::offset_of!(NetworkInfo, ldn);
    // Reject lengths which would make Ssid's comparison read outside its array.
    if data[common + std::mem::offset_of!(CommonNetworkInfo, ssid)] as usize > SSID_LENGTH_MAX {
        return None;
    }
    let channel_offset = common + std::mem::offset_of!(CommonNetworkInfo, channel);
    let link_level_offset = common + std::mem::offset_of!(CommonNetworkInfo, link_level);
    let network_type_offset = common + std::mem::offset_of!(CommonNetworkInfo, network_type);
    let security_mode_offset = ldn + std::mem::offset_of!(LdnNetworkInfo, security_mode);
    let accept_policy_offset = ldn + std::mem::offset_of!(LdnNetworkInfo, station_accept_policy);

    let channel = i16::from_ne_bytes(data[channel_offset..channel_offset + 2].try_into().ok()?);
    if !matches!(channel, 0 | 1 | 6 | 11 | 36 | 40 | 44 | 48) {
        return None;
    }
    if !matches!(data[link_level_offset] as i8, 0..=3)
        || !matches!(data[network_type_offset], 0..=3)
    {
        return None;
    }
    let security_mode = u16::from_ne_bytes(
        data[security_mode_offset..security_mode_offset + 2]
            .try_into()
            .ok()?,
    );
    if security_mode > 2 || data[accept_policy_offset] > 3 {
        return None;
    }

    read_copy_payload(data)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn scan_receives_packets_during_wait_and_retains_first_response() {
        let discovery = Arc::new(Mutex::new(LANDiscovery::new()));
        let sentinel = MacAddress { raw: [0xff; 6] };
        discovery
            .lock()
            .unwrap()
            .scan_results
            .insert(sentinel, NetworkInfo::default());
        let worker = discovery.clone();
        let scan =
            std::thread::spawn(move || LANDiscovery::scan(&worker, &ScanFilter::default(), 1));
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            if let Ok(mut guard) = discovery.try_lock() {
                if !guard.scan_results.contains_key(&sentinel) {
                    let mut info = NetworkInfo::default();
                    info.ldn.node_count = 2;
                    let payload = |info: &NetworkInfo| unsafe {
                        std::slice::from_raw_parts(
                            (info as *const NetworkInfo).cast(),
                            std::mem::size_of::<NetworkInfo>(),
                        )
                        .to_vec()
                    };
                    let mut packet = LdnPacket {
                        packet_type: LdnPacketType::ScanResp,
                        broadcast: false,
                        local_ip: [0; 4],
                        remote_ip: [0; 4],
                        data: payload(&info),
                    };
                    guard.receive_packet(&packet);
                    info.ldn.node_count = 3;
                    packet.data = payload(&info);
                    guard.receive_packet(&packet);
                    break;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "scan did not release discovery"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        let results = scan.join().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].ldn.node_count, 2);
    }

    #[test]
    fn station_ids_match_the_upstream_one_through_seven_range() {
        let discovery = LANDiscovery::new();
        assert_eq!(
            discovery
                .stations
                .iter()
                .map(|station| station.node_id)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5, 6, 7]
        );
    }

    #[test]
    fn node_updates_accumulate_until_the_latest_update_is_read() {
        let event_count = Arc::new(AtomicUsize::new(0));
        let event_count_for_callback = Arc::clone(&event_count);
        let mut discovery = LANDiscovery::new();
        discovery.initialize(
            Arc::new(move || {
                event_count_for_callback.fetch_add(1, Ordering::Relaxed);
            }),
            true,
        );
        discovery.state = State::AccessPointCreated;
        discovery.network_info.ldn.nodes[0].is_connected = 1;
        discovery.on_network_info_changed();
        discovery.network_info.ldn.nodes[0].is_connected = 0;
        discovery.on_network_info_changed();

        assert_eq!(event_count.load(Ordering::Relaxed), 2);
        let (_, updates) = discovery
            .get_network_info_latest_update(NODE_COUNT_MAX)
            .unwrap();
        assert_eq!(
            updates[0].state_change,
            NodeStateChange::DisconnectAndConnect
        );
        assert_eq!(
            discovery.node_changes[0].state_change,
            NodeStateChange::None
        );
    }

    #[test]
    fn network_packet_reader_rejects_invalid_enum_discriminants() {
        let info = NetworkInfo::default();
        let bytes = unsafe {
            std::slice::from_raw_parts(
                &info as *const NetworkInfo as *const u8,
                std::mem::size_of::<NetworkInfo>(),
            )
        };
        let mut invalid = bytes.to_vec();
        let network_type_offset = std::mem::offset_of!(NetworkInfo, common)
            + std::mem::offset_of!(CommonNetworkInfo, network_type);
        invalid[network_type_offset] = 0xFF;

        assert!(read_network_info(bytes).is_some());
        assert!(read_network_info(&invalid).is_none());
        assert!(read_network_info(&invalid[..0x20]).is_none());
        let mut invalid = bytes.to_vec();
        invalid[std::mem::offset_of!(NetworkInfo, common)
            + std::mem::offset_of!(CommonNetworkInfo, ssid)] = 255;
        assert!(read_network_info(&invalid).is_none());
    }
}
