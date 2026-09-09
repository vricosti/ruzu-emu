// SPDX-FileCopyrightText: 2018 Citra Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `input_common/drivers/udp_client.h` and `input_common/drivers/udp_client.cpp`.
//!
//! UDP client driver for Cemuhook protocol (e.g., DS4Windows, BetterJoy).

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicI8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::input::{BatteryLevel, ButtonNames};
use common::param_package::ParamPackage;
use common::settings_input::{native_analog, native_button, native_motion};
use common::uuid::UUID;
use parking_lot::Mutex as EngineMutex;

use crate::helpers::udp_protocol::{self, response, MessageType, MAX_PACKET_SIZE};
use crate::input_engine::{BasicMotion, InputEngine, PadIdentifier};
use crate::main_common::{AnalogMapping, ButtonMapping, MotionMapping};

/// Port of CemuhookUDP namespace types

/// Port of `PadTouch` enum from udp_client.h
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadTouch {
    Click,
    Undefined,
}

/// Port of `UDPPadStatus` struct from udp_client.h
#[derive(Debug, Clone)]
pub struct UdpPadStatus {
    pub host: String,
    pub port: u16,
    pub pad_index: usize,
}

impl Default for UdpPadStatus {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 26760,
            pad_index: 0,
        }
    }
}

/// Port of UDPClient::PadButton enum from udp_client.h
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum PadButton {
    Undefined = 0x0000,
    Share = 0x0001,
    L3 = 0x0002,
    R3 = 0x0004,
    Options = 0x0008,
    Up = 0x0010,
    Right = 0x0020,
    Down = 0x0040,
    Left = 0x0080,
    L2 = 0x0100,
    R2 = 0x0200,
    L1 = 0x0400,
    R1 = 0x0800,
    Triangle = 0x1000,
    Circle = 0x2000,
    Cross = 0x4000,
    Square = 0x8000,
    Touch1 = 0x10000,
    Touch2 = 0x20000,
    Home = 0x40000,
    TouchHardPress = 0x80000,
}

/// Port of UDPClient::PadAxes enum from udp_client.h
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PadAxes {
    LeftStickX,
    LeftStickY,
    RightStickX,
    RightStickY,
    AnalogLeft,
    AnalogDown,
    AnalogRight,
    AnalogUp,
    AnalogSquare,
    AnalogCross,
    AnalogCircle,
    AnalogTriangle,
    AnalogR1,
    AnalogL1,
    AnalogR2,
    AnalogL3,
    AnalogR3,
    Touch1X,
    Touch1Y,
    Touch2X,
    Touch2Y,
    Undefined,
}

/// Maximum number of UDP clients.
const MAX_UDP_CLIENTS: usize = 8;
/// Pads per client.
const PADS_PER_CLIENT: usize = 4;

/// Port of UDPClient::PadData struct from udp_client.h
struct PadData {
    connected: bool,
    packet_sequence: u64,
    last_update: Instant,
}

impl Default for PadData {
    fn default() -> Self {
        Self {
            connected: false,
            packet_sequence: 0,
            last_update: Instant::now(),
        }
    }
}

/// Port of UDPClient::ClientConnection struct from udp_client.h
struct ClientConnection {
    uuid: UUID,
    host: String,
    port: u16,
    active: Arc<AtomicI8>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Default for ClientConnection {
    fn default() -> Self {
        Self {
            uuid: UUID::from_string("00000000-0000-0000-0000-00007F000001"),
            host: "127.0.0.1".to_string(),
            port: 26760,
            active: Arc::new(AtomicI8::new(-1)),
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
        }
    }
}

/// Port of `UDPClient` class from udp_client.h / udp_client.cpp
pub struct UdpClient {
    engine: Arc<EngineMutex<InputEngine>>,
    pads: Arc<Mutex<Vec<PadData>>>,
    clients: Vec<ClientConnection>,
}

impl UdpClient {
    /// Port of UDPClient::UDPClient
    pub fn new(input_engine: String) -> Self {
        log::info!("Udp Initialization started");
        let mut client = Self {
            engine: Arc::new(EngineMutex::new(InputEngine::new(input_engine))),
            pads: Arc::new(Mutex::new(
                (0..MAX_UDP_CLIENTS * PADS_PER_CLIENT)
                    .map(|_| PadData::default())
                    .collect(),
            )),
            clients: (0..MAX_UDP_CLIENTS)
                .map(|_| ClientConnection::default())
                .collect(),
        };
        client.reload_sockets();
        client
    }

