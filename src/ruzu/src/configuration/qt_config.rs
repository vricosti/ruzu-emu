// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust counterpart of
// `/home/vricosti/Dev/emulators/eden/src/yuzu/configuration/qt_config.cpp`
// (`Config::ReadUIValues` / `Config::SaveUIValues` and the Qt-owned control
// values).
//
// Upstream persists `UISettings::values.game_dirs` with
// `QSettings::beginWriteArray("gamedirs")`, which writes a `size` key plus one
// group of keys per entry. `beginReadArray` then iterates `0..size` and ignores
// higher-numbered keys, so removing a directory only rewrites `size` and leaves
// the old group behind. Both halves of that behaviour matter and are reproduced
// here.
//
// The file written is **ruzu's own** config (`RuzuPath::ConfigDir`), never
// yuzu's — legacy configurations are read once by
// `crate::user_data_migration` through the copy-only migration worker and are
// never written to afterwards.

use std::io;
use std::path::{Path, PathBuf};

use common::fs::path_util::{get_ruzu_path, RuzuPath};
use common::settings_input::{
    native_analog, native_button, native_motion, ControllerType, PlayerInput,
    JOYCON_BODY_NEON_BLUE, JOYCON_BODY_NEON_RED, JOYCON_BUTTONS_NEON_BLUE, JOYCON_BUTTONS_NEON_RED,
};
use frontend_common::config::{BaseConfig, ConfigType};
use input_common::main_common::{generate_analog_param_from_keys, generate_keyboard_param};

use crate::uisettings::{self, GameDir};

/// Key prefix for every game-directory setting.
const GAMEDIRS_PREFIX: &str = "Paths\\gamedirs\\";

/// Key prefix for upstream `Settings::values.external_content_dirs`.
const EXTERNAL_CONTENT_DIRS_PREFIX: &str = "Paths\\external_content_dirs\\";

/// Upstream `QtConfig::ReadUIGamelistValues` opens the `UiGameList` category and
/// reads a `favorites` array of `program_id` entries. `TranslateCategory` renders
/// that category as `UIGameList`, and the group is a key prefix inside `[UI]`
/// rather than a section of its own — the same shape as `Paths\gamedirs\`.
const FAVORITES_PREFIX: &str = "UIGameList\\favorites\\";

/// The INI section the game-directory keys live in. Upstream's `QSettings`
/// group for the whole UI config is `UI`, and the `Paths\` part is a key
/// prefix inside it, not a section of its own.
const UI_SECTION: &str = "[UI]";

/// Path of ruzu's own configuration file.
pub fn config_path() -> PathBuf {
    get_ruzu_path(RuzuPath::ConfigDir).join("qt-config.ini")
}

/// Read the generic global categories through upstream's `Config::ReadValues`
/// owner. Qt-owned controls are loaded separately after this pass, matching
/// `QtConfig::ReadQtValues` ordering.
pub fn load_global_values() {
    let path = config_path();
    let mut config = BaseConfig::new(ConfigType::GlobalConfig);
    config.initialize(&path);
}

/// Persist the generic global categories through upstream's
/// `Config::SaveValues` owner. Qt-owned controls and UI values are written by
/// their specialized writers after this pass.
pub fn save_global_values() -> io::Result<()> {
    let path = config_path();
    let mut config = BaseConfig::new(ConfigType::GlobalConfig);
    // Upstream writes through the already-loaded, long-lived `QtConfig`
    // object. Reden reconstructs this adapter for each save, so load only the
    // INI document here: `initialize` would reload the old on-disk values into
    // `Settings::values` and discard the dialog changes before saving them.
    config.set_up_ini(&path);
    config.save_values();
    config.write_to_ini()
}

/// Read frontend shortcuts through upstream `QtConfig::ReadShortcutValues`.
pub fn load_shortcut_values() {
    let contents = std::fs::read_to_string(config_path()).unwrap_or_default();
    let shortcuts = parse_shortcut_values(&contents);
    uisettings::with_mut(|ui| ui.shortcuts = shortcuts);
}

fn parse_shortcut_values(contents: &str) -> Vec<uisettings::Shortcut> {
    let values = parse_section_values(&contents, "Shortcuts");
    uisettings::DEFAULT_HOTKEYS
        .iter()
        .map(|default| {
            let prefix = format!("{}\\{}", default.group, default.name);
            uisettings::Shortcut {
                name: default.name.to_owned(),
                group: default.group.to_owned(),
                keyseq: read_section_string_setting(
                    &values,
                    &format!("{prefix}\\KeySeq"),
                    default.keyseq,
                ),
                controller_keyseq: read_section_string_setting(
                    &values,
                    &format!("{prefix}\\Controller_KeySeq"),
                    default.controller_keyseq,
                ),
                // Upstream deliberately takes context from the default rather
                // than the INI because the historical serialized Qt enum was
                // ambiguous for WidgetWithChildrenShortcut.
                context: default.context,
                repeat: values
                    .get(&format!("{prefix}\\Repeat"))
                    .filter(|_| {
                        values
                            .get(&format!("{prefix}\\Repeat\\default"))
                            .is_some_and(|value| !is_true(value))
                    })
                    .map(|value| is_true(value))
                    .unwrap_or(default.repeat),
            }
        })
        .collect()
}

/// Persist frontend shortcuts through upstream `QtConfig::SaveShortcutValues`.
pub fn save_shortcut_values() -> io::Result<()> {
    let path = config_path();
    let mut contents = std::fs::read_to_string(&path).unwrap_or_default();
    let shortcuts = uisettings::with(|ui| ui.shortcuts.clone());
    for (shortcut, default) in shortcuts.iter().zip(uisettings::DEFAULT_HOTKEYS) {
        let prefix = format!("{}\\{}", shortcut.group, shortcut.name);
        contents = replace_section_setting(
            &contents,
            "Shortcuts",
            &format!("{prefix}\\KeySeq"),
            &shortcut.keyseq,
            shortcut.keyseq == default.keyseq,
        );
        contents = replace_section_setting(
            &contents,
            "Shortcuts",
            &format!("{prefix}\\Controller_KeySeq"),
            &shortcut.controller_keyseq,
            shortcut.controller_keyseq == default.controller_keyseq,
        );
        contents = replace_section_setting(
            &contents,
            "Shortcuts",
            &format!("{prefix}\\Context"),
            &shortcut.context.to_string(),
            shortcut.context == default.context,
        );
        contents = replace_section_setting(
            &contents,
            "Shortcuts",
            &format!("{prefix}\\Repeat"),
            &shortcut.repeat.to_string(),
            shortcut.repeat == default.repeat,
        );
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)
}

/// Persist the three settings owned by upstream `ConfigureTasDialog`.
pub fn save_tas_values() -> io::Result<()> {
    let path = config_path();
    let mut contents = std::fs::read_to_string(&path).unwrap_or_default();
    let values = common::settings::values();
    for (key, value, default) in [
        (
            "pause_tas_on_load",
            *values.pause_tas_on_load.get_value(),
            *values.pause_tas_on_load.get_default(),
        ),
        (
            "tas_enable",
            *values.tas_enable.get_value(),
            *values.tas_enable.get_default(),
        ),
        (
            "tas_loop",
            *values.tas_loop.get_value(),
            *values.tas_loop.get_default(),
        ),
    ] {
        contents = replace_section_setting(
            &contents,
            "Controls",
            key,
            &value.to_string(),
            value == default,
        );
    }
    drop(values);
    contents = replace_section_setting(
        &contents,
        "Data%20Storage",
        "tas_directory",
        &common::fs::path_util::get_ruzu_path_string(RuzuPath::TASDir),
        false,
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)
}

fn replace_section_setting(
    contents: &str,
    section: &str,
    key: &str,
    value: &str,
    is_default: bool,
) -> String {
    let mut lines: Vec<String> = contents.lines().map(str::to_owned).collect();
    let header = format!("[{section}]");
    let start = lines.iter().position(|line| line.trim() == header);
    let start = match start {
        Some(start) => start,
        None => {
            if !lines.is_empty() && !lines.last().is_some_and(String::is_empty) {
                lines.push(String::new());
            }
            lines.push(header);
            lines.len() - 1
        }
    };
    let end = lines[start + 1..]
        .iter()
        .position(|line| line.trim().starts_with('['))
        .map_or(lines.len(), |offset| start + 1 + offset);
    let assignments = [
        (format!("{key}\\default="), is_default.to_string()),
        (format!("{key}="), value.to_owned()),
    ];
    let mut insert_at = end;
    for (prefix, rendered) in assignments {
        if let Some(index) = (start + 1..end).find(|&index| lines[index].starts_with(&prefix)) {
            lines[index] = format!("{prefix}{rendered}");
        } else {
            lines.insert(insert_at, format!("{prefix}{rendered}"));
            insert_at += 1;
        }
    }
    let mut output = lines.join("\n");
    output.push('\n');
    output
}

