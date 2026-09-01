// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/nifm/nifm.cpp
//!
//! Network Interface Manager services.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::hle::result::{ErrorModule, ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{
    HLERequestContext, SessionRequestHandler, SessionRequestHandlerPtr,
};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};
use crate::internal_network::emu_net_state::{refresh_from_host, EmuNetState};
use crate::internal_network::network::{get_host_ipv4_address, translate_ipv4};
use crate::internal_network::network_interface::get_selected_network_interface;

/// Upstream: `ResultPendingConnection{ErrorModule::NIFM, 111}`
pub const RESULT_PENDING_CONNECTION: ResultCode =
    ResultCode::from_module_description(ErrorModule::NIFM, 111);

/// Upstream: `ResultNetworkCommunicationDisabled{ErrorModule::NIFM, 1111}`
pub const RESULT_NETWORK_COMMUNICATION_DISABLED: ResultCode =
    ResultCode::from_module_description(ErrorModule::NIFM, 1111);

/// RequestState enum. Upstream: `RequestState` in `nifm.cpp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum RequestState {
    NotSubmitted = 1,
    // Invalid = 1, // intentional duplicate in upstream
    OnHold = 2,
    Accepted = 3,
    Blocking = 4,
}

/// NetworkInterfaceType enum. Upstream: `NetworkInterfaceType` in `nifm.cpp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum NetworkInterfaceType {
    Invalid = 0,
    WiFiIeee80211 = 1,
    Ethernet = 2,
}

/// InternetConnectionStatus enum. Upstream: `InternetConnectionStatus` in `nifm.cpp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum InternetConnectionStatus {
    ConnectingUnknown1 = 0,
    ConnectingUnknown2 = 1,
    ConnectingUnknown3 = 2,
    ConnectingUnknown4 = 3,
    Connected = 4,
}

/// NetworkProfileType enum. Upstream: `NetworkProfileType` in `nifm.cpp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum NetworkProfileType {
    User = 0,
    SsidList = 1,
    Temporary = 2,
}

/// Upstream: `IpAddressSetting` in `nifm.cpp`.
#[derive(Clone, Copy, Default)]
#[repr(C)]
struct IpAddressSetting {
    is_automatic: u8,
    ip_address: [u8; 4],
    subnet_mask: [u8; 4],
    default_gateway: [u8; 4],
}

const _: () = assert!(std::mem::size_of::<IpAddressSetting>() == 0xD);

/// Upstream: `DnsSetting` in `nifm.cpp`.
#[derive(Clone, Copy, Default)]
#[repr(C)]
struct DnsSetting {
    is_automatic: u8,
    primary_dns: [u8; 4],
    secondary_dns: [u8; 4],
}

const _: () = assert!(std::mem::size_of::<DnsSetting>() == 0x9);

/// Upstream: `AuthenticationSetting` in `nifm.cpp`.
#[derive(Clone, Copy)]
#[repr(C)]
struct AuthenticationSetting {
    is_enabled: u8,
    user: [u8; 0x20],
    password: [u8; 0x20],
}

impl Default for AuthenticationSetting {
    fn default() -> Self {
        Self {
            is_enabled: 0,
            user: [0; 0x20],
            password: [0; 0x20],
        }
    }
}

const _: () = assert!(std::mem::size_of::<AuthenticationSetting>() == 0x41);

/// Upstream: `ProxySetting` in `nifm.cpp`.
#[derive(Clone, Copy)]
#[repr(C)]
struct ProxySetting {
    is_enabled: u8,
    _pad0: [u8; 1],
    port: u16,
    proxy_server: [u8; 0x64],
    authentication: AuthenticationSetting,
    _pad1: [u8; 1],
}

impl Default for ProxySetting {
    fn default() -> Self {
        Self {
            is_enabled: 0,
            _pad0: [0; 1],
            port: 0,
            proxy_server: [0; 0x64],
            authentication: AuthenticationSetting::default(),
            _pad1: [0; 1],
        }
    }
}

