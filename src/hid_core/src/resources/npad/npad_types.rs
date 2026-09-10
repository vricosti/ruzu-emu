// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of hid_core/resources/npad/npad_types.h and npad_types.cpp

use crate::hid_types::*;

pub const MAX_SUPPORTED_NPAD_ID_TYPES: usize = 10;
pub const STYLE_INDEX_COUNT: usize = 7;

/// This is nn::hid::NpadJoyHoldType
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u64)]
pub enum NpadJoyHoldType {
    #[default]
    Vertical = 0,
    Horizontal = 1,
}

/// This is nn::hid::NpadJoyAssignmentMode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u32)]
pub enum NpadJoyAssignmentMode {
    #[default]
    Dual = 0,
    Single = 1,
}

/// This is nn::hid::NpadJoyDeviceType
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(i64)]
pub enum NpadJoyDeviceType {
    #[default]
    Left = 0,
    Right = 1,
}

/// This is nn::hid::NpadHandheldActivationMode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u64)]
pub enum NpadHandheldActivationMode {
    #[default]
    Dual = 0,
    Single = 1,
    None = 2,
    MaxActivationMode = 3,
}

/// This is nn::hid::system::AppletFooterUiType
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum AppletFooterUiType {
    #[default]
    None = 0,
    HandheldNone = 1,
    HandheldJoyConLeftOnly = 2,
    HandheldJoyConRightOnly = 3,
    HandheldJoyConLeftJoyConRight = 4,
    JoyDual = 5,
    JoyDualLeftOnly = 6,
    JoyDualRightOnly = 7,
    JoyLeftHorizontal = 8,
    JoyLeftVertical = 9,
    JoyRightHorizontal = 10,
    JoyRightVertical = 11,
    SwitchProController = 12,
    CompatibleProController = 13,
    CompatibleJoyCon = 14,
    LarkHvc1 = 15,
    LarkHvc2 = 16,
    LarkNesLeft = 17,
    LarkNesRight = 18,
    Lucia = 19,
    Verification = 20,
    Lagon = 21,
}

/// This is nn::hid::NpadCommunicationMode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u64)]
pub enum NpadCommunicationMode {
    Mode5ms = 0,
    Mode10ms = 1,
    Mode15ms = 2,
    #[default]
    Default = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum NpadRevision {
    #[default]
    Revision0,
    Revision1,
    Revision2,
    Revision3,
    Unknown(u32),
}

impl NpadRevision {
    pub fn from_raw(raw: u32) -> Self {
        match raw {
            0 => Self::Revision0,
            1 => Self::Revision1,
            2 => Self::Revision2,
            3 => Self::Revision3,
            value => Self::Unknown(value),
        }
    }
}

/// This is nn::hid::detail::ColorAttribute
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u32)]
pub enum ColorAttribute {
    Ok = 0,
    ReadError = 1,
    #[default]
    NoController = 2,
}

/// This is nn::hid::detail::NpadFullKeyColorState
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct NpadFullKeyColorState {
    pub attribute: ColorAttribute,
    pub fullkey: NpadControllerColor,
}
const _: () = assert!(std::mem::size_of::<NpadFullKeyColorState>() == 0xC);

/// This is nn::hid::detail::NpadJoyColorState
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct NpadJoyColorState {
    pub attribute: ColorAttribute,
    pub left: NpadControllerColor,
    pub right: NpadControllerColor,
}
const _: () = assert!(std::mem::size_of::<NpadJoyColorState>() == 0x14);

/// This is nn::hid::NpadAttribute
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct NpadAttribute {
    pub raw: u32,
}
const _: () = assert!(std::mem::size_of::<NpadAttribute>() == 4);

/// NPadGenericState - common state for all npad styles
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct NPadGenericState {
    pub sampling_number: i64,
    pub npad_buttons: NpadButtonState,
    pub l_stick: AnalogStickState,
    pub r_stick: AnalogStickState,
    pub connection_status: NpadAttribute,
    pub _reserved: [u8; 4],
}
const _: () = assert!(std::mem::size_of::<NPadGenericState>() == 0x28);

/// This is nn::hid::NpadCondition, the global controller condition.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct NpadCondition {
    pub _00: u32,
    pub is_initialized: u32,
    pub hold_type: u32,
    pub is_valid: u32,
}

impl Default for NpadCondition {
    fn default() -> Self {
        Self {
            _00: 0,
            is_initialized: 1,
            hold_type: NpadJoyHoldType::Horizontal as u32,
            is_valid: 1,
        }
    }
}
const _: () = assert!(std::mem::size_of::<NpadCondition>() == 0x10);

