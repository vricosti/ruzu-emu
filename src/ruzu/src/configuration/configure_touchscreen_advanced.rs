// SPDX-License-Identifier: GPL-3.0-or-later
//! GTK counterpart of yuzu/configuration/configure_touchscreen_advanced.{h,cpp,ui}.

use gtk::prelude::*;

pub fn present(source: &impl IsA<gtk::Widget>) {
    let window = build_dialog();
    if let Some(parent) = source.root().and_downcast::<gtk::Window>() {
        window.set_transient_for(Some(&parent));
        window.set_destroy_with_parent(true);
    }
    window.present();
}

fn build_dialog() -> gtk::Window {
    let window = gtk::Window::builder()
        .title("Configure Touchscreen")
        .modal(true)
        .resizable(false)
        .default_width(380)
        .build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
    for set_margin in [
        gtk::Widget::set_margin_top,
        gtk::Widget::set_margin_bottom,
        gtk::Widget::set_margin_start,
        gtk::Widget::set_margin_end,
    ] {
        set_margin(content.upcast_ref(), 8);
    }
    let warning = gtk::Label::new(Some(
        "Warning: The settings in this page affect the inner workings of Ruzu's emulated touchscreen. Changing them may result in undesirable behavior, such as the touchscreen partially or not working. You should only use this page if you know what you are doing.",
    ));
    warning.set_wrap(true);
    warning.set_max_width_chars(48);
    warning.set_xalign(0.0);
    content.append(&warning);
    let (group, rows) = super::shared_widget::group("Touch Parameters");
    let values = {
        let settings = common::settings::values();
        [
            settings.touchscreen.diameter_x,
            settings.touchscreen.diameter_y,
            settings.touchscreen.rotation_angle,
        ]
    };
    let spins = ["Touch Diameter X", "Touch Diameter Y", "Rotational Angle"].map(|label| {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let caption = gtk::Label::new(Some(label));
        caption.set_xalign(0.0);
        caption.set_hexpand(true);
        // QSpinBox in upstream .ui has the default integer range 0..99.
        let spin = gtk::SpinButton::with_range(0.0, 99.0, 1.0);
        row.append(&caption);
        row.append(&spin);
        rows.append(&row);
        spin
    });
    for (spin, value) in spins.iter().zip(values) {
        // Upstream passes unsigned settings through QSpinBox::setValue(int).
        spin.set_value((value as i32) as f64);
    }
    content.append(&group);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let defaults = gtk::Button::with_label("Restore Defaults");
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::with_label("OK");
    ok.add_css_class("suggested-action");
    actions.append(&defaults);
    actions.append(&spacer);
    actions.append(&cancel);
    actions.append(&ok);
    content.append(&actions);
    window.set_child(Some(&content));
    {
        let spins = spins.clone();
        defaults.connect_clicked(move |_| {
            for (spin, value) in spins.iter().zip([15, 15, 0]) {
                spin.set_value(value.into());
            }
        });
    }
    let weak = window.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(window) = weak.upgrade() {
            window.close();
        }
    });
    let weak = window.downgrade();
    ok.connect_clicked(move |_| {
        {
            let mut settings = common::settings::values_mut();
            settings.touchscreen.diameter_x = spins[0].value_as_int() as u32;
            settings.touchscreen.diameter_y = spins[1].value_as_int() as u32;
            settings.touchscreen.rotation_angle = spins[2].value_as_int() as u32;
        }
        if let Some(window) = weak.upgrade() {
            window.close();
        }
    });
    crate::i18n::translate_widget_tree(&window);
    window
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display; run alone with --ignored"]
    fn touchscreen_dialog_apply_cancel_and_defaults() {
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
        {
            let mut settings = common::settings::values_mut();
            settings.touchscreen.diameter_x = 70;
            settings.touchscreen.diameter_y = 80;
            settings.touchscreen.rotation_angle = 90;
        }
        for accept in [false, true] {
            let window = build_dialog();
            let mut spins = Vec::<gtk::SpinButton>::new();
            let mut buttons = Vec::<gtk::Button>::new();
            collect(window.upcast_ref(), &mut spins);
            collect(window.upcast_ref(), &mut buttons);
            assert_eq!(
                spins.iter().map(|s| s.value_as_int()).collect::<Vec<_>>(),
                [70, 80, 90]
            );
            for spin in &spins {
                assert_eq!(spin.adjustment().lower(), 0.0);
                assert_eq!(spin.adjustment().upper(), 99.0);
            }
            let click = |label: &str| {
                buttons
                    .iter()
                    .find(|b| b.label().as_deref() == Some(label))
                    .unwrap()
                    .emit_clicked()
            };
            click("Restore Defaults");
            assert_eq!(
                spins.iter().map(|s| s.value_as_int()).collect::<Vec<_>>(),
                [15, 15, 0]
            );
            assert_eq!(common::settings::values().touchscreen.diameter_x, 70);
            click(if accept { "OK" } else { "Cancel" });
            let settings = common::settings::values();
            assert_eq!(
                settings.touchscreen.diameter_x,
                if accept { 15 } else { 70 }
            );
            assert_eq!(
                settings.touchscreen.diameter_y,
                if accept { 15 } else { 80 }
            );
            assert_eq!(
                settings.touchscreen.rotation_angle,
                if accept { 0 } else { 90 }
            );
        }
        crate::i18n::set_language("fr");
        let translated = build_dialog();
        assert_eq!(translated.title().as_deref(), Some("Configurer l'Écran Tactile"));
        let mut buttons = Vec::<gtk::Button>::new();
        collect(translated.upcast_ref(), &mut buttons);
        assert!(buttons.iter().any(|b| b.label().as_deref() == Some("Restaurer les paramètres par défaut")));
        crate::i18n::set_language("en");
        crate::i18n::translate_widget_tree(&translated);
        assert_eq!(translated.title().as_deref(), Some("Configure Touchscreen"));
        translated.close();
    }
}