const _: () = assert!(std::mem::size_of::<ProxySetting>() == 0xAA);

/// Upstream: `IpSettingData` in `nifm.cpp`.
#[derive(Clone, Copy, Default)]
#[repr(C)]
struct IpSettingData {
    ip_address_setting: IpAddressSetting,
    dns_setting: DnsSetting,
    proxy_setting: ProxySetting,
    mtu: u16,
}

const _: () = assert!(std::mem::size_of::<IpSettingData>() == 0xC2);

/// Upstream: `SfWirelessSettingData` in `nifm.cpp`.
#[derive(Clone, Copy)]
#[repr(C)]
struct SfWirelessSettingData {
    ssid_length: u8,
    ssid: [u8; 0x20],
    unknown_1: u8,
    unknown_2: u8,
    unknown_3: u8,
    passphrase: [u8; 0x41],
}

impl Default for SfWirelessSettingData {
    fn default() -> Self {
        Self {
            ssid_length: 0,
            ssid: [0; 0x20],
            unknown_1: 0,
            unknown_2: 0,
            unknown_3: 0,
            passphrase: [0; 0x41],
        }
    }
}

const _: () = assert!(std::mem::size_of::<SfWirelessSettingData>() == 0x65);

/// Upstream: `SfNetworkProfileData` in `nifm.cpp`.
#[derive(Clone, Copy)]
#[repr(C, packed)]
struct SfNetworkProfileData {
    ip_setting_data: IpSettingData,
    uuid: [u8; 0x10],
    network_name: [u8; 0x40],
    unknown_1: u8,
    unknown_2: u8,
    unknown_3: u8,
    unknown_4: u8,
    wireless_setting_data: SfWirelessSettingData,
    _pad0: [u8; 1],
}

impl Default for SfNetworkProfileData {
    fn default() -> Self {
        Self {
            ip_setting_data: IpSettingData::default(),
            uuid: [0; 0x10],
            network_name: [0; 0x40],
            unknown_1: 0,
            unknown_2: 0,
            unknown_3: 0,
            unknown_4: 0,
            wireless_setting_data: SfWirelessSettingData::default(),
            _pad0: [0; 1],
        }
    }
}

const _: () = assert!(std::mem::size_of::<SfNetworkProfileData>() == 0x17C);

/// Upstream-local `IpConfigInfo` in `IGeneralService::GetCurrentIpConfigInfo`.
#[derive(Clone, Copy, Default)]
#[repr(C)]
struct IpConfigInfo {
    ip_address_setting: IpAddressSetting,
    dns_setting: DnsSetting,
}

const _: () = assert!(
    std::mem::size_of::<IpConfigInfo>()
        == std::mem::size_of::<IpAddressSetting>() + std::mem::size_of::<DnsSetting>()
);

#[derive(Clone, Copy)]
#[repr(C)]
struct InternetConnectionStatusOutput {
    interface_type: u8,
    wifi_strength: u8,
    state: u8,
}

const _: () = assert!(std::mem::size_of::<InternetConnectionStatusOutput>() == 0x3);

fn write_c_string<const N: usize>(out: &mut [u8; N], value: &[u8]) {
    let count = value.len().min(N);
    out[..count].copy_from_slice(&value[..count]);
}

fn current_ip_address_setting() -> Option<IpAddressSetting> {
    let net_iface = get_selected_network_interface()?;
    Some(IpAddressSetting {
        is_automatic: 1,
        ip_address: translate_ipv4(net_iface.ip_address),
        subnet_mask: translate_ipv4(net_iface.subnet_mask),
        default_gateway: translate_ipv4(net_iface.gateway),
    })
}

fn upstream_dns_setting() -> DnsSetting {
    DnsSetting {
        is_automatic: 1,
        primary_dns: [1, 1, 1, 1],
        secondary_dns: [1, 0, 0, 1],
    }
}

