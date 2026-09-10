// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of
// `/home/vricosti/Dev/emulators/zuyu/src/yuzu/configuration/configure_vibration.cpp`.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

use gtk::prelude::*;
use hid_core::frontend::emulated_controller::{ControllerTriggerType, ControllerUpdateCallback};
use hid_core::hid_core::EmulatedControllerHandle;
use hid_core::hid_types::{DeviceIndex, VibrationValue, DEFAULT_VIBRATION_VALUE};

const NUM_PLAYERS: usize = 8;

struct CallbackRegistration {
    controller: EmulatedControllerHandle,
    key: i32,
}

struct DialogLifetime {
    controllers: Vec<EmulatedControllerHandle>,
    callbacks: Vec<CallbackRegistration>,
}

impl DialogLifetime {
    fn stop_vibrations(&self) {
        stop_vibrations(&self.controllers);
    }
}

impl Drop for DialogLifetime {
    fn drop(&mut self) {
        self.stop_vibrations();
        for registration in &self.callbacks {
            registration
                .controller
                .lock()
                .delete_callback(registration.key);
        }
    }
}

/// Present upstream's `ConfigureVibration` dialog.
pub fn present(
    source: &impl IsA<gtk::Widget>,
    hid_core: Arc<parking_lot::Mutex<hid_core::hid_core::HIDCore>>,
) {
    let window = gtk::Window::builder()
        .title("Configure Vibration")
        .modal(true)
        .resizable(false)
        .default_width(520)
        .build();
    if let Some(parent) = source.root().and_downcast::<gtk::Window>() {
        window.set_transient_for(Some(&parent));
    }

    let controllers = {
        let hid_core = hid_core.lock();
        (0..NUM_PLAYERS)
            .map(|index| hid_core.get_emulated_controller_by_index(index))
            .collect::<Vec<_>>()
    };
    let settings = common::settings::values();
    let players = settings.players.get_value();

    let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);

    let explanation = gtk::Label::new(Some(
        "Press any button on a controller to test its vibration strength.",
    ));
    explanation.set_xalign(0.0);
    content.append(&explanation);

    let strength_group = gtk::Frame::new(Some("Vibration strength"));
    let grid = gtk::Grid::new();
    grid.set_row_spacing(6);
    grid.set_column_spacing(10);
    grid.set_margin_top(8);
    grid.set_margin_bottom(8);
    grid.set_margin_start(8);
    grid.set_margin_end(8);

    let mut enabled_widgets = Vec::with_capacity(NUM_PLAYERS);
    let mut strength_widgets = Vec::with_capacity(NUM_PLAYERS);
    let mut strengths = Vec::with_capacity(NUM_PLAYERS);
    for (index, player) in players.iter().take(NUM_PLAYERS).enumerate() {
        let enabled = gtk::CheckButton::with_label(&format!("Player {}", index + 1));
        enabled.set_active(player.vibration_enabled);
        let strength = gtk::SpinButton::with_range(0.0, 100.0, 1.0);
        strength.set_value(player.vibration_strength.into());
        strength.set_width_chars(4);
        strength.set_sensitive(player.vibration_enabled);
        let percent = gtk::Label::new(Some("%"));

        grid.attach(&enabled, 0, index as i32, 1, 1);
        grid.attach(&strength, 1, index as i32, 1, 1);
        grid.attach(&percent, 2, index as i32, 1, 1);

        let current_strength = Arc::new(AtomicI32::new(player.vibration_strength));
        {
            let current_strength = Arc::clone(&current_strength);
            strength.connect_value_changed(move |spin| {
                current_strength.store(spin.value_as_int(), Ordering::Relaxed);
            });
        }
        {
            let strength = strength.clone();
            enabled.connect_toggled(move |check| strength.set_sensitive(check.is_active()));
        }
        enabled_widgets.push(enabled);
        strength_widgets.push(strength);
        strengths.push(current_strength);
    }
    drop(settings);
    strength_group.set_child(Some(&grid));
    content.append(&strength_group);

    let accurate = gtk::CheckButton::with_label("Enable accurate vibrations");
    accurate.set_active(
        *common::settings::values()
            .enable_accurate_vibrations
            .get_value(),
    );
    accurate.set_sensitive(common::settings::is_configuring_global());
    content.append(&accurate);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::with_label("OK");
    ok.add_css_class("suggested-action");
    actions.append(&spacer);
    actions.append(&cancel);
    actions.append(&ok);
    content.append(&actions);
    window.set_child(Some(&content));

    let mut callbacks = Vec::with_capacity(NUM_PLAYERS);
    for (player_index, controller) in controllers.iter().enumerate() {
        let callback_controller = Arc::clone(controller);
        let all_controllers = controllers.clone();
        let strength = Arc::clone(&strengths[player_index]);
        let callback = ControllerUpdateCallback {
            on_change: Arc::new(move |trigger_type| {
                vibrate_controller(
                    trigger_type,
                    player_index,
                    strength.load(Ordering::Relaxed),
                    &callback_controller,
                    &all_controllers,
                );
            }),
            is_npad_service: false,
        };
        let key = controller.lock().set_callback(callback);
        callbacks.push(CallbackRegistration {
            controller: Arc::clone(controller),
            key,
        });
    }
    let lifetime = Arc::new(parking_lot::Mutex::new(Some(DialogLifetime {
        controllers,
        callbacks,
    })));

    {
        let window = window.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(window) = window.upgrade() {
                window.close();
            }
        });
    }
    {
        let window = window.downgrade();
        ok.connect_clicked(move |_| {
            let mut settings = common::settings::values_mut();
            let players = settings.players.get_value_mut();
            for index in 0..NUM_PLAYERS {
                players[index].vibration_enabled = enabled_widgets[index].is_active();
                players[index].vibration_strength = strength_widgets[index].value_as_int();
            }
            settings
                .enable_accurate_vibrations
                .set_value(accurate.is_active());
            drop(settings);
            if let Some(window) = window.upgrade() {
                window.close();
            }
        });
    }
    window.connect_close_request(move |_| {
        lifetime.lock().take();
        gtk::glib::Propagation::Proceed
    });

    window.present();
}

