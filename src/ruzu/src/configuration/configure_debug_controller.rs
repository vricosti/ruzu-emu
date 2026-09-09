// SPDX-License-Identifier: GPL-2.0-or-later
//! GTK counterpart of yuzu/configuration/configure_debug_controller.{h,cpp,ui}.

use std::{cell::RefCell, rc::Rc, sync::Arc};
use gtk::prelude::*;
use super::configure_input_player::{self, InputProfileContext};

pub fn present(
    source: &impl IsA<gtk::Widget>,
    input: Rc<RefCell<input_common::InputSubsystem>>,
    hid: Arc<parking_lot::Mutex<hid_core::hid_core::HIDCore>>,
    profiles: Rc<InputProfileContext>,
) {
    let dialog = gtk::Dialog::builder()
        .title(&crate::i18n::tr("Configure Debug Controller"))
        .modal(true).default_width(1100).default_height(720).build();
    if let Some(parent) = source.root().and_downcast::<gtk::Window>() {
        dialog.set_transient_for(Some(&parent));
    }
    let page = configure_input_player::page(9, input, hid, profiles, None, true);
    // Clear/Defaults already belong to the shared player page in GTK.
    dialog.content_area().append(&page.widget);
    dialog.add_button(&crate::i18n::tr("Cancel"), gtk::ResponseType::Cancel);
    dialog.add_button(&crate::i18n::tr("OK"), gtk::ResponseType::Accept);
    dialog.set_default_response(gtk::ResponseType::Accept);
    let page = RefCell::new(Some(page));
    dialog.connect_response(move |dialog, response| {
        if let Some(page) = page.borrow_mut().take() {
            if response == gtk::ResponseType::Accept { (page.apply)(); }
            // Drop leaves configuration mode even if GTK retains the window.
        }
        dialog.close();
    });
    dialog.present();
}
