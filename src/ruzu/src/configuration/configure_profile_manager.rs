// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of
// `/home/vricosti/Dev/emulators/zuyu/src/yuzu/configuration/configure_profile_manager.cpp`
// (`ConfigureProfileManager`), whose widget tree lives in
// `configure_profile_manager.ui`.
//
// A "Profile Manager" group with the current user (avatar + name), a list of
// users (avatar, name, UUID), the Set Image / Add / Rename / Remove buttons, and
// the "Profile management is available only when game is not running." note.
//
// The users come from `Service::Account::ProfileManager`, which ruzu ports as
// `ruzu_core::hle::service::acc::profile_manager`.

use gtk::prelude::*;
use ruzu_core::hle::service::acc::profile_manager::{
    ProfileManager, MAX_USERS, PROFILE_USERNAME_SIZE,
};
use std::rc::Rc;

use super::configure_dialog::Page;
use super::shared_widget as w;

/// Avatar size in the list and next to the current user, matching upstream.
const AVATAR_SIZE: i32 = 64;

/// Build the Profiles tab — upstream `ConfigureProfileManager`.
pub fn page(editing_enabled: bool) -> Page {
    let (scroller, column) = w::page();

    let (group, content) = w::group("Profile Manager");
    group.set_sensitive(editing_enabled);

    let profiles = load_profiles();
    let current_index = *common::settings::values().current_user.get_value();
    let current = usize::try_from(current_index)
        .ok()
        .and_then(|i| profiles.get(i));

    // --- "Current User" row ----------------------------------------------
    let current_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let current_label = gtk::Label::new(Some("Current User"));
    current_label.set_xalign(0.0);
    current_label.set_hexpand(true);
    current_row.append(&current_label);
    let current_icon = avatar(
        current.and_then(|profile| profile.icon.as_ref()),
        AVATAR_SIZE,
    );
    current_row.append(&current_icon);
    let current_name = gtk::Label::new(Some(
        current.map(|p| p.username.as_str()).unwrap_or_default(),
    ));
    current_name.set_xalign(0.0);
    current_name.set_width_chars(20);
    current_row.append(&current_name);
    content.append(&current_row);

    // --- "Users" list -----------------------------------------------------
    let users_frame = gtk::Frame::new(Some("Users"));
    users_frame.set_vexpand(true);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    populate_user_list(&list, &profiles);

    let list_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .min_content_height(320)
        .child(&list)
        .build();
    users_frame.set_child(Some(&list_scroll));
    content.append(&users_frame);

    // --- Buttons ----------------------------------------------------------
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let set_image = gtk::Button::with_label("Set Image");
    // Upstream keeps Set Image / Rename / Remove disabled until a row is picked.
    set_image.set_sensitive(false);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let add = gtk::Button::with_label("Add");
    add.set_sensitive(profiles.len() < MAX_USERS);
    let rename = gtk::Button::with_label("Rename");
    rename.set_sensitive(false);
    let remove = gtk::Button::with_label("Remove");
    remove.set_sensitive(false);
    buttons.append(&set_image);
    buttons.append(&spacer);
    buttons.append(&add);
    buttons.append(&rename);
    buttons.append(&remove);
    content.append(&buttons);

    // Match upstream's `SetSelectedUser`, which enables the per-user buttons
    // once a row is selected.
    {
        let set_image = set_image.clone();
        let rename = rename.clone();
        let remove = remove.clone();
        let current_name = current_name.clone();
        let current_icon = current_icon.clone();
        list.connect_row_selected(move |_, row| {
            let selected = row.is_some();
            set_image.set_sensitive(selected);
            rename.set_sensitive(selected);
            remove.set_sensitive(selected);
            if let Some(row) = row {
                if !editing_enabled {
                    return;
                }
                common::settings::values_mut()
                    .current_user
                    .set_value(row.index());
                update_current_user(&current_name, &current_icon);
            }
        });
    }

    let note = gtk::Label::new(Some(
        "Profile management is available only when game is not running.",
    ));
    note.set_xalign(0.0);
    note.set_margin_top(4);
    content.append(&note);

    column.append(&group);

    // GTK owns a disk-backed snapshot rather than borrowing the emulation
    // thread's ProfileManager. Editing is disabled while it can be in use.
    let refresh: Rc<dyn Fn()> = {
        let list = list.downgrade();
        let add = add.downgrade();
        Rc::new(move || {
            if let Some(list) = list.upgrade() {
                let profiles = load_profiles();
                populate_user_list(&list, &profiles);
                if let Some(add) = add.upgrade() {
                    add.set_sensitive(profiles.len() < MAX_USERS);
                }
                update_current_user(&current_name, &current_icon);
            }
        })
    };
    for (button, editing) in [(&add, false), (&rename, true)] {
        let list = list.downgrade();
        let refresh = refresh.clone();
        button.connect_clicked(move |button| {
            if !editing_enabled {
                return;
            }
            let profile = if editing {
                let Some(list) = list.upgrade() else {
                    return;
                };
                let Some(row) = list.selected_row() else {
                    return;
                };
                let manager = ProfileManager::new();
                manager.get_profile_base(Some(row.index() as usize))
            } else {
                None
            };
            if editing && profile.is_none() {
                return;
            }
            edit_user(button, profile, refresh.clone());
        });
    }
    {
        let list = list.downgrade();
        let refresh = refresh.clone();
        remove.connect_clicked(move |button| {
            if !editing_enabled {
                return;
            }
            let Some(list) = list.upgrade() else {
                return;
            };
            let Some(row) = list.selected_row() else {
                return;
            };
            let manager = ProfileManager::new();
            let Some(uuid) = manager.get_user(row.index() as usize) else {
                return;
            };
            let parent = button.root().and_downcast::<gtk::Window>();
            let refresh = refresh.clone();
            crate::gtk_compat::ask_question(
                parent.as_ref(),
                "Remove User",
                "Remove the selected user profile?",
                "Cancel",
                "Remove",
                move |accepted| {
                    if !accepted {
                        return;
                    }
                    let mut manager = ProfileManager::new();
                    if manager.remove_user(uuid) {
                        manager.write_user_save_file();
                        common::settings::values_mut().current_user.set_value(0);
                        refresh();
                    }
                },
            );
        });
    }
    {
        let list = list.downgrade();
        set_image.connect_clicked(move |button| {
            if !editing_enabled {
                return;
            }
            let Some(list) = list.upgrade() else {
                return;
            };
            let Some(row) = list.selected_row() else {
                return;
            };
            let Some(uuid) = ProfileManager::new().get_user(row.index() as usize) else {
                return;
            };
            let parent = button.root().and_downcast::<gtk::Window>();
            let filter = gtk::FileFilter::new();
            filter.add_pixbuf_formats();
            let refresh = refresh.clone();
            let error_parent = parent.clone();
            crate::gtk_compat::open_file(
                parent.as_ref(),
                "Set Image",
                &[filter],
                None,
                move |file| {
                    let Some(path) = file.and_then(|file| file.path()) else {
                        return;
                    };
                    let result =
                        save_image(&path, common::uuid::UUID::from_bytes(uuid.to_le_bytes()));
                    if let Err(error) = result {
                        crate::gtk_compat::show_error(
                            error_parent.as_ref(),
                            "Error saving user image",
                            &error.to_string(),
                        );
                    } else {
                        refresh();
                    }
                },
            );
        });
    }

    Page::new("Profiles", scroller, || {
        // Upstream's `ApplyConfiguration` only writes `current_user`, which is
        // changed by picking a row rather than by OK; nothing to flush here.
    })
}