fn upstream_ip_setting_data() -> IpSettingData {
    let Some(ip_address_setting) = current_ip_address_setting() else {
        return IpSettingData::default();
    };

    IpSettingData {
        ip_address_setting,
        dns_setting: upstream_dns_setting(),
        proxy_setting: ProxySetting::default(),
        mtu: 1500,
    }
}

fn upstream_sf_network_profile_data() -> SfNetworkProfileData {
    let ip_setting_data = upstream_ip_setting_data();
    if ip_setting_data.ip_address_setting.is_automatic == 0 {
        return SfNetworkProfileData::default();
    }

    let mut network_name = [0u8; 0x40];
    write_c_string(&mut network_name, b"yuzu Network");

    let mut ssid = [0u8; 0x20];
    write_c_string(&mut ssid, b"yuzu Network");

    let mut passphrase = [0u8; 0x41];
    write_c_string(&mut passphrase, b"yuzupassword");

    SfNetworkProfileData {
        ip_setting_data,
        uuid: [
            0xef, 0xbe, 0xad, 0xde, 0, 0, 0, 0, 0xef, 0xbe, 0xad, 0xde, 0, 0, 0, 0,
        ],
        network_name,
        unknown_1: 0,
        unknown_2: 0,
        unknown_3: 0,
        unknown_4: 0,
        wireless_setting_data: SfWirelessSettingData {
            ssid_length: 12,
            ssid,
            unknown_1: 0,
            unknown_2: 0,
            unknown_3: 0,
            passphrase,
        },
        _pad0: [0; 1],
    }
}

fn upstream_ip_config_info() -> IpConfigInfo {
    let Some(ip_address_setting) = current_ip_address_setting() else {
        return IpConfigInfo::default();
    };

    IpConfigInfo {
        ip_address_setting,
        dns_setting: upstream_dns_setting(),
    }
}

/// IPC command IDs for IGeneralService
pub mod general_service_commands {
    pub const GET_CLIENT_ID: u32 = 1;
    pub const CREATE_SCAN_REQUEST: u32 = 2;
    pub const CREATE_REQUEST: u32 = 4;
    pub const GET_CURRENT_NETWORK_PROFILE: u32 = 5;
    pub const REMOVE_NETWORK_PROFILE: u32 = 10;
    pub const GET_CURRENT_IP_ADDRESS: u32 = 12;
    pub const CREATE_TEMPORARY_NETWORK_PROFILE: u32 = 14;
    pub const GET_CURRENT_IP_CONFIG_INFO: u32 = 15;
    pub const SET_WIRELESS_COMMUNICATION_ENABLED: u32 = 16;
    pub const IS_WIRELESS_COMMUNICATION_ENABLED: u32 = 17;
    pub const GET_INTERNET_CONNECTION_STATUS: u32 = 18;
    pub const SET_ETHERNET_COMMUNICATION_ENABLED: u32 = 19;
    pub const IS_ETHERNET_COMMUNICATION_ENABLED: u32 = 20;
    pub const IS_ANY_INTERNET_REQUEST_ACCEPTED: u32 = 21;
    pub const IS_ANY_FOREGROUND_REQUEST_ACCEPTED: u32 = 22;
}

fn internet_connection_available(has_host_address: bool, airplane_mode: bool) -> bool {
    has_host_address && !airplane_mode
}

/// IPC command IDs for IRequest
pub mod request_commands {
    pub const GET_REQUEST_STATE: u32 = 0;
    pub const GET_RESULT: u32 = 1;
    pub const GET_SYSTEM_EVENT_READABLE_HANDLES: u32 = 2;
    pub const CANCEL: u32 = 3;
    pub const SUBMIT: u32 = 4;
    pub const SET_REQUIREMENT_PRESET: u32 = 6;
    pub const SET_CONNECTION_CONFIRMATION_OPTION: u32 = 11;
    pub const GET_APPLET_INFO: u32 = 21;
}

