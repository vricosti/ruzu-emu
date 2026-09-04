// SPDX-FileCopyrightText: Copyright 2021 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `input_common/input_mapping.h` and `input_common/input_mapping.cpp`.
//!
//! Provides the MappingFactory that handles input mapping registration and configuration.

use std::collections::VecDeque;

use common::param_package::ParamPackage;

use crate::input_engine::{EngineInputType, MappingData};
use crate::main_common::polling;

/// Port of `MappingFactory` class from input_mapping.h / input_mapping.cpp
pub struct MappingFactory {
    /// FIFO counterpart of upstream's `Common::SPSCQueue`.
    input_queue: VecDeque<ParamPackage>,
    input_type: polling::InputType,
    is_enabled: bool,
    first_axis: i32,
    second_axis: i32,
}

impl MappingFactory {
    /// Port of MappingFactory::MappingFactory
    pub fn new() -> Self {
        Self {
            input_queue: VecDeque::new(),
            input_type: polling::InputType::None,
            is_enabled: false,
            first_axis: -1,
            second_axis: -1,
        }
    }

    /// Resets all variables to begin the mapping process.
    /// Port of MappingFactory::BeginMapping
    pub fn begin_mapping(&mut self, input_type: polling::InputType) {
        self.is_enabled = true;
        self.input_type = input_type;
        self.input_queue.clear();
        self.first_axis = -1;
        self.second_axis = -1;
    }

    /// Returns an input event with mapping information from the input_queue.
    /// Port of MappingFactory::GetNextInput
    pub fn get_next_input(&mut self) -> ParamPackage {
        self.input_queue.pop_front().unwrap_or_default()
    }

    /// Registers mapping input data from the driver.
    /// Port of MappingFactory::RegisterInput
    pub fn register_input(&mut self, data: &MappingData) {
        if !self.is_enabled {
            return;
        }
        if !self.is_driver_valid(data) {
            return;
        }

        match self.input_type {
            polling::InputType::Button => self.register_button(data),
            polling::InputType::Stick => self.register_stick(data),
            polling::InputType::Motion => self.register_motion(data),
            _ => {}
        }
    }

    /// Stop polling from all backends.
    /// Port of MappingFactory::StopMapping
    pub fn stop_mapping(&mut self) {
        self.is_enabled = false;
        self.input_type = polling::InputType::None;
        self.input_queue.clear();
    }

