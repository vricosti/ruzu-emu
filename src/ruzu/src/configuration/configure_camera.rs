//! GTK counterpart of Eden yuzu/configuration/configure_camera.{h,cpp,ui}.
use crate::{camera_capture, i18n::tr};
use gtk::{glib, prelude::*};
use std::{cell::RefCell, rc::Rc};

pub fn present(button: &gtk::Button) {
    let parent = button
        .root()
        .and_then(|root| root.downcast::<gtk::Window>().ok());
    let devices = camera_capture::available_devices().map(|devices| {
        devices
            .into_iter()
            .map(|device| (device.name.clone(), device.setting_id()))
            .collect()
    });
    build(parent.as_ref(), devices).present();
}

// Keep enumeration outside widget construction so GUI regression tests never
// inspect camera hardware. Preview still opens only on an explicit click.
fn build(
    parent: Option<&gtk::Window>,
    devices: Result<Vec<(String, String)>, String>,
) -> gtk::Window {
    let window = gtk::Window::builder()
        .title(tr("Configure Infrared Camera"))
        .modal(true)
        .destroy_with_parent(true)
        .resizable(false)
        .build();
    window.set_transient_for(parent);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    for setter in [
        gtk::prelude::WidgetExt::set_margin_top,
        gtk::prelude::WidgetExt::set_margin_bottom,
        gtk::prelude::WidgetExt::set_margin_start,
        gtk::prelude::WidgetExt::set_margin_end,
    ] {
        setter(&content, 8);
    }
    let explanation = gtk::Label::new(Some(&tr("Select where the image of the emulated camera comes from. It may be a virtual camera or a real camera.")));
    explanation.set_wrap(true);
    explanation.set_max_width_chars(45);
    content.append(&explanation);
    let status = gtk::Label::new(None);
    status.set_wrap(true);
    status.set_max_width_chars(45);
    let current = common::settings::values()
        .ir_sensor_device
        .get_value()
        .clone();
    let mut choices = vec![(tr("Auto"), "auto".to_owned())];
    match devices {
        Ok(devices) => choices.extend(devices),
        Err(error) => status.set_text(&error),
    }
    // Preserve an obsolete imported Qt ID until the user explicitly replaces
    // it. Saving an unrelated setting must not select a different camera.
    if !choices.iter().any(|(_, id)| id == &current) {
        choices.push((format!("{}: {current}", tr("Unavailable")), current.clone()));
    }
    let labels: Vec<_> = choices.iter().map(|(label, _)| label.as_str()).collect();
    let selection = gtk::DropDown::from_strings(&labels);
    selection.set_selected(
        choices
            .iter()
            .position(|(_, id)| id == &current)
            .unwrap_or(0) as u32,
    );
    content.append(&super::shared_widget::labeled_row(
        "Input device:",
        &selection,
    ));
    let picture = gtk::Picture::new();
    picture.set_size_request(320, 240);
    content.append(&picture);
    let preview = gtk::Button::with_label(&tr("Click to preview"));
    content.append(&preview);
    content.append(&status);
    let timer = Rc::new(RefCell::new(None::<glib::SourceId>));
    let choices = Rc::new(choices);
    preview.connect_clicked(glib::clone!(
        #[weak]
        picture,
        #[weak]
        selection,
        #[weak]
        status,
        #[strong]
        timer,
        #[strong]
        choices,
        move |_| {
            if let Some(source) = timer.borrow_mut().take() {
                source.remove();
            }
            picture.set_paintable(None::<&gtk::gdk::Texture>);
            let Some((_, id)) = choices.get(selection.selected() as usize) else {
                return;
            };
            let mut capture = match camera_capture::Capture::open(id) {
                Ok(capture) => capture,
                Err(error) => {
                    status.set_text(&error);
                    return;
                }
            };
            status.set_text("");
            let source = glib::timeout_add_local(
                std::time::Duration::from_millis(250),
                glib::clone!(
                    #[weak]
                    picture,
                    #[weak]
                    status,
                    #[strong]
                    timer,
                    #[upgrade_or]
                    glib::ControlFlow::Break,
                    move || {
                        match capture.frame(320, 240) {
                            Ok(Some(pixels)) => {
                                // Guest images are vertically flipped. The Qt preview
                                // is not; undo only that orientation for presentation.
                                let bytes = preview_bytes(&pixels, 320);
                                let texture = gtk::gdk::MemoryTexture::new(
                                    320,
                                    240,
                                    gtk::gdk::MemoryFormat::B8g8r8a8,
                                    &glib::Bytes::from_owned(bytes),
                                    320 * 4,
                                );
                                picture.set_paintable(Some(&texture));
                            }
                            Ok(None) => {}
                            Err(error) => {
                                status.set_text(&error);
                                timer.borrow_mut().take();
                                return glib::ControlFlow::Break;
                            }
                        }
                        glib::ControlFlow::Continue
                    }
                ),
            );
            *timer.borrow_mut() = Some(source);
        }
    ));
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let restore = gtk::Button::with_label(&tr("Restore Defaults"));
    restore.connect_clicked(glib::clone!(
        #[weak]
        selection,
        move |_| selection.set_selected(0)
    ));
    let cancel = gtk::Button::with_label(&tr("Cancel"));
    cancel.connect_clicked(glib::clone!(
        #[weak]
        window,
        move |_| window.close()
    ));
    let ok = gtk::Button::with_label(&tr("OK"));
    ok.connect_clicked(glib::clone!(
        #[weak]
        selection,
        #[weak]
        window,
        #[strong]
        choices,
        move |_| {
            if let Some((_, id)) = choices.get(selection.selected() as usize) {
                common::settings::values_mut()
                    .ir_sensor_device
                    .set_value(id.clone());
            }
            window.close();
        }
    ));
    for button in [&restore, &cancel, &ok] {
        buttons.append(button);
    }
    content.append(&buttons);
    let hide_timer = Rc::clone(&timer);
    window.connect_hide(move |_| {
        if let Some(source) = hide_timer.borrow_mut().take() {
            source.remove();
        }
    });
    window.connect_close_request(move |_| {
        if let Some(source) = timer.borrow_mut().take() {
            source.remove();
        }
        glib::Propagation::Proceed
    });
    window.set_child(Some(&content));
    window
}

