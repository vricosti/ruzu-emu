// SPDX-FileCopyrightText: 2017 Citra Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `input_common/helpers/stick_from_buttons.h` and `stick_from_buttons.cpp`.
//!
//! An analog device factory that takes direction button devices and combines
//! them into an analog device.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use common::input::{
    self, AnalogProperties, ButtonStatus, CallbackStatus, InputCallback, InputDevice, InputType,
    StickStatus,
};
use common::param_package::ParamPackage;

/// Some games such as EARTH DEFENSE FORCE: WORLD BROTHERS
/// do not play nicely with the theoretical maximum range.
/// Using a value one lower from the maximum emulates real stick behavior.
pub const MAX_RANGE: f32 = 32766.0 / 32767.0;
pub const TAU: f32 = common::math_util::PI * 2.0;
/// Use wider angle to ease the transition.
pub const APERTURE: f32 = TAU * 0.15;

/// Default analog properties for stick from buttons.
const STICK_PROPERTIES: AnalogProperties = AnalogProperties {
    deadzone: 0.0,
    range: 1.0,
    threshold: 0.5,
    offset: 0.0,
    inverted: false,
    inverted_button: false,
    toggle: false,
};

/// Returns whether old_angle is greater than new_angle within aperture.
/// Port of Stick::IsAngleGreater
pub fn is_angle_greater(old_angle: f32, new_angle: f32) -> bool {
    let top_limit = new_angle + APERTURE;
    (old_angle > new_angle && old_angle <= top_limit)
        || (old_angle + TAU > new_angle && old_angle + TAU <= top_limit)
}

/// Returns whether old_angle is smaller than new_angle within aperture.
/// Port of Stick::IsAngleSmaller
pub fn is_angle_smaller(old_angle: f32, new_angle: f32) -> bool {
    let bottom_limit = new_angle - APERTURE;
    (old_angle >= bottom_limit && old_angle < new_angle)
        || (old_angle - TAU >= bottom_limit && old_angle - TAU < new_angle)
}

/// Port of the inner `Stick` class from stick_from_buttons.cpp.
///
/// Handles angle interpolation for smooth diagonal transitions.
struct Stick {
    up: Box<dyn InputDevice>,
    down: Box<dyn InputDevice>,
    left: Box<dyn InputDevice>,
    right: Box<dyn InputDevice>,
    modifier: Box<dyn InputDevice>,
    // Ownership keeps the updater callback registered; dropping the device
    // unregisters it from InputEngine.
    _updater: Box<dyn InputDevice>,
    state: Arc<Mutex<StickState>>,
}

type ChangeCallback = Arc<dyn Fn(&CallbackStatus) + Send + Sync>;
type PendingChange = Option<(ChangeCallback, CallbackStatus)>;

struct StickState {
    modifier_scale: f32,
    modifier_angle: f32,
    angle: f32,
    goal_angle: f32,
    amplitude: f32,
    up_status: bool,
    down_status: bool,
    left_status: bool,
    right_status: bool,
    last_x_axis_value: f32,
    last_y_axis_value: f32,
    modifier_status: ButtonStatus,
    last_update: Option<Instant>,
    callback: InputCallback,
}

impl StickState {
    fn new(modifier_scale: f32, modifier_angle: f32) -> Self {
        Self {
            modifier_scale,
            modifier_angle,
            angle: 0.0,
            goal_angle: 0.0,
            amplitude: 0.0,
            up_status: false,
            down_status: false,
            left_status: false,
            right_status: false,
            last_x_axis_value: 0.0,
            last_y_axis_value: 0.0,
            modifier_status: ButtonStatus::default(),
            last_update: None,
            callback: InputCallback { on_change: None },
        }
    }

