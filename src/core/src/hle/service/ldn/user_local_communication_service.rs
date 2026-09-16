// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of core/hle/service/ldn/user_local_communication_service.{h,cpp}.

use super::lan_discovery::{read_network_info, LANDiscovery};
use super::ldn_results::*;
use super::ldn_types::*;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::kernel_helpers::ServiceContext;
use crate::hle::service::os::event::Event;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};
use crate::internal_network::network_interface::get_selected_network_interface;
use network::room_member::{CallbackHandle, LdnPacket, RoomMember};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

// Mechanical CMIF byte access. Do not construct enum-containing guest values
// until their matching ldn_types decoder has validated the discriminants.
fn request_bytes(ctx: &HLERequestContext) -> &[u8] {
    let words = &ctx.command_buffer()[ctx.data_payload_offset as usize + 2..];
    unsafe { std::slice::from_raw_parts(words.as_ptr().cast(), words.len() * 4) }
}

// Call only with the fully initialized, explicitly padded LDN wire structs.
fn wire_bytes<T: Copy>(value: &T) -> &[u8] {
    unsafe { std::slice::from_raw_parts((value as *const T).cast(), std::mem::size_of::<T>()) }
}

pub struct IUserLocalCommunicationService {
    service_context: ServiceContext,
    state_change_event: u32,
    // The weak callback target prevents a service/room reference cycle. The
    // backend lock is released during Scan's receive window in lan_discovery.rs.
    lan_discovery: Arc<Mutex<LANDiscovery>>,
    ldn_packet_received: Mutex<Option<(Weak<RoomMember>, CallbackHandle<LdnPacket>)>>,
    is_initialized: AtomicBool,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IUserLocalCommunicationService {
    pub fn new() -> Self {
        let mut service_context = ServiceContext::new("IUserLocalCommunicationService".into());
        let state_change_event =
            service_context.create_event("IUserLocalCommunicationService:StateChangeEvent".into());
        Self {
            service_context,
            state_change_event,
            lan_discovery: Arc::new(Mutex::new(LANDiscovery::new())),
            ldn_packet_received: Mutex::new(None),
            is_initialized: AtomicBool::new(false),
            handlers: build_handler_map(&[
                (0, Some(Self::get_state_handler), "GetState"),
                (1, Some(Self::get_network_info_handler), "GetNetworkInfo"),
                (2, Some(Self::get_ipv4_address_handler), "GetIpv4Address"),
                (
                    3,
                    Some(Self::get_disconnect_reason_handler),
                    "GetDisconnectReason",
                ),
                (
                    4,
                    Some(Self::get_security_parameter_handler),
                    "GetSecurityParameter",
                ),
                (
                    5,
                    Some(Self::get_network_config_handler),
                    "GetNetworkConfig",
                ),
                (
                    100,
                    Some(Self::attach_state_change_event_handler),
                    "AttachStateChangeEvent",
                ),
                (
                    101,
                    Some(Self::get_network_info_latest_update_handler),
                    "GetNetworkInfoLatestUpdate",
                ),
                (102, Some(Self::scan_handler), "Scan"),
                (103, Some(Self::scan_private_handler), "ScanPrivate"),
                (
                    104,
                    Some(Self::set_wireless_controller_restriction_handler),
                    "SetWirelessControllerRestriction",
                ),
                (106, Some(Self::set_protocol_handler), "SetProtocol"),
                (
                    200,
                    Some(Self::open_access_point_handler),
                    "OpenAccessPoint",
                ),
                (
                    201,
                    Some(Self::close_access_point_handler),
                    "CloseAccessPoint",
                ),
                (202, Some(Self::create_network_handler), "CreateNetwork"),
                (
                    203,
                    Some(Self::create_network_private_handler),
                    "CreateNetworkPrivate",
                ),
                (204, Some(Self::destroy_network_handler), "DestroyNetwork"),
                (205, None, "Reject"),
                (
                    206,
                    Some(Self::set_advertise_data_handler),
                    "SetAdvertiseData",
                ),
                (
                    207,
                    Some(Self::set_station_accept_policy_handler),
                    "SetStationAcceptPolicy",
                ),
                (
                    208,
                    Some(Self::add_accept_filter_entry_handler),
                    "AddAcceptFilterEntry",
                ),
                (209, None, "ClearAcceptFilter"),
                (300, Some(Self::open_station_handler), "OpenStation"),
                (301, Some(Self::close_station_handler), "CloseStation"),
                (302, Some(Self::connect_handler), "Connect"),
                (303, None, "ConnectPrivate"),
                (304, Some(Self::disconnect_handler), "Disconnect"),
                (400, Some(Self::initialize_handler), "Initialize"),
                (401, Some(Self::finalize_handler), "Finalize"),
                (402, Some(Self::initialize2_handler), "Initialize2"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn as_self(this: &dyn ServiceFramework) -> &Self {
        unsafe { &*(this as *const dyn ServiceFramework as *const Self) }
    }

    pub fn get_state(&self) -> (ResultCode, State) {
        (
            RESULT_SUCCESS,
            if self.is_initialized.load(Ordering::SeqCst) {
                self.lan_discovery.lock().unwrap().get_state()
            } else {
                State::Error
            },
        )
    }

    pub fn get_disconnect_reason(&self) -> (ResultCode, DisconnectReason) {
        (
            RESULT_SUCCESS,
            self.lan_discovery.lock().unwrap().get_disconnect_reason(),
        )
    }

    pub fn initialize(&self, _aruid: u64) -> ResultCode {
        if get_selected_network_interface().is_none() {
            return RESULT_AIRPLANE_MODE_ENABLED;
        }
        let Some(room) = network::network::get_room_member().upgrade() else {
            return RESULT_AIRPLANE_MODE_ENABLED;
        };
        let mut registration = self.ldn_packet_received.lock().unwrap();
        // Replace a repeated initialization's registration without leaking a
        // callback. Never hold the backend mutex while unbinding: RoomMember's
        // delivery holds its callback-list lock before entering this backend.
        if let Some((old_room, callback)) = registration.take() {
            if let Some(old_room) = old_room.upgrade() {
                old_room.unbind_on_ldn_packet_received(&callback);
            }
        }
        let discovery = Arc::downgrade(&self.lan_discovery);
        let callback = room.bind_on_ldn_packet_received(move |packet| {
            if let Some(discovery) = discovery.upgrade() {
                Self::on_ldn_packet_received(&discovery, packet);
            }
        });
        *registration = Some((Arc::downgrade(&room), callback));
        let event = self
            .service_context
            .get_event(self.state_change_event)
            .unwrap();
        self.lan_discovery
            .lock()
            .unwrap()
            .initialize(Arc::new(move || Self::on_event_fired(&event)), true);
        self.is_initialized.store(true, Ordering::SeqCst);
        RESULT_SUCCESS
    }

    pub fn finalize(&self) -> ResultCode {
        let mut registration = self.ldn_packet_received.lock().unwrap();
        if let Some((room, callback)) = registration.take() {
            if let Some(room) = room.upgrade() {
                room.unbind_on_ldn_packet_received(&callback);
            }
        }
        self.is_initialized.store(false, Ordering::SeqCst);
        self.lan_discovery.lock().unwrap().finalize()
    }

    pub fn initialize2(&self, _version: u32, process_id: u64) -> ResultCode {
        self.initialize(process_id)
    }

    fn on_ldn_packet_received(discovery: &Mutex<LANDiscovery>, packet: &LdnPacket) {
        discovery.lock().unwrap().receive_packet(packet);
    }

    fn on_event_fired(event: &Event) {
        event.signal();
    }

    fn get_state_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let (result, state) = Self::as_self(this).get_state();
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_u32(state as u32);
    }

    fn get_disconnect_reason_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let (result, reason) = Self::as_self(this).get_disconnect_reason();
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_raw(&(reason as i16));
    }

    fn get_network_info_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let info = Self::as_self(this)
            .lan_discovery
            .lock()
            .unwrap()
            .get_network_info();
        let result = info.as_ref().map_or_else(|e| *e, |_| RESULT_SUCCESS);
        ctx.write_buffer_c(wire_bytes(&info.unwrap_or_default()), 0);
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }

    fn get_ipv4_address_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut address = [0; 4];
        let mut mask = [0; 4];
        let result = if let Some(interface) = get_selected_network_interface() {
            address = interface.ip_address.octets();
            mask = interface.subnet_mask.octets();
            if let Some(room) = network::network::get_room_member().upgrade() {
                if room.is_connected() {
                    address = room.get_fake_ip_address();
                }
            }
            address.reverse();
            mask.reverse();
            RESULT_SUCCESS
        } else {
            RESULT_NO_IP_ADDRESS
        };
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(result);
        rb.push_raw(&address);
        rb.push_raw(&mask);
    }

    fn get_security_parameter_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let info = Self::as_self(this)
            .lan_discovery
            .lock()
            .unwrap()
            .get_network_info();
        let result = info.as_ref().map_or_else(|e| *e, |_| RESULT_SUCCESS);
        let mut out = SecurityParameter::default();
        if let Ok(info) = info {
            out.session_id = info.network_id.session_id;
            out.data = info.ldn.security_parameter;
        }
        let mut rb = ResponseBuilder::new(ctx, 10, 0, 0);
        rb.push_result(result);
        rb.push_raw(&out);
    }

    fn get_network_config_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let info = Self::as_self(this)
            .lan_discovery
            .lock()
            .unwrap()
            .get_network_info();
        let result = info.as_ref().map_or_else(|e| *e, |_| RESULT_SUCCESS);
        let mut out = NetworkConfig::default();
        if let Ok(info) = info {
            out.intent_id = info.network_id.intent_id;
            out.channel = info.common.channel;
            out.node_count_max = info.ldn.node_count_max;
            out.local_communication_version = info.ldn.nodes[0].local_communication_version as u16;
        }
        let mut rb = ResponseBuilder::new(ctx, 10, 0, 0);
        rb.push_result(result);
        rb.push_raw(&out);
    }

