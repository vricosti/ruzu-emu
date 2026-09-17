// SPDX-FileCopyrightText: Copyright 2016 Citra Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! GTK counterpart of Eden `yuzu/configuration/configure_per_game_addons.{h,cpp,ui}`.

use std::cell::Cell;
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use gtk::prelude::*;
use gtk::{gio, glib};
use ruzu_core::file_sys::patch_manager::{PatchManager, PatchSource, PatchType};
use ruzu_core::file_sys::registered_cache::{
    ContentProvider, ContentProviderUnion, ExternalContentProvider,
};
use ruzu_core::file_sys::vfs::vfs_real::RealVfsFilesystem;
use ruzu_core::hle::service::filesystem::filesystem::FileSystemController;
use ruzu_core::loader::loader::{get_loader, System as LoaderSystem};

use super::configure_dialog::Page;

/// One patch row supplied by the selected title's patch manager.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddOn {
    pub name: String,
    pub version: String,
    pub enabled: bool,
    pub patch_type: PatchType,
    pub source: PatchSource,
    pub location: String,
    pub numeric_version: u32,
}

struct AddOnRow {
    name: String,
    version: String,
    enabled: Cell<bool>,
    patch_type: PatchType,
    source: PatchSource,
    location: String,
    numeric_version: u32,
}

/// `ConfigurePerGameAddons::LoadFromFile`.
///
/// The GTK frontend does not keep a powered-on `Core::System` while the game
/// list is visible, so construct the same lightweight filesystem/controller
/// pair used by the list scanner and pass those upstream-owned dependencies to
/// `PatchManager`.
pub fn load_from_file(title_id: u64, path: &Path) -> Vec<AddOn> {
    use ruzu_core::file_sys::fs_filesystem::OpenMode;

    let vfs = RealVfsFilesystem::new();
    let content_provider = Arc::new(Mutex::new(ContentProviderUnion::new()));
    let mut controller = FileSystemController::new();
    controller.set_content_provider(content_provider.clone());
    controller.create_factories(vfs.clone(), false);
    let directories = crate::uisettings::with(|values| {
        values
            .game_dirs
            .iter()
            .filter(|directory| directory.is_filesystem_path())
            .cloned()
            .collect::<Vec<_>>()
    });
    let load_directories = directories
        .iter()
        .filter_map(|directory| {
            vfs.arc_open_directory(
                &directory.path,
                ruzu_core::file_sys::fs_filesystem::OpenMode::READ,
            )
        })
        .collect();
    let mut external_content_provider = Box::new(ExternalContentProvider::new(load_directories));
    {
        let mut provider = content_provider
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        unsafe {
            provider.set_external_provider(
                (&mut *external_content_provider as *mut ExternalContentProvider)
                    as *mut dyn ContentProvider,
            );
        }
    }
    let controller = Arc::new(Mutex::new(controller));
    let mut loader_system =
        LoaderSystem::new(Some(content_provider.clone()), Some(controller.clone()));

    let Some(file) = vfs.arc_open_file(&path.to_string_lossy(), OpenMode::READ) else {
        return Vec::new();
    };
    let Some(loader) = get_loader(&mut loader_system, file, 0, 0) else {
        return Vec::new();
    };
    let mut update_raw = None;
    loader.read_update_raw(&mut update_raw);

    let controller = controller.lock().unwrap_or_else(|error| error.into_inner());
    let content_provider = content_provider
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    PatchManager::new(title_id, &controller, &*content_provider)
        .get_patches(update_raw)
        .into_iter()
        .map(|patch| AddOn {
            name: patch.name,
            version: patch.version,
            enabled: patch.enabled,
            patch_type: patch.patch_type,
            source: patch.source,
            location: patch.location,
            numeric_version: patch.numeric_version,
        })
        .collect()
}

