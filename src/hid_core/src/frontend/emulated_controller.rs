// SPDX-FileCopyrightText: Copyright 2021 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of hid_core/frontend/emulated_controller.h and emulated_controller.cpp

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use parking_lot::Mutex;

use common::input::{
    AnalogStatus, BatteryLevel, BodyColorStatus, ButtonStatus, CallbackStatus, CameraFormat,
    CameraStatus, DriverResult, InputCallback, InputDevice, MifareRequest, MotionStatus, NfcState,
    NfcStatus, OutputDevice, PollingMode, StickStatus, TriggerStatus, VibrationAmplificationType,
    VibrationStatus,
};
use common::param_package::ParamPackage;
use common::settings_input::{self, ControllerType};
use common::uuid::UUID;

use crate::frontend::input_converter::{
    transform_to_button, transform_to_camera, transform_to_motion, transform_to_nfc,
    transform_to_stick, transform_to_trigger,
};
use crate::frontend::motion_input::{
    MotionInput, IS_AT_REST_LOOSE, IS_AT_REST_STANDARD, IS_AT_REST_TIGHT, THRESHOLD_LOOSE,
    THRESHOLD_STANDARD, THRESHOLD_TIGHT,
};
use crate::hid_types::*;
use crate::irsensor::irs_types::ImageTransferProcessorFormat;

pub const MAX_EMULATED_CONTROLLERS: usize = 2;
pub const OUTPUT_DEVICES_SIZE: usize = 5;

pub const HID_JOYSTICK_MAX: i32 = 0x7fff;
pub const HID_TRIGGER_MAX: i32 = 0x7fff;
pub const TURBO_BUTTON_DELAY: u32 = 4;
// Use a common UUID for TAS and Virtual Gamepad.
const TAS_UUID: UUID = UUID::from_bytes([
    0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x7, 0xA5, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
]);
const VIRTUAL_UUID: UUID = UUID::from_bytes([
    0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x7, 0xFF, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
]);

static SIMPLE_NPAD_BUTTON_STATE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy)]
struct ScriptedNpadPress {
    start_ms: u64,
    duration_ms: u64,
    buttons: u64,
}

fn parse_u64_auto(value: &str) -> Option<u64> {
    let value = value.trim();
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).ok()
    } else {
        value.parse::<u64>().ok()
    }
}

fn scripted_npad_presses() -> &'static [ScriptedNpadPress] {
    static PRESSES: OnceLock<Vec<ScriptedNpadPress>> = OnceLock::new();
    PRESSES.get_or_init(|| {
        let Some(spec) = std::env::var("RUZU_SCRIPTED_NPAD").ok() else {
            return Vec::new();
        };
        spec.split(',')
            .filter_map(|entry| {
                let mut parts = entry.split(':');
                let start_ms = parse_u64_auto(parts.next()?)?;
                let buttons = parse_u64_auto(parts.next()?)?;
                let duration_ms = parts.next().and_then(parse_u64_auto).unwrap_or(250);
                Some(ScriptedNpadPress {
                    start_ms,
                    duration_ms,
                    buttons,
                })
            })
            .collect()
    })
}

fn scripted_npad_button_bits() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    let presses = scripted_npad_presses();
    if presses.is_empty() {
        return 0;
    }
    let elapsed_ms = START.get_or_init(Instant::now).elapsed().as_millis() as u64;
    presses.iter().fold(0u64, |bits, press| {
        if elapsed_ms >= press.start_ms
            && elapsed_ms < press.start_ms.saturating_add(press.duration_ms)
        {
            bits | press.buttons
        } else {
            bits
        }
    })
}

/// Temporary frontend bridge for the SDL command-line frontend while the full
/// upstream InputSubsystem -> EmulatedController callback wiring is incomplete.
pub fn set_simple_npad_button(button: NpadButton, pressed: bool) {
    if pressed {
        SIMPLE_NPAD_BUTTON_STATE.fetch_or(button.bits(), Ordering::Relaxed);
    } else {
        SIMPLE_NPAD_BUTTON_STATE.fetch_and(!button.bits(), Ordering::Relaxed);
    }
}

pub fn get_simple_npad_button_state() -> NpadButtonState {
    NpadButtonState {
        raw: NpadButton::from_bits_truncate(
            SIMPLE_NPAD_BUTTON_STATE.load(Ordering::Relaxed) | scripted_npad_button_bits(),
        ),
    }
}