/// This is nn::hid::NpadSystemProperties
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct NPadSystemProperties {
    pub raw: i64,
}
const _: () = assert!(std::mem::size_of::<NPadSystemProperties>() == 0x8);

impl NPadSystemProperties {
    pub fn set_is_charging_joy_dual(&mut self, val: bool) {
        if val {
            self.raw |= 1 << 0;
        } else {
            self.raw &= !(1 << 0);
        }
    }
    pub fn set_is_charging_joy_left(&mut self, val: bool) {
        if val {
            self.raw |= 1 << 1;
        } else {
            self.raw &= !(1 << 1);
        }
    }
    pub fn set_is_charging_joy_right(&mut self, val: bool) {
        if val {
            self.raw |= 1 << 2;
        } else {
            self.raw &= !(1 << 2);
        }
    }
    pub fn set_is_powered_joy_dual(&mut self, val: bool) {
        if val {
            self.raw |= 1 << 3;
        } else {
            self.raw &= !(1 << 3);
        }
    }
    pub fn set_is_powered_joy_left(&mut self, val: bool) {
        if val {
            self.raw |= 1 << 4;
        } else {
            self.raw &= !(1 << 4);
        }
    }
    pub fn set_is_powered_joy_right(&mut self, val: bool) {
        if val {
            self.raw |= 1 << 5;
        } else {
            self.raw &= !(1 << 5);
        }
    }
}

impl NpadSystemButtonProperties {
    pub fn set_is_home_button_protection_enabled(&mut self, val: bool) {
        if val {
            self.raw |= 1 << 0;
        } else {
            self.raw &= !(1 << 0);
        }
    }
}

/// This is nn::hid::NpadSystemButtonProperties
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct NpadSystemButtonProperties {
    pub raw: i32,
}
const _: () = assert!(std::mem::size_of::<NpadSystemButtonProperties>() == 0x4);

/// This is nn::hid::system::DeviceType
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct DeviceType {
    pub raw: u32,
}

/// This is nn::hid::system::AppletFooterUiAttributes
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct AppletFooterUiAttributes {
    pub _padding: [u8; 0x4],
}

/// This is nn::hid::NpadLarkType
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u32)]
pub enum NpadLarkType {
    #[default]
    Invalid = 0,
    H1 = 1,
    H2 = 2,
    NL = 3,
    NR = 4,
}

/// This is nn::hid::NpadLuciaType
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u32)]
pub enum NpadLuciaType {
    #[default]
    Invalid = 0,
    J = 1,
    E = 2,
    U = 3,
}

/// This is nn::hid::NpadLagerType
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u32)]
pub enum NpadLagerType {
    #[default]
    Invalid = 0,
    J = 1,
    E = 2,
    U = 3,
}

// NpadGcTriggerState is defined in hid_types.rs (upstream has it in both
// hid_types.h and npad_types.h; we keep the single definition in hid_types.rs).

/// This is nn::hid::NpadLagonType
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u32)]
pub enum NpadLagonType {
    #[default]
    Invalid = 0,
}

pub type AppletFooterUiVariant = u8;

/// This is nn::hid::system::AppletDetailedUiType
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct AppletDetailedUiType {
    pub ui_variant: AppletFooterUiVariant,
    pub _padding: [u8; 0x2],
    pub footer: AppletFooterUiType,
}
const _: () = assert!(std::mem::size_of::<AppletDetailedUiType>() == 0x4);

/// This is nn::hid::detail::NfcXcdDeviceHandleStateImpl
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct NfcXcdDeviceHandleStateImpl {
    pub handle: u64,
    pub is_available: bool,
    pub is_activated: bool,
    pub _reserved: [u8; 0x6],
    pub sampling_number: u64,
}
const _: () = assert!(std::mem::size_of::<NfcXcdDeviceHandleStateImpl>() == 0x18);

/// nn::hidtypes::FeatureType
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct FeatureType {
    pub raw: u64,
}
const _: () = assert!(std::mem::size_of::<FeatureType>() == 8);