/// Build the sortable two-column patch list.
pub fn page(title_id: u64, game_path: &Path) -> Page {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.set_margin_top(10);
    root.set_margin_bottom(10);
    root.set_margin_start(10);
    root.set_margin_end(10);

    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    populate_store(&store, &load_from_file(title_id, game_path));

    let view = gtk::ColumnView::new(None::<gtk::MultiSelection>);
    view.set_hexpand(true);
    view.set_vexpand(true);
    view.set_show_column_separators(true);
    view.set_show_row_separators(false);

    let sort_model = gtk::SortListModel::new(Some(store.clone()), view.sorter());
    let selection = gtk::MultiSelection::new(Some(sort_model));
    view.set_model(Some(&selection));

    let name_column = addon_name_column(&store, &selection, title_id, game_path.to_path_buf());
    let version_column = addon_version_column();
    view.append_column(&name_column);
    view.append_column(&version_column);

    let frame = gtk::Frame::new(None);
    frame.set_hexpand(true);
    frame.set_vexpand(true);
    frame.set_child(Some(&view));
    root.append(&frame);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    buttons.set_homogeneous(true);
    let zip_button = gtk::Button::with_label("Import Mod from ZIP");
    let folder_button = gtk::Button::with_label("Import Mod from Folder");
    buttons.append(&zip_button);
    buttons.append(&folder_button);
    root.append(&buttons);

    let game_path = game_path.to_path_buf();
    connect_folder_install(&folder_button, title_id, game_path.clone(), store.clone());
    connect_zip_install(&zip_button, title_id, game_path, store.clone());

    Page::new("Add-Ons", root, move || {
        let mut disabled: Vec<String> = (0..store.n_items())
            .filter_map(|index| store.item(index))
            .filter_map(|item| item.downcast::<glib::BoxedAnyObject>().ok())
            .filter_map(|item| {
                let row = item.borrow::<AddOnRow>();
                (!row.enabled.get()).then(|| disabled_key(&row))
            })
            .collect();
        disabled.sort();

        let mut settings = common::settings::values_mut();
        let mut current = settings
            .disabled_addons
            .get(&title_id)
            .cloned()
            .unwrap_or_default();
        current.sort();
        if current != disabled {
            let cache =
                common::fs::path_util::get_ruzu_path(common::fs::path_util::RuzuPath::CacheDir)
                    .join("game_list")
                    .join(format!("{title_id:016X}.pv.txt"));
            let _ = std::fs::remove_file(cache);
        }
        settings.disabled_addons.insert(title_id, disabled);
    })
}

fn populate_store(store: &gio::ListStore, patches: &[AddOn]) {
    store.remove_all();
    for patch in patches {
        store.append(&glib::BoxedAnyObject::new(AddOnRow {
            name: patch.name.clone(),
            version: patch.version.clone(),
            enabled: Cell::new(patch.enabled),
            patch_type: patch.patch_type,
            source: patch.source,
            location: patch.location.clone(),
            numeric_version: patch.numeric_version,
        }));
    }
}

fn disabled_key(row: &AddOnRow) -> String {
    if row.name == "Update" && row.source == PatchSource::External && row.numeric_version != 0 {
        format!("Update@{}", row.numeric_version)
    } else {
        row.name.clone()
    }
}

fn addon_name_column(
    store: &gio::ListStore,
    selection: &gtk::MultiSelection,
    title_id: u64,
    game_path: PathBuf,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let store_for_setup = store.clone();
    let selection_for_setup = selection.clone();
    factory.connect_setup(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let check = gtk::CheckButton::new();
        check.set_hexpand(true);
        let weak_item = item.downgrade();
        let store = store_for_setup.clone();
        check.connect_toggled(move |check| {
            let Some(item) = weak_item.upgrade() else {
                return;
            };
            let Some(row_object) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
                return;
            };
            let is_update = {
                let row = row_object.borrow::<AddOnRow>();
                row.enabled.set(check.is_active());
                row.patch_type == PatchType::Update
            };
            if check.is_active() && is_update {
                for index in 0..store.n_items() {
                    let Some(other) = store.item(index).and_downcast::<glib::BoxedAnyObject>()
                    else {
                        continue;
                    };
                    if other != row_object {
                        let other = other.borrow::<AddOnRow>();
                        if other.patch_type == PatchType::Update {
                            other.enabled.set(false);
                        }
                    }
                }
                store.items_changed(0, store.n_items(), store.n_items());
            }
        });
        install_context_menu_gesture(
            &check,
            item,
            selection_for_setup.clone(),
            title_id,
            game_path.clone(),
            store_for_setup.clone(),
        );
        item.set_child(Some(&check));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(check) = item.child().and_downcast::<gtk::CheckButton>() else {
            return;
        };
        let Some(row) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let row = row.borrow::<AddOnRow>();
        check.set_label(Some(&row.name));
        check.set_active(row.enabled.get());
    });

    let sorter =
        gtk::CustomSorter::new(|a, b| compare_addon_rows(a, b, |row| row.name.to_lowercase()));
    let column = gtk::ColumnViewColumn::new(Some("Patch Name"), Some(factory));
    column.set_expand(true);
    column.set_resizable(true);
    column.set_sorter(Some(&sorter));
    column
}

