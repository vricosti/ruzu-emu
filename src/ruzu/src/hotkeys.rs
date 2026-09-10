// SPDX-License-Identifier: GPL-3.0-or-later
//
// GTK counterpart of `/home/vricosti/Dev/emulators/eden/src/yuzu/hotkeys.cpp`.
// The registry data is owned by `uisettings`; this module implements keyboard
// conversion and HID controller-shortcut detection for window-owned actions.

use gtk::prelude::*;
use std::sync::Arc;
use parking_lot::Mutex;
use hid_core::frontend::emulated_controller::{ControllerTriggerType, ControllerUpdateCallback};
use hid_core::hid_core::EmulatedControllerHandle;
use hid_core::hid_types::{NpadButton, NpadButtonState, HomeButtonState, CaptureButtonState};

/// GTK's native render child routes keys through MainWindow's capture handler,
/// before application accelerators. Track consumed hardware keys there to
/// provide QShortcut's auto-repeat contract without repeating guest input.
#[derive(Default)]
pub(crate) struct KeyboardShortcutState {
    held: std::collections::HashSet<u32>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum KeyboardShortcutEvent {
    Unhandled,
    Suppressed,
    Activate(&'static str),
}

impl KeyboardShortcutState {
    pub(crate) fn press(
        &mut self, keycode: u32, binding: Option<(&'static str, bool)>,
    ) -> KeyboardShortcutEvent {
        let was_held = self.held.contains(&keycode);
        let Some((action, repeat)) = binding else {
            return if was_held { KeyboardShortcutEvent::Suppressed } else { KeyboardShortcutEvent::Unhandled };
        };
        self.held.insert(keycode);
        if was_held && !repeat { KeyboardShortcutEvent::Suppressed }
        else { KeyboardShortcutEvent::Activate(action) }
    }

    pub(crate) fn release(&mut self, keycode: u32) -> bool { self.held.remove(&keycode) }
    pub(crate) fn clear(&mut self) { self.held.clear(); }
}

pub(crate) fn keyboard_binding(
    app: &gtk::Application, keyval: gtk::gdk::Key, state: gtk::gdk::ModifierType,
) -> Option<(&'static str, bool)> {
    keyboard_binding_in_context(app, keyval, state, false)
}

fn keyboard_binding_in_context(
    app: &gtk::Application, keyval: gtk::gdk::Key, state: gtk::gdk::ModifierType,
    application_only: bool,
) -> Option<(&'static str, bool)> {
    crate::uisettings::with(|values| {
        HOTKEY_ACTIONS.iter().find_map(|&(name, action)| {
            let shortcut = values.shortcuts.iter().find(|shortcut| shortcut.name == name)?;
            if application_only && shortcut.context != crate::uisettings::APPLICATION_SHORTCUT { return None; }
            let accelerator = gtk_accelerator_from_native(&shortcut.keyseq)?;
            let (key, modifiers) = gtk::accelerator_parse(&accelerator)?;
            let mask = gtk::accelerator_get_default_mod_mask();
            if key.to_lower() != keyval.to_lower() || modifiers != state & mask { return None; }
            let target = app.lookup_action(action.strip_prefix("app.")?)?;
            target.is_enabled().then_some((action, shortcut.repeat))
        })
    })
}

/// QShortcut application context also reaches nonmodal auxiliary windows.
/// Plain GTK transient windows do not inherit the parent's application or its
/// accelerators. Install only that context here, owned by the auxiliary window.
pub(crate) fn install_secondary_window_shortcuts(window: &gtk::Window, owner: &gtk::ApplicationWindow) {
    let held = std::rc::Rc::new(std::cell::RefCell::new(KeyboardShortcutState::default()));
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed({
        let held = std::rc::Rc::clone(&held);
        let owner = owner.downgrade();
        let window = window.downgrade();
        move |_, key, code, modifiers| {
            let (Some(owner), Some(window)) = (owner.upgrade(), window.upgrade()) else { return gtk::glib::Propagation::Proceed; };
            if window.is_modal() || owner_blocked_by_modal(&owner) { return gtk::glib::Propagation::Proceed; }
            let Some(app) = owner.application() else { return gtk::glib::Propagation::Proceed; };
            let binding = keyboard_binding_in_context(&app, key, modifiers, true);
            let event = held.borrow_mut().press(code, binding);
            match event {
                KeyboardShortcutEvent::Activate(action) => {
                    gtk::gio::prelude::ActionGroupExt::activate_action(&app, action.strip_prefix("app.").unwrap(), None);
                    gtk::glib::Propagation::Stop
                }
                KeyboardShortcutEvent::Suppressed => gtk::glib::Propagation::Stop,
                KeyboardShortcutEvent::Unhandled => gtk::glib::Propagation::Proceed,
            }
        }
    });
    keys.connect_key_released({
        let held = std::rc::Rc::clone(&held);
        move |_, _, code, _| { held.borrow_mut().release(code); }
    });
    window.add_controller(keys);
    let focus = gtk::EventControllerFocus::new();
    focus.connect_leave(move |_| held.borrow_mut().clear());
    window.add_controller(focus);
}

pub(crate) fn owner_blocked_by_modal(owner: &impl IsA<gtk::Window>) -> bool {
    let windows = gtk::Window::list_toplevels();
    windows.into_iter().filter_map(|widget| widget.downcast::<gtk::Window>().ok()).any(|window| {
        if !window.is_visible() || !window.is_modal() { return false; }
        let mut parent = window.transient_for();
        while let Some(window) = parent {
            if window == *owner.upcast_ref::<gtk::Window>() { return true; }
            parent = window.transient_for();
        }
        false
    })
}

/// ControllerButtonSequence in upstream hotkeys.h.
#[derive(Default, Clone, Copy)]
struct ControllerButtonSequence {
    capture: u64,
    home: u64,
    npad: NpadButton,
}

impl ControllerButtonSequence {
    fn parse(text: &str) -> Self {
        let mut sequence = Self::default();
        for button in text.split('+') {
            sequence.npad |= match button {
                "A" => NpadButton::A, "B" => NpadButton::B,
                "X" => NpadButton::X, "Y" => NpadButton::Y,
                "L" => NpadButton::L, "R" => NpadButton::R,
                "ZL" => NpadButton::ZL, "ZR" => NpadButton::ZR,
                "Dpad_Left" => NpadButton::LEFT, "Dpad_Right" => NpadButton::RIGHT,
                "Dpad_Up" => NpadButton::UP, "Dpad_Down" => NpadButton::DOWN,
                "Left_Stick" => NpadButton::STICK_L, "Right_Stick" => NpadButton::STICK_R,
                "Minus" => NpadButton::MINUS, "Plus" => NpadButton::PLUS,
                "Home" => { sequence.home = 1; NpadButton::empty() }
                "Screenshot" => { sequence.capture = 1; NpadButton::empty() }
                _ => NpadButton::empty(),
            };
        }
        sequence
    }

    fn is_empty(self) -> bool { self.npad.is_empty() && self.home == 0 && self.capture == 0 }
}

struct ControllerShortcutState {
    sequence: ControllerButtonSequence,
    active: bool,
    enabled: bool,
}

impl ControllerShortcutState {
    fn update(&mut self, trigger: ControllerTriggerType, buttons: (NpadButtonState, HomeButtonState, CaptureButtonState)) -> bool {
        if !self.enabled || trigger != ControllerTriggerType::Button || self.sequence.is_empty() {
            return false;
        }
        let matched = buttons.0.raw.contains(self.sequence.npad)
            && buttons.1.raw & self.sequence.home == self.sequence.home
            && buttons.2.raw & self.sequence.capture == self.sequence.capture;
        if matched && !self.active {
            self.active = true;
            return true;
        }
        // Preserve ControllerUpdateEvent literally: even a matching event
        // received while active clears the latch upstream.
        self.active = false;
        false
    }
}

/// ControllerShortcut: evaluate live button state in the HID callback, before
/// GTK dispatch. The callback-safe view avoids re-locking the controller owner.
pub struct ControllerShortcut {
    controller: EmulatedControllerHandle,
    callback_key: i32,
    state: Arc<Mutex<ControllerShortcutState>>,
}

impl ControllerShortcut {
    pub fn new(controller: EmulatedControllerHandle, activate: impl Fn() + Send + Sync + 'static) -> Self {
        let state = Arc::new(Mutex::new(ControllerShortcutState {
            sequence: ControllerButtonSequence::default(), active: false, enabled: true,
        }));
        let callback_state = Arc::clone(&state);
        let callback_key = {
            let mut owner = controller.lock();
            let reader = owner.button_state_reader();
            owner.set_callback(ControllerUpdateCallback {
                on_change: Arc::new(move |trigger| {
                    if trigger != ControllerTriggerType::Button { return; }
                    let activate_now = callback_state.lock().update(trigger, reader.read());
                    if activate_now { activate(); }
                }),
                is_npad_service: false,
            })
        };
        Self { controller, callback_key, state }
    }

    pub fn set_key(&self, text: &str) {
        self.state.lock().sequence = ControllerButtonSequence::parse(text);
    }
}

impl Drop for ControllerShortcut {
    fn drop(&mut self) {
        self.state.lock().enabled = false;
        self.controller.lock().delete_callback(self.callback_key);
    }
}

/// Apply the subset of upstream keyboard hotkeys whose GTK actions are already
/// ported. Reapplying disconnects the former accelerator exactly as
/// `HotkeyRegistry::LoadHotkeys` updates an existing `QShortcut`.
pub(crate) const HOTKEY_ACTIONS: &[(&str, &str)] = &[
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
        ("Toggle Performance Overlay", "app.show_perf_overlay"),
        ("Toggle Renderdoc Capture", "app.renderdoc_capture"),
        ("Toggle Mouse Panning", "app.toggle_mouse_panning"),
        ("Toggle Framerate Limit", "app.toggle_framerate_limit"),
        ("Toggle Turbo Speed", "app.toggle_turbo_speed"),
        ("Toggle Slow Speed", "app.toggle_slow_speed"),
        ("Exit ruzu", "app.quit"),
    ];

pub fn apply_accelerators(app: &gtk::Application) {
    for &(hotkey, action) in HOTKEY_ACTIONS {
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

pub(crate) fn gtk_accelerator_from_native(sequence: &str) -> Option<String> {
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
    fn keyboard_repeat_release_and_focus_lifecycle() {
        use KeyboardShortcutEvent::*;
        let mut state = KeyboardShortcutState::default();
        assert_eq!(state.press(10, None), Unhandled);
        assert_eq!(state.press(10, Some(("app.pause", false))), Activate("app.pause"));
        assert_eq!(state.press(10, Some(("app.pause", false))), Suppressed);
        // A released modifier or disabled/rebound action must not leak the
        // remainder of a consumed press into the guest.
        assert_eq!(state.press(10, None), Suppressed);
        assert!(state.release(10));
        assert!(!state.release(10));
        assert_eq!(state.press(10, Some(("app.audio_volume_up", true))), Activate("app.audio_volume_up"));
        assert_eq!(state.press(10, Some(("app.audio_volume_up", true))), Activate("app.audio_volume_up"));
        state.clear();
        assert!(!state.release(10));
        assert_eq!(state.press(10, Some(("app.pause", false))), Activate("app.pause"));
    }

    #[test]
    #[ignore = "requires GTK display; run in an isolated process"]
    fn secondary_window_shortcuts_respect_context_repeat_and_modality() {
        gtk::init().unwrap();
        let app = gtk::Application::builder().application_id("org.ruzu.SecondaryHotkeysTest").build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let owner = gtk::ApplicationWindow::builder().application(&app).build();
        let secondary = gtk::Window::builder().transient_for(&owner).build();
        install_secondary_window_shortcuts(&secondary, &owner);
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        for name in ["audio_volume_up", "fullscreen"] {
            let action = gtk::gio::SimpleAction::new(name, None);
            action.connect_activate({ let calls = calls.clone(); move |_, _| calls.set(calls.get() + 1) });
            app.add_action(&action);
        }
        let original = crate::uisettings::with(|values| values.shortcuts.clone());
        crate::uisettings::with_mut(|values| {
            values.shortcuts.retain(|shortcut| matches!(shortcut.name.as_str(), "Audio Volume Up" | "Fullscreen"));
            for shortcut in &mut values.shortcuts {
                shortcut.keyseq = if shortcut.name == "Fullscreen" { "F11" } else { "F12" }.into();
                shortcut.repeat = false;
            }
        });
        let controllers = secondary.observe_controllers();
        let keys = (0..controllers.n_items()).find_map(|i| controllers.item(i).and_downcast::<gtk::EventControllerKey>()).unwrap();
        let press = |key: gtk::gdk::Key| keys.emit_by_name::<bool>("key-pressed", &[&key, &96u32, &gtk::gdk::ModifierType::empty()]);
        assert!(!press(gtk::gdk::Key::F11), "window context belongs to main only");
        assert!(press(gtk::gdk::Key::F12));
        assert!(press(gtk::gdk::Key::F12));
        assert_eq!(calls.get(), 1);
        keys.emit_by_name::<()>("key-released", &[&gtk::gdk::Key::F12, &96u32, &gtk::gdk::ModifierType::empty()]);
        let modal = gtk::Window::builder().transient_for(&owner).modal(true).build();
        modal.set_visible(true);
        assert!(!press(gtk::gdk::Key::F12), "modal sibling blocks main-owned application shortcut");
        assert_eq!(calls.get(), 1);
        modal.set_visible(false);
        assert!(press(gtk::gdk::Key::F12));
        assert_eq!(calls.get(), 2);
        secondary.set_modal(true);
        assert!(!press(gtk::gdk::Key::F12));
        modal.destroy(); secondary.destroy(); owner.destroy();
        crate::uisettings::with_mut(|values| values.shortcuts = original);
    }

    #[test]
    #[ignore = "requires GTK display; run in an isolated process"]
    fn keyboard_lookup_honors_binding_repeat_and_action_enabled() {
        gtk::init().unwrap();
        let app = gtk::Application::builder().application_id("org.ruzu.KeyboardRoutingTest").build();
        let action = gtk::gio::SimpleAction::new("audio_volume_up", None);
        app.add_action(&action);
        let original = crate::uisettings::with(|values| values.shortcuts.clone());
        crate::uisettings::with_mut(|values| {
            values.shortcuts.retain(|shortcut| shortcut.name == "Audio Volume Up");
            values.shortcuts[0].keyseq = "Ctrl+Shift+M".into();
            values.shortcuts[0].repeat = true;
        });
        let mods = gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::SHIFT_MASK;
        assert_eq!(keyboard_binding(&app, gtk::gdk::Key::M, mods), Some(("app.audio_volume_up", true)));
        action.set_enabled(false);
        assert_eq!(keyboard_binding(&app, gtk::gdk::Key::M, mods), None);
        action.set_enabled(true);
        crate::uisettings::with_mut(|values| { values.shortcuts[0].repeat = false; });
        assert_eq!(keyboard_binding(&app, gtk::gdk::Key::M, mods), Some(("app.audio_volume_up", false)));
        crate::uisettings::with_mut(|values| { values.shortcuts[0].keyseq.clear(); });
        assert_eq!(keyboard_binding(&app, gtk::gdk::Key::M, mods), None);
        crate::uisettings::with_mut(|values| values.shortcuts = original);
    }

    #[test]
    fn controller_sequences_and_latch_follow_upstream() {
        let sequence = ControllerButtonSequence::parse("A+B+Home+Screenshot+Unknown");
        assert_eq!(sequence.npad, NpadButton::A | NpadButton::B);
        assert_eq!((sequence.home, sequence.capture), (1, 1));
        assert!(ControllerButtonSequence::parse("unknown++").is_empty());
        let mut state = ControllerShortcutState { sequence, active: false, enabled: true };
        let buttons = (NpadButtonState { raw: NpadButton::A | NpadButton::B | NpadButton::X },
            HomeButtonState { raw: 1 }, CaptureButtonState { raw: 1 });
        assert!(!state.update(ControllerTriggerType::Stick, buttons));
        assert!(state.update(ControllerTriggerType::Button, buttons));
        // This unusual repeated-match behavior is the current upstream code,
        // not a Rust debounce policy invented for this port.
        assert!(!state.update(ControllerTriggerType::Button, buttons));
        assert!(state.update(ControllerTriggerType::Button, buttons));
        assert!(!state.update(ControllerTriggerType::Button, Default::default()));
        state.enabled = false;
        assert!(!state.update(ControllerTriggerType::Button, buttons));
    }

    #[test]
    #[ignore = "requires isolated GTK/SDL and input-factory state"]
    fn controller_shortcuts_receive_fast_input_rebind_clear_and_drop() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use hid_core::frontend::emulated_controller::EmulatedController;
        use hid_core::hid_types::{NpadIdType, NpadStyleIndex};
        gtk::init().unwrap();
        let mut input = input_common::InputSubsystem::new();
        input.initialize();
        let controller = Arc::new(Mutex::new(EmulatedController::new(NpadIdType::Player1)));
        {
            let mut owner = controller.lock();
            owner.set_npad_style_index(NpadStyleIndex::Fullkey);
            for index in 0..2 {
                let mut params = common::param_package::ParamPackage::default();
                params.set_str("engine", "virtual_gamepad".to_owned());
                params.set_str("guid", common::uuid::UUID::default().raw_string());
                params.set_int("port", 0);
                params.set_int("pad", 0);
                params.set_int("button", index as i32);
                owner.set_button_param(index, params);
            }
            owner.reload_input();
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let shortcut = ControllerShortcut::new(Arc::clone(&controller), {
            let calls = Arc::clone(&calls);
            move || { calls.fetch_add(1, Ordering::Relaxed); }
        });
        shortcut.set_key("A+B");
        let gamepad = input.get_virtual_gamepad_mut().unwrap();
        let _owner = controller.lock();
        gamepad.set_button_state_by_id(0, 0, true);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        gamepad.set_button_state_by_id(0, 1, true);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        gamepad.set_button_state_by_id(0, 1, false);
        gamepad.set_button_state_by_id(0, 1, true);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        shortcut.set_key("");
        gamepad.set_button_state_by_id(0, 1, false);
        gamepad.set_button_state_by_id(0, 1, true);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        shortcut.set_key("A");
        gamepad.set_button_state_by_id(0, 0, false);
        gamepad.set_button_state_by_id(0, 0, true);
        assert_eq!(calls.load(Ordering::Relaxed), 3);
        drop(_owner);
        drop(shortcut);
        gamepad.set_button_state_by_id(0, 0, false);
        gamepad.set_button_state_by_id(0, 0, true);
        assert_eq!(calls.load(Ordering::Relaxed), 3);
    }

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