/// Keeps the env-gated scripted stick-direction bits consistent with the
/// analog coordinates a real `EmulatedController::SetStick` update exposes.
pub fn apply_simple_npad_stick_buttons(sticks: &mut AnalogSticks, buttons: NpadButton) {
    let left_x = i32::from(buttons.contains(NpadButton::STICK_L_RIGHT))
        - i32::from(buttons.contains(NpadButton::STICK_L_LEFT));
    let left_y = i32::from(buttons.contains(NpadButton::STICK_L_UP))
        - i32::from(buttons.contains(NpadButton::STICK_L_DOWN));
    let right_x = i32::from(buttons.contains(NpadButton::STICK_R_RIGHT))
        - i32::from(buttons.contains(NpadButton::STICK_R_LEFT));
    let right_y = i32::from(buttons.contains(NpadButton::STICK_R_UP))
        - i32::from(buttons.contains(NpadButton::STICK_R_DOWN));

    let left_active = buttons.intersects(
        NpadButton::STICK_L_LEFT
            | NpadButton::STICK_L_UP
            | NpadButton::STICK_L_RIGHT
            | NpadButton::STICK_L_DOWN,
    );
    let right_active = buttons.intersects(
        NpadButton::STICK_R_LEFT
            | NpadButton::STICK_R_UP
            | NpadButton::STICK_R_RIGHT
            | NpadButton::STICK_R_DOWN,
    );
    if left_active {
        sticks.left.x = left_x * HID_JOYSTICK_MAX;
        sticks.left.y = left_y * HID_JOYSTICK_MAX;
    }
    if right_active {
        sticks.right.x = right_x * HID_JOYSTICK_MAX;
        sticks.right.y = right_y * HID_JOYSTICK_MAX;
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AnalogSticks {
    pub left: AnalogStickState,
    pub right: AnalogStickState,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ControllerColors {
    pub fullkey: NpadControllerColor,
    pub left: NpadControllerColor,
    pub right: NpadControllerColor,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BatteryLevelState {
    pub dual: NpadPowerInfo,
    pub left: NpadPowerInfo,
    pub right: NpadPowerInfo,
}

#[derive(Debug, Clone, Default)]
pub struct CameraState {
    pub format: ImageTransferProcessorFormat,
    pub data: Vec<u8>,
    pub sample: usize,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RingSensorForce {
    pub force: f32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ControllerMotion {
    pub accel: Vec3f,
    pub gyro: Vec3f,
    pub rotation: Vec3f,
    pub euler: Vec3f,
    pub orientation: [Vec3f; 3],
    pub is_at_rest: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ControllerMotionInfo {
    pub raw_status: MotionStatus,
    pub emulated: MotionInput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum EmulatedDeviceIndex {
    LeftIndex = 0,
    RightIndex = 1,
    DualIndex = 2,
    AllDevices = 3,
}

pub type MotionState = [ControllerMotion; 2];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControllerTriggerType {
    Button,
    Stick,
    Trigger,
    Motion,
    Color,
    Battery,
    Vibration,
    IrSensor,
    RingController,
    Nfc,
    Connected,
    Disconnected,
    Type,
    All,
}

pub struct ControllerUpdateCallback {
    pub on_change: Arc<dyn Fn(ControllerTriggerType) + Send + Sync>,
    pub is_npad_service: bool,
}

pub(crate) struct DeferredControllerCallback {
    callback: Arc<dyn Fn(ControllerTriggerType) + Send + Sync>,
    trigger_type: ControllerTriggerType,
}

impl DeferredControllerCallback {
    pub(crate) fn dispatch(self) {
        (self.callback)(self.trigger_type);
    }
}

/// State needed by input-device callbacks after `EmulatedController` has
/// handed them to a driver thread.
struct ControllerEventContext {
    npad_id_type: NpadIdType,
    is_connected: AtomicBool,
    supported_style_tag: Mutex<NpadStyleTag>,
    callback_list: Mutex<HashMap<i32, ControllerUpdateCallback>>,
}

fn trigger_on_change(
    context: &ControllerEventContext,
    trigger_type: ControllerTriggerType,
    is_npad_service_update: bool,
) {
    let callbacks: Vec<_> = context
        .callback_list
        .lock()
        .values()
        .filter(|callback| is_npad_service_update || !callback.is_npad_service)
        .map(|callback| Arc::clone(&callback.on_change))
        .collect();
    for callback in callbacks {
        callback(trigger_type);
    }
}

fn is_controller_supported(npad: NpadStyleIndex, supported: NpadStyleTag) -> bool {
    let styles = supported.raw;
    match npad {
        NpadStyleIndex::Fullkey => styles.contains(NpadStyleSet::FULLKEY),
        NpadStyleIndex::Handheld => styles.contains(NpadStyleSet::HANDHELD),
        NpadStyleIndex::JoyconDual => styles.contains(NpadStyleSet::JOY_DUAL),
        NpadStyleIndex::JoyconLeft => styles.contains(NpadStyleSet::JOY_LEFT),
        NpadStyleIndex::JoyconRight => styles.contains(NpadStyleSet::JOY_RIGHT),
        NpadStyleIndex::GameCube => styles.contains(NpadStyleSet::GC),
        NpadStyleIndex::Pokeball => styles.contains(NpadStyleSet::PALMA),
        NpadStyleIndex::NES => styles.contains(NpadStyleSet::LARK),
        NpadStyleIndex::SNES => styles.contains(NpadStyleSet::LUCIA),
        NpadStyleIndex::N64 => styles.contains(NpadStyleSet::LAGOON),
        NpadStyleIndex::SegaGenesis => styles.contains(NpadStyleSet::LAGER),
        _ => false,
    }
}

/// The raw device values behind `ControllerStatus`, upstream's
/// `button_values` / `stick_values` / `trigger_values`.
///
/// Upstream keeps these next to the HID-service state in one `ControllerStatus`
/// struct guarded by the controller's mutex. Here they live behind their own
/// `Arc<Mutex<..>>` because the input devices call back from SDL's thread and
/// must be able to write them without holding a borrow of the controller.
#[derive(Debug)]
pub struct ControllerStatus {
    // Data from input_common
    pub button_values: Vec<ButtonStatus>,
    pub stick_values: Vec<StickStatus>,
    pub motion_values: [ControllerMotionInfo; 2],
    pub trigger_values: Vec<TriggerStatus>,
    pub color_values: [BodyColorStatus; 2],
    pub battery_values: [BatteryLevel; 2],
    pub camera_values: CameraStatus,
    pub ring_analog_value: AnalogStatus,
    pub nfc_values: NfcStatus,

    // Data for HID services
    pub home_button_state: HomeButtonState,
    pub capture_button_state: CaptureButtonState,
    pub npad_button_state: NpadButtonState,
    pub debug_pad_button_state: DebugPadButton,
    pub analog_stick_state: AnalogSticks,
    pub motion_state: MotionState,
    pub gc_trigger_state: NpadGcTriggerState,
    pub colors_state: ControllerColors,
    pub battery_state: BatteryLevelState,
    pub camera_state: CameraState,
    pub ring_analog_state: RingSensorForce,
    pub nfc_state: NfcStatus,
    pub left_polling_mode: PollingMode,
    pub right_polling_mode: PollingMode,
    pub motion_sensitivity: f32,

    /// Mirrors the controller's `npad_type`, so `set_button` can apply
    /// upstream's GameCube special case without reaching back into it.
    pub npad_type: NpadStyleIndex,
    /// Mirrors `is_configuring`; upstream reports nothing to the HID services
    /// while the configuration dialog is open.
    pub is_configuring: bool,
    /// Mirrors `system_buttons_enabled`, which gates Home and Capture.
    pub system_buttons_enabled: bool,
}

impl ControllerStatus {
    fn new() -> Self {
        Self {
            button_values: vec![
                ButtonStatus::default();
                settings_input::native_button::NUM_BUTTONS
            ],
            stick_values: vec![StickStatus::default(); settings_input::native_analog::NUM_ANALOGS],
            motion_values: std::array::from_fn(|_| ControllerMotionInfo::default()),
            trigger_values: vec![
                TriggerStatus::default();
                settings_input::native_trigger::NUM_TRIGGERS
            ],
            color_values: [BodyColorStatus::default(); 2],
            battery_values: [BatteryLevel::default(); 2],
            camera_values: CameraStatus::default(),
            ring_analog_value: AnalogStatus::default(),
            nfc_values: NfcStatus::default(),
            home_button_state: HomeButtonState::default(),
            capture_button_state: CaptureButtonState::default(),
            npad_button_state: NpadButtonState::default(),
            debug_pad_button_state: DebugPadButton::default(),
            analog_stick_state: AnalogSticks::default(),
            motion_state: [ControllerMotion::default(); 2],
            gc_trigger_state: NpadGcTriggerState::default(),
            colors_state: ControllerColors::default(),
            battery_state: BatteryLevelState::default(),
            camera_state: CameraState::default(),
            ring_analog_state: RingSensorForce::default(),
            nfc_state: NfcStatus::default(),
            left_polling_mode: PollingMode::Active,
            right_polling_mode: PollingMode::Active,
            motion_sensitivity: IS_AT_REST_STANDARD,
            npad_type: NpadStyleIndex::None,
            is_configuring: false,
            system_buttons_enabled: true,
        }
    }
}

impl Default for ControllerStatus {
    fn default() -> Self {
        Self::new()
    }
}

/// Port of `EmulatedController::SetColors`.
fn set_colors(
    status: &Arc<Mutex<ControllerStatus>>,
    event_context: &Arc<ControllerEventContext>,
    callback: &CallbackStatus,
    index: usize,
) {
    if index >= 2 {
        return;
    }

    let mut status = status.lock();
    status.color_values[index] = callback.color_status;
    if status.is_configuring {
        drop(status);
        trigger_on_change(event_context, ControllerTriggerType::Color, false);
        return;
    }
    if status.color_values[index].body == 0 {
        return;
    }

    let color = status.color_values[index];
    status.colors_state.fullkey = NpadControllerColor {
        body: EmulatedController::get_npad_color(color.body),
        button: EmulatedController::get_npad_color(color.buttons),
    };
    if status.npad_type == NpadStyleIndex::Fullkey {
        status.colors_state.left = NpadControllerColor {
            body: EmulatedController::get_npad_color(color.left_grip),
            button: EmulatedController::get_npad_color(color.buttons),
        };
        status.colors_state.right = NpadControllerColor {
            body: EmulatedController::get_npad_color(color.right_grip),
            button: EmulatedController::get_npad_color(color.buttons),
        };
    } else if index == EmulatedDeviceIndex::LeftIndex as usize {
        status.colors_state.left = NpadControllerColor {
            body: EmulatedController::get_npad_color(color.body),
            button: EmulatedController::get_npad_color(color.buttons),
        };
    } else {
        status.colors_state.right = NpadControllerColor {
            body: EmulatedController::get_npad_color(color.body),
            button: EmulatedController::get_npad_color(color.buttons),
        };
    }
    drop(status);
    trigger_on_change(event_context, ControllerTriggerType::Color, true);
}

/// Port of `EmulatedController::SetBattery`.
fn set_battery(
    status: &Arc<Mutex<ControllerStatus>>,
    event_context: &Arc<ControllerEventContext>,
    callback: &CallbackStatus,
    index: usize,
) {
    if index >= 2 {
        return;
    }

    let mut status = status.lock();
    status.battery_values[index] = callback.battery_status;
    if !status.is_configuring {
        let (is_powered, is_charging, battery_level) = match callback.battery_status {
            BatteryLevel::Charging => (true, true, NpadBatteryLevel::Full),
            BatteryLevel::Medium => (false, false, NpadBatteryLevel::High),
            BatteryLevel::Low => (false, false, NpadBatteryLevel::Low),
            BatteryLevel::Critical => (false, false, NpadBatteryLevel::Critical),
            BatteryLevel::Empty => (false, false, NpadBatteryLevel::Empty),
            BatteryLevel::None | BatteryLevel::Full => (true, false, NpadBatteryLevel::Full),
        };
        let power_info = NpadPowerInfo {
            is_powered,
            is_charging,
            _padding: [0; 6],
            battery_level,
        };
        if index == EmulatedDeviceIndex::LeftIndex as usize {
            status.battery_state.left = power_info;
        } else {
            status.battery_state.right = power_info;
        }
    }
    let service_update = !status.is_configuring;
    drop(status);
    trigger_on_change(
        event_context,
        ControllerTriggerType::Battery,
        service_update,
    );
}

/// Port of EmulatedController::SetButton.
///
/// Free-standing because the input devices call it from the driver's thread and
/// cannot borrow the controller; everything it touches lives in the shared
/// `ControllerStatus`, including the HID-service state the guest reads.
fn set_button(
    status: &Arc<Mutex<ControllerStatus>>,
    event_context: &Arc<ControllerEventContext>,
    callback: &CallbackStatus,
    index: usize,
    uuid: UUID,
) {
    use settings_input::native_button::Values as NB;

    let new_status = transform_to_button(callback);
    let mut status = status.lock();
    let Some(current_status) = status.button_values.get_mut(index) else {
        return;
    };

    // Only read button values that have the same uuid or are pressed once.
    if current_status.uuid != uuid && !new_status.value {
        return;
    }

    current_status.toggle = new_status.toggle;
    current_status.turbo = new_status.turbo;
    current_status.uuid = uuid;

    let mut value_changed = false;
    if !current_status.toggle {
        current_status.locked = false;
        if current_status.value != new_status.value {
            current_status.value = new_status.value;
            value_changed = true;
        }
    } else {
        // Toggle button and lock status.
        if new_status.value && !current_status.locked {
            current_status.locked = true;
            current_status.value = !current_status.value;
            value_changed = true;
        }
        // Unlock button ready for the next press.
        if !new_status.value && current_status.locked {
            current_status.locked = false;
        }
    }

    if !value_changed {
        return;
    }
    let value = current_status.value;

    if status.is_configuring {
        status.npad_button_state.raw = NpadButton::empty();
        status.debug_pad_button_state.raw = 0;
        status.home_button_state.raw = 0;
        status.capture_button_state.raw = 0;
        drop(status);
        trigger_on_change(event_context, ControllerTriggerType::Button, false);
        return;
    }

    // GC controllers have triggers, not buttons, on ZL and ZR.
    if status.npad_type == NpadStyleIndex::GameCube
        && (index == NB::ZL as usize || index == NB::ZR as usize)
    {
        return;
    }

    let system_buttons_enabled = status.system_buttons_enabled;
    let assign = |raw: &mut NpadButton, flag: NpadButton| {
        raw.set(flag, value);
    };
    let assign_debug = |raw: &mut u32, bit: u32| {
        if value {
            *raw |= 1u32 << bit;
        } else {
            *raw &= !(1u32 << bit);
        }
    };

    // Upstream's switch, in the same order. The debug pad shares the first
    // eleven bits with `DebugPadButton`.
    match index {
        i if i == NB::A as usize => {
            assign(&mut status.npad_button_state.raw, NpadButton::A);
            assign_debug(&mut status.debug_pad_button_state.raw, 0);
        }
        i if i == NB::B as usize => {
            assign(&mut status.npad_button_state.raw, NpadButton::B);
            assign_debug(&mut status.debug_pad_button_state.raw, 1);
        }
        i if i == NB::X as usize => {
            assign(&mut status.npad_button_state.raw, NpadButton::X);
            assign_debug(&mut status.debug_pad_button_state.raw, 2);
        }
        i if i == NB::Y as usize => {
            assign(&mut status.npad_button_state.raw, NpadButton::Y);
            assign_debug(&mut status.debug_pad_button_state.raw, 3);
        }
        i if i == NB::LStick as usize => {
            assign(&mut status.npad_button_state.raw, NpadButton::STICK_L);
        }
        i if i == NB::RStick as usize => {
            assign(&mut status.npad_button_state.raw, NpadButton::STICK_R);
        }
        i if i == NB::L as usize => {
            assign(&mut status.npad_button_state.raw, NpadButton::L);
            assign_debug(&mut status.debug_pad_button_state.raw, 4);
        }
        i if i == NB::R as usize => {
            assign(&mut status.npad_button_state.raw, NpadButton::R);
            assign_debug(&mut status.debug_pad_button_state.raw, 5);
        }
        i if i == NB::ZL as usize => {
            assign(&mut status.npad_button_state.raw, NpadButton::ZL);
            assign_debug(&mut status.debug_pad_button_state.raw, 6);
        }
        i if i == NB::ZR as usize => {
            assign(&mut status.npad_button_state.raw, NpadButton::ZR);
            assign_debug(&mut status.debug_pad_button_state.raw, 7);
        }
        i if i == NB::Plus as usize => {
            assign(&mut status.npad_button_state.raw, NpadButton::PLUS);
            assign_debug(&mut status.debug_pad_button_state.raw, 8);
        }
        i if i == NB::Minus as usize => {
            assign(&mut status.npad_button_state.raw, NpadButton::MINUS);
            assign_debug(&mut status.debug_pad_button_state.raw, 9);
        }
        i if i == NB::DLeft as usize => {
            assign(&mut status.npad_button_state.raw, NpadButton::LEFT);
            assign_debug(&mut status.debug_pad_button_state.raw, 10);
        }
        i if i == NB::DUp as usize => {
            assign(&mut status.npad_button_state.raw, NpadButton::UP);
            assign_debug(&mut status.debug_pad_button_state.raw, 11);
        }
        i if i == NB::DRight as usize => {
            assign(&mut status.npad_button_state.raw, NpadButton::RIGHT);
            assign_debug(&mut status.debug_pad_button_state.raw, 12);
        }
        i if i == NB::DDown as usize => {
            assign(&mut status.npad_button_state.raw, NpadButton::DOWN);
            assign_debug(&mut status.debug_pad_button_state.raw, 13);
        }
        i if i == NB::SLLeft as usize => {
            assign(&mut status.npad_button_state.raw, NpadButton::LEFT_SL);
        }
        i if i == NB::SLRight as usize => {
            assign(&mut status.npad_button_state.raw, NpadButton::RIGHT_SL);
        }
        i if i == NB::SRLeft as usize => {
            assign(&mut status.npad_button_state.raw, NpadButton::LEFT_SR);
        }
        i if i == NB::SRRight as usize => {
            assign(&mut status.npad_button_state.raw, NpadButton::RIGHT_SR);
        }
        i if i == NB::Home as usize => {
            if system_buttons_enabled {
                status.home_button_state.raw = u64::from(value);
            }
        }
        i if i == NB::Screenshot as usize => {
            if system_buttons_enabled {
                status.capture_button_state.raw = u64::from(value);
            }
        }
        _ => {}
    }

    let npad_type = status.npad_type;
    drop(status);

    if !event_context.is_connected.load(Ordering::Relaxed) {
        let should_connect = (event_context.npad_id_type == NpadIdType::Player1
            && npad_type != NpadStyleIndex::Handheld)
            || (event_context.npad_id_type == NpadIdType::Handheld
                && npad_type == NpadStyleIndex::Handheld);
        let supported =
            is_controller_supported(npad_type, *event_context.supported_style_tag.lock());
        if should_connect && supported && !event_context.is_connected.swap(true, Ordering::Relaxed)
        {
            trigger_on_change(event_context, ControllerTriggerType::Connected, true);
        }
    }
    trigger_on_change(event_context, ControllerTriggerType::Button, true);
}

/// Port of EmulatedController::SetStick.
fn set_stick(
    status: &Arc<Mutex<ControllerStatus>>,
    event_context: &Arc<ControllerEventContext>,
    callback: &CallbackStatus,
    index: usize,
    uuid: UUID,
) {
    use settings_input::native_analog::Values as NA;

    let stick_value = transform_to_stick(callback);
    let mut status = status.lock();
    let Some(current) = status.stick_values.get_mut(index) else {
        return;
    };

    // Only read stick values that have the same uuid or are over the threshold,
    // to avoid two devices flapping against each other.
    if current.uuid != uuid {
        let is_tas = uuid == TAS_UUID;
        if (is_tas && stick_value.x.value == 0.0 && stick_value.y.value == 0.0)
            || (!is_tas
                && !stick_value.down
                && !stick_value.up
                && !stick_value.left
                && !stick_value.right)
        {
            return;
        }
    }

    *current = stick_value;
    current.uuid = uuid;
    let (x, y) = (current.x.value, current.y.value);
    let (left, right, up, down) = (current.left, current.right, current.up, current.down);

    if status.is_configuring {
        status.analog_stick_state.left = AnalogStickState::default();
        status.analog_stick_state.right = AnalogStickState::default();
        drop(status);
        trigger_on_change(event_context, ControllerTriggerType::Stick, false);
        return;
    }

    let stick = AnalogStickState {
        x: (x * HID_JOYSTICK_MAX as f32) as i32,
        y: (y * HID_JOYSTICK_MAX as f32) as i32,
    };
    let raw = &mut status.npad_button_state.raw;
    if index == NA::LStick as usize {
        raw.set(NpadButton::STICK_L_LEFT, left);
        raw.set(NpadButton::STICK_L_UP, up);
        raw.set(NpadButton::STICK_L_RIGHT, right);
        raw.set(NpadButton::STICK_L_DOWN, down);
        status.analog_stick_state.left = stick;
    } else if index == NA::RStick as usize {
        raw.set(NpadButton::STICK_R_LEFT, left);
        raw.set(NpadButton::STICK_R_UP, up);
        raw.set(NpadButton::STICK_R_RIGHT, right);
        raw.set(NpadButton::STICK_R_DOWN, down);
        status.analog_stick_state.right = stick;
    }
    drop(status);
    trigger_on_change(event_context, ControllerTriggerType::Stick, true);
}

/// Port of EmulatedController::SetTrigger.
fn set_trigger(
    status: &Arc<Mutex<ControllerStatus>>,
    event_context: &Arc<ControllerEventContext>,
    callback: &CallbackStatus,
    index: usize,
    uuid: UUID,
) {
    let trigger_value = transform_to_trigger(callback);
    let mut status = status.lock();
    let Some(current) = status.trigger_values.get_mut(index) else {
        return;
    };

    // Only read trigger values that have the same uuid or are pressed once.
    if current.uuid != uuid && !trigger_value.pressed.value {
        let is_service_update = !status.is_configuring;
        drop(status);
        trigger_on_change(
            event_context,
            ControllerTriggerType::Trigger,
            is_service_update,
        );
        return;
    }

    *current = trigger_value;
    current.uuid = uuid;
    let analog = current.analog.value;
    let pressed = current.pressed.value;

    if status.is_configuring {
        status.gc_trigger_state.left = 0;
        status.gc_trigger_state.right = 0;
        drop(status);
        trigger_on_change(event_context, ControllerTriggerType::Trigger, false);
        return;
    }

    // Only GC controllers have analog triggers.
    if status.npad_type != NpadStyleIndex::GameCube {
        return;
    }

    let scaled = (analog * HID_TRIGGER_MAX as f32) as i32;
    if index == EmulatedDeviceIndex::LeftIndex as usize {
        status.gc_trigger_state.left = scaled;
        status.npad_button_state.raw.set(NpadButton::ZL, pressed);
    } else if index == EmulatedDeviceIndex::RightIndex as usize {
        status.gc_trigger_state.right = scaled;
        status.npad_button_state.raw.set(NpadButton::ZR, pressed);
    }
    drop(status);
    trigger_on_change(event_context, ControllerTriggerType::Trigger, true);
}

/// Port of `EmulatedController::SetMotion`.
fn set_motion(
    status: &Arc<Mutex<ControllerStatus>>,
    event_context: &Arc<ControllerEventContext>,
    callback: &CallbackStatus,
    index: usize,
) {
    let mut status = status.lock();
    if index >= status.motion_values.len() {
        return;
    }
    let raw_status = transform_to_motion(callback);
    let motion_sensitivity = status.motion_sensitivity;
    let motion_value = &mut status.motion_values[index];
    motion_value.raw_status = raw_status;
    motion_value.emulated.set_acceleration(Vec3f {
        x: raw_status.accel.x.value,
        y: raw_status.accel.y.value,
        z: raw_status.accel.z.value,
    });
    motion_value.emulated.set_gyroscope(Vec3f {
        x: raw_status.gyro.x.value,
        y: raw_status.gyro.y.value,
        z: raw_status.gyro.z.value,
    });
    motion_value
        .emulated
        .set_user_gyro_threshold(raw_status.gyro.x.properties.threshold);
    motion_value
        .emulated
        .update_rotation(raw_status.delta_timestamp);
    motion_value
        .emulated
        .update_orientation(raw_status.delta_timestamp);

    status.motion_state[index] = ControllerMotion {
        accel: motion_value.emulated.get_acceleration(),
        gyro: motion_value.emulated.get_gyroscope(),
        rotation: motion_value.emulated.get_rotations(),
        euler: motion_value.emulated.get_euler_angles(),
        orientation: motion_value.emulated.get_orientation(),
        is_at_rest: !motion_value.emulated.is_moving(motion_sensitivity),
    };
    let service_update = !status.is_configuring;
    drop(status);
    trigger_on_change(event_context, ControllerTriggerType::Motion, service_update);
}

/// Port of `EmulatedController::SetCamera`.
fn set_camera(
    status: &Arc<Mutex<ControllerStatus>>,
    event_context: &Arc<ControllerEventContext>,
    callback: &CallbackStatus,
) {
    let mut status = status.lock();
    status.camera_values = transform_to_camera(callback);
    if !status.is_configuring {
        status.camera_state.sample += 1;
        status.camera_state.format = camera_format_to_irs(status.camera_values.format);
        status.camera_state.data = status.camera_values.data.clone();
    }
    let service_update = !status.is_configuring;
    drop(status);
    trigger_on_change(
        event_context,
        ControllerTriggerType::IrSensor,
        service_update,
    );
}

/// Port of `EmulatedController::SetRingAnalog`.
fn set_ring_analog(
    status: &Arc<Mutex<ControllerStatus>>,
    event_context: &Arc<ControllerEventContext>,
    callback: &CallbackStatus,
) {
    let mut status = status.lock();
    let force_value = transform_to_stick(callback);
    status.ring_analog_value = force_value.x;
    if !status.is_configuring {
        status.ring_analog_state.force = force_value.x.value;
    }
    let service_update = !status.is_configuring;
    drop(status);
    trigger_on_change(
        event_context,
        ControllerTriggerType::RingController,
        service_update,
    );
}

/// Port of `EmulatedController::SetNfc`.
fn set_nfc(
    status: &Arc<Mutex<ControllerStatus>>,
    event_context: &Arc<ControllerEventContext>,
    callback: &CallbackStatus,
) {
    let mut status = status.lock();
    status.nfc_values = transform_to_nfc(callback);
    if !status.is_configuring {
        status.nfc_state = status.nfc_values.clone();
    }
    let service_update = !status.is_configuring;
    drop(status);
    trigger_on_change(event_context, ControllerTriggerType::Nfc, service_update);
}

fn camera_format_to_irs(format: CameraFormat) -> ImageTransferProcessorFormat {
    match format {
        CameraFormat::Size320x240 => ImageTransferProcessorFormat::Size320x240,
        CameraFormat::Size160x120 => ImageTransferProcessorFormat::Size160x120,
        CameraFormat::Size80x60 => ImageTransferProcessorFormat::Size80x60,
        CameraFormat::Size40x30 => ImageTransferProcessorFormat::Size40x30,
        CameraFormat::Size20x15 => ImageTransferProcessorFormat::Size20x15,
        CameraFormat::None => ImageTransferProcessorFormat::None,
    }
}

fn irs_format_to_camera(format: ImageTransferProcessorFormat) -> CameraFormat {
    match format {
        ImageTransferProcessorFormat::Size320x240 => CameraFormat::Size320x240,
        ImageTransferProcessorFormat::Size160x120 => CameraFormat::Size160x120,
        ImageTransferProcessorFormat::Size80x60 => CameraFormat::Size80x60,
        ImageTransferProcessorFormat::Size40x30 => CameraFormat::Size40x30,
        ImageTransferProcessorFormat::Size20x15 => CameraFormat::Size20x15,
        ImageTransferProcessorFormat::None => CameraFormat::None,
    }
}

pub struct EmulatedController {
    npad_id_type: NpadIdType,
    npad_type: NpadStyleIndex,
    original_npad_type: NpadStyleIndex,
    is_configuring: bool,
    is_initialized: bool,
    system_buttons_enabled: bool,
    turbo_button_state: u32,
    nfc_handles: usize,
    last_vibration_value: [VibrationValue; 2],
    last_vibration_timepoint: [Option<Instant>; 2],

    // Temporary values to avoid doing changes while the controller is in configuring mode
    tmp_npad_type: NpadStyleIndex,
    tmp_is_connected: bool,

    mutex: Mutex<()>,
    event_context: Arc<ControllerEventContext>,
    last_callback_key: i32,
    defer_callback_dispatch: bool,
    deferred_callbacks: Vec<DeferredControllerCallback>,

    // The parameters each input device is built from — upstream's
    // `button_params`, `stick_params`, `motion_params`, `trigger_params`,
    // `ring_params`.
    button_params: Vec<ParamPackage>,
    stick_params: Vec<ParamPackage>,
    motion_params: Vec<ParamPackage>,
    trigger_params: Vec<ParamPackage>,
    battery_params: [ParamPackage; 2],
    color_params: [ParamPackage; 2],
    camera_params: [ParamPackage; 2],
    ring_params: [ParamPackage; 2],
    nfc_params: [ParamPackage; 2],
    output_params: Vec<ParamPackage>,
    virtual_button_params: Vec<ParamPackage>,
    virtual_stick_params: Vec<ParamPackage>,
    virtual_motion_params: Vec<ParamPackage>,

    // The live devices, kept alive so their callbacks keep firing. Upstream's
    // `button_devices` / `stick_devices` / `trigger_devices`.
    button_devices: Vec<Box<dyn InputDevice>>,
    stick_devices: Vec<Box<dyn InputDevice>>,
    motion_devices: Vec<Box<dyn InputDevice>>,
    trigger_devices: Vec<Box<dyn InputDevice>>,
    battery_devices: Vec<Box<dyn InputDevice>>,
    color_devices: Vec<Box<dyn InputDevice>>,
    camera_devices: Vec<Box<dyn InputDevice>>,
    ring_analog_devices: Vec<Box<dyn InputDevice>>,
    nfc_devices: Vec<Box<dyn InputDevice>>,
    output_devices: Vec<Box<dyn OutputDevice>>,
    virtual_button_devices: Vec<Box<dyn InputDevice>>,
    virtual_stick_devices: Vec<Box<dyn InputDevice>>,
    virtual_motion_devices: Vec<Box<dyn InputDevice>>,

    /// The controller's status, shared with the device callbacks: they run on
    /// the driver's thread and cannot borrow the controller itself.
    status: Arc<Mutex<ControllerStatus>>,
}

fn vibration_status(vibration: VibrationValue, strength: f32) -> VibrationStatus {
    VibrationStatus {
        low_amplitude: (vibration.low_amplitude * strength).min(1.0),
        low_frequency: vibration.low_frequency,
        high_amplitude: (vibration.high_amplitude * strength).min(1.0),
        high_frequency: vibration.high_frequency,
        amplification_type: if strength > 0.7 {
            VibrationAmplificationType::Exponential
        } else {
            VibrationAmplificationType::Linear
        },
    }
}

impl EmulatedController {
    /// Port of EmulatedController::MapSettingsTypeToNPad.
    pub fn map_settings_type_to_npad(controller_type: ControllerType) -> NpadStyleIndex {
        match controller_type {
            ControllerType::ProController => NpadStyleIndex::Fullkey,
            ControllerType::DualJoyconDetached => NpadStyleIndex::JoyconDual,
            ControllerType::LeftJoycon => NpadStyleIndex::JoyconLeft,
            ControllerType::RightJoycon => NpadStyleIndex::JoyconRight,
            ControllerType::Handheld => NpadStyleIndex::Handheld,
            ControllerType::GameCube => NpadStyleIndex::GameCube,
            ControllerType::Pokeball => NpadStyleIndex::Pokeball,
            ControllerType::NES => NpadStyleIndex::NES,
            ControllerType::SNES => NpadStyleIndex::SNES,
            ControllerType::N64 => NpadStyleIndex::N64,
            ControllerType::SegaGenesis => NpadStyleIndex::SegaGenesis,
        }
    }

    /// Port of EmulatedController::MapNPadToSettingsType.
    pub fn map_npad_to_settings_type(npad_type: NpadStyleIndex) -> ControllerType {
        match npad_type {
            NpadStyleIndex::Fullkey => ControllerType::ProController,
            NpadStyleIndex::JoyconDual => ControllerType::DualJoyconDetached,
            NpadStyleIndex::JoyconLeft => ControllerType::LeftJoycon,
            NpadStyleIndex::JoyconRight => ControllerType::RightJoycon,
            NpadStyleIndex::Handheld => ControllerType::Handheld,
            NpadStyleIndex::GameCube => ControllerType::GameCube,
            NpadStyleIndex::Pokeball => ControllerType::Pokeball,
            NpadStyleIndex::NES => ControllerType::NES,
            NpadStyleIndex::SNES => ControllerType::SNES,
            NpadStyleIndex::N64 => ControllerType::N64,
            NpadStyleIndex::SegaGenesis => ControllerType::SegaGenesis,
            _ => ControllerType::ProController,
        }
    }

    pub fn new(npad_id_type: NpadIdType) -> Self {
        let event_context = Arc::new(ControllerEventContext {
            npad_id_type,
            is_connected: AtomicBool::new(false),
            supported_style_tag: Mutex::new(NpadStyleTag {
                raw: NpadStyleSet::ALL,
            }),
            callback_list: Mutex::new(HashMap::new()),
        });
        Self {
            npad_id_type,
            npad_type: NpadStyleIndex::None,
            original_npad_type: NpadStyleIndex::None,
            is_configuring: false,
            is_initialized: false,
            system_buttons_enabled: true,
            turbo_button_state: 0,
            nfc_handles: 0,
            last_vibration_value: [DEFAULT_VIBRATION_VALUE; 2],
            last_vibration_timepoint: [None; 2],
            tmp_npad_type: NpadStyleIndex::None,
            tmp_is_connected: false,
            mutex: Mutex::new(()),
            event_context,
            last_callback_key: 0,
            defer_callback_dispatch: false,
            deferred_callbacks: Vec::new(),
            button_params: vec![
                ParamPackage::default();
                settings_input::native_button::NUM_BUTTONS
            ],
            stick_params: vec![ParamPackage::default(); settings_input::native_analog::NUM_ANALOGS],
            motion_params: vec![
                ParamPackage::default();
                settings_input::native_motion::NUM_MOTIONS
            ],
            trigger_params: vec![
                ParamPackage::default();
                settings_input::native_trigger::NUM_TRIGGERS
            ],
            battery_params: std::array::from_fn(|_| ParamPackage::default()),
            color_params: std::array::from_fn(|_| ParamPackage::default()),
            camera_params: std::array::from_fn(|_| ParamPackage::default()),
            ring_params: std::array::from_fn(|_| ParamPackage::default()),
            nfc_params: std::array::from_fn(|_| ParamPackage::default()),
            output_params: vec![ParamPackage::default(); OUTPUT_DEVICES_SIZE],
            virtual_button_params: vec![
                ParamPackage::default();
                settings_input::native_button::NUM_BUTTONS
            ],
            virtual_stick_params: vec![
                ParamPackage::default();
                settings_input::native_analog::NUM_ANALOGS
            ],
            virtual_motion_params: vec![
                ParamPackage::default();
                settings_input::native_motion::NUM_MOTIONS
            ],
            button_devices: Vec::new(),
            stick_devices: Vec::new(),
            motion_devices: Vec::new(),
            trigger_devices: Vec::new(),
            battery_devices: Vec::new(),
            color_devices: Vec::new(),
            camera_devices: Vec::new(),
            ring_analog_devices: Vec::new(),
            nfc_devices: Vec::new(),
            output_devices: Vec::new(),
            virtual_button_devices: Vec::new(),
            virtual_stick_devices: Vec::new(),
            virtual_motion_devices: Vec::new(),
            status: Arc::new(Mutex::new(ControllerStatus::new())),
        }
    }

    pub fn get_npad_id_type(&self) -> NpadIdType {
        self.npad_id_type
    }

    pub fn set_npad_style_index(&mut self, npad_type: NpadStyleIndex) {
        let _lock = self.mutex.lock();
        if self.is_configuring {
            if self.tmp_npad_type == npad_type {
                return;
            }
            self.tmp_npad_type = npad_type;
        } else {
            if self.npad_type == npad_type {
                return;
            }
            if self.event_context.is_connected.load(Ordering::Relaxed) {
                log::warn!(
                    "Controller {:?} type changed while it is connected",
                    self.npad_id_type
                );
            }
            self.npad_type = npad_type;
        }
        // `set_button` and `set_trigger` need the type to apply upstream's
        // GameCube special cases without reaching back into the controller.
        self.status.lock().npad_type = npad_type;
        drop(_lock);
        self.trigger_on_change(ControllerTriggerType::Type, !self.is_configuring);
    }

    pub fn get_npad_style_index(&self, get_temporary_value: bool) -> NpadStyleIndex {
        let _lock = self.mutex.lock();
        if get_temporary_value && self.is_configuring {
            self.tmp_npad_type
        } else {
            self.npad_type
        }
    }

    pub fn set_supported_npad_style_tag(&mut self, supported_styles: NpadStyleTag) {
        *self.event_context.supported_style_tag.lock() = supported_styles;
        if !self.is_connected(false) {
            return;
        }

        // Attempt to reconnect with the originally configured type first.
        if self.npad_type != self.original_npad_type {
            self.disconnect();
            let current_npad_type = self.npad_type;
            self.set_npad_style_index(self.original_npad_type);
            if self.is_controller_supported(false) {
                self.connect(false);
                return;
            }
            self.set_npad_style_index(current_npad_type);
            self.connect(false);
        }

        if self.is_controller_supported(false) {
            return;
        }

        self.disconnect();

        if self.is_controller_fullkey(false) && supported_styles.raw.contains(NpadStyleSet::FULLKEY)
        {
            log::warn!(
                "Reconnecting controller type {:?} as Pro controller",
                self.npad_type
            );
            self.set_npad_style_index(NpadStyleIndex::Fullkey);
            self.connect(false);
            return;
        }

        if self.npad_type == NpadStyleIndex::JoyconDual
            && supported_styles.raw.contains(NpadStyleSet::FULLKEY)
        {
            log::warn!(
                "Reconnecting controller type {:?} as Pro controller",
                self.npad_type
            );
            self.set_npad_style_index(NpadStyleIndex::Fullkey);
            self.connect(false);
            return;
        }

        if self.npad_type == NpadStyleIndex::Fullkey
            && supported_styles.raw.contains(NpadStyleSet::JOY_DUAL)
        {
            log::warn!(
                "Reconnecting controller type {:?} as Dual Joycons",
                self.npad_type
            );
            self.set_npad_style_index(NpadStyleIndex::JoyconDual);
            self.connect(false);
            return;
        }

        log::error!(
            "Controller type {:?} is not supported. Disconnecting controller",
            self.npad_type
        );
    }

    pub fn connect(&mut self, use_temporary_value: bool) {
        if !self.is_controller_supported(use_temporary_value) {
            let npad_type = if self.is_configuring && use_temporary_value {
                self.tmp_npad_type
            } else {
                self.npad_type
            };
            log::error!("Controller type {:?} is not supported", npad_type);
            return;
        }

        let _lock = self.mutex.lock();
        if self.is_configuring {
            if self.tmp_is_connected {
                return;
            }
            self.tmp_is_connected = true;
            drop(_lock);
            self.trigger_on_change(ControllerTriggerType::Connected, false);
            return;
        }
        if self
            .event_context
            .is_connected
            .swap(true, Ordering::Relaxed)
        {
            return;
        }
        drop(_lock);
        self.trigger_on_change(ControllerTriggerType::Connected, true);
    }

    pub fn disconnect(&mut self) {
        let _lock = self.mutex.lock();
        if self.is_configuring {
            if !self.tmp_is_connected {
                return;
            }
            self.tmp_is_connected = false;
            drop(_lock);
            self.trigger_on_change(ControllerTriggerType::Disconnected, false);
            return;
        }
        if !self
            .event_context
            .is_connected
            .swap(false, Ordering::Relaxed)
        {
            return;
        }
        drop(_lock);
        self.trigger_on_change(ControllerTriggerType::Disconnected, true);
    }

    pub fn is_connected(&self, get_temporary_value: bool) -> bool {
        if get_temporary_value && self.is_configuring {
            self.tmp_is_connected
        } else {
            self.event_context.is_connected.load(Ordering::Relaxed)
        }
    }

    /// Port of EmulatedController::UnloadInput.
    ///
    /// Upstream resets every device `unique_ptr`, which unregisters that
    /// device's callback from the engine through its destructor. Dropping the
    /// vectors here does the same: each `InputFrom*` calls
    /// `InputEngine::delete_callback` in its `Drop`.
    pub fn unload_input(&mut self) {
        self.is_initialized = false;
        self.button_devices.clear();
        self.stick_devices.clear();
        self.motion_devices.clear();
        self.trigger_devices.clear();
        self.battery_devices.clear();
        self.color_devices.clear();
        self.camera_devices.clear();
        self.ring_analog_devices.clear();
        self.nfc_devices.clear();
        self.output_devices.clear();
        self.virtual_button_devices.clear();
        self.virtual_stick_devices.clear();
        self.virtual_motion_devices.clear();
    }

    pub fn enable_configuration(&mut self) {
        self.is_configuring = true;
        self.tmp_is_connected = self.event_context.is_connected.load(Ordering::Relaxed);
        self.tmp_npad_type = self.npad_type;
        let mut status = self.status.lock();
        status.is_configuring = true;
        status.npad_type = self.tmp_npad_type;
    }

    pub fn disable_configuration(&mut self) {
        self.is_configuring = false;
        self.status.lock().is_configuring = false;

        // The physical-color devices are not part of the currently ported
        // device set. The remaining ordering follows upstream: apply type
        // first, then the temporary connection state.
        if self.tmp_npad_type != self.npad_type {
            if self.is_connected(false) {
                self.disconnect();
            }
            self.set_npad_style_index(self.tmp_npad_type);
            self.original_npad_type = self.tmp_npad_type;
        }

        if self.tmp_is_connected != self.is_connected(false) {
            if self.tmp_is_connected {
                self.connect(false);
                return;
            }
            self.disconnect();
        }
    }

    pub fn enable_system_buttons(&mut self) {
        self.system_buttons_enabled = true;
        self.status.lock().system_buttons_enabled = true;
    }

    pub fn disable_system_buttons(&mut self) {
        self.system_buttons_enabled = false;
        self.status.lock().system_buttons_enabled = false;
    }

    pub fn reset_system_buttons(&mut self) {
        let mut status = self.status.lock();
        status.home_button_state = HomeButtonState::default();
        status.capture_button_state = CaptureButtonState::default();
    }

    pub fn is_configuring_mode(&self) -> bool {
        self.is_configuring
    }

    /// Port of EmulatedController::LoadVirtualGamepadParams.
    fn load_virtual_gamepad_params(&mut self) {
        let player_index = crate::hid_util::npad_id_type_to_index(self.npad_id_type);
        let mut common_params = ParamPackage::default();
        common_params.set_str("engine", "virtual_gamepad".to_string());
        common_params.set_int("port", player_index as i32);
        self.virtual_button_params.fill(common_params.clone());
        self.virtual_stick_params.fill(common_params.clone());
        self.virtual_motion_params.fill(common_params);

        for (index, param) in self.virtual_button_params.iter_mut().enumerate() {
            param.set_int("button", index as i32);
        }
        self.virtual_stick_params[settings_input::native_analog::Values::LStick as usize]
            .set_int("axis_x", 0);
        self.virtual_stick_params[settings_input::native_analog::Values::LStick as usize]
            .set_int("axis_y", 1);
        self.virtual_stick_params[settings_input::native_analog::Values::RStick as usize]
            .set_int("axis_x", 2);
        self.virtual_stick_params[settings_input::native_analog::Values::RStick as usize]
            .set_int("axis_y", 3);
        for param in &mut self.virtual_stick_params {
            param.set_float("deadzone", 0.0);
            param.set_float("range", 1.0);
        }
        for param in &mut self.virtual_motion_params {
            param.set_int("motion", 0);
        }
    }

    /// Port of EmulatedController::LoadDevices.
    ///
    /// Upstream derives trigger and output parameters from representative
    /// button mappings before building the corresponding devices.
    fn load_devices(&mut self) {
        // TODO(german77): Use more buttons to detect the correct device.
        let left_joycon =
            self.button_params[settings_input::native_button::Values::DRight as usize].clone();
        let right_joycon =
            self.button_params[settings_input::native_button::Values::A as usize].clone();

        // Triggers for GC controllers, upstream's `trigger_params` assignment.
        self.trigger_params[EmulatedDeviceIndex::LeftIndex as usize] =
            self.button_params[settings_input::native_button::Values::ZL as usize].clone();
        self.trigger_params[EmulatedDeviceIndex::RightIndex as usize] =
            self.button_params[settings_input::native_button::Values::ZR as usize].clone();

        self.color_params[EmulatedDeviceIndex::LeftIndex as usize] = left_joycon.clone();
        self.color_params[EmulatedDeviceIndex::RightIndex as usize] = right_joycon.clone();
        self.battery_params[EmulatedDeviceIndex::LeftIndex as usize] = left_joycon.clone();
        self.battery_params[EmulatedDeviceIndex::RightIndex as usize] = right_joycon.clone();
        for color in &mut self.color_params {
            color.set_int("color", 1);
        }
        for battery in &mut self.battery_params {
            battery.set_int("battery", 1);
        }

        self.camera_params[0] = right_joycon.clone();
        self.camera_params[0].set_int("camera", 1);
        self.nfc_params[1] = right_joycon.clone();
        self.nfc_params[1].set_int("nfc", 1);

        if matches!(
            self.npad_id_type,
            NpadIdType::Player1 | NpadIdType::Handheld
        ) {
            self.camera_params[1] = ParamPackage::from_serialized("engine:camera,camera:1");
            self.nfc_params[0] = ParamPackage::from_serialized("engine:virtual_amiibo,nfc:1");
            #[cfg(not(target_os = "android"))]
            {
                self.ring_params[1] =
                    ParamPackage::from_serialized("engine:joycon,axis_x:100,axis_y:101");
            }
        }

        self.output_params[DeviceIndex::Left as usize] = left_joycon;
        self.output_params[DeviceIndex::Right as usize] = right_joycon;
        self.output_params[2] = self.camera_params[1].clone();
        self.output_params[3] = self.nfc_params[0].clone();
        for output in &mut self.output_params {
            output.set_int("output", 1);
        }

        self.load_virtual_gamepad_params();

        self.button_devices = self
            .button_params
            .iter()
            .map(common::input::create_input_device)
            .collect();
        self.stick_devices = self
            .stick_params
            .iter()
            .map(common::input::create_input_device)
            .collect();
        self.motion_devices = self
            .motion_params
            .iter()
            .map(common::input::create_input_device)
            .collect();
        self.trigger_devices = self
            .trigger_params
            .iter()
            .map(common::input::create_input_device)
            .collect();
        self.battery_devices = self
            .battery_params
            .iter()
            .map(common::input::create_input_device)
            .collect();
        self.color_devices = self
            .color_params
            .iter()
            .map(common::input::create_input_device)
            .collect();
        self.camera_devices = self
            .camera_params
            .iter()
            .map(common::input::create_input_device)
            .collect();
        self.ring_analog_devices = self
            .ring_params
            .iter()
            .map(common::input::create_input_device)
            .collect();
        self.nfc_devices = self
            .nfc_params
            .iter()
            .map(common::input::create_input_device)
            .collect();
        self.output_devices = self
            .output_params
            .iter()
            .map(common::input::create_output_device)
            .collect();
        self.virtual_button_devices = self
            .virtual_button_params
            .iter()
            .map(common::input::create_input_device)
            .collect();
        self.virtual_stick_devices = self
            .virtual_stick_params
            .iter()
            .map(common::input::create_input_device)
            .collect();
        self.virtual_motion_devices = self
            .virtual_motion_params
            .iter()
            .map(common::input::create_input_device)
            .collect();
    }

    /// Port of EmulatedController::ReloadInput.
    ///
    /// Builds the devices, then gives each one a callback that folds its status
    /// into the shared status. Upstream calls `ForceUpdate()` on each device right
    /// after, so a device that already has a value reports it without waiting
    /// for the next change.
    pub fn reload_input(&mut self) {
        self.load_devices();

        for (index, device) in self.button_devices.iter_mut().enumerate() {
            let uuid = UUID::from_string(&self.button_params[index].get_str("guid", ""));
            let values = Arc::clone(&self.status);
            let event_context = Arc::clone(&self.event_context);
            device.set_callback(InputCallback {
                on_change: Some(Arc::new(move |callback| {
                    set_button(&values, &event_context, callback, index, uuid);
                })),
            });
            device.force_update();
        }

        for (index, device) in self.stick_devices.iter_mut().enumerate() {
            let uuid = UUID::from_string(&self.stick_params[index].get_str("guid", ""));
            let values = Arc::clone(&self.status);
            let event_context = Arc::clone(&self.event_context);
            device.set_callback(InputCallback {
                on_change: Some(Arc::new(move |callback| {
                    set_stick(&values, &event_context, callback, index, uuid);
                })),
            });
            device.force_update();
        }

        for (index, device) in self.trigger_devices.iter_mut().enumerate() {
            let uuid = UUID::from_string(&self.trigger_params[index].get_str("guid", ""));
            let values = Arc::clone(&self.status);
            let event_context = Arc::clone(&self.event_context);
            device.set_callback(InputCallback {
                on_change: Some(Arc::new(move |callback| {
                    set_trigger(&values, &event_context, callback, index, uuid);
                })),
            });
            device.force_update();
        }

        for (index, device) in self.battery_devices.iter_mut().enumerate() {
            let status = Arc::clone(&self.status);
            let event_context = Arc::clone(&self.event_context);
            device.set_callback(InputCallback {
                on_change: Some(Arc::new(move |callback| {
                    set_battery(&status, &event_context, callback, index);
                })),
            });
            device.force_update();
        }

        for (index, device) in self.color_devices.iter_mut().enumerate() {
            let status = Arc::clone(&self.status);
            let event_context = Arc::clone(&self.event_context);
            device.set_callback(InputCallback {
                on_change: Some(Arc::new(move |callback| {
                    set_colors(&status, &event_context, callback, index);
                })),
            });
            device.force_update();
        }

        for (index, device) in self.motion_devices.iter_mut().enumerate() {
            let status = Arc::clone(&self.status);
            let event_context = Arc::clone(&self.event_context);
            device.set_callback(InputCallback {
                on_change: Some(Arc::new(move |callback| {
                    set_motion(&status, &event_context, callback, index);
                })),
            });

            let mut status = self.status.lock();
            let sensitivity = status.motion_sensitivity;
            let emulated = &mut status.motion_values[index].emulated;
            emulated.reset_rotations();
            emulated.reset_quaternion();
            status.motion_state[index] = ControllerMotion {
                accel: emulated.get_acceleration(),
                gyro: emulated.get_gyroscope(),
                rotation: emulated.get_rotations(),
                euler: emulated.get_euler_angles(),
                orientation: emulated.get_orientation(),
                is_at_rest: !emulated.is_moving(sensitivity),
            };
        }

        for device in &mut self.camera_devices {
            let status = Arc::clone(&self.status);
            let event_context = Arc::clone(&self.event_context);
            device.set_callback(InputCallback {
                on_change: Some(Arc::new(move |callback| {
                    set_camera(&status, &event_context, callback);
                })),
            });
            device.force_update();
        }

        for device in &mut self.ring_analog_devices {
            let status = Arc::clone(&self.status);
            let event_context = Arc::clone(&self.event_context);
            device.set_callback(InputCallback {
                on_change: Some(Arc::new(move |callback| {
                    set_ring_analog(&status, &event_context, callback);
                })),
            });
            device.force_update();
        }

        for device in &mut self.nfc_devices {
            let status = Arc::clone(&self.status);
            let event_context = Arc::clone(&self.event_context);
            device.set_callback(InputCallback {
                on_change: Some(Arc::new(move |callback| {
                    set_nfc(&status, &event_context, callback);
                })),
            });
            device.force_update();
        }

        for (index, device) in self.virtual_button_devices.iter_mut().enumerate() {
            let status = Arc::clone(&self.status);
            let event_context = Arc::clone(&self.event_context);
            device.set_callback(InputCallback {
                on_change: Some(Arc::new(move |callback| {
                    set_button(&status, &event_context, callback, index, VIRTUAL_UUID);
                })),
            });
        }

        for (index, device) in self.virtual_stick_devices.iter_mut().enumerate() {
            let status = Arc::clone(&self.status);
            let event_context = Arc::clone(&self.event_context);
            device.set_callback(InputCallback {
                on_change: Some(Arc::new(move |callback| {
                    set_stick(&status, &event_context, callback, index, VIRTUAL_UUID);
                })),
            });
        }

        for (index, device) in self.virtual_motion_devices.iter_mut().enumerate() {
            let status = Arc::clone(&self.status);
            let event_context = Arc::clone(&self.event_context);
            device.set_callback(InputCallback {
                on_change: Some(Arc::new(move |callback| {
                    set_motion(&status, &event_context, callback, index);
                })),
            });
        }

        self.turbo_button_state = 0;
        self.is_initialized = true;
    }

    /// Port of EmulatedController::ReloadFromSettings.
    pub fn reload_from_settings(&mut self) {
        self.reload_from_settings_before_input_reload();
        self.reload_input();
    }

    fn reload_from_settings_before_input_reload(&mut self) {
        let player_index = crate::hid_util::npad_id_type_to_index(self.npad_id_type);
        let (buttons, analogs, motions, ringcon_analog, controller_type, connected) = {
            let settings = common::settings::values();
            let player = &settings.players.get_value()[player_index];
            (
                player.buttons.clone(),
                player.analogs.clone(),
                player.motions.clone(),
                settings.ringcon_analogs.clone(),
                player.controller_type,
                player.connected,
            )
        };

        for (index, param) in buttons.iter().enumerate() {
            self.button_params[index] = ParamPackage::from_serialized(param);
        }
        for (index, param) in analogs.iter().enumerate() {
            self.stick_params[index] = ParamPackage::from_serialized(param);
        }
        for (index, param) in motions.iter().enumerate() {
            self.motion_params[index] = ParamPackage::from_serialized(param);
        }
        self.ring_params[0] = ParamPackage::from_serialized(&ringcon_analog);

        self.status.lock().color_values = [BodyColorStatus::default(); 2];
        self.reload_colors_from_settings();

        // Other or debug controllers are always a Pro Controller upstream.
        let npad_type = if self.npad_id_type == NpadIdType::Other {
            NpadStyleIndex::Fullkey
        } else {
            Self::map_settings_type_to_npad(controller_type)
        };
        self.set_npad_style_index(npad_type);
        self.original_npad_type = self.npad_type;

        // Disable special features before disconnecting.
        if self.get_polling_mode(EmulatedDeviceIndex::RightIndex) != PollingMode::Active {
            self.set_polling_mode(EmulatedDeviceIndex::RightIndex, PollingMode::Active);
        }

        self.disconnect();
        if connected {
            self.connect(false);
        }
    }

    /// Perform `ReloadFromSettings` while retaining the callbacks that Eden
    /// invokes after each state transition. `HIDCore` dispatches the returned
    /// callbacks only after releasing the Rust controller-owner mutex; Eden's
    /// controller pointer has no equivalent outer mutex to re-enter.
    pub(crate) fn reload_from_settings_deferred(&mut self) -> Vec<DeferredControllerCallback> {
        assert!(!self.defer_callback_dispatch);
        self.defer_callback_dispatch = true;
        self.reload_from_settings_before_input_reload();
        self.defer_callback_dispatch = false;
        std::mem::take(&mut self.deferred_callbacks)
    }

    /// Port of EmulatedController::SetButtonParam.
    pub fn set_button_param(&mut self, index: usize, param: ParamPackage) {
        if index >= self.button_params.len() {
            return;
        }
        self.button_params[index] = param;
        self.reload_input();
    }

    /// Port of EmulatedController::SetStickParam.
    pub fn set_stick_param(&mut self, index: usize, param: ParamPackage) {
        if index >= self.stick_params.len() {
            return;
        }
        self.stick_params[index] = param;
        self.reload_input();
    }

    /// Port of EmulatedController::SetMotionParam.
    pub fn set_motion_param(&mut self, index: usize, param: ParamPackage) {
        if index >= self.motion_params.len() {
            return;
        }
        self.motion_params[index] = param;
        self.reload_input();
    }

    /// Port of EmulatedController::StartMotionCalibration.
    pub fn start_motion_calibration(&mut self) {
        for motion in &mut self.status.lock().motion_values {
            motion.emulated.calibrate();
        }
    }

    /// Port of EmulatedController::GetButtonParam.
    pub fn get_button_param(&self, index: usize) -> ParamPackage {
        self.button_params.get(index).cloned().unwrap_or_default()
    }

    /// Port of EmulatedController::GetStickParam.
    pub fn get_stick_param(&self, index: usize) -> ParamPackage {
        self.stick_params.get(index).cloned().unwrap_or_default()
    }

    /// Port of EmulatedController::GetMotionParam.
    pub fn get_motion_param(&self, index: usize) -> ParamPackage {
        self.motion_params.get(index).cloned().unwrap_or_default()
    }

    /// Port of EmulatedController::GetRingParam.
    pub fn get_ring_param(&self) -> ParamPackage {
        self.ring_params[0].clone()
    }

    /// Port of EmulatedController::SetRingParam.
    pub fn set_ring_param(&mut self, param: ParamPackage) {
        self.ring_params[0] = param;
        self.reload_input();
    }

    /// Load every parameter from one `PlayerInput` and reload the devices once.
    ///
    /// Divergence from upstream, and the reason is the configuration dialog:
    /// upstream's `ConfigureInputPlayer` edits the `EmulatedController` itself
    /// and calls `SetButtonParam` per change, so `ReloadFromSettings` only ever
    /// needs to read the global settings. This port's dialog edits a working
    /// copy of `PlayerInput` that is only written back on OK, so it needs a way
    /// to push that copy in without going through the globals. Setting the
    /// parameters one at a time through the `Set*Param` methods above would
    /// rebuild every device once per binding.
    pub fn reload_from_player(&mut self, player: &settings_input::PlayerInput) {
        for (index, param) in player.buttons.iter().enumerate() {
            self.button_params[index] = ParamPackage::from_serialized(param);
        }
        for (index, param) in player.analogs.iter().enumerate() {
            self.stick_params[index] = ParamPackage::from_serialized(param);
        }
        for (index, param) in player.motions.iter().enumerate() {
            self.motion_params[index] = ParamPackage::from_serialized(param);
        }
        self.reload_input();
    }

    /// Port of EmulatedController::GetButtonsValues.
    pub fn get_buttons_values(&self) -> Vec<ButtonStatus> {
        self.status.lock().button_values.clone()
    }

    /// Port of EmulatedController::GetSticksValues.
    pub fn get_sticks_values(&self) -> Vec<StickStatus> {
        self.status.lock().stick_values.clone()
    }

    /// Port of EmulatedController::GetTriggersValues.
    pub fn get_triggers_values(&self) -> Vec<TriggerStatus> {
        self.status.lock().trigger_values.clone()
    }

    /// Port of EmulatedController::GetMotionValues.
    pub fn get_motion_values(&self) -> [ControllerMotionInfo; 2] {
        self.status.lock().motion_values.clone()
    }

    /// Port of EmulatedController::GetBatteryValues.
    pub fn get_battery_values(&self) -> [BatteryLevel; MAX_EMULATED_CONTROLLERS] {
        self.status.lock().battery_values
    }

    /// Port of EmulatedController::GetCameraValues.
    pub fn get_camera_values(&self) -> CameraStatus {
        self.status.lock().camera_values.clone()
    }

    /// Port of EmulatedController::GetRingSensorValues.
    pub fn get_ring_sensor_values(&self) -> AnalogStatus {
        self.status.lock().ring_analog_value
    }

    /// Port of EmulatedController::SaveCurrentConfig.
    pub fn save_current_config(&self) {
        let player_index = crate::hid_util::npad_id_type_to_index(self.npad_id_type);
        let mut settings = common::settings::values_mut();
        let player = &mut settings.players.get_value_mut()[player_index];
        player.connected = self.is_connected(false);
        player.controller_type = Self::map_npad_to_settings_type(self.npad_type);
        for (destination, source) in player.buttons.iter_mut().zip(&self.button_params) {
            *destination = source.serialize();
        }
        for (destination, source) in player.analogs.iter_mut().zip(&self.stick_params) {
            *destination = source.serialize();
        }
        for (destination, source) in player.motions.iter_mut().zip(&self.motion_params) {
            *destination = source.serialize();
        }
        if self.npad_id_type == NpadIdType::Player1 {
            settings.ringcon_analogs = self.ring_params[0].serialize();
        }
    }

    /// Port of EmulatedController::RestoreConfig.
    pub fn restore_config(&mut self) {
        if !self.is_configuring {
            return;
        }
        self.reload_from_settings();
    }

    /// Port of EmulatedController::ReloadColorsFromSettings.
    pub fn reload_colors_from_settings(&mut self) {
        let mut status = self.status.lock();
        if status.color_values[EmulatedDeviceIndex::LeftIndex as usize].body != 0
            && status.color_values[EmulatedDeviceIndex::RightIndex as usize].body != 0
        {
            return;
        }

        let player_index = crate::hid_util::npad_id_type_to_index(self.npad_id_type);
        let settings = common::settings::values();
        let player = &settings.players.get_value()[player_index];
        status.colors_state.fullkey = NpadControllerColor {
            body: Self::get_npad_color(player.body_color_left),
            button: Self::get_npad_color(player.button_color_left),
        };
        status.colors_state.left = NpadControllerColor {
            body: Self::get_npad_color(player.body_color_left),
            button: Self::get_npad_color(player.button_color_left),
        };
        status.colors_state.right = NpadControllerColor {
            body: Self::get_npad_color(player.body_color_right),
            button: Self::get_npad_color(player.button_color_right),
        };
    }

    /// Port of EmulatedController::IsControllerFullkey.
    fn is_controller_fullkey(&self, use_temporary_value: bool) -> bool {
        let npad = if self.is_configuring && use_temporary_value {
            self.tmp_npad_type
        } else {
            self.npad_type
        };
        matches!(
            npad,
            NpadStyleIndex::Fullkey
                | NpadStyleIndex::GameCube
                | NpadStyleIndex::NES
                | NpadStyleIndex::SNES
                | NpadStyleIndex::N64
                | NpadStyleIndex::SegaGenesis
        )
    }

    /// Port of EmulatedController::IsControllerSupported.
    fn is_controller_supported(&self, use_temporary_value: bool) -> bool {
        let npad = if self.is_configuring && use_temporary_value {
            self.tmp_npad_type
        } else {
            self.npad_type
        };
        is_controller_supported(npad, *self.event_context.supported_style_tag.lock())
    }

    /// Port of EmulatedController::GetHomeButtons.
    pub fn get_home_buttons(&self) -> HomeButtonState {
        let status = self.status.lock();
        if self.is_configuring {
            return HomeButtonState::default();
        }
        status.home_button_state
    }

    /// Port of EmulatedController::GetCaptureButtons.
    pub fn get_capture_buttons(&self) -> CaptureButtonState {
        let status = self.status.lock();
        if self.is_configuring {
            return CaptureButtonState::default();
        }
        status.capture_button_state
    }

    /// Port of EmulatedController::GetNpadButtons.
    pub fn get_npad_buttons(&self) -> NpadButtonState {
        let status = self.status.lock();
        if self.is_configuring {
            return NpadButtonState::default();
        }
        NpadButtonState {
            raw: status.npad_button_state.raw & self.get_turbo_button_mask(&status),
        }
    }

    /// Port of EmulatedController::GetDebugPadButtons.
    pub fn get_debug_pad_buttons(&self) -> DebugPadButton {
        let status = self.status.lock();
        if self.is_configuring {
            return DebugPadButton::default();
        }
        status.debug_pad_button_state
    }

    /// Port of EmulatedController::GetSticks.
    pub fn get_sticks(&self) -> AnalogSticks {
        let status = self.status.lock();
        if self.is_configuring {
            return AnalogSticks::default();
        }
        status.analog_stick_state
    }

    /// Port of EmulatedController::GetTriggers.
    pub fn get_triggers(&self) -> NpadGcTriggerState {
        let status = self.status.lock();
        if self.is_configuring {
            return NpadGcTriggerState::default();
        }
        status.gc_trigger_state
    }

    /// Port of EmulatedController::GetMotions.
    pub fn get_motions(&self) -> MotionState {
        self.status.lock().motion_state
    }

    /// Port of EmulatedController::GetColors.
    pub fn get_colors(&self) -> ControllerColors {
        self.status.lock().colors_state
    }

    /// Port of EmulatedController::GetBattery.
    pub fn get_battery(&self) -> BatteryLevelState {
        self.status.lock().battery_state
    }

    /// Port of EmulatedController::GetCamera.
    pub fn get_camera(&self) -> CameraState {
        self.status.lock().camera_state.clone()
    }

    /// Port of EmulatedController::GetRingSensorForce.
    pub fn get_ring_sensor_force(&self) -> RingSensorForce {
        self.status.lock().ring_analog_state
    }

    /// Port of EmulatedController::GetNfc.
    pub fn get_nfc(&self) -> NfcStatus {
        self.status.lock().nfc_state.clone()
    }

    /// Port of EmulatedController::GetNpadColor.
    pub fn get_npad_color(color: u32) -> NpadColor {
        NpadColor {
            r: ((color >> 16) & 0xFF) as u8,
            g: ((color >> 8) & 0xFF) as u8,
            b: (color & 0xFF) as u8,
            a: 0xFF,
        }
    }

    /// Port of EmulatedController::SetVibration (simple on/off version).
    pub fn set_vibration_simple(&mut self, should_vibrate: bool) -> bool {
        let mut vibration = DEFAULT_VIBRATION_VALUE;
        if should_vibrate {
            vibration.low_amplitude = 1.0;
            vibration.high_amplitude = 1.0;
        }
        self.set_vibration(DeviceIndex::Left, vibration)
    }

    /// Port of `EmulatedController::SetVibration(DeviceIndex, VibrationValue)`.
    pub fn set_vibration(&mut self, device_index: DeviceIndex, vibration: VibrationValue) -> bool {
        if !self.is_initialized {
            return false;
        }
        let index = match device_index {
            DeviceIndex::Left => DeviceIndex::Left as usize,
            DeviceIndex::Right => DeviceIndex::Right as usize,
            DeviceIndex::None | DeviceIndex::MaxDeviceIndex => return false,
        };
        if index >= self.output_devices.len() {
            return false;
        }

        // Skip duplicated vibrations.
        if self.last_vibration_value[index].is_equal(&vibration) {
            return *common::settings::values().vibration_enabled.get_value();
        }
        self.last_vibration_value[index] = vibration;

        let player_index = crate::hid_util::npad_id_type_to_index(self.npad_id_type);
        let (master_enabled, accurate, player_enabled, strength) = {
            let settings = common::settings::values();
            let player = &settings.players.get_value()[player_index];
            (
                *settings.vibration_enabled.get_value(),
                *settings.enable_accurate_vibrations.get_value(),
                player.vibration_enabled,
                player.vibration_strength as f32 / 100.0,
            )
        };
        if !master_enabled || !player_enabled {
            return false;
        }

        if !accurate {
            let now = Instant::now();
            if (vibration.low_amplitude != 0.0 || vibration.high_amplitude != 0.0)
                && self.last_vibration_timepoint[index]
                    .is_some_and(|last| now.duration_since(last).as_millis() < 15)
            {
                return false;
            }
            self.last_vibration_timepoint[index] = Some(now);
        }

        let status = vibration_status(vibration, strength);

        // Send vibrations to Android's input overlay first.
        if let Some(android) = self.output_devices.get_mut(4) {
            android.set_vibration(&status);
        }
        self.output_devices[index].set_vibration(&status) == DriverResult::Success
    }

    /// Port of `EmulatedController::IsVibrationEnabled`.
    pub fn is_vibration_enabled(&self, device_index: usize) -> bool {
        let player_index = crate::hid_util::npad_id_type_to_index(self.npad_id_type);
        let player_enabled =
            common::settings::values().players.get_value()[player_index].vibration_enabled;
        self.is_initialized
            && player_enabled
            && self
                .output_devices
                .get(device_index)
                .is_some_and(|device| device.is_vibration_enabled())
    }

    /// Port of EmulatedController::SetPollingMode.
    pub fn set_polling_mode(
        &mut self,
        device_index: EmulatedDeviceIndex,
        polling_mode: PollingMode,
    ) -> DriverResult {
        log::info!(
            "Set polling mode {:?}, device_index={:?}",
            polling_mode,
            device_index
        );

        if !self.is_initialized {
            return DriverResult::InvalidHandle;
        }

        if device_index == EmulatedDeviceIndex::LeftIndex {
            self.status.lock().left_polling_mode = polling_mode;
            return self.output_devices[DeviceIndex::Left as usize].set_polling_mode(polling_mode);
        }

        if device_index == EmulatedDeviceIndex::RightIndex {
            self.status.lock().right_polling_mode = polling_mode;
            let virtual_nfc_result = self.output_devices[3].set_polling_mode(polling_mode);
            let mapped_nfc_result =
                self.output_devices[DeviceIndex::Right as usize].set_polling_mode(polling_mode);

            // Restore previous state.
            if mapped_nfc_result != DriverResult::Success {
                self.output_devices[DeviceIndex::Right as usize]
                    .set_polling_mode(PollingMode::Active);
            }

            if virtual_nfc_result == DriverResult::Success {
                return virtual_nfc_result;
            }
            return mapped_nfc_result;
        }

        {
            let mut status = self.status.lock();
            status.left_polling_mode = polling_mode;
            status.right_polling_mode = polling_mode;
        }
        self.output_devices[DeviceIndex::Left as usize].set_polling_mode(polling_mode);
        self.output_devices[DeviceIndex::Right as usize].set_polling_mode(polling_mode);
        self.output_devices[3].set_polling_mode(polling_mode);
        DriverResult::Success
    }

    /// Port of EmulatedController::GetPollingMode.
    pub fn get_polling_mode(&self, device_index: EmulatedDeviceIndex) -> PollingMode {
        let status = self.status.lock();
        if device_index == EmulatedDeviceIndex::LeftIndex {
            status.left_polling_mode
        } else {
            status.right_polling_mode
        }
    }

    /// Port of EmulatedController::SetCameraFormat.
    pub fn set_camera_format(&mut self, camera_format: ImageTransferProcessorFormat) -> bool {
        log::info!("Set camera format {:?}", camera_format);
        if !self.is_initialized {
            return false;
        }
        let camera_format = irs_format_to_camera(camera_format);
        if self.output_devices[DeviceIndex::Right as usize].set_camera_format(camera_format)
            == DriverResult::Success
        {
            return true;
        }
        self.output_devices[2].set_camera_format(camera_format) == DriverResult::Success
    }

    /// Port of EmulatedController::GetActualVibrationValue.
    pub fn get_actual_vibration_value(&self, device_index: DeviceIndex) -> VibrationValue {
        let _lock = self.mutex.lock();
        match device_index {
            DeviceIndex::Left => self.last_vibration_value[0],
            DeviceIndex::Right => self.last_vibration_value[1],
            _ => DEFAULT_VIBRATION_VALUE,
        }
    }

    /// Port of EmulatedController::HasNfc.
    pub fn has_nfc(&self) -> bool {
        if !self.is_initialized {
            return false;
        }
        if !matches!(
            self.npad_type,
            NpadStyleIndex::JoyconRight
                | NpadStyleIndex::JoyconDual
                | NpadStyleIndex::Fullkey
                | NpadStyleIndex::Handheld
        ) {
            return false;
        }
        let has_virtual_nfc = matches!(
            self.npad_id_type,
            NpadIdType::Player1 | NpadIdType::Handheld
        );
        let is_virtual_nfc_supported =
            self.output_devices[3].supports_nfc() != NfcState::NotSupported;
        self.is_connected(false) && has_virtual_nfc && is_virtual_nfc_supported
    }

    /// Port of EmulatedController::AddNfcHandle.
    pub fn add_nfc_handle(&mut self) -> bool {
        self.nfc_handles += 1;
        self.set_polling_mode(EmulatedDeviceIndex::RightIndex, PollingMode::NFC)
            == DriverResult::Success
    }

    /// Port of EmulatedController::RemoveNfcHandle.
    pub fn remove_nfc_handle(&mut self) -> bool {
        self.nfc_handles = self.nfc_handles.wrapping_sub(1);
        if self.nfc_handles == 0 {
            return self.set_polling_mode(EmulatedDeviceIndex::RightIndex, PollingMode::Active)
                == DriverResult::Success;
        }
        true
    }

    /// Port of EmulatedController::StartNfcPolling.
    pub fn start_nfc_polling(&mut self) -> bool {
        if !self.is_initialized {
            return false;
        }
        let device_result = self.output_devices[DeviceIndex::Right as usize].start_nfc_polling();
        let virtual_device_result = self.output_devices[3].start_nfc_polling();
        device_result == NfcState::Success || virtual_device_result == NfcState::Success
    }

    /// Port of EmulatedController::StopNfcPolling.
    pub fn stop_nfc_polling(&mut self) -> bool {
        if !self.is_initialized {
            return false;
        }
        let device_result = self.output_devices[DeviceIndex::Right as usize].stop_nfc_polling();
        let virtual_device_result = self.output_devices[3].stop_nfc_polling();
        device_result == NfcState::Success || virtual_device_result == NfcState::Success
    }

    /// Port of EmulatedController::ReadAmiiboData.
    pub fn read_amiibo_data(&mut self, data: &mut Vec<u8>) -> bool {
        if !self.is_initialized {
            return false;
        }
        if self.output_devices[DeviceIndex::Right as usize].read_amiibo_data(data)
            == NfcState::Success
        {
            return true;
        }
        self.output_devices[3].read_amiibo_data(data) == NfcState::Success
    }

    /// Port of EmulatedController::ReadMifareData.
    pub fn read_mifare_data(
        &mut self,
        request: &MifareRequest,
        out_data: &mut MifareRequest,
    ) -> bool {
        if !self.is_initialized {
            return false;
        }
        if self.output_devices[DeviceIndex::Right as usize].read_mifare_data(request, out_data)
            == NfcState::Success
        {
            return true;
        }
        self.output_devices[3].read_mifare_data(request, out_data) == NfcState::Success
    }

    /// Port of EmulatedController::WriteMifareData.
    pub fn write_mifare_data(&mut self, request: &MifareRequest) -> bool {
        if !self.is_initialized {
            return false;
        }
        if self.output_devices[DeviceIndex::Right as usize].write_mifare_data(request)
            == NfcState::Success
        {
            return true;
        }
        self.output_devices[3].write_mifare_data(request) == NfcState::Success
    }

    /// Port of EmulatedController::WriteNfc.
    pub fn write_nfc(&mut self, data: &[u8]) -> bool {
        if !self.is_initialized {
            return false;
        }
        if self.output_devices[DeviceIndex::Right as usize].supports_nfc() != NfcState::NotSupported
        {
            return self.output_devices[DeviceIndex::Right as usize].write_nfc_data(data)
                == NfcState::Success;
        }
        self.output_devices[3].write_nfc_data(data) == NfcState::Success
    }

    /// Port of EmulatedController::SetGyroscopeZeroDriftMode.
    pub fn set_gyroscope_zero_drift_mode(&mut self, mode: GyroscopeZeroDriftMode) {
        let mut status = self.status.lock();
        let (sensitivity, threshold) = match mode {
            GyroscopeZeroDriftMode::Loose => (IS_AT_REST_LOOSE, THRESHOLD_LOOSE),
            GyroscopeZeroDriftMode::Tight => (IS_AT_REST_TIGHT, THRESHOLD_TIGHT),
            GyroscopeZeroDriftMode::Standard => (IS_AT_REST_STANDARD, THRESHOLD_STANDARD),
        };
        status.motion_sensitivity = sensitivity;
        for motion in &mut status.motion_values {
            motion.emulated.set_gyro_threshold(threshold);
        }
    }

    /// Port of EmulatedController::StatusUpdate.
    pub fn status_update(&mut self) {
        self.turbo_button_state = (self.turbo_button_state + 1) % (TURBO_BUTTON_DELAY * 2);
        let force_updates = {
            let status = self.status.lock();
            std::array::from_fn::<_, 2, _>(|index| {
                status.motion_values[index].raw_status.force_update
            })
        };
        for (index, device) in self.motion_devices.iter_mut().enumerate() {
            if force_updates.get(index).copied().unwrap_or(false) {
                device.force_update();
            }
        }
    }

    /// Port of EmulatedController::GetTurboButtonMask.
    fn get_turbo_button_mask(&self, status: &ControllerStatus) -> NpadButton {
        // Apply no mask when disabled
        if self.turbo_button_state < TURBO_BUTTON_DELAY {
            return NpadButton::ALL;
        }

        use settings_input::native_button::Values as NB;
        let mut turbo_buttons = NpadButton::empty();
        for (index, button) in status.button_values.iter().enumerate() {
            if !button.turbo {
                continue;
            }
            let flag = match index {
                i if i == NB::A as usize => NpadButton::A,
                i if i == NB::B as usize => NpadButton::B,
                i if i == NB::X as usize => NpadButton::X,
                i if i == NB::Y as usize => NpadButton::Y,
                i if i == NB::L as usize => NpadButton::L,
                i if i == NB::R as usize => NpadButton::R,
                i if i == NB::ZL as usize => NpadButton::ZL,
                i if i == NB::ZR as usize => NpadButton::ZR,
                i if i == NB::DLeft as usize => NpadButton::LEFT,
                i if i == NB::DUp as usize => NpadButton::UP,
                i if i == NB::DRight as usize => NpadButton::RIGHT,
                i if i == NB::DDown as usize => NpadButton::DOWN,
                i if i == NB::SLLeft as usize => NpadButton::LEFT_SL,
                i if i == NB::SLRight as usize => NpadButton::RIGHT_SL,
                i if i == NB::SRLeft as usize => NpadButton::LEFT_SR,
                i if i == NB::SRRight as usize => NpadButton::RIGHT_SR,
                _ => continue,
            };
            turbo_buttons.insert(flag);
        }
        NpadButton::from_bits_truncate(!turbo_buttons.bits())
    }

    pub fn get_led_pattern(&self) -> LedPattern {
        match self.npad_id_type {
            NpadIdType::Player1 => LedPattern::new(1, 0, 0, 0),
            NpadIdType::Player2 => LedPattern::new(1, 1, 0, 0),
            NpadIdType::Player3 => LedPattern::new(1, 1, 1, 0),
            NpadIdType::Player4 => LedPattern::new(1, 1, 1, 1),
            NpadIdType::Player5 => LedPattern::new(1, 0, 0, 1),
            NpadIdType::Player6 => LedPattern::new(1, 0, 1, 0),
            NpadIdType::Player7 => LedPattern::new(1, 0, 1, 1),
            NpadIdType::Player8 => LedPattern::new(0, 1, 1, 0),
            _ => LedPattern::new(0, 0, 0, 0),
        }
    }

    /// Port of `EmulatedController::SetLedPattern`.
    pub fn set_led_pattern(&mut self) {
        if !self.is_initialized {
            return;
        }

        let pattern = self.get_led_pattern();
        let status = common::input::LedStatus {
            led_1: pattern.raw & (1 << 0) != 0,
            led_2: pattern.raw & (1 << 1) != 0,
            led_3: pattern.raw & (1 << 2) != 0,
            led_4: pattern.raw & (1 << 3) != 0,
        };
        for device in &mut self.output_devices {
            device.set_led(&status);
        }
    }

    pub fn set_callback(&mut self, update_callback: ControllerUpdateCallback) -> i32 {
        let key = self.last_callback_key;
        self.event_context
            .callback_list
            .lock()
            .insert(key, update_callback);
        self.last_callback_key += 1;
        key
    }

    pub fn delete_callback(&mut self, key: i32) {
        if self
            .event_context
            .callback_list
            .lock()
            .remove(&key)
            .is_none()
        {
            log::error!("Tried to delete non-existent callback {}", key);
        }
    }

    fn trigger_on_change(&mut self, trigger_type: ControllerTriggerType, is_service_update: bool) {
        if self.defer_callback_dispatch {
            self.deferred_callbacks.extend(
                self.event_context
                    .callback_list
                    .lock()
                    .values()
                    .filter(|callback| is_service_update || !callback.is_npad_service)
                    .map(|callback| DeferredControllerCallback {
                        callback: Arc::clone(&callback.on_change),
                        trigger_type,
                    }),
            );
            return;
        }
        trigger_on_change(&self.event_context, trigger_type, is_service_update);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::input::{AnalogStatus, InputType};
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn deferred_callbacks_run_after_the_controller_owner_is_released() {
        let calls = Arc::new(AtomicUsize::new(0));
        let callback_calls = Arc::clone(&calls);
        let mut controller = EmulatedController::new(NpadIdType::Player1);
        controller.set_callback(ControllerUpdateCallback {
            on_change: Arc::new(move |_| {
                callback_calls.fetch_add(1, Ordering::SeqCst);
            }),
            is_npad_service: false,
        });

        controller.defer_callback_dispatch = true;
        controller.trigger_on_change(ControllerTriggerType::Disconnected, true);
        controller.defer_callback_dispatch = false;

        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let callbacks = std::mem::take(&mut controller.deferred_callbacks);
        drop(controller);
        for callback in callbacks {
            callback.dispatch();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    struct PollingOutputDevice {
        calls: Arc<Mutex<Vec<PollingMode>>>,
        result: DriverResult,
    }

    struct LedOutputDevice {
        calls: Arc<Mutex<Vec<common::input::LedStatus>>>,
    }

    #[derive(Default)]
    struct SpecializedOutputCalls {
        camera_formats: Vec<CameraFormat>,
        start_nfc: usize,
        stop_nfc: usize,
        read_amiibo: usize,
        write_nfc: usize,
        read_mifare: usize,
        write_mifare: usize,
    }

    struct SpecializedOutputDevice {
        calls: Arc<Mutex<SpecializedOutputCalls>>,
        driver_result: DriverResult,
        nfc_state: NfcState,
        supports_nfc: NfcState,
    }

    impl OutputDevice for SpecializedOutputDevice {
        fn set_camera_format(&mut self, format: CameraFormat) -> DriverResult {
            self.calls.lock().camera_formats.push(format);
            self.driver_result
        }

        fn supports_nfc(&self) -> NfcState {
            self.supports_nfc
        }

        fn start_nfc_polling(&mut self) -> NfcState {
            self.calls.lock().start_nfc += 1;
            self.nfc_state
        }

        fn stop_nfc_polling(&mut self) -> NfcState {
            self.calls.lock().stop_nfc += 1;
            self.nfc_state
        }

        fn read_amiibo_data(&mut self, out_data: &mut Vec<u8>) -> NfcState {
            self.calls.lock().read_amiibo += 1;
            if self.nfc_state == NfcState::Success {
                out_data.push(0xA5);
            }
            self.nfc_state
        }

        fn write_nfc_data(&mut self, _data: &[u8]) -> NfcState {
            self.calls.lock().write_nfc += 1;
            self.nfc_state
        }

        fn read_mifare_data(
            &mut self,
            _request: &MifareRequest,
            _out_data: &mut MifareRequest,
        ) -> NfcState {
            self.calls.lock().read_mifare += 1;
            self.nfc_state
        }

        fn write_mifare_data(&mut self, _request: &MifareRequest) -> NfcState {
            self.calls.lock().write_mifare += 1;
            self.nfc_state
        }
    }

    fn specialized_output_device(
        driver_result: DriverResult,
        nfc_state: NfcState,
        supports_nfc: NfcState,
    ) -> (Box<dyn OutputDevice>, Arc<Mutex<SpecializedOutputCalls>>) {
        let calls = Arc::new(Mutex::new(SpecializedOutputCalls::default()));
        (
            Box::new(SpecializedOutputDevice {
                calls: Arc::clone(&calls),
                driver_result,
                nfc_state,
                supports_nfc,
            }),
            calls,
        )
    }

    struct CountingInputDevice {
        force_updates: Arc<Mutex<usize>>,
        callback: InputCallback,
    }

    impl InputDevice for CountingInputDevice {
        fn force_update(&mut self) {
            *self.force_updates.lock() += 1;
        }

        fn set_callback(&mut self, callback: InputCallback) {
            self.callback = callback;
        }

        fn trigger_on_change(&self, status: &CallbackStatus) {
            if let Some(callback) = &self.callback.on_change {
                callback(status);
            }
        }
    }

    impl OutputDevice for LedOutputDevice {
        fn set_led(&mut self, status: &common::input::LedStatus) -> DriverResult {
            self.calls.lock().push(*status);
            DriverResult::Success
        }
    }

    impl OutputDevice for PollingOutputDevice {
        fn set_polling_mode(&mut self, polling_mode: PollingMode) -> DriverResult {
            self.calls.lock().push(polling_mode);
            self.result
        }
    }

    fn polling_output_device(
        result: DriverResult,
    ) -> (Box<dyn OutputDevice>, Arc<Mutex<Vec<PollingMode>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        (
            Box::new(PollingOutputDevice {
                calls: Arc::clone(&calls),
                result,
            }),
            calls,
        )
    }

    fn event_context(npad_id_type: NpadIdType) -> Arc<ControllerEventContext> {
        Arc::new(ControllerEventContext {
            npad_id_type,
            is_connected: AtomicBool::new(false),
            supported_style_tag: Mutex::new(NpadStyleTag {
                raw: NpadStyleSet::ALL,
            }),
            callback_list: Mutex::new(HashMap::new()),
        })
    }

    fn button_callback(pressed: bool) -> CallbackStatus {
        CallbackStatus {
            input_type: InputType::Button,
            button_status: ButtonStatus {
                value: pressed,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn stick_callback(x: f32, y: f32) -> CallbackStatus {
        CallbackStatus {
            input_type: InputType::Stick,
            stick_status: StickStatus {
                x: AnalogStatus {
                    raw_value: x,
                    ..Default::default()
                },
                y: AnalogStatus {
                    raw_value: y,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// A press has to reach `npad_button_state`, not just the raw values the
    /// configuration preview reads. Folding it only into `button_values` left
    /// the pad working in the dialog and dead in the game.
    #[test]
    fn a_press_reaches_the_state_the_guest_reads() {
        use settings_input::native_button::Values as NB;
        let status = Arc::new(Mutex::new(ControllerStatus::new()));
        let events = event_context(NpadIdType::Player1);
        let uuid = UUID::new();

        set_button(
            &status,
            &events,
            &button_callback(true),
            NB::A as usize,
            uuid,
        );
        {
            let status = status.lock();
            assert!(status.button_values[NB::A as usize].value);
            assert!(status.npad_button_state.raw.contains(NpadButton::A));
            assert_eq!(status.debug_pad_button_state.raw & 1, 1);
        }

        set_button(
            &status,
            &events,
            &button_callback(false),
            NB::A as usize,
            uuid,
        );
        let status = status.lock();
        assert!(!status.button_values[NB::A as usize].value);
        assert!(!status.npad_button_state.raw.contains(NpadButton::A));
    }

    #[test]
    fn debug_pad_uses_the_upstream_bit_positions() {
        use settings_input::native_button::Values as NB;

        let mappings = [
            (NB::A, 0),
            (NB::B, 1),
            (NB::X, 2),
            (NB::Y, 3),
            (NB::L, 4),
            (NB::R, 5),
            (NB::ZL, 6),
            (NB::ZR, 7),
            (NB::Plus, 8),
            (NB::Minus, 9),
            (NB::DLeft, 10),
            (NB::DUp, 11),
            (NB::DRight, 12),
            (NB::DDown, 13),
        ];
        let status = Arc::new(Mutex::new(ControllerStatus::new()));
        let events = event_context(NpadIdType::Other);
        let uuid = UUID::new();

        for (button, bit) in mappings {
            set_button(
                &status,
                &events,
                &button_callback(true),
                button as usize,
                uuid,
            );
            assert_eq!(status.lock().debug_pad_button_state.raw, 1 << bit);
            set_button(
                &status,
                &events,
                &button_callback(false),
                button as usize,
                uuid,
            );
            assert_eq!(status.lock().debug_pad_button_state.raw, 0);
        }
    }

    #[test]
    fn player_one_button_auto_connects_and_notifies_callbacks() {
        use settings_input::native_button::Values as NB;

        let status = Arc::new(Mutex::new(ControllerStatus::new()));
        status.lock().npad_type = NpadStyleIndex::Fullkey;
        let events = event_context(NpadIdType::Player1);
        let observed = Arc::new(Mutex::new(Vec::new()));
        events.callback_list.lock().insert(
            0,
            ControllerUpdateCallback {
                on_change: Arc::new({
                    let observed = Arc::clone(&observed);
                    move |event| observed.lock().push(event)
                }),
                is_npad_service: true,
            },
        );

        set_button(
            &status,
            &events,
            &button_callback(true),
            NB::A as usize,
            UUID::new(),
        );

        assert!(events.is_connected.load(Ordering::Relaxed));
        assert_eq!(
            *observed.lock(),
            vec![
                ControllerTriggerType::Connected,
                ControllerTriggerType::Button
            ]
        );
    }

    /// Home and Capture are gated on `system_buttons_enabled` upstream.
    #[test]
    fn the_system_buttons_can_be_gated_off() {
        use settings_input::native_button::Values as NB;
        let status = Arc::new(Mutex::new(ControllerStatus::new()));
        let events = event_context(NpadIdType::Player1);
        status.lock().system_buttons_enabled = false;
        set_button(
            &status,
            &events,
            &button_callback(true),
            NB::Home as usize,
            UUID::new(),
        );
        assert_eq!(status.lock().home_button_state.raw, 0);

        // Release first: upstream only folds a value into the HID state when the
        // raw value actually transitions, so pressing an already-pressed button
        // is a no-op.
        set_button(
            &status,
            &events,
            &button_callback(false),
            NB::Home as usize,
            UUID::new(),
        );
        status.lock().system_buttons_enabled = true;
        set_button(
            &status,
            &events,
            &button_callback(true),
            NB::Home as usize,
            UUID::new(),
        );
        assert_eq!(status.lock().home_button_state.raw, 1);
    }

    /// A GameCube pad reports ZL and ZR through its analog triggers, so the
    /// digital bindings must not also set the buttons.
    #[test]
    fn a_gamecube_pad_ignores_the_digital_z_buttons() {
        use settings_input::native_button::Values as NB;
        let status = Arc::new(Mutex::new(ControllerStatus::new()));
        let events = event_context(NpadIdType::Player1);
        status.lock().npad_type = NpadStyleIndex::GameCube;

        set_button(
            &status,
            &events,
            &button_callback(true),
            NB::ZL as usize,
            UUID::new(),
        );
        let status = status.lock();
        // The raw value is still recorded — the preview draws it — but the
        // guest-facing state is left to `set_trigger`.
        assert!(status.button_values[NB::ZL as usize].value);
        assert!(!status.npad_button_state.raw.contains(NpadButton::ZL));
    }

    /// A stick has to land in `analog_stick_state`, scaled to the HID range.
    #[test]
    fn a_stick_reaches_the_state_the_guest_reads() {
        use settings_input::native_analog::Values as NA;
        let status = Arc::new(Mutex::new(ControllerStatus::new()));
        let events = event_context(NpadIdType::Player1);

        set_stick(
            &status,
            &events,
            &stick_callback(1.0, 0.0),
            NA::LStick as usize,
            UUID::new(),
        );
        let status = status.lock();
        assert_eq!(status.analog_stick_state.left.x, HID_JOYSTICK_MAX);
        assert_eq!(status.analog_stick_state.left.y, 0);
        assert!(status
            .npad_button_state
            .raw
            .contains(NpadButton::STICK_L_RIGHT));
        // The other stick is untouched.
        assert_eq!(status.analog_stick_state.right.x, 0);
    }

    /// While the configuration dialog is open upstream reports nothing to the
    /// HID services, so a mapping session cannot leak into a running game.
    #[test]
    fn configuring_mode_reports_nothing_to_the_guest() {
        use settings_input::native_button::Values as NB;
        let status = Arc::new(Mutex::new(ControllerStatus::new()));
        let events = event_context(NpadIdType::Player1);
        status.lock().is_configuring = true;

        set_button(
            &status,
            &events,
            &button_callback(true),
            NB::B as usize,
            UUID::new(),
        );
        let status = status.lock();
        assert!(status.button_values[NB::B as usize].value);
        assert!(status.npad_button_state.raw.is_empty());
    }

    #[test]
    fn configuration_applies_temporary_type_and_connection_in_upstream_order() {
        let mut controller = EmulatedController::new(NpadIdType::Player1);
        controller.set_npad_style_index(NpadStyleIndex::Fullkey);
        controller.original_npad_type = NpadStyleIndex::Fullkey;
        controller.connect(false);

        controller.enable_configuration();
        controller.set_npad_style_index(NpadStyleIndex::JoyconDual);
        controller.disconnect();

        // Configuration only changes the temporary values.
        assert_eq!(
            controller.get_npad_style_index(false),
            NpadStyleIndex::Fullkey
        );
        assert_eq!(
            controller.get_npad_style_index(true),
            NpadStyleIndex::JoyconDual
        );
        assert!(controller.is_connected(false));
        assert!(!controller.is_connected(true));

        controller.disable_configuration();
        assert_eq!(
            controller.get_npad_style_index(false),
            NpadStyleIndex::JoyconDual
        );
        assert_eq!(controller.original_npad_type, NpadStyleIndex::JoyconDual);
        assert!(!controller.is_connected(false));
    }

    #[test]
    fn changing_type_during_configuration_preserves_a_connected_controller() {
        let mut controller = EmulatedController::new(NpadIdType::Player1);
        controller.set_npad_style_index(NpadStyleIndex::Fullkey);
        controller.original_npad_type = NpadStyleIndex::Fullkey;
        controller.connect(false);

        controller.enable_configuration();
        controller.set_npad_style_index(NpadStyleIndex::JoyconDual);
        controller.disable_configuration();

        assert_eq!(
            controller.get_npad_style_index(false),
            NpadStyleIndex::JoyconDual
        );
        assert!(controller.is_connected(false));
    }

    #[test]
    fn supported_style_change_uses_upstream_fullkey_fallbacks() {
        let mut controller = EmulatedController::new(NpadIdType::Player1);
        controller.set_npad_style_index(NpadStyleIndex::GameCube);
        controller.original_npad_type = NpadStyleIndex::GameCube;
        controller.connect(false);

        controller.set_supported_npad_style_tag(NpadStyleTag {
            raw: NpadStyleSet::FULLKEY,
        });

        assert_eq!(
            controller.get_npad_style_index(false),
            NpadStyleIndex::Fullkey
        );
        assert!(controller.is_connected(false));
    }

    #[test]
    fn pokeball_is_not_a_fullkey_controller() {
        let mut controller = EmulatedController::new(NpadIdType::Player1);
        controller.set_npad_style_index(NpadStyleIndex::Pokeball);
        controller.original_npad_type = NpadStyleIndex::Pokeball;
        controller.connect(false);

        controller.set_supported_npad_style_tag(NpadStyleTag {
            raw: NpadStyleSet::FULLKEY,
        });

        assert_eq!(
            controller.get_npad_style_index(false),
            NpadStyleIndex::Pokeball
        );
        assert!(!controller.is_connected(false));
    }

    /// `unload_input` has to drop the devices, or their engine callbacks keep
    /// firing into a controller nothing is reading any more.
    #[test]
    fn unloading_releases_every_device() {
        let mut controller = EmulatedController::new(NpadIdType::Player1);
        controller.reload_input();
        assert!(!controller.button_devices.is_empty());
        assert_eq!(controller.output_devices.len(), OUTPUT_DEVICES_SIZE);

        controller.unload_input();
        assert!(controller.button_devices.is_empty());
        assert!(controller.stick_devices.is_empty());
        assert!(controller.motion_devices.is_empty());
        assert!(controller.trigger_devices.is_empty());
        assert!(controller.camera_devices.is_empty());
        assert!(controller.ring_analog_devices.is_empty());
        assert!(controller.nfc_devices.is_empty());
        assert!(controller.output_devices.is_empty());
        assert!(controller.virtual_button_devices.is_empty());
        assert!(controller.virtual_stick_devices.is_empty());
        assert!(controller.virtual_motion_devices.is_empty());
    }

    #[test]
    fn load_devices_derives_vibration_outputs_from_upstream_buttons() {
        let mut controller = EmulatedController::new(NpadIdType::Player1);
        controller.button_params[settings_input::native_button::Values::DRight as usize] =
            ParamPackage::from_serialized("engine:null,pad:7");
        controller.button_params[settings_input::native_button::Values::A as usize] =
            ParamPackage::from_serialized("engine:null,pad:9");

        controller.load_devices();

        assert_eq!(controller.output_params.len(), OUTPUT_DEVICES_SIZE);
        assert_eq!(
            controller.output_params[DeviceIndex::Left as usize].get_int("pad", 0),
            7
        );
        assert_eq!(
            controller.output_params[DeviceIndex::Right as usize].get_int("pad", 0),
            9
        );
        assert!(controller
            .output_params
            .iter()
            .all(|param| param.get_int("output", 0) == 1));
        assert_eq!(
            controller.virtual_motion_params[0].get_str("engine", ""),
            "virtual_gamepad"
        );
        assert_eq!(controller.virtual_motion_params[0].get_int("motion", -1), 0);
    }

    #[test]
    fn right_polling_prefers_virtual_nfc_and_restores_a_rejected_mapped_device() {
        let mut controller = EmulatedController::new(NpadIdType::Player1);
        let (left, _) = polling_output_device(DriverResult::NotSupported);
        let (right, right_calls) = polling_output_device(DriverResult::NotSupported);
        let (camera, _) = polling_output_device(DriverResult::NotSupported);
        let (virtual_nfc, virtual_nfc_calls) = polling_output_device(DriverResult::Success);
        let (android, _) = polling_output_device(DriverResult::NotSupported);
        controller.output_devices = vec![left, right, camera, virtual_nfc, android];
        controller.is_initialized = true;

        assert_eq!(
            controller.set_polling_mode(EmulatedDeviceIndex::RightIndex, PollingMode::NFC),
            DriverResult::Success
        );
        assert_eq!(
            controller.get_polling_mode(EmulatedDeviceIndex::RightIndex),
            PollingMode::NFC
        );
        assert_eq!(*virtual_nfc_calls.lock(), vec![PollingMode::NFC]);
        assert_eq!(
            *right_calls.lock(),
            vec![PollingMode::NFC, PollingMode::Active]
        );
    }

    #[test]
    fn all_devices_polling_updates_both_controller_modes() {
        let mut controller = EmulatedController::new(NpadIdType::Player1);
        let (left, left_calls) = polling_output_device(DriverResult::NotSupported);
        let (right, right_calls) = polling_output_device(DriverResult::NotSupported);
        let (camera, camera_calls) = polling_output_device(DriverResult::NotSupported);
        let (virtual_nfc, virtual_nfc_calls) = polling_output_device(DriverResult::NotSupported);
        let (android, android_calls) = polling_output_device(DriverResult::NotSupported);
        controller.output_devices = vec![left, right, camera, virtual_nfc, android];
        controller.is_initialized = true;

        assert_eq!(
            controller.set_polling_mode(EmulatedDeviceIndex::AllDevices, PollingMode::IR),
            DriverResult::Success
        );
        assert_eq!(
            controller.get_polling_mode(EmulatedDeviceIndex::LeftIndex),
            PollingMode::IR
        );
        assert_eq!(
            controller.get_polling_mode(EmulatedDeviceIndex::RightIndex),
            PollingMode::IR
        );
        assert_eq!(*left_calls.lock(), vec![PollingMode::IR]);
        assert_eq!(*right_calls.lock(), vec![PollingMode::IR]);
        assert_eq!(*virtual_nfc_calls.lock(), vec![PollingMode::IR]);
        assert!(camera_calls.lock().is_empty());
        assert!(android_calls.lock().is_empty());
    }

    #[test]
    fn camera_format_falls_back_to_the_virtual_camera() {
        let mut controller = EmulatedController::new(NpadIdType::Player1);
        let (left, _) = polling_output_device(DriverResult::NotSupported);
        let (right, right_calls) = specialized_output_device(
            DriverResult::NotSupported,
            NfcState::NotSupported,
            NfcState::NotSupported,
        );
        let (camera, camera_calls) = specialized_output_device(
            DriverResult::Success,
            NfcState::NotSupported,
            NfcState::NotSupported,
        );
        let (virtual_nfc, _) = polling_output_device(DriverResult::NotSupported);
        let (android, _) = polling_output_device(DriverResult::NotSupported);
        controller.output_devices = vec![left, right, camera, virtual_nfc, android];
        controller.is_initialized = true;

        assert!(controller.set_camera_format(ImageTransferProcessorFormat::Size80x60));
        assert_eq!(
            right_calls.lock().camera_formats,
            vec![CameraFormat::Size80x60]
        );
        assert_eq!(
            camera_calls.lock().camera_formats,
            vec![CameraFormat::Size80x60]
        );
    }

    #[test]
    fn nfc_support_and_operations_follow_upstream_device_priority() {
        let mut controller = EmulatedController::new(NpadIdType::Player1);
        controller.set_npad_style_index(NpadStyleIndex::Fullkey);
        controller.connect(false);
        let (left, _) = polling_output_device(DriverResult::NotSupported);
        let (right, right_calls) = specialized_output_device(
            DriverResult::NotSupported,
            NfcState::NotSupported,
            NfcState::NotSupported,
        );
        let (camera, _) = polling_output_device(DriverResult::NotSupported);
        let (virtual_nfc, virtual_calls) = specialized_output_device(
            DriverResult::NotSupported,
            NfcState::Success,
            NfcState::Success,
        );
        let (android, _) = polling_output_device(DriverResult::NotSupported);
        controller.output_devices = vec![left, right, camera, virtual_nfc, android];
        controller.is_initialized = true;

        assert!(controller.has_nfc());
        assert!(controller.start_nfc_polling());
        assert!(controller.stop_nfc_polling());
        let mut amiibo = Vec::new();
        assert!(controller.read_amiibo_data(&mut amiibo));
        assert_eq!(amiibo, vec![0xA5]);
        assert!(
            controller.read_mifare_data(&MifareRequest::default(), &mut MifareRequest::default())
        );
        assert!(controller.write_mifare_data(&MifareRequest::default()));
        assert!(controller.write_nfc(&[1, 2, 3]));

        let right_calls = right_calls.lock();
        assert_eq!(right_calls.start_nfc, 1);
        assert_eq!(right_calls.stop_nfc, 1);
        assert_eq!(right_calls.read_amiibo, 1);
        assert_eq!(right_calls.read_mifare, 1);
        assert_eq!(right_calls.write_mifare, 1);
        assert_eq!(right_calls.write_nfc, 0);
        drop(right_calls);
        let virtual_calls = virtual_calls.lock();
        assert_eq!(virtual_calls.start_nfc, 1);
        assert_eq!(virtual_calls.stop_nfc, 1);
        assert_eq!(virtual_calls.read_amiibo, 1);
        assert_eq!(virtual_calls.read_mifare, 1);
        assert_eq!(virtual_calls.write_mifare, 1);
        assert_eq!(virtual_calls.write_nfc, 1);
    }

    #[test]
    fn specialized_input_callbacks_update_guest_visible_state() {
        let status = Arc::new(Mutex::new(ControllerStatus::new()));
        let events = event_context(NpadIdType::Player1);

        let mut motion_callback = CallbackStatus {
            input_type: InputType::Motion,
            ..Default::default()
        };
        motion_callback.motion_status.accel.z.raw_value = -1.0;
        motion_callback.motion_status.gyro.x.raw_value = 0.02;
        motion_callback.motion_status.gyro.x.properties.threshold = THRESHOLD_STANDARD;
        motion_callback.motion_status.delta_timestamp = 1000;
        set_motion(&status, &events, &motion_callback, 0);
        assert_eq!(status.lock().motion_state[0].accel.z, -1.0);

        set_camera(
            &status,
            &events,
            &CallbackStatus {
                input_type: InputType::IrSensor,
                camera_status: CameraFormat::Size40x30,
                raw_data: vec![1, 2, 3],
                ..Default::default()
            },
        );
        assert_eq!(status.lock().camera_state.sample, 1);
        assert_eq!(status.lock().camera_state.data, vec![1, 2, 3]);

        set_ring_analog(&status, &events, &stick_callback(0.5, 0.0));
        assert_eq!(status.lock().ring_analog_state.force, 0.5);

        let mut nfc_status = NfcStatus::default();
        nfc_status.state = NfcState::NewAmiibo;
        set_nfc(
            &status,
            &events,
            &CallbackStatus {
                input_type: InputType::Nfc,
                nfc_status,
                ..Default::default()
            },
        );
        assert_eq!(status.lock().nfc_state.state, NfcState::NewAmiibo);
    }

    #[test]
    fn battery_values_accessor_returns_both_raw_device_levels() {
        let controller = EmulatedController::new(NpadIdType::Player1);
        controller.status.lock().battery_values = [BatteryLevel::Charging, BatteryLevel::Low];

        assert_eq!(
            controller.get_battery_values(),
            [BatteryLevel::Charging, BatteryLevel::Low]
        );
    }

    #[test]
    fn motion_drift_mode_and_forced_refresh_match_upstream() {
        let mut controller = EmulatedController::new(NpadIdType::Player1);
        controller.set_gyroscope_zero_drift_mode(GyroscopeZeroDriftMode::Tight);
        assert_eq!(
            controller.status.lock().motion_sensitivity,
            IS_AT_REST_TIGHT
        );

        controller.status.lock().motion_values[0]
            .raw_status
            .force_update = true;
        let first_updates = Arc::new(Mutex::new(0));
        let second_updates = Arc::new(Mutex::new(0));
        controller.motion_devices = vec![
            Box::new(CountingInputDevice {
                force_updates: Arc::clone(&first_updates),
                callback: InputCallback { on_change: None },
            }),
            Box::new(CountingInputDevice {
                force_updates: Arc::clone(&second_updates),
                callback: InputCallback { on_change: None },
            }),
        ];

        controller.status_update();
        assert_eq!(*first_updates.lock(), 1);
        assert_eq!(*second_updates.lock(), 0);
    }

    #[test]
    fn set_led_pattern_updates_every_output_device() {
        let mut controller = EmulatedController::new(NpadIdType::Player3);
        let first_calls = Arc::new(Mutex::new(Vec::new()));
        let second_calls = Arc::new(Mutex::new(Vec::new()));
        controller.output_devices = vec![
            Box::new(LedOutputDevice {
                calls: Arc::clone(&first_calls),
            }),
            Box::new(LedOutputDevice {
                calls: Arc::clone(&second_calls),
            }),
        ];
        controller.is_initialized = true;

        controller.set_led_pattern();

        for calls in [first_calls, second_calls] {
            let calls = calls.lock();
            assert_eq!(calls.len(), 1);
            assert!(calls[0].led_1);
            assert!(calls[0].led_2);
            assert!(calls[0].led_3);
            assert!(!calls[0].led_4);
        }
    }

    #[test]
    fn save_and_restore_config_use_the_controller_owned_parameters() {
        let player_index = crate::hid_util::npad_id_type_to_index(NpadIdType::Player8);
        let original_player = common::settings::values().players.get_value()[player_index].clone();

        let mut controller = EmulatedController::new(NpadIdType::Player8);
        controller.npad_type = NpadStyleIndex::GameCube;
        controller
            .event_context
            .is_connected
            .store(true, Ordering::Relaxed);
        controller.button_params[0] = ParamPackage::from_serialized("engine:save_test,button:4");
        controller.stick_params[0] = ParamPackage::from_serialized("engine:save_test,axis_x:2");
        controller.motion_params[0] = ParamPackage::from_serialized("engine:save_test,motion:1");

        controller.save_current_config();

        {
            let settings = common::settings::values();
            let saved = &settings.players.get_value()[player_index];
            assert!(saved.connected);
            assert_eq!(saved.controller_type, ControllerType::GameCube);
            assert_eq!(
                ParamPackage::from_serialized(&saved.buttons[0]).get_str("engine", ""),
                "save_test"
            );
            assert_eq!(
                ParamPackage::from_serialized(&saved.analogs[0]).get_int("axis_x", -1),
                2
            );
            assert_eq!(
                ParamPackage::from_serialized(&saved.motions[0]).get_int("motion", -1),
                1
            );
        }

        controller.enable_configuration();
        controller.set_npad_style_index(NpadStyleIndex::JoyconDual);
        controller.disconnect();
        controller.button_params[0] = ParamPackage::from_serialized("engine:discarded");
        controller.restore_config();

        assert_eq!(
            controller.get_npad_style_index(true),
            NpadStyleIndex::GameCube
        );
        assert!(controller.is_connected(true));
        assert_eq!(
            controller.get_button_param(0).get_str("engine", ""),
            "save_test"
        );
        controller.disable_configuration();
        controller.unload_input();

        common::settings::values_mut().players.get_value_mut()[player_index] = original_player;
    }

    #[test]
    fn vibration_strength_uses_upstream_curve_and_amplitude_cap() {
        let strong = vibration_status(
            VibrationValue {
                low_amplitude: 0.75,
                low_frequency: 160.0,
                high_amplitude: 1.0,
                high_frequency: 320.0,
            },
            1.5,
        );
        assert_eq!(strong.low_amplitude, 1.0);
        assert_eq!(strong.high_amplitude, 1.0);
        assert_eq!(
            strong.amplification_type,
            VibrationAmplificationType::Exponential
        );

        let weak = vibration_status(DEFAULT_VIBRATION_VALUE, 0.7);
        assert_eq!(weak.amplification_type, VibrationAmplificationType::Linear);
    }

    use super::{apply_simple_npad_stick_buttons, parse_u64_auto, AnalogSticks};

    #[test]
    fn scripted_npad_parser_uses_decimal_unless_prefixed_hex() {
        assert_eq!(parse_u64_auto("1000"), Some(1000));
        assert_eq!(parse_u64_auto("0x1000"), Some(0x1000));
        assert_eq!(parse_u64_auto("0X4C0"), Some(0x4C0));
        assert_eq!(parse_u64_auto("not-a-number"), None);
    }

    #[test]
    fn scripted_stick_direction_bits_also_drive_analog_coordinates() {
        let mut sticks = AnalogSticks::default();
        apply_simple_npad_stick_buttons(
            &mut sticks,
            NpadButton::STICK_L_DOWN | NpadButton::STICK_R_LEFT,
        );

        assert_eq!(sticks.left.x, 0);
        assert_eq!(sticks.left.y, -HID_JOYSTICK_MAX);
        assert_eq!(sticks.right.x, -HID_JOYSTICK_MAX);
        assert_eq!(sticks.right.y, 0);
    }
}
