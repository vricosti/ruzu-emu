// SPDX-FileCopyrightText: Copyright 2021 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of hid_core/hid_core.h and hid_core/hid_core.cpp

use std::sync::Arc;

use parking_lot::Mutex;

use crate::frontend::emulated_console::EmulatedConsole;
use crate::frontend::emulated_controller::EmulatedController;
use crate::frontend::emulated_devices::EmulatedDevices;
use crate::hid_types::*;
use crate::hid_util;

/// Number of emulated controllers
pub const AVAILABLE_CONTROLLERS: usize = 10;

/// Stable shared ownership counterpart of upstream's
/// `std::unique_ptr<EmulatedController>`.
pub type EmulatedControllerHandle = Arc<Mutex<EmulatedController>>;

pub struct HIDCore {
    player_1: EmulatedControllerHandle,
    player_2: EmulatedControllerHandle,
    player_3: EmulatedControllerHandle,
    player_4: EmulatedControllerHandle,
    player_5: EmulatedControllerHandle,
    player_6: EmulatedControllerHandle,
    player_7: EmulatedControllerHandle,
    player_8: EmulatedControllerHandle,
    other: EmulatedControllerHandle,
    handheld: EmulatedControllerHandle,
    console: Box<EmulatedConsole>,
    devices: Box<EmulatedDevices>,
    supported_style_tag: NpadStyleTag,
    last_active_controller: NpadIdType,
}

impl HIDCore {
    pub fn new() -> Self {
        Self {
            player_1: Arc::new(Mutex::new(EmulatedController::new(NpadIdType::Player1))),
            player_2: Arc::new(Mutex::new(EmulatedController::new(NpadIdType::Player2))),
            player_3: Arc::new(Mutex::new(EmulatedController::new(NpadIdType::Player3))),
            player_4: Arc::new(Mutex::new(EmulatedController::new(NpadIdType::Player4))),
            player_5: Arc::new(Mutex::new(EmulatedController::new(NpadIdType::Player5))),
            player_6: Arc::new(Mutex::new(EmulatedController::new(NpadIdType::Player6))),
            player_7: Arc::new(Mutex::new(EmulatedController::new(NpadIdType::Player7))),
            player_8: Arc::new(Mutex::new(EmulatedController::new(NpadIdType::Player8))),
            other: Arc::new(Mutex::new(EmulatedController::new(NpadIdType::Other))),
            handheld: Arc::new(Mutex::new(EmulatedController::new(NpadIdType::Handheld))),
            console: Box::new(EmulatedConsole::new()),
            devices: Box::new(EmulatedDevices::new()),
            supported_style_tag: NpadStyleTag {
                raw: NpadStyleSet::ALL,
            },
            last_active_controller: NpadIdType::Handheld,
        }
    }

    pub fn get_emulated_controller(&self, npad_id_type: NpadIdType) -> EmulatedControllerHandle {
        match npad_id_type {
            NpadIdType::Player1 => Arc::clone(&self.player_1),
            NpadIdType::Player2 => Arc::clone(&self.player_2),
            NpadIdType::Player3 => Arc::clone(&self.player_3),
            NpadIdType::Player4 => Arc::clone(&self.player_4),
            NpadIdType::Player5 => Arc::clone(&self.player_5),
            NpadIdType::Player6 => Arc::clone(&self.player_6),
            NpadIdType::Player7 => Arc::clone(&self.player_7),
            NpadIdType::Player8 => Arc::clone(&self.player_8),
            NpadIdType::Other => Arc::clone(&self.other),
            NpadIdType::Handheld => Arc::clone(&self.handheld),
            _ => panic!("Invalid NpadIdType={:?}", npad_id_type),
        }
    }

    pub fn get_emulated_controller_by_index(&self, index: usize) -> EmulatedControllerHandle {
        self.get_emulated_controller(hid_util::index_to_npad_id_type(index))
    }

    pub fn get_emulated_console(&self) -> &EmulatedConsole {
        &self.console
    }

    pub fn get_emulated_console_mut(&mut self) -> &mut EmulatedConsole {
        &mut self.console
    }

    pub fn get_emulated_devices(&self) -> &EmulatedDevices {
        &self.devices
    }

    pub fn get_emulated_devices_mut(&mut self) -> &mut EmulatedDevices {
        &mut self.devices
    }

