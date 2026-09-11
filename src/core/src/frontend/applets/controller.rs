// SPDX-FileCopyrightText: Copyright 2020 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/frontend/applets/controller.h and controller.cpp
//! Controller configuration applet interface.

use std::sync::Arc;

use hid_core::hid_core::{HIDCore, AVAILABLE_CONTROLLERS};
use hid_core::hid_types::{NpadIdType, NpadStyleIndex};
use parking_lot::Mutex;

use super::applet::Applet;

/// Corresponds to upstream `BorderColor` (std::array<u8, 4>).
pub type BorderColor = [u8; 4];

/// Corresponds to upstream `ExplainText` (std::array<char, 0x81>).
pub type ExplainText = [u8; 0x81];

/// Parameters for the controller applet.
///
/// Corresponds to upstream `Core::Frontend::ControllerParameters`.
#[derive(Debug, Clone)]
pub struct ControllerParameters {
    pub min_players: i8,
    pub max_players: i8,
    pub keep_controllers_connected: bool,
    pub enable_single_mode: bool,
    pub enable_border_color: bool,
    pub border_colors: Vec<BorderColor>,
    pub enable_explain_text: bool,
    pub explain_text: Vec<ExplainText>,
    pub allow_pro_controller: bool,
    pub allow_handheld: bool,
    pub allow_dual_joycons: bool,
    pub allow_left_joycon: bool,
    pub allow_right_joycon: bool,
    pub allow_gamecube_controller: bool,
}

impl Default for ControllerParameters {
    fn default() -> Self {
        Self {
            min_players: 0,
            max_players: 0,
            keep_controllers_connected: false,
            enable_single_mode: false,
            enable_border_color: false,
            border_colors: Vec::new(),
            enable_explain_text: false,
            explain_text: Vec::new(),
            allow_pro_controller: false,
            allow_handheld: false,
            allow_dual_joycons: false,
            allow_left_joycon: false,
            allow_right_joycon: false,
            allow_gamecube_controller: false,
        }
    }
}

/// Callback type for controller reconfiguration results.
///
/// Corresponds to upstream `ControllerApplet::ReconfigureCallback`.
pub type ReconfigureCallback = Box<dyn FnOnce(bool) + Send>;

/// Controller applet trait.
///
/// Corresponds to upstream `Core::Frontend::ControllerApplet`.
pub trait ControllerApplet: Applet {
    fn reconfigure_controllers(
        &self,
        callback: ReconfigureCallback,
        parameters: &ControllerParameters,
    );
}

/// Corresponds to upstream `Core::Frontend::DefaultControllerApplet`.
#[derive(Clone)]
pub struct DefaultControllerApplet {
    hid_core: Arc<Mutex<HIDCore>>,
}

impl DefaultControllerApplet {
    pub fn new(hid_core: Arc<Mutex<HIDCore>>) -> Self {
        Self { hid_core }
    }
}

impl Applet for DefaultControllerApplet {
    fn close(&self) {}
}

impl ControllerApplet for DefaultControllerApplet {
    fn reconfigure_controllers(
        &self,
        callback: ReconfigureCallback,
        parameters: &ControllerParameters,
    ) {
        log::info!("called, deducing the best configuration based on the given parameters!");

        let min_supported_players = if parameters.enable_single_mode {
            1
        } else {
            parameters.min_players as usize
        };

        let (handheld, controllers) = {
            let hid_core = self.hid_core.lock();
            let handheld = hid_core.get_emulated_controller(NpadIdType::Handheld);
            let controllers = (0..AVAILABLE_CONTROLLERS - 2)
                .map(|index| hid_core.get_emulated_controller_by_index(index))
                .collect::<Vec<_>>();
            (handheld, controllers)
        };

        use hid_core::hid_core::with_controller;
        with_controller(&handheld, |handheld| handheld.disconnect());

        for (index, controller) in controllers.into_iter().enumerate() {
            with_controller(&controller, |controller| controller.disconnect());

            if index >= min_supported_players {
                continue;
            }

            let style = if parameters.allow_pro_controller {
                NpadStyleIndex::Fullkey
            } else if parameters.allow_dual_joycons {
                NpadStyleIndex::JoyconDual
            } else if parameters.allow_left_joycon && parameters.allow_right_joycon {
                if index % 2 == 0 {
                    NpadStyleIndex::JoyconLeft
                } else {
                    NpadStyleIndex::JoyconRight
                }
            } else if index == 0
                && parameters.enable_single_mode
                && parameters.allow_handheld
                && !common::settings::is_docked_mode(&common::settings::values())
            {
                NpadStyleIndex::Handheld
            } else {
                panic!("Unable to add a new controller based on the given parameters");
            };
            with_controller(&controller, |controller| controller.set_npad_style_index(style));
            with_controller(&controller, |controller| controller.connect(true));
        }

        callback(true);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use hid_core::hid_types::NpadStyleIndex;

    use super::*;

    #[test]
    fn default_applet_connects_minimum_players_as_fullkey() {
        let hid_core = Arc::new(Mutex::new(HIDCore::new()));
        let applet = DefaultControllerApplet::new(Arc::clone(&hid_core));
        let callback_called = Arc::new(AtomicBool::new(false));
        let callback_called_copy = Arc::clone(&callback_called);
        let parameters = ControllerParameters {
            min_players: 1,
            max_players: 4,
            allow_pro_controller: true,
            ..ControllerParameters::default()
        };

        applet.reconfigure_controllers(
            Box::new(move |success| {
                assert!(success);
                callback_called_copy.store(true, Ordering::Relaxed);
            }),
            &parameters,
        );

        assert!(callback_called.load(Ordering::Relaxed));
        let player_1 = hid_core.lock().get_emulated_controller(NpadIdType::Player1);
        let player_2 = hid_core.lock().get_emulated_controller(NpadIdType::Player2);
        assert!(player_1.lock().is_connected(false));
        assert_eq!(
            player_1.lock().get_npad_style_index(false),
            NpadStyleIndex::Fullkey
        );
        assert!(!player_2.lock().is_connected(false));
    }
}
