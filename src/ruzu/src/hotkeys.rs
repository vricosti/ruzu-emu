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
        ("Toggle Filter Bar", "app.show_filter_bar"),
        ("Toggle Status Bar", "app.show_status_bar"),
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
    fn converts_native_shortcut_labels_to_gtk_accelerators() {
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
