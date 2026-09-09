// SPDX-License-Identifier: GPL-3.0-or-later
//! GTK counterpart of yuzu/configuration/configure_debug_controller.{h,cpp,ui}.

use super::configure_input_player::{self, InputProfileContext};
use gtk::prelude::*;
use std::{cell::RefCell, rc::Rc, sync::Arc};

pub fn present(
    source: &impl IsA<gtk::Widget>,
    input: Rc<RefCell<input_common::InputSubsystem>>,
    hid: Arc<parking_lot::Mutex<hid_core::hid_core::HIDCore>>,
    profiles: Rc<InputProfileContext>,
) {
    let window = gtk::Window::builder()
        .title("Configure Debug Controller")
        .modal(true)
        .default_width(1100)
        .default_height(750)
        .build();
    if let Some(parent) = source.root().and_downcast::<gtk::Window>() {
        window.set_transient_for(Some(&parent));
        window.set_destroy_with_parent(true);
        // The parent's GObject can outlive its native window too.
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
    let page = configure_input_player::page(9, input, hid, profiles, None, true);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(8);
    content.set_margin_bottom(8);
    content.set_margin_start(8);
    content.set_margin_end(8);
    content.append(&page.widget);
    // Clear/Defaults already belong to the embedded player page in GTK.
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::with_label("OK");
    ok.add_css_class("suggested-action");
    actions.append(&cancel);
    actions.append(&ok);
    content.append(&actions);
    window.set_child(Some(&content));
    // Release the embedded controller configuration on close, even if GTK
    // keeps the destroyed window's callbacks alive until its next main-loop turn.
    let owner = Rc::new(RefCell::new(Some(page)));
    let close_owner = Rc::clone(&owner);
    window.connect_close_request(move |_| {
        close_owner.borrow_mut().take();
        gtk::glib::Propagation::Proceed
    });
    // gtk_window_destroy tears down the native window without necessarily
    // disposing its GObject immediately. Release on that native teardown too.
    let unrealize_owner = Rc::clone(&owner);
    window.connect_unrealize(move |_| {
        unrealize_owner.borrow_mut().take();
    });
    let weak = window.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(window) = weak.upgrade() {
            window.close();
        }
    });
    let weak = window.downgrade();
    ok.connect_clicked(move |_| {
        if let Some(page) = owner.borrow().as_ref() {
            (page.apply)();
        }
        if let Some(window) = weak.upgrade() {
            window.close();
        }
    });
    window.present();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display; run alone with --ignored"]
    fn debug_dialog_releases_configuration_on_every_close_path() {
        fn button(widget: &gtk::Widget, label: &str) -> Option<gtk::Button> {
            if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                if button.label().as_deref() == Some(label) {
                    return Some(button.clone());
                }
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                if let Some(found) = button(&widget, label) {
                    return Some(found);
                }
                child = widget.next_sibling();
            }
            None
        }
        gtk::init().unwrap();
        let input = Rc::new(RefCell::new(input_common::InputSubsystem::new()));
        let hid = Arc::new(parking_lot::Mutex::new(hid_core::hid_core::HIDCore::new()));
        let other = hid.lock().get_emulated_controller_by_index(9);
        let profiles = Rc::new(InputProfileContext::new(
            super::super::input_profiles::InputProfiles::new(),
        ));
        for action in ["Cancel", "OK", "close", "destroy", "parent destroy"] {
            let parent = gtk::Window::new();
            let source = gtk::Button::new();
            parent.set_child(Some(&source));
            parent.present();
            present(
                &source,
                Rc::clone(&input),
                Arc::clone(&hid),
                Rc::clone(&profiles),
            );
            let window = gtk::Window::list_toplevels()
                .into_iter()
                .filter_map(|widget| widget.downcast::<gtk::Window>().ok())
                .find(|window| window.title().as_deref() == Some("Configure Debug Controller"))
                .unwrap();
            assert!(other.lock().is_configuring_mode());
            match action {
                "close" => window.close(),
                "destroy" => window.destroy(),
                "parent destroy" => parent.destroy(),
                label => button(window.upcast_ref(), label).unwrap().emit_clicked(),
            }
            assert!(
                !other.lock().is_configuring_mode(),
                "configuration retained after {action}"
            );
            parent.destroy();
        }
    }
}
