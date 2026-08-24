// SPDX-FileCopyrightText: Copyright 2021 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `input_common/drivers/mouse.h` and `input_common/drivers/mouse.cpp`.
//!
//! Mouse input driver that receives mouse events and forwards them to input devices.

use common::input::ButtonNames;
use common::param_package::ParamPackage;
use parking_lot::Mutex;
use std::sync::Arc;

use crate::input_engine::{BasicMotion, InputEngine, PadIdentifier};
use crate::main_common::AnalogMapping;

const UPDATE_TIME: i32 = 10;
const DEFAULT_PANNING_SENSITIVITY: f32 = 0.0010;
const DEFAULT_STICK_SENSITIVITY: f32 = 0.0006;
const DEFAULT_DEADZONE_COUNTERWEIGHT: f32 = 0.01;
const DEFAULT_MOTION_PANNING_SENSITIVITY: f32 = 2.5;
const DEFAULT_MOTION_SENSITIVITY: f32 = 0.416;
const MAXIMUM_ROTATION_SPEED: f32 = 2.0;
const MAXIMUM_STICK_RANGE: f32 = 1.5;
const MOUSE_AXIS_X: i32 = 0;
const MOUSE_AXIS_Y: i32 = 1;
const WHEEL_AXIS_X: i32 = 2;
const WHEEL_AXIS_Y: i32 = 3;

fn identifier() -> PadIdentifier {
    PadIdentifier {
        guid: Default::default(),
        port: 0,
        pad: 0,
    }
}

fn motion_identifier() -> PadIdentifier {
    PadIdentifier {
        guid: Default::default(),
        port: 0,
        pad: 1,
    }
}

fn real_mouse_identifier() -> PadIdentifier {
    PadIdentifier {
        guid: Default::default(),
        port: 1,
        pad: 0,
    }
}

fn touch_identifier() -> PadIdentifier {
    PadIdentifier {
        guid: Default::default(),
        port: 2,
        pad: 0,
    }
}

/// Port of `MouseButton` enum from mouse.h
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Wheel,
    Backward,
    Forward,
    Task,
    Extra,
    Undefined,
}

/// Port of `Mouse` class from mouse.h / mouse.cpp
pub struct Mouse {
    engine: Arc<Mutex<InputEngine>>,
    mouse_origin: (i32, i32),
    last_mouse_change: (f32, f32),
    last_motion_change: (f32, f32, f32),
    wheel_position: (i32, i32),
    button_pressed: bool,
}

impl Mouse {
    /// Port of Mouse::Mouse
    pub fn new(input_engine: String) -> Self {
        let engine = Arc::new(Mutex::new(InputEngine::new(input_engine)));
        {
            let mut engine = engine.lock();
            engine.pre_set_controller(&identifier());
            engine.pre_set_controller(&real_mouse_identifier());
            engine.pre_set_controller(&touch_identifier());
            engine.pre_set_controller(&motion_identifier());
            engine.pre_set_axis(&identifier(), MOUSE_AXIS_X);
            engine.pre_set_axis(&identifier(), MOUSE_AXIS_Y);
            engine.pre_set_axis(&identifier(), WHEEL_AXIS_X);
            engine.pre_set_axis(&identifier(), WHEEL_AXIS_Y);
            engine.pre_set_axis(&real_mouse_identifier(), MOUSE_AXIS_X);
            engine.pre_set_axis(&real_mouse_identifier(), MOUSE_AXIS_Y);
            engine.pre_set_axis(&touch_identifier(), MOUSE_AXIS_X);
            engine.pre_set_axis(&touch_identifier(), MOUSE_AXIS_Y);
        }
        Self {
            engine,
            mouse_origin: (0, 0),
            last_mouse_change: (0.0, 0.0),
            last_motion_change: (0.0, 0.0, 0.0),
            wheel_position: (0, 0),
            button_pressed: false,
        }
    }

    /// Returns the shared input engine used by the registered factories.
    pub fn engine(&self) -> Arc<Mutex<InputEngine>> {
        Arc::clone(&self.engine)
    }