fn vibrate_controller(
    trigger_type: ControllerTriggerType,
    player_index: usize,
    vibration_strength: i32,
    controller: &EmulatedControllerHandle,
    controllers: &[EmulatedControllerHandle],
) {
    if trigger_type != ControllerTriggerType::Button {
        return;
    }
    if !controller
        .lock()
        .get_buttons_values()
        .iter()
        .any(|button| button.value)
    {
        stop_vibrations(controllers);
        return;
    }

    let (old_enabled, old_strength) = {
        let mut settings = common::settings::values_mut();
        let player = &mut settings.players.get_value_mut()[player_index];
        let old = (player.vibration_enabled, player.vibration_strength);
        player.vibration_enabled = true;
        player.vibration_strength = vibration_strength;
        old
    };

    let vibration = VibrationValue {
        low_amplitude: 1.0,
        low_frequency: 160.0,
        high_amplitude: 1.0,
        high_frequency: 320.0,
    };
    let mut controller = controller.lock();
    controller.set_vibration(DeviceIndex::Left, vibration);
    controller.set_vibration(DeviceIndex::Right, vibration);
    drop(controller);

    let mut settings = common::settings::values_mut();
    let player = &mut settings.players.get_value_mut()[player_index];
    player.vibration_enabled = old_enabled;
    player.vibration_strength = old_strength;
}

fn stop_vibrations(controllers: &[EmulatedControllerHandle]) {
    for controller in controllers {
        let mut controller = controller.lock();
        controller.set_vibration(DeviceIndex::Left, DEFAULT_VIBRATION_VALUE);
        controller.set_vibration(DeviceIndex::Right, DEFAULT_VIBRATION_VALUE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display; run alone in its own test process"]
    fn accurate_vibration_is_editable_only_in_global_configuration() {
        gtk::init().expect("GTK display required");
        fn find_accurate(widget: &gtk::Widget) -> Option<gtk::CheckButton> {
            if let Some(check) = widget.downcast_ref::<gtk::CheckButton>() {
                if check.label().as_deref() == Some("Enable accurate vibrations") {
                    return Some(check.clone());
                }
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                child = widget.next_sibling();
                if let Some(check) = find_accurate(&widget) { return Some(check); }
            }
            None
        }
        let source = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let core = Arc::new(parking_lot::Mutex::new(hid_core::hid_core::HIDCore::new()));
        for global in [true, false] {
            common::settings::set_configuring_global(global);
            present(&source, Arc::clone(&core));
            let window = gtk::Window::list_toplevels().into_iter()
                .filter_map(|widget| widget.downcast::<gtk::Window>().ok())
                .find(|window| window.title().as_deref() == Some("Configure Vibration"))
                .unwrap();
            assert_eq!(find_accurate(window.upcast_ref()).unwrap().is_sensitive(), global);
            window.close();
        }
        common::settings::set_configuring_global(true);
    }

    #[test]
    fn vibration_dialog_covers_all_upstream_player_slots() {
        assert_eq!(NUM_PLAYERS, 8);
    }
}
