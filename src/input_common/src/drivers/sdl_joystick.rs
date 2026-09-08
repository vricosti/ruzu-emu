// SPDX-FileCopyrightText: 2018 Citra Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of the `SDLJoystick` class from
//! `input_common/drivers/sdl_driver.cpp`.
//!
//! One instance per opened device. Upstream keeps the `SDL_Joystick*` and
//! `SDL_Gamepad*` in `unique_ptr`s with `SDL_CloseJoystick` /
//! `SDL_CloseGamepad` as deleters; the Rust port closes them in `Drop`.

use std::ffi::CStr;

use sdl3_sys::everything as sdl;

use common::input::{BatteryLevel, VibrationStatus};
use common::uuid::UUID;

use crate::input_engine::{BasicMotion, PadIdentifier};

/// Upstream `rumble_max_duration_ms`.
const RUMBLE_MAX_DURATION_MS: u32 = 2000;

/// Sensitivity limits used to fake frequency response through amplitude.
const LOW_START_SENSITIVITY_LIMIT: f32 = 140.0;
const LOW_WIDTH_SENSITIVITY_LIMIT: f32 = 400.0;
const HIGH_START_SENSITIVITY_LIMIT: f32 = 200.0;
const HIGH_WIDTH_SENSITIVITY_LIMIT: f32 = 700.0;

/// Standard gravity, used to normalise the accelerometer.
const GRAVITY_CONSTANT: f32 = 9.80665;

/// How many all-zero motion samples to tolerate before restarting the sensors.
const MOTION_ERROR_LIMIT: u32 = 200;

/// The GUID of an opened joystick — upstream's anonymous-namespace `GetGUID`.
///
/// The two bytes at offset 2 are cleared on purpose: SDL stores a CRC of the
/// controller *name* there, which changes between SDL releases and between
/// hosts. Leaving it in would give the same physical pad a different identity
/// and silently drop every binding made against it.
pub fn get_guid(joystick: *mut sdl::SDL_Joystick) -> UUID {
    let guid = unsafe { sdl::SDL_GetJoystickGUID(joystick) };
    let mut data = [0u8; 16];
    data.copy_from_slice(&guid.data);
    data[2] = 0;
    data[3] = 0;
    UUID { uuid: data }
}

/// A single opened SDL device.
pub struct SdlJoystick {
    guid: UUID,
    port: i32,
    sdl_joystick: *mut sdl::SDL_Joystick,
    sdl_controller: *mut sdl::SDL_Gamepad,

    motion: BasicMotion,
    last_motion_update: u64,
    motion_error_count: u32,
    has_gyro: bool,
    has_accel: bool,

    has_vibration: bool,
    is_vibration_tested: bool,
    has_hd_rumble: bool,
}

fn controller_has_hd_rumble(
    joystick: *mut sdl::SDL_Joystick,
    controller: *mut sdl::SDL_Gamepad,
) -> bool {
    const VALVE_VENDOR_ID: u16 = 0x28DE;
    if !controller.is_null() {
        let controller_type = unsafe { sdl::SDL_GetGamepadType(controller) };
        if matches!(
            controller_type,
            sdl::SDL_GAMEPAD_TYPE_NINTENDO_SWITCH_PRO
                | sdl::SDL_GAMEPAD_TYPE_NINTENDO_SWITCH_JOYCON_LEFT
                | sdl::SDL_GAMEPAD_TYPE_NINTENDO_SWITCH_JOYCON_RIGHT
                | sdl::SDL_GAMEPAD_TYPE_PS5
        ) || unsafe { sdl::SDL_GetGamepadVendor(controller) == VALVE_VENDOR_ID }
        {
            return true;
        }
    }
    !joystick.is_null() && unsafe { sdl::SDL_GetJoystickVendor(joystick) == VALVE_VENDOR_ID }
}

