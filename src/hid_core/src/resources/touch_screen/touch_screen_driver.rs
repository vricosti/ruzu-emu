// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of hid_core/resources/touch_screen/touch_screen_driver.h and touch_screen_driver.cpp
//!
//! The touch screen driver handles reading raw touch input from the emulated
//! console and converting it into TouchState entries.

use std::sync::Arc;

use common::ResultCode;
use parking_lot::Mutex;

use super::touch_types::*;
use crate::hid_core::HIDCore;
use crate::hid_types::{TouchAttribute, TouchScreenModeForNx};

/// Maximum number of fingers tracked by the touch driver.
/// Matches upstream `Core::HID::TouchFingerState` array size.
const MAX_TOUCH_FINGERS: usize = MAX_FINGERS;

/// Per-finger tracking state used internally by the driver.
#[derive(Debug, Clone, Copy, Default)]
struct TouchFinger {
    id: u32,
    pressed: bool,
    attribute: TouchAttribute,
    position_x: f32,
    position_y: f32,
}

/// This handles all requests to Ftm3bd56(TouchPanel) hardware.
/// Port of upstream `TouchDriver`.
pub struct TouchScreenDriver {
    hid_core: Arc<Mutex<HIDCore>>,
    is_running: bool,
    touch_status: TouchScreenState,
    fingers: [TouchFinger; MAX_TOUCH_FINGERS],
    touch_mode: TouchScreenModeForNx,
}

impl TouchScreenDriver {
    pub fn new(hid_core: Arc<Mutex<HIDCore>>) -> Self {
        Self {
            hid_core,
            is_running: false,
            touch_status: TouchScreenState::default(),
            fingers: [TouchFinger::default(); MAX_TOUCH_FINGERS],
            touch_mode: TouchScreenModeForNx::UseSystemSetting,
        }
    }

    /// Port of TouchDriver::StartTouchSensor.
    pub fn start_touch_sensor(&mut self) -> ResultCode {
        self.is_running = true;
        ResultCode::SUCCESS
    }

    /// Port of TouchDriver::StopTouchSensor.
    pub fn stop_touch_sensor(&mut self) -> ResultCode {
        self.is_running = false;
        ResultCode::SUCCESS
    }

    /// Port of TouchDriver::IsRunning.
    pub fn is_running(&self) -> bool {
        self.is_running
    }

    /// Port of TouchDriver::ProcessTouchScreenAutoTune.
    pub fn process_touch_screen_auto_tune(&self) {
        // Upstream TODO: not yet implemented in C++ upstream (empty body)
    }

    /// Port of TouchDriver::WaitForDummyInput.
    pub fn wait_for_dummy_input(&mut self) -> ResultCode {
        self.touch_status = TouchScreenState::default();
        ResultCode::SUCCESS
    }

    /// Port of TouchDriver::WaitForInput.
    pub fn wait_for_input(&mut self) -> ResultCode {
        let touch = self.hid_core.lock().get_emulated_console().get_touch();
        let touch_input = std::array::from_fn(|index| {
            let finger = touch[index];
            (
                finger.id,
                finger.pressed,
                finger.position_x,
                finger.position_y,
            )
        });
        self.process_touch_input(&touch_input);
        ResultCode::SUCCESS
    }

