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
use std::cell::{Cell, RefCell};
use std::rc::Rc;

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

/// ConfigureUi's two row combo boxes. Item IDs are independent of filtered
/// positions, equivalent to QComboBox itemData. Unlike upstream's rebuild via
/// findData(currentData()) (an index), preserve the actual ID across updates.
struct RowTextChoices {
    first: gtk::DropDown,
    second: gtk::DropDown,
    first_ids: RefCell<Vec<u8>>,
    second_ids: RefCell<Vec<u8>>,
    updating: Cell<bool>,
}

fn row_text_ids(first: bool, other: Option<u8>) -> Vec<u8> {
    (0..uisettings::GAME_LIST_ROW_TEXT.len() as u8)
        .filter(|id| (!first || *id != 4) && Some(*id) != other)
        .collect()
}

impl RowTextChoices {
    fn selected(&self, first: bool) -> Option<u8> {
        let (combo, ids) = if first { (&self.first, &self.first_ids) }
            else { (&self.second, &self.second_ids) };
        ids.borrow().get(combo.selected() as usize).copied()
    }

    fn update_row_combo(&self, first: bool, selected: u8, other: Option<u8>) {
        let ids = row_text_ids(first, other);
        let position = ids.iter().position(|id| *id == selected).unwrap_or(0) as u32;
        let labels: Vec<String> = ids.iter().map(|id|
            crate::i18n::tr(uisettings::GAME_LIST_ROW_TEXT[*id as usize])).collect();
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let (combo, stored) = if first { (&self.first, &self.first_ids) }
            else { (&self.second, &self.second_ids) };
        *stored.borrow_mut() = ids;
        combo.set_model(Some(&gtk::StringList::new(&refs)));
        combo.set_selected(position);
    }

    fn initialize(first: gtk::DropDown, second: gtk::DropDown, first_id: u8, second_id: u8) -> Rc<Self> {
        let choices = Rc::new(Self { first, second, first_ids: RefCell::new(Vec::new()),
            second_ids: RefCell::new(Vec::new()), updating: Cell::new(true) });
        choices.update_row_combo(true, first_id, None);
        choices.update_row_combo(false, second_id, choices.selected(true));
        choices.update_row_combo(true, choices.selected(true).unwrap(), choices.selected(false));
        choices.updating.set(false);
        for first_changed in [true, false] {
            let combo = if first_changed { &choices.first } else { &choices.second };
            let weak = Rc::downgrade(&choices);
            combo.connect_selected_notify(move |_| {
                let Some(choices) = weak.upgrade() else { return; };
                if choices.updating.get() || crate::i18n::is_retranslating() { return; }
                let Some(selected) = choices.selected(!first_changed) else { return; };
                choices.updating.set(true);
                choices.update_row_combo(!first_changed, selected, choices.selected(first_changed));
                choices.updating.set(false);
            });
        }
        choices
    }
}

/// PopulateResolutionComboBox: both console heights at every supported scale.
fn screenshot_resolutions() -> Vec<u32> {
    use ruzu_core::frontend::framebuffer_layout::{screen_docked, screen_undocked};
    let mut heights = std::collections::BTreeSet::new();
    for &(_, setup) in common::settings_enums::ResolutionSetup::canonicalizations() {
        let mut info = common::settings::ResolutionScalingInfo::default();
        common::settings::translate_resolution_info(setup, &mut info);
        for height in [screen_undocked::HEIGHT, screen_docked::HEIGHT] {
            heights.insert((height as f32 * info.up_factor) as u32);
        }
    }
    std::iter::once(0).chain(heights).collect()
}

/// Build the UI tab — upstream `ConfigureUi`.
#[cfg(test)]
fn page() -> Page {
    page_with_screenshot_info().0
}

pub(super) type ScreenshotInfoCallback = Rc<dyn Fn(common::settings_enums::AspectRatio, common::settings_enums::ResolutionSetup)>;