    /// Port of Mouse::Move.
    pub fn move_cursor(&mut self, x: i32, y: i32, center_x: i32, center_y: i32) {
        let mouse_change = (x - center_x, y - center_y);
        if self.is_mouse_panning_enabled() {
            let settings = common::settings::values();
            let x_sensitivity = *settings.mouse_panning_x_sensitivity.get_value() as f32
                * DEFAULT_PANNING_SENSITIVITY;
            let y_sensitivity = *settings.mouse_panning_y_sensitivity.get_value() as f32
                * DEFAULT_PANNING_SENSITIVITY;
            let deadzone_counterweight = *settings.mouse_panning_deadzone_counterweight.get_value()
                as f32
                * DEFAULT_DEADZONE_COUNTERWEIGHT;
            self.last_motion_change.0 += -(mouse_change.1 as f32) * x_sensitivity;
            self.last_motion_change.1 += -(mouse_change.0 as f32) * y_sensitivity;
            self.last_mouse_change.0 += mouse_change.0 as f32 * x_sensitivity;
            self.last_mouse_change.1 += mouse_change.1 as f32 * y_sensitivity;
            let length =
                (self.last_mouse_change.0.powi(2) + self.last_mouse_change.1.powi(2)).sqrt();
            if length < deadzone_counterweight && length != 0.0 {
                self.last_mouse_change.0 =
                    self.last_mouse_change.0 / length * deadzone_counterweight;
                self.last_mouse_change.1 =
                    self.last_mouse_change.1 / length * deadzone_counterweight;
            }
            return;
        }

        if self.button_pressed {
            let mouse_move = (x - self.mouse_origin.0, y - self.mouse_origin.1);
            let settings = common::settings::values();
            let x_sensitivity = *settings.mouse_panning_x_sensitivity.get_value() as f32
                * DEFAULT_STICK_SENSITIVITY;
            let y_sensitivity = *settings.mouse_panning_y_sensitivity.get_value() as f32
                * DEFAULT_STICK_SENSITIVITY;
            let pending = {
                let mut engine = self.engine.lock();
                vec![
                    engine.set_axis(
                        &identifier(),
                        MOUSE_AXIS_X,
                        mouse_move.0 as f32 * x_sensitivity,
                    ),
                    engine.set_axis(
                        &identifier(),
                        MOUSE_AXIS_Y,
                        -(mouse_move.1 as f32) * y_sensitivity,
                    ),
                ]
            };
            for callbacks in pending {
                callbacks.dispatch();
            }
            self.last_motion_change.0 = -(mouse_move.1 as f32) * x_sensitivity;
            self.last_motion_change.1 = -(mouse_move.0 as f32) * y_sensitivity;
        }
    }

    /// Signals that real mouse has moved.
    /// Port of Mouse::MouseMove
    pub fn mouse_move(&mut self, touch_x: f32, touch_y: f32) {
        let id = real_mouse_identifier();
        let pending = {
            let mut engine = self.engine.lock();
            vec![
                engine.set_axis(&id, MOUSE_AXIS_X, touch_x),
                engine.set_axis(&id, MOUSE_AXIS_Y, touch_y),
            ]
        };
        for callbacks in pending {
            callbacks.dispatch();
        }
    }

    /// Signals that touch finger has moved.
    /// Port of Mouse::TouchMove
    pub fn touch_move(&mut self, touch_x: f32, touch_y: f32) {
        let id = touch_identifier();
        let pending = {
            let mut engine = self.engine.lock();
            vec![
                engine.set_axis(&id, MOUSE_AXIS_X, touch_x),
                engine.set_axis(&id, MOUSE_AXIS_Y, touch_y),
            ]
        };
        for callbacks in pending {
            callbacks.dispatch();
        }
    }

    /// Sets the status of a button to pressed.
    /// Port of Mouse::PressButton
    pub fn press_button(&mut self, x: i32, y: i32, button: MouseButton) {
        let id = identifier();
        let pending = self.engine.lock().set_button(&id, button as i32, true);
        pending.dispatch();

        // Set initial analog parameters
        self.mouse_origin = (x, y);
        self.button_pressed = true;
    }

    /// Sets the status of a mouse button to pressed.
    /// Port of Mouse::PressMouseButton
    pub fn press_mouse_button(&mut self, button: MouseButton) {
        let id = real_mouse_identifier();
        let pending = self.engine.lock().set_button(&id, button as i32, true);
        pending.dispatch();
    }

    /// Sets the status of touch finger to pressed.
    /// Port of Mouse::PressTouchButton
    pub fn press_touch_button(&mut self, touch_x: f32, touch_y: f32, button: MouseButton) {
        let id = touch_identifier();
        let pending = {
            let mut engine = self.engine.lock();
            vec![
                engine.set_axis(&id, MOUSE_AXIS_X, touch_x),
                engine.set_axis(&id, MOUSE_AXIS_Y, touch_y),
                engine.set_button(&id, button as i32, true),
            ]
        };
        for callbacks in pending {
            callbacks.dispatch();
        }
    }

