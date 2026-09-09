// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of hid_core/resources/debug_pad/debug_pad.h and debug_pad.cpp

use super::debug_pad_types::{DebugPadAttribute, DebugPadState};
use crate::hid_types::{AnalogStickState, DebugPadButton};
use crate::resources::controller_base::ControllerActivation;
use crate::resources::shared_memory_format::DebugPadSharedMemoryFormat;

/// Stick state pair for left/right analog sticks.
#[derive(Debug, Clone, Copy, Default)]
pub struct StickState {
    pub left: AnalogStickState,
    pub right: AnalogStickState,
}

/// DebugPad controller — reads debug pad input from emulated controller (Other)
/// and writes into shared memory.
pub struct DebugPad {
    pub activation: ControllerActivation,
    next_state: DebugPadState,
}

impl DebugPad {
    pub fn new() -> Self {
        Self {
            activation: ControllerActivation::new(),
            next_state: DebugPadState::default(),
        }
    }

    pub fn on_init(&mut self) {}
    pub fn on_release(&mut self) {}

    /// Port of DebugPad::OnUpdate.
    ///
    /// Upstream logic:
    ///   lock shared_mutex
    ///   get active aruid -> AruidData
    ///   shared_memory = data->shared_memory_format->debug_pad
    ///   if not activated: clear lifo buffers, return
    ///   last_entry = debug_pad_lifo.ReadCurrentEntry().state
    ///   next_state.sampling_number = last_entry.sampling_number + 1
    ///   if debug_pad_enabled:
    ///       next_state.attribute.connected = 1
    ///       next_state.pad_state = controller->GetDebugPadButtons()
    ///       sticks = controller->GetSticks()
    ///       next_state.l_stick = sticks.left
    ///       next_state.r_stick = sticks.right
    ///   debug_pad_lifo.WriteNextEntry(next_state)
    pub fn on_update(
        &mut self,
        shared_memory: &mut DebugPadSharedMemoryFormat,
        button_state: &DebugPadButton,
        stick_state: &StickState,
    ) {
        if !self.activation.is_controller_activated() {
            shared_memory.debug_pad_lifo.buffer_count = 0;
            shared_memory.debug_pad_lifo.buffer_tail = 0;
            return;
        }

        let last_entry = shared_memory.debug_pad_lifo.read_current_entry();
        self.next_state.sampling_number = last_entry.state.sampling_number + 1;

        if *common::settings::values().debug_pad_enabled.get_value() {
            let mut attr = DebugPadAttribute::default();
            attr.set_connected(true);
            self.next_state.attribute = attr;

            self.next_state.pad_state = *button_state;
            self.next_state.l_stick = stick_state.left;
            self.next_state.r_stick = stick_state.right;
        }

        shared_memory
            .debug_pad_lifo
            .write_next_entry(self.next_state);
    }
}

impl Default for DebugPad {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_pad_setting_controls_sample_updates_like_upstream() {
        const CHILD: &str = "RUZU_TEST_DEBUG_PAD_SETTING";
        if std::env::var_os(CHILD).is_none() {
            assert!(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "resources::debug_pad::debug_pad::tests::debug_pad_setting_controls_sample_updates_like_upstream"])
                .env(CHILD, "1").status().unwrap().success());
            return;
        }
        let mut pad = DebugPad::new();
        let mut memory = DebugPadSharedMemoryFormat::default();
        let buttons = DebugPadButton { raw: 0x123 };
        let sticks = StickState {
            left: AnalogStickState { x: 123, y: -456 },
            right: AnalogStickState { x: -789, y: 321 },
        };
        common::settings::values_mut().debug_pad_enabled.set_value(false);
        pad.activation.activate();
        pad.on_update(&mut memory, &buttons, &sticks);
        let sample = memory.debug_pad_lifo.read_current_entry().state;
        assert_eq!(sample.sampling_number, 1);
        assert!(!sample.attribute.connected());
        assert_eq!(sample.pad_state.raw, 0);
        common::settings::values_mut().debug_pad_enabled.set_value(true);
        pad.on_update(&mut memory, &buttons, &sticks);
        let sample = memory.debug_pad_lifo.read_current_entry().state;
        assert_eq!(sample.sampling_number, 2);
        assert!(sample.attribute.connected());
        assert_eq!(sample.pad_state.raw, 0x123);
        assert_eq!((sample.l_stick.x, sample.l_stick.y), (123, -456));
        assert_eq!((sample.r_stick.x, sample.r_stick.y), (-789, 321));
        // Upstream keeps next_state when disabled; it does not synthesize
        // a disconnect or reset the existing buttons here.
        common::settings::values_mut().debug_pad_enabled.set_value(false);
        pad.on_update(&mut memory, &DebugPadButton::default(), &StickState::default());
        let sample = memory.debug_pad_lifo.read_current_entry().state;
        assert_eq!(sample.sampling_number, 3);
        assert_eq!(sample.pad_state.raw, 0x123);
        pad.activation.deactivate();
        pad.on_update(&mut memory, &buttons, &sticks);
        assert_eq!(memory.debug_pad_lifo.buffer_count, 0);
        assert_eq!(memory.debug_pad_lifo.buffer_tail, 0);
    }
}