/// Upstream PopulateUserList: preserve account order, names, UUIDs and icons.
fn populate_user_list(list: &gtk::ListBox, profiles: &[Profile]) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    for profile in profiles {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.set_margin_top(4);
        row.set_margin_bottom(4);
        row.set_margin_start(6);
        row.append(&avatar(profile.icon.as_ref(), AVATAR_SIZE));
        let text = gtk::Label::new(Some(&format!(
            "{}\n{}",
            profile.username, profile.user_uuid
        )));
        text.set_xalign(0.0);
        row.append(&text);
        list.append(&row);
    }
}

fn update_current_user(name: &gtk::Label, icon: &gtk::Picture) {
    let profiles = load_profiles();
    let index = *common::settings::values().current_user.get_value() as usize;
    if let Some(profile) = profiles.get(index) {
        name.set_text(&profile.username);
        icon.set_paintable(profile.icon.as_ref());
    }
}

// GTK's name-only editor replaces the Qt NewUserDialog. The existing separate
// Set Image action owns image selection; UUIDs are generated, never borrowed
// from another user. Limit UTF-8 bytes to the fixed account name payload.
fn edit_user(
    source: &gtk::Button,
    profile: Option<ruzu_core::hle::service::acc::profile_manager::ProfileBase>,
    refresh: Rc<dyn Fn()>,
) {
    let dialog = gtk::Dialog::builder()
        .title(if profile.is_some() {
            "Rename User"
        } else {
            "Add User"
        })
        .modal(true)
        .resizable(false)
        .build();
    if let Some(parent) = source.root().and_downcast::<gtk::Window>() {
        dialog.set_transient_for(Some(&parent));
        dialog.set_destroy_with_parent(true);
    }
    dialog.add_button(&crate::i18n::tr("Cancel"), gtk::ResponseType::Cancel);
    dialog.add_button(&crate::i18n::tr("OK"), gtk::ResponseType::Accept);
    let entry = gtk::Entry::new();
    entry.set_max_length(PROFILE_USERNAME_SIZE as i32);
    entry.set_activates_default(true);
    if let Some(profile) = profile {
        let length = profile
            .username
            .iter()
            .position(|b| *b == 0)
            .unwrap_or(PROFILE_USERNAME_SIZE);
        entry.set_text(&String::from_utf8_lossy(&profile.username[..length]));
    }
    for widget in [entry.upcast_ref::<gtk::Widget>()] {
        widget.set_margin_top(8);
        widget.set_margin_bottom(8);
        widget.set_margin_start(8);
        widget.set_margin_end(8);
    }
    dialog.content_area().append(&entry);
    dialog.set_default_response(gtk::ResponseType::Accept);
    dialog.set_response_sensitive(gtk::ResponseType::Accept, valid_username(&entry.text()));
    let weak = dialog.downgrade();
    entry.connect_changed(move |entry| {
        if let Some(dialog) = weak.upgrade() {
            dialog.set_response_sensitive(gtk::ResponseType::Accept, valid_username(&entry.text()));
        }
    });
    dialog.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept {
            if !valid_username(&entry.text()) {
                return;
            }
            let mut name = [0; PROFILE_USERNAME_SIZE];
            let text = entry.text();
            name[..text.len()].copy_from_slice(text.as_bytes());
            let mut manager = ProfileManager::new();
            let success = if let Some(mut profile) = profile {
                profile.username = name;
                manager.set_profile_base(u128::from_le_bytes(profile.user_uuid), &profile)
            } else {
                manager.create_new_user(common::uuid::UUID::make_random().as_u128(), &name)
                    == ruzu_core::hle::result::RESULT_SUCCESS
            };
            if !success {
                crate::gtk_compat::show_error(
                    Some(dialog),
                    "User Profile",
                    "Unable to update the user profile.",
                );
                return;
            }
            manager.write_user_save_file();
            refresh();
        }
        dialog.close();
    });
    crate::i18n::translate_widget_tree(&dialog);
    dialog.present();
}