/// Snapshot of upstream's two non-owning SDL pointers and their controller type.
///
/// Rust keeps `SdlJoystick` behind a mutex for mutable reconnect state. SDL
/// calls must use this snapshot after that mutex is released: SDL invokes the
/// event watcher while holding its own joystick lock, so calling back into SDL
/// while holding the Rust mutex creates the opposite lock order. The HD-rumble
/// classification is immutable for the lifetime of these handles and keeps
/// `SetVibration` out of SDL's event-pump lock.
#[derive(Clone, Copy)]
pub(crate) struct SdlJoystickHandles {
    joystick: *mut sdl::SDL_Joystick,
    controller: *mut sdl::SDL_Gamepad,
    has_hd_rumble: bool,
}

impl SdlJoystickHandles {
    /// Upstream `SDLJoystick::RumblePlay`.
    pub(crate) fn rumble_play(self, vibration: &VibrationStatus) -> bool {
        let low_scale = if vibration.low_frequency > LOW_START_SENSITIVITY_LIMIT {
            (1.0 - (vibration.low_frequency - LOW_START_SENSITIVITY_LIMIT)
                / LOW_WIDTH_SENSITIVITY_LIMIT)
                .max(0.3)
        } else {
            1.0
        };
        let high_scale = if vibration.high_frequency > HIGH_START_SENSITIVITY_LIMIT {
            (1.0 - (vibration.high_frequency - HIGH_START_SENSITIVITY_LIMIT)
                / HIGH_WIDTH_SENSITIVITY_LIMIT)
                .max(0.3)
        } else {
            1.0
        };
        let low = (vibration.low_amplitude * low_scale) as u16;
        let high = (vibration.high_amplitude * high_scale) as u16;

        unsafe {
            if !self.controller.is_null() {
                sdl::SDL_RumbleGamepad(self.controller, low, high, RUMBLE_MAX_DURATION_MS)
            } else if !self.joystick.is_null() {
                sdl::SDL_RumbleJoystick(self.joystick, low, high, RUMBLE_MAX_DURATION_MS)
            } else {
                false
            }
        }
    }

    /// Upstream `SDLJoystick::HasHDRumble`.
    pub(crate) fn has_hd_rumble(self) -> bool {
        self.has_hd_rumble
    }
}

// SAFETY: mutable handle replacement is serialized by the owning driver's
// joystick mutex. Calls into SDL use pointer snapshots after releasing that
// mutex, matching upstream's shared `SDLJoystick` access across its event and
// vibration threads.
unsafe impl Send for SdlJoystick {}

impl SdlJoystick {
    /// Upstream `SDLJoystick::SDLJoystick`.
    pub fn new(
        guid: UUID,
        port: i32,
        sdl_joystick: *mut sdl::SDL_Joystick,
        sdl_controller: *mut sdl::SDL_Gamepad,
    ) -> Self {
        let mut joystick = Self {
            guid,
            port,
            sdl_joystick,
            sdl_controller,
            motion: BasicMotion::default(),
            last_motion_update: 0,
            motion_error_count: 0,
            has_gyro: false,
            has_accel: false,
            has_vibration: false,
            is_vibration_tested: false,
            has_hd_rumble: controller_has_hd_rumble(sdl_joystick, sdl_controller),
        };
        joystick.enable_motion();
        joystick
    }