/// Read `UISettings::values.language`, the `Paths\language` entry owned by
/// upstream `Config::ReadUIValues`.
pub fn load_ui_language() {
    let contents = std::fs::read_to_string(config_path()).unwrap_or_default();
    let language = read_ui_string_setting(&contents, "Paths\\language", "");
    uisettings::with_mut(|values| values.language.set_value(language));
}

/// Read upstream's checkable `View` action state from `Category::Ui`.
pub fn load_view_values() {
    let contents = std::fs::read_to_string(config_path()).unwrap_or_default();
    let ui = parse_section_values(&contents, "UI");
    uisettings::with_mut(|values| {
        values.single_window_mode.set_value(read_ui_bool_setting(
            &ui,
            "singleWindowMode",
            *values.single_window_mode.get_default(),
        ));
        values.fullscreen.set_value(read_ui_bool_setting(
            &ui,
            "fullscreen",
            *values.fullscreen.get_default(),
        ));
        values.display_titlebar.set_value(read_ui_bool_setting(
            &ui,
            "displayTitleBars",
            *values.display_titlebar.get_default(),
        ));
        values.show_filter_bar.set_value(read_ui_bool_setting(
            &ui,
            "showFilterBar",
            *values.show_filter_bar.get_default(),
        ));
        values.show_status_bar.set_value(read_ui_bool_setting(
            &ui,
            "showStatusBar",
            *values.show_status_bar.get_default(),
        ));
        values.enable_gamemode.set_value(read_ui_bool_setting(
            &ui,
            "enable_gamemode",
            *values.enable_gamemode.get_default(),
        ));
        #[cfg(unix)]
        {
            values.gui_force_x11.set_value(read_ui_bool_setting(
                &ui,
                "gui_force_x11",
                *values.gui_force_x11.get_default(),
            ));
            values
                .gui_hide_backend_warning
                .set_value(read_ui_bool_setting(
                    &ui,
                    "gui_hide_backend_warning",
                    *values.gui_hide_backend_warning.get_default(),
                ));
        }
    });
}

/// Read the three Direct Connect fields owned by upstream
/// `QtConfig::ReadMultiplayerValues`.
pub fn load_multiplayer_values() {
    let contents = std::fs::read_to_string(config_path()).unwrap_or_default();
    let values = parse_section_values(&contents, UI_SECTION);
    uisettings::with_mut(|ui| {
        ui.multiplayer_nickname.set_value(read_ui_string_setting(
            &contents,
            "Multiplayer\\nickname",
            ui.multiplayer_nickname.get_default(),
        ));
        ui.multiplayer_filter_text.set_value(read_ui_string_setting(
            &contents,
            "Multiplayer\\filter_text",
            ui.multiplayer_filter_text.get_default(),
        ));
        ui.multiplayer_filter_games_owned
            .set_value(read_ui_bool_setting(
                &values,
                "Multiplayer\\filter_games_owned",
                *ui.multiplayer_filter_games_owned.get_default(),
            ));
        ui.multiplayer_filter_hide_empty
            .set_value(read_ui_bool_setting(
                &values,
                "Multiplayer\\filter_games_hide_empty",
                *ui.multiplayer_filter_hide_empty.get_default(),
            ));
        ui.multiplayer_filter_hide_full
            .set_value(read_ui_bool_setting(
                &values,
                "Multiplayer\\filter_games_hide_full",
                *ui.multiplayer_filter_hide_full.get_default(),
            ));
        ui.multiplayer_ip.set_value(read_ui_string_setting(
            &contents,
            "Multiplayer\\ip",
            ui.multiplayer_ip.get_default(),
        ));
        ui.multiplayer_port.set_value(read_ui_u32_setting(
            &values,
            "Multiplayer\\port",
            *ui.multiplayer_port.get_default(),
        ));
    });
}

/// Persist the three Direct Connect fields through upstream
/// `QtConfig::SaveMultiplayerValues`'s `Category::Multiplayer` writer.
pub fn save_multiplayer_values() -> io::Result<()> {
    let path = config_path();
    let mut contents = std::fs::read_to_string(&path).unwrap_or_default();
    uisettings::with(|ui| {
        contents = replace_ui_string_setting(
            &contents,
            "Multiplayer\\nickname",
            ui.multiplayer_nickname.get_value(),
            ui.multiplayer_nickname.get_default(),
        );
        contents = replace_ui_string_setting(
            &contents,
            "Multiplayer\\filter_text",
            ui.multiplayer_filter_text.get_value(),
            ui.multiplayer_filter_text.get_default(),
        );
        for (key, setting) in [
            (
                "Multiplayer\\filter_games_owned",
                &ui.multiplayer_filter_games_owned,
            ),
            (
                "Multiplayer\\filter_games_hide_empty",
                &ui.multiplayer_filter_hide_empty,
            ),
            (
                "Multiplayer\\filter_games_hide_full",
                &ui.multiplayer_filter_hide_full,
            ),
        ] {
            let value = *setting.get_value();
            contents = replace_section_setting(
                &contents,
                "UI",
                key,
                if value { "true" } else { "false" },
                value == *setting.get_default(),
            );
        }
        contents = replace_ui_string_setting(
            &contents,
            "Multiplayer\\ip",
            ui.multiplayer_ip.get_value(),
            ui.multiplayer_ip.get_default(),
        );
        let port = *ui.multiplayer_port.get_value();
        contents = replace_section_setting(
            &contents,
            "UI",
            "Multiplayer\\port",
            &port.to_string(),
            port == *ui.multiplayer_port.get_default(),
        );
    });
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)
}

/// Persist frontend UI values through upstream `QtConfig::SaveUIValues`'s
/// generic `Category::Ui` / `Category::UiGeneral` writer.
pub fn save_view_values() -> io::Result<()> {
    let path = config_path();
    let mut contents = std::fs::read_to_string(&path).unwrap_or_default();
    uisettings::with(|values| {
        for (key, value, default) in [
            (
                "singleWindowMode",
                *values.single_window_mode.get_value(),
                *values.single_window_mode.get_default(),
            ),
            (
                "fullscreen",
                *values.fullscreen.get_value(),
                *values.fullscreen.get_default(),
            ),
            (
                "displayTitleBars",
                *values.display_titlebar.get_value(),
                *values.display_titlebar.get_default(),
            ),
            (
                "showFilterBar",
                *values.show_filter_bar.get_value(),
                *values.show_filter_bar.get_default(),
            ),
            (
                "showStatusBar",
                *values.show_status_bar.get_value(),
                *values.show_status_bar.get_default(),
            ),
            (
                "enable_gamemode",
                *values.enable_gamemode.get_value(),
                *values.enable_gamemode.get_default(),
            ),
        ] {
            contents =
                replace_section_setting(&contents, "UI", key, &value.to_string(), value == default);
        }
        #[cfg(unix)]
        for (key, value, default) in [
            (
                "gui_force_x11",
                *values.gui_force_x11.get_value(),
                *values.gui_force_x11.get_default(),
            ),
            (
                "gui_hide_backend_warning",
                *values.gui_hide_backend_warning.get_value(),
                *values.gui_hide_backend_warning.get_default(),
            ),
        ] {
            contents =
                replace_section_setting(&contents, "UI", key, &value.to_string(), value == default);
        }
    });
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)
}

fn read_ui_bool_setting(
    values: &std::collections::BTreeMap<String, String>,
    key: &str,
    default: bool,
) -> bool {
    if values
        .get(&format!("{key}\\default"))
        .is_none_or(|value| is_true(value))
    {
        default
    } else {
        values
            .get(key)
            .map(|value| is_true(value))
            .unwrap_or(default)
    }
}

fn read_ui_u32_setting(
    values: &std::collections::BTreeMap<String, String>,
    key: &str,
    default: u32,
) -> u32 {
    if values
        .get(&format!("{key}\\default"))
        .is_none_or(|value| is_true(value))
    {
        return default;
    }
    values
        .get(key)
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(default)
}

/// Persist the selected interface locale through upstream's
/// `Config::SaveUIValues` key and default marker.
pub fn save_ui_language() -> io::Result<()> {
    let path = config_path();
    let contents = std::fs::read_to_string(&path).unwrap_or_default();
    let language = uisettings::with(|values| values.language.get_value().clone());
    let updated = replace_ui_string_setting(&contents, "Paths\\language", &language, "");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, updated)
}