    fn get_angle(&self, now: Instant) -> f32 {
        let mut new_angle = self.angle;

        let mut time_difference = self.last_update.map_or(0.5, |last_update| {
            now.duration_since(last_update).as_millis() as f32 / 1000.0
        });
        if time_difference > 0.5 {
            time_difference = 0.5;
        }

        if is_angle_greater(new_angle, self.goal_angle) {
            new_angle -= self.modifier_angle * time_difference;
            if new_angle < 0.0 {
                new_angle += TAU;
            }
            if !is_angle_greater(new_angle, self.goal_angle) {
                return self.goal_angle;
            }
        } else if is_angle_smaller(new_angle, self.goal_angle) {
            new_angle += self.modifier_angle * time_difference;
            if new_angle >= TAU {
                new_angle -= TAU;
            }
            if !is_angle_smaller(new_angle, self.goal_angle) {
                return self.goal_angle;
            }
        } else {
            return self.goal_angle;
        }
        new_angle
    }

    fn set_goal_angle(&mut self, r: bool, l: bool, u: bool, d: bool) {
        let pi = common::math_util::PI;
        if r && !u && !d {
            self.goal_angle = 0.0;
        }
        if r && u && !d {
            self.goal_angle = pi * 0.25;
        }
        if u && !l && !r {
            self.goal_angle = pi * 0.5;
        }
        if l && u && !d {
            self.goal_angle = pi * 0.75;
        }
        if l && !u && !d {
            self.goal_angle = pi;
        }
        if l && !u && d {
            self.goal_angle = pi * 1.25;
        }
        if d && !l && !r {
            self.goal_angle = pi * 1.5;
        }
        if r && !u && d {
            self.goal_angle = pi * 1.75;
        }
    }

    fn update_up_button_status(&mut self, callback: &CallbackStatus) -> PendingChange {
        self.up_status = callback.button_status.value;
        self.update_status()
    }

    fn update_down_button_status(&mut self, callback: &CallbackStatus) -> PendingChange {
        self.down_status = callback.button_status.value;
        self.update_status()
    }

    fn update_left_button_status(&mut self, callback: &CallbackStatus) -> PendingChange {
        self.left_status = callback.button_status.value;
        self.update_status()
    }

    fn update_right_button_status(&mut self, callback: &CallbackStatus) -> PendingChange {
        self.right_status = callback.button_status.value;
        self.update_status()
    }

    fn update_mod_button_status(&mut self, callback: &CallbackStatus) -> PendingChange {
        let new_status = callback.button_status;
        let new_button_value = if new_status.inverted {
            !new_status.value
        } else {
            new_status.value
        };
        self.modifier_status.toggle = new_status.toggle;

        if !self.modifier_status.toggle {
            self.modifier_status.locked = false;
            self.modifier_status.value = new_button_value;
        } else {
            if new_button_value && !self.modifier_status.locked {
                self.modifier_status.locked = true;
                self.modifier_status.value = !self.modifier_status.value;
            }
            if !new_button_value && self.modifier_status.locked {
                self.modifier_status.locked = false;
            }
        }
        self.update_status()
    }

    fn get_status(&self) -> StickStatus {
        let mut status = StickStatus::default();
        status.x.properties = STICK_PROPERTIES;
        status.y.properties = STICK_PROPERTIES;

        if *common::settings::values()
            .emulate_analog_keyboard
            .get_value()
        {
            let now = std::time::Instant::now();
            let angle = self.get_angle(now);
            status.x.raw_value = angle.cos() * self.amplitude;
            status.y.raw_value = angle.sin() * self.amplitude;
            return status;
        }

        status.x.raw_value = self.goal_angle.cos() * self.amplitude;
        status.y.raw_value = self.goal_angle.sin() * self.amplitude;
        status
    }

