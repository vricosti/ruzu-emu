// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of hid_core/resources/keyboard/keyboard.h and keyboard.cpp

use super::keyboard_types::KeyboardState;
use crate::hid_types::{KeyboardAttribute, KeyboardKey, KeyboardModifier};
use crate::resources::controller_base::ControllerActivation;
use crate::resources::shared_memory_format::KeyboardSharedMemoryFormat;

/// Keyboard controller — reads keyboard input from emulated devices and writes
/// into shared memory.
pub struct Keyboard {
    pub activation: ControllerActivation,
    next_state: KeyboardState,
}

impl Keyboard {
    pub fn new() -> Self {
        Self {
            activation: ControllerActivation::new(),
            next_state: KeyboardState::default(),
        }
    }

    pub fn on_init(&mut self) {}
    pub fn on_release(&mut self) {}

    /// Port of Keyboard::OnUpdate.
    ///
    /// Upstream logic:
    ///   lock shared_mutex
    ///   get active aruid -> AruidData
    ///   shared_memory = data->shared_memory_format->keyboard
    ///   if not activated: clear lifo buffers, return
    ///   last_entry = keyboard_lifo.ReadCurrentEntry().state
    ///   next_state.sampling_number = last_entry.sampling_number + 1
    ///   if keyboard_enabled:
    ///       next_state.key = emulated_devices->GetKeyboard()
    ///       next_state.modifier = emulated_devices->GetKeyboardModifier()
    ///       next_state.attribute.is_connected = 1
    ///   keyboard_lifo.WriteNextEntry(next_state)
    pub fn on_update(
        &mut self,
        shared_memory: &mut KeyboardSharedMemoryFormat,
        keyboard_state: &KeyboardKey,
        keyboard_modifier: &KeyboardModifier,
    ) {
        if !self.activation.is_controller_activated() {
            shared_memory.keyboard_lifo.buffer_count = 0;
            shared_memory.keyboard_lifo.buffer_tail = 0;
            return;
        }

        let last_entry = shared_memory.keyboard_lifo.read_current_entry();
        self.next_state.sampling_number = last_entry.state.sampling_number + 1;

        if *common::settings::values().keyboard_enabled.get_value() {
            self.next_state.key = *keyboard_state;
            self.next_state.modifier = *keyboard_modifier;
            // attribute.is_connected.Assign(1)
            self.next_state.attribute = KeyboardAttribute { raw: 1 };
        }

        shared_memory
            .keyboard_lifo
            .write_next_entry(self.next_state);
    }
}

impl Default for Keyboard {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enable_setting_controls_keyboard_publication() {
        const CHILD: &str = "RUZU_TEST_KEYBOARD_SETTING";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "resources::keyboard::keyboard::tests::enable_setting_controls_keyboard_publication"])
                .env(CHILD, "1").status().unwrap();
            assert!(status.success());
            return;
        }
        let mut device = Keyboard::new();
        let mut memory = KeyboardSharedMemoryFormat::default();
        let mut keys = KeyboardKey::default();
        keys.key[3] = 0x80;
        let modifier = KeyboardModifier { raw: 2 };
        device.activation.activate();
        common::settings::values_mut().keyboard_enabled.set_value(false);
        device.on_update(&mut memory, &keys, &modifier);
        let disabled = memory.keyboard_lifo.read_current_entry().state;
        assert_eq!(disabled.attribute.raw, 0);
        assert_eq!(disabled.key.key, [0; 32]);
        common::settings::values_mut().keyboard_enabled.set_value(true);
        device.on_update(&mut memory, &keys, &modifier);
        let enabled = memory.keyboard_lifo.read_current_entry().state;
        assert_eq!(enabled.attribute.raw, 1);
        assert_eq!(enabled.key.key, keys.key);
        assert_eq!(enabled.modifier.raw, 2);
        assert_eq!(enabled.sampling_number, disabled.sampling_number + 1);
        // Upstream retains next_state's keys/modifiers when disabled again.
        common::settings::values_mut().keyboard_enabled.set_value(false);
        device.on_update(&mut memory, &KeyboardKey::default(), &KeyboardModifier::default());
        let retained = memory.keyboard_lifo.read_current_entry().state;
        assert_eq!(retained.key.key, keys.key);
        assert_eq!(retained.modifier.raw, 2);
    }
}