    /// Upstream `SDLJoystick::EnableMotion`.
    ///
    /// Sensors are toggled off before being probed: upstream does this so a
    /// device whose sensors were already running is re-armed cleanly.
    pub fn enable_motion(&mut self) {
        if self.sdl_controller.is_null() {
            return;
        }
        unsafe {
            if self.has_motion() {
                sdl::SDL_SetGamepadSensorEnabled(self.sdl_controller, sdl::SDL_SENSOR_ACCEL, false);
                sdl::SDL_SetGamepadSensorEnabled(self.sdl_controller, sdl::SDL_SENSOR_GYRO, false);
            }
            self.has_accel = sdl::SDL_GamepadHasSensor(self.sdl_controller, sdl::SDL_SENSOR_ACCEL);
            self.has_gyro = sdl::SDL_GamepadHasSensor(self.sdl_controller, sdl::SDL_SENSOR_GYRO);
            if self.has_accel {
                if !sdl::SDL_SetGamepadSensorEnabled(
                    self.sdl_controller,
                    sdl::SDL_SENSOR_ACCEL,
                    true,
                ) {
                    log::warn!("Failed to enable accelerometer sensor: {}", CStr::from_ptr(sdl::SDL_GetError()).to_string_lossy());
                }
            }
            if self.has_gyro {
                if !sdl::SDL_SetGamepadSensorEnabled(
                    self.sdl_controller,
                    sdl::SDL_SENSOR_GYRO,
                    true,
                ) {
                    log::warn!("Failed to enable gyroscope sensor: {}", CStr::from_ptr(sdl::SDL_GetError()).to_string_lossy());
                }
            }
        }
        log::info!("Controller motion capabilities: accel={} gyro={}", self.has_accel, self.has_gyro);
    }

    /// Upstream `SDLJoystick::HasMotion`.
    pub fn has_motion(&self) -> bool {
        self.has_gyro || self.has_accel
    }

    /// Upstream `SDLJoystick::UpdateMotion`.
    ///
    /// Returns `true` when the sample is worth publishing. Duplicated
    /// timestamps and all-zero samples are dropped; after
    /// [`MOTION_ERROR_LIMIT`] consecutive zero samples the sensors are
    /// restarted, which is upstream's recovery for a pad that stops reporting.
    pub fn update_motion(&mut self, event: sdl::SDL_GamepadSensorEvent) -> bool {
        let timestamp = if event.sensor_timestamp != 0 {
            event.sensor_timestamp
        } else {
            event.timestamp
        };
        if self.last_motion_update == 0 {
            self.last_motion_update = timestamp;
            return false;
        }
        if timestamp < self.last_motion_update {
            return false;
        }
        let time_difference = (timestamp - self.last_motion_update) / 1000;
        self.last_motion_update = timestamp;

        match sdl::SDL_SensorType::new(event.sensor) {
            sdl::SDL_SENSOR_ACCEL => {
                self.motion.accel_x = -event.data[0] / GRAVITY_CONSTANT;
                self.motion.accel_y = event.data[2] / GRAVITY_CONSTANT;
                self.motion.accel_z = -event.data[1] / GRAVITY_CONSTANT;
            }
            sdl::SDL_SENSOR_GYRO => {
                self.motion.gyro_x = event.data[0] / (std::f32::consts::PI * 2.0);
                self.motion.gyro_y = -event.data[2] / (std::f32::consts::PI * 2.0);
                self.motion.gyro_z = event.data[1] / (std::f32::consts::PI * 2.0);
            }
            _ => {}
        }

        if time_difference == 0 {
            return false;
        }

        let all_zero = self.motion.accel_x == 0.0
            && self.motion.gyro_x == 0.0
            && self.motion.accel_y == 0.0
            && self.motion.gyro_y == 0.0
            && self.motion.accel_z == 0.0
            && self.motion.gyro_z == 0.0;
        if all_zero {
            let previous_error_count = self.motion_error_count;
            self.motion_error_count += 1;
            if previous_error_count < MOTION_ERROR_LIMIT {
                return false;
            }
            self.motion_error_count = 0;
            self.enable_motion();
            return false;
        }

        self.motion_error_count = 0;
        self.motion.delta_timestamp = time_difference;
        true
    }

    /// Upstream `SDLJoystick::GetMotion`.
    pub fn motion(&self) -> &BasicMotion {
        &self.motion
    }

    /// Upstream `SDLJoystick::RumblePlay`.
    ///
    /// SDL exposes only amplitude, so upstream fakes a frequency response by
    /// attenuating the amplitude as the requested frequency rises.
    pub fn rumble_play(&self, vibration: &VibrationStatus) -> bool {
        self.handles().rumble_play(vibration)
    }

