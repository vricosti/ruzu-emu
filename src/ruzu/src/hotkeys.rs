// SPDX-License-Identifier: GPL-3.0-or-later
//
// GTK counterpart of `/home/vricosti/Dev/emulators/eden/src/yuzu/hotkeys.cpp`.
// The registry data is owned by `uisettings`; this module connects the keyboard
// half to GTK application actions and matches window-owned emulation hotkeys.

use gtk::prelude::*;

pub fn matches(action: &str, keyval: gtk::gdk::Key, state: gtk::gdk::ModifierType) -> bool {
    let sequence = crate::uisettings::with(|values| {
        values
            .shortcuts
            .iter()
            .find(|shortcut| shortcut.name == action)
            .map(|shortcut| shortcut.keyseq.clone())
    });
    let Some(accelerator) = sequence.and_then(|value| gtk_accelerator_from_native(&value)) else {
        return false;
    };
    let Some((expected_key, expected_modifiers)) = gtk::accelerator_parse(&accelerator) else {
        return false;
    };
    let modifier_mask = gtk::gdk::ModifierType::SHIFT_MASK
        | gtk::gdk::ModifierType::CONTROL_MASK
        | gtk::gdk::ModifierType::ALT_MASK
        | gtk::gdk::ModifierType::SUPER_MASK
        | gtk::gdk::ModifierType::META_MASK;
    expected_key == keyval && expected_modifiers == state & modifier_mask
}

/// Apply the subset of upstream keyboard hotkeys whose GTK actions are already
/// ported. Reapplying disconnects the former accelerator exactly as
/// `HotkeyRegistry::LoadHotkeys` updates an existing `QShortcut`.
pub fn apply_accelerators(app: &gtk::Application) {
    for (hotkey, action) in [
        ("Continue/Pause Emulation", "app.pause"),
        ("Stop Emulation", "app.stop"),
        ("Restart Emulation", "app.restart"),
        ("Fullscreen", "app.fullscreen"),
        ("Exit Fullscreen", "app.exit_fullscreen"),
        ("Load File", "app.load_file"),
        ("Configure", "app.configure"),
        ("Configure Current Game", "app.configure_current_game"),
        ("Change Adapting Filter", "app.toggle_adapting_filter"),
        ("Change GPU Mode", "app.toggle_gpu_accuracy"),
        ("Change Docked Mode", "app.toggle_docked_mode"),
        ("Audio Mute/Unmute", "app.audio_mute"),
        ("Audio Volume Down", "app.audio_volume_down"),
        ("Audio Volume Up", "app.audio_volume_up"),
        ("Capture Screenshot", "app.capture_screenshot"),
        ("Load/Remove Amiibo", "app.load_amiibo"),
        ("TAS Start/Stop", "app.tas_start"),
        ("TAS Record", "app.tas_record"),
        ("TAS Reset", "app.tas_reset"),
        ("Browse Public Game Lobby", "app.view_lobby"),
        ("Direct Connect to Room", "app.connect_to_room"),
        ("Show Current Room", "app.show_room"),
        ("Leave Room", "app.leave_room"),
        ("Toggle Filter Bar", "app.show_filter_bar"),
        ("Toggle Status Bar", "app.show_status_bar"),
        ("Toggle Renderdoc Capture", "app.renderdoc_capture"),
        ("Toggle Framerate Limit", "app.toggle_framerate_limit"),
        ("Toggle Turbo Speed", "app.toggle_turbo_speed"),
        ("Toggle Slow Speed", "app.toggle_slow_speed"),
        ("Exit ruzu", "app.quit"),
    ] {
        let accelerator = crate::uisettings::with(|values| {
            values
                .shortcuts
                .iter()
                .find(|shortcut| shortcut.name == hotkey)
                .and_then(|shortcut| gtk_accelerator_from_native(&shortcut.keyseq))
        });
        match accelerator.as_deref() {
            Some(accelerator) => app.set_accels_for_action(action, &[accelerator]),
            None => app.set_accels_for_action(action, &[]),
        }
    }
}