    pub fn engine(&self) -> Arc<EngineMutex<InputEngine>> {
        Arc::clone(&self.engine)
    }

    fn engine_name(&self) -> String {
        self.engine.lock().get_engine_name().to_string()
    }

    /// Port of UDPClient::ReloadSockets
    pub fn reload_sockets(&mut self) {
        self.reset();

        let servers = common::settings::values()
            .udp_input_servers
            .get_value()
            .clone();
        let mut client = 0;
        for server in servers.split(',').filter(|server| !server.is_empty()) {
            if client == MAX_UDP_CLIENTS {
                break;
            }
            let Some((host, port)) = parse_server(server) else {
                log::error!("Invalid UDP input server {server}");
                continue;
            };
            if self.get_client_number(host, port) != MAX_UDP_CLIENTS {
                log::error!("Duplicated UDP servers found");
                continue;
            }
            self.start_communication(client, host, port);
            client += 1;
        }
    }

    /// Port of UDPClient::GetInputDevices (override)
    pub fn get_input_devices(&self) -> Vec<ParamPackage> {
        let mut devices = Vec::new();
        if !*common::settings::values().enable_udp_controller.get_value() {
            return devices;
        }
        let pads = self.pads.lock().unwrap();
        for client in 0..self.clients.len() {
            if self.clients[client].active.load(Ordering::Acquire) != 1 {
                continue;
            }
            for index in 0..PADS_PER_CLIENT {
                let pad_index = client * PADS_PER_CLIENT + index;
                if !pads[pad_index].connected {
                    continue;
                }
                let pad_identifier = self.get_pad_identifier(pad_index);
                let mut identifier = ParamPackage::default();
                identifier.set_str("engine", self.engine_name());
                identifier.set_str("display", format!("UDP Controller {}", pad_identifier.pad));
                identifier.set_str("guid", pad_identifier.guid.raw_string());
                identifier.set_int("port", pad_identifier.port as i32);
                identifier.set_int("pad", pad_identifier.pad as i32);
                devices.push(identifier);
            }
        }
        devices
    }

    /// Port of UDPClient::GetButtonMappingForDevice (override)
    pub fn get_button_mapping_for_device(&self, params: &ParamPackage) -> ButtonMapping {
        // This list excludes any button that can't be really mapped
        const SWITCH_TO_DSU_BUTTON: [(native_button::Values, PadButton); 22] = [
            (native_button::Values::A, PadButton::Circle),
            (native_button::Values::B, PadButton::Cross),
            (native_button::Values::X, PadButton::Triangle),
            (native_button::Values::Y, PadButton::Square),
            (native_button::Values::Plus, PadButton::Options),
            (native_button::Values::Minus, PadButton::Share),
            (native_button::Values::DLeft, PadButton::Left),
            (native_button::Values::DUp, PadButton::Up),
            (native_button::Values::DRight, PadButton::Right),
            (native_button::Values::DDown, PadButton::Down),
            (native_button::Values::L, PadButton::L1),
            (native_button::Values::R, PadButton::R1),
            (native_button::Values::ZL, PadButton::L2),
            (native_button::Values::ZR, PadButton::R2),
            (native_button::Values::SLLeft, PadButton::L2),
            (native_button::Values::SRLeft, PadButton::R2),
            (native_button::Values::SLRight, PadButton::L2),
            (native_button::Values::SRRight, PadButton::R2),
            (native_button::Values::LStick, PadButton::L3),
            (native_button::Values::RStick, PadButton::R3),
            (native_button::Values::Home, PadButton::Home),
            (native_button::Values::Screenshot, PadButton::TouchHardPress),
        ];

        if !params.has("guid") || !params.has("port") || !params.has("pad") {
            return ButtonMapping::new();
        }

        let mut mapping = ButtonMapping::new();
        for &(switch_button, dsu_button) in &SWITCH_TO_DSU_BUTTON {
            let mut button_params = ParamPackage::default();
            button_params.set_str("engine", self.engine_name());
            button_params.set_str("guid", params.get_str("guid", ""));
            button_params.set_int("port", params.get_int("port", 0));
            button_params.set_int("pad", params.get_int("pad", 0));
            button_params.set_int("button", dsu_button as i32);
            mapping.insert(switch_button as i32, button_params);
        }

        mapping
    }