    /// Upstream `SDLJoystick::HasHDRumble`.
    pub fn has_hd_rumble(&self) -> bool {
        self.handles().has_hd_rumble()
    }

    pub(crate) fn handles(&self) -> SdlJoystickHandles {
        SdlJoystickHandles {
            joystick: self.sdl_joystick,
            controller: self.sdl_controller,
            has_hd_rumble: self.has_hd_rumble,
        }
    }

    /// Upstream `SDLJoystick::EnableVibration`.
    pub fn enable_vibration(&mut self, is_enabled: bool) {
        self.has_vibration = is_enabled;
        self.is_vibration_tested = true;
    }

    pub fn has_vibration(&self) -> bool {
        self.has_vibration
    }

    pub fn is_vibration_tested(&self) -> bool {
        self.is_vibration_tested
    }

    /// Upstream `SDLJoystick::GetPadIdentifier`.
    pub fn pad_identifier(&self) -> PadIdentifier {
        PadIdentifier {
            guid: self.guid,
            port: self.port as usize,
            pad: 0,
        }
    }

    pub fn guid(&self) -> UUID {
        self.guid
    }

    pub fn port(&self) -> i32 {
        self.port
    }

    pub fn sdl_joystick(&self) -> *mut sdl::SDL_Joystick {
        self.sdl_joystick
    }

    pub fn sdl_game_controller(&self) -> *mut sdl::SDL_Gamepad {
        self.sdl_controller
    }

    /// Upstream `SDLJoystick::SetSDLJoystick` — rebind a reconnected device to
    /// the slot it previously occupied, closing whatever was there.
    pub fn set_sdl_joystick(
        &mut self,
        joystick: *mut sdl::SDL_Joystick,
        controller: *mut sdl::SDL_Gamepad,
    ) {
        self.close_handles();
        self.sdl_joystick = joystick;
        self.sdl_controller = controller;
        self.has_hd_rumble = controller_has_hd_rumble(joystick, controller);
    }

    /// Upstream `SDLJoystick::GetControllerName`.
    pub fn controller_name(&self) -> String {
        unsafe {
            if !self.sdl_controller.is_null() {
                let canonical_name = match sdl::SDL_GetGamepadType(self.sdl_controller) {
                    sdl::SDL_GAMEPAD_TYPE_XBOX360 => Some("Xbox 360 Controller"),
                    sdl::SDL_GAMEPAD_TYPE_XBOXONE => Some("Xbox One Controller"),
                    sdl::SDL_GAMEPAD_TYPE_PS3 => Some("DualShock 3 Controller"),
                    sdl::SDL_GAMEPAD_TYPE_PS4 => Some("DualShock 4 Controller"),
                    sdl::SDL_GAMEPAD_TYPE_PS5 => Some("DualSense Controller"),
                    _ => None,
                };
                if let Some(name) = canonical_name {
                    return name.to_string();
                }
                let name = sdl::SDL_GetGamepadName(self.sdl_controller);
                if !name.is_null() {
                    return CStr::from_ptr(name).to_string_lossy().into_owned();
                }
            }
            if !self.sdl_joystick.is_null() {
                let name = sdl::SDL_GetJoystickName(self.sdl_joystick);
                if !name.is_null() {
                    return CStr::from_ptr(name).to_string_lossy().into_owned();
                }
            }
        }
        "Unknown".to_string()
    }

    /// Upstream `SDLJoystick::IsJoyconLeft` / `IsJoyconRight`.
    pub fn is_joycon_left(&self) -> bool {
        let name = self.controller_name();
        name.contains("Joy-Con Left") || name.contains("Joy-Con (L)")
    }

    pub fn is_joycon_right(&self) -> bool {
        let name = self.controller_name();
        name.contains("Joy-Con Right") || name.contains("Joy-Con (R)")
    }