/// IPC command IDs for NetworkInterface ("nifm:a", "nifm:s", "nifm:u")
pub mod interface_commands {
    pub const CREATE_GENERAL_SERVICE_OLD: u32 = 4;
    pub const CREATE_GENERAL_SERVICE: u32 = 5;
}

fn push_interface_response(ctx: &mut HLERequestContext, object: SessionRequestHandlerPtr) {
    let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
    rb.push_result(RESULT_SUCCESS);
    rb.push_ipc_interface(object);
}

/// IScanRequest.
pub struct IScanRequest {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IScanRequest {
    pub fn new() -> Self {
        let handlers = build_handler_map(&[
            (0, None, "Submit"),
            (1, None, "IsProcessing"),
            (2, None, "GetResult"),
            (3, None, "GetSystemEventReadableHandle"),
            (4, None, "SetChannels"),
        ]);
        Self {
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for IScanRequest {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "IScanRequest"
    }
}

impl ServiceFramework for IScanRequest {
    fn get_service_name(&self) -> &str {
        "IScanRequest"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// IRequest.
pub struct IRequest {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
    state: RequestState,
    service_context: crate::hle::service::kernel_helpers::ServiceContext,
    event1_handle: u32,
    event2_handle: u32,
}

impl IRequest {
    pub fn new() -> Self {
        let handlers = build_handler_map(&[
            (
                request_commands::GET_REQUEST_STATE,
                Some(IRequest::get_request_state_handler),
                "GetRequestState",
            ),
            (
                request_commands::GET_RESULT,
                Some(IRequest::get_result_handler),
                "GetResult",
            ),
            (
                request_commands::GET_SYSTEM_EVENT_READABLE_HANDLES,
                Some(IRequest::get_system_event_readable_handles_handler),
                "GetSystemEventReadableHandles",
            ),
            (
                request_commands::CANCEL,
                Some(IRequest::cancel_handler),
                "Cancel",
            ),
            (
                request_commands::SUBMIT,
                Some(IRequest::submit_handler),
                "Submit",
            ),
            (5, None, "SetRequirement"),
            (
                request_commands::SET_REQUIREMENT_PRESET,
                Some(IRequest::set_requirement_preset_handler),
                "SetRequirementPreset",
            ),
            (8, None, "SetPriority"),
            (9, None, "SetNetworkProfileId"),
            (10, None, "SetRejectable"),
            (
                request_commands::SET_CONNECTION_CONFIRMATION_OPTION,
                Some(IRequest::set_connection_confirmation_option_handler),
                "SetConnectionConfirmationOption",
            ),
            (12, None, "SetPersistent"),
            (13, None, "SetInstant"),
            (14, None, "SetSustainable"),
            (15, None, "SetRawPriority"),
            (16, None, "SetGreedy"),
            (17, None, "SetSharable"),
            (18, None, "SetRequirementByRevision"),
            (19, None, "GetRequirement"),
            (20, None, "GetRevision"),
            (
                request_commands::GET_APPLET_INFO,
                Some(IRequest::get_applet_info_handler),
                "GetAppletInfo",
            ),
            (22, None, "GetAdditionalInfo"),
            (23, None, "SetKeptInSleep"),
            (24, None, "RegisterSocketDescriptor"),
            (25, None, "UnregisterSocketDescriptor"),
        ]);

        let mut service_context =
            crate::hle::service::kernel_helpers::ServiceContext::new("IRequest".to_string());
        let event1_handle = service_context.create_event("IRequest:Event1".to_string());
        let event2_handle = service_context.create_event("IRequest:Event2".to_string());

        Self {
            handlers,
            handlers_tipc: BTreeMap::new(),
            state: RequestState::NotSubmitted,
            service_context,
            event1_handle,
            event2_handle,
        }
    }

    fn update_state(&mut self, new_state: RequestState) {
        self.state = new_state;
        if let Some(event) = self.service_context.get_event(self.event1_handle) {
            event.signal();
        }
    }

    fn submit_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &mut *(std::ptr::addr_of!(*this).cast::<IRequest>().cast_mut()) };
        log::debug!("(STUBBED) IRequest::Submit called");

        if this.state == RequestState::NotSubmitted {
            this.update_state(RequestState::OnHold);
        }

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_request_state_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const IRequest) };
        log::debug!("(STUBBED) IRequest::GetRequestState called");

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(this.state as u32);
    }

