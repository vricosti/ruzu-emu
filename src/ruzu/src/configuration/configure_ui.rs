// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of
// `/home/vricosti/Dev/emulators/zuyu/src/yuzu/configuration/configure_ui.cpp`
// (`ConfigureUi`), whose widget tree lives in `configure_ui.ui`.
//
// Three groups: "General" (language + theme), "Game List" (column toggles, icon
// sizes, row text), and "Screenshots" (save-as prompt, path, resolution).
//
// The icon-size and row-text combo contents come from upstream's
// `ConfigureUi::InitializeIconSizeComboBox` / `InitializeRowComboBoxes`.

use gtk::prelude::*;

use crate::uisettings;

use super::configure_dialog::Page;
use super::shared_widget as w;

/// Game icon sizes — upstream `ConfigureUi::InitializeIconSizeComboBox`.
const GAME_ICON_SIZES: &[(u32, &str)] = &[
    (0, "None"),
    (32, "Small (32x32)"),
    (64, "Standard (64x64)"),
    (128, "Large (128x128)"),
    (256, "Full Size (256x256)"),
];

/// Folder icon sizes — upstream's second icon-size combo.
const FOLDER_ICON_SIZES: &[(u32, &str)] = &[
    (0, "None"),
    (24, "Small (24x24)"),
    (48, "Standard (48x48)"),
    (72, "Large (72x72)"),
];

/// Screenshot resolutions — upstream `ConfigureUi::UpdateScreenshotInfo`, whose
/// first entry reports the resolution the current aspect/scale would produce.
const SCREENSHOT_RESOLUTIONS: &[(u32, &str)] = &[
    (0, "Auto"),
    (720, "1280x720 (720p)"),
    (1080, "1920x1080 (1080p)"),
    (1440, "2560x1440 (1440p)"),
    (2160, "3840x2160 (4K)"),
];