    /// Processes raw touch finger input from the emulated console.
    /// This corresponds to the body of upstream WaitForInput() that reads
    /// from EmulatedConsole::GetTouch() and processes finger state transitions.
    pub fn process_touch_input(
        &mut self,
        touch_input: &[(u32, bool, f32, f32); MAX_TOUCH_FINGERS],
    ) {
        self.touch_status = TouchScreenState::default();

        for id in 0..self.touch_status.states.len() {
            let (current_id, current_pressed, current_x, current_y) = touch_input[id];
            let finger = &mut self.fingers[id];
            finger.id = current_id;

            if finger.attribute.start_touch() {
                finger.attribute = TouchAttribute::default();
                continue;
            }

            if finger.attribute.end_touch() {
                finger.attribute = TouchAttribute::default();
                finger.pressed = false;
                continue;
            }

            if !finger.pressed && current_pressed {
                finger.attribute = TouchAttribute::default();
                finger.attribute.set_start_touch(true);
                finger.pressed = true;
                finger.position_x = current_x;
                finger.position_y = current_y;
                continue;
            }

            if finger.pressed && !current_pressed {
                finger.attribute = TouchAttribute::default();
                finger.attribute.set_end_touch(true);
                continue;
            }

            // Only update position if touch is not on a special frame
            finger.position_x = current_x;
            finger.position_y = current_y;
        }

        // Collect active (pressed) fingers
        let mut active_fingers = [TouchFinger::default(); MAX_TOUCH_FINGERS];
        let mut active_count = 0usize;
        for finger in &self.fingers {
            if finger.pressed {
                active_fingers[active_count] = *finger;
                active_count += 1;
            }
        }

        self.touch_status.entry_count = active_count as i32;
        let settings = common::settings::values();
        for id in 0..MAX_TOUCH_FINGERS {
            if id < active_count {
                let touch_entry = &mut self.touch_status.states[id];
                touch_entry.position_x =
                    (active_fingers[id].position_x * TOUCH_SENSOR_WIDTH as f32) as u32;
                touch_entry.position_y =
                    (active_fingers[id].position_y * TOUCH_SENSOR_HEIGHT as f32) as u32;
                touch_entry.diameter_x = settings.touchscreen.diameter_x;
                touch_entry.diameter_y = settings.touchscreen.diameter_y;
                touch_entry.rotation_angle = settings.touchscreen.rotation_angle as i32;
                touch_entry.finger = active_fingers[id].id;
                touch_entry.attribute = active_fingers[id].attribute;
            }
        }
    }

    /// Port of TouchDriver::GetNextTouchState.
    pub fn get_next_touch_state(&self, out_state: &mut TouchScreenState) {
        *out_state = self.touch_status;
    }

    /// Port of TouchDriver::SetTouchMode.
    pub fn set_touch_mode(&mut self, mode: TouchScreenModeForNx) {
        self.touch_mode = mode;
    }

    /// Port of TouchDriver::GetTouchMode.
    pub fn get_touch_mode(&self) -> TouchScreenModeForNx {
        self.touch_mode
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_samples_follow_current_geometry_settings() {
        // Isolate process-global settings from concurrent HID tests.
        const CHILD: &str = "RUZU_TEST_TOUCH_GEOMETRY_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "resources::touch_screen::touch_screen_driver::tests::touch_samples_follow_current_geometry_settings", "--nocapture"])
                .env(CHILD, "1")
                .status().unwrap();
            assert!(status.success());
            return;
        }
        let hid = Arc::new(Mutex::new(HIDCore::new()));
        let mut driver = TouchScreenDriver::new(hid);
        let mut input = [(0, false, 0.0, 0.0); MAX_TOUCH_FINGERS];
        input[0] = (3, true, 0.25, 0.5);
        input[1] = (7, true, 0.5, 0.25);
        for (x, y, angle) in [(70, 80, 90), (0, 99, u32::MAX)] {
            {
                let mut settings = common::settings::values_mut();
                settings.touchscreen.diameter_x = x;
                settings.touchscreen.diameter_y = y;
                settings.touchscreen.rotation_angle = angle;
            }
            driver.process_touch_input(&input);
            let mut state = TouchScreenState::default();
            driver.get_next_touch_state(&mut state);
            assert_eq!(state.entry_count, 2);
            for entry in &state.states[..2] {
                assert_eq!(entry.diameter_x, x);
                assert_eq!(entry.diameter_y, y);
                assert_eq!(entry.rotation_angle, angle as i32);
            }
            assert_eq!(state.states[0].finger, 3);
            assert_eq!(state.states[1].finger, 7);
            assert_eq!(state.states[2].diameter_x, 0);
        }
    }
}