    /// Port of UDPClient::GetAnalogMappingForDevice (override)
    pub fn get_analog_mapping_for_device(&self, params: &ParamPackage) -> AnalogMapping {
        if !params.has("guid") || !params.has("port") || !params.has("pad") {
            return AnalogMapping::new();
        }

        let mut mapping = AnalogMapping::new();
        let mut left_analog_params = ParamPackage::default();
        left_analog_params.set_str("engine", self.engine_name());
        left_analog_params.set_str("guid", params.get_str("guid", ""));
        left_analog_params.set_int("port", params.get_int("port", 0));
        left_analog_params.set_int("pad", params.get_int("pad", 0));
        left_analog_params.set_int("axis_x", PadAxes::LeftStickX as i32);
        left_analog_params.set_int("axis_y", PadAxes::LeftStickY as i32);
        mapping.insert(native_analog::Values::LStick as i32, left_analog_params);

        let mut right_analog_params = ParamPackage::default();
        right_analog_params.set_str("engine", self.engine_name());
        right_analog_params.set_str("guid", params.get_str("guid", ""));
        right_analog_params.set_int("port", params.get_int("port", 0));
        right_analog_params.set_int("pad", params.get_int("pad", 0));
        right_analog_params.set_int("axis_x", PadAxes::RightStickX as i32);
        right_analog_params.set_int("axis_y", PadAxes::RightStickY as i32);
        mapping.insert(native_analog::Values::RStick as i32, right_analog_params);
        mapping
    }

    /// Port of UDPClient::GetMotionMappingForDevice (override)
    pub fn get_motion_mapping_for_device(&self, params: &ParamPackage) -> MotionMapping {
        if !params.has("guid") || !params.has("port") || !params.has("pad") {
            return MotionMapping::new();
        }

        let mut mapping = MotionMapping::new();
        let mut left_motion_params = ParamPackage::default();
        left_motion_params.set_str("engine", self.engine_name());
        left_motion_params.set_str("guid", params.get_str("guid", ""));
        left_motion_params.set_int("port", params.get_int("port", 0));
        left_motion_params.set_int("pad", params.get_int("pad", 0));
        left_motion_params.set_int("motion", 0);

        let mut right_motion_params = ParamPackage::default();
        right_motion_params.set_str("engine", self.engine_name());
        right_motion_params.set_str("guid", params.get_str("guid", ""));
        right_motion_params.set_int("port", params.get_int("port", 0));
        right_motion_params.set_int("pad", params.get_int("pad", 0));
        right_motion_params.set_int("motion", 0);

        mapping.insert(native_motion::Values::MotionLeft as i32, left_motion_params);
        mapping.insert(
            native_motion::Values::MotionRight as i32,
            right_motion_params,
        );
        mapping
    }

    /// Port of UDPClient::GetUIName (override)
    pub fn get_ui_name(&self, params: &ParamPackage) -> ButtonNames {
        if params.has("button") {
            return self.get_ui_button_name(params);
        }
        if params.has("axis") {
            return ButtonNames::Value;
        }
        if params.has("motion") {
            return ButtonNames::Engine;
        }
        ButtonNames::Invalid
    }

    /// Port of UDPClient::IsStickInverted (override)
    pub fn is_stick_inverted(&self, params: &ParamPackage) -> bool {
        if !params.has("guid") || !params.has("port") || !params.has("pad") {
            return false;
        }

        let x_axis = params.get_int("axis_x", 0) as u8;
        let y_axis = params.get_int("axis_y", 0) as u8;
        if x_axis != PadAxes::LeftStickY as u8 && x_axis != PadAxes::RightStickY as u8 {
            return false;
        }
        if y_axis != PadAxes::LeftStickX as u8 && y_axis != PadAxes::RightStickX as u8 {
            return false;
        }
        true
    }