    /// Port of MappingFactory::RegisterButton
    fn register_button(&mut self, data: &MappingData) {
        let mut new_input = ParamPackage::default();
        new_input.set_str("engine", data.engine.clone());
        if data.pad.guid.is_valid() {
            new_input.set_str("guid", data.pad.guid.raw_string());
        }
        new_input.set_int("port", data.pad.port as i32);
        new_input.set_int("pad", data.pad.pad as i32);

        match data.r#type {
            EngineInputType::Button => {
                // Workaround for old compatibility
                if data.engine == "keyboard" {
                    new_input.set_int("code", data.index);
                } else {
                    new_input.set_int("button", data.index);
                }
            }
            EngineInputType::HatButton => {
                new_input.set_int("hat", data.index);
                new_input.set_str("direction", data.hat_name.clone());
            }
            EngineInputType::Analog => {
                // Ignore mouse axis when mapping buttons
                if data.engine == "mouse" && data.index != 4 {
                    return;
                }
                new_input.set_int("axis", data.index);
                new_input.set_float("threshold", 0.5);
            }
            EngineInputType::Motion => {
                new_input.set_int("motion", data.index);
            }
            _ => return,
        }
        self.input_queue.push_back(new_input);
    }

    /// Port of MappingFactory::RegisterStick
    fn register_stick(&mut self, data: &MappingData) {
        let mut new_input = ParamPackage::default();
        new_input.set_str("engine", data.engine.clone());
        if data.pad.guid.is_valid() {
            new_input.set_str("guid", data.pad.guid.raw_string());
        }
        new_input.set_int("port", data.pad.port as i32);
        new_input.set_int("pad", data.pad.pad as i32);

        // If engine is mouse map the mouse position as a joystick
        if data.engine == "mouse" {
            new_input.set_int("axis_x", 0);
            new_input.set_int("axis_y", 1);
            new_input.set_float("threshold", 0.5);
            new_input.set_float("range", 1.0);
            new_input.set_float("deadzone", 0.0);
            self.input_queue.push_back(new_input);
            return;
        }

        match data.r#type {
            EngineInputType::Button | EngineInputType::HatButton => {
                self.register_button(data);
                return;
            }
            EngineInputType::Analog => {
                if self.first_axis == data.index {
                    return;
                }
                if self.first_axis == -1 {
                    self.first_axis = data.index;
                    return;
                }
                new_input.set_int("axis_x", self.first_axis);
                new_input.set_int("axis_y", data.index);
                new_input.set_float("threshold", 0.5);
                new_input.set_float("range", 0.95);
                new_input.set_float("deadzone", 0.15);
            }
            _ => return,
        }
        self.input_queue.push_back(new_input);
    }

    /// Port of MappingFactory::RegisterMotion
    fn register_motion(&mut self, data: &MappingData) {
        let mut new_input = ParamPackage::default();
        new_input.set_str("engine", data.engine.clone());
        if data.pad.guid.is_valid() {
            new_input.set_str("guid", data.pad.guid.raw_string());
        }
        new_input.set_int("port", data.pad.port as i32);
        new_input.set_int("pad", data.pad.pad as i32);

        // If engine is mouse map it automatically to mouse motion
        if data.engine == "mouse" {
            new_input.set_int("motion", 0);
            new_input.set_int("pad", 1);
            new_input.set_float("threshold", 0.001);
            self.input_queue.push_back(new_input);
            return;
        }

        match data.r#type {
            EngineInputType::Button | EngineInputType::HatButton => {
                self.register_button(data);
                return;
            }
            EngineInputType::Analog => {
                if self.first_axis == data.index {
                    return;
                }
                if self.second_axis == data.index {
                    return;
                }
                if self.first_axis == -1 {
                    self.first_axis = data.index;
                    return;
                }
                if self.second_axis == -1 {
                    self.second_axis = data.index;
                    return;
                }
                new_input.set_int("axis_x", self.first_axis);
                new_input.set_int("axis_y", self.second_axis);
                new_input.set_int("axis_z", data.index);
                new_input.set_float("range", 1.0);
                new_input.set_float("deadzone", 0.20);
            }
            EngineInputType::Motion => {
                new_input.set_int("motion", data.index);
            }
            _ => return,
        }
        self.input_queue.push_back(new_input);
    }

    /// Port of MappingFactory::IsDriverValid
    fn is_driver_valid(&self, data: &MappingData) -> bool {
        // Only port 0 can be mapped on the keyboard
        if data.engine == "keyboard" && data.pad.port != 0 {
            return false;
        }
        // Only port 0 can be mapped on the mouse
        if data.engine == "mouse" && data.pad.port != 0 {
            return false;
        }
        // To prevent mapping with two devices we disable any UDP except motion.
        // Upstream: if (!Settings::values.enable_udp_controller && engine == "cemuhookudp"
        //               && type != Motion) return false;
        {
            let settings = common::settings::values();
            if !*settings.enable_udp_controller.get_value()
                && data.engine == "cemuhookudp"
                && data.r#type != crate::input_engine::EngineInputType::Motion
            {
                return false;
            }
        }
        // The following drivers don't need to be mapped
        if data.engine == "touch_from_button" {
            return false;
        }
        if data.engine == "analog_from_button" {
            return false;
        }
        if data.engine == "virtual_gamepad" {
            return false;
        }
        true
    }
}

impl Default for MappingFactory {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn button(engine: &str, index: i32) -> MappingData {
        MappingData {
            engine: engine.to_string(),
            r#type: EngineInputType::Button,
            index,
            button_value: true,
            ..MappingData::default()
        }
    }

    #[test]
    fn get_next_input_preserves_upstream_fifo_order() {
        let mut factory = MappingFactory::new();
        factory.begin_mapping(polling::InputType::Button);
        factory.register_input(&button("sdl", 1));
        factory.register_input(&button("keyboard", 2));

        let first = factory.get_next_input();
        let second = factory.get_next_input();
        assert_eq!(first.get_str("engine", ""), "sdl");
        assert_eq!(first.get_int("button", -1), 1);
        assert_eq!(second.get_str("engine", ""), "keyboard");
        assert_eq!(second.get_int("code", -1), 2);
    }
}
