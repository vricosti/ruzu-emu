// SPDX-License-Identifier: GPL-3.0-or-later
//! GTK counterpart of yuzu/configuration/configure_ringcon.{h,cpp,ui}.

use common::input::{DriverResult, PollingMode};
use common::param_package::ParamPackage;
use gtk::prelude::*;
use hid_core::frontend::emulated_controller::{
    ControllerTriggerType, ControllerUpdateCallback, EmulatedController, EmulatedDeviceIndex,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

const ANALOG_SUB_BUTTONS: [&str; 2] = ["left", "right"];

struct ConfigureRingController {
    controller: Arc<parking_lot::Mutex<EmulatedController>>,
    input: Rc<RefCell<input_common::InputSubsystem>>,
    buttons: [gtk::Button; 2],
    deadzone: gtk::Scale,
    sensor: gtk::Label,
    capture: Cell<Option<(usize, Instant)>>,
    ring_enabled: Cell<bool>,
    changed: Arc<AtomicBool>,
    callback_key: i32,
    timer: RefCell<Option<gtk::glib::SourceId>>,
    updating: Cell<bool>,
    pressed_keys: RefCell<std::collections::HashSet<i32>>,
    pressed_mouse: Cell<Option<input_common::drivers::mouse::MouseButton>>,
}

impl ConfigureRingController {
    fn update_ui(&self) {
        let param = self.controller.lock().get_ring_param();
        self.updating.set(true);
        for (button, direction) in self.buttons.iter().zip(ANALOG_SUB_BUTTONS) {
            button.set_label(&analog_to_text(&param, direction));
        }
        self.deadzone
            .set_value((param.get_float("deadzone", 0.15) * 100.0) as i32 as f64);
        self.updating.set(false);
    }

    fn apply_configuration(&self) {
        hid_core::hid_core::with_controller(&self.controller, |controller| {
            controller.disable_configuration()
        });
        let mut controller = self.controller.lock();
        controller.save_current_config();
        controller.enable_configuration();
    }

    fn restore_defaults(&self) {
        let defaults = input_common::main_common::generate_analog_param_from_keys(
            0,
            0,
            super::qt_config::DEFAULT_RINGCON_ANALOGS[0],
            super::qt_config::DEFAULT_RINGCON_ANALOGS[1],
            0,
            0.05,
        );
        self.controller
            .lock()
            .set_ring_param(ParamPackage::from_serialized(&defaults));
        self.update_ui();
    }

    fn handle_click(&self, index: usize) {
        if self.capture.get().is_some() {
            return;
        }
        self.buttons[index].set_label(&crate::i18n::tr("[waiting]"));
        self.buttons[index].grab_focus();
        self.input
            .borrow_mut()
            .begin_mapping(input_common::polling::InputType::Stick);
        self.capture
            .set(Some((index, Instant::now() + Duration::from_millis(2500))));
    }

    fn set_polling_result(&self, param: Option<ParamPackage>) {
        let Some((index, _)) = self.capture.take() else {
            return;
        };
        self.input.borrow_mut().stop_mapping();
        if let Some(param) = param {
            let mut controller = self.controller.lock();
            let mut analog = controller.get_ring_param();
            set_analog_param(&param, &mut analog, ANALOG_SUB_BUTTONS[index]);
            controller.set_ring_param(analog);
        }
        self.update_ui();
    }

    fn enable_ring_controller(&self) {
        self.ring_enabled.set(false);
        self.sensor.set_text(&crate::i18n::tr("Not connected"));
        if !*common::settings::values().enable_joycon_driver.get_value() {
            self.sensor
                .set_text(&crate::i18n::tr("Direct Joycon driver is not enabled"));
            return;
        }
        let result = self
            .controller
            .lock()
            .set_polling_mode(EmulatedDeviceIndex::RightIndex, PollingMode::Ring);
        self.ring_enabled.set(result == DriverResult::Success);
        let message = match result {
            DriverResult::Success => {
                self.changed.store(true, Ordering::Release);
                return;
            }
            DriverResult::NotSupported => {
                "The current mapped device doesn't support the ring controller"
            }
            DriverResult::NoDeviceDetected => {
                "The current mapped device doesn't have a ring attached"
            }
            DriverResult::InvalidHandle => "The current mapped device is not connected",
            _ => "Unexpected driver result",
        };
        self.sensor.set_text(&crate::i18n::tr(message));
    }
}

impl Drop for ConfigureRingController {
    fn drop(&mut self) {
        if let Some(timer) = self.timer.get_mut().take() {
            timer.remove();
        }
        if self.capture.take().is_some() {
            self.input.borrow_mut().stop_mapping();
        }
        {
            let mut input = self.input.borrow_mut();
            if let Some(keyboard) = input.get_keyboard() {
                for key in self.pressed_keys.get_mut().drain() {
                    keyboard.release_key(key);
                }
            }
            if let (Some(mouse), Some(button)) = (input.get_mouse_mut(), self.pressed_mouse.take())
            {
                mouse.release_button(button);
            }
        }
        self.controller
            .lock()
            .set_polling_mode(EmulatedDeviceIndex::RightIndex, PollingMode::Active);
        hid_core::hid_core::with_controller(&self.controller, |controller| {
            controller.disable_configuration()
        });
        self.controller.lock().delete_callback(self.callback_key);
    }
}

pub fn present(
    source: &impl IsA<gtk::Widget>,
    input: Rc<RefCell<input_common::InputSubsystem>>,
    hid: Arc<parking_lot::Mutex<hid_core::hid_core::HIDCore>>,
) {
    let window = gtk::Window::builder()
        .title("Configure Ring Controller")
        .modal(true)
        .default_width(420)
        .build();
    if let Some(parent) = source.root().and_downcast::<gtk::Window>() {
        window.set_transient_for(Some(&parent));
        window.set_destroy_with_parent(true);
        let child = window.downgrade();
        let handler = parent.connect_unrealize(move |_| {
            if let Some(child) = child.upgrade() {
                child.destroy();
            }
        });
        let parent = parent.downgrade();
        let handler = RefCell::new(Some(handler));
        window.connect_unrealize(move |_| {
            if let (Some(parent), Some(handler)) = (parent.upgrade(), handler.borrow_mut().take()) {
                parent.disconnect(handler);
            }
        });
    }
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(8);
    content.set_margin_bottom(8);
    content.set_margin_start(8);
    content.set_margin_end(8);
    let description = gtk::Label::new(Some("To use Ring-Con, configure player 1 as right Joy-Con (both physical and emulated), and player 2 as left Joy-Con (left physical and dual emulated) before starting the game."));
    description.set_wrap(true);
    description.set_max_width_chars(52);
    content.append(&description);
    let (group, rows) = super::shared_widget::group("Virtual Ring Sensor Parameters");
    let buttons = [gtk::Button::new(), gtk::Button::new()];
    for (label, button) in ["Pull", "Push"].into_iter().zip(&buttons) {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let label = gtk::Label::new(Some(label));
        label.set_hexpand(true);
        row.append(&label);
        row.append(button);
        rows.append(&row);
    }
    let deadzone = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    deadzone.set_format_value_func(|_, value| {
        crate::i18n::tr("Deadzone: %1%").replace("%1", &(value as i32).to_string())
    });
    rows.append(&deadzone);
    content.append(&group);
    let (group, rows) = super::shared_widget::group("Direct Joycon Driver");
    let enable = gtk::Button::with_label("Enable Ring Input");
    rows.append(&enable);
    let sensor = gtk::Label::new(Some("Not connected"));
    sensor.set_wrap(true);
    rows.append(&sensor);
    content.append(&group);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let defaults = gtk::Button::with_label("Restore Defaults");
    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::with_label("OK");
    for button in [&defaults, &cancel, &ok] {
        actions.append(button);
    }
    content.append(&actions);
    window.set_child(Some(&content));
    let controller = hid.lock().get_emulated_controller_by_index(0);
    let changed = Arc::new(AtomicBool::new(false));
    let callback_key = {
        let mut controller = controller.lock();
        controller.save_current_config();
        controller.enable_configuration();
        let changed = Arc::clone(&changed);
        // Input callbacks can run off the GTK thread and under controller locks.
        // Transfer only a notification; read the value on the UI thread below.
        controller.set_callback(ControllerUpdateCallback {
            is_npad_service: false,
            on_change: Arc::new(move |kind| {
                if kind == ControllerTriggerType::RingController {
                    changed.store(true, Ordering::Release);
                }
            }),
        })
    };
    let state = Rc::new(ConfigureRingController {
        controller,
        input,
        buttons,
        deadzone,
        sensor,
        capture: Cell::new(None),
        ring_enabled: Cell::new(false),
        changed,
        callback_key,
        timer: RefCell::new(None),
        updating: Cell::new(false),
        pressed_keys: RefCell::new(Default::default()),
        pressed_mouse: Cell::new(None),
    });
    state.update_ui();
    for (index, button) in state.buttons.iter().enumerate() {
        let weak = Rc::downgrade(&state);
        button.connect_clicked(move |_| {
            if let Some(state) = weak.upgrade() {
                state.handle_click(index);
            }
        });
        let popup = gtk::Popover::new();
        popup.set_parent(button);
        let actions = gtk::Box::new(gtk::Orientation::Vertical, 4);
        for (label, invert) in [("Clear", false), ("Invert axis", true)] {
            let action = gtk::Button::with_label(label);
            let weak = Rc::downgrade(&state);
            let popup = popup.downgrade();
            action.connect_clicked(move |_| {
                if let Some(state) = weak.upgrade() {
                    let mut controller = state.controller.lock();
                    let mut param = controller.get_ring_param();
                    if invert {
                        let value = if param.get_str("invert_x", "+") == "-" {
                            "+"
                        } else {
                            "-"
                        };
                        param.set_str("invert_x", value.into());
                    } else if param.get_str("engine", "") == "analog_from_button" {
                        param.erase(ANALOG_SUB_BUTTONS[index]);
                    } else {
                        param = ParamPackage::default();
                    }
                    // Eden's Clear action rewrites the unchanged parameter; remove
                    // the binding here so Clear actually clears persisted input.
                    controller.set_ring_param(param);
                    drop(controller);
                    state.update_ui();
                }
                if let Some(popup) = popup.upgrade() {
                    popup.popdown();
                }
            });
            actions.append(&action);
        }
        popup.set_child(Some(&actions));
        let gesture = gtk::GestureClick::new();
        gesture.set_button(3);
        let weak = Rc::downgrade(&state);
        let popup_weak = popup.downgrade();
        gesture.connect_pressed(move |_, _, _, _| {
            if let Some(state) = weak.upgrade() {
                if state.capture.get().is_none() {
                    if let Some(popup) = popup_weak.upgrade() {
                        popup.popup();
                    }
                }
            }
        });
        button.add_controller(gesture);
        button.connect_unrealize(move |_| popup.unparent());
    }
    let weak = Rc::downgrade(&state);
    state.deadzone.connect_value_changed(move |scale| {
        if let Some(state) = weak.upgrade() {
            if state.updating.get() {
                return;
            }
            let mut controller = state.controller.lock();
            let mut param = controller.get_ring_param();
            param.set_str(
                "deadzone",
                ((scale.value() as i32) as f32 / 100.0).to_string(),
            );
            controller.set_ring_param(param);
        }
    });
    let weak = Rc::downgrade(&state);
    defaults.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            state.restore_defaults();
        }
    });
    let weak = Rc::downgrade(&state);
    enable.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            state.enable_ring_controller();
        }
    });
    let weak = window.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(window) = weak.upgrade() {
            window.close();
        }
    });
    let weak = window.downgrade();
    let owner = Rc::downgrade(&state);
    ok.connect_clicked(move |_| {
        if let Some(state) = owner.upgrade() {
            state.apply_configuration();
        }
        if let Some(window) = weak.upgrade() {
            window.close();
        }
    });
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(&state);
    keys.connect_key_pressed(move |_, key, _, _| {
        let Some(state) = weak.upgrade() else {
            return gtk::glib::Propagation::Proceed;
        };
        if state.capture.get().is_none() {
            return gtk::glib::Propagation::Proceed;
        }
        if key != gtk::gdk::Key::Escape {
            let code = crate::main_window::gdk_key_to_qt_key(key);
            state.pressed_keys.borrow_mut().insert(code);
            if let Some(keyboard) = state.input.borrow().get_keyboard() {
                keyboard.press_key(code);
            }
        }
        gtk::glib::Propagation::Stop
    });
    let weak = Rc::downgrade(&state);
    keys.connect_key_released(move |_, key, _, _| {
        if let Some(state) = weak.upgrade() {
            let code = crate::main_window::gdk_key_to_qt_key(key);
            if state.pressed_keys.borrow_mut().remove(&code) {
                if let Some(keyboard) = state.input.borrow().get_keyboard() {
                    keyboard.release_key(code);
                }
            }
        }
    });
    window.add_controller(keys);
    let mouse = gtk::GestureClick::new();
    mouse.set_button(0);
    mouse.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(&state);
    mouse.connect_pressed(move |gesture, _, _, _| {
        if let Some(state) = weak.upgrade() {
            if state.capture.get().is_none() {
                return;
            }
            if let Some(mouse) = state.input.borrow_mut().get_mouse_mut() {
                let button = mouse_button(gesture.current_button());
                state.pressed_mouse.set(Some(button));
                mouse.press_button(0, 0, button);
                mouse.notify_changed();
            }
            gesture.set_state(gtk::EventSequenceState::Claimed);
        }
    });
    let weak = Rc::downgrade(&state);
    mouse.connect_released(move |_, _, _, _| {
        if let Some(state) = weak.upgrade() {
            if let Some(button) = state.pressed_mouse.take() {
                if let Some(mouse) = state.input.borrow_mut().get_mouse_mut() {
                    mouse.release_button(button);
                }
            }
        }
    });
    window.add_controller(mouse);
    let weak = Rc::downgrade(&state);
    *state.timer.borrow_mut() = Some(gtk::glib::timeout_add_local(
        Duration::from_millis(25),
        move || {
            let Some(state) = weak.upgrade() else {
                return gtk::glib::ControlFlow::Break;
            };
            if let Some((_, deadline)) = state.capture.get() {
                if Instant::now() >= deadline {
                    state.set_polling_result(None);
                } else {
                    let param = state.input.borrow_mut().get_next_input();
                    if param.has("engine") {
                        state.set_polling_result(Some(param));
                    }
                }
            }
            if state.ring_enabled.get() && state.changed.swap(false, Ordering::AcqRel) {
                state.sensor.set_text(&format!(
                    "{:.3}",
                    state.controller.lock().get_ring_sensor_values().raw_value
                ));
            }
            gtk::glib::ControlFlow::Continue
        },
    ));
    let owner = Rc::new(RefCell::new(Some(state)));
    let close_owner = Rc::clone(&owner);
    window.connect_close_request(move |_| {
        close_owner.borrow_mut().take();
        gtk::glib::Propagation::Proceed
    });
    window.connect_unrealize(move |_| {
        owner.borrow_mut().take();
    });
    crate::i18n::translate_widget_tree(&window);
    window.present();
}

