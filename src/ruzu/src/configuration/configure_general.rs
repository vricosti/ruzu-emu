// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of
// `/home/vricosti/Dev/emulators/zuyu/src/yuzu/configuration/configure_general.cpp`
// (`ConfigureGeneral`), whose widget tree lives in `configure_general.ui`.
//
// The page holds two groups:
//   * "General" — the emulation-behaviour settings followed by the frontend
//     Gamemode and X11 rows in upstream setting-id order;
//   * "External Content" — host directories scanned for updates and DLC.
// plus a "Reset All Settings" button pinned to the bottom.
//
// Upstream builds these rows generically through
// `ConfigurationShared::Builder::BuildWidget`, driven by the settings registry.
// The Rust port constructs them explicitly (see `shared_widget`'s module note),
// so the row list here is the literal contents of `configure_general.ui`.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

use crate::uisettings;

use super::configure_dialog::Page;
use super::shared_translation as tr;
use super::shared_widget as w;

/// Build the General tab — upstream `ConfigureGeneral`.
pub fn page() -> Page {
    let (scroller, column) = w::page();

    // --- "General" group -------------------------------------------------
    let (general_group, general) = w::group("General");

    let confirm_index =
        uisettings::with(|v| tr::index_of(tr::CONFIRM_STOP, v.confirm_before_stopping.get_value()));
    let (confirm_row, confirm) = w::combo_row(
        "Confirm before stopping emulation",
        &tr::labels(tr::CONFIRM_STOP),
        confirm_index,
    );
    general.append(&confirm_row);

    let pause_background = w::check_row(
        "Pause emulation when in background",
        uisettings::with(|v| *v.pause_when_in_background.get_value()),
    );
    general.append(&pause_background);

    let hide_mouse = w::check_row(
        "Hide mouse on inactivity",
        uisettings::with(|v| *v.hide_mouse.get_value()),
    );
    general.append(&hide_mouse);

    let disable_controller_applet = w::check_row(
        "Disable controller applet",
        uisettings::with(|v| *v.controller_applet_disabled.get_value()),
    );
    general.append(&disable_controller_applet);

    let select_user_on_boot = w::check_row(
        "Prompt for user on game boot",
        uisettings::with(|v| *v.select_user_on_boot.get_value()),
    );
    general.append(&select_user_on_boot);

    let enable_gamemode = w::check_row(
        "Enable Gamemode",
        uisettings::with(|values| *values.enable_gamemode.get_value()),
    );
    general.append(&enable_gamemode);

    #[cfg(target_os = "linux")]
    let force_x11 = {
        let force_x11 = w::check_row(
            "Force X11 as Graphics Backend",
            uisettings::with(|values| *values.gui_force_x11.get_value()),
        );
        general.append(&force_x11);
        force_x11
    };

    column.append(&general_group);

    // --- "External Content" group ---------------------------------------
    let (external_group, external) = w::group("External Content");
    let description = gtk::Label::new(Some(&crate::i18n::tr(
        "Add directories to scan for DLCs and Updates without installing to NAND",
    )));
    description.set_xalign(0.0);
    description.set_wrap(true);
    external.append(&description);

    let external_dirs = Rc::new(RefCell::new(
        common::settings::values().external_content_dirs.clone(),
    ));
    let external_list = gtk::ListBox::new();
    external_list.set_selection_mode(gtk::SelectionMode::Single);
    external_list.add_css_class("boxed-list");
    for directory in external_dirs.borrow().iter() {
        append_external_directory_row(&external_list, directory);
    }
    let external_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(170)
        .hexpand(true)
        .vexpand(true)
        .child(&external_list)
        .build();
    external_scroll.set_has_frame(true);
    external.append(&external_scroll);

    let add_external = gtk::Button::with_label(&crate::i18n::tr("Add Directory"));
    let remove_external = gtk::Button::with_label(&crate::i18n::tr("Remove Selected"));
    remove_external.set_sensitive(false);
    let external_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    external_buttons.append(&add_external);
    external_buttons.append(&remove_external);
    external.append(&external_buttons);

    external_list.connect_row_selected(glib::clone!(
        #[weak]
        remove_external,
        move |_, row| remove_external.set_sensitive(row.is_some())
    ));
    remove_external.connect_clicked(glib::clone!(
        #[weak]
        external_list,
        #[strong]
        external_dirs,
        move |_| {
            let Some(row) = external_list.selected_row() else {
                return;
            };
            let index = row.index() as usize;
            external_list.remove(&row);
            if index < external_dirs.borrow().len() {
                external_dirs.borrow_mut().remove(index);
            }
        }
    ));
    add_external.connect_clicked(glib::clone!(
        #[weak]
        external_list,
        #[strong]
        external_dirs,
        move |button| {
            let parent = button.root().and_downcast::<gtk::Window>();
            let external_list = external_list.clone();
            let external_dirs = Rc::clone(&external_dirs);
            crate::gtk_compat::select_folder(
                parent.as_ref(),
                "Select External Content Directory...",
                move |selected| {
                    let Some(path) = selected.and_then(|file| file.path()) else {
                        return;
                    };
                    let path = normalize_external_directory(&path);
                    if external_dirs.borrow().contains(&path) {
                        let parent = external_list.root().and_downcast::<gtk::Window>();
                        crate::gtk_compat::show_message(
                            parent.as_ref(),
                            "Directory Already Added",
                            "This directory is already in the list.",
                        );
                        return;
                    }
                    external_dirs.borrow_mut().push(path.clone());
                    append_external_directory_row(&external_list, &path);
                },
            );
        }
    ));
    column.append(&external_group);

    // --- "Reset All Settings" --------------------------------------------
    // Upstream pins this button bottom-left of the page (a `QSpacerItem` sits
    // between it and the groups above), so it sits below the scrolling column
    // rather than immediately after the Linux group.
    let reset = gtk::Button::with_label("Reset All Settings");
    reset.set_halign(gtk::Align::Start);
    reset.set_margin_top(8);
    reset.set_margin_bottom(10);
    reset.set_margin_start(10);
    reset.connect_clicked(|_| {
        // Upstream pops a confirmation, resets `Settings` + `UISettings` to
        // their defaults, and closes the dialog via the callback installed by
        // `ConfigureDialog` (`general_tab->SetResetCallback`). Wiring the reset
        // itself needs the config writer, which is a separate slice; log until
        // then rather than silently doing nothing.
        log::info!("Reset All Settings requested (config writer not yet wired)");
    });

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&scroller);
    root.append(&reset);

    Page::new("General", root, move || {
        let confirm_value = tr::value_at(tr::CONFIRM_STOP, confirm.selected());
        let pause = pause_background.is_active();
        let hide = hide_mouse.is_active();
        let no_controller_applet = disable_controller_applet.is_active();
        let select_user = select_user_on_boot.is_active();
        let gamemode = enable_gamemode.is_active();
        #[cfg(target_os = "linux")]
        let use_x11 = force_x11.is_active();

        uisettings::with_mut(|v| {
            v.confirm_before_stopping.set_value(confirm_value);
            v.pause_when_in_background.set_value(pause);
            v.hide_mouse.set_value(hide);
            v.controller_applet_disabled.set_value(no_controller_applet);
            v.select_user_on_boot.set_value(select_user);
            v.enable_gamemode.set_value(gamemode);
            #[cfg(target_os = "linux")]
            v.gui_force_x11.set_value(use_x11);
        });

        let new_external_dirs = external_dirs.borrow().clone();
        let changed = {
            let mut settings = common::settings::values_mut();
            if settings.external_content_dirs == new_external_dirs {
                false
            } else {
                settings.external_content_dirs = new_external_dirs.clone();
                true
            }
        };
        if changed {
            crate::util::game::reset_metadata(None, false);
            uisettings::request_game_list_reload();
        }
    })
}

fn append_external_directory_row(list: &gtk::ListBox, directory: &str) {
    let label = gtk::Label::new(Some(directory));
    label.set_xalign(0.0);
    label.set_margin_top(4);
    label.set_margin_bottom(4);
    label.set_margin_start(6);
    label.set_margin_end(6);
    label.set_selectable(false);
    list.append(&label);
}

fn normalize_external_directory(path: &std::path::Path) -> String {
    let mut normalized = path.to_string_lossy().into_owned();
    if !normalized.ends_with(std::path::MAIN_SEPARATOR) {
        normalized.push(std::path::MAIN_SEPARATOR);
    }
    normalized
}