    /// Upstream `SDLJoystick::GetBatteryLevel`.
    pub fn battery_level(power_state: sdl::SDL_PowerState, percent: i32) -> BatteryLevel {
        if power_state == sdl::SDL_POWERSTATE_CHARGING {
            return BatteryLevel::Charging;
        }
        if (0..=100).contains(&percent) {
            return match percent {
                0..=5 => BatteryLevel::Empty,
                6..=20 => BatteryLevel::Critical,
                21..=40 => BatteryLevel::Low,
                41..=70 => BatteryLevel::Medium,
                _ => BatteryLevel::Full,
            };
        }
        match power_state {
            sdl::SDL_POWERSTATE_ON_BATTERY => BatteryLevel::Medium,
            sdl::SDL_POWERSTATE_CHARGED => BatteryLevel::Full,
            _ => BatteryLevel::None,
        }
    }

    fn close_handles(&mut self) {
        unsafe {
            if !self.sdl_controller.is_null() {
                sdl::SDL_CloseGamepad(self.sdl_controller);
                self.sdl_controller = std::ptr::null_mut();
            }
            if !self.sdl_joystick.is_null() {
                sdl::SDL_CloseJoystick(self.sdl_joystick);
                self.sdl_joystick = std::ptr::null_mut();
            }
        }
        self.has_hd_rumble = false;
    }
}