impl FeatureType {
    pub fn has_left_analog_stick(&self) -> bool {
        (self.raw & (1 << 0)) != 0
    }
    pub fn has_right_analog_stick(&self) -> bool {
        (self.raw & (1 << 1)) != 0
    }
    pub fn has_left_joy_six_axis_sensor(&self) -> bool {
        (self.raw & (1 << 2)) != 0
    }
    pub fn has_right_joy_six_axis_sensor(&self) -> bool {
        (self.raw & (1 << 3)) != 0
    }
    pub fn has_fullkey_joy_six_axis_sensor(&self) -> bool {
        (self.raw & (1 << 4)) != 0
    }
    pub fn has_left_lra_vibration_device(&self) -> bool {
        (self.raw & (1 << 5)) != 0
    }
    pub fn has_right_lra_vibration_device(&self) -> bool {
        (self.raw & (1 << 6)) != 0
    }
    pub fn has_gc_vibration_device(&self) -> bool {
        (self.raw & (1 << 7)) != 0
    }
    pub fn has_erm_vibration_device(&self) -> bool {
        (self.raw & (1 << 8)) != 0
    }
    pub fn has_left_joy_rail_bus(&self) -> bool {
        (self.raw & (1 << 9)) != 0
    }
    pub fn has_right_joy_rail_bus(&self) -> bool {
        (self.raw & (1 << 10)) != 0
    }
    pub fn has_internal_bus(&self) -> bool {
        (self.raw & (1 << 11)) != 0
    }
    pub fn is_palma(&self) -> bool {
        (self.raw & (1 << 12)) != 0
    }
    pub fn has_nfc(&self) -> bool {
        (self.raw & (1 << 13)) != 0
    }
    pub fn has_ir_sensor(&self) -> bool {
        (self.raw & (1 << 14)) != 0
    }
    pub fn is_analog_stick_calibration_supported(&self) -> bool {
        (self.raw & (1 << 15)) != 0
    }
    pub fn is_six_axis_sensor_user_calibration_supported(&self) -> bool {
        (self.raw & (1 << 16)) != 0
    }
    pub fn has_left_right_joy_battery(&self) -> bool {
        (self.raw & (1 << 17)) != 0
    }
    pub fn has_fullkey_battery(&self) -> bool {
        (self.raw & (1 << 18)) != 0
    }
    pub fn is_disconnect_controller_if_battery_none(&self) -> bool {
        (self.raw & (1 << 19)) != 0
    }
    pub fn has_controller_color(&self) -> bool {
        (self.raw & (1 << 20)) != 0
    }
    pub fn has_grip_color(&self) -> bool {
        (self.raw & (1 << 21)) != 0
    }
    pub fn has_identification_code(&self) -> bool {
        (self.raw & (1 << 22)) != 0
    }
    pub fn has_bluetooth_address(&self) -> bool {
        (self.raw & (1 << 23)) != 0
    }
    pub fn has_mcu(&self) -> bool {
        (self.raw & (1 << 24)) != 0
    }
    pub fn has_notification_led(&self) -> bool {
        (self.raw & (1 << 25)) != 0
    }
    pub fn has_directional_buttons(&self) -> bool {
        (self.raw & (1 << 26)) != 0
    }
    pub fn has_indicator_led(&self) -> bool {
        (self.raw & (1 << 27)) != 0
    }
}

/// This is nn::hid::AssignmentStyle
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct AssignmentStyle {
    pub raw: u32,
}
const _: () = assert!(std::mem::size_of::<AssignmentStyle>() == 4);

impl AssignmentStyle {
    pub fn is_external_assigned(&self) -> bool {
        (self.raw & (1 << 0)) != 0
    }
    pub fn is_external_left_assigned(&self) -> bool {
        (self.raw & (1 << 1)) != 0
    }
    pub fn is_external_right_assigned(&self) -> bool {
        (self.raw & (1 << 2)) != 0
    }
    pub fn is_handheld_assigned(&self) -> bool {
        (self.raw & (1 << 3)) != 0
    }
    pub fn is_handheld_left_assigned(&self) -> bool {
        (self.raw & (1 << 4)) != 0
    }
    pub fn is_handheld_right_assigned(&self) -> bool {
        (self.raw & (1 << 5)) != 0
    }
}

/// This is nn::hid::server::IAbstractedPad::InternalFlags
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct InternalFlags {
    pub raw: u32,
}
const _: () = assert!(std::mem::size_of::<InternalFlags>() == 4);

impl InternalFlags {
    pub fn is_bound(&self) -> bool {
        (self.raw & (1 << 0)) != 0
    }
    pub fn is_connected(&self) -> bool {
        (self.raw & (1 << 1)) != 0
    }
    pub fn is_battery_low_ovln_required(&self) -> bool {
        (self.raw & (1 << 2)) != 0
    }
    pub fn set_is_battery_low_ovln_required(&mut self, val: bool) {
        if val {
            self.raw |= 1 << 2;
        } else {
            self.raw &= !(1 << 2);
        }
    }
    pub fn is_battery_low_ovln_delay_required(&self) -> bool {
        (self.raw & (1 << 3)) != 0
    }
    pub fn is_sample_received(&self) -> bool {
        (self.raw & (1 << 4)) != 0
    }
    pub fn is_virtual_input(&self) -> bool {
        (self.raw & (1 << 5)) != 0
    }
    pub fn is_wired(&self) -> bool {
        (self.raw & (1 << 6)) != 0
    }
    pub fn use_center_clamp(&self) -> bool {
        (self.raw & (1 << 8)) != 0
    }
    pub fn set_use_center_clamp(&mut self, val: bool) {
        if val {
            self.raw |= 1 << 8;
        } else {
            self.raw &= !(1 << 8);
        }
    }
    pub fn has_virtual_six_axis_sensor_acceleration(&self) -> bool {
        (self.raw & (1 << 9)) != 0
    }
    pub fn has_virtual_six_axis_sensor_angle(&self) -> bool {
        (self.raw & (1 << 10)) != 0
    }
    pub fn is_debug_pad(&self) -> bool {
        (self.raw & (1 << 11)) != 0
    }
    pub fn set_is_connected(&mut self, val: bool) {
        if val {
            self.raw |= 1 << 1;
        } else {
            self.raw &= !(1 << 1);
        }
    }
}