fn valid_username(name: &str) -> bool {
    !name.is_empty() && name.len() <= PROFILE_USERNAME_SIZE && !name.contains('\0')
}

/// Upstream saveImage: encode the selected image as a quality-100 JPEG in the
/// account avatar directory. Decode first so an invalid input cannot overwrite
/// an existing avatar. Conflicting non-directory account paths are reported,
/// not removed implicitly by this GTK frontend.
fn save_image(
    source: &std::path::Path,
    uuid: common::uuid::UUID,
) -> Result<(), Box<dyn std::error::Error>> {
    let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_file(source)?;
    let destination = image_path(uuid);
    std::fs::create_dir_all(destination.parent().unwrap())?;
    pixbuf.savev(destination, "jpeg", &[("quality", "100")])?;
    Ok(())
}

fn avatar(icon: Option<&gtk::gdk::Texture>, size: i32) -> gtk::Picture {
    let picture = gtk::Picture::new();
    picture.set_size_request(size, size);
    picture.set_hexpand(false);
    picture.set_vexpand(false);
    picture.set_can_shrink(true);
    picture.set_keep_aspect_ratio(false);
    picture.set_paintable(icon);
    picture
}

/// One row of the users list.
struct Profile {
    username: String,
    user_uuid: String,
    icon: Option<gtk::gdk::Texture>,
}

