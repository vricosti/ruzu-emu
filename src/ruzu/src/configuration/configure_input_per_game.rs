// SPDX-FileCopyrightText: Copyright 2022 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! GTK counterpart of `yuzu/configuration/configure_input_per_game.{h,cpp,ui}`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gtk::prelude::*;

use super::configure_dialog::Page;
use super::input_profiles::InputProfiles;
use super::shared_widget as w;

const PLAYER_COUNT: usize = 8;
const HANDHELD_INDEX: usize = 8;

/// Build the eight per-player profile selectors in upstream order.
pub fn page(hid_core: Arc<parking_lot::Mutex<hid_core::hid_core::HIDCore>>) -> Page {
    let (scroller, column) = w::page();
    let (group, content) = w::group("Input Profiles");

    let profiles = Rc::new(RefCell::new(InputProfiles::new()));
    let profile_names = profiles.borrow_mut().get_input_profile_names();
    let mut labels = vec!["Use global input configuration".to_string()];
    labels.extend(profile_names.iter().cloned());
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();

    let current_profiles: Vec<String> = common::settings::values()
        .players
        .get_value()
        .iter()
        .take(PLAYER_COUNT)
        .map(|player| player.profile_name.clone())
        .collect();

    let mut selectors = Vec::with_capacity(PLAYER_COUNT);
    for index in 0..PLAYER_COUNT {
        let selected = current_profiles[index]
            .is_empty()
            .then_some(0)
            .or_else(|| {
                profile_names
                    .iter()
                    .position(|name| name == &current_profiles[index])
                    .map(|position| position as u32 + 1)
            })
            .unwrap_or(0);
        let (row, selector) = w::combo_row(
            &format!("Player {} profile", index + 1),
            &label_refs,
            selected,
        );
        content.append(&row);
        selectors.push(selector);
    }
    column.append(&group);

    let (controllers, handheld_controller) = {
        let hid_core = hid_core.lock();
        let controllers = (0..PLAYER_COUNT)
            .map(|index| hid_core.get_emulated_controller_by_index(index))
            .collect::<Vec<_>>();
        let handheld = hid_core.get_emulated_controller(hid_core::hid_types::NpadIdType::Handheld);
        (controllers, handheld)
    };

    Page::new("Input Profiles", scroller, move || {
        let mut profiles = profiles.borrow_mut();

        for (index, selector) in selectors.iter().enumerate() {
            let selected = selector.selected() as usize;
            if selected == 0 {
                {
                    let mut settings = common::settings::values_mut();
                    settings.players.set_global(false);
                    settings.players.get_value_mut()[index].profile_name.clear();
                    if index == 0 {
                        settings.players.get_value_mut()[HANDHELD_INDEX] = Default::default();
                    }
                    settings.players.set_global(true);
                }
                hid_core::hid_core::reload_controller_from_settings(&controllers[index]);
                continue;
            }
            let Some(profile_name) = profile_names.get(selected - 1) else {
                continue;
            };
            let loaded = {
                let mut settings = common::settings::values_mut();
                settings.players.set_global(false);
                let player = &mut settings.players.get_value_mut()[index];
                if !profiles.load_profile(profile_name, player) {
                    false
                } else {
                    player.profile_name = profile_name.clone();
                    player.connected = true;

                    if index == 0 {
                        let handheld = if player.controller_type
                            == common::settings_input::ControllerType::Handheld
                        {
                            player.clone()
                        } else {
                            Default::default()
                        };
                        settings.players.get_value_mut()[HANDHELD_INDEX] = handheld;
                    }
                    true
                }
            };
            if loaded {
                hid_core::hid_core::reload_controller_from_settings(&controllers[index]);
                if index == 0 {
                    hid_core::hid_core::reload_controller_from_settings(&handheld_controller);
                }
            }
        }

        // `ConfigureInputPerGame::SaveConfiguration` forces custom storage
        // before serializing, even when the final combo selects globals.
        common::settings::values_mut().players.set_global(false);
    })
}