    pub fn set_supported_style_tag(&mut self, style_tag: NpadStyleTag) {
        self.supported_style_tag.raw = style_tag.raw;
        self.player_1
            .lock()
            .set_supported_npad_style_tag(self.supported_style_tag);
        self.player_2
            .lock()
            .set_supported_npad_style_tag(self.supported_style_tag);
        self.player_3
            .lock()
            .set_supported_npad_style_tag(self.supported_style_tag);
        self.player_4
            .lock()
            .set_supported_npad_style_tag(self.supported_style_tag);
        self.player_5
            .lock()
            .set_supported_npad_style_tag(self.supported_style_tag);
        self.player_6
            .lock()
            .set_supported_npad_style_tag(self.supported_style_tag);
        self.player_7
            .lock()
            .set_supported_npad_style_tag(self.supported_style_tag);
        self.player_8
            .lock()
            .set_supported_npad_style_tag(self.supported_style_tag);
        self.other
            .lock()
            .set_supported_npad_style_tag(self.supported_style_tag);
        self.handheld
            .lock()
            .set_supported_npad_style_tag(self.supported_style_tag);
    }

    pub fn get_supported_style_tag(&self) -> NpadStyleTag {
        self.supported_style_tag
    }

    /// Counts the connected players from P1-P8
    pub fn get_player_count(&self) -> i8 {
        let mut active_players: i8 = 0;
        for player_index in 0..(AVAILABLE_CONTROLLERS - 2) {
            let controller = self.get_emulated_controller_by_index(player_index);
            if controller.lock().is_connected(false) {
                active_players += 1;
            }
        }
        active_players
    }

    /// Returns the first connected npad id
    pub fn get_first_npad_id(&self) -> NpadIdType {
        for player_index in 0..AVAILABLE_CONTROLLERS {
            let controller = self.get_emulated_controller_by_index(player_index);
            let controller = controller.lock();
            if controller.is_connected(false) {
                return controller.get_npad_id_type();
            }
        }
        NpadIdType::Player1
    }

    /// Returns the first disconnected npad id
    pub fn get_first_disconnected_npad_id(&self) -> NpadIdType {
        for player_index in 0..AVAILABLE_CONTROLLERS {
            let controller = self.get_emulated_controller_by_index(player_index);
            let controller = controller.lock();
            if !controller.is_connected(false) {
                return controller.get_npad_id_type();
            }
        }
        NpadIdType::Player1
    }

    pub fn set_last_active_controller(&mut self, npad_id: NpadIdType) {
        self.last_active_controller = npad_id;
    }

    pub fn get_last_active_controller(&self) -> NpadIdType {
        self.last_active_controller
    }

    pub fn enable_all_controller_configuration(&mut self) {
        self.player_1.lock().enable_configuration();
        self.player_2.lock().enable_configuration();
        self.player_3.lock().enable_configuration();
        self.player_4.lock().enable_configuration();
        self.player_5.lock().enable_configuration();
        self.player_6.lock().enable_configuration();
        self.player_7.lock().enable_configuration();
        self.player_8.lock().enable_configuration();
        self.other.lock().enable_configuration();
        self.handheld.lock().enable_configuration();
    }

    pub fn disable_all_controller_configuration(&mut self) {
        self.player_1.lock().disable_configuration();
        self.player_2.lock().disable_configuration();
        self.player_3.lock().disable_configuration();
        self.player_4.lock().disable_configuration();
        self.player_5.lock().disable_configuration();
        self.player_6.lock().disable_configuration();
        self.player_7.lock().disable_configuration();
        self.player_8.lock().disable_configuration();
        self.other.lock().disable_configuration();
        self.handheld.lock().disable_configuration();
    }

    pub fn reload_input_devices(&mut self) {
        fn reload(controller: &EmulatedControllerHandle) {
            let callbacks = controller.lock().reload_from_settings_deferred();
            for callback in callbacks {
                callback.dispatch();
            }
            controller.lock().reload_input();
        }

        reload(&self.player_1);
        reload(&self.player_2);
        reload(&self.player_3);
        reload(&self.player_4);
        reload(&self.player_5);
        reload(&self.player_6);
        reload(&self.player_7);
        reload(&self.player_8);
        reload(&self.other);
        reload(&self.handheld);
        self.console.reload_from_settings();
        self.devices.reload_from_settings();
    }

    pub fn unload_input_devices(&mut self) {
        self.player_1.lock().unload_input();
        self.player_2.lock().unload_input();
        self.player_3.lock().unload_input();
        self.player_4.lock().unload_input();
        self.player_5.lock().unload_input();
        self.player_6.lock().unload_input();
        self.player_7.lock().unload_input();
        self.player_8.lock().unload_input();
        self.other.lock().unload_input();
        self.handheld.lock().unload_input();
        self.console.unload_input();
        self.devices.unload_input();
    }
}

impl Default for HIDCore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_getters_return_the_hid_core_owned_instance() {
        let hid_core = HIDCore::new();
        let by_id = hid_core.get_emulated_controller(NpadIdType::Player1);
        let by_index = hid_core.get_emulated_controller_by_index(0);

        assert!(Arc::ptr_eq(&by_id, &by_index));
    }
}
