// SPDX-FileCopyrightText: Copyright 2022 yuzu Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/core/hle/service/ldn/ldn_types.h

pub const SSID_LENGTH_MAX: usize = 32;
pub const ADVERTISE_DATA_SIZE_MAX: usize = 384;
pub const USER_NAME_BYTES_MAX: usize = 32;
pub const NODE_COUNT_MAX: usize = 8;
pub const STATION_COUNT_MAX: usize = NODE_COUNT_MAX - 1;
pub const PASSPHRASE_LENGTH_MAX: usize = 64;

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityMode {
    All = 0,
    Retail = 1,
    Debug = 2,
}

impl Default for SecurityMode {
    fn default() -> Self {
        SecurityMode::All
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeStateChange {
    None = 0,
    Connect = 1,
    Disconnect = 2,
    DisconnectAndConnect = 3,
}

impl Default for NodeStateChange {
    fn default() -> Self {
        NodeStateChange::None
    }
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct ScanFilterFlag: u32 {
        const NONE = 0;
        const LOCAL_COMMUNICATION_ID = 1 << 0;
        const SESSION_ID = 1 << 1;
        const NETWORK_TYPE = 1 << 2;
        const SSID = 1 << 4;
        const SCENE_ID = 1 << 5;
        const INTENT_ID = Self::LOCAL_COMMUNICATION_ID.bits() | Self::SCENE_ID.bits();
        const NETWORK_ID = Self::INTENT_ID.bits() | Self::SESSION_ID.bits();
    }
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkType {
    None = 0,
    General = 1,
    Ldn = 2,
    All = 3,
}

impl Default for NetworkType {
    fn default() -> Self {
        NetworkType::None
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackedNetworkType {
    None = 0,
    General = 1,
    Ldn = 2,
    All = 3,
}

impl Default for PackedNetworkType {
    fn default() -> Self {
        PackedNetworkType::None
    }
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    None = 0,
    Initialized = 1,
    AccessPointOpened = 2,
    AccessPointCreated = 3,
    StationOpened = 4,
    StationConnected = 5,
    Error = 6,
}

impl Default for State {
    fn default() -> Self {
        State::None
    }
}

#[repr(i16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisconnectReason {
    Unknown = -1,
    None = 0,
    DisconnectedByUser = 1,
    DisconnectedBySystem = 2,
    DestroyedByUser = 3,
    DestroyedBySystem = 4,
    Rejected = 5,
    SignalLost = 6,
}

impl Default for DisconnectReason {
    fn default() -> Self {
        DisconnectReason::None
    }
}

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkError {
    Unknown = -1,
    None = 0,
    PortUnreachable = 1,
    TooManyPlayers = 2,
    VersionTooLow = 3,
    VersionTooHigh = 4,
    ConnectFailure = 5,
    ConnectNotFound = 6,
    ConnectTimeout = 7,
    ConnectRejected = 8,
    RejectFailed = 9,
}

impl Default for NetworkError {
    fn default() -> Self {
        NetworkError::None
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptPolicy {
    AcceptAll = 0,
    RejectAll = 1,
    BlackList = 2,
    WhiteList = 3,
}

impl Default for AcceptPolicy {
    fn default() -> Self {
        AcceptPolicy::AcceptAll
    }
}

#[repr(i16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WifiChannel {
    Default = 0,
    Wifi24_1 = 1,
    Wifi24_6 = 6,
    Wifi24_11 = 11,
    Wifi50_36 = 36,
    Wifi50_40 = 40,
    Wifi50_44 = 44,
    Wifi50_48 = 48,
}

impl Default for WifiChannel {
    fn default() -> Self {
        WifiChannel::Default
    }
}

#[repr(i8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkLevel {
    Bad = 0,
    Low = 1,
    Good = 2,
    Excellent = 3,
}

impl Default for LinkLevel {
    fn default() -> Self {
        LinkLevel::Bad
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeStatus {
    Disconnected = 0,
    Connected = 1,
}

impl Default for NodeStatus {
    fn default() -> Self {
        NodeStatus::Disconnected
    }
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WirelessControllerRestriction {
    None = 0,
    Default = 1,
}

impl Default for WirelessControllerRestriction {
    fn default() -> Self {
        WirelessControllerRestriction::None
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ConnectOption {
    pub raw: u32,
}
const _: () = assert!(std::mem::size_of::<ConnectOption>() == 0x4);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct NodeLatestUpdate {
    pub state_change: NodeStateChange,
    pub _padding: [u8; 7],
}
const _: () = assert!(std::mem::size_of::<NodeLatestUpdate>() == 0x8);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionId {
    pub high: u64,
    pub low: u64,
}
const _: () = assert!(std::mem::size_of::<SessionId>() == 0x10);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IntentId {
    pub local_communication_id: u64,
    pub _reserved0: [u8; 2],
    pub scene_id: u16,
    pub _reserved1: [u8; 4],
}
const _: () = assert!(std::mem::size_of::<IntentId>() == 0x10);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct NetworkId {
    pub intent_id: IntentId,
    pub session_id: SessionId,
}
const _: () = assert!(std::mem::size_of::<NetworkId>() == 0x20);

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ssid {
    pub length: u8,
    pub raw: [u8; SSID_LENGTH_MAX + 1],
}
const _: () = assert!(std::mem::size_of::<Ssid>() == 0x22);

impl Default for Ssid {
    fn default() -> Self {
        Self {
            length: 0,
            raw: [0u8; SSID_LENGTH_MAX + 1],
        }
    }
}

impl Ssid {
    pub fn from_str(data: &str) -> Self {
        let mut ssid = Self::default();
        let len = data.len().min(SSID_LENGTH_MAX);
        ssid.length = len as u8;
        ssid.raw[..len].copy_from_slice(&data.as_bytes()[..len]);
        ssid.raw[len] = 0;
        ssid
    }

    pub fn get_string_value(&self) -> String {
        let end = self
            .raw
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.raw.len());
        String::from_utf8_lossy(&self.raw[..end]).to_string()
    }
}

impl PartialEq for Ssid {
    fn eq(&self, other: &Self) -> bool {
        self.length == other.length
            && self.raw[..self.length as usize] == other.raw[..other.length as usize]
    }
}
impl Eq for Ssid {}

pub type Ipv4Address = [u8; 4];

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MacAddress {
    pub raw: [u8; 6],
}
const _: () = assert!(std::mem::size_of::<MacAddress>() == 0x6);

impl std::hash::Hash for MacAddress {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        let mut value: u64 = 0;
        unsafe {
            std::ptr::copy_nonoverlapping(self.raw.as_ptr(), &mut value as *mut u64 as *mut u8, 6);
        }
        value.hash(state);
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ScanFilter {
    pub network_id: NetworkId,
    pub network_type: NetworkType,
    pub mac_address: MacAddress,
    pub ssid: Ssid,
    pub _padding: [u8; 0x10],
    pub flag: ScanFilterFlag,
}
const _: () = assert!(std::mem::size_of::<ScanFilter>() == 0x60);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CommonNetworkInfo {
    pub bssid: MacAddress,
    pub ssid: Ssid,
    pub channel: WifiChannel,
    pub link_level: LinkLevel,
    pub network_type: PackedNetworkType,
    pub _padding: [u8; 4],
}
const _: () = assert!(std::mem::size_of::<CommonNetworkInfo>() == 0x30);

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NodeInfo {
    pub ipv4_address: Ipv4Address,
    pub mac_address: MacAddress,
    pub node_id: i8,
    pub is_connected: u8,
    pub user_name: [u8; USER_NAME_BYTES_MAX + 1],
    pub _reserved0: u8,
    pub local_communication_version: i16,
    pub _reserved1: [u8; 0x10],
}

impl Default for NodeInfo {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}
const _: () = assert!(std::mem::size_of::<NodeInfo>() == 0x40);

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct LdnNetworkInfo {
    pub security_parameter: [u8; 0x10],
    pub security_mode: SecurityMode,
    pub station_accept_policy: AcceptPolicy,
    pub has_action_frame: u8,
    pub _padding0: [u8; 2],
    pub node_count_max: u8,
    pub node_count: u8,
    pub nodes: [NodeInfo; NODE_COUNT_MAX],
    pub _reserved0: [u8; 2],
    pub advertise_data_size: u16,
    pub advertise_data: [u8; ADVERTISE_DATA_SIZE_MAX],
    pub _reserved1: [u8; 0x8C],
    pub random_authentication_id: u64,
}
const _: () = assert!(std::mem::size_of::<LdnNetworkInfo>() == 0x430);

impl Default for LdnNetworkInfo {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct NetworkInfo {
    pub network_id: NetworkId,
    pub common: CommonNetworkInfo,
    pub ldn: LdnNetworkInfo,
}
const _: () = assert!(std::mem::size_of::<NetworkInfo>() == 0x480);

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SecurityConfig {
    pub security_mode: SecurityMode,
    pub passphrase_size: u16,
    pub passphrase: [u8; PASSPHRASE_LENGTH_MAX],
}

impl Default for SecurityConfig {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}
const _: () = assert!(std::mem::size_of::<SecurityConfig>() == 0x44);

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct UserConfig {
    pub user_name: [u8; USER_NAME_BYTES_MAX + 1],
    pub _reserved: [u8; 0xF],
}
const _: () = assert!(std::mem::size_of::<UserConfig>() == 0x30);

impl Default for UserConfig {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

#[repr(C, packed(4))]
#[derive(Debug, Clone, Copy, Default)]
pub struct ConnectRequest {
    pub security_config: SecurityConfig,
    pub user_config: UserConfig,
    pub local_communication_version: u32,
    pub option_unknown: u32,
    pub network_info: NetworkInfo,
}
const _: () = assert!(std::mem::size_of::<ConnectRequest>() == 0x4FC);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SecurityParameter {
    pub data: [u8; 0x10],
    pub session_id: SessionId,
}
const _: () = assert!(std::mem::size_of::<SecurityParameter>() == 0x20);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct NetworkConfig {
    pub intent_id: IntentId,
    pub channel: WifiChannel,
    pub node_count_max: u8,
    pub _reserved0: u8,
    pub local_communication_version: u16,
    pub _reserved1: [u8; 0xA],
}
const _: () = assert!(std::mem::size_of::<NetworkConfig>() == 0x20);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct AddressEntry {
    pub ipv4_address: Ipv4Address,
    pub mac_address: MacAddress,
    pub _reserved: [u8; 2],
}
const _: () = assert!(std::mem::size_of::<AddressEntry>() == 0xC);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct AddressList {
    pub addresses: [AddressEntry; 8],
}
const _: () = assert!(std::mem::size_of::<AddressList>() == 0x60);

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GroupInfo {
    pub info: [u8; 0x200],
}

impl Default for GroupInfo {
    fn default() -> Self {
        Self { info: [0u8; 0x200] }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CreateNetworkConfig {
    pub security_config: SecurityConfig,
    pub user_config: UserConfig,
    pub _padding: [u8; 4],
    pub network_config: NetworkConfig,
}
const _: () = assert!(std::mem::size_of::<CreateNetworkConfig>() == 0x98);

#[repr(C, packed(4))]
#[derive(Debug, Clone, Copy, Default)]
pub struct CreateNetworkConfigPrivate {
    pub security_config: SecurityConfig,
    pub security_parameter: SecurityParameter,
    pub user_config: UserConfig,
    pub _padding: [u8; 4],
    pub network_config: NetworkConfig,
}
const _: () = assert!(std::mem::size_of::<CreateNetworkConfigPrivate>() == 0xB8);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ConnectNetworkData {
    pub security_config: SecurityConfig,
    pub user_config: UserConfig,
    pub local_communication_version: i32,
    pub option: ConnectOption,
}
const _: () = assert!(std::mem::size_of::<ConnectNetworkData>() == 0x7C);

// C++ memcpy accepts enum bit patterns which cannot be represented by Rust
// enums. Validate discriminants before constructing these guest wire values.
fn valid_security_mode(data: &[u8]) -> bool {
    data.get(..2)
        .is_some_and(|v| u16::from_le_bytes(v.try_into().unwrap()) <= 2)
}

fn valid_network_channel(data: &[u8]) -> bool {
    let offset = std::mem::offset_of!(NetworkConfig, channel);
    data.get(offset..offset + 2).is_some_and(|v| {
        matches!(
            i16::from_le_bytes(v.try_into().unwrap()),
            0 | 1 | 6 | 11 | 36 | 40 | 44 | 48
        )
    })
}

impl CreateNetworkConfig {
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < std::mem::size_of::<Self>()
            || !valid_security_mode(data)
            || !valid_network_channel(&data[std::mem::offset_of!(Self, network_config)..])
        {
            return None;
        }
        Some(unsafe { std::ptr::read_unaligned(data.as_ptr().cast::<Self>()) })
    }
}

impl CreateNetworkConfigPrivate {
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < std::mem::size_of::<Self>()
            || !valid_security_mode(data)
            || !valid_network_channel(&data[std::mem::offset_of!(Self, network_config)..])
        {
            return None;
        }
        Some(unsafe { std::ptr::read_unaligned(data.as_ptr().cast::<Self>()) })
    }
}

impl ConnectNetworkData {
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < std::mem::size_of::<Self>() || !valid_security_mode(data) {
            return None;
        }
        Some(unsafe { std::ptr::read_unaligned(data.as_ptr().cast::<Self>()) })
    }
}

impl ScanFilter {
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < std::mem::size_of::<Self>() {
            return None;
        }
        let offset = std::mem::offset_of!(Self, network_type);
        if u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) > 3
            || data[std::mem::offset_of!(Self, ssid)] as usize > SSID_LENGTH_MAX
        {
            return None;
        }
        Some(unsafe { std::ptr::read_unaligned(data.as_ptr().cast::<Self>()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, offset_of, size_of};

    #[test]
    fn guest_request_layouts_and_signed_version_are_preserved() {
        assert_eq!(offset_of!(CreateNetworkConfig, network_config), 0x78);
        assert_eq!(
            offset_of!(CreateNetworkConfigPrivate, security_parameter),
            0x44
        );
        assert_eq!(offset_of!(CreateNetworkConfigPrivate, network_config), 0x98);
        assert_eq!(align_of::<CreateNetworkConfigPrivate>(), 4);
        assert_eq!(offset_of!(NetworkInfo, common), 0x20);
        assert_eq!(offset_of!(NetworkInfo, ldn), 0x50);
        assert_eq!(offset_of!(LdnNetworkInfo, nodes), 0x18);
        assert_eq!(offset_of!(LdnNetworkInfo, random_authentication_id), 0x428);
        let mut bytes = vec![0; size_of::<ConnectNetworkData>() + 1];
        let version = 1 + offset_of!(ConnectNetworkData, local_communication_version);
        bytes[version..version + 4].copy_from_slice(&(-2i32).to_le_bytes());
        let decoded = ConnectNetworkData::from_bytes(&bytes[1..]).unwrap();
        assert_eq!(decoded.local_communication_version, -2);
        assert_eq!(decoded.local_communication_version as u16, 0xfffe);
    }

    #[test]
    fn guest_enum_payloads_are_checked_before_materializing_rust_values() {
        let mut public = vec![0; size_of::<CreateNetworkConfig>()];
        let mut private = vec![0; size_of::<CreateNetworkConfigPrivate>()];
        let mut connect = vec![0; size_of::<ConnectNetworkData>()];
        let mut filter = vec![0; size_of::<ScanFilter>()];
        assert!(CreateNetworkConfig::from_bytes(&public).is_some());
        assert!(CreateNetworkConfigPrivate::from_bytes(&private).is_some());
        assert!(ConnectNetworkData::from_bytes(&connect).is_some());
        assert!(ScanFilter::from_bytes(&filter).is_some());
        public[0] = 3;
        private[0] = 3;
        connect[0] = 3;
        filter[offset_of!(ScanFilter, network_type)] = 4;
        assert!(CreateNetworkConfig::from_bytes(&public).is_none());
        assert!(CreateNetworkConfigPrivate::from_bytes(&private).is_none());
        assert!(ConnectNetworkData::from_bytes(&connect).is_none());
        assert!(ScanFilter::from_bytes(&filter).is_none());
        public[0] = 0;
        private[0] = 0;
        filter[offset_of!(ScanFilter, network_type)] = 0;
        public[offset_of!(CreateNetworkConfig, network_config)
            + offset_of!(NetworkConfig, channel)] = 2;
        private[offset_of!(CreateNetworkConfigPrivate, network_config)
            + offset_of!(NetworkConfig, channel)] = 2;
        filter[offset_of!(ScanFilter, ssid)] = 255;
        assert!(CreateNetworkConfig::from_bytes(&public).is_none());
        assert!(CreateNetworkConfigPrivate::from_bytes(&private).is_none());
        assert!(ScanFilter::from_bytes(&filter).is_none());
        assert!(CreateNetworkConfig::from_bytes(&[]).is_none());
        assert!(CreateNetworkConfigPrivate::from_bytes(&[]).is_none());
        assert!(ConnectNetworkData::from_bytes(&[]).is_none());
        assert!(ScanFilter::from_bytes(&[]).is_none());
    }
}
