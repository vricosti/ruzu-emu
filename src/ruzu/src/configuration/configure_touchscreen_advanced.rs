// SPDX-License-Identifier: GPL-2.0-or-later
//! GTK counterpart of yuzu/configuration/configure_touchscreen_advanced.{h,cpp,ui}.

use gtk::prelude::*;
use std::rc::Rc;

struct ConfigureTouchscreenAdvanced {
    diameter_x: gtk::SpinButton,
    diameter_y: gtk::SpinButton,
    angle: gtk::SpinButton,
}

impl ConfigureTouchscreenAdvanced {
    fn new() -> Self {
        // The upstream .ui leaves QSpinBox's default range at 0..99.
        let result = Self {
            diameter_x: gtk::SpinButton::with_range(0.0, 99.0, 1.0),
            diameter_y: gtk::SpinButton::with_range(0.0, 99.0, 1.0),
            angle: gtk::SpinButton::with_range(0.0, 99.0, 1.0),
        };
        result.load_configuration();
        result
    }

    fn load_configuration(&self) {
        let settings = common::settings::values();
        self.diameter_x
            .set_value(settings.touchscreen.diameter_x as f64);
        self.diameter_y
            .set_value(settings.touchscreen.diameter_y as f64);
        self.angle
            .set_value(settings.touchscreen.rotation_angle as f64);
    }

    fn apply_configuration(&self) {
        for spin in [&self.diameter_x, &self.diameter_y, &self.angle] {
            spin.update();
        }
        let mut settings = common::settings::values_mut();
        settings.touchscreen.diameter_x = self.diameter_x.value_as_int() as u32;
        settings.touchscreen.diameter_y = self.diameter_y.value_as_int() as u32;
        settings.touchscreen.rotation_angle = self.angle.value_as_int() as u32;
    }

    fn restore_defaults(&self) {
        self.diameter_x.set_value(15.0);
        self.diameter_y.set_value(15.0);
        self.angle.set_value(0.0);
    }
}

pub fn present(source: &impl IsA<gtk::Widget>) {
    let window = gtk::Window::builder()
        .title("Configure Touchscreen")
        .modal(true)
        .resizable(false)
        .default_width(360)
        .build();
    if let Some(parent) = source.root().and_downcast::<gtk::Window>() {
        window.set_transient_for(Some(&parent));
    }
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(8);
    content.set_margin_bottom(8);
    content.set_margin_start(8);
    content.set_margin_end(8);
    let warning = gtk::Label::new(Some("Warning: These settings affect the inner workings of Ruzu's emulated touchscreen. Changing them may cause the touchscreen to work incorrectly or stop working. Only change them if you know what you are doing."));
    warning.set_wrap(true);
    warning.set_max_width_chars(48);
    content.append(&warning);
    let state = Rc::new(ConfigureTouchscreenAdvanced::new());
    let frame = gtk::Frame::new(Some("Touch Parameters"));
    let grid = gtk::Grid::new();
    grid.set_row_spacing(8);
    grid.set_column_spacing(12);
    for (row, (name, spin)) in [
        ("Touch Diameter X", &state.diameter_x),
        ("Touch Diameter Y", &state.diameter_y),
        ("Rotational Angle", &state.angle),
    ]
    .into_iter()
    .enumerate()
    {
        grid.attach(&gtk::Label::new(Some(name)), 0, row as i32, 1, 1);
        grid.attach(spin, 1, row as i32, 1, 1);
    }
    frame.set_child(Some(&grid));
    content.append(&frame);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let restore = gtk::Button::with_label("Restore Defaults");
    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::with_label("OK");
    for button in [&restore, &cancel, &ok] {
        actions.append(button);
    }
    let draft = Rc::clone(&state);
    restore.connect_clicked(move |_| draft.restore_defaults());
    let weak = window.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(window) = weak.upgrade() {
            window.close();
        }
    });
    let weak = window.downgrade();
    ok.connect_clicked(move |_| {
        state.apply_configuration();
        if let Some(window) = weak.upgrade() {
            window.close();
        }
    });
    content.append(&actions);
    window.set_child(Some(&content));
    window.present();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display; run alone with --ignored"]
    fn touchscreen_draft_apply_defaults_and_runtime_values() {
        gtk::init().unwrap();
        {
            let mut settings = common::settings::values_mut();
            settings.touchscreen.diameter_x = 70;
            settings.touchscreen.diameter_y = 80;
            settings.touchscreen.rotation_angle = 90;
        }
        let dialog = ConfigureTouchscreenAdvanced::new();
        assert_eq!(dialog.diameter_x.value_as_int(), 70);
        assert_eq!(dialog.diameter_y.value_as_int(), 80);
        assert_eq!(dialog.angle.value_as_int(), 90);
        dialog.restore_defaults();
        assert_eq!(common::settings::values().touchscreen.diameter_x, 70);
        assert_eq!(dialog.diameter_x.value_as_int(), 15);
        assert_eq!(dialog.diameter_y.value_as_int(), 15);
        assert_eq!(dialog.angle.value_as_int(), 0);
        dialog.diameter_x.set_value(31.0);
        dialog.diameter_y.set_value(47.0);
        dialog.angle.set_value(63.0);
        dialog.apply_configuration();
        use hid_core::resources::touch_screen::{
            touch_screen_driver::TouchScreenDriver,
            touch_types::{TouchScreenState, MAX_FINGERS},
        };
        let hid = std::sync::Arc::new(parking_lot::Mutex::new(hid_core::hid_core::HIDCore::new()));
        let mut driver = TouchScreenDriver::new(hid);
        let mut fingers = [(0, false, 0.0, 0.0); MAX_FINGERS];
        fingers[0] = (0, true, 0.5, 0.25);
        driver.process_touch_input(&fingers);
        let mut state = TouchScreenState::default();
        driver.get_next_touch_state(&mut state);
        assert_eq!(state.entry_count, 1);
        assert_eq!(
            (
                state.states[0].diameter_x,
                state.states[0].diameter_y,
                state.states[0].rotation_angle
            ),
            (31, 47, 63)
        );
        dialog.restore_defaults();
        dialog.apply_configuration();
        driver.process_touch_input(&fingers);
        driver.get_next_touch_state(&mut state);
        assert_eq!(
            (
                state.states[0].diameter_x,
                state.states[0].diameter_y,
                state.states[0].rotation_angle
            ),
            (15, 15, 0)
        );
        dialog.angle.set_value(360.0);
        dialog.diameter_x.set_value(-1.0);
        assert_eq!(dialog.angle.value_as_int(), 99);
        assert_eq!(dialog.diameter_x.value_as_int(), 0);
    }
}