    /// Sets the status of all buttons bound with the key to released.
    /// Port of Mouse::ReleaseButton
    pub fn release_button(&mut self, button: MouseButton) {
        let id = identifier();
        let real_id = real_mouse_identifier();
        let touch_id = touch_identifier();

        let reset_stick = !self.is_mouse_panning_enabled();
        let pending = {
            let mut engine = self.engine.lock();
            let mut pending = vec![
                engine.set_button(&id, button as i32, false),
                engine.set_button(&real_id, button as i32, false),
                engine.set_button(&touch_id, button as i32, false),
            ];
            if reset_stick {
                pending.push(engine.set_axis(&id, MOUSE_AXIS_X, 0.0));
                pending.push(engine.set_axis(&id, MOUSE_AXIS_Y, 0.0));
            }
            pending
        };
        for callbacks in pending {
            callbacks.dispatch();
        }

        self.last_motion_change.0 = 0.0;
        self.last_motion_change.1 = 0.0;

        self.button_pressed = false;
    }

    /// Sets the status of the mouse wheel.
    /// Port of Mouse::MouseWheelChange
    pub fn mouse_wheel_change(&mut self, x: i32, y: i32) {
        self.wheel_position.0 += x;
        self.wheel_position.1 += y;
        self.last_motion_change.2 += y as f32;
        let id = identifier();
        let pending = {
            let mut engine = self.engine.lock();
            vec![
                engine.set_axis(&id, WHEEL_AXIS_X, self.wheel_position.0 as f32),
                engine.set_axis(&id, WHEEL_AXIS_Y, self.wheel_position.1 as f32),
            ]
        };
        for callbacks in pending {
            callbacks.dispatch();
        }
    }

    /// Port of Mouse::ReleaseAllButtons
    pub fn release_all_buttons(&mut self) {
        let pending = self.engine.lock().reset_button_state();
        for callbacks in pending {
            callbacks.dispatch();
        }
        self.button_pressed = false;
    }

    /// Notifies the engine that accumulated mouse state must be published.
    /// Port of Mouse::NotifyChanged.
    pub fn notify_changed(&mut self) {
        self.update_stick_input();
        self.update_motion_input();
    }

    /// Port of Mouse::GetInputDevices (override)
    pub fn get_input_devices(&self) -> Vec<ParamPackage> {
        let mut param = ParamPackage::default();
        param.set_str("engine", self.engine.lock().get_engine_name().to_string());
        param.set_str("display", "Keyboard/Mouse".to_string());
        vec![param]
    }

    /// Port of Mouse::GetAnalogMappingForDevice (override)
    pub fn get_analog_mapping_for_device(&self, _params: &ParamPackage) -> AnalogMapping {
        // Only overwrite different buttons from default
        let mut mapping = AnalogMapping::new();
        let mut right_analog_params = ParamPackage::default();
        right_analog_params.set_str("engine", self.engine.lock().get_engine_name().to_string());
        right_analog_params.set_int("axis_x", 0);
        right_analog_params.set_int("axis_y", 1);
        right_analog_params.set_float("threshold", 0.5);
        right_analog_params.set_float("range", 1.0);
        right_analog_params.set_float("deadzone", 0.0);
        // Settings::NativeAnalog::RStick == 1
        mapping.insert(1, right_analog_params);
        mapping
    }

    /// Port of Mouse::GetUIName (override)
    pub fn get_ui_name(&self, params: &ParamPackage) -> ButtonNames {
        if params.has("button") {
            return self.get_ui_button_name(params);
        }
        if params.has("axis") {
            return ButtonNames::Value;
        }
        if params.has("axis_x") && params.has("axis_y") && params.has("axis_z") {
            return ButtonNames::Engine;
        }
        if params.has("motion") {
            return ButtonNames::Engine;
        }
        ButtonNames::Invalid
    }

    // ---- Private methods ----