    fn get_result_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe { &mut *(std::ptr::addr_of!(*this).cast::<IRequest>().cast_mut()) };
        log::debug!("(STUBBED) IRequest::GetResult called");

        let has_connection = internet_connection_available(
            get_host_ipv4_address().is_some(),
            *common::settings::values().airplane_mode.get_value(),
        );
        let result = match this.state {
            RequestState::NotSubmitted => {
                if has_connection {
                    RESULT_SUCCESS
                } else {
                    RESULT_NETWORK_COMMUNICATION_DISABLED
                }
            }
            RequestState::OnHold => {
                if has_connection {
                    this.update_state(RequestState::Accepted);
                } else {
                    this.update_state(RequestState::NotSubmitted);
                }
                RESULT_PENDING_CONNECTION
            }
            _ => RESULT_SUCCESS,
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    fn get_system_event_readable_handles_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let this = unsafe { &*(this as *const dyn ServiceFramework as *const IRequest) };
        log::warn!("(STUBBED) IRequest::GetSystemEventReadableHandles called");

        let event1 = this
            .service_context
            .get_event(this.event1_handle)
            .and_then(|event| event.copy_handle(ctx))
            .unwrap_or(0);
        let event2 = this
            .service_context
            .get_event(this.event2_handle)
            .and_then(|event| event.copy_handle(ctx))
            .unwrap_or(0);

        let mut rb = ResponseBuilder::new(ctx, 2, 2, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_objects(event1);
        rb.push_copy_objects(event2);
    }

    fn cancel_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) IRequest::Cancel called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn set_requirement_preset_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let param = rp.pop_u32();
        log::warn!(
            "(STUBBED) IRequest::SetRequirementPreset called, param={}",
            param
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn set_connection_confirmation_option_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) IRequest::SetConnectionConfirmationOption called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_applet_info_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) IRequest::GetAppletInfo called");
        let out = vec![0u8; ctx.get_write_buffer_size(0)];
        ctx.write_buffer(&out, 0);

        let mut rb = ResponseBuilder::new(ctx, 6, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(0);
        rb.push_u32(0);
        rb.push_u32(0);
    }
}

impl Drop for IRequest {
    fn drop(&mut self) {
        self.service_context.close_event(self.event1_handle);
        self.service_context.close_event(self.event2_handle);
    }
}

impl SessionRequestHandler for IRequest {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "IRequest"
    }
}

