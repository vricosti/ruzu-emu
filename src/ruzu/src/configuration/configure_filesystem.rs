// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of
// `/home/vricosti/Dev/emulators/zuyu/src/yuzu/configuration/configure_filesystem.cpp`
// (`ConfigureFilesystem`), whose widget tree lives in `configure_filesystem.ui`.
//
// Four groups: "Storage Directories" (NAND, SD card), "Gamecard" (inserted /
// current-game / path), "Patch Manager" (dump root, mod load root, NSO/ExeFS
// dump toggles), and "Caching" (game-list metadata cache + reset button).
//
// The default paths come from `Common::FS::GetYuzuPath`, ported in ruzu as
// `common::fs::path_util`.

use std::path::Path;

use gtk::prelude::*;

use common::fs::path_util::{get_ruzu_path_string, set_ruzu_path, RuzuPath};

use super::configure_dialog::Page;
use super::shared_widget as w;

/// Build the Filesystem tab — upstream `ConfigureFilesystem`.
pub fn page() -> Page {
    let (scroller, column) = w::page();

    // --- "Storage Directories" -------------------------------------------
    let (storage_group, storage) = w::group("Storage Directories");

    let (nand_row, nand, nand_browse) =
        w::path_row("NAND", &get_ruzu_path_string(RuzuPath::NANDDir));
    storage.append(&nand_row);

    let (save_row, save, save_browse) =
        w::path_row("Save Data", &get_ruzu_path_string(RuzuPath::SaveDir));
    storage.append(&save_row);

    let (sdmc_row, sdmc, sdmc_browse) =
        w::path_row("SD Card", &get_ruzu_path_string(RuzuPath::SDMCDir));
    storage.append(&sdmc_row);

    column.append(&storage_group);

    // --- "Gamecard" -------------------------------------------------------
    let (gamecard_group, gamecard) = w::group("Gamecard");

    let inserted = w::check_row(
        "Inserted",
        *common::settings::values().gamecard_inserted.get_value(),
    );
    gamecard.append(&inserted);

    let current_game = w::check_row(
        "Current Game",
        *common::settings::values().gamecard_current_game.get_value(),
    );
    // Upstream disables "Current Game" unless a gamecard is inserted.
    current_game.set_sensitive(inserted.is_active());
    gamecard.append(&current_game);

    let gamecard_path_value = common::settings::values().gamecard_path.get_value().clone();
    let (gamecard_path_row, gamecard_path, gamecard_browse) =
        w::path_row("Path", &gamecard_path_value);
    // Upstream likewise disables the path unless a gamecard is inserted and it
    // is not the currently-running game.
    gamecard_path_row.set_sensitive(inserted.is_active() && !current_game.is_active());
    gamecard.append(&gamecard_path_row);

    // Keep the dependent widgets in step, matching upstream's
    // `UpdateEnabledControls()` slot.
    {
        let current_game = current_game.clone();
        let gamecard_path_row = gamecard_path_row.clone();
        inserted.connect_toggled(move |check| {
            current_game.set_sensitive(check.is_active());
            gamecard_path_row.set_sensitive(check.is_active() && !current_game.is_active());
        });
    }
    {
        let inserted = inserted.clone();
        let gamecard_path_row = gamecard_path_row.clone();
        current_game.connect_toggled(move |check| {
            gamecard_path_row.set_sensitive(inserted.is_active() && !check.is_active());
        });
    }

    column.append(&gamecard_group);

    // --- "Patch Manager" --------------------------------------------------
    let (patch_group, patch) = w::group("Patch Manager");

    let (dump_row, dump_root, dump_browse) =
        w::path_row("Dump Root", &get_ruzu_path_string(RuzuPath::DumpDir));
    patch.append(&dump_row);

    let (load_row, load_root, load_browse) =
        w::path_row("Mod Load Root", &get_ruzu_path_string(RuzuPath::LoadDir));
    patch.append(&load_row);

    let dumps = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let dump_nso = w::check_row(
        "Dump Decompressed NSOs",
        *common::settings::values().dump_nso.get_value(),
    );
    dump_nso.set_hexpand(true);
    let dump_exefs = w::check_row(
        "Dump ExeFS",
        *common::settings::values().dump_exefs.get_value(),
    );
    dumps.append(&dump_nso);
    dumps.append(&dump_exefs);
    patch.append(&dumps);

    column.append(&patch_group);

    // --- "Caching" --------------------------------------------------------
    let (caching_group, caching) = w::group("Caching");

    let caching_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let cache_metadata = w::check_row(
        "Cache Game List Metadata",
        crate::uisettings::with(|v| *v.cache_game_list.get_value()),
    );
    cache_metadata.set_hexpand(true);
    let reset_cache = gtk::Button::with_label("Reset Metadata Cache");
    caching_row.append(&cache_metadata);
    caching_row.append(&reset_cache);
    caching.append(&caching_row);

    column.append(&caching_group);

    // Directory pickers for every path row.
    connect_folder_picker(&nand_browse, &nand, "Select NAND Directory...");
    connect_folder_picker(&save_browse, &save, "Select Save Data Directory...");
    connect_folder_picker(&sdmc_browse, &sdmc, "Select SD Card Directory...");
    connect_gamecard_picker(&gamecard_browse, &gamecard_path);
    connect_folder_picker(&dump_browse, &dump_root, "Select Dump Directory...");
    connect_folder_picker(&load_browse, &load_root, "Select Mod Load Directory...");

    reset_cache.connect_clicked(|button| {
        let parent = button.root().and_downcast::<gtk::Window>();
        crate::util::game::reset_metadata(parent.as_ref(), true);
    });

    Page::new("Filesystem", scroller, move || {
        let card_inserted = inserted.is_active();
        let card_current_game = current_game.is_active();
        let card_path = gamecard_path.text().to_string();
        let nso = dump_nso.is_active();
        let exefs = dump_exefs.is_active();
        let cache = cache_metadata.is_active();

        {
            let mut values = common::settings::values_mut();
            values.gamecard_inserted.set_value(card_inserted);
            values.gamecard_current_game.set_value(card_current_game);
            values.gamecard_path.set_value(card_path);
            values.dump_nso.set_value(nso);
            values.dump_exefs.set_value(exefs);
        }
        crate::uisettings::with_mut(|v| v.cache_game_list.set_value(cache));

        // The storage/patch directory entries feed `Common::FS::SetYuzuPath`
        // upstream — `path_util::set_ruzu_path` here.
        set_ruzu_path(RuzuPath::NANDDir, Path::new(&nand.text()));
        set_ruzu_path(RuzuPath::SaveDir, Path::new(&save.text()));
        set_ruzu_path(RuzuPath::SDMCDir, Path::new(&sdmc.text()));
        set_ruzu_path(RuzuPath::DumpDir, Path::new(&dump_root.text()));
        set_ruzu_path(RuzuPath::LoadDir, Path::new(&load_root.text()));
    })
}