fn read_ui_string_setting(contents: &str, key: &str, default: &str) -> String {
    let values = parse_section_values(contents, UI_SECTION);
    if values
        .get(&format!("{key}\\default"))
        .is_none_or(|value| is_true(value))
    {
        return default.to_string();
    }
    values
        .get(key)
        .map(|value| unquote(value).to_string())
        .unwrap_or_else(|| default.to_string())
}

fn read_section_string_setting(
    values: &std::collections::BTreeMap<String, String>,
    key: &str,
    default: &str,
) -> String {
    if values
        .get(&format!("{key}\\default"))
        .is_none_or(|value| is_true(value))
    {
        return default.to_owned();
    }
    values
        .get(key)
        .map(|value| unquote(value).to_owned())
        .unwrap_or_else(|| default.to_owned())
}

fn replace_ui_string_setting(contents: &str, key: &str, value: &str, default: &str) -> String {
    let default_key = format!("{key}\\default");
    let rendered = [
        format!("{default_key}={}", value == default),
        format!("{key}={value}"),
    ];
    let mut output = Vec::new();
    let mut in_ui = false;
    let mut saw_ui = false;
    let mut written = false;

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if in_ui && !written {
                output.extend(rendered.iter().cloned());
                written = true;
            }
            in_ui = trimmed == UI_SECTION;
            saw_ui |= in_ui;
            output.push(line.to_string());
            continue;
        }
        let owned = trimmed
            .split_once('=')
            .is_some_and(|(found, _)| found.trim() == key || found.trim() == default_key);
        if in_ui && owned {
            if !written {
                output.extend(rendered.iter().cloned());
                written = true;
            }
            continue;
        }
        output.push(line.to_string());
    }

    if !written {
        if !saw_ui {
            if !output.is_empty() {
                output.push(String::new());
            }
            output.push(UI_SECTION.to_string());
        }
        output.extend(rendered);
    }

    let mut text = output.join("\n");
    text.push('\n');
    text
}

fn parse_section_values(
    contents: &str,
    section: &str,
) -> std::collections::BTreeMap<String, String> {
    let mut values = std::collections::BTreeMap::new();
    let mut in_section = false;
    let section_header = if section.starts_with('[') {
        section.to_string()
    } else {
        format!("[{section}]")
    };
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_section = trimmed == section_header;
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once('=') {
            values.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    values
}

/// Read the configured game directories — upstream `Config::ReadUIValues`'s
/// `gamedirs` array.
pub fn load_game_dirs() -> Vec<GameDir> {
    match std::fs::read_to_string(config_path()) {
        Ok(contents) => parse_game_dirs(&contents),
        Err(_) => Vec::new(),
    }
}

/// Read `Settings::values.external_content_dirs` from the QSettings array
/// owned by upstream `QtConfig::ReadUIValues`.
pub fn load_external_content_dirs() {
    let directories = match std::fs::read_to_string(config_path()) {
        Ok(contents) => parse_external_content_dirs(&contents),
        Err(_) => Vec::new(),
    };
    common::settings::values_mut().external_content_dirs = directories;
}

/// Persist `Settings::values.external_content_dirs` through upstream
/// `QtConfig::SaveUIValues`'s `external_content_dirs` array.
pub fn save_external_content_dirs(directories: &[String]) -> io::Result<()> {
    let path = config_path();
    let contents = std::fs::read_to_string(&path).unwrap_or_default();
    let updated = replace_external_content_dirs(&contents, directories);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, updated)
}

pub fn parse_external_content_dirs(contents: &str) -> Vec<String> {
    use std::collections::BTreeMap;

    let mut size: Option<u32> = None;
    let mut directories = BTreeMap::new();
    for line in contents.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let Some(rest) = key.strip_prefix(EXTERNAL_CONTENT_DIRS_PREFIX) else {
            continue;
        };
        let Some((index, field)) = rest.split_once('\\') else {
            if rest == "size" {
                size = value.trim().parse().ok();
            }
            continue;
        };
        let Ok(index) = index.parse::<u32>() else {
            continue;
        };
        if field == "path" && !value.is_empty() {
            directories.insert(index, value.to_string());
        }
    }
    directories
        .into_iter()
        .filter(|(index, _)| size.is_none_or(|size| *index <= size))
        .map(|(_, path)| path)
        .collect()
}

fn replace_external_content_dirs(contents: &str, directories: &[String]) -> String {
    let had_trailing_newline = contents.is_empty() || contents.ends_with('\n');
    let is_external_dir_line = |line: &str| {
        line.trim()
            .split_once('=')
            .is_some_and(|(key, _)| key.starts_with(EXTERNAL_CONTENT_DIRS_PREFIX))
    };

    let mut out = Vec::new();
    let mut block_written = false;
    for line in contents.lines() {
        if is_external_dir_line(line) {
            if !block_written {
                out.extend(render_external_content_dirs(directories));
                block_written = true;
            }
            continue;
        }
        out.push(line.to_string());
    }
    if !block_written {
        if !out.iter().any(|line| line.trim() == UI_SECTION) {
            if !out.is_empty() {
                out.push(String::new());
            }
            out.push(UI_SECTION.to_string());
        }
        out.extend(render_external_content_dirs(directories));
    }

    let mut text = out.join("\n");
    if had_trailing_newline && !text.is_empty() {
        text.push('\n');
    }
    text
}

fn render_external_content_dirs(directories: &[String]) -> Vec<String> {
    let mut lines = Vec::with_capacity(directories.len() + 1);
    lines.push(format!(
        "{EXTERNAL_CONTENT_DIRS_PREFIX}size={}",
        directories.len()
    ));
    for (position, path) in directories.iter().enumerate() {
        lines.push(format!(
            "{EXTERNAL_CONTENT_DIRS_PREFIX}{}\\path={path}",
            position + 1
        ));
    }
    lines
}

/// Read game-directory paths from another yuzu-schema frontend config without
/// loading any of its other settings.
pub fn load_game_dirs_from(path: &Path) -> io::Result<Vec<GameDir>> {
    std::fs::read_to_string(path).map(|contents| parse_game_dirs(&contents))
}

/// Merge only the source frontend's configured game-directory paths into
/// Ruzu. Existing Ruzu entries win when the same path is already present.
pub fn import_game_dirs_from(path: &Path) -> io::Result<usize> {
    let source = load_game_dirs_from(path)?;
    let (merged, imported) = merge_game_dirs(load_game_dirs(), source);
    save_game_dirs(&merged)?;
    Ok(imported)
}

fn merge_game_dirs(mut existing: Vec<GameDir>, source: Vec<GameDir>) -> (Vec<GameDir>, usize) {
    let mut imported = 0;
    for directory in source {
        if existing
            .iter()
            .any(|current| current.path == directory.path)
        {
            continue;
        }
        existing.push(directory);
        imported += 1;
    }
    (existing, imported)
}

/// Persist `dirs` back into ruzu's config — upstream `Config::SaveUIValues`.
///
/// Every other key in the file is preserved byte-for-byte: only the
/// `Paths\gamedirs\…` lines are replaced, in place, at the position the first
/// one occupied.
pub fn save_game_dirs(dirs: &[GameDir]) -> io::Result<()> {
    let path = config_path();
    let contents = std::fs::read_to_string(&path).unwrap_or_default();
    let updated = replace_game_dirs(&contents, dirs);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, updated)
}

/// Read the favorited program IDs — upstream `QtConfig::ReadUIGamelistValues`.
pub fn load_favorited_ids() -> Vec<u64> {
    match std::fs::read_to_string(config_path()) {
        Ok(contents) => parse_favorited_ids(&contents),
        Err(_) => Vec::new(),
    }
}

/// Read upstream `UISettings::values.favorites_expanded` from UiGameList.
pub fn load_favorites_expanded() {
    let contents = std::fs::read_to_string(config_path()).unwrap_or_default();
    let ui = parse_section_values(&contents, UI_SECTION);
    uisettings::with_mut(|values| {
        let default = *values.favorites_expanded.get_default();
        values.favorites_expanded.set_value(read_ui_bool_setting(
            &ui,
            "UIGameList\\favorites_expanded",
            default,
        ));
    });
}

/// Persist upstream `UISettings::values.favorites_expanded` in UiGameList.
pub fn save_favorites_expanded() -> io::Result<()> {
    let path = config_path();
    let contents = std::fs::read_to_string(&path).unwrap_or_default();
    let (value, default) = uisettings::with(|values| {
        (
            *values.favorites_expanded.get_value(),
            *values.favorites_expanded.get_default(),
        )
    });
    let updated = replace_section_setting(
        &contents,
        "UI",
        "UIGameList\\favorites_expanded",
        &value.to_string(),
        value == default,
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, updated)
}