impl ServiceFramework for IRequest {
    fn get_service_name(&self) -> &str {
        "IRequest"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// INetworkProfile.
pub struct INetworkProfile {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl INetworkProfile {
    pub fn new() -> Self {
        let handlers = build_handler_map(&[
            (0, None, "Update"),
            (1, None, "PersistOld"),
            (2, None, "Persist"),
        ]);
        Self {
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for INetworkProfile {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "INetworkProfile"
    }
}

impl ServiceFramework for INetworkProfile {
    fn get_service_name(&self) -> &str {
        "INetworkProfile"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// IGeneralService.
pub struct IGeneralService {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IGeneralService {
    pub fn new() -> Self {
        let handlers = build_handler_map(&[
            (
                general_service_commands::GET_CLIENT_ID,
                Some(IGeneralService::get_client_id_handler),
                "GetClientId",
            ),
            (
                general_service_commands::CREATE_SCAN_REQUEST,
                Some(IGeneralService::create_scan_request_handler),
                "CreateScanRequest",
            ),
            (
                general_service_commands::CREATE_REQUEST,
                Some(IGeneralService::create_request_handler),
                "CreateRequest",
            ),
            (
                general_service_commands::GET_CURRENT_NETWORK_PROFILE,
                Some(IGeneralService::get_current_network_profile_handler),
                "GetCurrentNetworkProfile",
            ),
            (6, None, "EnumerateNetworkInterfaces"),
            (7, None, "EnumerateNetworkProfiles"),
            (8, None, "GetNetworkProfile"),
            (9, None, "SetNetworkProfile"),
            (
                general_service_commands::REMOVE_NETWORK_PROFILE,
                Some(IGeneralService::remove_network_profile_handler),
                "RemoveNetworkProfile",
            ),
            (11, None, "GetScanDataOld"),
            (
                general_service_commands::GET_CURRENT_IP_ADDRESS,
                Some(IGeneralService::get_current_ip_address_handler),
                "GetCurrentIpAddress",
            ),
            (13, None, "GetCurrentAccessPointOld"),
            (
                general_service_commands::CREATE_TEMPORARY_NETWORK_PROFILE,
                Some(IGeneralService::create_temporary_network_profile_handler),
                "CreateTemporaryNetworkProfile",
            ),
            (
                general_service_commands::GET_CURRENT_IP_CONFIG_INFO,
                Some(IGeneralService::get_current_ip_config_info_handler),
                "GetCurrentIpConfigInfo",
            ),
            (
                general_service_commands::SET_WIRELESS_COMMUNICATION_ENABLED,
                Some(IGeneralService::set_wireless_communication_enabled_handler),
                "SetWirelessCommunicationEnabled",
            ),
            (
                general_service_commands::IS_WIRELESS_COMMUNICATION_ENABLED,
                Some(IGeneralService::is_wireless_communication_enabled_handler),
                "IsWirelessCommunicationEnabled",
            ),
            (
                general_service_commands::GET_INTERNET_CONNECTION_STATUS,
                Some(IGeneralService::get_internet_connection_status_handler),
                "GetInternetConnectionStatus",
            ),
            (
                general_service_commands::SET_ETHERNET_COMMUNICATION_ENABLED,
                Some(IGeneralService::set_ethernet_communication_enabled_handler),
                "SetEthernetCommunicationEnabled",
            ),
            (
                general_service_commands::IS_ETHERNET_COMMUNICATION_ENABLED,
                Some(IGeneralService::is_ethernet_communication_enabled_handler),
                "IsEthernetCommunicationEnabled",
            ),
            (
                general_service_commands::IS_ANY_INTERNET_REQUEST_ACCEPTED,
                Some(IGeneralService::is_any_internet_request_accepted_handler),
                "IsAnyInternetRequestAccepted",
            ),
            (
                general_service_commands::IS_ANY_FOREGROUND_REQUEST_ACCEPTED,
                Some(IGeneralService::is_any_foreground_request_accepted_handler),
                "IsAnyForegroundRequestAccepted",
            ),
            (23, None, "PutToSleep"),
            (24, None, "WakeUp"),
            (25, None, "GetSsidListVersion"),
            (26, None, "SetExclusiveClient"),
            (27, None, "GetDefaultIpSetting"),
            (28, None, "SetDefaultIpSetting"),
            (29, None, "SetWirelessCommunicationEnabledForTest"),
            (30, None, "SetEthernetCommunicationEnabledForTest"),
            (31, None, "GetTelemetorySystemEventReadableHandle"),
            (32, None, "GetTelemetryInfo"),
            (33, None, "ConfirmSystemAvailability"),
            (34, None, "SetBackgroundRequestEnabled"),
            (35, None, "GetScanData"),
            (36, None, "GetCurrentAccessPoint"),
            (37, None, "Shutdown"),
            (38, None, "GetAllowedChannels"),
            (39, None, "NotifyApplicationSuspended"),
            (40, None, "SetAcceptableNetworkTypeFlag"),
            (41, None, "GetAcceptableNetworkTypeFlag"),
            (42, None, "NotifyConnectionStateChanged"),
            (43, None, "SetWowlDelayedWakeTime"),
        ]);

        Self {
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn get_client_id_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) IGeneralService::GetClientId called");
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(1);
    }

    fn create_scan_request_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("IGeneralService::CreateScanRequest called");
        push_interface_response(ctx, Arc::new(IScanRequest::new()));
    }

    fn create_request_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("IGeneralService::CreateRequest called");
        push_interface_response(ctx, Arc::new(IRequest::new()));
    }

    fn get_current_network_profile_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) IGeneralService::GetCurrentNetworkProfile called");
        let network_profile_data = upstream_sf_network_profile_data();
        let out = unsafe {
            core::slice::from_raw_parts(
                (&network_profile_data as *const SfNetworkProfileData).cast::<u8>(),
                core::mem::size_of::<SfNetworkProfileData>(),
            )
        };
        ctx.write_buffer(out, 0);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn remove_network_profile_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) IGeneralService::RemoveNetworkProfile called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_current_ip_address_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) IGeneralService::GetCurrentIpAddress called");
        let ipv4 = get_host_ipv4_address().unwrap_or([0, 0, 0, 0]);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_raw(&ipv4);
    }

    fn create_temporary_network_profile_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("IGeneralService::CreateTemporaryNetworkProfile called");

        let mut uuid = [0u8; 16];
        let buffer = ctx.read_buffer(0);
        if buffer.len() >= 24 {
            uuid.copy_from_slice(&buffer[8..24]);
        }

        let mut rb = ResponseBuilder::new(ctx, 6, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(Arc::new(INetworkProfile::new()));
        rb.push_raw(&uuid);
    }

    fn get_current_ip_config_info_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) IGeneralService::GetCurrentIpConfigInfo called");
        let ip_config_info = upstream_ip_config_info();
        let mut rb = ResponseBuilder::new(
            ctx,
            (2 + (core::mem::size_of::<IpConfigInfo>() + 3) / core::mem::size_of::<u32>()) as u32,
            0,
            0,
        );
        rb.push_result(RESULT_SUCCESS);
        rb.push_raw(&ip_config_info);
    }