fn gtk_accelerator_from_native(sequence: &str) -> Option<String> {
    // Consume modifier prefixes rather than splitting the final key: '+' is
    // itself a valid QKeySequence/GTK display label, including "Ctrl++" and
    // "Ctrl+KP\u{2009}+" emitted by the recording dialog.
    let normalized = sequence.replace(['\u{2009}', '\u{202f}', '\u{00a0}'], " ");
    let mut key = normalized.trim();
    let mut accelerator = String::new();
    while key != "+" && key != "KP +" {
        let Some((modifier, rest)) = key.split_once('+') else { break; };
        accelerator.push_str(match modifier.trim().to_ascii_lowercase().as_str() {
            "ctrl" | "control" => "<Control>",
            "shift" => "<Shift>",
            "alt" => "<Alt>",
            "meta" => "<Meta>",
            "super" => "<Super>",
            _ => return None,
        });
        key = rest.trim();
    }
    if key.is_empty() {
        return None;
    }
    accelerator.push_str(match key {
        "Esc" => "Escape",
        // QKeySequence uses punctuation; GTK expects the GDK key name.
        "," => "comma",
        "." => "period",
        "-" => "minus",
        "=" => "equal",
        "+" => "plus",
        // `gtk_accelerator_get_label` renders keypad operators as localized
        // display labels such as `KP -`, while `gtk_accelerator_parse` accepts
        // their stable GDK key names. Preserve the native label in the config
        // like Eden and translate it only at the GTK registry boundary.
        "KP -" => "KP_Subtract",
        "KP +" => "KP_Add",
        "KP *" => "KP_Multiply",
        "KP /" => "KP_Divide",
        "KP Enter" => "KP_Enter",
        other => other,
    });
    Some(accelerator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires GTK display; run in an isolated process"]
    fn recorded_plus_keys_roundtrip_into_live_accelerators() {
        gtk::init().unwrap();
        let app = gtk::Application::builder()
            .application_id("org.ruzu.PlusHotkeyTest").build();
        let original = crate::uisettings::with(|values| values.shortcuts.clone());
        for key in [gtk::gdk::Key::plus, gtk::gdk::Key::KP_Add] {
            for modifiers in [gtk::gdk::ModifierType::empty(), gtk::gdk::ModifierType::CONTROL_MASK] {
                // This is the same label-producing API as SequenceDialog.
                let label = gtk::accelerator_get_label(key, modifiers).to_string();
                crate::uisettings::with_mut(|values| {
                    values.shortcuts.iter_mut().find(|shortcut| shortcut.name == "Configure")
                        .unwrap().keyseq = label.clone();
                });
                apply_accelerators(&app);
                let installed = app.accels_for_action("app.configure");
                assert_eq!(installed.len(), 1, "{label}");
                assert_eq!(gtk::accelerator_parse(&installed[0]), Some((key, modifiers)), "{label}");
            }
        }
        crate::uisettings::with_mut(|values| {
            values.shortcuts.iter_mut().find(|shortcut| shortcut.name == "Configure")
                .unwrap().keyseq.clear();
        });
        apply_accelerators(&app);
        assert!(app.accels_for_action("app.configure").is_empty());
        crate::uisettings::with_mut(|values| values.shortcuts = original);
    }

    #[test]
    #[ignore = "requires GTK display; run in an isolated process"]
    fn tools_and_multiplayer_hotkeys_use_the_existing_menu_actions() {
        gtk::init().unwrap();
        let app = gtk::Application::builder()
            .application_id("org.ruzu.MenuHotkeyTest")
            .build();
        let original = crate::uisettings::with(|values| values.shortcuts.clone());
        for (name, action) in [
            ("Configure Current Game", "app.configure_current_game"),
            ("Capture Screenshot", "app.capture_screenshot"),
            ("Load/Remove Amiibo", "app.load_amiibo"),
            ("TAS Start/Stop", "app.tas_start"),
            ("TAS Record", "app.tas_record"),
            ("TAS Reset", "app.tas_reset"),
            ("Browse Public Game Lobby", "app.view_lobby"),
            ("Direct Connect to Room", "app.connect_to_room"),
            ("Show Current Room", "app.show_room"),
            ("Leave Room", "app.leave_room"),
            ("Change Adapting Filter", "app.toggle_adapting_filter"),
            ("Change GPU Mode", "app.toggle_gpu_accuracy"),
            ("Change Docked Mode", "app.toggle_docked_mode"),
            ("Audio Mute/Unmute", "app.audio_mute"),
            ("Audio Volume Down", "app.audio_volume_down"),
            ("Audio Volume Up", "app.audio_volume_up"),
            ("Toggle Framerate Limit", "app.toggle_framerate_limit"),
            ("Toggle Turbo Speed", "app.toggle_turbo_speed"),
            ("Toggle Slow Speed", "app.toggle_slow_speed"),
        ] {
            for (key, expected) in [("F7", vec!["F7"]), ("F8", vec!["F8"]), ("", vec![])] {
                crate::uisettings::with_mut(|values| {
                    values.shortcuts.iter_mut().find(|shortcut| shortcut.name == name)
                        .unwrap().keyseq = key.to_owned();
                });
                apply_accelerators(&app);
                assert_eq!(app.accels_for_action(action), expected, "{name}");
            }
        }
        crate::uisettings::with_mut(|values| values.shortcuts = original);
    }

    #[test]
    #[ignore = "requires GTK display; run in an isolated process"]
    fn renderdoc_accelerator_is_installed_rebound_and_removed() {
        gtk::init().unwrap();
        for sequence in ["Ctrl+,", "Ctrl+."] {
            assert!(gtk::accelerator_parse(&gtk_accelerator_from_native(sequence).unwrap()).is_some());
        }
        let app = gtk::Application::builder()
            .application_id("org.ruzu.RenderdocHotkeyTest")
            .build();
        let original = crate::uisettings::with(|values| values.shortcuts.clone());
        for (sequence, expected) in [("F7", vec!["F7"]), ("F8", vec!["F8"]), ("", vec![])] {
            crate::uisettings::with_mut(|values| {
                values.shortcuts.iter_mut()
                    .find(|shortcut| shortcut.name == "Toggle Renderdoc Capture")
                    .unwrap().keyseq = sequence.to_owned();
            });
            apply_accelerators(&app);
            assert_eq!(app.accels_for_action("app.renderdoc_capture"), expected);
        }
        crate::uisettings::with_mut(|values| values.shortcuts = original);
    }

    #[test]
    fn converts_native_shortcut_labels_to_gtk_accelerators() {
        for (native, expected) in [
            ("+", "plus"), ("Ctrl++", "<Control>plus"),
            ("Ctrl+Shift++", "<Control><Shift>plus"),
            ("KP\u{2009}+", "KP_Add"), ("Ctrl+KP\u{2009}+", "<Control>KP_Add"),
            ("Alt+KP +", "<Alt>KP_Add"),
        ] {
            assert_eq!(gtk_accelerator_from_native(native).as_deref(), Some(expected));
        }
        for invalid in ["", "Ctrl+", "Home+B", "Ctrl+++", "Ctrl+KP++"] {
            assert_eq!(gtk_accelerator_from_native(invalid), None);
        }
        for (native, gtk_key) in [("Ctrl+,", "<Control>comma"), ("Ctrl+.", "<Control>period")] {
            assert_eq!(gtk_accelerator_from_native(native).as_deref(), Some(gtk_key));
        }
        assert_eq!(
            gtk_accelerator_from_native("Ctrl+Shift+F4").as_deref(),
            Some("<Control><Shift>F4")
        );
        assert_eq!(
            gtk_accelerator_from_native("Esc").as_deref(),
            Some("Escape")
        );
        assert_eq!(gtk_accelerator_from_native("Home+B"), None);
        assert_eq!(
            gtk_accelerator_from_native("KP\u{2009}-").as_deref(),
            Some("KP_Subtract")
        );
    }
}