/// Persist `ids` back into ruzu's config — upstream `QtConfig::SaveUIGamelistValues`.
///
/// As with [`save_game_dirs`], every other key is preserved byte-for-byte and the
/// rewritten block takes the position of the first old line.
pub fn save_favorited_ids(ids: &[u64]) -> io::Result<()> {
    let path = config_path();
    let contents = std::fs::read_to_string(&path).unwrap_or_default();
    let updated = replace_favorited_ids(&contents, ids);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, updated)
}

/// Upstream reads the array with `ReadUnsignedIntegerSetting`, so the on-disk value
/// is an unsigned decimal program ID.
pub fn parse_favorited_ids(contents: &str) -> Vec<u64> {
    use std::collections::BTreeMap;

    let mut size: Option<u32> = None;
    let mut ids: BTreeMap<u32, u64> = BTreeMap::new();

    for line in contents.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let Some(rest) = key.strip_prefix(FAVORITES_PREFIX) else {
            continue;
        };
        let Some((index_str, field)) = rest.split_once('\\') else {
            if rest == "size" {
                size = value.trim().parse().ok();
            }
            continue;
        };
        let Ok(index) = index_str.parse::<u32>() else {
            continue;
        };
        // `…\default` suffixes are metadata, not the value.
        if field == "program_id" {
            if let Ok(program_id) = value.trim().parse::<u64>() {
                ids.insert(index, program_id);
            }
        }
    }

    ids.into_iter()
        // yuzu's arrays are 1-based on disk, so `size = N` covers 1..=N.
        .filter(|(index, _)| size.is_none_or(|size| *index <= size))
        .map(|(_, program_id)| program_id)
        .collect()
}

/// Return `contents` with its `UIGameList\favorites\…` lines replaced by `ids`.
fn replace_favorited_ids(contents: &str, ids: &[u64]) -> String {
    let had_trailing_newline = contents.is_empty() || contents.ends_with('\n');

    let is_favorite_line = |line: &str| {
        line.trim()
            .split_once('=')
            .is_some_and(|(key, _)| key.starts_with(FAVORITES_PREFIX))
    };

    let mut out: Vec<String> = Vec::new();
    let mut block_written = false;
    for line in contents.lines() {
        if is_favorite_line(line) {
            if !block_written {
                out.extend(render_favorited_ids(ids));
                block_written = true;
            }
            continue;
        }
        out.push(line.to_string());
    }

    if !block_written {
        if !out.iter().any(|line| line.trim() == UI_SECTION) {
            if !out.is_empty() {
                out.push(String::new());
            }
            out.push(UI_SECTION.to_string());
        }
        out.extend(render_favorited_ids(ids));
    }

    let mut text = out.join("\n");
    if had_trailing_newline && !text.is_empty() {
        text.push('\n');
    }
    text
}

fn render_favorited_ids(ids: &[u64]) -> Vec<String> {
    let mut lines = Vec::with_capacity(ids.len() + 1);
    lines.push(format!("{FAVORITES_PREFIX}size={}", ids.len()));
    for (position, program_id) in ids.iter().enumerate() {
        // 1-based on disk, matching what yuzu writes.
        lines.push(format!(
            "{FAVORITES_PREFIX}{}\\program_id={program_id}",
            position + 1
        ));
    }
    lines
}

/// The INI section the per-player control bindings live in.
const CONTROLS_SECTION: &str = "[Controls]";

const QT_KEY_LEFT: i32 = 0x0100_0012;
const QT_KEY_UP: i32 = 0x0100_0013;
const QT_KEY_RIGHT: i32 = 0x0100_0014;
const QT_KEY_DOWN: i32 = 0x0100_0015;
const QT_KEY_SHIFT: i32 = 0x0100_0020;

/// Upstream `QtConfig::default_buttons`.
pub(super) const DEFAULT_BUTTONS: [i32; native_button::NUM_BUTTONS] = [
    b'C' as i32,
    b'X' as i32,
    b'V' as i32,
    b'Z' as i32,
    b'F' as i32,
    b'G' as i32,
    b'Q' as i32,
    b'E' as i32,
    b'R' as i32,
    b'T' as i32,
    b'M' as i32,
    b'N' as i32,
    QT_KEY_LEFT,
    QT_KEY_UP,
    QT_KEY_RIGHT,
    QT_KEY_DOWN,
    b'Q' as i32,
    b'E' as i32,
    0,
    0,
    b'Q' as i32,
    b'E' as i32,
];

/// Upstream `QtConfig::default_motions`.
pub(super) const DEFAULT_MOTIONS: [i32; native_motion::NUM_MOTIONS] = [b'7' as i32, b'8' as i32];

/// Upstream `QtConfig::default_analogs`.
pub(super) const DEFAULT_ANALOGS: [[i32; 4]; native_analog::NUM_ANALOGS] = [
    [b'W' as i32, b'S' as i32, b'A' as i32, b'D' as i32],
    [b'I' as i32, b'K' as i32, b'J' as i32, b'L' as i32],
];

/// Upstream `QtConfig::default_stick_mod`.
pub(super) const DEFAULT_STICK_MOD: [i32; native_analog::NUM_ANALOGS] = [QT_KEY_SHIFT, 0];

/// Read every player's bindings — upstream `QtConfig::ReadQtPlayerValues`,
/// called once per player from `Config::ReadControlValues`.
pub fn load_control_values() {
    // A missing file is the first launch, and it still has to go through the
    // loop below: upstream's `ReadBooleanSetting` applies its `player_index == 0`
    // default whether or not the file exists, and `PlayerInput::default()`
    // starts every player disconnected. Returning early here left player 1
    // unusable until the user ticked the box by hand.
    let contents = std::fs::read_to_string(config_path()).unwrap_or_default();
    let values = parse_controls(&contents);

    let mut settings = common::settings::values_mut();
    let players = settings.players.get_value_mut();
    for (index, player) in players.iter_mut().enumerate() {
        load_player_values(player, index, &values);
    }
}

fn load_player_values(
    player: &mut PlayerInput,
    index: usize,
    values: &std::collections::BTreeMap<String, String>,
) {
    let prefix = format!("player_{index}_");
    load_player_bindings(player, &prefix, values);

    player.connected = read_bool(values, &format!("{prefix}connected"), index == 0);
    player.controller_type = values
        .get(&format!("{prefix}type"))
        .and_then(|value| value.parse::<u8>().ok())
        .and_then(|value| ControllerType::try_from(value).ok())
        .unwrap_or(ControllerType::ProController);
    player.vibration_enabled = read_bool(values, &format!("{prefix}vibration_enabled"), true);
    player.vibration_strength = read_number(values, &format!("{prefix}vibration_strength"), 100);
    player.body_color_left = read_number(
        values,
        &format!("{prefix}body_color_left"),
        JOYCON_BODY_NEON_BLUE,
    );
    player.body_color_right = read_number(
        values,
        &format!("{prefix}body_color_right"),
        JOYCON_BODY_NEON_RED,
    );
    player.button_color_left = read_number(
        values,
        &format!("{prefix}button_color_left"),
        JOYCON_BUTTONS_NEON_BLUE,
    );
    player.button_color_right = read_number(
        values,
        &format!("{prefix}button_color_right"),
        JOYCON_BUTTONS_NEON_RED,
    );
    player.profile_name = values
        .get(&format!("{prefix}profile_name"))
        .cloned()
        .unwrap_or_default();
}

/// Upstream `QtConfig::ReadQtPlayerValues`: bindings use a `player_N_`
/// prefix in the global config and no prefix in an input-profile config.
fn load_player_bindings(
    player: &mut PlayerInput,
    prefix: &str,
    values: &std::collections::BTreeMap<String, String>,
) {
    for (slot, name) in native_button::MAPPING.iter().enumerate() {
        let default = generate_keyboard_param(DEFAULT_BUTTONS[slot]);
        player.buttons[slot] = values
            .get(&format!("{prefix}{name}"))
            .filter(|value| !value.is_empty())
            .cloned()
            .unwrap_or(default);
    }

    for (slot, name) in native_analog::MAPPING.iter().enumerate() {
        let keys = DEFAULT_ANALOGS[slot];
        let default = generate_analog_param_from_keys(
            keys[0],
            keys[1],
            keys[2],
            keys[3],
            DEFAULT_STICK_MOD[slot],
            0.5,
        );
        player.analogs[slot] = values
            .get(&format!("{prefix}{name}"))
            .filter(|value| !value.is_empty())
            .cloned()
            .unwrap_or(default);
    }

    for (slot, name) in native_motion::MAPPING.iter().enumerate() {
        let default = generate_keyboard_param(DEFAULT_MOTIONS[slot]);
        player.motions[slot] = values
            .get(&format!("{prefix}{name}"))
            .filter(|value| !value.is_empty())
            .cloned()
            .unwrap_or(default);
    }
}

