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

/// Runs `f` on the locked controller and delivers every notification it
/// raised only after the lock is released.
///
/// Upstream fires `ControllerUpdateCallback`s inline from `Connect`,
/// `Disconnect`, `SetNpadStyleIndex`, `DisableConfiguration` and every
/// `ForceUpdate`, and a callback may call straight back into the controller
/// (`NfcDevice::NpadUpdate` does, through `HasNfc`/`AddNfcHandle`/
/// `RemoveNfcHandle`). Upstream has no lock to re-enter; the Rust owner is a
/// non-reentrant mutex, so notifications raised under it are queued by
/// `EmulatedController::run_deferred` and delivered here, in the order
/// upstream would have fired them, once the guard is gone.
///
/// Reach the owner through this for any call that can raise a notification,
/// one call per closure so the queued callbacks interleave with the caller's
/// own sequence the way upstream's inline ones do. A bare
/// `controller.lock().connect(..)` deadlocks the moment an NFC device is
/// registered on that controller - the Properties dialog hang after a game
/// had opened `nfp:user`.
pub fn with_controller<R>(
    controller: &EmulatedControllerHandle,
    f: impl FnOnce(&mut EmulatedController) -> R,
) -> R {
    let (result, callbacks) = controller.lock().run_deferred(f);
    for callback in callbacks {
        callback.dispatch();
    }
    result
}

/// Upstream `EmulatedController::ReloadFromSettings` for a shared owner: the
/// parameter/connection half, then `ReloadInput`, each delivering its
/// notifications before the next runs, as the inline upstream calls do.
pub fn reload_controller_from_settings(controller: &EmulatedControllerHandle) {
    with_controller(controller, |controller| {
        controller.reload_from_settings_before_input_reload()
    });
    with_controller(controller, |controller| controller.reload_input());
}

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
        for controller in [
            &self.player_1,
            &self.player_2,
            &self.player_3,
            &self.player_4,
            &self.player_5,
            &self.player_6,
            &self.player_7,
            &self.player_8,
            &self.other,
            &self.handheld,
        ] {
            with_controller(controller, |controller| controller.disable_configuration());
        }
    }

    pub fn reload_input_devices(&mut self) {
        reload_controller_from_settings(&self.player_1);
        reload_controller_from_settings(&self.player_2);
        reload_controller_from_settings(&self.player_3);
        reload_controller_from_settings(&self.player_4);
        reload_controller_from_settings(&self.player_5);
        reload_controller_from_settings(&self.player_6);
        reload_controller_from_settings(&self.player_7);
        reload_controller_from_settings(&self.player_8);
        reload_controller_from_settings(&self.other);
        reload_controller_from_settings(&self.handheld);
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

    /// A callback that re-enters the controller - as `NfcDevice::npad_update`
    /// does - must run after the owner is released, and every notification
    /// the closure raised must arrive, in the order upstream fires them.
    #[test]
    fn with_controller_delivers_reentrant_callbacks_after_releasing_the_owner() {
        use crate::frontend::emulated_controller::{
            ControllerTriggerType, ControllerUpdateCallback,
        };

        let hid_core = HIDCore::new();
        let controller = hid_core.get_emulated_controller_by_index(0);
        let delivered = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&delivered);
        let owner = Arc::downgrade(&controller);
        controller.lock().set_callback(ControllerUpdateCallback {
            on_change: Arc::new(move |trigger| {
                let connected = owner
                    .upgrade()
                    .map(|controller| controller.lock().is_connected(false));
                log.lock().push((trigger, connected));
            }),
            is_npad_service: false,
        });

        with_controller(&controller, |controller| {
            controller.set_npad_style_index(NpadStyleIndex::JoyconDual)
        });
        with_controller(&controller, |controller| controller.connect(false));
        with_controller(&controller, |controller| controller.disconnect());

        assert_eq!(
            *delivered.lock(),
            [
                (ControllerTriggerType::Type, Some(false)),
                (ControllerTriggerType::Connected, Some(true)),
                (ControllerTriggerType::Disconnected, Some(false)),
            ]
        );
    }
}