    fn is_wireless_communication_enabled_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u8(if *common::settings::values().airplane_mode.get_value() {
            0
        } else {
            1
        });
    }

    fn set_wireless_communication_enabled_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut rp = RequestParser::new(ctx);
        let enable = rp.pop_u8();
        common::settings::values_mut()
            .airplane_mode
            .set_value(enable == 0);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_internet_connection_status_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        refresh_from_host();
        let state = EmuNetState::get().lock();
        let out = if state.connected {
            InternetConnectionStatusOutput {
                interface_type: if state.via_wifi {
                    NetworkInterfaceType::WiFiIeee80211 as u8
                } else {
                    NetworkInterfaceType::Ethernet as u8
                },
                wifi_strength: state.bars,
                state: InternetConnectionStatus::Connected as u8,
            }
        } else {
            InternetConnectionStatusOutput {
                interface_type: NetworkInterfaceType::WiFiIeee80211 as u8,
                wifi_strength: 0,
                state: InternetConnectionStatus::ConnectingUnknown1 as u8,
            }
        };
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_raw(&out);
    }

    fn is_ethernet_communication_enabled_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) IGeneralService::IsEthernetCommunicationEnabled called");
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u8(
            if internet_connection_available(
                get_host_ipv4_address().is_some(),
                *common::settings::values().airplane_mode.get_value(),
            ) {
                1
            } else {
                0
            },
        );
    }

    fn set_ethernet_communication_enabled_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        use std::sync::atomic::Ordering;

        let mut rp = RequestParser::new(ctx);
        EmuNetState::get()
            .ethernet_enabled
            .store(rp.pop_u8() != 0, Ordering::Relaxed);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn is_any_internet_request_accepted_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::error!("(STUBBED) IGeneralService::IsAnyInternetRequestAccepted called");
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u8(
            if internet_connection_available(
                get_host_ipv4_address().is_some(),
                *common::settings::values().airplane_mode.get_value(),
            ) {
                1
            } else {
                0
            },
        );
    }

    fn is_any_foreground_request_accepted_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) IGeneralService::IsAnyForegroundRequestAccepted called");
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u8(0);
    }
}