fn preview_bytes(pixels: &[u32], width: usize) -> Vec<u8> {
    pixels
        .chunks_exact(width)
        .rev()
        .flatten()
        .flat_map(|pixel| pixel.to_le_bytes())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn widgets<T: IsA<gtk::Widget> + glib::types::StaticType + Clone>(
        root: &gtk::Widget,
    ) -> Vec<T> {
        let mut result = Vec::new();
        if let Ok(widget) = root.clone().downcast::<T>() {
            result.push(widget);
        }
        let mut child = root.first_child();
        while let Some(widget) = child {
            result.extend(widgets::<T>(&widget));
            child = widget.next_sibling();
        }
        result
    }

    #[test]
    #[ignore = "requires a display; run alone, never enumerates or opens cameras"]
    fn camera_dialog_preserves_cancel_and_applies_explicit_selection() {
        gtk::init().unwrap();
        let original = "sdl-name:Synthetic camera";
        for accept in [false, true] {
            common::settings::values_mut()
                .ir_sensor_device
                .set_value(original.into());
            let window = build(None, Ok(vec![("Synthetic camera".into(), original.into())]));
            window.present();
            let selection = widgets::<gtk::DropDown>(window.upcast_ref()).remove(0);
            assert_eq!(selection.selected(), 1);
            let buttons = widgets::<gtk::Button>(window.upcast_ref());
            let click = |label: &str| {
                buttons
                    .iter()
                    .find(|button| button.label().as_deref() == Some(tr(label).as_str()))
                    .unwrap()
                    .emit_clicked()
            };
            click("Restore Defaults");
            assert_eq!(selection.selected(), 0);
            click(if accept { "OK" } else { "Cancel" });
            assert_eq!(
                common::settings::values().ir_sensor_device.get_value(),
                if accept { "auto" } else { original }
            );
            window.destroy();
        }
        common::settings::values_mut()
            .ir_sensor_device
            .set_value("obsolete-qt-id".into());
        let window = build(None, Ok(Vec::new()));
        assert_eq!(
            widgets::<gtk::DropDown>(window.upcast_ref())
                .remove(0)
                .selected(),
            1
        );
        window.destroy();
    }

    #[test]
    fn preview_undoes_guest_flip_and_has_explicit_byte_order() {
        assert_eq!(
            super::preview_bytes(&[0xff112233, 0xff445566], 1),
            [0x66, 0x55, 0x44, 0xff, 0x33, 0x22, 0x11, 0xff]
        );
    }
}