fn read_bool(
    values: &std::collections::BTreeMap<String, String>,
    key: &str,
    default: bool,
) -> bool {
    values
        .get(key)
        .map(|value| value == "true" || value == "1")
        .unwrap_or(default)
}

fn read_number<T>(values: &std::collections::BTreeMap<String, String>, key: &str, default: T) -> T
where
    T: std::str::FromStr,
{
    values
        .get(key)
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// Persist every player's bindings — upstream `QtConfig::SaveQtPlayerValues`.
///
/// Only the `[Controls]` keys this function owns are rewritten; every other
/// line in the file is preserved, the same way `save_game_dirs` leaves the rest
/// of the INI alone.
pub fn save_control_values() -> io::Result<()> {
    let path = config_path();
    let contents = std::fs::read_to_string(&path).unwrap_or_default();

    let mut entries: Vec<(String, String)> = Vec::new();
    {
        let settings = common::settings::values();
        for (index, player) in settings.players.get_value().iter().enumerate() {
            append_player_entries(&mut entries, index, player);
        }
    }

    let updated = replace_controls(&contents, &entries);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, updated)
}

/// Load the custom player-profile selection and bindings for one title.
/// Maps to `QtConfig(ConfigType::PerGameConfig)` plus
/// `Config::ReadControlValues` / `QtConfig::ReadQtControlPlayerValues`.
pub fn load_per_game_control_values(path: &std::path::Path) {
    let contents = std::fs::read_to_string(path).unwrap_or_default();
    let values = parse_controls(&contents);

    let mut settings = common::settings::values_mut();
    settings.players.set_global(false);
    let global_players = settings.players.get_value_explicit(true).clone();
    let players = settings.players.get_value_mut();
    for (index, player) in players.iter_mut().enumerate() {
        let profile_key = format!("player_{index}_profile_name");
        let profile_name = values.get(&profile_key).cloned().unwrap_or_default();
        if profile_name.is_empty() {
            *player = global_players[index].clone();
            player.profile_name.clear();
        } else {
            load_player_values(player, index, &values);
        }
    }
}

/// Save only players that select a custom profile for this title.
/// Upstream `Config::SavePlayerValues` returns before writing when the profile
/// name is empty in a per-game configuration.
pub fn save_per_game_control_values(path: &std::path::Path) -> io::Result<()> {
    let contents = std::fs::read_to_string(path).unwrap_or_default();
    let mut entries = Vec::new();
    {
        let settings = common::settings::values();
        for (index, player) in settings.players.get_value().iter().enumerate() {
            if !player.profile_name.is_empty() {
                append_player_entries(&mut entries, index, player);
            }
        }
    }

    let updated = replace_controls(&contents, &entries);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, updated)
}

/// Upstream `QtConfig::ReadQtControlPlayerValues` for
/// `ConfigType::InputProfile`.
pub(crate) fn load_input_profile(contents: &str, player: &mut PlayerInput) {
    let values = parse_profile_controls(contents);
    load_player_bindings(player, "", &values);
    player.controller_type = values
        .get("type")
        .and_then(|value| value.parse::<u8>().ok())
        .and_then(|value| ControllerType::try_from(value).ok())
        .unwrap_or(ControllerType::ProController);
}

/// Upstream `QtConfig::SaveQtControlPlayerValues` for
/// `ConfigType::InputProfile`.
pub(crate) fn serialize_input_profile(player: &PlayerInput) -> String {
    let mut entries = Vec::new();
    entries.push((
        "type".to_string(),
        (player.controller_type as u8).to_string(),
    ));
    for (slot, name) in native_button::MAPPING.iter().enumerate() {
        entries.push((name.to_string(), player.buttons[slot].clone()));
    }
    for (slot, name) in native_analog::MAPPING.iter().enumerate() {
        entries.push((name.to_string(), player.analogs[slot].clone()));
    }
    for (slot, name) in native_motion::MAPPING.iter().enumerate() {
        entries.push((name.to_string(), player.motions[slot].clone()));
    }
    replace_controls("", &entries)
}

fn append_player_entries(entries: &mut Vec<(String, String)>, index: usize, player: &PlayerInput) {
    let prefix = format!("player_{index}_");
    for (slot, name) in native_button::MAPPING.iter().enumerate() {
        entries.push((format!("{prefix}{name}"), player.buttons[slot].clone()));
    }
    for (slot, name) in native_analog::MAPPING.iter().enumerate() {
        entries.push((format!("{prefix}{name}"), player.analogs[slot].clone()));
    }
    for (slot, name) in native_motion::MAPPING.iter().enumerate() {
        entries.push((format!("{prefix}{name}"), player.motions[slot].clone()));
    }
    entries.push((format!("{prefix}connected"), player.connected.to_string()));
    entries.push((
        format!("{prefix}type"),
        (player.controller_type as u8).to_string(),
    ));
    entries.push((
        format!("{prefix}vibration_enabled"),
        player.vibration_enabled.to_string(),
    ));
    entries.push((
        format!("{prefix}vibration_strength"),
        player.vibration_strength.to_string(),
    ));
    entries.push((
        format!("{prefix}body_color_left"),
        player.body_color_left.to_string(),
    ));
    entries.push((
        format!("{prefix}body_color_right"),
        player.body_color_right.to_string(),
    ));
    entries.push((
        format!("{prefix}button_color_left"),
        player.button_color_left.to_string(),
    ));
    entries.push((
        format!("{prefix}button_color_right"),
        player.button_color_right.to_string(),
    ));
    entries.push((format!("{prefix}profile_name"), player.profile_name.clone()));
}

/// Parse the `player_N_…` keys out of the `[Controls]` section.
///
/// Upstream writes each binding as a `key\default=` line followed by the value,
/// quoted because the parameter string contains commas. Both the quotes and the
/// companion `\default` line are handled here; keys whose `\default` is `true`
/// still carry their value, so they are read like any other.
pub fn parse_controls(contents: &str) -> std::collections::BTreeMap<String, String> {
    let mut values = std::collections::BTreeMap::new();
    let mut in_section = false;

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_section = trimmed == CONTROLS_SECTION;
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim();
        // `key\default=` is metadata about the neighbouring key, not a binding.
        if key.ends_with("\\default") || !key.starts_with("player_") {
            continue;
        }
        values.insert(key.to_string(), unquote(value.trim()).to_string());
    }

    values
}

fn parse_profile_controls(contents: &str) -> std::collections::BTreeMap<String, String> {
    let mut values = std::collections::BTreeMap::new();
    let mut in_section = false;

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_section = trimmed == CONTROLS_SECTION;
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.ends_with("\\default") {
            continue;
        }
        values.insert(key.to_string(), unquote(value.trim()).to_string());
    }
    values
}

/// Replace the `player_N_…` lines of `contents` with `entries`.
///
/// The rewritten block is dropped where the first existing binding sat, so a
/// hand-edited file keeps its shape; a file with no `[Controls]` section at all
/// gains one at the end.
pub fn replace_controls(contents: &str, entries: &[(String, String)]) -> String {
    let is_binding = |line: &str| {
        let trimmed = line.trim();
        let Some((key, _)) = trimmed.split_once('=') else {
            return false;
        };
        key.trim().starts_with("player_")
    };

    let rendered: Vec<String> = entries
        .iter()
        .flat_map(|(key, value)| {
            // Upstream's `WriteStringSetting` emits the `\default` marker first;
            // a binding written from the dialog is never the built-in default.
            [
                format!("{key}\\default=false"),
                format!("{key}=\"{value}\""),
            ]
        })
        .collect();

    let mut output: Vec<String> = Vec::new();
    let mut in_section = false;
    let mut written = false;
    let mut saw_section = false;

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            // Leaving `[Controls]` without having met a binding: append here so
            // the keys land in their own section rather than the next one.
            if in_section && !written {
                output.extend(rendered.iter().cloned());
                written = true;
            }
            in_section = trimmed == CONTROLS_SECTION;
            saw_section |= in_section;
            output.push(line.to_string());
            continue;
        }
        if in_section && is_binding(line) {
            if !written {
                output.extend(rendered.iter().cloned());
                written = true;
            }
            continue;
        }
        output.push(line.to_string());
    }

    if !written {
        if !saw_section {
            output.push(CONTROLS_SECTION.to_string());
        }
        output.extend(rendered);
    }

    let mut text = output.join("\n");
    text.push('\n');
    text
}