/// Read the console's user profiles. Upstream constructs a
/// `Service::Account::ProfileManager` and walks `GetAllUsers()`.
///
fn load_profiles() -> Vec<Profile> {
    let manager = ruzu_core::hle::service::acc::profile_manager::ProfileManager::new();

    (0..manager.get_user_count())
        .filter_map(|index| {
            let base = manager.get_profile_base(Some(index))?;
            let uuid = common::uuid::UUID::from_bytes(base.user_uuid);
            let username_end = base
                .username
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(base.username.len());

            Some(Profile {
                username: String::from_utf8_lossy(&base.username[..username_end]).into_owned(),
                user_uuid: uuid.formatted_string(),
                icon: load_profile_icon(uuid),
            })
        })
        .collect()
}

/// Upstream `GetImagePath` / `GetIcon`: load the per-user JPEG from the account
/// save and use `ACCOUNT_BACKUP_JPEG` when it is absent or invalid.
fn load_profile_icon(uuid: common::uuid::UUID) -> Option<gtk::gdk::Texture> {
    std::fs::read(image_path(uuid))
        .ok()
        .and_then(|bytes| texture_from_bytes(&bytes, AVATAR_SIZE))
        .or_else(|| texture_from_bytes(&ruzu_core::constants::ACCOUNT_BACKUP_JPEG, AVATAR_SIZE))
}

fn image_path(uuid: common::uuid::UUID) -> std::path::PathBuf {
    common::fs::path_util::get_ruzu_path(common::fs::path_util::RuzuPath::NANDDir).join(format!(
        "system/save/8000000000000010/su/avators/{}.jpg",
        uuid.formatted_string()
    ))
}

fn texture_from_bytes(bytes: &[u8], size: i32) -> Option<gtk::gdk::Texture> {
    use gtk::gdk_pixbuf::prelude::PixbufLoaderExt;

    let loader = gtk::gdk_pixbuf::PixbufLoader::new();
    loader.write(bytes).ok()?;
    loader.close().ok()?;
    let pixbuf = loader.pixbuf()?;
    let scaled = pixbuf.scale_simple(size, size, gtk::gdk_pixbuf::InterpType::Bilinear)?;
    Some(gtk::gdk::Texture::for_pixbuf(&scaled))
}

#[cfg(test)]
mod tests {
    use super::{texture_from_bytes, AVATAR_SIZE};
    use gtk::prelude::TextureExt;

    #[test]
    fn profile_names_fit_the_fixed_utf8_payload() {
        assert!(!super::valid_username(""));
        assert!(super::valid_username(&"a".repeat(32)));
        assert!(!super::valid_username(&"a".repeat(33)));
        assert!(super::valid_username(&"é".repeat(16)));
        assert!(!super::valid_username(&"é".repeat(17)));
        assert!(!super::valid_username("a\0b"));
    }

