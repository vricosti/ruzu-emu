// SPDX-License-Identifier: GPL-3.0-or-later
//
// GTK counterpart of Eden `src/yuzu/install_dialog.{h,cpp}`.

use std::path::PathBuf;
use std::rc::Rc;

use gtk::prelude::*;

/// Confirm which of the files selected by the native chooser should be
/// installed. Upstream checks every file initially and returns only checked
/// paths from `InstallDialog::GetFiles`.
pub fn present(
    parent: &gtk::ApplicationWindow,
    files: Vec<PathBuf>,
    accepted: impl FnOnce(Vec<PathBuf>) + 'static,
) {
    let dialog = gtk::Dialog::builder()
        .title(crate::i18n::tr("Install Files to NAND"))
        .modal(true)
        .transient_for(parent)
        .default_width(520)
        .default_height(320)
        .build();

    let content = dialog.content_area();
    content.set_spacing(8);
    content.set_margin_top(8);
    content.set_margin_bottom(8);
    content.set_margin_start(8);
    content.set_margin_end(8);

    let description = gtk::Label::new(Some(&crate::i18n::tr(
        "Please confirm these are the files you wish to install.",
    )));
    description.set_xalign(0.0);
    content.append(&description);

    let update_description = gtk::Label::new(Some(&crate::i18n::tr(
        "Installing an Update or DLC will overwrite the previously installed one.",
    )));
    update_description.set_xalign(0.0);
    update_description.set_wrap(true);
    content.append(&update_description);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    let choices = files
        .into_iter()
        .map(|path| {
            let label = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
                .unwrap_or_else(|| path.to_string_lossy().into_owned());
            let check = gtk::CheckButton::with_label(&label);
            check.set_active(true);
            check.set_margin_top(4);
            check.set_margin_bottom(4);
            check.set_margin_start(8);
            check.set_margin_end(8);
            list.append(&check);
            (path, check)
        })
        .collect::<Vec<_>>();
    let choices = Rc::new(choices);

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&list)
        .build();
    content.append(&scroll);

    dialog.add_button(&crate::i18n::tr("Cancel"), gtk::ResponseType::Cancel);
    dialog.add_button(&crate::i18n::tr("Install"), gtk::ResponseType::Accept);

    let accepted = Rc::new(std::cell::RefCell::new(Some(accepted)));
    dialog.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept {
            let selected = choices
                .iter()
                .filter(|(_, check)| check.is_active())
                .map(|(path, _)| path.clone())
                .collect();
            if let Some(accepted) = accepted.borrow_mut().take() {
                accepted(selected);
            }
        }
        dialog.close();
    });
    dialog.present();
}