/// Build the UI tab — upstream `ConfigureUi`.
pub fn page() -> Page {
    let (scroller, column) = w::page();

    // --- "General" --------------------------------------------------------
    let (general_group, general) = w::group("General");

    let note = gtk::Label::new(Some(
        "Note: Changing language will apply your configuration.",
    ));
    note.set_xalign(0.0);
    general.append(&note);

    let language_labels: Vec<String> = crate::i18n::AVAILABLE_LANGUAGES
        .iter()
        .map(|(_, label)| crate::i18n::tr(label))
        .collect();
    let language_refs: Vec<&str> = language_labels.iter().map(String::as_str).collect();
    let current_language = uisettings::with(|v| v.language.get_value().clone());
    let language_index = crate::i18n::AVAILABLE_LANGUAGES
        .iter()
        .position(|(locale, _)| *locale == current_language)
        .unwrap_or(0) as u32;
    let (language_row, language) =
        w::combo_row("Interface language:", &language_refs, language_index);
    general.append(&language_row);

    language.connect_selected_notify(|combo| {
        if crate::i18n::is_retranslating() {
            return;
        }
        let locale = crate::i18n::AVAILABLE_LANGUAGES
            .get(combo.selected() as usize)
            .map(|(locale, _)| *locale)
            .unwrap_or("");
        uisettings::with_mut(|values| values.language.set_value(locale.to_string()));
        crate::i18n::set_language(locale);
        crate::retranslate_application();
        if let Some(root) = combo.root().and_downcast::<gtk::Window>() {
            crate::i18n::translate_widget_tree(&root);
        }
    });

    let theme_labels: Vec<&str> = uisettings::THEMES.iter().map(|(name, _)| *name).collect();
    let theme_index = uisettings::with(|v| {
        let current = v.theme.get_value().clone();
        uisettings::THEMES
            .iter()
            .position(|(name, internal)| *name == current || *internal == current)
            .unwrap_or(0) as u32
    });
    let (theme_row, theme) = w::combo_row("Theme:", &theme_labels, theme_index);
    general.append(&theme_row);

    column.append(&general_group);

    // --- "Game List" ------------------------------------------------------
    let (game_list_group, game_list) = w::group("Game List");

    let show_compat = w::check_row(
        "Show Compatibility List",
        uisettings::with(|v| *v.show_compat.get_value()),
    );
    let show_add_ons = w::check_row(
        "Show Add-Ons Column",
        uisettings::with(|v| *v.show_add_ons.get_value()),
    );
    let show_size = w::check_row(
        "Show Size Column",
        uisettings::with(|v| *v.show_size.get_value()),
    );
    let show_types = w::check_row(
        "Show File Types Column",
        uisettings::with(|v| *v.show_types.get_value()),
    );
    let show_play_time = w::check_row(
        "Show Play Time Column",
        uisettings::with(|v| *v.show_play_time.get_value()),
    );
    for check in [
        &show_compat,
        &show_add_ons,
        &show_size,
        &show_types,
        &show_play_time,
    ] {
        game_list.append(check);
    }

    let game_icon_labels: Vec<&str> = GAME_ICON_SIZES.iter().map(|(_, l)| *l).collect();
    let game_icon_index =
        uisettings::with(|v| index_by_value(GAME_ICON_SIZES, *v.game_icon_size.get_value()));
    let (game_icon_row, game_icon) =
        w::combo_row("Game Icon Size:", &game_icon_labels, game_icon_index);
    game_list.append(&game_icon_row);

    let folder_icon_labels: Vec<&str> = FOLDER_ICON_SIZES.iter().map(|(_, l)| *l).collect();
    let folder_icon_index =
        uisettings::with(|v| index_by_value(FOLDER_ICON_SIZES, *v.folder_icon_size.get_value()));
    let (folder_icon_row, folder_icon) =
        w::combo_row("Folder Icon Size:", &folder_icon_labels, folder_icon_index);
    game_list.append(&folder_icon_row);

    let row_text = uisettings::GAME_LIST_ROW_TEXT;
    let row_1_index = uisettings::with(|v| *v.row_1_text_id.get_value() as u32);
    let (row_1_row, row_1) = w::combo_row("Row 1 Text:", row_text, row_1_index);
    game_list.append(&row_1_row);

    let row_2_index = uisettings::with(|v| *v.row_2_text_id.get_value() as u32);
    let (row_2_row, row_2) = w::combo_row("Row 2 Text:", row_text, row_2_index);
    game_list.append(&row_2_row);

    column.append(&game_list_group);

    // --- "Screenshots" ----------------------------------------------------
    let (screenshots_group, screenshots) = w::group("Screenshots");

    let save_as = w::check_row(
        "Ask Where To Save Screenshots (Windows Only)",
        uisettings::with(|v| *v.enable_screenshot_save_as.get_value()),
    );
    screenshots.append(&save_as);

    // Upstream defaults this to `GetYuzuPathString(YuzuPath::ScreenshotsDir)`
    // and only stores an override, so an empty setting must still show the
    // real destination rather than a blank field.
    let screenshot_path = uisettings::with(|v| {
        let stored = v.screenshot_path.get_value().clone();
        if stored.is_empty() {
            common::fs::path_util::get_ruzu_path_string(
                common::fs::path_util::RuzuPath::ScreenshotsDir,
            )
        } else {
            stored
        }
    });
    let (path_row, path_entry, path_browse) = w::path_row("Screenshots Path:", &screenshot_path);
    screenshots.append(&path_row);

    let resolution_labels: Vec<&str> = SCREENSHOT_RESOLUTIONS.iter().map(|(_, l)| *l).collect();
    let resolution_index = uisettings::with(|v| {
        index_by_value(SCREENSHOT_RESOLUTIONS, *v.screenshot_height.get_value())
    });
    let (resolution_row, resolution) =
        w::combo_row("Resolution:", &resolution_labels, resolution_index);
    screenshots.append(&resolution_row);

    // Upstream opens a `QFileDialog::getExistingDirectory` here.
    let entry_for_browse = path_entry.clone();
    path_browse.connect_clicked(move |button| {
        let entry = entry_for_browse.clone();
        let parent = button.root().and_downcast::<gtk::Window>();
        crate::gtk_compat::select_folder(
            parent.as_ref(),
            "Select Screenshots Path...",
            move |result| {
                if let Some(folder) = result {
                    if let Some(path) = folder.path() {
                        entry.set_text(&path.to_string_lossy());
                    }
                }
            },
        );
    });

    column.append(&screenshots_group);

    Page::new("UI", scroller, move || {
        let theme_name = uisettings::THEMES
            .get(theme.selected() as usize)
            .map(|(name, _)| name.to_string())
            .unwrap_or_default();
        let language_code = crate::i18n::AVAILABLE_LANGUAGES
            .get(language.selected() as usize)
            .map(|(code, _)| code.to_string())
            .unwrap_or_default();
        let game_icon_value = value_at(GAME_ICON_SIZES, game_icon.selected());
        let folder_icon_value = value_at(FOLDER_ICON_SIZES, folder_icon.selected());
        let screenshot_height = value_at(SCREENSHOT_RESOLUTIONS, resolution.selected());

        let compat = show_compat.is_active();
        let add_ons = show_add_ons.is_active();
        let size = show_size.is_active();
        let types = show_types.is_active();
        let play_time = show_play_time.is_active();
        let ask_where = save_as.is_active();
        let path = path_entry.text().to_string();
        let row_1_id = row_1.selected() as u8;
        let row_2_id = row_2.selected() as u8;

        uisettings::with_mut(|v| {
            v.theme.set_value(theme_name);
            v.language.set_value(language_code);
            v.show_compat.set_value(compat);
            v.show_add_ons.set_value(add_ons);
            v.show_size.set_value(size);
            v.show_types.set_value(types);
            v.show_play_time.set_value(play_time);
            v.game_icon_size.set_value(game_icon_value);
            v.folder_icon_size.set_value(folder_icon_value);
            v.row_1_text_id.set_value(row_1_id);
            v.row_2_text_id.set_value(row_2_id);
            v.enable_screenshot_save_as.set_value(ask_where);
            v.screenshot_path.set_value(path);
            v.screenshot_height.set_value(screenshot_height);
        });

        // Upstream re-runs `UpdateUITheme()` from `OnConfigure` when the theme
        // changed, so the new stylesheet takes effect without a restart.
        crate::main_window::update_ui_theme();
        // ConfigureUi::ApplyConfiguration requests a list rebuild after the
        // settings are applied; recycled GTK rows must be rebound as well.
        uisettings::request_game_list_reload();
    })
}