impl SessionRequestHandler for IGeneralService {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "IGeneralService"
    }
}

impl ServiceFramework for IGeneralService {
    fn get_service_name(&self) -> &str {
        "IGeneralService"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// NetworkInterface ("nifm:a", "nifm:s", "nifm:u").
pub struct NetworkInterface {
    name: String,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl NetworkInterface {
    pub fn new(name: &str) -> Self {
        let handlers = build_handler_map(&[
            (
                interface_commands::CREATE_GENERAL_SERVICE_OLD,
                Some(NetworkInterface::create_general_service_old_handler),
                "CreateGeneralServiceOld",
            ),
            (
                interface_commands::CREATE_GENERAL_SERVICE,
                Some(NetworkInterface::create_general_service_handler),
                "CreateGeneralService",
            ),
        ]);
        Self {
            name: name.to_string(),
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn create_general_service_old_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("NetworkInterface::CreateGeneralServiceOld called");
        push_interface_response(ctx, Arc::new(IGeneralService::new()));
    }

    fn create_general_service_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("NetworkInterface::CreateGeneralService called");
        push_interface_response(ctx, Arc::new(IGeneralService::new()));
    }
}

impl SessionRequestHandler for NetworkInterface {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        &self.name
    }
}

impl ServiceFramework for NetworkInterface {
    fn get_service_name(&self) -> &str {
        &self.name
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// Registers "nifm:u", "nifm:a", "nifm:s" services.
///
/// Corresponds to `LoopProcess` in upstream `nifm.cpp`.
pub fn loop_process(system: crate::core::SystemRef) {
    use crate::hle::service::hle_ipc::SessionRequestHandlerPtr;
    use crate::hle::service::server_manager::ServerManager;

    let server_manager = ServerManager::new_shared(system);

    {
        let mut server_manager = server_manager.lock().unwrap();
        server_manager.register_named_service(
            "nifm:a",
            Box::new(|| -> SessionRequestHandlerPtr { Arc::new(NetworkInterface::new("nifm:a")) }),
            16,
        );
        server_manager.register_named_service(
            "nifm:s",
            Box::new(|| -> SessionRequestHandlerPtr { Arc::new(NetworkInterface::new("nifm:s")) }),
            16,
        );
        server_manager.register_named_service(
            "nifm:u",
            Box::new(|| -> SessionRequestHandlerPtr { Arc::new(NetworkInterface::new("nifm:u")) }),
            16,
        );
    }

    ServerManager::run_server_shared(server_manager);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn airplane_mode_suppresses_host_connectivity() {
        assert!(internet_connection_available(true, false));
        assert!(!internet_connection_available(true, true));
        assert!(!internet_connection_available(false, false));
    }

    #[test]
    fn wireless_and_ethernet_commands_are_registered() {
        let service = IGeneralService::new();
        assert!(service
            .handlers
            .get(&general_service_commands::SET_WIRELESS_COMMUNICATION_ENABLED)
            .is_some_and(|entry| entry.handler_callback.is_some()));
        assert!(service
            .handlers
            .get(&general_service_commands::SET_ETHERNET_COMMUNICATION_ENABLED)
            .is_some_and(|entry| entry.handler_callback.is_some()));
    }
}