/// This is nn::hid::server::IAbstractedPad
#[derive(Debug, Clone, Default)]
pub struct IAbstractedPad {
    pub internal_flags: InternalFlags,
    pub controller_id: u64,
    pub controller_number: u32,
    pub low_battery_display_delay_time: u64,
    pub low_battery_display_delay_interval: u64,
    pub feature_set: FeatureType,
    pub disabled_feature_set: FeatureType,
    pub assignment_style: AssignmentStyle,
    pub device_type: NpadStyleIndex,
    pub interface_type: NpadInterfaceType,
    pub power_info: NpadPowerInfo,
    pub pad_state: u32,
    pub button_mask: u32,
    pub system_button_mask: u32,
    pub indicator: u8,
    pub virtual_six_axis_sensor_acceleration: Vec<f32>,
    pub virtual_six_axis_sensor_angle: Vec<f32>,
    pub color: u64,
}

/// This is nn::hid::NpadStatus
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct NpadStatus {
    pub raw: u32,
}
const _: () = assert!(std::mem::size_of::<NpadStatus>() == 4);

impl NpadStatus {
    pub fn is_supported_styleset_set(&self) -> bool {
        (self.raw & (1 << 0)) != 0
    }
    pub fn set_is_supported_styleset_set(&mut self, val: bool) {
        if val {
            self.raw |= 1 << 0;
        } else {
            self.raw &= !(1 << 0);
        }
    }
    pub fn is_hold_type_set(&self) -> bool {
        (self.raw & (1 << 1)) != 0
    }
    pub fn set_is_hold_type_set(&mut self, val: bool) {
        if val {
            self.raw |= 1 << 1;
        } else {
            self.raw &= !(1 << 1);
        }
    }
    pub fn lr_assignment_mode(&self) -> bool {
        (self.raw & (1 << 2)) != 0
    }
    pub fn set_lr_assignment_mode(&mut self, val: bool) {
        if val {
            self.raw |= 1 << 2;
        } else {
            self.raw &= !(1 << 2);
        }
    }
    pub fn assigning_single_on_sl_sr_press(&self) -> bool {
        (self.raw & (1 << 3)) != 0
    }
    pub fn set_assigning_single_on_sl_sr_press(&mut self, val: bool) {
        if val {
            self.raw |= 1 << 3;
        } else {
            self.raw &= !(1 << 3);
        }
    }
    pub fn is_full_policy(&self) -> bool {
        (self.raw & (1 << 4)) != 0
    }
    pub fn set_is_full_policy(&mut self, val: bool) {
        if val {
            self.raw |= 1 << 4;
        } else {
            self.raw &= !(1 << 4);
        }
    }
    pub fn is_policy(&self) -> bool {
        (self.raw & (1 << 5)) != 0
    }
    pub fn set_is_policy(&mut self, val: bool) {
        if val {
            self.raw |= 1 << 5;
        } else {
            self.raw &= !(1 << 5);
        }
    }
    pub fn use_center_clamp(&self) -> bool {
        (self.raw & (1 << 6)) != 0
    }
    pub fn set_use_center_clamp(&mut self, val: bool) {
        if val {
            self.raw |= 1 << 6;
        } else {
            self.raw &= !(1 << 6);
        }
    }
    pub fn system_ext_state(&self) -> bool {
        (self.raw & (1 << 7)) != 0
    }
    pub fn set_system_ext_state(&mut self, val: bool) {
        if val {
            self.raw |= 1 << 7;
        } else {
            self.raw &= !(1 << 7);
        }
    }
}

/// Color properties used by NpadAbstractPropertiesHandler
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct ColorProperties {
    pub attribute: ColorAttribute,
    pub color: NpadControllerColor,
    pub _padding: [u8; 0x4],
}