    fn update_status(&mut self) -> PendingChange {
        let mut r = self.right_status;
        let mut l = self.left_status;
        let mut u = self.up_status;
        let mut d = self.down_status;

        // Eliminate contradictory movements
        if r && l {
            r = false;
            l = false;
        }
        if u && d {
            u = false;
            d = false;
        }

        // Move if a key is pressed
        if r || l || u || d {
            self.amplitude = if self.modifier_status.value {
                self.modifier_scale
            } else {
                MAX_RANGE
            };
        } else {
            self.amplitude = 0.0;
        }

        let now = Instant::now();
        let time_difference = self.last_update.map_or(u128::MAX, |last_update| {
            now.duration_since(last_update).as_millis()
        });

        if time_difference < 10 {
            // Disable analog mode if inputs are too fast
            self.set_goal_angle(r, l, u, d);
            self.angle = self.goal_angle;
        } else {
            self.angle = self.get_angle(now);
            self.set_goal_angle(r, l, u, d);
        }

        self.last_update = Some(now);
        let stick_status = self.get_status();
        self.last_x_axis_value = stick_status.x.raw_value;
        self.last_y_axis_value = stick_status.y.raw_value;
        let status = CallbackStatus {
            input_type: InputType::Stick,
            stick_status,
            ..Default::default()
        };
        self.pending_change(status)
    }

    fn soft_update(&mut self) -> PendingChange {
        let status = CallbackStatus {
            input_type: InputType::Stick,
            stick_status: self.get_status(),
            ..Default::default()
        };
        if self.last_x_axis_value == status.stick_status.x.raw_value
            && self.last_y_axis_value == status.stick_status.y.raw_value
        {
            return None;
        }
        self.last_x_axis_value = status.stick_status.x.raw_value;
        self.last_y_axis_value = status.stick_status.y.raw_value;
        self.pending_change(status)
    }

    fn pending_change(&self, status: CallbackStatus) -> PendingChange {
        self.callback
            .on_change
            .as_ref()
            .map(|callback| (Arc::clone(callback), status))
    }
}

fn dispatch_pending(change: PendingChange) {
    if let Some((callback, status)) = change {
        callback(&status);
    }
}

impl Stick {
    fn new(
        mut up: Box<dyn InputDevice>,
        mut down: Box<dyn InputDevice>,
        mut left: Box<dyn InputDevice>,
        mut right: Box<dyn InputDevice>,
        mut modifier: Box<dyn InputDevice>,
        mut updater: Box<dyn InputDevice>,
        modifier_scale: f32,
        modifier_angle: f32,
    ) -> Self {
        let state = Arc::new(Mutex::new(StickState::new(modifier_scale, modifier_angle)));

        macro_rules! set_button_callback {
            ($device:ident, $method:ident) => {{
                let state = Arc::clone(&state);
                $device.set_callback(InputCallback {
                    on_change: Some(Arc::new(move |callback| {
                        let change = state.lock().unwrap().$method(callback);
                        dispatch_pending(change);
                    })),
                });
            }};
        }
        set_button_callback!(up, update_up_button_status);
        set_button_callback!(down, update_down_button_status);
        set_button_callback!(left, update_left_button_status);
        set_button_callback!(right, update_right_button_status);
        set_button_callback!(modifier, update_mod_button_status);
        {
            let state = Arc::clone(&state);
            updater.set_callback(InputCallback {
                on_change: Some(Arc::new(move |_| {
                    let change = state.lock().unwrap().soft_update();
                    dispatch_pending(change);
                })),
            });
        }

        Self {
            up,
            down,
            left,
            right,
            modifier,
            _updater: updater,
            state,
        }
    }
}

impl InputDevice for Stick {
    fn force_update(&mut self) {
        self.up.force_update();
        self.down.force_update();
        self.left.force_update();
        self.right.force_update();
        self.modifier.force_update();
    }

    fn set_callback(&mut self, callback: InputCallback) {
        self.state.lock().unwrap().callback = callback;
    }

    fn trigger_on_change(&self, status: &CallbackStatus) {
        let callback = self.state.lock().unwrap().callback.on_change.clone();
        if let Some(callback) = callback {
            callback(status);
        }
    }
}

