// SPDX-FileCopyrightText: Copyright 2022 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `input_common/drivers/joycon.h` and `input_common/drivers/joycon.cpp`.
//!
//! Nintendo Joy-Con and Pro Controller driver via HID API.

use std::sync::Arc;

use parking_lot::Mutex;

use common::input::{
    ButtonNames, CameraFormat, DriverResult, LedStatus, MifareRequest, NfcState, PollingMode,
    VibrationStatus,
};
use common::param_package::ParamPackage;
use common::settings_input::{native_analog, native_button, native_motion};
use common::uuid::UUID;

use crate::helpers::joycon_protocol::joycon_types::{ControllerType, PadAxes, PadButton};
use crate::input_engine::{InputEngine, PadIdentifier};
use crate::main_common::{AnalogMapping, ButtonMapping, MotionMapping};

/// Port of `Joycons` class from joycon.h / joycon.cpp
pub struct Joycons {
    /// Shared with the factories registered by `InputSubsystem::Impl`, matching
    /// upstream's shared `Joycons` instance.
    engine: Arc<Mutex<InputEngine>>,
    // In C++ these hold shared_ptr<JoyconDriver>. Since JoyconDriver requires
    // SDL HID API for actual operation, we track connection status only.
}

impl Joycons {
    /// Port of Joycons::Joycons
    pub fn new(input_engine: String) -> Self {
        // In C++: checks Settings::values.enable_joycon_driver and enable_procon_driver,
        // calls SDL_hid_init(), then Setup().
        log::info!("Joycon driver initialization started");
        Self {
            engine: Arc::new(Mutex::new(InputEngine::new(input_engine))),
        }
    }

    /// Shared handle to the underlying input engine for factory registration.
    pub fn engine(&self) -> Arc<Mutex<InputEngine>> {
        Arc::clone(&self.engine)
    }

    /// Port of Joycons::IsVibrationEnabled (override)
    pub fn is_vibration_enabled(&self, _identifier: &PadIdentifier) -> bool {
        // In C++: gets the handle and checks handle->IsVibrationEnabled()
        false
    }

    /// Port of Joycons::SetVibration (override)
    pub fn set_vibration(
        &mut self,
        _identifier: &PadIdentifier,
        _vibration: &VibrationStatus,
    ) -> DriverResult {
        // In C++: converts to native VibrationValue, gets handle, calls handle->SetVibration()
        DriverResult::InvalidHandle
    }

    /// Port of Joycons::SetLeds (override)
    pub fn set_leds(
        &mut self,
        _identifier: &PadIdentifier,
        _led_status: &LedStatus,
    ) -> DriverResult {
        // In C++: converts led_status to a bitmask and calls handle->SetLedConfig()
        DriverResult::InvalidHandle
    }

    /// Port of Joycons::SetCameraFormat (override)
    pub fn set_camera_format(
        &mut self,
        _identifier: &PadIdentifier,
        _camera_format: CameraFormat,
    ) -> DriverResult {
        // In C++: calls handle->SetIrsConfig()
        DriverResult::InvalidHandle
    }

    /// Port of Joycons::SupportsNfc (override)
    pub fn supports_nfc(&self, _identifier: &PadIdentifier) -> NfcState {
        NfcState::Success
    }

    /// Port of Joycons::StartNfcPolling (override)
    pub fn start_nfc_polling(&mut self, _identifier: &PadIdentifier) -> NfcState {
        NfcState::Unknown
    }

    /// Port of Joycons::StopNfcPolling (override)
    pub fn stop_nfc_polling(&mut self, _identifier: &PadIdentifier) -> NfcState {
        NfcState::Unknown
    }

    /// Port of Joycons::ReadAmiiboData (override)
    pub fn read_amiibo_data(
        &mut self,
        _identifier: &PadIdentifier,
        _out_data: &mut Vec<u8>,
    ) -> NfcState {
        NfcState::Unknown
    }

    /// Port of Joycons::WriteNfcData (override)
    pub fn write_nfc_data(&mut self, _identifier: &PadIdentifier, _data: &[u8]) -> NfcState {
        NfcState::Unknown
    }

    /// Port of Joycons::ReadMifareData (override)
    pub fn read_mifare_data(
        &mut self,
        _identifier: &PadIdentifier,
        _request: &MifareRequest,
        _out_data: &mut MifareRequest,
    ) -> NfcState {
        NfcState::Unknown
    }

    /// Port of Joycons::WriteMifareData (override)
    pub fn write_mifare_data(
        &mut self,
        _identifier: &PadIdentifier,
        _request: &MifareRequest,
    ) -> NfcState {
        NfcState::Unknown
    }

    /// Port of Joycons::SetPollingMode (override)
    pub fn set_polling_mode(
        &mut self,
        _identifier: &PadIdentifier,
        _polling_mode: PollingMode,
    ) -> DriverResult {
        DriverResult::InvalidHandle
    }

    /// Port of Joycons::GetInputDevices (override)
    pub fn get_input_devices(&self) -> Vec<ParamPackage> {
        // In C++ iterates left_joycons, right_joycons, pro_controller arrays
        // and lists connected devices plus dual joycon pairs.
        Vec::new()
    }

