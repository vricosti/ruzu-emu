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