    // ---- Private methods ----

    /// Port of UDPClient::Reset
    fn reset(&mut self) {
        for client in &mut self.clients {
            if let Some(handle) = client.thread.take() {
                client.active.store(-1, Ordering::Release);
                client.stop.store(true, Ordering::Release);
                let _ = handle.join();
            }
        }
    }

    /// Port of UDPClient::GetClientNumber
    fn get_client_number(&self, host: &str, port: u16) -> usize {
        for (client, conn) in self.clients.iter().enumerate() {
            if conn.active.load(Ordering::Acquire) == -1 {
                continue;
            }
            if conn.host == host && conn.port == port {
                return client;
            }
        }
        MAX_UDP_CLIENTS
    }

    /// Port of UDPClient::GetBatteryLevel
    fn get_battery_level(battery: response::Battery) -> BatteryLevel {
        match battery {
            response::Battery::Dying => BatteryLevel::Empty,
            response::Battery::Low => BatteryLevel::Critical,
            response::Battery::Medium => BatteryLevel::Low,
            response::Battery::High => BatteryLevel::Medium,
            response::Battery::Full | response::Battery::Charged => BatteryLevel::Full,
            response::Battery::None | response::Battery::Charging => BatteryLevel::Charging,
        }
    }

    /// Port of UDPClient::GetPadIdentifier
    fn get_pad_identifier(&self, pad_index: usize) -> PadIdentifier {
        let client = pad_index / PADS_PER_CLIENT;
        PadIdentifier {
            guid: self.clients[client].uuid,
            port: self.clients[client].port as usize,
            pad: pad_index,
        }
    }

    /// Port of UDPClient::GetHostUUID
    fn get_host_uuid(&self, host: &str) -> UUID {
        // In C++: parses IPv4, formats as hex UUID
        // "00000000-0000-0000-0000-0000" + hex(ip)
        let parts: Vec<&str> = host.split('.').collect();
        if parts.len() == 4 {
            let ip_val: u32 = parts
                .iter()
                .fold(0u32, |acc, p| (acc << 8) | p.parse::<u32>().unwrap_or(0));
            let hex_host = format!("00000000-0000-0000-0000-0000{:06x}", ip_val);
            UUID::from_string(&hex_host)
        } else {
            UUID::default()
        }
    }

    /// Port of UDPClient::GetUIButtonName
    fn get_ui_button_name(&self, params: &ParamPackage) -> ButtonNames {
        let button_raw = params.get_int("button", 0) as u32;
        match button_raw {
            x if x == PadButton::Left as u32 => ButtonNames::ButtonLeft,
            x if x == PadButton::Right as u32 => ButtonNames::ButtonRight,
            x if x == PadButton::Down as u32 => ButtonNames::ButtonDown,
            x if x == PadButton::Up as u32 => ButtonNames::ButtonUp,
            x if x == PadButton::L1 as u32 => ButtonNames::L1,
            x if x == PadButton::L2 as u32 => ButtonNames::L2,
            x if x == PadButton::L3 as u32 => ButtonNames::L3,
            x if x == PadButton::R1 as u32 => ButtonNames::R1,
            x if x == PadButton::R2 as u32 => ButtonNames::R2,
            x if x == PadButton::R3 as u32 => ButtonNames::R3,
            x if x == PadButton::Circle as u32 => ButtonNames::Circle,
            x if x == PadButton::Cross as u32 => ButtonNames::Cross,
            x if x == PadButton::Square as u32 => ButtonNames::Square,
            x if x == PadButton::Triangle as u32 => ButtonNames::Triangle,
            x if x == PadButton::Share as u32 => ButtonNames::Share,
            x if x == PadButton::Options as u32 => ButtonNames::Options,
            x if x == PadButton::Home as u32 => ButtonNames::Home,
            x if x == PadButton::Touch1 as u32
                || x == PadButton::Touch2 as u32
                || x == PadButton::TouchHardPress as u32 =>
            {
                ButtonNames::Touch
            }
            _ => ButtonNames::Undefined,
        }
    }

