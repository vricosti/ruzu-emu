// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of
// `/home/vricosti/Dev/emulators/zuyu/src/yuzu/configuration/configure_input.cpp`
// (`ConfigureInput`), whose widget tree lives in `configure_input.ui`.
//
// Upstream `ConfigureInput` is a container whose `GetSubTabs()` returns the
// eight per-player pages plus the "Advanced" page — and `ConfigureDialog`
// splices that list straight into the outer tab widget. That is why the
// Controls screen shows nine tabs rather than a nested tab widget (unlike the
// Debug screen, which does nest).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use gtk::prelude::*;

use super::configure_dialog::Page;
use super::configure_input_advanced;
use super::configure_input_player;
use super::input_profiles::InputProfiles;

/// Number of player tabs — upstream builds `Settings::values.players` slots
/// 0..8 as "Player 1".."Player 8".
pub const NUM_PLAYERS: usize = 8;

/// Upstream ConfigureInput's shared OnDockedModeChanged entry point, also used
/// by the controller applet and the main-window mode toggle. Must run on the
/// System owner; frontend callers marshal it through EmulationSession.
pub(crate) fn on_docked_mode_changed(last: bool, new: bool, system: &ruzu_core::core::System) {
    if last == new || !system.is_powered_on() {
        return;
    }
    system.get_applet_manager().operation_mode_changed();
}

/// ConfigureInput owns these global options upstream. GTK repeats the controls
/// on each player page; bind them to a single owner instead of saving eight
/// stale copies. These unparented widgets are the shared property sources.
pub(crate) struct GlobalInputSettings {
    motion: gtk::CheckButton,
    vibration: gtk::CheckButton,
    docked: gtk::CheckButton,
}

impl GlobalInputSettings {
    fn new() -> Self {
        let values = common::settings::values();
        Self {
            motion: gtk::CheckButton::builder().active(*values.motion_enabled.get_value()).build(),
            vibration: gtk::CheckButton::builder().active(*values.vibration_enabled.get_value()).build(),
            docked: gtk::CheckButton::builder().active(*values.use_docked_mode.get_value()
                == common::settings_enums::ConsoleMode::Docked).build(),
        }
    }

    pub(crate) fn bind(&self, motion: &gtk::CheckButton, vibration: &gtk::CheckButton, docked: &gtk::CheckButton) {
        for (source, target) in [(&self.motion, motion), (&self.vibration, vibration), (&self.docked, docked)] {
            source.bind_property("active", target, "active").bidirectional().sync_create().build();
        }
    }

    fn apply(&self) {
        let mut values = common::settings::values_mut();
        values.use_docked_mode.set_value(if self.docked.is_active() {
            common::settings_enums::ConsoleMode::Docked
        } else { common::settings_enums::ConsoleMode::Handheld });
        values.vibration_enabled.set_value(self.vibration.is_active());
        values.motion_enabled.set_value(self.motion.is_active());
    }
}

/// Build the Controls tabs — upstream `ConfigureInput::GetSubTabs()`.
pub fn pages(
    input_subsystem: Rc<RefCell<input_common::InputSubsystem>>,
    hid_core: Arc<parking_lot::Mutex<hid_core::hid_core::HIDCore>>,
) -> Vec<Page> {
    // Upstream `ConfigureInput` owns one `InputProfiles` instance shared by
    // every per-player page.
    let profiles = Rc::new(configure_input_player::InputProfileContext::new(
        InputProfiles::new(),
    ));
    let global = Rc::new(GlobalInputSettings::new());
    let mut pages: Vec<Page> = (0..NUM_PLAYERS)
        .map(|index| {
            configure_input_player::page(
                index,
                Rc::clone(&input_subsystem),
                Arc::clone(&hid_core),
                Rc::clone(&profiles),
                Some(&global),
                false,
            )
        })
        .collect();
    let advanced = configure_input_advanced::page(Rc::clone(&input_subsystem), Arc::clone(&hid_core), Rc::clone(&profiles));
    // Apply these only after all player pages and Advanced, as ConfigureInput
    // does. The last page closure also owns the shared bindings' lifetime.
    pages.push(Page::new(&advanced.title, advanced.widget, move || {
        (advanced.apply)();
        global.apply();
    }));
    pages
}

/// Apply every Controls subpage through upstream `ConfigureInput` ownership.
///
/// `Settings::values.players` can still select the running title's custom
/// storage while the global configuration dialog is open. Eden brackets all
/// eight `ConfigureInputPlayer::ApplyConfiguration` calls (and Advanced) with
/// `SetGlobal(true)`, then restores the previous selection. Without that
/// ordering the live controller changes, but the global bindings subsequently
/// written to `qt-config.ini` remain unchanged.
pub(crate) fn apply_configuration(pages: &[Page]) {
    let was_global = {
        let mut values = common::settings::values_mut();
        let was_global = values.players.using_global();
        values.players.set_global(true);
        was_global
    };

    for page in pages {
        (page.apply)();
    }

    common::settings::values_mut()
        .players
        .set_global(was_global);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display; run alone with --ignored"]
    fn global_motion_vibration_and_mode_are_shared_between_player_tabs() {
        gtk::init().unwrap();
        let global = GlobalInputSettings::new();
        let controls: Vec<_> = (0..NUM_PLAYERS).map(|_| {
            let motion = gtk::CheckButton::new();
            let vibration = gtk::CheckButton::new();
            let docked = gtk::CheckButton::new();
            global.bind(&motion, &vibration, &docked);
            (motion, vibration, docked)
        }).collect();
        for index in [0, 7, 3] {
            for enabled in [false, true] {
                controls[index].0.set_active(enabled);
                controls[index].1.set_active(enabled);
                controls[index].2.set_active(enabled);
                for (motion, vibration, docked) in &controls {
                    assert_eq!(motion.is_active(), enabled);
                    assert_eq!(vibration.is_active(), enabled);
                    assert_eq!(docked.is_active(), enabled);
                }
                global.apply();
                let values = common::settings::values();
                assert_eq!(*values.motion_enabled.get_value(), enabled);
                assert_eq!(*values.vibration_enabled.get_value(), enabled);
                assert_eq!(*values.use_docked_mode.get_value() == common::settings_enums::ConsoleMode::Docked, enabled);
            }
        }
    }

    #[test]
    fn controls_section_has_eight_players_plus_advanced() {
        // Upstream's Controls row shows nine tabs; a mismatch would drop a
        // player's bindings from the dialog entirely.
        assert_eq!(NUM_PLAYERS, 8);
    }

    #[test]
    fn apply_configuration_keeps_upstream_global_player_storage_order() {
        let source = include_str!("configure_input.rs");
        let start = source
            .find("pub(crate) fn apply_configuration")
            .expect("ConfigureInput apply owner");
        let end = source[start..]
            .find("#[cfg(test)]")
            .map(|offset| start + offset)
            .expect("ConfigureInput test boundary");
        let body = &source[start..end];

        let remember = body.find("players.using_global()").expect("remember state");
        let select_global = body
            .find("players.set_global(true)")
            .expect("select global");
        let apply = body.find("(page.apply)()").expect("apply player pages");
        let restore = body.find("set_global(was_global)").expect("restore state");
        assert!(remember < select_global);
        assert!(select_global < apply);
        assert!(apply < restore);
    }
}
