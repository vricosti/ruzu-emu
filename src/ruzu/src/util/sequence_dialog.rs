// SPDX-License-Identifier: GPL-3.0-or-later
//
// GTK counterpart of
// `/home/vricosti/Dev/emulators/eden/src/yuzu/util/sequence_dialog/sequence_dialog.cpp`.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;

/// Show the single-key sequence editor and return the accepted native label.
/// Upstream deliberately keeps only the first chord entered by
/// `QKeySequenceEdit`; GTK captures exactly one chord here.
pub fn present(source: &impl IsA<gtk::Widget>, accepted: impl FnOnce(String) + 'static) {
    let dialog = gtk::Dialog::builder()
        .title(&crate::i18n::tr("Enter a hotkey"))
        .modal(true)
        .default_width(360)
        .build();
    if let Some(parent) = source.root().and_downcast::<gtk::Window>() {
        dialog.set_transient_for(Some(&parent));
    }

    let sequence = gtk::Entry::builder()
        .editable(false)
        .can_focus(true)
        .placeholder_text(crate::i18n::tr("Press a key or key combination"))
        .hexpand(true)
        .build();
    sequence.set_margin_top(12);
    sequence.set_margin_bottom(12);
    sequence.set_margin_start(12);
    sequence.set_margin_end(12);
    dialog.content_area().append(&sequence);

    dialog.add_button(&crate::i18n::tr("Cancel"), gtk::ResponseType::Cancel);
    let ok = dialog.add_button(&crate::i18n::tr("OK"), gtk::ResponseType::Accept);
    ok.set_sensitive(false);
    dialog.set_default_response(gtk::ResponseType::Accept);

    let captured = Rc::new(RefCell::new(None::<String>));
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed({
        let captured = Rc::clone(&captured);
        let sequence = sequence.clone();
        let ok = ok.clone();
        move |_, keyval, _keycode, state| {
            if is_modifier_key(keyval) {
                return gtk::glib::Propagation::Stop;
            }
            let modifiers = state
                & (gtk::gdk::ModifierType::SHIFT_MASK
                    | gtk::gdk::ModifierType::CONTROL_MASK
                    | gtk::gdk::ModifierType::ALT_MASK
                    | gtk::gdk::ModifierType::SUPER_MASK
                    | gtk::gdk::ModifierType::META_MASK);
            let label = gtk::accelerator_get_label(keyval, modifiers).to_string();
            sequence.set_text(&label);
            *captured.borrow_mut() = Some(label);
            ok.set_sensitive(true);
            gtk::glib::Propagation::Stop
        }
    });
    dialog.add_controller(keys);

    let accepted = Rc::new(RefCell::new(Some(
        Box::new(accepted) as Box<dyn FnOnce(String)>
    )));
    dialog.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept {
            if let (Some(sequence), Some(accepted)) =
                (captured.borrow_mut().take(), accepted.borrow_mut().take())
            {
                accepted(sequence);
            }
        }
        dialog.close();
    });

    dialog.present();
    sequence.grab_focus();
}

fn is_modifier_key(key: gtk::gdk::Key) -> bool {
    matches!(
        key,
        gtk::gdk::Key::Shift_L
            | gtk::gdk::Key::Shift_R
            | gtk::gdk::Key::Control_L
            | gtk::gdk::Key::Control_R
            | gtk::gdk::Key::Alt_L
            | gtk::gdk::Key::Alt_R
            | gtk::gdk::Key::Meta_L
            | gtk::gdk::Key::Meta_R
            | gtk::gdk::Key::Super_L
            | gtk::gdk::Key::Super_R
            | gtk::gdk::Key::Hyper_L
            | gtk::gdk::Key::Hyper_R
            | gtk::gdk::Key::ISO_Level3_Shift
    )
}