    /// Port of Joycons::GetButtonMappingForDevice (override)
    pub fn get_button_mapping_for_device(&self, params: &ParamPackage) -> ButtonMapping {
        const SWITCH_TO_JOYCON_BUTTON: [(native_button::Values, PadButton, bool); 18] = [
            (native_button::Values::A, PadButton::A, true),
            (native_button::Values::B, PadButton::B, true),
            (native_button::Values::X, PadButton::X, true),
            (native_button::Values::Y, PadButton::Y, true),
            (native_button::Values::DLeft, PadButton::Left, false),
            (native_button::Values::DUp, PadButton::Up, false),
            (native_button::Values::DRight, PadButton::Right, false),
            (native_button::Values::DDown, PadButton::Down, false),
            (native_button::Values::L, PadButton::L, false),
            (native_button::Values::R, PadButton::R, true),
            (native_button::Values::ZL, PadButton::ZL, false),
            (native_button::Values::ZR, PadButton::ZR, true),
            (native_button::Values::Plus, PadButton::Plus, true),
            (native_button::Values::Minus, PadButton::Minus, false),
            (native_button::Values::Home, PadButton::Home, true),
            (native_button::Values::Screenshot, PadButton::Capture, false),
            (native_button::Values::LStick, PadButton::StickL, false),
            (native_button::Values::RStick, PadButton::StickR, true),
        ];

        if !params.has("port") {
            return ButtonMapping::new();
        }

        let mut mapping = ButtonMapping::new();
        for &(switch_button, joycon_button, side) in &SWITCH_TO_JOYCON_BUTTON {
            let port = params.get_int("port", 0) as usize;
            let mut pad = params.get_int("pad", 0) as u8;
            if pad == ControllerType::Dual as u8 {
                pad = if side {
                    ControllerType::Right as u8
                } else {
                    ControllerType::Left as u8
                };
            }
            let pad_type = match pad {
                x if x == ControllerType::Left as u8 => ControllerType::Left,
                x if x == ControllerType::Right as u8 => ControllerType::Right,
                x if x == ControllerType::Pro as u8 => ControllerType::Pro,
                _ => ControllerType::None,
            };

            let mut button_params = self.get_param_package(port, pad_type);
            button_params.set_int("button", joycon_button as i32);
            mapping.insert(switch_button as i32, button_params);
        }

        // Map SL and SR buttons for left joycons
        if params.get_int("pad", 0) == ControllerType::Left as i32 {
            let port = params.get_int("port", 0) as usize;
            let button_params = self.get_param_package(port, ControllerType::Left);

            let mut sl_button_params = button_params.clone();
            let mut sr_button_params = button_params;
            sl_button_params.set_int("button", PadButton::LeftSL as i32);
            sr_button_params.set_int("button", PadButton::LeftSR as i32);
            mapping.insert(native_button::Values::SLLeft as i32, sl_button_params);
            mapping.insert(native_button::Values::SRLeft as i32, sr_button_params);
        }

        // Map SL and SR buttons for right joycons
        if params.get_int("pad", 0) == ControllerType::Right as i32 {
            let port = params.get_int("port", 0) as usize;
            let button_params = self.get_param_package(port, ControllerType::Right);

            let mut sl_button_params = button_params.clone();
            let mut sr_button_params = button_params;
            sl_button_params.set_int("button", PadButton::RightSL as i32);
            sr_button_params.set_int("button", PadButton::RightSR as i32);
            mapping.insert(native_button::Values::SLRight as i32, sl_button_params);
            mapping.insert(native_button::Values::SRRight as i32, sr_button_params);
        }

        mapping
    }

    /// Port of Joycons::GetAnalogMappingForDevice (override)
    pub fn get_analog_mapping_for_device(&self, params: &ParamPackage) -> AnalogMapping {
        if !params.has("port") {
            return AnalogMapping::new();
        }

        let port = params.get_int("port", 0) as usize;
        let pad_raw = params.get_int("pad", 0) as u8;
        let mut pad_left = pad_raw;
        let mut pad_right = pad_raw;
        if pad_raw == ControllerType::Dual as u8 {
            pad_left = ControllerType::Left as u8;
            pad_right = ControllerType::Right as u8;
        }

        let pad_left_type = match pad_left {
            x if x == ControllerType::Left as u8 => ControllerType::Left,
            x if x == ControllerType::Right as u8 => ControllerType::Right,
            x if x == ControllerType::Pro as u8 => ControllerType::Pro,
            _ => ControllerType::None,
        };
        let pad_right_type = match pad_right {
            x if x == ControllerType::Left as u8 => ControllerType::Left,
            x if x == ControllerType::Right as u8 => ControllerType::Right,
            x if x == ControllerType::Pro as u8 => ControllerType::Pro,
            _ => ControllerType::None,
        };

        let mut mapping = AnalogMapping::new();
        let mut left_analog_params = self.get_param_package(port, pad_left_type);
        left_analog_params.set_int("axis_x", PadAxes::LeftStickX as i32);
        left_analog_params.set_int("axis_y", PadAxes::LeftStickY as i32);
        mapping.insert(native_analog::Values::LStick as i32, left_analog_params);

        let mut right_analog_params = self.get_param_package(port, pad_right_type);
        right_analog_params.set_int("axis_x", PadAxes::RightStickX as i32);
        right_analog_params.set_int("axis_y", PadAxes::RightStickY as i32);
        mapping.insert(native_analog::Values::RStick as i32, right_analog_params);
        mapping
    }