fn install_context_menu_gesture(
    anchor: &gtk::CheckButton,
    item: &gtk::ListItem,
    selection: gtk::MultiSelection,
    title_id: u64,
    game_path: PathBuf,
    store: gio::ListStore,
) {
    let gesture = gtk::GestureClick::new();
    gesture.set_button(gtk::gdk::BUTTON_SECONDARY);
    let weak_item = item.downgrade();
    let anchor = anchor.clone();
    let menu_anchor = anchor.clone();
    gesture.connect_pressed(move |gesture, _, x, y| {
        let Some(item) = weak_item.upgrade() else {
            return;
        };
        let position = item.position();
        if !selection.is_selected(position) {
            selection.unselect_all();
            selection.select_item(position, false);
        }
        let selected = selection.selection();
        let rows: Vec<glib::BoxedAnyObject> = (0..selected.size() as u32)
            .filter_map(|index| selection.item(selected.nth(index)))
            .filter_map(|object| object.downcast::<glib::BoxedAnyObject>().ok())
            .collect();
        if rows.is_empty() {
            return;
        }

        let menu = gio::Menu::new();
        menu.append(Some("Delete"), Some("addons.delete"));
        let locations: Vec<PathBuf> = rows
            .iter()
            .filter_map(|row| {
                let location = &row.borrow::<AddOnRow>().location;
                (!location.is_empty()).then(|| PathBuf::from(location))
            })
            .collect();
        if locations.len() == 1 && locations[0].exists() {
            menu.append(Some("Open in File Manager"), Some("addons.open"));
        }

        let actions = gio::SimpleActionGroup::new();
        let delete = gio::SimpleAction::new("delete", None);
        let parent = menu_anchor.root().and_downcast::<gtk::Window>();
        let locations_for_delete = locations.clone();
        let store_for_delete = store.clone();
        let game_path_for_delete = game_path.clone();
        delete.connect_activate(move |_, _| {
            if locations_for_delete.is_empty() {
                crate::gtk_compat::show_warning(
                    parent.as_ref(),
                    "Invalid Selection",
                    "Only mods, cheats, and patches can be deleted.",
                );
                return;
            }
            let parent_for_answer = parent.clone();
            let locations = locations_for_delete.clone();
            let store = store_for_delete.clone();
            let game_path = game_path_for_delete.clone();
            crate::gtk_compat::ask_question(
                parent.as_ref(),
                "Delete add-on(s)?",
                "Once deleted, these can NOT be recovered. Are you 100% sure you want to delete them?",
                "No",
                "Yes",
                move |accepted| {
                    if !accepted {
                        return;
                    }
                    for location in locations {
                        let _ = if location.is_dir() {
                            std::fs::remove_dir_all(location)
                        } else {
                            std::fs::remove_file(location)
                        };
                    }
                    populate_store(&store, &load_from_file(title_id, &game_path));
                    crate::gtk_compat::show_message(
                        parent_for_answer.as_ref(),
                        "Successfully deleted",
                        "Successfully deleted all selected mods.",
                    );
                },
            );
        });
        actions.add_action(&delete);

        let open = gio::SimpleAction::new("open", None);
        open.connect_activate(move |_, _| {
            if let Some(location) = locations.first() {
                let file = gio::File::for_path(location);
                let _ = gio::AppInfo::launch_default_for_uri(
                    &file.uri(),
                    gio::AppLaunchContext::NONE,
                );
            }
        });
        actions.add_action(&open);

        if crate::util::inline_menu::enabled() {
            crate::util::inline_menu::show_context_menu(menu_anchor.upcast_ref(), "addons", menu.upcast_ref(), actions.upcast_ref(), x, y);
            gesture.set_state(gtk::EventSequenceState::Claimed);
            return;
        }
        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.set_has_arrow(false);
        popover.insert_action_group("addons", Some(&actions));
        popover.set_parent(&menu_anchor);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.connect_closed(|popover| {
            let popover = popover.clone();
            glib::idle_add_local_once(move || popover.unparent());
        });
        popover.popup();
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });
    anchor.add_controller(gesture);
}

fn connect_folder_install(
    button: &gtk::Button,
    title_id: u64,
    game_path: PathBuf,
    store: gio::ListStore,
) {
    button.connect_clicked(move |button| {
        let parent = button.root().and_downcast::<gtk::Window>();
        let game_path = game_path.clone();
        let store = store.clone();
        let parent_for_result = parent.clone();
        crate::gtk_compat::select_folder(parent.as_ref(), "Mod Folder", move |selected| {
            let Some(path) = selected.and_then(|file| file.path()) else {
                return;
            };
            install_mod_paths(
                &path,
                None,
                title_id,
                &game_path,
                &store,
                parent_for_result.as_ref(),
            );
        });
    });
}