/// Port of `StickFromButton` class from stick_from_buttons.h / stick_from_buttons.cpp
///
/// Creates an analog device from direction button devices.
/// Parameters:
///   - "up": serialized ParamPackage for creating a button device for up direction
///   - "down": serialized ParamPackage for creating a button device for down direction
///   - "left": serialized ParamPackage for creating a button device for left direction
///   - "right": serialized ParamPackage for creating a button device for right direction
///   - "modifier": serialized ParamPackage for creating a button device as the modifier
///   - "modifier_scale": a float for the multiplier the modifier gives to the position
pub struct StickFromButton;

impl StickFromButton {
    pub fn new() -> Self {
        Self
    }

    /// Port of StickFromButton::Create (override)
    ///
    /// Creates the inner Stick device with up/down/left/right/modifier buttons.
    /// The Stick device handles angle interpolation for smooth diagonal transitions,
    /// modifier for reduced-range movement, and callback-based input forwarding.
    pub fn create(&self, params: &ParamPackage) -> Box<dyn InputDevice> {
        let null_engine = {
            let mut p = ParamPackage::default();
            p.set_str("engine", "null".to_string());
            p.serialize()
        };
        let up = input::create_input_device_from_string(&params.get_str("up", &null_engine));
        let down = input::create_input_device_from_string(&params.get_str("down", &null_engine));
        let left = input::create_input_device_from_string(&params.get_str("left", &null_engine));
        let right = input::create_input_device_from_string(&params.get_str("right", &null_engine));
        let modifier =
            input::create_input_device_from_string(&params.get_str("modifier", &null_engine));
        let updater = input::create_input_device_from_string("engine:updater,button:0");
        let modifier_scale = params.get_float("modifier_scale", 0.5);
        let modifier_angle = params.get_float("modifier_angle", 5.5);

        Box::new(Stick::new(
            up,
            down,
            left,
            right,
            modifier,
            updater,
            modifier_scale,
            modifier_angle,
        ))
    }
}

impl Default for StickFromButton {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolation_uses_whole_milliseconds_and_caps_elapsed_time() {
        let start = Instant::now();
        let mut state = StickState::new(0.5, 0.2);
        state.angle = 0.0;
        state.goal_angle = common::math_util::PI * 0.25;
        state.last_update = Some(start);
        for (micros, expected) in [(999, 0.0), (1999, 0.0002), (500000, 0.1), (900000, 0.1)] {
            let angle = state.get_angle(start + std::time::Duration::from_micros(micros));
            assert!((angle - expected).abs() < 1e-7, "elapsed {micros}: {angle}");
        }
    }

    #[test]
    fn keyboard_analog_setting_selects_interpolated_or_direct_angle() {
        const CHILD: &str = "RUZU_TEST_KEYBOARD_ANALOG_SETTING";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "helpers::stick_from_buttons::tests::keyboard_analog_setting_selects_interpolated_or_direct_angle"])
                .env(CHILD, "1").status().unwrap();
            assert!(status.success());
            return;
        }
        let mut state = StickState::new(0.5, 0.2);
        state.angle = 0.0;
        state.goal_angle = common::math_util::PI * 0.25;
        state.amplitude = MAX_RANGE;
        // None selects the capped 0.5-second interval, without wall-clock sleeps.
        state.last_update = None;
        for enabled in [false, true, false] {
            common::settings::values_mut().emulate_analog_keyboard.set_value(enabled);
            let status = state.get_status();
            let angle = if enabled { 0.1 } else { state.goal_angle };
            assert!((status.x.raw_value - angle.cos() * MAX_RANGE).abs() < 1e-6);
            assert!((status.y.raw_value - angle.sin() * MAX_RANGE).abs() < 1e-6);
        }
    }
}

impl common::input::InputDeviceFactory for StickFromButton {
    fn create(&self, params: &ParamPackage) -> Box<dyn InputDevice> {
        StickFromButton::create(self, params)
    }
}