    /// Port of Joycons::GetMotionMappingForDevice (override)
    pub fn get_motion_mapping_for_device(&self, params: &ParamPackage) -> MotionMapping {
        if !params.has("port") {
            return MotionMapping::new();
        }

        let port = params.get_int("port", 0) as usize;
        let pad_raw = params.get_int("pad", 0) as u8;
        let mut pad_left = pad_raw;
        let mut pad_right = pad_raw;
        if pad_raw == ControllerType::Dual as u8 {
            pad_left = ControllerType::Left as u8;
            pad_right = ControllerType::Right as u8;
        }

        let pad_left_type = match pad_left {
            x if x == ControllerType::Left as u8 => ControllerType::Left,
            _ => ControllerType::None,
        };
        let pad_right_type = match pad_right {
            x if x == ControllerType::Right as u8 => ControllerType::Right,
            x if x == ControllerType::Pro as u8 => ControllerType::Pro,
            _ => ControllerType::None,
        };

        let mut mapping = MotionMapping::new();
        let mut left_motion_params = self.get_param_package(port, pad_left_type);
        left_motion_params.set_int("motion", 0);
        mapping.insert(native_motion::Values::MotionLeft as i32, left_motion_params);

        let mut right_motion_params = self.get_param_package(port, pad_right_type);
        right_motion_params.set_int("motion", 1);
        mapping.insert(
            native_motion::Values::MotionRight as i32,
            right_motion_params,
        );
        mapping
    }

    /// Port of Joycons::GetUIName (override)
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

    // ---- Private methods ----

    /// Port of Joycons::Reset
    fn reset(&mut self) {
        // In C++: stops scan_thread and all JoyconDriver instances, calls SDL_hid_exit().
    }

    /// Port of Joycons::GetIdentifier
    fn get_identifier(&self, port: usize, controller_type: ControllerType) -> PadIdentifier {
        let mut guid = [0u8; 16];
        guid[15] = controller_type as u8;
        PadIdentifier {
            guid: UUID::from_bytes(guid),
            port,
            pad: controller_type as usize,
        }
    }

    /// Port of Joycons::GetParamPackage
    fn get_param_package(&self, port: usize, controller_type: ControllerType) -> ParamPackage {
        let identifier = self.get_identifier(port, controller_type);
        let mut params = ParamPackage::default();
        params.set_str("engine", self.engine.lock().get_engine_name().to_string());
        params.set_str("guid", identifier.guid.raw_string());
        params.set_int("port", identifier.port as i32);
        params.set_int("pad", identifier.pad as i32);
        params
    }

    /// Port of Joycons::GetUIButtonName
    fn get_ui_button_name(&self, params: &ParamPackage) -> ButtonNames {
        let button_raw = params.get_int("button", 0) as u32;
        match button_raw {
            x if x == PadButton::Left as u32 => ButtonNames::ButtonLeft,
            x if x == PadButton::Right as u32 => ButtonNames::ButtonRight,
            x if x == PadButton::Down as u32 => ButtonNames::ButtonDown,
            x if x == PadButton::Up as u32 => ButtonNames::ButtonUp,
            x if x == PadButton::LeftSL as u32 || x == PadButton::RightSL as u32 => {
                ButtonNames::TriggerSL
            }
            x if x == PadButton::LeftSR as u32 || x == PadButton::RightSR as u32 => {
                ButtonNames::TriggerSR
            }
            x if x == PadButton::L as u32 => ButtonNames::TriggerL,
            x if x == PadButton::R as u32 => ButtonNames::TriggerR,
            x if x == PadButton::ZL as u32 => ButtonNames::TriggerZL,
            x if x == PadButton::ZR as u32 => ButtonNames::TriggerZR,
            x if x == PadButton::A as u32 => ButtonNames::ButtonA,
            x if x == PadButton::B as u32 => ButtonNames::ButtonB,
            x if x == PadButton::X as u32 => ButtonNames::ButtonX,
            x if x == PadButton::Y as u32 => ButtonNames::ButtonY,
            x if x == PadButton::Plus as u32 => ButtonNames::ButtonPlus,
            x if x == PadButton::Minus as u32 => ButtonNames::ButtonMinus,
            x if x == PadButton::Home as u32 => ButtonNames::ButtonHome,
            x if x == PadButton::Capture as u32 => ButtonNames::ButtonCapture,
            x if x == PadButton::StickL as u32 => ButtonNames::ButtonStickL,
            x if x == PadButton::StickR as u32 => ButtonNames::ButtonStickR,
            _ => ButtonNames::Undefined,
        }
    }
}

impl Drop for Joycons {
    fn drop(&mut self) {
        self.reset();
    }
}