fn mouse_button(button: u32) -> input_common::drivers::mouse::MouseButton {
    use input_common::drivers::mouse::MouseButton::*;
    match button {
        1 => Left,
        2 => Wheel,
        3 => Right,
        8 => Backward,
        9 => Forward,
        _ => Extra,
    }
}

/// ConfigureRingController's file-local SetAnalogParam helper.
/// A complete axis replaces the mapping, while a button replaces only its direction.
fn set_analog_param(input: &ParamPackage, analog: &mut ParamPackage, direction: &str) {
    if input.has("axis_x") && input.has("axis_y") {
        *analog = input.clone();
        return;
    }
    if !analog.has("engine") || analog.has("axis_x") || analog.has("axis_y") {
        *analog = ParamPackage::default();
        analog.set_str("engine", "analog_from_button".into());
    }
    analog.set_str(direction, input.serialize());
}

/// ConfigureRingController::AnalogToText.
fn analog_to_text(param: &ParamPackage, direction: &str) -> String {
    if !param.has("engine") {
        return crate::i18n::tr("[not set]");
    }
    if param.get_str("engine", "") == "analog_from_button" {
        return super::configure_input_player::button_to_text(&param.get_str(direction, ""));
    }
    if !param.has("axis_x") || !param.has("axis_y") {
        return crate::i18n::tr("[unknown]");
    }
    let (axis, invert, positive) = match direction {
        "left" => ("axis_x", "invert_x", false),
        "right" => ("axis_x", "invert_x", true),
        "up" => ("axis_y", "invert_y", true),
        "down" => ("axis_y", "invert_y", false),
        "modifier" => return crate::i18n::tr("[unused]"),
        _ => return crate::i18n::tr("[unknown]"),
    };
    let positive = positive != (param.get_str(invert, "+") == "-");
    format!(
        "{} {}{}",
        crate::i18n::tr("Axis"),
        param.get_str(axis, ""),
        if positive { "+" } else { "-" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display; run alone with --ignored"]
    fn ring_dialog_apply_cancel_capture_and_teardown() {
        fn collect<T: IsA<gtk::Widget> + gtk::glib::object::ObjectType>(
            widget: &gtk::Widget,
            out: &mut Vec<T>,
        ) {
            if let Ok(value) = widget.clone().downcast::<T>() {
                out.push(value);
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                collect(&widget, out);
                child = widget.next_sibling();
            }
        }
        gtk::init().unwrap();
        crate::i18n::set_language("en");
        let input = Rc::new(RefCell::new(input_common::InputSubsystem::new()));
        input.borrow_mut().initialize();
        let hid = Arc::new(parking_lot::Mutex::new(hid_core::hid_core::HIDCore::new()));
        let controller = hid.lock().get_emulated_controller_by_index(0);
        controller.lock().reload_from_settings();
        for action in ["Cancel", "OK", "destroy", "parent destroy"] {
            let initial = ParamPackage::from_serialized("engine:analog_from_button,deadzone:0.15");
            controller.lock().set_ring_param(initial.clone());
            let parent = gtk::Window::new();
            let source = gtk::Button::new();
            parent.set_child(Some(&source));
            parent.present();
            present(&source, Rc::clone(&input), Arc::clone(&hid));
            let window = gtk::Window::list_toplevels()
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::Window>().ok())
                .find(|w| w.title().as_deref() == Some("Configure Ring Controller"))
                .unwrap();
            assert!(controller.lock().is_configuring_mode());
            let mut buttons = Vec::<gtk::Button>::new();
            collect(window.upcast_ref(), &mut buttons);
            let mut scales = Vec::<gtk::Scale>::new();
            collect(window.upcast_ref(), &mut scales);
            scales[0].set_value(37.0);
            assert_eq!(
                common::settings::values().ringcon_analogs,
                initial.serialize()
            );
            assert_eq!(
                controller
                    .lock()
                    .get_ring_param()
                    .get_float("deadzone", 0.0),
                0.37
            );
            let pull = buttons
                .iter()
                .find(|b| b.label().as_deref() == Some("[not set]"))
                .unwrap();
            pull.emit_clicked();
            input.borrow().get_keyboard().unwrap().press_key(71);
            let deadline = Instant::now() + Duration::from_secs(1);
            while pull.label().as_deref() == Some("[waiting]") && Instant::now() < deadline {
                gtk::glib::MainContext::default().iteration(false);
                std::thread::sleep(Duration::from_millis(2));
            }
            input.borrow().get_keyboard().unwrap().release_key(71);
            assert_ne!(pull.label().as_deref(), Some("[waiting]"));
            let param = controller.lock().get_ring_param();
            assert_eq!(
                ParamPackage::from_serialized(&param.get_str("left", "")).get_int("code", 0),
                71
            );
            if action == "parent destroy" {
                parent.destroy();
            } else if action == "destroy" {
                window.destroy();
            } else {
                buttons
                    .iter()
                    .find(|b| b.label().as_deref() == Some(action))
                    .unwrap()
                    .emit_clicked();
            }
            assert!(!controller.lock().is_configuring_mode());
            assert_eq!(
                common::settings::values().ringcon_analogs,
                if action == "OK" {
                    param.serialize()
                } else {
                    initial.serialize()
                }
            );
            assert_eq!(
                controller
                    .lock()
                    .get_polling_mode(EmulatedDeviceIndex::RightIndex),
                PollingMode::Active
            );
            parent.destroy();
        }
    }

    #[test]
    fn ring_mapping_replaces_axes_but_preserves_the_opposite_button() {
        let left = ParamPackage::from_serialized("engine:keyboard,code:65");
        let right = ParamPackage::from_serialized("engine:keyboard,code:68");
        let mut analog = ParamPackage::default();
        set_analog_param(&left, &mut analog, ANALOG_SUB_BUTTONS[0]);
        set_analog_param(&right, &mut analog, ANALOG_SUB_BUTTONS[1]);
        assert_eq!(analog.get_str("engine", ""), "analog_from_button");
        assert_eq!(analog.get_str("left", ""), left.serialize());
        assert_eq!(analog.get_str("right", ""), right.serialize());

        let axis =
            ParamPackage::from_serialized("engine:sdl,axis_x:2,axis_y:3,invert_x:-,deadzone:0.25");
        set_analog_param(&axis, &mut analog, "left");
        assert_eq!(analog.serialize(), axis.serialize());
        set_analog_param(&right, &mut analog, "right");
        assert!(!analog.has("axis_x"));
        assert!(!analog.has("left"));
        assert!(!analog.has("deadzone"));
        assert_eq!(analog.get_str("right", ""), right.serialize());

        // Upstream also discards partially specified axis mappings.
        for incomplete in ["engine:sdl,axis_x:2", "engine:sdl,axis_y:3"] {
            let mut analog = ParamPackage::from_serialized(incomplete);
            set_analog_param(&left, &mut analog, "left");
            assert_eq!(analog.get_str("engine", ""), "analog_from_button");
            assert!(!analog.has("axis_x") && !analog.has("axis_y"));
        }
    }

    #[test]
    fn ring_axis_labels_follow_pull_push_and_inversion() {
        let mut axis = ParamPackage::from_serialized("engine:sdl,axis_x:2,axis_y:3");
        assert!(analog_to_text(&axis, "left").ends_with("2-"));
        assert!(analog_to_text(&axis, "right").ends_with("2+"));
        axis.set_str("invert_x", "-".into());
        assert!(analog_to_text(&axis, "left").ends_with("2+"));
        assert!(analog_to_text(&axis, "right").ends_with("2-"));
    }
}
