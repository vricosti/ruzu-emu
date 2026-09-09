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
    let mut parts = sequence.split('+').map(str::trim).collect::<Vec<_>>();
    let key = parts.pop()?;
    if key.is_empty() {
        return None;
    }
    let mut accelerator = String::new();
    for modifier in parts {
        accelerator.push_str(match modifier.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => "<Control>",
            "shift" => "<Shift>",
            "alt" => "<Alt>",
            "meta" => "<Meta>",
            "super" => "<Super>",
            _ => return None,
        });
    }
    let normalized_key = key
        .replace(['\u{2009}', '\u{202f}', '\u{00a0}'], " ")
        .trim()
        .to_owned();
    accelerator.push_str(match normalized_key.as_str() {
        "Esc" => "Escape",
        // QKeySequence uses punctuation; GTK expects the GDK key name.
        "," => "comma",
        "." => "period",
        "-" => "minus",
        "=" => "equal",
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
    fn tools_and_multiplayer_hotkeys_use_the_existing_menu_actions() {
        gtk::init().unwrap();
        let app = gtk::Application::builder()
            .application_id("org.ruzu.MenuHotkeyTest")
            .build();
        let original = crate::uisettings::with(|values| values.shortcuts.clone());
        for (name, action) in [
            ("Capture Screenshot", "app.capture_screenshot"),
            ("Load/Remove Amiibo", "app.load_amiibo"),
            ("TAS Start/Stop", "app.tas_start"),
            ("TAS Record", "app.tas_record"),
            ("TAS Reset", "app.tas_reset"),
            ("Browse Public Game Lobby", "app.view_lobby"),
            ("Direct Connect to Room", "app.connect_to_room"),
            ("Show Current Room", "app.show_room"),
            ("Leave Room", "app.leave_room"),
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