/// Row index whose stored value equals `value`, or 0.
fn index_by_value(table: &[(u32, &str)], value: u32) -> u32 {
    table
        .iter()
        .position(|(stored, _)| *stored == value)
        .unwrap_or(0) as u32
}

/// Stored value at row `index`, or the first row's.
fn value_at(table: &[(u32, &str)], index: u32) -> u32 {
    table
        .get(index as usize)
        .map(|(value, _)| *value)
        .unwrap_or(table[0].0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_size_defaults_select_the_standard_rows() {
        // Upstream defaults are 64 (game) and 48 (folder), both labelled
        // "Standard" — a mismatch here would silently show "None".
        assert_eq!(
            GAME_ICON_SIZES[index_by_value(GAME_ICON_SIZES, 64) as usize].1,
            "Standard (64x64)"
        );
        assert_eq!(
            FOLDER_ICON_SIZES[index_by_value(FOLDER_ICON_SIZES, 48) as usize].1,
            "Standard (48x48)"
        );
    }

    #[test]
    fn index_and_value_round_trip() {
        let idx = index_by_value(SCREENSHOT_RESOLUTIONS, 1080);
        assert_eq!(value_at(SCREENSHOT_RESOLUTIONS, idx), 1080);
    }

    #[test]
    fn unknown_value_falls_back_to_first_row() {
        assert_eq!(index_by_value(GAME_ICON_SIZES, 99), 0);
        assert_eq!(value_at(GAME_ICON_SIZES, 999), 0);
    }
}