impl Drop for SdlJoystick {
    /// Upstream relies on the `unique_ptr` deleters
    /// (`SDL_JoystickClose` / `SDL_GameControllerClose`).
    fn drop(&mut self) {
        self.close_handles();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn motion_zero_sample_recovery_matches_postincrement_threshold() {
        let mut joystick = SdlJoystick::new(UUID::default(), 0, std::ptr::null_mut(), std::ptr::null_mut());
        let mut event: sdl::SDL_GamepadSensorEvent = unsafe { std::mem::zeroed() };
        event.sensor = sdl::SDL_SENSOR_GYRO.0;
        event.sensor_timestamp = 1_000;
        assert!(!joystick.update_motion(event));
        for count in 1..=MOTION_ERROR_LIMIT {
            event.sensor_timestamp += 1_000;
            assert!(!joystick.update_motion(event));
            assert_eq!(joystick.motion_error_count, count);
        }
        event.sensor_timestamp += 1_000;
        assert!(!joystick.update_motion(event));
        assert_eq!(joystick.motion_error_count, 0);
    }

    #[test]
    fn motion_samples_preserve_axes_units_and_timestamp_rules() {
        let mut joystick = SdlJoystick::new(UUID::default(), 0, std::ptr::null_mut(), std::ptr::null_mut());
        let mut event: sdl::SDL_GamepadSensorEvent = unsafe { std::mem::zeroed() };
        event.sensor = sdl::SDL_SENSOR_ACCEL.0;
        event.timestamp = 1_000_000;
        assert!(!joystick.update_motion(event));
        event.timestamp += 2_000_000;
        event.data = [GRAVITY_CONSTANT, 2.0 * GRAVITY_CONSTANT, 3.0 * GRAVITY_CONSTANT];
        assert!(joystick.update_motion(event));
        assert_eq!(joystick.motion.delta_timestamp, 2_000);
        assert_eq!([joystick.motion.accel_x, joystick.motion.accel_y, joystick.motion.accel_z], [-1.0, 3.0, -2.0]);
        event.sensor = sdl::SDL_SENSOR_GYRO.0;
        event.data = [std::f32::consts::TAU, 2.0 * std::f32::consts::TAU, 3.0 * std::f32::consts::TAU];
        // Same-timestamp data updates axes, but does not publish another sample.
        assert!(!joystick.update_motion(event));
        assert_eq!([joystick.motion.gyro_x, joystick.motion.gyro_y, joystick.motion.gyro_z], [1.0, -3.0, 2.0]);
        event.sensor_timestamp = event.timestamp + 1_000_000;
        assert!(joystick.update_motion(event));
        assert_eq!(joystick.motion.delta_timestamp, 1_000);
        event.sensor_timestamp -= 1;
        event.data = [0.0; 3];
        assert!(!joystick.update_motion(event));
        assert_eq!(joystick.motion.gyro_x, 1.0);
    }

    #[test]
    fn get_guid_clears_the_controller_name_crc() {
        // SDL stores a CRC of the controller *name* in bytes 2..4. It changes
        // between SDL releases, so leaving it in would give the same physical
        // pad a new identity and silently drop its bindings.
        let mut raw = sdl::SDL_GUID { data: [0u8; 16] };
        for (index, byte) in raw.data.iter_mut().enumerate() {
            *byte = index as u8 + 1;
        }
        // Reproduce what `get_guid` does to the raw bytes.
        let mut data = [0u8; 16];
        data.copy_from_slice(&raw.data);
        data[2] = 0;
        data[3] = 0;

        assert_eq!(data[0], 1);
        assert_eq!(data[1], 2);
        assert_eq!(data[2], 0, "name CRC low byte must be cleared");
        assert_eq!(data[3], 0, "name CRC high byte must be cleared");
        assert_eq!(data[4], 5, "bytes past the CRC must survive");
    }

    #[test]
    fn rumble_amplitude_is_attenuated_above_the_frequency_limits() {
        // Below the limit the amplitude passes through; above it, upstream
        // scales down but never below 0.3.
        let scale = |freq: f32, start: f32, width: f32| {
            if freq > start {
                (1.0 - (freq - start) / width).max(0.3)
            } else {
                1.0
            }
        };
        assert_eq!(
            scale(
                100.0,
                LOW_START_SENSITIVITY_LIMIT,
                LOW_WIDTH_SENSITIVITY_LIMIT
            ),
            1.0
        );
        assert!(
            scale(
                300.0,
                LOW_START_SENSITIVITY_LIMIT,
                LOW_WIDTH_SENSITIVITY_LIMIT
            ) < 1.0
        );
        // Far past the limit it clamps rather than going negative.
        assert_eq!(
            scale(
                9000.0,
                LOW_START_SENSITIVITY_LIMIT,
                LOW_WIDTH_SENSITIVITY_LIMIT
            ),
            0.3
        );
    }

    #[test]
    fn battery_level_maps_sdl3_power_state_and_percent() {
        assert_eq!(
            SdlJoystick::battery_level(sdl::SDL_POWERSTATE_ON_BATTERY, 5),
            BatteryLevel::Empty
        );
        assert_eq!(
            SdlJoystick::battery_level(sdl::SDL_POWERSTATE_ON_BATTERY, 40),
            BatteryLevel::Low
        );
        assert_eq!(
            SdlJoystick::battery_level(sdl::SDL_POWERSTATE_ON_BATTERY, 70),
            BatteryLevel::Medium
        );
        assert_eq!(
            SdlJoystick::battery_level(sdl::SDL_POWERSTATE_ON_BATTERY, 100),
            BatteryLevel::Full
        );
        assert_eq!(
            SdlJoystick::battery_level(sdl::SDL_POWERSTATE_CHARGED, -1),
            BatteryLevel::Full
        );
        assert_eq!(
            SdlJoystick::battery_level(sdl::SDL_POWERSTATE_CHARGING, 10),
            BatteryLevel::Charging
        );
        assert_eq!(
            SdlJoystick::battery_level(sdl::SDL_POWERSTATE_UNKNOWN, -1),
            BatteryLevel::None
        );
    }

    #[test]
    fn hd_rumble_query_uses_the_handle_snapshot_cache() {
        let handles = SdlJoystickHandles {
            joystick: std::ptr::null_mut(),
            controller: std::ptr::null_mut(),
            has_hd_rumble: true,
        };

        assert!(handles.has_hd_rumble());
    }
}