/// Wire the gamecard path button to the file chooser used by upstream
/// `ConfigureFilesystem::SetDirectory(DirectoryTarget::Gamecard)`.
fn connect_gamecard_picker(button: &gtk::Button, entry: &gtk::Entry) {
    let entry = entry.clone();
    button.connect_clicked(move |button| {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("NX Gamecard"));
        filter.add_pattern("*.xci");
        let entry = entry.clone();
        let parent = button.root().and_downcast::<gtk::Window>();
        crate::gtk_compat::open_file(
            parent.as_ref(),
            "Select Gamecard Path...",
            std::slice::from_ref(&filter),
            Some(&filter),
            move |result| {
                if let Some(path) = result.and_then(|file| file.path()) {
                    entry.set_text(&path.to_string_lossy());
                }
            },
        );
    });
}

/// Wire a `...` button to a folder chooser that writes into `entry`.
fn connect_folder_picker(button: &gtk::Button, entry: &gtk::Entry, title: &str) {
    let entry = entry.clone();
    let title = title.to_string();
    button.connect_clicked(move |button| {
        let entry = entry.clone();
        let parent = button.root().and_downcast::<gtk::Window>();
        crate::gtk_compat::select_folder(parent.as_ref(), &title, move |result| {
            if let Some(folder) = result {
                if let Some(path) = folder.path() {
                    entry.set_text(&path.to_string_lossy());
                }
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display; run alone for global filesystem/settings state"]
    fn executable_dump_switches_initialize_and_apply_independently() {
        gtk::init().expect("a GTK display is required");
        let root = tempfile::tempdir().unwrap();
        for (path, name) in [
            (RuzuPath::NANDDir, "nand"),
            (RuzuPath::SaveDir, "save"),
            (RuzuPath::SDMCDir, "sdmc"),
            (RuzuPath::DumpDir, "dump"),
            (RuzuPath::LoadDir, "load"),
        ] {
            let directory = root.path().join(name);
            std::fs::create_dir(&directory).unwrap();
            set_ruzu_path(path, &directory);
        }
        fn find_switch(widget: &gtk::Widget, label: &str) -> Option<gtk::CheckButton> {
            if let Some(check) = widget.downcast_ref::<gtk::CheckButton>() {
                if check.label().as_deref() == Some(label) {
                    return Some(check.clone());
                }
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                if let Some(check) = find_switch(&widget, label) {
                    return Some(check);
                }
                child = widget.next_sibling();
            }
            None
        }
        for enabled in [false, true] {
            {
                let mut values = common::settings::values_mut();
                values.dump_nso.set_value(enabled);
                values.dump_exefs.set_value(!enabled);
            }
            let page = page();
            let nso = find_switch(&page.widget, "Dump Decompressed NSOs").unwrap();
            let exefs = find_switch(&page.widget, "Dump ExeFS").unwrap();
            assert_eq!(nso.is_active(), enabled);
            assert_eq!(exefs.is_active(), !enabled);
            nso.set_active(!enabled);
            exefs.set_active(enabled);
            (page.apply)();
            let values = common::settings::values();
            assert_eq!(*values.dump_nso.get_value(), !enabled);
            assert_eq!(*values.dump_exefs.get_value(), enabled);
        }
    }
}