/// Strip the surrounding quotes yuzu writes around values containing commas.
fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(value)
}

/// Parse the `Paths\gamedirs\…` block of a yuzu-schema INI.
///
/// `Paths\gamedirs\size` is authoritative: entries numbered above it are stale
/// leftovers from a previously longer array and must be ignored, exactly as
/// `QSettings::beginReadArray` ignores them. A stale entry nested inside a live
/// one would otherwise make every game under it appear twice.
pub fn parse_game_dirs(contents: &str) -> Vec<GameDir> {
    use std::collections::BTreeMap;

    let mut size: Option<u32> = None;
    let mut paths: BTreeMap<u32, String> = BTreeMap::new();
    let mut deep: BTreeMap<u32, bool> = BTreeMap::new();
    let mut expanded: BTreeMap<u32, bool> = BTreeMap::new();

    for line in contents.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let Some(rest) = key.strip_prefix(GAMEDIRS_PREFIX) else {
            continue;
        };
        let Some((index_str, field)) = rest.split_once('\\') else {
            if rest == "size" {
                size = value.trim().parse().ok();
            }
            continue;
        };
        let Ok(index) = index_str.parse::<u32>() else {
            continue;
        };
        // `…\default` suffixes record whether the value is at its default; they
        // are metadata, not the value, and must not override it.
        match field {
            "path" => {
                paths.insert(index, value.to_owned());
            }
            "deep_scan" => {
                deep.insert(index, is_true(value));
            }
            "expanded" => {
                expanded.insert(index, is_true(value));
            }
            _ => {}
        }
    }

    paths
        .into_iter()
        // yuzu's arrays are 1-based on disk, so `size = N` covers 1..=N.
        .filter(|(index, _)| size.is_none_or(|size| *index <= size))
        .map(|(index, path)| GameDir {
            path,
            deep_scan: deep.get(&index).copied().unwrap_or(false),
            expanded: expanded.get(&index).copied().unwrap_or(true),
        })
        .collect()
}

/// Return `contents` with its `Paths\gamedirs\…` lines replaced by `dirs`.
fn replace_game_dirs(contents: &str, dirs: &[GameDir]) -> String {
    let had_trailing_newline = contents.is_empty() || contents.ends_with('\n');

    let is_gamedir_line = |line: &str| {
        line.trim()
            .split_once('=')
            .is_some_and(|(key, _)| key.starts_with(GAMEDIRS_PREFIX))
    };

    let mut out: Vec<String> = Vec::new();
    let mut block_written = false;
    for line in contents.lines() {
        if is_gamedir_line(line) {
            // Emit the whole new block where the first old line sat, and drop
            // every other old line.
            if !block_written {
                out.extend(render_game_dirs(dirs));
                block_written = true;
            }
            continue;
        }
        out.push(line.to_string());
    }

    if !block_written {
        // No existing block: append under `[UI]`, creating the section if the
        // file does not have one yet.
        if !out.iter().any(|line| line.trim() == UI_SECTION) {
            if !out.is_empty() {
                out.push(String::new());
            }
            out.push(UI_SECTION.to_string());
        }
        out.extend(render_game_dirs(dirs));
    }

    let mut text = out.join("\n");
    if had_trailing_newline && !text.is_empty() {
        text.push('\n');
    }
    text
}

/// Render the `Paths\gamedirs\…` lines for `dirs`, in upstream's key order and
/// with the `…\default` markers `QSettings` writes alongside each value.
fn render_game_dirs(dirs: &[GameDir]) -> Vec<String> {
    let mut lines = Vec::with_capacity(dirs.len() * 5 + 1);
    lines.push(format!("{GAMEDIRS_PREFIX}size={}", dirs.len()));
    for (position, dir) in dirs.iter().enumerate() {
        // 1-based on disk, matching what yuzu writes.
        let index = position + 1;
        lines.push(format!("{GAMEDIRS_PREFIX}{index}\\path={}", dir.path));
        lines.push(format!(
            "{GAMEDIRS_PREFIX}{index}\\deep_scan\\default={}",
            !dir.deep_scan
        ));
        lines.push(format!(
            "{GAMEDIRS_PREFIX}{index}\\deep_scan={}",
            dir.deep_scan
        ));
        lines.push(format!(
            "{GAMEDIRS_PREFIX}{index}\\expanded\\default={}",
            dir.expanded
        ));
        lines.push(format!(
            "{GAMEDIRS_PREFIX}{index}\\expanded={}",
            dir.expanded
        ));
    }
    lines
}