pub(super) fn page_with_screenshot_info() -> (Page, ScreenshotInfoCallback) {
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

    // GTK frontend extension: GIMP-style relative font scaling (50–200%).
    let (font_scale_row, font_scale) = w::spin_row(
        "Interface text size (%):",
        uisettings::with(|v| (*v.font_scale.get_value()).clamp(50, 200)) as f64,
        50.0, 200.0, 10.0, "",
    );
    general.append(&font_scale_row);

    column.append(&general_group);

    // --- "Game List" ------------------------------------------------------
    let (game_list_group, game_list) = w::group("Game List");

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
    let row_choices = RowTextChoices::initialize(row_1, row_2, row_1_index as u8, row_2_index as u8);

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

    let resolutions = screenshot_resolutions();
    let resolution_text: Vec<String> = resolutions.iter().map(|height| {
        if *height == 0 { crate::i18n::tr("Auto") } else { height.to_string() }
    }).collect();
    let resolution_labels: Vec<&str> = resolution_text.iter().map(String::as_str).collect();
    let resolution_index = uisettings::with(|v| {
        resolutions.iter().position(|height| height == v.screenshot_height.get_value()).unwrap_or(0) as u32
    });
    let (resolution_row, resolution) =
        w::combo_row("Resolution:", &resolution_labels, resolution_index);
    screenshots.append(&resolution_row);

    // ConfigureUi::UpdateScreenshotInfo / UpdateWidthText. Keep the draft
    // Graphics selections local to this dialog, without applying settings.
    let screenshot_info = {
        let values = common::settings::values();
        Rc::new(Cell::new((*values.aspect_ratio.get_value(), *values.resolution_setup.get_value())))
    };
    let dimensions = gtk::Label::new(None);
    dimensions.set_xalign(0.0);
    screenshots.append(&dimensions);
    let refresh: Rc<dyn Fn()> = {
        let resolution = resolution.downgrade();
        let info = screenshot_info.clone();
        let resolutions = resolutions.clone();
        Rc::new(move || {
            let Some(resolution) = resolution.upgrade() else { return };
            let height = resolutions.get(resolution.selected() as usize).copied().unwrap_or(0);
            let (ratio, setup) = info.get();
            dimensions.set_text(&screenshot_dimensions_text(height, ratio, setup));
        })
    };
    refresh();
    let on_height = refresh.clone();
    resolution.connect_selected_notify(move |_| on_height());
    let update_screenshot_info: ScreenshotInfoCallback = Rc::new(move |ratio, setup| {
        screenshot_info.set((ratio, setup));
        refresh();
    });

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

    let page = Page::new("UI", scroller, move || {
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
        let screenshot_height = resolutions.get(resolution.selected() as usize).copied().unwrap_or(0);

        let add_ons = show_add_ons.is_active();
        let size = show_size.is_active();
        let types = show_types.is_active();
        let play_time = show_play_time.is_active();
        let ask_where = save_as.is_active();
        let path = path_entry.text().to_string();
        let row_1_id = row_choices.selected(true).expect("first row always has choices");
        let row_2_id = row_choices.selected(false).expect("second row always has choices");

        uisettings::with_mut(|v| {
            v.theme.set_value(theme_name);
            v.font_scale.set_value(font_scale.value_as_int() as u32);
            v.language.set_value(language_code);
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
    });
    (page, update_screenshot_info)
}

fn screenshot_dimensions_text(height: u32, ratio: common::settings_enums::AspectRatio, setup: common::settings_enums::ResolutionSetup) -> String {
    use ruzu_core::frontend::framebuffer_layout::{screen_docked, screen_undocked};
    if height != 0 {
        return format!("{} x {}", uisettings::calculate_width(height, ratio), height);
    }
    let mut info = common::settings::ResolutionScalingInfo::default();
    common::settings::translate_resolution_info(setup, &mut info);
    let undocked = (screen_undocked::HEIGHT as f32 * info.up_factor) as u32;
    let docked = (screen_docked::HEIGHT as f32 * info.up_factor) as u32;
    format!("{} ({} x {}, {} x {})", crate::i18n::tr("Auto"),
        uisettings::calculate_width(undocked, ratio), undocked,
        uisettings::calculate_width(docked, ratio), docked)
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
    #[test]
    fn screenshot_preview_scales_only_automatic_height() {
        use common::settings_enums::{AspectRatio as A, ResolutionSetup as R};
        let fixed = super::screenshot_dimensions_text(541, A::R16_9, R::Res2X);
        assert_eq!(fixed, "961 x 541");
        assert!(super::screenshot_dimensions_text(0, A::R4_3, R::Res2X)
            .ends_with("(1920 x 1440, 2880 x 2160)"));
        assert!(super::screenshot_dimensions_text(0, A::R16_9, R::Res1_2X)
            .ends_with("(640 x 360, 960 x 540)"));
    }

    #[test]
    #[ignore = "requires GTK display; run alone with isolated XDG directories"]
    fn graphics_drafts_update_screenshot_preview_without_applying() {
        use gtk::prelude::*;
        use common::settings_enums::{AspectRatio as A, ResolutionSetup as R};
        fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
            let mut all = vec![widget.clone()];
            let mut child = widget.first_child();
            while let Some(current) = child {
                all.extend(descendants(&current));
                child = current.next_sibling();
            }
            all
        }
        gtk::init().unwrap();
        crate::i18n::set_language("en");
        common::settings::set_configuring_global(true);
        {
            let mut values = common::settings::values_mut();
            values.aspect_ratio.set_value(A::R16_9);
            values.resolution_setup.set_value(R::Res1X);
        }
        crate::uisettings::with_mut(|v| v.screenshot_height.set_value(0));
        let (ui, update) = super::page_with_screenshot_info();
        let graphics = crate::configuration::configure_graphics::page_with_screenshot_info(|| {}, false, update);
        let widgets = descendants(&graphics.widget);
        let combo = |label: &str| widgets.iter().find_map(|widget| {
            let text = widget.downcast_ref::<gtk::Label>()?;
            (text.label() == label).then(|| widget.next_sibling().and_downcast::<gtk::DropDown>()).flatten()
        }).unwrap();
        let aspect = combo("Aspect Ratio:");
        let resolution = combo("Resolution:");
        use crate::configuration::shared_translation as tr;
        aspect.set_selected(tr::index_of(tr::ASPECT_RATIO, &A::R4_3));
        resolution.set_selected(tr::index_of(tr::RESOLUTION_SETUP, &R::Res2X));
        assert!(descendants(&ui.widget).iter().any(|widget| widget.downcast_ref::<gtk::Label>()
            .is_some_and(|label| label.label().ends_with("(1920 x 1440, 2880 x 2160)"))));
        let values = common::settings::values();
        assert_eq!(*values.aspect_ratio.get_value(), A::R16_9);
        assert_eq!(*values.resolution_setup.get_value(), R::Res1X);
        drop(values);
        let height = descendants(&ui.widget).iter().find_map(|widget| {
            let label = widget.downcast_ref::<gtk::Label>()?;
            (label.label() == "Resolution:")
                .then(|| widget.next_sibling().and_downcast::<gtk::DropDown>()).flatten()
        }).unwrap();
        height.set_selected(super::screenshot_resolutions().iter().position(|&h| h == 720).unwrap() as u32);
        assert!(descendants(&ui.widget).iter().any(|widget| widget.downcast_ref::<gtk::Label>()
            .is_some_and(|label| label.label() == "960 x 720")));
        crate::uisettings::with(|v| assert_eq!(*v.screenshot_height.get_value(), 0));
    }

    #[test]
    #[ignore = "requires GTK display and isolated XDG directories; run alone"]
    fn ui_page_applies_row_ids_after_language_changes() {
        use gtk::prelude::*;
        fn find_row(widget: &gtk::Widget, label: &str) -> Option<gtk::DropDown> {
            if let Some(first) = widget.first_child() {
                if first.downcast_ref::<gtk::Label>().is_some_and(|text| text.label() == label) {
                    if let Some(combo) = first.next_sibling().and_downcast::<gtk::DropDown>() {
                        return Some(combo);
                    }
                }
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                if let Some(combo) = find_row(&current, label) { return Some(combo); }
                child = current.next_sibling();
            }
            None
        }
        gtk::init().unwrap();
        crate::i18n::set_language("en");
        crate::uisettings::with_mut(|v| {
            v.language.set_value("en".into());
            v.row_1_text_id.set_value(3);
            v.row_2_text_id.set_value(2);
        });
        let page = super::page();
        let first = find_row(&page.widget, "Row 1 Text:").unwrap();
        let second = find_row(&page.widget, "Row 2 Text:").unwrap();
        // ID 3 is at index 2 in the first filtered dropdown.
        assert_eq!(first.selected(), 2);
        second.set_selected(0); // Filename, ID 0.
        for locale in ["fr", "de", "en"] {
            crate::i18n::set_language(locale);
            crate::i18n::translate_widget_tree(&page.widget);
            (page.apply)();
            crate::uisettings::with(|v| {
                assert_eq!(*v.row_1_text_id.get_value(), 3);
                assert_eq!(*v.row_2_text_id.get_value(), 0);
            });
        }
        crate::configuration::qt_config::save_view_values().unwrap();
        crate::uisettings::with_mut(|v| {
            v.row_1_text_id.set_value(1);
            v.row_2_text_id.set_value(4);
        });
        crate::configuration::qt_config::load_view_values();
        crate::uisettings::with(|v| {
            assert_eq!(*v.row_1_text_id.get_value(), 3);
            assert_eq!(*v.row_2_text_id.get_value(), 0);
        });
    }

    #[test]
    fn row_choices_exclude_duplicates_and_none_only_from_first() {
        for other in 0..5 {
            let first = super::row_text_ids(true, Some(other));
            let second = super::row_text_ids(false, Some(other));
            assert!(!first.contains(&other));
            assert!(!first.contains(&4));
            assert!(!second.contains(&other));
            assert_eq!(second.contains(&4), other != 4);
        }
    }

    #[test]
    #[ignore = "requires GTK display; run alone"]
    fn filtered_row_choices_keep_semantic_ids_after_repeated_updates() {
        gtk::init().unwrap();
        let choices = super::RowTextChoices::initialize(
            gtk::DropDown::from_strings(&[]), gtk::DropDown::from_strings(&[]), 3, 2);
        assert_eq!(choices.selected(true), Some(3));
        for id in [0, 1, 2, 4, 0, 1] {
            let position = choices.second_ids.borrow().iter().position(|entry| *entry == id).unwrap();
            choices.second.set_selected(position as u32);
            assert_eq!(choices.selected(false), Some(id));
            assert_eq!(choices.selected(true), Some(3), "other row must retain its ID");
            assert!(!choices.first_ids.borrow().contains(&id));
        }
        for id in [0, 2, 3, 0] {
            let position = choices.first_ids.borrow().iter().position(|entry| *entry == id).unwrap();
            choices.first.set_selected(position as u32);
            assert_eq!(choices.selected(true), Some(id));
            assert_eq!(choices.selected(false), Some(1));
            assert!(!choices.second_ids.borrow().contains(&id));
        }
        // Old files could select None on the first row or duplicate IDs.
        for (first, second) in [(4, 4), (3, 3), (255, 255)] {
            let choices = super::RowTextChoices::initialize(
                gtk::DropDown::from_strings(&[]), gtk::DropDown::from_strings(&[]), first, second);
            assert!(choices.selected(true).unwrap() < 4);
            assert_ne!(choices.selected(true), choices.selected(false));
        }
    }

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
        assert_eq!(screenshot_resolutions(), vec![
            0, 180, 270, 360, 540, 720, 810, 900, 1080, 1350, 1440,
            1620, 2160, 2880, 3240, 3600, 4320, 5040, 5400, 5760,
            6480, 7560, 8640,
        ]);
    }

    #[test]
    fn unknown_value_falls_back_to_first_row() {
        assert_eq!(index_by_value(GAME_ICON_SIZES, 99), 0);
        assert_eq!(value_at(GAME_ICON_SIZES, 999), 0);
    }
}