    /// Port of `UDPClient::StartCommunication`.
    fn start_communication(&mut self, client: usize, host: &str, port: u16) {
        log::info!("Starting communication with UDP input server on {host}:{port}");
        let uuid = self.get_host_uuid(host);
        let connection = &mut self.clients[client];
        connection.uuid = uuid;
        connection.host = host.to_string();
        connection.port = port;
        connection.active.store(0, Ordering::Release);
        connection.stop = Arc::new(AtomicBool::new(false));

        for index in 0..PADS_PER_CLIENT {
            let identifier = PadIdentifier {
                guid: uuid,
                port: port as usize,
                pad: client * PADS_PER_CLIENT + index,
            };
            let mut engine = self.engine.lock();
            engine.pre_set_controller(&identifier);
            engine.pre_set_motion(&identifier, 0);
        }

        let engine = Arc::clone(&self.engine);
        let pads = Arc::clone(&self.pads);
        let active = Arc::clone(&connection.active);
        let stop = Arc::clone(&connection.stop);
        let host = host.to_string();
        connection.thread = Some(thread::spawn(move || {
            socket_loop(&host, port, Arc::clone(&stop), move |data| {
                on_pad_data(&engine, &pads, &active, uuid, port, client, data);
            });
        }));
    }
}

fn parse_server(server: &str) -> Option<(&str, u16)> {
    // If the first getline reaches EOF, the second one leaves its token
    // unchanged; otherwise only the next colon-delimited token is parsed.
    let (host, port) = server.split_once(':').unwrap_or((server, server));
    // ReloadSockets uses strtol(base=0), checks only the terminating character,
    // then casts to u16. Preserve signs, leading whitespace, native long
    // overflow and truncation rather than rejecting values as unsigned Rust.
    let mut token = port.split(':').next()?.as_bytes().to_vec();
    token.push(0);
    let mut end = std::ptr::null_mut();
    // SAFETY: token is NUL terminated and remains alive for both the parse and
    // the end-pointer check. strtol returns an address within that buffer.
    let value = unsafe { libc::strtol(token.as_ptr().cast(), &mut end, 0) };
    if unsafe { *end } != 0 {
        return None;
    }
    Some((host, value as u16))
}

fn generate_client_id() -> u32 {
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    time ^ std::process::id()
}

/// Rust socket owner corresponding to upstream `CemuhookUDP::Socket` and
/// `SocketLoop`. A short read timeout makes `Stop` observable without sending a
/// synthetic packet to the socket.
fn socket_loop(
    host: &str,
    port: u16,
    stop: Arc<AtomicBool>,
    mut on_pad_data: impl FnMut(response::PadData),
) {
    socket_loop_until(host, port, stop, None, &mut on_pad_data);
}

fn socket_loop_until(
    host: &str,
    port: u16,
    stop: Arc<AtomicBool>,
    deadline: Option<Instant>,
    on_pad_data: &mut impl FnMut(response::PadData),
) {
    let host = host.parse::<Ipv4Addr>().unwrap_or_else(|_| {
        log::error!("Invalid IPv4 address \"{host}\" provided to socket");
        Ipv4Addr::UNSPECIFIED
    });
    let endpoint = SocketAddrV4::new(host, port);
    let Ok(socket) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else {
        log::error!("Failed to bind UDP input socket");
        return;
    };
    if let Err(error) = socket.set_read_timeout(Some(Duration::from_millis(100))) {
        log::error!("Failed to configure UDP input socket: {error}");
        return;
    }

    let client_id = generate_client_id();
    let port_request = udp_protocol::create_port_info_request(client_id);
    let pad_request = udp_protocol::create_pad_data_request(client_id);
    let mut next_send = Instant::now();
    let mut receive_buffer = [0u8; MAX_PACKET_SIZE];

    while !stop.load(Ordering::Acquire) && deadline.is_none_or(|deadline| Instant::now() < deadline)
    {
        let now = Instant::now();
        if now >= next_send {
            let _ = socket.send_to(&port_request, endpoint);
            let _ = socket.send_to(&pad_request, endpoint);
            next_send = now + Duration::from_secs(3);
        }

        match socket.recv_from(&mut receive_buffer) {
            Ok((size, _)) => {
                let packet = &mut receive_buffer[..size];
                match udp_protocol::response::validate(packet) {
                    Some(MessageType::Version) => {
                        if let Some(data) = udp_protocol::response::decode_version(packet) {
                            log::trace!("Version packet received: {}", data.version);
                        }
                    }
                    Some(MessageType::PortInfo) => {
                        if let Some(data) = udp_protocol::response::decode_port_info(packet) {
                            log::trace!("PortInfo packet received: {:?}", data.model);
                        }
                    }
                    Some(MessageType::PadData) => {
                        if let Some(data) = udp_protocol::response::decode_pad_data(packet) {
                            on_pad_data(data);
                        }
                    }
                    None => {}
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => {
                log::error!("UDP input receive failed: {error}");
                break;
            }
        }
    }
}

fn on_pad_data(
    engine: &Arc<EngineMutex<InputEngine>>,
    pads: &Arc<Mutex<Vec<PadData>>>,
    active: &Arc<AtomicI8>,
    uuid: UUID,
    port: u16,
    client: usize,
    data: response::PadData,
) {
    let pad_index = client * PADS_PER_CLIENT + data.info.id as usize;
    if pad_index >= MAX_UDP_CLIENTS * PADS_PER_CLIENT {
        log::error!("Invalid pad id {}", data.info.id);
        return;
    }

    let time_difference = {
        let mut pads = pads.lock().unwrap();
        let pad = &mut pads[pad_index];
        if data.packet_counter as u64 == pad.packet_sequence {
            log::warn!(
                "PadData packet dropped because its stale info. Current count: {} Packet count: {}",
                pad.packet_sequence,
                data.packet_counter
            );
            pad.connected = false;
            return;
        }
        active.store(1, Ordering::Release);
        pad.connected = true;
        pad.packet_sequence = data.packet_counter as u64;
        let now = Instant::now();
        let elapsed = now.duration_since(pad.last_update).as_micros() as u64;
        pad.last_update = now;
        elapsed
    };

    let identifier = PadIdentifier {
        guid: uuid,
        port: port as usize,
        pad: pad_index,
    };
    let gyro_scale = 1.0 / 312.0;
    let motion = BasicMotion {
        gyro_x: data.gyro.pitch * gyro_scale,
        gyro_y: data.gyro.roll * gyro_scale,
        gyro_z: -data.gyro.yaw * gyro_scale,
        accel_x: data.accel.x,
        accel_y: -data.accel.z,
        accel_z: data.accel.y,
        delta_timestamp: time_difference,
    };
    let callbacks = engine.lock().set_motion(&identifier, 0, &motion);
    callbacks.dispatch();

    let touch_param = common::param_package::ParamPackage::from_serialized(
        common::settings::values().touch_device.get_value(),
    );
    let min_x = touch_param.get_int("min_x", 100) as u16;
    let min_y = touch_param.get_int("min_y", 50) as u16;
    let max_x = touch_param.get_int("max_x", 1800) as u16;
    let max_y = touch_param.get_int("max_y", 850) as u16;
    for (id, touch) in data.touch.iter().enumerate() {
        let (axis_x, axis_y, button) = if id == 0 {
            (PadAxes::Touch1X, PadAxes::Touch1Y, PadButton::Touch1)
        } else {
            (PadAxes::Touch2X, PadAxes::Touch2Y, PadButton::Touch2)
        };
        let x = (touch.x.clamp(min_x, max_x) - min_x) as f32 / (max_x - min_x) as f32;
        let y = (touch.y.clamp(min_y, max_y) - min_y) as f32 / (max_y - min_y) as f32;
        let is_active = touch.is_active != 0;
        let callbacks =
            engine
                .lock()
                .set_axis(&identifier, axis_x as i32, if is_active { x } else { 0.0 });
        callbacks.dispatch();
        let callbacks =
            engine
                .lock()
                .set_axis(&identifier, axis_y as i32, if is_active { y } else { 0.0 });
        callbacks.dispatch();
        let callbacks = engine
            .lock()
            .set_button(&identifier, button as i32, is_active);
        callbacks.dispatch();
    }

    for (axis, value) in [
        (
            PadAxes::LeftStickX,
            (data.left_stick_x as f32 - 127.0) / 127.0,
        ),
        (
            PadAxes::LeftStickY,
            (data.left_stick_y as f32 - 127.0) / 127.0,
        ),
        (
            PadAxes::RightStickX,
            (data.right_stick_x as f32 - 127.0) / 127.0,
        ),
        (
            PadAxes::RightStickY,
            (data.right_stick_y as f32 - 127.0) / 127.0,
        ),
    ] {
        let callbacks = engine.lock().set_axis(&identifier, axis as i32, value);
        callbacks.dispatch();
    }

    const BUTTONS: [PadButton; 16] = [
        PadButton::Share,
        PadButton::L3,
        PadButton::R3,
        PadButton::Options,
        PadButton::Up,
        PadButton::Right,
        PadButton::Down,
        PadButton::Left,
        PadButton::L2,
        PadButton::R2,
        PadButton::L1,
        PadButton::R1,
        PadButton::Triangle,
        PadButton::Circle,
        PadButton::Cross,
        PadButton::Square,
    ];
    for (bit, button) in BUTTONS.into_iter().enumerate() {
        let callbacks = engine.lock().set_button(
            &identifier,
            button as i32,
            data.digital_button & (1 << bit) != 0,
        );
        callbacks.dispatch();
    }
    for (button, pressed) in [
        (PadButton::Home, data.home != 0),
        (PadButton::TouchHardPress, data.touch_hard_press != 0),
    ] {
        let callbacks = engine
            .lock()
            .set_button(&identifier, button as i32, pressed);
        callbacks.dispatch();
    }
    let battery = UdpClient::get_battery_level(data.info.battery);
    let callbacks = engine.lock().set_battery(&identifier, battery);
    callbacks.dispatch();
}

impl Drop for UdpClient {
    fn drop(&mut self) {
        self.reset();
    }
}

/// Port of `CalibrationConfigurationJob` class from udp_client.h
pub struct CalibrationConfigurationJob {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// Port of CalibrationConfigurationJob::Status enum from udp_client.h
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalibrationStatus {
    Initialized,
    Ready,
    Stage1Completed,
    Completed,
}

impl CalibrationConfigurationJob {
    /// Port of CalibrationConfigurationJob constructor
    ///
    /// In C++: spawns a thread that creates a Socket, listens for touch data,
    /// computes min/max calibration values, and calls status/data callbacks.
    /// This requires the UDP Socket implementation (boost::asio).
    pub fn new(
        host: &str,
        port: u16,
        status_callback: Box<dyn Fn(CalibrationStatus) + Send + 'static>,
        data_callback: Box<dyn Fn(u16, u16, u16, u16) + Send + 'static>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let host = host.to_string();
        let thread = thread::spawn(move || {
            let mut min_x = u16::MAX;
            let mut min_y = u16::MAX;
            let mut current_status = CalibrationStatus::Initialized;
            let callback_stop = Arc::clone(&thread_stop);
            socket_loop(&host, port, Arc::clone(&thread_stop), move |data| {
                const CALIBRATION_THRESHOLD: u16 = 100;
                if current_status == CalibrationStatus::Initialized {
                    current_status = CalibrationStatus::Ready;
                    status_callback(current_status);
                }
                let touch = data.touch[0].clone();
                if touch.is_active == 0 {
                    return;
                }
                log::debug!("Current touch: {} {}", touch.x, touch.y);
                min_x = min_x.min(touch.x);
                min_y = min_y.min(touch.y);
                if current_status == CalibrationStatus::Ready {
                    current_status = CalibrationStatus::Stage1Completed;
                    status_callback(current_status);
                }
                if touch.x.saturating_sub(min_x) > CALIBRATION_THRESHOLD
                    && touch.y.saturating_sub(min_y) > CALIBRATION_THRESHOLD
                {
                    current_status = CalibrationStatus::Completed;
                    data_callback(min_x, min_y, touch.x, touch.y);
                    status_callback(current_status);
                    callback_stop.store(true, Ordering::Release);
                }
            });
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }

    /// Port of CalibrationConfigurationJob::Stop
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for CalibrationConfigurationJob {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Port of TestCommunication free function from udp_client.h
///
/// In C++: spawns a thread that creates a Socket, waits for pad data,
/// and calls success/failure callbacks based on whether data arrives
/// within 10 seconds. Requires UDP socket implementation.
pub fn test_communication(
    host: &str,
    port: u16,
    success_callback: Box<dyn Fn() + Send + 'static>,
    failure_callback: Box<dyn Fn() + Send + 'static>,
) {
    let host = host.to_string();
    thread::spawn(move || {
        let stop = Arc::new(AtomicBool::new(false));
        let success = Arc::new(AtomicBool::new(false));
        let callback_stop = Arc::clone(&stop);
        let callback_success = Arc::clone(&success);
        socket_loop_until(
            &host,
            port,
            Arc::clone(&stop),
            Some(Instant::now() + Duration::from_secs(10)),
            &mut move |_| {
                callback_success.store(true, Ordering::Release);
                callback_stop.store(true, Ordering::Release);
            },
        );
        if success.load(Ordering::Acquire) {
            success_callback();
        } else {
            failure_callback();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn server_ports_preserve_strtol_conversion() {
        for (port, expected) in [
            ("26760", Some(26760)), ("+26760", Some(26760)),
            (" \t26760", Some(26760)), ("-1", Some(65535)),
            ("-0x1", Some(65535)), ("010", Some(8)),
            ("0X10", Some(16)), ("65536", Some(0)), ("", Some(0)),
            ("999999999999999999999999999", Some(65535)),
            ("-999999999999999999999999999", Some(0)),
            ("08", None), ("1 ", None), ("+", None), ("0x", None),
        ] {
            assert_eq!(parse_server(&format!("127.0.0.1:{port}")),
                expected.map(|value| ("127.0.0.1", value)), "{port:?}");
        }
        assert_eq!(parse_server("26760"), Some(("26760", 26760)));
        assert_eq!(parse_server("localhost"), None);
        assert_eq!(parse_server("localhost:1:2"), Some(("localhost", 1)));
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = u32::MAX;
        for &byte in data {
            crc ^= byte as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB_88320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    fn pad_response() -> Vec<u8> {
        let mut packet = vec![0u8; std::mem::size_of::<udp_protocol::Header>() + 80];
        packet[0..4].copy_from_slice(&udp_protocol::SERVER_MAGIC.to_le_bytes());
        packet[4..6].copy_from_slice(&udp_protocol::PROTOCOL_VERSION.to_le_bytes());
        packet[6..8].copy_from_slice(&84u16.to_le_bytes());
        packet[16..20].copy_from_slice(&(MessageType::PadData as u32).to_le_bytes());
        packet[21] = response::State::Connected as u8;
        packet[22] = response::Model::FullGyro as u8;
        packet[23] = response::ConnectionType::Usb as u8;
        packet[30] = response::Battery::Full as u8;
        packet[32..36].copy_from_slice(&1u32.to_le_bytes());
        let checksum = crc32(&packet);
        packet[8..12].copy_from_slice(&checksum.to_le_bytes());
        packet
    }

    #[test]
    fn communication_test_reports_a_valid_pad_packet() {
        let server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let port = server.local_addr().unwrap().port();
        let responder = thread::spawn(move || {
            let mut request = [0u8; MAX_PACKET_SIZE];
            let (_, peer) = server.recv_from(&mut request).unwrap();
            server.send_to(&pad_response(), peer).unwrap();
        });

        let (sender, receiver) = mpsc::channel();
        let success = sender.clone();
        test_communication(
            "127.0.0.1",
            port,
            Box::new(move || success.send(true).unwrap()),
            Box::new(move || sender.send(false).unwrap()),
        );
        assert_eq!(receiver.recv_timeout(Duration::from_secs(3)).unwrap(), true);
        responder.join().unwrap();
    }
}