/// INI booleans, which yuzu writes as `true` / `false` but older configs may
/// carry as `1` / `0`.
fn is_true(value: &str) -> bool {
    matches!(value.trim(), "true" | "1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_reader_honors_default_markers_and_custom_empty_bindings() {
        let input = concat!(
            "[Shortcuts]\n",
            "Main Window\\Audio Mute/Unmute\\KeySeq\\default=false\n",
            "Main Window\\Audio Mute/Unmute\\KeySeq=Ctrl+F12\n",
            "Main Window\\Audio Volume Down\\KeySeq\\default=true\n",
            "Main Window\\Audio Volume Down\\KeySeq=F1\n",
            "Main Window\\Audio Volume Up\\KeySeq\\default=false\n",
            "Main Window\\Audio Volume Up\\KeySeq=\n",
        );
        let shortcuts = parse_shortcut_values(input);

        assert_eq!(shortcuts[0].keyseq, "Ctrl+F12");
        assert_eq!(shortcuts[1].keyseq, "-");
        assert_eq!(shortcuts[2].keyseq, "");
    }

    #[test]
    fn view_boolean_settings_honor_default_markers() {
        let values = parse_section_values(
            "[UI]\nshowFilterBar\\default=false\nshowFilterBar=false\nshowStatusBar\\default=true\nshowStatusBar=false\n",
            "UI",
        );
        assert!(!read_ui_bool_setting(&values, "showFilterBar", true));
        assert!(read_ui_bool_setting(&values, "showStatusBar", true));
        assert!(read_ui_bool_setting(&values, "missing", true));
    }

    #[test]
    fn view_boolean_writer_uses_upstream_ui_keys() {
        let updated = replace_section_setting("", "UI", "showFilterBar", "false", false);
        assert!(updated.contains("[UI]"));
        assert!(updated.contains("showFilterBar\\default=false"));
        assert!(updated.contains("showFilterBar=false"));
    }

    #[test]
    fn linux_backend_preferences_use_upstream_ui_keys() {
        let updated = replace_section_setting("", "UI", "gui_force_x11", "true", false);
        let updated =
            replace_section_setting(&updated, "UI", "gui_hide_backend_warning", "true", false);
        let values = parse_section_values(&updated, "UI");
        assert!(read_ui_bool_setting(&values, "gui_force_x11", false));
        assert!(read_ui_bool_setting(
            &values,
            "gui_hide_backend_warning",
            false
        ));
    }

    #[test]
    fn gamemode_uses_current_upstream_ui_general_key() {
        let updated = replace_section_setting("", "UI", "enable_gamemode", "false", false);
        let values = parse_section_values(&updated, "UI");
        assert!(!read_ui_bool_setting(&values, "enable_gamemode", true));
        assert!(!updated.contains("[Linux]"));
    }

    #[test]
    fn favorites_expanded_uses_upstream_game_list_key() {
        let key = "UIGameList\\favorites_expanded";
        let updated = replace_section_setting("", "UI", key, "false", false);
        let values = parse_section_values(&updated, "UI");

        assert!(!read_ui_bool_setting(&values, key, true));
        assert!(updated.contains("UIGameList\\favorites_expanded\\default=false"));
        assert!(updated.contains("UIGameList\\favorites_expanded=false"));
    }

    #[test]
    fn tas_setting_replacement_preserves_sections_and_default_marker() {
        let original = "[Controls]\ntas_enable\\default=true\ntas_enable=false\n[UI]\nfoo=bar\n";
        let updated = replace_section_setting(original, "Controls", "tas_enable", "true", false);

        assert!(updated.contains("tas_enable\\default=false"));
        assert!(updated.contains("tas_enable=true"));
        assert!(updated.contains("[UI]\nfoo=bar"));
        assert_eq!(updated.matches("tas_enable=").count(), 1);
    }

    /// The binding a real Xbox pad produces, as yuzu writes it.
    const SDL_BINDING: &str = "engine:sdl,port:0,guid:030000005e040000000b000015050000,button:1";

    #[test]
    fn ui_language_honors_default_marker() {
        assert_eq!(
            read_ui_string_setting(
                "[UI]\nPaths\\language\\default=false\nPaths\\language=fr\n",
                "Paths\\language",
                "",
            ),
            "fr"
        );
        assert_eq!(
            read_ui_string_setting(
                "[UI]\nPaths\\language\\default=true\nPaths\\language=fr\n",
                "Paths\\language",
                "",
            ),
            ""
        );
    }

    #[test]
    fn ui_language_replacement_preserves_other_sections() {
        let updated = replace_ui_string_setting(
            "[UI]\nPaths\\language\\default=false\nPaths\\language=en\n[System]\nlanguage_index=2\n",
            "Paths\\language",
            "ja_JP",
            "",
        );
        assert!(updated.contains("Paths\\language\\default=false"));
        assert!(updated.contains("Paths\\language=ja_JP"));
        assert!(updated.contains("[System]\nlanguage_index=2"));
        assert_eq!(updated.matches("Paths\\language=").count(), 1);
    }

    /// Upstream defaults `player.connected` to `player_index == 0`, and applies
    /// that default whether or not a config file exists. Bailing out on a
    /// missing file left player 1 disconnected on a fresh install.
    #[test]
    fn player_one_is_connected_on_a_first_launch() {
        let values = parse_controls("");
        let mut player_one = PlayerInput::default();
        let mut player_two = PlayerInput::default();
        load_player_values(&mut player_one, 0, &values);
        load_player_values(&mut player_two, 1, &values);

        assert!(player_one.connected);
        assert!(!player_two.connected);
        assert!(player_one.buttons.iter().all(|binding| !binding.is_empty()));
        assert!(player_one.analogs.iter().all(|binding| !binding.is_empty()));
        assert!(player_one.motions.iter().all(|binding| !binding.is_empty()));

        let button_a = common::param_package::ParamPackage::from_serialized(&player_one.buttons[0]);
        assert_eq!(button_a.get_str("engine", ""), "keyboard");
        assert_eq!(button_a.get_int("code", -1), i32::from(b'C'));
    }

    /// A stored `connected` beats the default, in both directions.
    #[test]
    fn a_stored_connected_flag_wins_over_the_default() {
        let values =
            parse_controls("[Controls]\nplayer_0_connected=false\nplayer_1_connected=true\n");
        let mut players = std::array::from_fn::<_, 3, _>(|_| PlayerInput::default());
        for (index, player) in players.iter_mut().enumerate() {
            load_player_values(player, index, &values);
        }
        assert!(!players[0].connected);
        assert!(players[1].connected);
        assert!(!players[2].connected);
    }

    #[test]
    fn player_metadata_survives_a_save_and_reload() {
        let mut source = PlayerInput::default();
        source.connected = true;
        source.controller_type = ControllerType::GameCube;
        source.vibration_enabled = false;
        source.vibration_strength = 63;
        source.body_color_left = 1;
        source.body_color_right = 2;
        source.button_color_left = 3;
        source.button_color_right = 4;
        source.profile_name = "arcade".to_string();
        source.buttons[0] = SDL_BINDING.to_string();

        let mut entries = Vec::new();
        append_player_entries(&mut entries, 0, &source);
        let values = parse_controls(&replace_controls("", &entries));
        let mut loaded = PlayerInput::default();
        load_player_values(&mut loaded, 0, &values);

        assert!(loaded.connected);
        assert_eq!(loaded.controller_type, ControllerType::GameCube);
        assert!(!loaded.vibration_enabled);
        assert_eq!(loaded.vibration_strength, 63);
        assert_eq!(loaded.body_color_left, 1);
        assert_eq!(loaded.body_color_right, 2);
        assert_eq!(loaded.button_color_left, 3);
        assert_eq!(loaded.button_color_right, 4);
        assert_eq!(loaded.profile_name, "arcade");
        assert_eq!(loaded.buttons[0], SDL_BINDING);
    }

    #[test]
    fn input_profile_uses_unprefixed_control_keys() {
        let mut source = PlayerInput::default();
        source.controller_type = ControllerType::GameCube;
        source.buttons[0] = SDL_BINDING.to_string();

        let written = serialize_input_profile(&source);
        assert!(written.contains("type=\"5\""));
        assert!(written.contains(&format!("button_a=\"{SDL_BINDING}\"")));
        assert!(!written.contains("player_0_"));

        let mut loaded = PlayerInput::default();
        load_input_profile(&written, &mut loaded);
        assert_eq!(loaded.controller_type, ControllerType::GameCube);
        assert_eq!(loaded.buttons[0], SDL_BINDING);
    }

    #[test]
    fn control_bindings_survive_a_save_and_reload() {
        // The whole point of the Controls page: what the dialog wrote must come
        // back byte-for-byte on the next launch.
        let entries = vec![
            ("player_0_button_a".to_string(), SDL_BINDING.to_string()),
            (
                "player_0_lstick".to_string(),
                "engine:sdl,axis_x:0,axis_y:1,offset_x:-0.03".to_string(),
            ),
        ];
        let written = replace_controls("", &entries);
        let parsed = parse_controls(&written);

        assert_eq!(
            parsed.get("player_0_button_a").map(String::as_str),
            Some(SDL_BINDING)
        );
        assert_eq!(
            parsed.get("player_0_lstick").map(String::as_str),
            Some("engine:sdl,axis_x:0,axis_y:1,offset_x:-0.03")
        );
    }

    #[test]
    fn bindings_are_quoted_so_their_commas_survive() {
        // A parameter string is a comma-separated list; written bare it would
        // still round-trip through this parser but would break every other INI
        // reader, yuzu's included.
        let entries = vec![("player_0_button_a".to_string(), SDL_BINDING.to_string())];
        let written = replace_controls("", &entries);
        assert!(written.contains(&format!("player_0_button_a=\"{SDL_BINDING}\"")));
        // Upstream pairs each key with its `\default` marker.
        assert!(written.contains("player_0_button_a\\default=false"));
    }

    #[test]
    fn saving_controls_leaves_the_rest_of_the_file_alone() {
        // `save_control_values` shares the config file with every other
        // setting; clobbering a neighbouring section would lose them.
        let original = "[UI]\nPaths\\gamedirs\\size=1\n[Controls]\nplayer_0_button_a\\default=false\nplayer_0_button_a=\"old\"\ntouchscreen_enabled=true\n[Core]\nuse_multi_core=true\n";
        let entries = vec![("player_0_button_a".to_string(), "new".to_string())];
        let updated = replace_controls(original, &entries);

        assert!(updated.contains("Paths\\gamedirs\\size=1"));
        assert!(updated.contains("touchscreen_enabled=true"));
        assert!(updated.contains("use_multi_core=true"));
        assert!(updated.contains("player_0_button_a=\"new\""));
        assert!(!updated.contains("\"old\""));
    }

    #[test]
    fn a_config_without_a_controls_section_gains_one() {
        let entries = vec![("player_0_button_a".to_string(), "x".to_string())];
        let updated = replace_controls("[UI]\nsomething=1\n", &entries);
        assert!(updated.contains("[Controls]"));
        assert!(updated.contains("something=1"));

        // And the new keys must land inside that section, not before it.
        let section = updated.find("[Controls]").unwrap();
        let key = updated.find("player_0_button_a").unwrap();
        assert!(key > section);
    }

    #[test]
    fn bindings_from_another_section_are_not_read_as_controls() {
        // `player_` keys only mean a binding inside [Controls]; a same-named key
        // elsewhere must not leak in.
        let contents =
            "[UI]\nplayer_0_button_a=\"decoy\"\n[Controls]\nplayer_0_button_b=\"real\"\n";
        let parsed = parse_controls(contents);
        assert!(!parsed.contains_key("player_0_button_a"));
        assert_eq!(
            parsed.get("player_0_button_b").map(String::as_str),
            Some("real")
        );
    }

    #[test]
    fn the_default_marker_is_not_mistaken_for_a_binding() {
        let contents = "[Controls]\nplayer_0_button_a\\default=false\nplayer_0_button_a=\"v\"\n";
        let parsed = parse_controls(contents);
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed.get("player_0_button_a").map(String::as_str),
            Some("v")
        );
    }

    /// A config with a stale 5th entry nested inside the 4th — the shape a
    /// removed-then-re-added game directory leaves behind.
    const CONFIG_WITH_STALE_ENTRY: &str = concat!(
        "[UI]\n",
        "Paths\\gamedirs\\size=4\n",
        "Paths\\gamedirs\\1\\path=SDMC\n",
        "Paths\\gamedirs\\1\\deep_scan=false\n",
        "Paths\\gamedirs\\2\\path=UserNAND\n",
        "Paths\\gamedirs\\3\\path=SysNAND\n",
        "Paths\\gamedirs\\4\\path=/games/roms\n",
        "Paths\\gamedirs\\4\\deep_scan\\default=false\n",
        "Paths\\gamedirs\\4\\deep_scan=true\n",
        "Paths\\gamedirs\\5\\path=/games/roms/Mario Kart 8 Deluxe [NSP]\n",
        "Paths\\gamedirs\\5\\deep_scan=false\n",
    );

    fn dir(path: &str, deep_scan: bool) -> GameDir {
        GameDir {
            path: path.to_string(),
            deep_scan,
            expanded: true,
        }
    }

    #[test]
    fn stale_entries_past_size_are_ignored() {
        let dirs = parse_game_dirs(CONFIG_WITH_STALE_ENTRY);
        assert_eq!(dirs.len(), 4);
        assert!(!dirs.iter().any(|d| d.path.contains("Mario Kart")));
    }

    #[test]
    fn provider_tokens_are_kept_but_flagged_as_non_paths() {
        let dirs = parse_game_dirs(CONFIG_WITH_STALE_ENTRY);
        assert!(!dirs[0].is_filesystem_path()); // SDMC
        assert!(dirs[3].is_filesystem_path()); // /games/roms
    }

    #[test]
    fn default_suffixed_keys_do_not_override_the_value() {
        // `…\deep_scan\default=false` must not win over `…\deep_scan=true`.
        let dirs = parse_game_dirs(CONFIG_WITH_STALE_ENTRY);
        assert!(dirs[3].deep_scan);
    }

    #[test]
    fn missing_size_keeps_every_entry() {
        let config = "Paths\\gamedirs\\1\\path=/a\nPaths\\gamedirs\\2\\path=/b\n";
        assert_eq!(parse_game_dirs(config).len(), 2);
    }

    #[test]
    fn round_trips_through_save_and_load() {
        let dirs = vec![dir("/games/a", true), dir("/games/b", false)];
        let written = replace_game_dirs("[UI]\n", &dirs);
        assert_eq!(parse_game_dirs(&written), dirs);
    }

    #[test]
    fn external_content_directories_round_trip_with_upstream_array_shape() {
        let directories = vec![
            "/updates/homebrew/".to_string(),
            "/dlc/open-source-title/".to_string(),
        ];
        let written = replace_external_content_dirs("[UI]\nUnrelated=value\n", &directories);
        assert_eq!(parse_external_content_dirs(&written), directories);
        assert!(written.contains("Paths\\external_content_dirs\\1\\path=/updates/homebrew/"));
        assert!(written.contains("Paths\\external_content_dirs\\2\\path=/dlc/open-source-title/"));
        assert!(written.contains("Unrelated=value"));
    }

    #[test]
    fn external_content_array_ignores_stale_entries_past_size() {
        let contents = concat!(
            "[UI]\n",
            "Paths\\external_content_dirs\\size=1\n",
            "Paths\\external_content_dirs\\1\\path=/updates/homebrew/\n",
            "Paths\\external_content_dirs\\2\\path=/stale/\n",
        );
        assert_eq!(
            parse_external_content_dirs(contents),
            vec!["/updates/homebrew/".to_string()]
        );
    }

    #[test]
    fn game_directory_import_merges_by_path_and_preserves_ruzu_values() {
        let existing = vec![dir("/games/homebrew", false)];
        let source = vec![
            dir("/games/homebrew", true),
            dir("/games/homebrew-extra", true),
        ];
        let (merged, imported) = merge_game_dirs(existing.clone(), source);
        assert_eq!(imported, 1);
        assert_eq!(merged[0], existing[0]);
        assert_eq!(merged[1], dir("/games/homebrew-extra", true));
    }

    #[test]
    fn writing_preserves_every_other_key() {
        let original = concat!(
            "[Controls]\n",
            "player_0_type=0\n",
            "[UI]\n",
            "Paths\\gamedirs\\size=1\n",
            "Paths\\gamedirs\\1\\path=/old\n",
            "Multiplayer\\nickname=vric\n",
        );
        let updated = replace_game_dirs(original, &[dir("/new", false)]);
        assert!(updated.contains("player_0_type=0"));
        assert!(updated.contains("Multiplayer\\nickname=vric"));
        assert!(updated.contains("[Controls]"));
        assert!(updated.contains("Paths\\gamedirs\\1\\path=/new"));
        assert!(!updated.contains("/old"));
    }

    #[test]
    fn multiplayer_fields_round_trip_with_upstream_ui_keys() {
        let mut contents = "[UI]\nUnrelated=value\n".to_string();
        contents = replace_ui_string_setting(&contents, "Multiplayer\\nickname", "Player One", "");
        contents =
            replace_ui_string_setting(&contents, "Multiplayer\\filter_text", "preferred room", "");
        contents = replace_section_setting(
            &contents,
            "UI",
            "Multiplayer\\filter_games_owned",
            "true",
            false,
        );
        contents = replace_section_setting(
            &contents,
            "UI",
            "Multiplayer\\filter_games_hide_empty",
            "true",
            false,
        );
        contents = replace_section_setting(
            &contents,
            "UI",
            "Multiplayer\\filter_games_hide_full",
            "false",
            false,
        );
        contents = replace_ui_string_setting(&contents, "Multiplayer\\ip", "room.example.org", "");
        contents = replace_section_setting(&contents, "UI", "Multiplayer\\port", "24873", false);

        let values = parse_section_values(&contents, UI_SECTION);
        assert_eq!(
            read_ui_string_setting(&contents, "Multiplayer\\nickname", ""),
            "Player One"
        );
        assert_eq!(
            read_ui_string_setting(&contents, "Multiplayer\\ip", ""),
            "room.example.org"
        );
        assert_eq!(
            read_ui_string_setting(&contents, "Multiplayer\\filter_text", ""),
            "preferred room"
        );
        assert!(read_ui_bool_setting(
            &values,
            "Multiplayer\\filter_games_owned",
            false
        ));
        assert!(read_ui_bool_setting(
            &values,
            "Multiplayer\\filter_games_hide_empty",
            false
        ));
        assert!(!read_ui_bool_setting(
            &values,
            "Multiplayer\\filter_games_hide_full",
            true
        ));
        assert_eq!(
            read_ui_u32_setting(&values, "Multiplayer\\port", 24872),
            24873
        );
        assert!(contents.contains("Unrelated=value"));
    }

    #[test]
    fn writing_removes_stale_entries_rather_than_leaving_them() {
        // The whole point of rewriting in place: entry 5 must not survive, or
        // the next reader with a larger `size` would pick it up again.
        let updated = replace_game_dirs(CONFIG_WITH_STALE_ENTRY, &[dir("/games/roms", true)]);
        assert!(!updated.contains("Mario Kart 8 Deluxe [NSP]"));
        assert_eq!(parse_game_dirs(&updated), vec![dir("/games/roms", true)]);
    }

    #[test]
    fn block_is_written_once_at_the_first_old_position() {
        let updated = replace_game_dirs(CONFIG_WITH_STALE_ENTRY, &[dir("/games/roms", true)]);
        assert_eq!(updated.matches("Paths\\gamedirs\\size=").count(), 1);
    }

    #[test]
    fn ui_section_is_created_when_absent() {
        let updated = replace_game_dirs("[Controls]\nplayer_0_type=0\n", &[dir("/a", false)]);
        assert!(updated.contains("[UI]"));
        // The new keys must land after the section header, not before it.
        let ui = updated.find("[UI]").unwrap();
        let key = updated.find("Paths\\gamedirs\\size=").unwrap();
        assert!(ui < key);
    }

    #[test]
    fn empty_list_writes_size_zero() {
        let updated = replace_game_dirs(CONFIG_WITH_STALE_ENTRY, &[]);
        assert!(updated.contains("Paths\\gamedirs\\size=0"));
        assert_eq!(parse_game_dirs(&updated), Vec::new());
    }

    #[test]
    fn indices_are_one_based_like_yuzu() {
        let updated = replace_game_dirs("[UI]\n", &[dir("/a", false), dir("/b", false)]);
        assert!(updated.contains("Paths\\gamedirs\\1\\path=/a"));
        assert!(updated.contains("Paths\\gamedirs\\2\\path=/b"));
        assert!(!updated.contains("Paths\\gamedirs\\0\\"));
    }
}
