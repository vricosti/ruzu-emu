// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of
// `eden/src/yuzu/configuration/configure_tas.{h,cpp,ui}`.

use gtk::glib;
use gtk::prelude::*;

pub fn present(parent: &gtk::Window) {
    let window = gtk::Window::builder()
        .title(crate::i18n::tr("TAS Configuration"))
        .transient_for(parent)
        .modal(true)
        .resizable(false)
        .default_width(560)
        .build();

    let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);

    let information = gtk::Box::new(gtk::Orientation::Vertical, 10);
    for text in [
        "Reads controller input from scripts in the same format as TAS-nx scripts.\nFor a more detailed explanation, please consult the user handbook.",
        "To check which hotkeys control the playback/recording, please refer to the Hotkey settings (Configure -> General -> Hotkeys).",
        "WARNING: This is an experimental feature.\nIt will not play back scripts frame perfectly with the current, imperfect syncing method.",
    ] {
        let label = gtk::Label::new(Some(&crate::i18n::tr(text)));
        label.set_wrap(true);
        label.set_xalign(0.0);
        information.append(&label);
    }
    let settings = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let path = common::fs::path_util::get_ruzu_path_string(common::fs::path_util::RuzuPath::TASDir);
    let (path_row, path_entry, browse) = super::shared_widget::path_row("Path", &path);
    browse.set_label("...");
    for (title, child) in [("TAS", &information), ("Settings", &settings), ("Script Directory", &path_row)] {
        let frame = gtk::Frame::new(Some(&crate::i18n::tr(title)));
        child.set_margin_top(10);
        child.set_margin_bottom(10);
        child.set_margin_start(10);
        child.set_margin_end(10);
        frame.set_child(Some(child));
        content.append(&frame);
    }

    let values = common::settings::values();
    let enabled = gtk::CheckButton::with_label(&crate::i18n::tr("Enable TAS features"));
    enabled.set_active(*values.tas_enable.get_value());
    let loop_script = gtk::CheckButton::with_label(&crate::i18n::tr("Loop script"));
    loop_script.set_active(*values.tas_loop.get_value());
    let pause_on_load =
        gtk::CheckButton::with_label(&crate::i18n::tr("Pause execution during loads"));
    pause_on_load.set_active(*values.pause_tas_on_load.get_value());
    pause_on_load.set_sensitive(false);
    let show_recording_dialog =
        gtk::CheckButton::with_label(&crate::i18n::tr("Show recording dialog"));
    show_recording_dialog.set_active(*values.tas_show_recording_dialog.get_value());
    drop(values);
    settings.append(&enabled);
    settings.append(&loop_script);
    settings.append(&pause_on_load);
    settings.append(&show_recording_dialog);

    browse.connect_clicked(glib::clone!(
        #[weak]
        window,
        #[weak]
        path_entry,
        move |_| {
            let chooser = gtk::FileChooserNative::new(
                Some(&crate::i18n::tr("Select TAS Load Directory...")),
                Some(&window),
                gtk::FileChooserAction::SelectFolder,
                Some(&crate::i18n::tr("Select")),
                Some(&crate::i18n::tr("Cancel")),
            );
            chooser.set_modal(true);
            let _ = chooser.set_current_folder(Some(&gtk::gio::File::for_path(path_entry.text().as_str())));
            let keep_alive = chooser.clone();
            chooser.run_async(move |chooser, response| {
                if response == gtk::ResponseType::Accept {
                    if let Some(path) = chooser.file().and_then(|folder| folder.path()) {
                        path_entry.set_text(&directory_text(&path));
                    }
                }
                chooser.destroy();
                drop(keep_alive);
            });
        }
    ));

    let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    spacer.set_size_request(-1, 40);
    spacer.set_vexpand(true);
    content.append(&spacer);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label(&crate::i18n::tr("Cancel"));
    let ok = gtk::Button::with_label(&crate::i18n::tr("OK"));
    ok.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&ok);
    content.append(&buttons);
    window.set_child(Some(&content));

    cancel.connect_clicked(glib::clone!(
        #[weak]
        window,
        move |_| window.close()
    ));
    ok.connect_clicked(glib::clone!(
        #[weak]
        window,
        move |_| {
            common::fs::path_util::set_ruzu_path(
                common::fs::path_util::RuzuPath::TASDir,
                std::path::Path::new(path_entry.text().as_str()),
            );
            let mut values = common::settings::values_mut();
            values.tas_enable.set_value(enabled.is_active());
            values.tas_loop.set_value(loop_script.is_active());
            values
                .pause_tas_on_load
                .set_value(pause_on_load.is_active());
            values.tas_show_recording_dialog.set_value(show_recording_dialog.is_active());
            drop(values);
            if let Err(error) = super::qt_config::save_tas_values() {
                log::error!("Failed to save TAS configuration: {error}");
            }
            window.close();
        }
    ));
    window.present();
}

// ConfigureTasDialog::SetDirectory retains a trailing slash in the edit field.
fn directory_text(path: &std::path::Path) -> String {
    let mut text = path.to_string_lossy().into_owned();
    if !text.ends_with('/') {
        text.push('/');
    }
    text
}

#[cfg(test)]
mod tests {
    #[test]
    fn directory_selection_appends_exactly_one_slash() {
        use std::path::Path;
        assert_eq!(super::directory_text(Path::new("/tas")), "/tas/");
        assert_eq!(super::directory_text(Path::new("/tas/")), "/tas/");
        assert_eq!(super::directory_text(Path::new("/")), "/");
    }
}