fn connect_zip_install(
    button: &gtk::Button,
    title_id: u64,
    game_path: PathBuf,
    store: gio::ListStore,
) {
    button.connect_clicked(move |button| {
        let parent = button.root().and_downcast::<gtk::Window>();
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Zipped Archives (*.zip)"));
        filter.add_pattern("*.zip");
        let game_path = game_path.clone();
        let store = store.clone();
        let parent_for_result = parent.clone();
        crate::gtk_compat::open_file(
            parent.as_ref(),
            "Zipped Mod Location",
            &[filter.clone()],
            Some(&filter),
            move |selected| {
                let Some(path) = selected.and_then(|file| file.path()) else {
                    return;
                };
                match crate::util::mod_install::extract_mod(&path) {
                    Ok(temporary) => {
                        let fallback = path.file_stem().and_then(|name| name.to_str());
                        install_mod_paths(
                            &temporary,
                            fallback,
                            title_id,
                            &game_path,
                            &store,
                            parent_for_result.as_ref(),
                        );
                    }
                    Err(error) => crate::gtk_compat::show_warning(
                        parent_for_result.as_ref(),
                        "Mod Extract Failed",
                        &error,
                    ),
                }
            },
        );
    });
}

fn install_mod_paths(
    root: &Path,
    fallback_name: Option<&str>,
    title_id: u64,
    game_path: &Path,
    store: &gio::ListStore,
    parent: Option<&gtk::Window>,
) {
    let game_path = game_path.to_path_buf();
    let store = store.clone();
    let parent = parent.cloned();
    let parent_for_dialog = parent.clone();
    crate::util::mod_install::choose_mod_folders(
        root,
        fallback_name,
        parent_for_dialog.as_ref(),
        move |mods| {
            if mods.is_empty() {
                return;
            }
            let failed: Vec<String> = mods
                .iter()
                .filter(|path| {
                    frontend_common::mod_manager::install_mod(path, title_id, true)
                        == frontend_common::mod_manager::ModInstallResult::Failed
                })
                .filter_map(|path| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                })
                .collect();
            if failed.is_empty() {
                populate_store(&store, &load_from_file(title_id, &game_path));
                crate::gtk_compat::show_message(
                    parent.as_ref(),
                    "Mod Install Succeeded",
                    "Successfully installed all mods.",
                );
            } else {
                crate::gtk_compat::show_warning(
                    parent.as_ref(),
                    "Mod Install Failed",
                    &format!(
                        "Failed to install the following mods:\n\t{}",
                        failed.join("\n\t")
                    ),
                );
            }
        },
    );
}

fn addon_version_column() -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        item.set_child(Some(&gtk::Label::builder().xalign(0.0).build()));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(label) = item.child().and_downcast::<gtk::Label>() else {
            return;
        };
        let Some(row) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        label.set_label(&row.borrow::<AddOnRow>().version);
    });

    let sorter =
        gtk::CustomSorter::new(|a, b| compare_addon_rows(a, b, |row| row.version.to_lowercase()));
    let column = gtk::ColumnViewColumn::new(Some("Version"), Some(factory));
    column.set_fixed_width(130);
    column.set_resizable(true);
    column.set_sorter(Some(&sorter));
    column
}

fn compare_addon_rows(
    a: &glib::Object,
    b: &glib::Object,
    value: impl Fn(&AddOnRow) -> String,
) -> gtk::Ordering {
    let Some(a) = a.downcast_ref::<glib::BoxedAnyObject>() else {
        return gtk::Ordering::Equal;
    };
    let Some(b) = b.downcast_ref::<glib::BoxedAnyObject>() else {
        return gtk::Ordering::Equal;
    };
    match value(&a.borrow::<AddOnRow>()).cmp(&value(&b.borrow::<AddOnRow>())) {
        Ordering::Less => gtk::Ordering::Smaller,
        Ordering::Equal => gtk::Ordering::Equal,
        Ordering::Greater => gtk::Ordering::Larger,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_updates_persist_by_numeric_version() {
        let row = AddOnRow {
            name: "Update".to_string(),
            version: "1.7.1".to_string(),
            enabled: Cell::new(false),
            patch_type: PatchType::Update,
            source: PatchSource::External,
            location: String::new(),
            numeric_version: 458752,
        };
        assert_eq!(disabled_key(&row), "Update@458752");

        let nand = AddOnRow {
            source: PatchSource::NAND,
            numeric_version: 458752,
            ..row
        };
        assert_eq!(disabled_key(&nand), "Update");
    }
}