    #[test]
    #[ignore = "requires a GTK display; run alone with --ignored"]
    fn profile_buttons_persist_add_rename_remove_and_select() {
        use super::ProfileManager;
        use gtk::prelude::*;
        fn find<T: IsA<gtk::Widget> + gtk::glib::object::IsClass>(root: &gtk::Widget) -> Option<T> {
            if let Ok(widget) = root.clone().downcast::<T>() {
                return Some(widget);
            }
            let mut child = root.first_child();
            while let Some(widget) = child {
                if let Some(found) = find(&widget) {
                    return Some(found);
                }
                child = widget.next_sibling();
            }
            None
        }
        fn button(root: &gtk::Widget, label: &str) -> Option<gtk::Button> {
            if let Some(b) = root.downcast_ref::<gtk::Button>() {
                if b.label().as_deref() == Some(label) {
                    return Some(b.clone());
                }
            }
            let mut child = root.first_child();
            while let Some(widget) = child {
                if let Some(found) = button(&widget, label) {
                    return Some(found);
                }
                child = widget.next_sibling();
            }
            None
        }
        fn editor() -> gtk::Dialog {
            gtk::Window::list_toplevels()
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::Dialog>().ok())
                .find(|w| w.is_visible())
                .expect("click must open the editor")
        }
        gtk::init().unwrap();
        let root = tempfile::tempdir().unwrap();
        common::fs::path_util::set_ruzu_path(common::fs::path_util::RuzuPath::NANDDir, root.path());
        common::settings::values_mut().current_user.set_value(0);
        let page = super::page(true);
        let window = gtk::Window::new();
        window.set_child(Some(&page.widget));
        let list = find::<gtk::ListBox>(&page.widget).unwrap();
        button(&page.widget, "Add").unwrap().emit_clicked();
        let dialog = editor();
        find::<gtk::Entry>(dialog.upcast_ref())
            .unwrap()
            .set_text("Homebrew");
        dialog.response(gtk::ResponseType::Accept);
        let manager = ProfileManager::new();
        assert_eq!(manager.get_user_count(), 2);
        let uuid = manager.get_user(1).unwrap();
        assert_ne!(uuid, 0);
        let uuid_value = common::uuid::UUID::from_bytes(uuid.to_le_bytes());
        let source = root.path().join("avatar.png");
        let pixbuf =
            gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, false, 8, 16, 16)
                .unwrap();
        pixbuf.fill(0x2277bbff);
        pixbuf.savev(&source, "png", &[]).unwrap();
        super::save_image(&source, uuid_value).unwrap();
        let image_path = super::image_path(uuid_value);
        let bytes = std::fs::read(&image_path).unwrap();
        assert_eq!(&bytes[..2], &[0xff, 0xd8]);
        assert!(super::load_profile_icon(uuid_value).is_some());
        std::fs::write(&source, b"invalid image").unwrap();
        assert!(super::save_image(&source, uuid_value).is_err());
        assert_eq!(std::fs::read(&image_path).unwrap(), bytes);
        list.select_row(list.row_at_index(1).as_ref());
        assert_eq!(*common::settings::values().current_user.get_value(), 1);
        button(&page.widget, "Rename").unwrap().emit_clicked();
        let dialog = editor();
        find::<gtk::Entry>(dialog.upcast_ref())
            .unwrap()
            .set_text("Homebrew renamed");
        dialog.response(gtk::ResponseType::Accept);
        let manager = ProfileManager::new();
        assert_eq!(manager.get_user(1), Some(uuid));
        assert_eq!(
            &manager.get_profile_base(Some(1)).unwrap().username[..16],
            b"Homebrew renamed"
        );
        list.select_row(list.row_at_index(1).as_ref());
        button(&page.widget, "Remove").unwrap().emit_clicked();
        let dialog = editor();
        dialog.response(gtk::ResponseType::Cancel);
        assert_eq!(ProfileManager::new().get_user_count(), 2);
        button(&page.widget, "Remove").unwrap().emit_clicked();
        editor().response(gtk::ResponseType::Accept);
        assert_eq!(ProfileManager::new().get_user_count(), 1);
        assert!(!ProfileManager::new().user_exists(uuid));
        assert_eq!(*common::settings::values().current_user.get_value(), 0);
        let locked = super::page(false);
        assert!(!button(&locked.widget, "Add").unwrap().is_sensitive());
        button(&locked.widget, "Add").unwrap().emit_clicked();
        assert_eq!(ProfileManager::new().get_user_count(), 1);
        window.close();
    }

    #[test]
    fn account_backup_jpeg_matches_upstream_avatar_size() {
        let texture = texture_from_bytes(&ruzu_core::constants::ACCOUNT_BACKUP_JPEG, AVATAR_SIZE)
            .expect("the fallback avatar must decode");

        assert_eq!(texture.width(), AVATAR_SIZE);
        assert_eq!(texture.height(), AVATAR_SIZE);
    }
}
