// SPDX-License-Identifier: GPL-3.0-or-later
//! Boot-time UserSelector/General path of yuzu/applets/qt_profile_select.
//! Guest applet dispatch is separate from this startup preference.

use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

type Completion = Rc<RefCell<Option<Box<dyn FnOnce(Option<usize>)>>>>;

fn complete(callback: &Completion, index: Option<usize>) {
    let callback = callback.borrow_mut().take();
    if let Some(callback) = callback {
        callback(index);
    }
}

/// QtProfileSelectionDialog::exec for the boot selector. The single-user
/// bypass and Cancel outcome match upstream; GTK uses asynchronous completion.
pub fn select_for_boot(
    parent: &gtk::Window,
    hid: &Arc<parking_lot::Mutex<hid_core::hid_core::HIDCore>>,
    callback: impl FnOnce(Option<usize>) + 'static,
) {
    let manager = ruzu_core::hle::service::acc::profile_manager::ProfileManager::new();
    if manager.get_user_count() == 1 {
        callback(Some(0));
        return;
    }
    let dialog = gtk::Dialog::builder()
        .title(crate::i18n::tr("Profile Selector"))
        .transient_for(parent)
        .modal(true)
        .destroy_with_parent(true)
        .default_width(550)
        .default_height(400)
        .build();
    dialog.add_button(&crate::i18n::tr("Cancel"), gtk::ResponseType::Cancel);
    dialog.add_button(&crate::i18n::tr("OK"), gtk::ResponseType::Ok);
    dialog.set_default_response(gtk::ResponseType::Ok);
    let content = dialog.content_area();
    content.set_margin_top(8);
    content.set_margin_bottom(8);
    content.set_margin_start(8);
    content.set_margin_end(8);
    content.append(&gtk::Label::new(Some(&crate::i18n::tr("Select a user:"))));
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.set_activate_on_single_click(false);
    let mut indices = Vec::new();
    for index in 0..manager.get_user_count() {
        let Some(base) = manager.get_profile_base(Some(index)) else {
            continue;
        };
        let uuid = common::uuid::UUID::from_bytes(base.user_uuid);
        let end = base
            .username
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(base.username.len());
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let image = get_icon(uuid);
        row.append(&image);
        let label = gtk::Label::new(Some(&format!(
            "{}\n{}",
            String::from_utf8_lossy(&base.username[..end]),
            uuid.formatted_string()
        )));
        label.set_xalign(0.0);
        row.append(&label);
        list.append(&row);
        indices.push(index);
    }
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&list)
        .build();
    content.append(&scroll);
    list.select_row(list.row_at_index(0).as_ref());
    dialog.set_response_sensitive(gtk::ResponseType::Ok, !indices.is_empty());
    let callback: Completion = Rc::new(RefCell::new(Some(Box::new(callback))));
    dialog.connect_response({
        let callback = Rc::clone(&callback);
        let list = list.clone();
        move |dialog, response| {
            let selected = if response == gtk::ResponseType::Ok {
                list.selected_row()
                    .and_then(|row| indices.get(row.index() as usize).copied())
            } else {
                None
            };
            // Remove the continuation before close-request's rejecting fallback,
            // then dismiss the modal before starting the selected user's game.
            let callback = callback.borrow_mut().take();
            dialog.destroy();
            if let Some(callback) = callback {
                callback(selected);
            }
        }
    });
    dialog.connect_close_request(move |_| {
        complete(&callback, None);
        gtk::glib::Propagation::Proceed
    });
    list.connect_row_activated({
        let dialog = dialog.downgrade();
        move |_, _| {
            if let Some(dialog) = dialog.upgrade() {
                dialog.response(gtk::ResponseType::Ok);
            }
        }
    });
    let navigation = crate::util::controller_navigation::ControllerNavigation::new(hid);
    let weak = dialog.downgrade();
    gtk::glib::timeout_add_local(std::time::Duration::from_millis(30), move || {
        use crate::util::controller_navigation::NavigationKey;
        let Some(dialog) = weak.upgrade() else {
            return gtk::glib::ControlFlow::Break;
        };
        if !dialog.is_visible() {
            return gtk::glib::ControlFlow::Break;
        }
        if !dialog.is_active() {
            navigation.discard_pending_keys();
            return gtk::glib::ControlFlow::Continue;
        }
        for key in navigation.take_pending_keys() {
            match key {
                NavigationKey::Enter | NavigationKey::Escape => {
                    dialog.response(if key == NavigationKey::Enter {
                        gtk::ResponseType::Ok
                    } else {
                        gtk::ResponseType::Cancel
                    });
                    return gtk::glib::ControlFlow::Break;
                }
                NavigationKey::Up | NavigationKey::Down => {
                    let current = list.selected_row().map(|row| row.index()).unwrap_or(0);
                    let next = current + if key == NavigationKey::Up { -1 } else { 1 };
                    if let Some(row) = list.row_at_index(next) {
                        list.select_row(Some(&row));
                    }
                }
                _ => {}
            }
        }
        gtk::glib::ControlFlow::Continue
    });
    dialog.present();
}

/// QtProfileSelectionDialog GetIcon: custom avatar, then the account fallback.
fn get_icon(uuid: common::uuid::UUID) -> gtk::Picture {
    let path = common::fs::path_util::get_ruzu_path(common::fs::path_util::RuzuPath::NANDDir).join(
        format!(
            "system/save/8000000000000010/su/avators/{}.jpg",
            uuid.formatted_string()
        ),
    );
    let decode = |bytes: &[u8]| {
        use gtk::gdk_pixbuf::prelude::PixbufLoaderExt;
        let loader = gtk::gdk_pixbuf::PixbufLoader::new();
        loader.write(bytes).ok()?;
        loader.close().ok()?;
        let pixbuf =
            loader
                .pixbuf()?
                .scale_simple(64, 64, gtk::gdk_pixbuf::InterpType::Bilinear)?;
        Some(gtk::gdk::Texture::for_pixbuf(&pixbuf))
    };
    let texture = std::fs::read(path)
        .ok()
        .and_then(|bytes| decode(&bytes))
        .or_else(|| decode(&ruzu_core::constants::ACCOUNT_BACKUP_JPEG));
    let picture = gtk::Picture::new();
    picture.set_paintable(texture.as_ref());
    picture.set_size_request(64, 64);
    picture
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_or_cancel_completes_only_once() {
        for selected in [Some(2), None] {
            let output = Rc::new(RefCell::new(Vec::new()));
            let sink = Rc::clone(&output);
            let callback: Completion = Rc::new(RefCell::new(Some(Box::new(move |value| {
                sink.borrow_mut().push(value)
            }))));
            complete(&callback, selected);
            complete(&callback, None);
            assert_eq!(*output.borrow(), vec![selected]);
        }
    }
}