    fn attach_state_change_event_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let event = service
            .service_context
            .get_event(service.state_change_event)
            .unwrap();
        let object = event.copy_object_id(ctx).unwrap_or(0);
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object);
    }

    fn get_network_info_latest_update_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let size = ctx.get_write_buffer_size(1);
        let count = size / std::mem::size_of::<NodeLatestUpdate>();
        let mut output = vec![0; size];
        let mut network = NetworkInfo::default();
        let result = if count == 0 {
            RESULT_BAD_INPUT
        } else {
            match Self::as_self(this)
                .lan_discovery
                .lock()
                .unwrap()
                .get_network_info_latest_update(count)
            {
                Ok((info, updates)) => {
                    network = info;
                    for (i, update) in updates.iter().enumerate() {
                        output[i * 8..i * 8 + 8].copy_from_slice(wire_bytes(update));
                    }
                    RESULT_SUCCESS
                }
                Err(result) => result,
            }
        };
        ctx.write_buffer_c(wire_bytes(&network), 0);
        ctx.write_buffer_c(&output, 1);
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }

    fn scan_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let size = ctx.get_write_buffer_size(0);
        let capacity = size / std::mem::size_of::<NetworkInfo>();
        let filter = ScanFilter::from_bytes(&request_bytes(ctx)[8..]); // s16 channel, align 8
        let mut output = vec![0; size];
        let mut count = 0i16;
        let result = if capacity == 0 {
            RESULT_BAD_INPUT
        } else if let Some(filter) = filter {
            let networks =
                LANDiscovery::scan(&Self::as_self(this).lan_discovery, &filter, capacity);
            count = networks.len() as i16;
            for (i, network) in networks.iter().enumerate() {
                let start = i * std::mem::size_of::<NetworkInfo>();
                output[start..start + std::mem::size_of::<NetworkInfo>()]
                    .copy_from_slice(wire_bytes(network));
            }
            RESULT_SUCCESS
        } else {
            RESULT_BAD_INPUT
        };
        ctx.write_buffer(&output, 0);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_raw(&count);
    }

    fn scan_private_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let size = ctx.get_write_buffer_size(0);
        let capacity = size / std::mem::size_of::<NetworkInfo>();
        let filter = ScanFilter::from_bytes(&request_bytes(ctx)[8..]); // s16 channel, align 8
        let mut output = vec![0; size];
        let mut count = 0i16;
        let result = if capacity != 0 {
            RESULT_BAD_INPUT
        } else if let Some(filter) = filter {
            let networks =
                LANDiscovery::scan(&Self::as_self(this).lan_discovery, &filter, capacity);
            count = networks.len() as i16;
            for (i, network) in networks.iter().enumerate() {
                let start = i * std::mem::size_of::<NetworkInfo>();
                output[start..start + std::mem::size_of::<NetworkInfo>()]
                    .copy_from_slice(wire_bytes(network));
            }
            RESULT_SUCCESS
        } else {
            RESULT_BAD_INPUT
        };
        ctx.write_buffer(&output, 0);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_raw(&count);
    }

    fn set_wireless_controller_restriction_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) set_wireless_controller_restriction called");
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }

    fn set_protocol_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) set_protocol called");
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }

    fn set_station_accept_policy_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) set_station_accept_policy called");
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }

    fn add_accept_filter_entry_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) add_accept_filter_entry called");
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }

    fn open_access_point_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let result = Self::as_self(this)
            .lan_discovery
            .lock()
            .unwrap()
            .open_access_point();
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }

    fn close_access_point_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let result = Self::as_self(this)
            .lan_discovery
            .lock()
            .unwrap()
            .close_access_point();
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }

    fn destroy_network_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let result = Self::as_self(this)
            .lan_discovery
            .lock()
            .unwrap()
            .destroy_network();
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }

    fn open_station_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let result = Self::as_self(this)
            .lan_discovery
            .lock()
            .unwrap()
            .open_station();
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }

    fn close_station_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let result = Self::as_self(this)
            .lan_discovery
            .lock()
            .unwrap()
            .close_station();
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }

    fn disconnect_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let result = Self::as_self(this)
            .lan_discovery
            .lock()
            .unwrap()
            .disconnect();
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }

    fn create_network_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let result = if let Some(config) = CreateNetworkConfig::from_bytes(request_bytes(ctx)) {
            // Copy packed fields to aligned locals before borrowing.
            let security = config.security_config;
            let user = config.user_config;
            let network = config.network_config;
            Self::as_self(this)
                .lan_discovery
                .lock()
                .unwrap()
                .create_network(&security, &user, &network)
        } else {
            RESULT_BAD_INPUT
        };
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }

    fn create_network_private_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let result =
            if let Some(config) = CreateNetworkConfigPrivate::from_bytes(request_bytes(ctx)) {
                // Copy packed fields to aligned locals before borrowing.
                let security = config.security_config;
                let user = config.user_config;
                let network = config.network_config;
                Self::as_self(this)
                    .lan_discovery
                    .lock()
                    .unwrap()
                    .create_network(&security, &user, &network)
            } else {
                RESULT_BAD_INPUT
            };
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }

    fn set_advertise_data_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let result = Self::as_self(this)
            .lan_discovery
            .lock()
            .unwrap()
            .set_advertise_data(&ctx.read_buffer(0));
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }

    fn connect_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let config = ConnectNetworkData::from_bytes(request_bytes(ctx));
        let network = read_network_info(&ctx.read_buffer_x(0));
        let result = match (config, network) {
            (Some(config), Some(network)) => {
                Self::as_self(this).lan_discovery.lock().unwrap().connect(
                    &network,
                    &config.user_config,
                    config.local_communication_version as u16,
                )
            }
            _ => RESULT_BAD_INPUT,
        };
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }

    fn initialize_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let result = Self::as_self(this).initialize(ctx.get_pid());
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }

    fn initialize2_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let version = RequestParser::new(ctx).pop_u32();
        let result = Self::as_self(this).initialize2(version, ctx.get_pid());
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }

    fn finalize_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let result = Self::as_self(this).finalize();
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }
}

