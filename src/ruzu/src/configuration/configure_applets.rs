// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of
// Eden `src/yuzu/configuration/configure_applets.cpp`
// (`ConfigureApplets`), whose widget tree lives in `configure_applets.ui`.
//
// A single "Applet mode preference" group: one combo per library applet,
// choosing between the emulator's own dialog ("Custom frontend", `AppletMode::HLE`)
// and the console's real applet ("Real applet", `AppletMode::LLE`).
//
// The row labels come from `shared_translation.cpp`'s `INSERT(Settings,
// <field>_applet_mode, ...)` entries. ConfigureApplets::Setup orders visible
// settings by registration ID, skipping its six hidden applets.

use gtk::prelude::*;

use common::settings::Values;
use common::settings_enums::AppletMode;

use super::configure_dialog::Page;
use super::shared_translation as tr;
use super::shared_widget as w;

/// The applets the dialog exposes, in upstream registration order, paired with
/// an accessor for the matching `Settings::Values` field.
///
/// Upstream exposes nine of the fifteen `*_applet_mode` settings; the rest
/// (`shop`, `login_share`, `wifi_web_auth`, `my_page`, `net_connect`,
/// `data_erase`) have hidden rows upstream. This page leaves their settings
/// untouched, including values loaded from the configuration file.
type Field = fn(&mut Values) -> &mut common::settings_common::SwitchableSetting<AppletMode>;

const APPLETS: &[(&str, Field)] = &[
    ("Amiibo editor", |v| &mut v.cabinet_applet_mode),
    ("Controller configuration", |v| {
        &mut v.controller_applet_mode
    }),
    ("Error", |v| &mut v.error_applet_mode),
    ("Player select", |v| &mut v.player_select_applet_mode),
    ("Software keyboard", |v| &mut v.swkbd_applet_mode),
    ("Mii Edit", |v| &mut v.mii_edit_applet_mode),
    ("Online web", |v| &mut v.web_applet_mode),
    ("Photo viewer", |v| &mut v.photo_viewer_applet_mode),
    ("Offline web", |v| &mut v.offline_web_applet_mode),
];

/// Build the Applets tab — upstream `ConfigureApplets`.
pub fn page(runtime_lock: bool) -> Page {
    let configuring_global = common::settings::is_configuring_global();
    let (scroller, column) = w::page();

    let (group, content) = w::group("Applet mode preference");

    let labels = tr::labels(tr::APPLET_MODE);
    let mut combos = Vec::with_capacity(APPLETS.len());

    for (label, field) in APPLETS {
        let (current, policy) = {
            // Copy the value and policy out of the settings guard
            // rather than holding the lock across widget construction.
            let mut values = common::settings::values_mut();
            let setting = field(&mut values);
            (
                *setting.get_value(),
                w::SettingEditPolicy::new(setting, runtime_lock, configuring_global),
            )
        };
        let (row, combo) = w::combo_row(label, &labels, tr::index_of(tr::APPLET_MODE, &current));
        row.set_sensitive(policy.sensitive);
        content.append(&row);
        combos.push((*field, combo, policy));
    }

    let enable_overlay = w::check_row(
        "Enable Overlay Applet",
        *common::settings::values().enable_overlay.get_value(),
    );
    let overlay_policy = w::SettingEditPolicy::new(
        &common::settings::values().enable_overlay,
        runtime_lock,
        configuring_global,
    );
    enable_overlay.set_sensitive(overlay_policy.sensitive);
    content.append(&enable_overlay);

    column.append(&group);

    Page::new("Applets", scroller, move || {
        let mut values = common::settings::values_mut();
        for (field, combo, policy) in &combos {
            let mode = tr::value_at(tr::APPLET_MODE, combo.selected());
            policy.apply(field(&mut values), mode);
        }
        overlay_policy.apply(&mut values.enable_overlay, enable_overlay.is_active());
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applet_preferences_are_startup_only_in_global_and_per_game_dialogs() {
        for configuring_global in [true, false] {
            let mut values = Values::default();
            for (_, field) in APPLETS {
                let setting = field(&mut values);
                setting.set_global(configuring_global);
                setting.set_value(AppletMode::HLE);
                let running = w::SettingEditPolicy::new(setting, false, configuring_global);
                assert!(!running.sensitive);
                running.apply(setting, AppletMode::LLE);
                assert_eq!(*setting.get_value(), AppletMode::HLE);
                let stopped = w::SettingEditPolicy::new(setting, true, configuring_global);
                assert!(stopped.sensitive);
                stopped.apply(setting, AppletMode::LLE);
                assert_eq!(*setting.get_value(), AppletMode::LLE);
            }
            let setting = &mut values.enable_overlay;
            setting.set_global(configuring_global);
            let running = w::SettingEditPolicy::new(setting, false, configuring_global);
            assert!(!running.sensitive);
            running.apply(setting, true);
            assert!(!*setting.get_value());
            let stopped = w::SettingEditPolicy::new(setting, true, configuring_global);
            assert!(stopped.sensitive);
            stopped.apply(setting, true);
            assert!(*setting.get_value());
        }
    }

    #[test]
    fn applet_rows_match_upstream_ui_order() {
        // ConfigureApplets::Setup retains this visible registration order.
        let labels: Vec<&str> = APPLETS.iter().map(|(label, _)| *label).collect();
        assert_eq!(
            labels,
            vec![
                "Amiibo editor",
                "Controller configuration",
                "Error",
                "Player select",
                "Software keyboard",
                "Mii Edit",
                "Online web",
                "Photo viewer",
                "Offline web",
            ]
        );
    }

    #[test]
    fn every_row_targets_a_distinct_setting() {
        let mut values = Values::default();
        // Stamp each field with a distinct mode, then verify each accessor
        // reads back what it wrote — catching a copy-paste duplicate.
        for (index, (_, field)) in APPLETS.iter().enumerate() {
            let mode = if index % 2 == 0 {
                AppletMode::HLE
            } else {
                AppletMode::LLE
            };
            field(&mut values).set_value(mode);
        }
        for (index, (_, field)) in APPLETS.iter().enumerate() {
            let expected = if index % 2 == 0 {
                AppletMode::HLE
            } else {
                AppletMode::LLE
            };
            assert_eq!(*field(&mut values).get_value(), expected);
        }
    }
}