    /// Port of Mouse::UpdateStickInput.
    fn update_stick_input(&mut self) {
        if !self.is_mouse_panning_enabled() {
            return;
        }

        let length = (self.last_mouse_change.0.powi(2) + self.last_mouse_change.1.powi(2)).sqrt();
        if length > MAXIMUM_STICK_RANGE {
            self.last_mouse_change.0 = self.last_mouse_change.0 / length * MAXIMUM_STICK_RANGE;
            self.last_mouse_change.1 = self.last_mouse_change.1 / length * MAXIMUM_STICK_RANGE;
        }

        let pending = {
            let mut engine = self.engine.lock();
            vec![
                engine.set_axis(&identifier(), MOUSE_AXIS_X, self.last_mouse_change.0),
                engine.set_axis(&identifier(), MOUSE_AXIS_Y, -self.last_mouse_change.1),
            ]
        };
        for callbacks in pending {
            callbacks.dispatch();
        }

        let settings = common::settings::values();
        let clamped_length = length.min(1.0);
        let decay_strength = *settings.mouse_panning_decay_strength.get_value() as f32;
        let decay = 1.0 - clamped_length * clamped_length * decay_strength * 0.01;
        let min_decay = *settings.mouse_panning_min_decay.get_value() as f32;
        let clamped_decay = (1.0 - min_decay / 100.0).min(decay);
        self.last_mouse_change.0 *= clamped_decay;
        self.last_mouse_change.1 *= clamped_decay;
    }

    /// Port of Mouse::UpdateMotionInput.
    fn update_motion_input(&mut self) {
        let panning_enabled = self.is_mouse_panning_enabled();
        let sensitivity = if panning_enabled {
            DEFAULT_MOTION_PANNING_SENSITIVITY
        } else {
            DEFAULT_MOTION_SENSITIVITY
        };

        let rotation_velocity =
            (self.last_motion_change.0.powi(2) + self.last_motion_change.1.powi(2)).sqrt();
        if rotation_velocity > MAXIMUM_ROTATION_SPEED / sensitivity {
            let multiplier = MAXIMUM_ROTATION_SPEED / rotation_velocity / sensitivity;
            self.last_motion_change.0 *= multiplier;
            self.last_motion_change.1 *= multiplier;
        }

        let motion_data = BasicMotion {
            gyro_x: self.last_motion_change.0 * sensitivity,
            gyro_y: self.last_motion_change.1 * sensitivity,
            gyro_z: self.last_motion_change.2 * sensitivity,
            accel_x: 0.0,
            accel_y: 0.0,
            accel_z: 0.0,
            delta_timestamp: UPDATE_TIME as u64 * 1000,
        };

        if panning_enabled {
            self.last_motion_change.0 = 0.0;
            self.last_motion_change.1 = 0.0;
        }
        self.last_motion_change.2 = 0.0;

        let pending = self
            .engine
            .lock()
            .set_motion(&motion_identifier(), 0, &motion_data);
        pending.dispatch();
    }

    /// Port of Mouse::IsMousePanningEnabled
    fn is_mouse_panning_enabled(&self) -> bool {
        let settings = common::settings::values();
        *settings.mouse_panning.get_value() && !*settings.mouse_enabled.get_value()
    }

    /// Port of Mouse::GetUIButtonName
    fn get_ui_button_name(&self, params: &ParamPackage) -> ButtonNames {
        let button_value = params.get_int("button", 0);
        // Match MouseButton enum order
        match button_value {
            0 => ButtonNames::ButtonLeft,
            1 => ButtonNames::ButtonRight,
            2 => ButtonNames::ButtonMouseWheel,
            3 => ButtonNames::ButtonBackward,
            4 => ButtonNames::ButtonForward,
            5 => ButtonNames::ButtonTask,
            6 => ButtonNames::ButtonExtra,
            _ => ButtonNames::Undefined,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_changed_publishes_upstream_motion_sample() {
        let mut mouse = Mouse::new("mouse-test".to_string());
        mouse.last_motion_change = (1.0, -2.0, 3.0);
        let panning_enabled = mouse.is_mouse_panning_enabled();
        let sensitivity = if panning_enabled {
            DEFAULT_MOTION_PANNING_SENSITIVITY
        } else {
            DEFAULT_MOTION_SENSITIVITY
        };

        mouse.notify_changed();

        let motion = mouse.engine.lock().get_motion(&motion_identifier(), 0);
        let rotation_velocity = 5.0_f32.sqrt();
        let multiplier = if rotation_velocity > MAXIMUM_ROTATION_SPEED / sensitivity {
            MAXIMUM_ROTATION_SPEED / rotation_velocity / sensitivity
        } else {
            1.0
        };
        assert!((motion.gyro_x - multiplier * sensitivity).abs() < f32::EPSILON * 8.0);
        assert!((motion.gyro_y + 2.0 * multiplier * sensitivity).abs() < f32::EPSILON * 8.0);
        assert!((motion.gyro_z - 3.0 * sensitivity).abs() < f32::EPSILON * 8.0);
        assert_eq!(motion.delta_timestamp, UPDATE_TIME as u64 * 1000);
        assert_eq!(mouse.last_motion_change.2, 0.0);
    }
}