impl Drop for IUserLocalCommunicationService {
    fn drop(&mut self) {
        if let Some((room, callback)) = self.ldn_packet_received.get_mut().unwrap().take() {
            if let Some(room) = room.upgrade() {
                room.unbind_on_ldn_packet_received(&callback);
            }
        }
        self.service_context.close_event(self.state_change_event);
    }
}

impl SessionRequestHandler for IUserLocalCommunicationService {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }
    fn service_name(&self) -> &str {
        "IUserLocalCommunicationService"
    }
}

impl ServiceFramework for IUserLocalCommunicationService {
    fn get_service_name(&self) -> &str {
        "IUserLocalCommunicationService"
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
    use network::room_member::LdnPacketType;

    fn invoke(service: &IUserLocalCommunicationService, command: u32) -> HLERequestContext {
        let mut ctx = HLERequestContext::new();
        service.handlers[&command].handler_callback.unwrap()(service, &mut ctx);
        ctx
    }

    #[test]
    fn command_table_matches_upstream_implemented_and_null_entries() {
        let service = IUserLocalCommunicationService::new();
        for id in [
            0, 1, 2, 3, 4, 5, 100, 101, 102, 103, 104, 106, 200, 201, 202, 203, 204, 206, 207, 208,
            300, 301, 302, 304, 400, 401, 402,
        ] {
            assert!(service.handlers[&id].handler_callback.is_some(), "{id}");
        }
        for id in [205, 209, 303] {
            assert!(service.handlers[&id].handler_callback.is_none());
        }
        assert_eq!(service.handlers.len(), 30);
    }

    #[test]
    fn ipc_state_machine_and_missing_buffer_errors() {
        let service = IUserLocalCommunicationService::new();
        assert_eq!(invoke(&service, 0).cmd_buf[8], State::Error as u32);
        for command in [1, 4, 5, 200, 201, 300, 301] {
            assert_eq!(invoke(&service, command).cmd_buf[6], RESULT_BAD_STATE.0);
        }
        for command in [101, 102, 302] {
            assert_eq!(invoke(&service, command).cmd_buf[6], RESULT_BAD_INPUT.0);
        }
        let event = service
            .service_context
            .get_event(service.state_change_event)
            .unwrap();
        let notification = event.clone();
        service.lan_discovery.lock().unwrap().initialize(
            Arc::new(move || IUserLocalCommunicationService::on_event_fired(&notification)),
            true,
        );
        service.is_initialized.store(true, Ordering::SeqCst);
        assert_eq!(invoke(&service, 0).cmd_buf[8], State::Initialized as u32);
        for (command, state) in [
            (200, State::AccessPointOpened),
            (201, State::Initialized),
            (300, State::StationOpened),
            (301, State::Initialized),
        ] {
            assert_eq!(invoke(&service, command).cmd_buf[6], 0);
            assert_eq!(invoke(&service, 0).cmd_buf[8], state as u32);
        }
        assert_eq!(invoke(&service, 304).cmd_buf[6], 0);
        assert!(event.is_signaled());
        assert_eq!(invoke(&service, 401).cmd_buf[6], 0);
        assert_eq!(invoke(&service, 0).cmd_buf[8], State::Error as u32);
    }

    #[test]
    fn packet_callback_updates_ipc_security_and_network_configuration() {
        let service = IUserLocalCommunicationService::new();
        service
            .lan_discovery
            .lock()
            .unwrap()
            .set_state(State::StationOpened);
        let mut info = NetworkInfo::default();
        info.network_id.session_id = SessionId { high: 17, low: 23 };
        info.network_id.intent_id.scene_id = 9;
        info.ldn.security_parameter = [0x5a; 16];
        info.common.channel = WifiChannel::Wifi50_44;
        info.ldn.node_count_max = 8;
        info.ldn.nodes[0].local_communication_version = -2;
        IUserLocalCommunicationService::on_ldn_packet_received(
            &service.lan_discovery,
            &LdnPacket {
                packet_type: LdnPacketType::SyncNetwork,
                local_ip: [0; 4],
                remote_ip: [0; 4],
                broadcast: false,
                data: wire_bytes(&info).to_vec(),
            },
        );
        let security = invoke(&service, 4);
        assert_eq!(security.cmd_buf[6], 0);
        assert_eq!(&security.cmd_buf[8..12], &[0x5a5a5a5a; 4]);
        assert_eq!(&security.cmd_buf[12..16], &[17, 0, 23, 0]);
        let config = invoke(&service, 5);
        assert_eq!(config.cmd_buf[6], 0);
        assert_eq!(config.cmd_buf[10], 9 << 16);
        assert_eq!(config.cmd_buf[12], 44 | (8 << 16));
        assert_eq!(config.cmd_buf[13], 0xfffe);
        assert_eq!(&config.cmd_buf[14..16], &[0; 2]);
    }

    #[test]
    fn scan_private_preserves_upstream_empty_output_guard_and_update_count_precedence() {
        let service = IUserLocalCommunicationService::new();
        let descriptor = |size| crate::hle::ipc::BufferDescriptorABW {
            size_bits_0_31: size,
            address_bits_0_31: 0,
            raw_word2: 0,
        };
        let mut ctx = HLERequestContext::new();
        ctx.set_buffer_b_descriptors_for_test(vec![descriptor(0x480)]);
        service.handlers[&103].handler_callback.unwrap()(&service, &mut ctx);
        assert_eq!(ctx.cmd_buf[6], RESULT_BAD_INPUT.0);
        // Upstream ScanPrivate requires an empty output span, unlike Scan.
        assert_eq!(invoke(&service, 103).cmd_buf[6], 0);
        let mut ctx = HLERequestContext::new();
        ctx.set_buffer_b_descriptors_for_test(vec![descriptor(0x480), descriptor(9 * 8)]);
        service.handlers[&101].handler_callback.unwrap()(&service, &mut ctx);
        assert_eq!(ctx.cmd_buf[6], RESULT_INVALID_BUFFER_COUNT.0);
    }

    #[test]
    fn finalize_and_drop_remove_room_callbacks_without_retaining_service() {
        let room = Arc::new(RoomMember::new());
        for finalize in [false, true] {
            let service = IUserLocalCommunicationService::new();
            let weak = Arc::downgrade(&service.lan_discovery);
            let callback_target = weak.clone();
            let callback = room.bind_on_ldn_packet_received(move |packet| {
                if let Some(discovery) = callback_target.upgrade() {
                    IUserLocalCommunicationService::on_ldn_packet_received(&discovery, packet);
                }
            });
            *service.ldn_packet_received.lock().unwrap() =
                Some((Arc::downgrade(&room), callback.clone()));
            assert_eq!(Arc::strong_count(&callback), 3);
            if finalize {
                assert_eq!(service.finalize(), RESULT_SUCCESS);
                assert_eq!(Arc::strong_count(&callback), 1);
            }
            drop(service);
            assert_eq!(Arc::strong_count(&callback), 1);
            assert!(weak.upgrade().is_none());
        }
    }
}
