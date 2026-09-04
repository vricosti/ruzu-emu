// SPDX-FileCopyrightText: 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/frontend_common/config.h and config.cpp
//!
//! Provides the base `Config` trait and configuration management infrastructure
//! for reading/writing settings from INI files.

use std::collections::BTreeMap;
use std::path::Path;

use common::settings_common::InputSetting;
use common::settings_enums::{Category, ConsoleMode};
use common::settings_input::{
    ControllerType, PlayerInput, TouchFromButtonMap, JOYCON_BODY_NEON_BLUE, JOYCON_BODY_NEON_RED,
    JOYCON_BUTTONS_NEON_BLUE, JOYCON_BUTTONS_NEON_RED,
};
use common::settings_setting::BasicSetting;

// ---------------------------------------------------------------------------
// ConfigType
// ---------------------------------------------------------------------------

/// The type of configuration.
/// Maps to C++ `Config::ConfigType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigType {
    GlobalConfig,
    PerGameConfig,
    InputProfile,
}

// ---------------------------------------------------------------------------
// Special characters for output adjustment
// ---------------------------------------------------------------------------

/// Special characters that trigger quoting in output strings.
/// Maps to C++ `Config::special_characters`.
const SPECIAL_CHARACTERS: [char; 18] = [
    '!', '#', '$', '%', '^', '&', '*', '|', ';', '\'', '"', ',', '<', '>', '?', '`', '~', '=',
];

// ---------------------------------------------------------------------------
// Config trait
// ---------------------------------------------------------------------------

/// Base configuration management trait.
/// Maps to C++ `Config` class.
///
/// Derived config implementations must implement the platform-specific
/// read/save methods.
pub trait Config {
    /// Returns the config type.
    fn config_type(&self) -> ConfigType;

    /// Returns whether this is a global config.
    fn is_global(&self) -> bool {
        self.config_type() == ConfigType::GlobalConfig
    }

    /// Returns whether this is a custom (per-game) config.
    fn is_custom_config(&self) -> bool {
        self.config_type() == ConfigType::PerGameConfig
    }

    /// Returns the path to the configuration file.
    fn get_config_file_path(&self) -> &str;

    /// Checks if a key exists in the given section.
    fn exists(&self, section: &str, key: &str) -> bool;

    // -----------------------------------------------------------------------
    // Platform-specific methods (pure virtual in C++)
    // -----------------------------------------------------------------------

    /// Reload all values (platform-specific and global).
    fn reload_all_values(&mut self);

    /// Save all values (platform-specific and global).
    fn save_all_values(&mut self);

    fn read_hidbus_values(&mut self);
    fn read_debug_control_values(&mut self);
    fn read_path_values(&mut self);
    fn read_shortcut_values(&mut self);
    fn read_ui_values(&mut self);
    fn read_ui_gamelist_values(&mut self);
    fn read_ui_layout_values(&mut self);
    fn read_multiplayer_values(&mut self);

    fn save_hidbus_values(&mut self);
    fn save_debug_control_values(&mut self);
    fn save_path_values(&mut self);
    fn save_shortcut_values(&mut self);
    fn save_ui_values(&mut self);
    fn save_ui_gamelist_values(&mut self);
    fn save_ui_layout_values(&mut self);
    fn save_multiplayer_values(&mut self);
}

// ---------------------------------------------------------------------------
// Helper functions (static methods from C++ Config class)
// ---------------------------------------------------------------------------

/// Adjusts a key string by replacing `/` with `\` and spaces with `%20`.
/// Maps to C++ `Config::AdjustKey`.
pub fn adjust_key(key: &str) -> String {
    key.replace('/', "\\").replace(' ', "%20")
}

/// Adjusts an output string for INI serialization.
/// Maps to C++ `Config::AdjustOutputString`.
pub fn adjust_output_string(string: &str) -> String {
    let mut adjusted = string.replace('\\', "/");

    // Handle double-slash normalization (non-Android)
    if adjusted.starts_with("//") {
        adjusted = adjusted.replace("//", "/");
        adjusted.insert(0, '/');
    } else {
        adjusted = adjusted.replace("//", "/");
    }

    // Needed for backwards compatibility with QSettings deserialization
    for &ch in &SPECIAL_CHARACTERS {
        if adjusted.contains(ch) {
            adjusted.insert(0, '"');
            adjusted.push('"');
            break;
        }
    }
    adjusted
}

/// Converts a value to its string representation.
/// Maps to C++ `Config::ToString` template.
pub fn to_string_bool(value: bool) -> String {
    if value {
        "true".to_string()
    } else {
        "false".to_string()
    }
}

/// Converts an integer to string.
pub fn to_string_i64(value: i64) -> String {
    value.to_string()
}

/// Converts an unsigned integer to string.
pub fn to_string_u64(value: u64) -> String {
    value.to_string()
}

// ---------------------------------------------------------------------------
// BaseConfig (shared state for concrete Config implementations)
// ---------------------------------------------------------------------------

/// Shared base state for `Config` implementations.
/// Maps to the non-virtual data members of C++ `Config`.
///
/// Concrete implementations would embed this and delegate to it for the
/// common read/write/group/array logic.
pub struct BaseConfig {
    pub config_type: ConfigType,
    pub config_loc: String,
    pub global: bool,
    pub key_stack: Vec<String>,
    pub array_stack: Vec<ConfigArrayEntry>,
    ini: BTreeMap<String, BTreeMap<String, String>>,
}

/// Public version of ConfigArray for use in BaseConfig.
#[derive(Clone, Debug)]
pub struct ConfigArrayEntry {
    pub name: String,
    pub size: i32,
    pub index: i32,
}

impl BaseConfig {
    pub fn new(config_type: ConfigType) -> Self {
        Self {
            global: config_type == ConfigType::GlobalConfig,
            config_type,
            config_loc: String::new(),
            key_stack: Vec::new(),
            array_stack: Vec::new(),
            ini: BTreeMap::new(),
        }
    }

    /// Loads the INI document owned by the config instance.
    ///
    /// Maps to `Config::Initialize`, which performs `SetUpIni` then `Reload`.
    pub fn initialize(&mut self, config_path: &Path) {
        self.set_up_ini(config_path);
        if self.config_type != ConfigType::InputProfile {
            self.read_values();
            self.save_values();
            let _ = self.write_to_ini();
        }
    }

    /// Load the INI document without reloading the global settings singleton.
    ///
    /// This is upstream `Config::SetUpIni`. It is used by Reden's stateless
    /// save adapter to update the already-loaded document before invoking
    /// `SaveValues`, matching `QtConfig::SaveAllValues` on its long-lived
    /// `Config` instance.
    pub fn set_up_ini(&mut self, config_path: &Path) {
        self.config_loc = config_path.to_string_lossy().into_owned();
        let contents = std::fs::read_to_string(config_path).unwrap_or_default();
        self.load_ini(&contents);
    }

    /// Replaces the loaded INI document. Kept public for focused config tests.
    pub fn load_ini(&mut self, contents: &str) {
        self.ini.clear();
        let mut section = String::new();

        for raw_line in contents.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line
                .strip_prefix('[')
                .and_then(|line| line.strip_suffix(']'))
            {
                section = name.to_string();
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            self.ini
                .entry(section.clone())
                .or_default()
                .insert(key.trim().to_string(), value.trim().to_string());
        }
    }

    /// Begins a configuration group.
    pub fn begin_group(&mut self, group: &str) {
        assert!(
            self.array_stack.is_empty(),
            "Can't begin a group while reading/writing from a config array"
        );
        self.key_stack.push(adjust_key(group));
    }

    /// Ends the current configuration group.
    pub fn end_group(&mut self) {
        assert!(
            !self.key_stack.is_empty(),
            "Can't end a group if you haven't started one yet"
        );
        assert!(
            self.array_stack.is_empty(),
            "Can't end a group when reading/writing from a config array"
        );
        self.key_stack.pop();
    }

    /// Gets the current section (first key stack entry).
    pub fn get_section(&self) -> String {
        if self.key_stack.is_empty() {
            String::new()
        } else {
            self.key_stack[0].clone()
        }
    }

    /// Gets the current group path (key stack entries after the first).
    pub fn get_group(&self) -> String {
        if self.key_stack.len() <= 1 {
            return String::new();
        }
        let mut key = String::new();
        for i in 1..self.key_stack.len() {
            key.push_str(&self.key_stack[i]);
            key.push('\\');
        }
        key
    }

    /// Gets the full key including group and array context.
    pub fn get_full_key(&self, key: &str, skip_array_index: bool) -> String {
        if self.array_stack.is_empty() {
            return format!("{}{}", self.get_group(), adjust_key(key));
        }

        let mut array_key = String::new();
        for (i, entry) in self.array_stack.iter().enumerate() {
            if !entry.name.is_empty() {
                array_key.push_str(&entry.name);
                array_key.push('\\');
            }

            if !skip_array_index || (self.array_stack.len() - 1 != i && self.array_stack.len() > 1)
            {
                array_key.push_str(&entry.index.to_string());
                array_key.push('\\');
            }
        }
        format!("{}{}{}", self.get_group(), array_key, adjust_key(key))
    }

    fn read_raw(&self, key: &str) -> Option<&str> {
        let section = self.get_section();
        let full_key = self.get_full_key(key, false);
        self.ini
            .get(&section)
            .and_then(|values| values.get(&full_key))
            .map(String::as_str)
    }

    fn write_raw(&mut self, key: &str, value: String) {
        let section = self.get_section();
        let full_key = self.get_full_key(key, false);
        self.ini.entry(section).or_default().insert(full_key, value);
    }

    /// Maps to `Config::ReadSettingGeneric`.
    fn read_setting_generic(&self, setting: &mut dyn BasicSetting) {
        if !setting.save() || (!setting.switchable() && !self.global) {
            return;
        }

        let key = adjust_key(setting.label());
        let mut use_global = true;
        if setting.switchable() && !self.global {
            use_global = self.read_boolean_setting(&format!("{key}\\use_global"), Some(true));
            setting.set_global(use_global);
        }

        if self.global || !use_global {
            let is_default = self.read_boolean_setting(&format!("{key}\\default"), Some(true));
            if is_default {
                setting.load_string("");
            } else {
                let value = self.read_string_setting(&key, Some(&setting.default_to_string()));
                setting.load_string(&value);
            }
        }
    }

    /// Maps to `Config::WriteSettingGeneric`.
    fn write_setting_generic(&mut self, setting: &mut dyn BasicSetting) {
        if !setting.save() {
            return;
        }

        let key = adjust_key(setting.label());
        if setting.switchable() {
            if !self.global {
                self.write_raw(
                    &format!("{key}\\use_global"),
                    to_string_bool(setting.using_global()),
                );
            }
            if self.global || !setting.using_global() {
                let value = if self.global {
                    setting.to_string_global()
                } else {
                    setting.to_string_repr()
                };
                self.write_raw(
                    &format!("{key}\\default"),
                    to_string_bool(value == setting.default_to_string()),
                );
                self.write_raw(&key, adjust_output_string(&value));
            }
        } else if self.global {
            let value = setting.to_string_repr();
            self.write_raw(
                &format!("{key}\\default"),
                to_string_bool(value == setting.default_to_string()),
            );
            self.write_raw(&key, adjust_output_string(&value));
        }
    }

    /// Maps to `Config::ReadCategory`.
    pub fn read_category(&mut self, category: Category) {
        self.read_category_from_group(category.translate(), category);
    }

    /// Read `category` from an explicitly owned INI group.
    ///
    /// Most categories use `TranslateCategory(category)`, but upstream stores
    /// `Category::Network` inside the broader `[Services]` group.
    fn read_category_from_group(&mut self, group: &str, category: Category) {
        self.begin_group(group);
        {
            let mut values = common::settings::values_mut();
            values.for_each_setting_in_category_mut(category, |setting| {
                self.read_setting_generic(setting)
            });
        }
        self.end_group();
    }

    /// Maps to `Config::WriteCategory`.
    pub fn write_category(&mut self, category: Category) {
        self.write_category_to_group(category.translate(), category);
    }

    /// Write `category` to an explicitly owned INI group. See
    /// [`Self::read_category_from_group`].
    fn write_category_to_group(&mut self, group: &str, category: Category) {
        self.begin_group(group);
        {
            let mut values = common::settings::values_mut();
            values.for_each_setting_in_category_mut(category, |setting| {
                self.write_setting_generic(setting)
            });
        }
        self.end_group();
    }

    /// Read the categories shared by global and per-game configurations.
    /// Maps to `Config::ReadValues`'s non-global portion.
    pub fn read_values(&mut self) {
        if self.global {
            self.read_data_storage_values();
            self.read_debugging_values();
            self.read_disabled_add_on_values();
            self.read_category(Category::Services);
            self.read_category(Category::WebService);
            self.read_category(Category::Miscellaneous);
        }
        self.read_category(Category::LibraryApplet);
        self.read_category_from_group(Category::Services.translate(), Category::Network);
        self.read_control_values();
        for category in [
            Category::Core,
            Category::Cpu,
            Category::CpuDebug,
            Category::CpuUnsafe,
            Category::Linux,
            Category::Renderer,
            Category::RendererAdvanced,
            Category::RendererHacks,
            Category::RendererExtensions,
            Category::RendererDebug,
            Category::Audio,
            Category::System,
            Category::SystemAudio,
        ] {
            self.read_category(category);
        }
        self.migrate_legacy_split_renderer_backend();
    }

    /// Consume Reden's pre-parity `shader_backend` key once and fold it into
    /// Eden's single serialized `renderer_backend` enum. The first three
    /// backend discriminants were already identical, so only the legacy GLASM
    /// and SPIR-V side key needs migration.
    fn migrate_legacy_split_renderer_backend(&mut self) {
        let Some(renderer) = self.ini.get("Renderer") else {
            return;
        };
        let legacy_backend = renderer
            .get("shader_backend")
            .and_then(|value| value.parse::<u32>().ok());
        let uses_global = !self.global
            && renderer
                .get("shader_backend\\use_global")
                .is_none_or(|value| value == "true");
        let is_default = renderer
            .get("shader_backend\\default")
            .is_none_or(|value| value == "true");

        if !uses_global && !is_default {
            let migrated = match legacy_backend {
                Some(1) => Some(common::settings_enums::RendererBackend::OpenGlGlasm),
                Some(2) => Some(common::settings_enums::RendererBackend::OpenGlSpirV),
                _ => None,
            };
            if let Some(migrated) = migrated {
                let mut values = common::settings::values_mut();
                if *values.renderer_backend.get_value()
                    == common::settings_enums::RendererBackend::OpenGlGlsl
                {
                    values.renderer_backend.set_value(migrated);
                }
            }
        }

        if let Some(renderer) = self.ini.get_mut("Renderer") {
            renderer.remove("shader_backend");
            renderer.remove("shader_backend\\default");
            renderer.remove("shader_backend\\use_global");
        }
    }

    /// Maps to `Config::ReadDataStorageValues`.
    fn read_data_storage_values(&mut self) {
        use common::fs::path_util::{get_ruzu_path_string, set_ruzu_path, RuzuPath};

        self.begin_group(Category::DataStorage.translate());
        for (path, setting) in [
            (RuzuPath::NANDDir, "nand_directory"),
            (RuzuPath::SDMCDir, "sdmc_directory"),
            (RuzuPath::LoadDir, "load_directory"),
            (RuzuPath::DumpDir, "dump_directory"),
            (RuzuPath::TASDir, "tas_directory"),
        ] {
            let configured = self.read_string_setting(setting, None);
            set_ruzu_path(path, Path::new(&configured));
        }

        let save_directory = self.read_string_setting("save_directory", None);
        if save_directory.is_empty() {
            let nand_directory = get_ruzu_path_string(RuzuPath::NANDDir);
            set_ruzu_path(RuzuPath::SaveDir, Path::new(&nand_directory));
        } else {
            set_ruzu_path(RuzuPath::SaveDir, Path::new(&save_directory));
        }

        {
            let mut values = common::settings::values_mut();
            values.for_each_setting_in_category_mut(Category::DataStorage, |setting| {
                self.read_setting_generic(setting)
            });
        }
        self.end_group();
    }

    /// Maps to `Config::ReadDisabledAddOnValues`.
    fn read_disabled_add_on_values(&mut self) {
        self.begin_group("DisabledAddOns");
        let entry_count = self.begin_array("");
        for entry_index in 0..entry_count {
            self.set_array_index(entry_index);
            let title_id = self.read_unsigned_integer_setting("title_id", Some(0));
            let disabled_count = self.begin_array("disabled");
            let mut disabled = Vec::with_capacity(disabled_count as usize);
            for disabled_index in 0..disabled_count {
                self.set_array_index(disabled_index);
                disabled.push(self.read_string_setting("d", Some("")));
            }
            self.end_array();
            common::settings::values_mut()
                .disabled_addons
                .insert(title_id, disabled);
        }
        self.end_array();
        self.end_group();
    }

    /// Maps to `Config::ReadDebuggingValues`.
    fn read_debugging_values(&mut self) {
        self.begin_group(Category::Debugging.translate());
        common::settings::values_mut().record_frame_times =
            self.read_boolean_setting("record_frame_times", Some(false));
        {
            let mut values = common::settings::values_mut();
            for category in [Category::Debugging, Category::DebuggingGraphics] {
                values.for_each_setting_in_category_mut(category, |setting| {
                    self.read_setting_generic(setting)
                });
            }
        }
        self.end_group();
    }

    /// Maps to `Config::ReadControlValues`.
    fn read_control_values(&mut self) {
        self.read_category(Category::Controls);
        self.begin_group(Category::Controls.translate());

        let is_custom_config = self.config_type == ConfigType::PerGameConfig;
        {
            let mut values = common::settings::values_mut();
            values.players.set_global(!is_custom_config);
        }
        let player_count = common::settings::values().players.get_value().len();
        for player_index in 0..player_count {
            self.read_player_values(player_index);
        }

        let controller_type = common::settings::values().players.get_value()[0].controller_type;
        if controller_type == ControllerType::Handheld {
            let mut values = common::settings::values_mut();
            values.use_docked_mode.set_global(!is_custom_config);
            values.use_docked_mode.set_value(ConsoleMode::Handheld);
        }

        if is_custom_config {
            self.end_group();
            return;
        }

        self.read_touchscreen_values();
        self.read_motion_touch_values();
        self.end_group();
    }

    /// Maps to `Config::ReadTouchscreenValues`.
    fn read_touchscreen_values(&self) {
        let mut values = common::settings::values_mut();
        values.touchscreen.enabled = self.read_boolean_setting("touchscreen_enabled", Some(true));
        values.touchscreen.rotation_angle =
            self.read_integer_setting("touchscreen_angle", Some(0)) as u32;
        values.touchscreen.diameter_x =
            self.read_integer_setting("touchscreen_diameter_x", Some(90)) as u32;
        values.touchscreen.diameter_y =
            self.read_integer_setting("touchscreen_diameter_y", Some(90)) as u32;
    }

    /// Maps to `Config::ReadMotionTouchValues`.
    fn read_motion_touch_values(&mut self) {
        let mut maps = Vec::new();
        let mut map_count = self.begin_array("touch_from_button_maps");
        if map_count > 0 {
            for map_index in 0..map_count {
                self.set_array_index(map_index);
                let name = self.read_string_setting("name", Some("default"));
                let entry_count = self.begin_array("entries");
                let mut buttons = Vec::with_capacity(entry_count as usize);
                for entry_index in 0..entry_count {
                    self.set_array_index(entry_index);
                    buttons.push(self.read_string_setting("bind", None));
                }
                self.end_array();
                maps.push(TouchFromButtonMap { name, buttons });
            }
        } else {
            maps.push(TouchFromButtonMap {
                name: "default".to_string(),
                buttons: Vec::new(),
            });
            map_count = 1;
        }
        self.end_array();

        let mut values = common::settings::values_mut();
        values.touch_from_button_maps = maps;
        let selected = *values.touch_from_button_map_index.get_value();
        values
            .touch_from_button_map_index
            .set_value(selected.min(map_count - 1));
    }

    /// Write the categories shared by global and per-game configurations.
    /// Maps to `Config::SaveValues`'s non-global portion.
    pub fn save_values(&mut self) {
        if self.global {
            self.save_data_storage_values();
            self.save_debugging_values();
            self.save_disabled_add_on_values();
            self.write_category(Category::WebService);
            self.write_category(Category::Miscellaneous);
        }
        self.write_category(Category::LibraryApplet);
        self.write_category_to_group(Category::Services.translate(), Category::Network);
        self.save_control_values();
        for category in [
            Category::Core,
            Category::Cpu,
            Category::CpuDebug,
            Category::CpuUnsafe,
            Category::Linux,
            Category::Renderer,
            Category::RendererAdvanced,
            Category::RendererHacks,
            Category::RendererExtensions,
            Category::RendererDebug,
            Category::Audio,
            Category::System,
            Category::SystemAudio,
        ] {
            self.write_category(category);
        }
    }

    /// Maps to `Config::SaveDataStorageValues`.
    fn save_data_storage_values(&mut self) {
        use common::fs::path_util::{get_ruzu_path_string, RuzuPath};

        self.begin_group(Category::DataStorage.translate());
        for (setting, path) in [
            ("nand_directory", RuzuPath::NANDDir),
            ("sdmc_directory", RuzuPath::SDMCDir),
            ("load_directory", RuzuPath::LoadDir),
            ("dump_directory", RuzuPath::DumpDir),
            ("tas_directory", RuzuPath::TASDir),
        ] {
            let value = get_ruzu_path_string(path);
            self.write_string_setting(setting, &value, Some(&value));
        }

        let save_path = get_ruzu_path_string(RuzuPath::SaveDir);
        let nand_path = get_ruzu_path_string(RuzuPath::NANDDir);
        let serialized_save_path = if save_path == nand_path {
            ""
        } else {
            save_path.as_str()
        };
        self.write_string_setting("save_directory", serialized_save_path, Some(""));

        {
            let mut values = common::settings::values_mut();
            values.for_each_setting_in_category_mut(Category::DataStorage, |setting| {
                self.write_setting_generic(setting)
            });
        }
        self.end_group();
    }

    /// Maps to `Config::SaveDisabledAddOnValues`.
    fn save_disabled_add_on_values(&mut self) {
        let disabled_addons = common::settings::values().disabled_addons.clone();
        self.begin_group("DisabledAddOns");
        self.begin_array("");
        for (entry_index, (title_id, disabled)) in disabled_addons.iter().enumerate() {
            self.set_array_index(entry_index as i32);
            self.write_unsigned_integer_setting("title_id", *title_id, Some(0));
            self.begin_array("disabled");
            for (disabled_index, name) in disabled.iter().enumerate() {
                self.set_array_index(disabled_index as i32);
                self.write_string_setting("d", name, Some(""));
            }
            self.end_array();
        }
        self.end_array();
        self.end_group();
    }

    /// Maps to `Config::SaveDebuggingValues`.
    fn save_debugging_values(&mut self) {
        self.begin_group(Category::Debugging.translate());
        self.write_boolean_setting(
            "record_frame_times",
            common::settings::values().record_frame_times,
            None,
        );
        {
            let mut values = common::settings::values_mut();
            for category in [Category::Debugging, Category::DebuggingGraphics] {
                values.for_each_setting_in_category_mut(category, |setting| {
                    self.write_setting_generic(setting)
                });
            }
        }
        self.end_group();
    }

    /// Maps to `Config::SaveControlValues`.
    fn save_control_values(&mut self) {
        self.write_category(Category::Controls);
        self.begin_group(Category::Controls.translate());

        let is_custom_config = self.config_type == ConfigType::PerGameConfig;
        {
            let mut values = common::settings::values_mut();
            values.players.set_global(!is_custom_config);
        }
        let player_count = common::settings::values().players.get_value().len();
        for player_index in 0..player_count {
            self.save_player_values(player_index);
        }

        if is_custom_config {
            self.end_group();
            return;
        }

        self.save_touchscreen_values();
        self.save_motion_touch_values();
        self.end_group();
    }

    /// Maps to `Config::SavePlayerValues`.
    fn save_player_values(&mut self, player_index: usize) {
        let player_prefix = if self.config_type == ConfigType::InputProfile {
            String::new()
        } else {
            format!("player_{player_index}_")
        };
        let player = common::settings::values().players.get_value()[player_index].clone();

        if self.config_type == ConfigType::PerGameConfig {
            if player.profile_name.is_empty() {
                return;
            }
            self.write_string_setting(
                &format!("{player_prefix}profile_name"),
                &player.profile_name,
                Some(""),
            );
        }

        self.write_integer_setting(
            &format!("{player_prefix}type"),
            player.controller_type as i64,
            Some(ControllerType::ProController as i64),
        );

        if !player_prefix.is_empty() || !common::settings::is_configuring_global() {
            if self.global {
                let profile_name = common::settings::values().players.get_value_explicit(true)
                    [player_index]
                    .profile_name
                    .clone();
                self.write_string_setting(
                    &format!("{player_prefix}profile_name"),
                    &profile_name,
                    Some(""),
                );
            }
            self.write_boolean_setting(
                &format!("{player_prefix}connected"),
                player.connected,
                Some(player_index == 0),
            );
            self.write_boolean_setting(
                &format!("{player_prefix}vibration_enabled"),
                player.vibration_enabled,
                Some(true),
            );
            self.write_integer_setting(
                &format!("{player_prefix}vibration_strength"),
                player.vibration_strength as i64,
                Some(100),
            );
            self.write_integer_setting(
                &format!("{player_prefix}body_color_left"),
                player.body_color_left as i64,
                Some(JOYCON_BODY_NEON_BLUE as i64),
            );
            self.write_integer_setting(
                &format!("{player_prefix}body_color_right"),
                player.body_color_right as i64,
                Some(JOYCON_BODY_NEON_RED as i64),
            );
            self.write_integer_setting(
                &format!("{player_prefix}button_color_left"),
                player.button_color_left as i64,
                Some(JOYCON_BUTTONS_NEON_BLUE as i64),
            );
            self.write_integer_setting(
                &format!("{player_prefix}button_color_right"),
                player.button_color_right as i64,
                Some(JOYCON_BUTTONS_NEON_RED as i64),
            );
        }
    }

    /// Maps to `Config::SaveTouchscreenValues`.
    fn save_touchscreen_values(&mut self) {
        let touchscreen = common::settings::values().touchscreen.clone();
        self.write_boolean_setting("touchscreen_enabled", touchscreen.enabled, Some(true));
        self.write_integer_setting(
            "touchscreen_angle",
            touchscreen.rotation_angle as i64,
            Some(0),
        );
        self.write_integer_setting(
            "touchscreen_diameter_x",
            touchscreen.diameter_x as i64,
            Some(90),
        );
        self.write_integer_setting(
            "touchscreen_diameter_y",
            touchscreen.diameter_y as i64,
            Some(90),
        );
    }

    /// Maps to `Config::SaveMotionTouchValues`.
    fn save_motion_touch_values(&mut self) {
        let maps = common::settings::values().touch_from_button_maps.clone();
        self.begin_array("touch_from_button_maps");
        for (map_index, map) in maps.iter().enumerate() {
            self.set_array_index(map_index as i32);
            self.write_string_setting("name", &map.name, Some("default"));
            self.begin_array("entries");
            for (entry_index, binding) in map.buttons.iter().enumerate() {
                self.set_array_index(entry_index as i32);
                self.write_string_setting("bind", binding, None);
            }
            self.end_array();
        }
        self.end_array();
    }

    /// Reloads the base settings and republishes defaults into the INI map.
    ///
    /// This is Eden `Config::Reload`. Callers deliberately choose when to use
    /// it: Global and PerGame initialization reload, while InputProfile
    /// initialization only sets up the INI document.
    pub fn reload(&mut self) {
        self.read_values();
        self.save_values();
    }

    /// Serialize the current INI document to `config_loc`.
    /// Maps to `Config::WriteToIni`.
    pub fn write_to_ini(&self) -> std::io::Result<()> {
        let path = Path::new(&self.config_loc);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut output = String::new();
        for (section, values) in &self.ini {
            if !section.is_empty() {
                output.push('[');
                output.push_str(section);
                output.push_str("]\n");
            }
            for (key, value) in values {
                output.push_str(key);
                output.push('=');
                output.push_str(value);
                output.push('\n');
            }
            output.push('\n');
        }
        std::fs::write(path, output)
    }

    fn parse_bool(value: &str) -> Option<bool> {
        let value = value.trim_matches('"').as_bytes();
        match value.first().map(u8::to_ascii_lowercase) {
            Some(b't' | b'y' | b'1') => Some(true),
            Some(b'f' | b'n' | b'0') => Some(false),
            Some(b'o') => match value.get(1).map(u8::to_ascii_lowercase) {
                Some(b'n') => Some(true),
                Some(b'f') => Some(false),
                _ => None,
            },
            _ => None,
        }
    }

    /// Maps to `Config::ReadBooleanSetting`.
    pub fn read_boolean_setting(&self, key: &str, default_value: Option<bool>) -> bool {
        let Some(default_value) = default_value else {
            return self
                .read_raw(key)
                .and_then(Self::parse_bool)
                .unwrap_or(false);
        };

        let use_default = self
            .read_raw(&format!("{key}\\default"))
            .and_then(Self::parse_bool)
            .unwrap_or(false);
        if use_default {
            default_value
        } else {
            self.read_raw(key)
                .and_then(Self::parse_bool)
                .unwrap_or(default_value)
        }
    }

    /// Maps to `Config::ReadIntegerSetting`.
    pub fn read_integer_setting(&self, key: &str, default_value: Option<i64>) -> i64 {
        let Some(default_value) = default_value else {
            return self
                .read_raw(key)
                .and_then(|value| value.trim_matches('"').parse().ok())
                .unwrap_or(0);
        };

        let use_default = self
            .read_raw(&format!("{key}\\default"))
            .and_then(Self::parse_bool)
            .unwrap_or(true);
        if use_default {
            default_value
        } else {
            self.read_raw(key)
                .and_then(|value| value.trim_matches('"').parse().ok())
                .unwrap_or(default_value)
        }
    }

    /// Maps to `Config::ReadUnsignedIntegerSetting`.
    fn read_unsigned_integer_setting(&self, key: &str, default_value: Option<u64>) -> u64 {
        let Some(default_value) = default_value else {
            return self
                .read_raw(key)
                .and_then(|value| value.trim_matches('"').parse().ok())
                .unwrap_or(0);
        };

        let use_default = self
            .read_raw(&format!("{key}\\default"))
            .and_then(Self::parse_bool)
            .unwrap_or(true);
        if use_default {
            default_value
        } else {
            self.read_raw(key)
                .and_then(|value| value.trim_matches('"').parse().ok())
                .unwrap_or(default_value)
        }
    }

    /// Maps to `Config::ReadStringSetting`.
    pub fn read_string_setting(&self, key: &str, default_value: Option<&str>) -> String {
        let mut result = match default_value {
            None => self.read_raw(key).unwrap_or_default().to_string(),
            Some(default_value) => {
                let use_default = self
                    .read_raw(&format!("{key}\\default"))
                    .and_then(Self::parse_bool)
                    .unwrap_or(true);
                if use_default {
                    default_value.to_string()
                } else {
                    self.read_raw(key).unwrap_or(default_value).to_string()
                }
            }
        };

        // Upstream removes quotes after SimpleIni returns the value.
        result.retain(|character| character != '"');
        if default_value.is_some() {
            result = result.replace("//", "/");
        }
        result
    }

    /// Maps to the no-`use_global` branch of `Config::WritePreparedSetting`.
    fn write_prepared_setting(
        &mut self,
        key: &str,
        adjusted_value: String,
        adjusted_default: Option<String>,
    ) {
        if let Some(default) = adjusted_default {
            self.write_raw(
                &format!("{key}\\default"),
                to_string_bool(default == adjusted_value),
            );
        }
        self.write_raw(key, adjusted_value);
    }

    /// Maps to `Config::WriteBooleanSetting`.
    fn write_boolean_setting(&mut self, key: &str, value: bool, default: Option<bool>) {
        self.write_prepared_setting(key, to_string_bool(value), default.map(to_string_bool));
    }

    /// Maps to `Config::WriteIntegerSetting` for the signed values used here.
    fn write_integer_setting(&mut self, key: &str, value: i64, default: Option<i64>) {
        self.write_prepared_setting(
            key,
            value.to_string(),
            default.map(|value| value.to_string()),
        );
    }

    /// Maps to `Config::WriteIntegerSetting` for unsigned values.
    fn write_unsigned_integer_setting(&mut self, key: &str, value: u64, default: Option<u64>) {
        self.write_prepared_setting(
            key,
            value.to_string(),
            default.map(|value| value.to_string()),
        );
    }

    /// Maps to `Config::WriteStringSetting`.
    fn write_string_setting(&mut self, key: &str, value: &str, default: Option<&str>) {
        self.write_prepared_setting(
            key,
            adjust_output_string(value),
            default.map(adjust_output_string),
        );
    }

    /// Maps to `Config::ReadSystemValues` and its two `ReadCategory` calls.
    pub fn read_system_values(&mut self) {
        self.begin_group("System");
        {
            let mut values = common::settings::values_mut();
            self.read_system_values_into(&mut values);
        }
        self.end_group();
    }

    fn read_system_values_into(&self, values: &mut common::settings::Values) {
        use common::settings_enums::{AudioMode, ConsoleMode, Language, Region, TimeZone};

        let language = self.read_integer_setting(
            "language_index",
            Some(*values.language_index.get_default() as i64),
        );
        values.language_index.set_value(
            Language::from_u32(language as u32).unwrap_or(*values.language_index.get_default()),
        );

        let region = self.read_integer_setting(
            "region_index",
            Some(*values.region_index.get_default() as i64),
        );
        values.region_index.set_value(
            Region::from_u32(region as u32).unwrap_or(*values.region_index.get_default()),
        );

        let time_zone = self.read_integer_setting(
            "time_zone_index",
            Some(*values.time_zone_index.get_default() as i64),
        );
        values.time_zone_index.set_value(
            TimeZone::from_u32(time_zone as u32).unwrap_or(*values.time_zone_index.get_default()),
        );

        values
            .custom_rtc_enabled
            .set_value(self.read_boolean_setting(
                "custom_rtc_enabled",
                Some(*values.custom_rtc_enabled.get_default()),
            ));
        values
            .custom_rtc_offset
            .set_value(self.read_integer_setting(
                "custom_rtc_offset",
                Some(*values.custom_rtc_offset.get_default()),
            ));
        values.rng_seed_enabled.set_value(self.read_boolean_setting(
            "rng_seed_enabled",
            Some(*values.rng_seed_enabled.get_default()),
        ));
        values
            .rng_seed
            .set_value(self.read_u32_setting("rng_seed", *values.rng_seed.get_default()));
        values.device_name.set_value(
            self.read_string_setting("device_name", Some(values.device_name.get_default())),
        );
        values.current_user.set_value(self.read_integer_setting(
            "current_user",
            Some(*values.current_user.get_default() as i64),
        ) as i32);

        let console_mode = self.read_integer_setting(
            "use_docked_mode",
            Some(*values.use_docked_mode.get_default() as i64),
        );
        values.use_docked_mode.set_value(
            ConsoleMode::from_u32(console_mode as u32)
                .unwrap_or(*values.use_docked_mode.get_default()),
        );

        let sound_mode = self.read_integer_setting(
            "sound_index",
            Some(*values.sound_index.get_default() as i64),
        );
        values.sound_index.set_value(
            AudioMode::from_u32(sound_mode as u32).unwrap_or(*values.sound_index.get_default()),
        );
    }

    fn read_u32_setting(&self, key: &str, default_value: u32) -> u32 {
        let use_default = self
            .read_raw(&format!("{key}\\default"))
            .and_then(Self::parse_bool)
            .unwrap_or(true);
        if use_default {
            return default_value;
        }
        self.read_raw(key)
            .and_then(|value| {
                let value = value.trim_matches('"');
                value
                    .strip_prefix("0x")
                    .or_else(|| value.strip_prefix("0X"))
                    .map_or_else(
                        || value.parse().ok(),
                        |hex| u32::from_str_radix(hex, 16).ok(),
                    )
            })
            .unwrap_or(default_value)
    }

    /// Maps to `Config::ReadPlayerValues`.
    pub fn read_player_values(&self, player_index: usize) {
        let configuring_global = common::settings::is_configuring_global();
        let mut values = common::settings::values_mut();
        self.read_player_values_into(player_index, &mut values.players, configuring_global);
    }

    fn read_player_values_into(
        &self,
        player_index: usize,
        players: &mut InputSetting<[PlayerInput; 10]>,
        configuring_global: bool,
    ) {
        let player_prefix = if self.config_type == ConfigType::InputProfile {
            String::new()
        } else {
            format!("player_{player_index}_")
        };
        let profile_name = self.read_string_setting(&format!("{player_prefix}profile_name"), None);

        if self.config_type == ConfigType::PerGameConfig {
            if profile_name.is_empty() {
                let mut global_player = players.get_value_explicit(true)[player_index].clone();
                global_player.profile_name.clear();
                players.get_value_mut()[player_index] = global_player;
                return;
            }
            players.get_value_mut()[player_index].profile_name = profile_name.clone();
        }

        if player_prefix.is_empty() && configuring_global {
            let controller = controller_type_from_config(self.read_integer_setting(
                &format!("{player_prefix}type"),
                Some(ControllerType::ProController as i64),
            ));
            if matches!(
                controller,
                ControllerType::LeftJoycon | ControllerType::RightJoycon
            ) {
                players.get_value_mut()[player_index].controller_type = controller;
            }
            return;
        }

        if self.global {
            players.get_value_explicit_mut(true)[player_index].profile_name = profile_name.clone();
        }

        let player = &mut players.get_value_mut()[player_index];
        player.connected = self.read_boolean_setting(
            &format!("{player_prefix}connected"),
            Some(player_index == 0),
        );
        player.controller_type = controller_type_from_config(self.read_integer_setting(
            &format!("{player_prefix}type"),
            Some(ControllerType::ProController as i64),
        ));
        player.vibration_enabled =
            self.read_boolean_setting(&format!("{player_prefix}vibration_enabled"), Some(true));
        player.vibration_strength = self
            .read_integer_setting(&format!("{player_prefix}vibration_strength"), Some(100))
            as i32;
        player.body_color_left = self.read_integer_setting(
            &format!("{player_prefix}body_color_left"),
            Some(JOYCON_BODY_NEON_BLUE as i64),
        ) as u32;
        player.body_color_right = self.read_integer_setting(
            &format!("{player_prefix}body_color_right"),
            Some(JOYCON_BODY_NEON_RED as i64),
        ) as u32;
        player.button_color_left = self.read_integer_setting(
            &format!("{player_prefix}button_color_left"),
            Some(JOYCON_BUTTONS_NEON_BLUE as i64),
        ) as u32;
        player.button_color_right = self.read_integer_setting(
            &format!("{player_prefix}button_color_right"),
            Some(JOYCON_BUTTONS_NEON_RED as i64),
        ) as u32;
    }

    /// Begins a config array.
    pub fn begin_array(&mut self, array: &str) -> i32 {
        self.array_stack.push(ConfigArrayEntry {
            name: adjust_key(array),
            size: 0,
            index: 0,
        });
        let section = self.get_section();
        let size_key = self.get_full_key("size", true);
        let size = self
            .ini
            .get(&section)
            .and_then(|values| values.get(&size_key))
            .and_then(|value| value.parse::<i32>().ok())
            .unwrap_or(0)
            .max(0);
        self.array_stack.last_mut().unwrap().size = size;
        size
    }

    /// Ends the current config array.
    pub fn end_array(&mut self) {
        assert!(
            !self.array_stack.is_empty(),
            "Can't end a config array before starting one"
        );

        let entry = self.array_stack.last().unwrap();
        let size = if entry.index == 0 { 0 } else { entry.size };
        let section = self.get_section();
        let size_key = if self.key_stack.len() == 1 && entry.name.is_empty() {
            "size".to_string()
        } else {
            self.get_full_key("size", true)
        };
        self.ini
            .entry(section)
            .or_default()
            .insert(size_key, size.to_string());

        self.array_stack.pop();
    }

    /// Sets the current array index.
    pub fn set_array_index(&mut self, index: i32) {
        assert!(
            !self.array_stack.is_empty(),
            "Can't set the array index if you haven't started one yet"
        );

        let array_index = index + 1;
        if let Some(entry) = self.array_stack.last_mut() {
            entry.size = array_index;
            entry.index = array_index;
        }
    }
}

fn controller_type_from_config(value: i64) -> ControllerType {
    ControllerType::try_from(value as u8).unwrap_or(ControllerType::ProController)
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::settings_common::SwitchableSetting;

    static SETTINGS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn test_adjust_key() {
        assert_eq!(adjust_key("some/path"), "some\\path");
        assert_eq!(adjust_key("hello world"), "hello%20world");
    }

    #[test]
    fn test_adjust_output_string_special_chars() {
        let result = adjust_output_string("value!test");
        assert!(result.starts_with('"'));
        assert!(result.ends_with('"'));
    }

    #[test]
    fn test_adjust_output_string_no_special() {
        let result = adjust_output_string("simple");
        assert_eq!(result, "simple");
    }

    #[test]
    fn test_base_config_group_stack() {
        let mut cfg = BaseConfig::new(ConfigType::GlobalConfig);
        cfg.begin_group("Controls");
        assert_eq!(cfg.get_section(), "Controls");
        cfg.end_group();
        assert!(cfg.key_stack.is_empty());
    }

    #[test]
    fn test_base_config_full_key() {
        let mut cfg = BaseConfig::new(ConfigType::GlobalConfig);
        cfg.begin_group("Section");
        let key = cfg.get_full_key("mykey", false);
        assert_eq!(key, "mykey");
        cfg.end_group();
    }

    #[test]
    fn read_settings_honor_upstream_default_markers() {
        let mut cfg = BaseConfig::new(ConfigType::GlobalConfig);
        cfg.load_ini(
            r#"
            [Controls]
            enabled\default=true
            enabled=false
            count\default=false
            count=42
            binding\default=false
            binding="engine:sdl,button:1"
            "#,
        );
        cfg.begin_group("Controls");

        assert!(cfg.read_boolean_setting("enabled", Some(true)));
        assert_eq!(cfg.read_integer_setting("count", Some(7)), 42);
        assert_eq!(
            cfg.read_string_setting("binding", Some("fallback")),
            "engine:sdl,button:1"
        );
    }

    #[test]
    fn read_system_values_honors_configured_locale_and_defaults() {
        let mut cfg = BaseConfig::new(ConfigType::GlobalConfig);
        cfg.load_ini(
            r#"
            [System]
            language_index\default=false
            language_index=2
            region_index\default=false
            region_index=2
            time_zone_index\default=true
            time_zone_index=4
            rng_seed_enabled\default=false
            rng_seed_enabled=true
            rng_seed\default=false
            rng_seed=0x1234ABCD
            sound_index\default=false
            sound_index=2
            "#,
        );
        cfg.begin_group("System");
        let mut values = common::settings::Values::default();

        cfg.read_system_values_into(&mut values);

        assert_eq!(
            *values.language_index.get_value(),
            common::settings_enums::Language::French
        );
        assert_eq!(
            *values.region_index.get_value(),
            common::settings_enums::Region::Europe
        );
        assert_eq!(
            *values.time_zone_index.get_value(),
            common::settings_enums::TimeZone::Auto
        );
        assert!(*values.rng_seed_enabled.get_value());
        assert_eq!(*values.rng_seed.get_value(), 0x1234_ABCD);
        assert_eq!(
            *values.sound_index.get_value(),
            common::settings_enums::AudioMode::Surround
        );
    }

    #[test]
    fn read_values_loads_the_renderer_category() {
        use common::settings_enums::RendererBackend;

        let _guard = SETTINGS_TEST_LOCK.lock().unwrap();
        let previous = {
            let values = common::settings::values();
            *values.renderer_backend.get_value_global()
        };
        let mut cfg = BaseConfig::new(ConfigType::GlobalConfig);
        cfg.load_ini("[Renderer]\nbackend\\default=false\nbackend=0\n");

        cfg.read_values();

        assert_eq!(
            *common::settings::values()
                .renderer_backend
                .get_value_global(),
            RendererBackend::OpenGlGlsl
        );
        common::settings::values_mut()
            .renderer_backend
            .set_value(previous);
    }

    #[test]
    fn read_values_loads_global_debugging_and_renderer_subcategories() {
        let _guard = SETTINGS_TEST_LOCK.lock().unwrap();
        let previous = {
            let values = common::settings::values();
            (
                *values.dump_exefs.get_value(),
                *values.fix_bloom_effects.get_value_global(),
                *values.sample_shading.get_value_global(),
            )
        };
        let mut cfg = BaseConfig::new(ConfigType::GlobalConfig);
        cfg.load_ini(
            "[Debugging]\n\
             dump_exefs\\default=false\n\
             dump_exefs=true\n\
             [Renderer]\n\
             fix_bloom_effects\\default=false\n\
             fix_bloom_effects=true\n\
             sample_shading_fraction\\default=false\n\
             sample_shading_fraction=37\n",
        );

        cfg.read_values();

        {
            let values = common::settings::values();
            assert!(*values.dump_exefs.get_value());
            assert!(*values.fix_bloom_effects.get_value_global());
            assert_eq!(*values.sample_shading.get_value_global(), 37);
        }
        {
            let mut values = common::settings::values_mut();
            values.dump_exefs.set_value(previous.0);
            values.fix_bloom_effects.set_value(previous.1);
            values.sample_shading.set_value(previous.2);
        }
    }

    #[test]
    fn save_values_writes_the_renderer_category() {
        use common::settings_enums::{CpuBackend, RendererBackend};

        let _guard = SETTINGS_TEST_LOCK.lock().unwrap();
        let previous = {
            let values = common::settings::values();
            (
                *values.renderer_backend.get_value_global(),
                *values.cpu_backend.get_value_global(),
            )
        };
        {
            let mut values = common::settings::values_mut();
            values.renderer_backend.set_value(RendererBackend::Null);
            values.cpu_backend.set_value(CpuBackend::Nce);
        }
        let mut cfg = BaseConfig::new(ConfigType::GlobalConfig);

        cfg.save_values();

        let renderer = cfg.ini.get("Renderer").unwrap();
        assert_eq!(
            renderer.get("backend\\default").map(String::as_str),
            Some("false")
        );
        assert_eq!(renderer.get("backend").map(String::as_str), Some("2"));
        let cpu = cfg.ini.get("Cpu").unwrap();
        // NCE is clamped out by the x86-64 CPU-backend range, but the stored
        // Dynarmic value must still be Eden's numeric `0`, never "Dynarmic".
        assert_eq!(cpu.get("cpu_backend").map(String::as_str), Some("0"));
        {
            let mut values = common::settings::values_mut();
            values.renderer_backend.set_value(previous.0);
            values.cpu_backend.set_value(previous.1);
        }
    }

    #[test]
    fn save_values_registers_every_visible_graphics_setting() {
        let _guard = SETTINGS_TEST_LOCK.lock().unwrap();
        let mut cfg = BaseConfig::new(ConfigType::GlobalConfig);
        cfg.save_values();

        let renderer = cfg.ini.get("Renderer").expect("Renderer section");
        for key in [
            // Graphics
            "backend",
            "vulkan_device",
            "use_asynchronous_gpu_emulation",
            "use_vsync",
            "fullscreen_mode",
            "aspect_ratio",
            "resolution_setup",
            "scaling_filter",
            "anti_aliasing",
            "fsr_sharpening_slider",
            "bg_red",
            "bg_green",
            "bg_blue",
            // Advanced
            "gpu_accuracy",
            "dma_accuracy",
            "gpu_fence_behavior",
            "vram_usage_mode",
            "nvdec_emulation",
            "max_anisotropy",
            "accelerate_astc",
            "frame_pacing_mode",
            "astc_recompression",
            "sync_memory_operations",
            "force_max_clock",
            "use_disk_shader_cache",
            "use_vulkan_driver_pipeline_cache",
            "enable_compute_pipelines",
            "use_video_framerate",
            "use_reactive_flushing",
            "barrier_feedback_loops",
            "enable_buffer_history",
            "enable_gpu_buffer_readback",
            // Extras / Hacks
            "skip_cpu_inner_invalidation",
            "async_presentation",
            "fix_bloom_effects",
            "emulate_bgr565",
            "rescale_hack",
            "use_asynchronous_shaders",
            "gpu_unswizzle_texture_size",
            "gpu_unswizzle_stream_size",
            "gpu_unswizzle_chunk_size",
            "gpu_unswizzle_enabled",
            // Extras / Vulkan Extensions
            "dyna_state",
            "sample_shading_fraction",
            "vertex_input_dynamic_state",
        ] {
            assert!(renderer.contains_key(key), "missing persisted value {key}");
            assert!(
                renderer.contains_key(&format!("{key}\\default")),
                "missing persisted default marker for {key}"
            );
        }
    }

    #[test]
    fn setup_ini_for_save_preserves_pending_global_renderer_change() {
        use common::settings_enums::RendererBackend;

        let _guard = SETTINGS_TEST_LOCK.lock().unwrap();
        let (previous_global, previous_use_global) = {
            let values = common::settings::values();
            (
                *values.renderer_backend.get_value_global(),
                values.renderer_backend.using_global(),
            )
        };
        {
            let mut values = common::settings::values_mut();
            values.renderer_backend.set_global(true);
            values
                .renderer_backend
                .set_value(RendererBackend::OpenGlGlsl);
        }

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("reden-config-save-{unique}.ini"));
        std::fs::write(&path, "[Renderer]\nbackend\\default=true\nbackend=1\n").unwrap();

        let mut cfg = BaseConfig::new(ConfigType::GlobalConfig);
        cfg.set_up_ini(&path);
        assert_eq!(
            *common::settings::values()
                .renderer_backend
                .get_value_global(),
            RendererBackend::OpenGlGlsl
        );
        cfg.save_values();
        assert_eq!(
            cfg.ini["Renderer"].get("backend").map(String::as_str),
            Some("0")
        );

        let _ = std::fs::remove_file(path);
        let mut values = common::settings::values_mut();
        values.renderer_backend.set_global(true);
        values.renderer_backend.set_value(previous_global);
        values.renderer_backend.set_global(previous_use_global);
    }

    #[test]
    fn all_opengl_backends_round_trip_through_global_ini() {
        use common::settings_enums::RendererBackend;

        let _guard = SETTINGS_TEST_LOCK.lock().unwrap();
        let previous = {
            let values = common::settings::values();
            (
                *values.renderer_backend.get_value_global(),
                values.renderer_backend.using_global(),
            )
        };
        for backend in [
            RendererBackend::OpenGlGlsl,
            RendererBackend::OpenGlGlasm,
            RendererBackend::OpenGlSpirV,
        ] {
            {
                let mut values = common::settings::values_mut();
                values.renderer_backend.set_global(true);
                values.renderer_backend.set_value(backend);
            }

            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!("reden-opengl-save-{unique}.ini"));

            let mut writer = BaseConfig::new(ConfigType::GlobalConfig);
            writer.set_up_ini(&path);
            writer.save_values();
            writer.write_to_ini().unwrap();

            common::settings::values_mut()
                .renderer_backend
                .set_value(RendererBackend::Vulkan);
            let mut reader = BaseConfig::new(ConfigType::GlobalConfig);
            reader.initialize(&path);

            assert_eq!(
                *common::settings::values().renderer_backend.get_value(),
                backend
            );
            let _ = std::fs::remove_file(path);
        }

        let mut values = common::settings::values_mut();
        values.renderer_backend.set_global(true);
        values.renderer_backend.set_value(previous.0);
        values.renderer_backend.set_global(previous.1);
    }

    #[test]
    fn every_visible_graphics_value_round_trips_through_global_ini() {
        use common::settings::Values;
        use common::settings_enums::{
            AnisotropyMode, AntiAliasing, AspectRatio, AstcDecodeMode, AstcRecompression,
            DmaAccuracy, ExtendedDynamicState, FramePacingMode, FullscreenMode, GpuAccuracy,
            GpuFenceBehavior, GpuUnswizzle, GpuUnswizzleChunk, GpuUnswizzleSize, NvdecEmulation,
            RendererBackend, ResolutionSetup, ScalingFilter, VSyncMode, VramUsageMode,
        };

        let _guard = SETTINGS_TEST_LOCK.lock().unwrap();
        let previous = common::settings::values().clone();
        *common::settings::values_mut() = Values::default();
        {
            let mut values = common::settings::values_mut();
            values
                .renderer_backend
                .set_value(RendererBackend::OpenGlSpirV);
            values.vulkan_device.set_value(1);
            values.resolution_setup.set_value(ResolutionSetup::Res3_2X);
            values.vsync_mode.set_value(VSyncMode::Mailbox);
            values.scaling_filter.set_value(ScalingFilter::SgsrEdge);
            values.fsr_sharpening_slider.set_value(77);
            values.aspect_ratio.set_value(AspectRatio::R21_9);
            values.anti_aliasing.set_value(AntiAliasing::Smaa);
            values.use_asynchronous_gpu_emulation.set_value(false);
            values.fullscreen_mode.set_value(FullscreenMode::Borderless);
            values.bg_red.set_value(10);
            values.bg_green.set_value(20);
            values.bg_blue.set_value(30);

            values.gpu_accuracy.set_value(GpuAccuracy::Low);
            values.dma_accuracy.set_value(DmaAccuracy::Safe);
            values
                .gpu_fence_behavior
                .set_value(GpuFenceBehavior::Strict);
            values.vram_usage_mode.set_value(VramUsageMode::Aggressive);
            values.nvdec_emulation.set_value(NvdecEmulation::Cpu);
            values.max_anisotropy.set_value(AnisotropyMode::X8);
            values
                .accelerate_astc
                .set_value(AstcDecodeMode::CpuAsynchronous);
            values
                .frame_pacing_mode
                .set_value(FramePacingMode::Target90);
            values.astc_recompression.set_value(AstcRecompression::Bc3);
            values.sync_memory_operations.set_value(true);
            values.renderer_force_max_clock.set_value(true);
            values.use_disk_shader_cache.set_value(false);
            values.use_vulkan_driver_pipeline_cache.set_value(false);
            values.enable_compute_pipelines.set_value(true);
            values.use_video_framerate.set_value(true);
            values.use_reactive_flushing.set_value(false);
            values.barrier_feedback_loops.set_value(false);
            values.enable_buffer_history.set_value(true);
            values.enable_gpu_buffer_readback.set_value(true);

            values.skip_cpu_inner_invalidation.set_value(true);
            values.async_presentation.set_value(true);
            values.fix_bloom_effects.set_value(true);
            values.emulate_bgr565.set_value(true);
            values.rescale_hack.set_value(true);
            values.use_asynchronous_shaders.set_value(true);
            values
                .gpu_unswizzle_texture_size
                .set_value(GpuUnswizzleSize::Small);
            values
                .gpu_unswizzle_stream_size
                .set_value(GpuUnswizzle::Low);
            values
                .gpu_unswizzle_chunk_size
                .set_value(GpuUnswizzleChunk::High);
            values.gpu_unswizzle_enabled.set_value(true);

            values.dyna_state.set_value(ExtendedDynamicState::EDS3);
            values.sample_shading.set_value(42);
            values.vertex_input_dynamic_state.set_value(false);
        }

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("reden-graphics-roundtrip-{unique}.ini"));
        let mut writer = BaseConfig::new(ConfigType::GlobalConfig);
        writer.set_up_ini(&path);
        writer.save_values();
        writer.write_to_ini().unwrap();

        *common::settings::values_mut() = Values::default();
        let mut reader = BaseConfig::new(ConfigType::GlobalConfig);
        reader.initialize(&path);
        {
            let values = common::settings::values();
            assert_eq!(
                *values.renderer_backend.get_value(),
                RendererBackend::OpenGlSpirV
            );
            assert_eq!(*values.vulkan_device.get_value(), 1);
            assert_eq!(
                *values.resolution_setup.get_value(),
                ResolutionSetup::Res3_2X
            );
            assert_eq!(*values.vsync_mode.get_value(), VSyncMode::Mailbox);
            assert_eq!(*values.scaling_filter.get_value(), ScalingFilter::SgsrEdge);
            assert_eq!(*values.fsr_sharpening_slider.get_value(), 77);
            assert_eq!(*values.aspect_ratio.get_value(), AspectRatio::R21_9);
            assert_eq!(*values.anti_aliasing.get_value(), AntiAliasing::Smaa);
            assert!(!*values.use_asynchronous_gpu_emulation.get_value());
            assert_eq!(
                *values.fullscreen_mode.get_value(),
                FullscreenMode::Borderless
            );
            assert_eq!(
                (
                    *values.bg_red.get_value(),
                    *values.bg_green.get_value(),
                    *values.bg_blue.get_value()
                ),
                (10, 20, 30)
            );
            assert_eq!(*values.gpu_accuracy.get_value(), GpuAccuracy::Low);
            assert_eq!(*values.dma_accuracy.get_value(), DmaAccuracy::Safe);
            assert_eq!(
                *values.gpu_fence_behavior.get_value(),
                GpuFenceBehavior::Strict
            );
            assert_eq!(
                *values.vram_usage_mode.get_value(),
                VramUsageMode::Aggressive
            );
            assert_eq!(*values.nvdec_emulation.get_value(), NvdecEmulation::Cpu);
            assert_eq!(*values.max_anisotropy.get_value(), AnisotropyMode::X8);
            assert_eq!(
                *values.accelerate_astc.get_value(),
                AstcDecodeMode::CpuAsynchronous
            );
            assert_eq!(
                *values.frame_pacing_mode.get_value(),
                FramePacingMode::Target90
            );
            assert_eq!(
                *values.astc_recompression.get_value(),
                AstcRecompression::Bc3
            );
            assert!(*values.sync_memory_operations.get_value());
            assert!(*values.renderer_force_max_clock.get_value());
            assert!(!*values.use_disk_shader_cache.get_value());
            assert!(!*values.use_vulkan_driver_pipeline_cache.get_value());
            assert!(*values.enable_compute_pipelines.get_value());
            assert!(*values.use_video_framerate.get_value());
            assert!(!*values.use_reactive_flushing.get_value());
            assert!(!*values.barrier_feedback_loops.get_value());
            assert!(*values.enable_buffer_history.get_value());
            assert!(*values.enable_gpu_buffer_readback.get_value());
            assert!(*values.skip_cpu_inner_invalidation.get_value());
            assert!(*values.async_presentation.get_value());
            assert!(*values.fix_bloom_effects.get_value());
            assert!(*values.emulate_bgr565.get_value());
            assert!(*values.rescale_hack.get_value());
            assert!(*values.use_asynchronous_shaders.get_value());
            assert_eq!(
                *values.gpu_unswizzle_texture_size.get_value(),
                GpuUnswizzleSize::Small
            );
            assert_eq!(
                *values.gpu_unswizzle_stream_size.get_value(),
                GpuUnswizzle::Low
            );
            assert_eq!(
                *values.gpu_unswizzle_chunk_size.get_value(),
                GpuUnswizzleChunk::High
            );
            assert!(*values.gpu_unswizzle_enabled.get_value());
            assert_eq!(*values.dyna_state.get_value(), ExtendedDynamicState::EDS3);
            assert_eq!(*values.sample_shading.get_value(), 42);
            assert!(!*values.vertex_input_dynamic_state.get_value());
        }

        let _ = std::fs::remove_file(path);
        *common::settings::values_mut() = previous;
    }

    #[test]
    fn legacy_split_shader_backend_is_folded_into_eden_backend_enum() {
        use common::settings_enums::RendererBackend;

        let _guard = SETTINGS_TEST_LOCK.lock().unwrap();
        let previous = *common::settings::values()
            .renderer_backend
            .get_value_global();
        let mut cfg = BaseConfig::new(ConfigType::GlobalConfig);
        cfg.load_ini(
            "[Renderer]\nbackend\\default=false\nbackend=0\nshader_backend\\default=false\nshader_backend=2\n",
        );
        cfg.read_values();

        assert_eq!(
            *common::settings::values().renderer_backend.get_value(),
            RendererBackend::OpenGlSpirV
        );
        let renderer = cfg.ini.get("Renderer").unwrap();
        assert!(!renderer.contains_key("shader_backend"));
        assert!(!renderer.contains_key("shader_backend\\default"));

        common::settings::values_mut()
            .renderer_backend
            .set_value(previous);
    }

    #[test]
    fn data_storage_values_apply_paths_and_default_save_to_nand() {
        use common::fs::path_util::{get_ruzu_path, set_ruzu_path, RuzuPath};

        let _guard = SETTINGS_TEST_LOCK.lock().unwrap();
        let paths = [
            RuzuPath::NANDDir,
            RuzuPath::SDMCDir,
            RuzuPath::LoadDir,
            RuzuPath::DumpDir,
            RuzuPath::TASDir,
            RuzuPath::SaveDir,
        ];
        let previous: Vec<_> = paths.iter().map(|path| get_ruzu_path(*path)).collect();
        let root = std::env::temp_dir().join(format!(
            "reden-config-paths-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for name in ["nand", "sdmc", "load", "dump", "tas"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }

        let mut cfg = BaseConfig::new(ConfigType::GlobalConfig);
        cfg.load_ini(&format!(
            "[Data%20Storage]\n\
             nand_directory={}\n\
             sdmc_directory={}\n\
             load_directory={}\n\
             dump_directory={}\n\
             tas_directory={}\n\
             save_directory=\n",
            root.join("nand").display(),
            root.join("sdmc").display(),
            root.join("load").display(),
            root.join("dump").display(),
            root.join("tas").display(),
        ));
        assert_eq!(
            cfg.ini["Data%20Storage"]["nand_directory"],
            root.join("nand").to_string_lossy()
        );
        cfg.read_data_storage_values();

        assert_eq!(get_ruzu_path(RuzuPath::NANDDir), root.join("nand"));
        assert_eq!(get_ruzu_path(RuzuPath::SDMCDir), root.join("sdmc"));
        assert_eq!(get_ruzu_path(RuzuPath::LoadDir), root.join("load"));
        assert_eq!(get_ruzu_path(RuzuPath::DumpDir), root.join("dump"));
        assert_eq!(get_ruzu_path(RuzuPath::TASDir), root.join("tas"));
        assert_eq!(get_ruzu_path(RuzuPath::SaveDir), root.join("nand"));

        for (path, value) in paths.into_iter().zip(previous) {
            set_ruzu_path(path, &value);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn disabled_addons_round_trip_full_width_title_ids() {
        let _guard = SETTINGS_TEST_LOCK.lock().unwrap();
        let previous = common::settings::values().disabled_addons.clone();
        let title_id = 0xf123_4567_89ab_cdef;
        {
            let mut values = common::settings::values_mut();
            values.disabled_addons.clear();
            values.disabled_addons.insert(
                title_id,
                vec!["Update".to_string(), "Optional content".to_string()],
            );
        }

        let mut written = BaseConfig::new(ConfigType::GlobalConfig);
        written.save_disabled_add_on_values();
        let serialized = written.ini.get("DisabledAddOns").unwrap().clone();
        common::settings::values_mut().disabled_addons.clear();

        let mut read = BaseConfig::new(ConfigType::GlobalConfig);
        read.ini.insert("DisabledAddOns".to_string(), serialized);
        read.read_disabled_add_on_values();

        assert_eq!(
            common::settings::values().disabled_addons.get(&title_id),
            Some(&vec!["Update".to_string(), "Optional content".to_string()])
        );
        common::settings::values_mut().disabled_addons = previous;
    }

    #[test]
    fn read_control_values_loads_players_handheld_touchscreen_and_touch_maps() {
        let _guard = SETTINGS_TEST_LOCK.lock().unwrap();
        let previous = {
            let values = common::settings::values();
            (
                values.players.clone(),
                values.use_docked_mode.clone(),
                values.touchscreen.clone(),
                values.touch_from_button_map_index.clone(),
                values.touch_from_button_maps.clone(),
            )
        };
        let mut cfg = BaseConfig::new(ConfigType::GlobalConfig);
        cfg.load_ini(
            r#"
            [Controls]
            player_0_profile_name=portable
            player_0_connected\default=false
            player_0_connected=false
            player_0_type\default=false
            player_0_type=4
            player_0_vibration_enabled\default=false
            player_0_vibration_enabled=false
            player_0_vibration_strength\default=false
            player_0_vibration_strength=47
            touchscreen_enabled\default=false
            touchscreen_enabled=false
            touchscreen_angle\default=false
            touchscreen_angle=90
            touchscreen_diameter_x\default=false
            touchscreen_diameter_x=70
            touchscreen_diameter_y\default=false
            touchscreen_diameter_y=80
            touch_from_button_map\default=false
            touch_from_button_map=9
            touch_from_button_maps\size=1
            touch_from_button_maps\1\name\default=false
            touch_from_button_maps\1\name=custom
            touch_from_button_maps\1\entries\size=1
            touch_from_button_maps\1\entries\1\bind=engine:keyboard,code:1
            "#,
        );

        cfg.read_control_values();

        {
            let values = common::settings::values();
            let player = &values.players.get_value()[0];
            assert_eq!(player.profile_name, "portable");
            assert!(!player.connected);
            assert_eq!(player.controller_type, ControllerType::Handheld);
            assert!(!player.vibration_enabled);
            assert_eq!(player.vibration_strength, 47);
            assert_eq!(*values.use_docked_mode.get_value(), ConsoleMode::Handheld);
            assert!(!values.touchscreen.enabled);
            assert_eq!(values.touchscreen.rotation_angle, 90);
            assert_eq!(values.touchscreen.diameter_x, 70);
            assert_eq!(values.touchscreen.diameter_y, 80);
            assert_eq!(*values.touch_from_button_map_index.get_value(), 0);
            assert_eq!(values.touch_from_button_maps.len(), 1);
            assert_eq!(values.touch_from_button_maps[0].name, "custom");
            assert_eq!(
                values.touch_from_button_maps[0].buttons,
                ["engine:keyboard,code:1"]
            );
        }

        cfg.save_control_values();
        let controls = cfg.ini.get("Controls").unwrap();
        assert_eq!(
            controls.get("player_0_profile_name").map(String::as_str),
            Some("portable")
        );
        assert_eq!(controls.get("player_0_type").map(String::as_str), Some("4"));
        assert_eq!(
            controls
                .get("player_0_vibration_strength")
                .map(String::as_str),
            Some("47")
        );
        assert_eq!(
            controls
                .get("touch_from_button_maps\\size")
                .map(String::as_str),
            Some("1")
        );
        assert_eq!(
            controls
                .get("touch_from_button_maps\\1\\entries\\size")
                .map(String::as_str),
            Some("1")
        );

        {
            let mut values = common::settings::values_mut();
            values.players = previous.0;
            values.use_docked_mode = previous.1;
            values.touchscreen = previous.2;
            values.touch_from_button_map_index = previous.3;
            values.touch_from_button_maps = previous.4;
        }
    }

    #[test]
    fn read_player_values_matches_global_player_defaults() {
        let mut cfg = BaseConfig::new(ConfigType::GlobalConfig);
        cfg.load_ini(
            r#"
            [Controls]
            player_0_connected\default=false
            player_0_connected=false
            player_0_type\default=false
            player_0_type=5
            player_0_vibration_strength\default=false
            player_0_vibration_strength=63
            "#,
        );
        cfg.begin_group("Controls");

        let mut players = InputSetting::<[PlayerInput; 10]>::new();
        cfg.read_player_values_into(0, &mut players, true);
        cfg.read_player_values_into(1, &mut players, true);
        let first = &players.get_value()[0];
        let second = &players.get_value()[1];

        assert!(!first.connected);
        assert_eq!(first.controller_type, ControllerType::GameCube);
        assert_eq!(first.vibration_strength, 63);
        assert!(!second.connected);
        assert_eq!(second.controller_type, ControllerType::ProController);
    }

    #[test]
    fn missing_global_config_connects_only_player_one() {
        let mut cfg = BaseConfig::new(ConfigType::GlobalConfig);
        cfg.load_ini("");
        cfg.begin_group("Controls");

        let mut players = InputSetting::<[PlayerInput; 10]>::new();
        cfg.read_player_values_into(0, &mut players, true);
        cfg.read_player_values_into(1, &mut players, true);

        assert!(players.get_value()[0].connected);
        assert!(!players.get_value()[1].connected);
    }

    #[test]
    fn per_game_empty_profile_copies_global_player() {
        let mut cfg = BaseConfig::new(ConfigType::PerGameConfig);
        cfg.load_ini(
            r#"
            [Controls]
            player_0_profile_name=
            "#,
        );
        cfg.begin_group("Controls");

        let mut players = InputSetting::<[PlayerInput; 10]>::new();
        players.get_value_explicit_mut(true)[0].connected = true;
        players.get_value_explicit_mut(true)[0].profile_name = "global".to_string();
        players.set_global(false);

        cfg.read_player_values_into(0, &mut players, false);

        assert!(players.get_value()[0].connected);
        assert!(players.get_value()[0].profile_name.is_empty());
    }

    #[test]
    fn test_to_string_bool() {
        assert_eq!(to_string_bool(true), "true");
        assert_eq!(to_string_bool(false), "false");
    }

    #[test]
    fn per_game_generic_setting_round_trips_global_state_and_custom_value() {
        let mut config = BaseConfig::new(ConfigType::PerGameConfig);
        config.load_ini(
            "[Core]\nuse_multi_core\\use_global=false\nuse_multi_core\\default=false\nuse_multi_core=false\n",
        );
        config.begin_group("Core");

        let mut setting = SwitchableSetting::new(true, "use_multi_core", Category::Core);
        config.read_setting_generic(&mut setting);
        assert!(!setting.using_global());
        assert!(!*setting.get_value());
        assert!(*setting.get_value_global());

        setting.set_value(true);
        config.write_setting_generic(&mut setting);
        assert_eq!(
            config
                .ini
                .get("Core")
                .and_then(|section| section.get("use_multi_core\\use_global"))
                .map(String::as_str),
            Some("false")
        );
        assert_eq!(
            config
                .ini
                .get("Core")
                .and_then(|section| section.get("use_multi_core"))
                .map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn scaling_filter_round_trips_the_full_upstream_enum_range() {
        use common::settings_common::Specialization;
        use common::settings_enums::{Category, ScalingFilter};

        let mut config = BaseConfig::new(ConfigType::GlobalConfig);
        config.load_ini("[Renderer]\nscaling_filter\\default=false\nscaling_filter=14\n");
        config.begin_group("Renderer");
        let mut setting = SwitchableSetting::with_options(
            ScalingFilter::Bilinear,
            "scaling_filter",
            Category::Renderer,
            Specialization::DEFAULT,
            true,
            true,
        );

        config.read_setting_generic(&mut setting);
        assert_eq!(*setting.get_value(), ScalingFilter::SgsrEdge);
        config.write_setting_generic(&mut setting);
        assert_eq!(
            config
                .ini
                .get("Renderer")
                .and_then(|section| section.get("scaling_filter"))
                .map(String::as_str),
            Some("14")
        );
    }

    #[test]
    fn network_category_uses_upstream_services_group() {
        let _guard = SETTINGS_TEST_LOCK.lock().unwrap();
        let previous = {
            let values = common::settings::values();
            (
                values.network_interface.clone(),
                values.airplane_mode.clone(),
            )
        };

        let mut config = BaseConfig::new(ConfigType::GlobalConfig);
        config.load_ini(
            "[Services]\nnetwork_interface\\default=false\nnetwork_interface=wlo1\nairplane_mode\\default=false\nairplane_mode=true\n",
        );
        config.read_values();

        {
            let values = common::settings::values();
            assert_eq!(values.network_interface.get_value(), "wlo1");
            assert!(*values.airplane_mode.get_value());
        }

        config.save_values();
        assert_eq!(
            config
                .ini
                .get("Services")
                .and_then(|section| section.get("network_interface"))
                .map(String::as_str),
            Some("wlo1")
        );
        assert!(!config.ini.contains_key("Network"));

        let mut values = common::settings::values_mut();
        values.network_interface = previous.0;
        values.airplane_mode = previous.1;
    }

    #[test]
    fn enum_settings_accept_upstream_canonical_and_numeric_forms() {
        use std::str::FromStr;

        assert_eq!(
            common::settings_enums::RendererBackend::from_str("Vulkan"),
            Ok(common::settings_enums::RendererBackend::Vulkan)
        );
        assert_eq!(
            common::settings_enums::RendererBackend::from_str("1"),
            Ok(common::settings_enums::RendererBackend::Vulkan)
        );
        assert_eq!(
            common::settings_enums::RendererBackend::from_str("Metal"),
            Ok(common::settings_enums::RendererBackend::Metal)
        );
        assert_eq!(
            common::settings_enums::RendererBackend::from_str("5"),
            Ok(common::settings_enums::RendererBackend::Metal)
        );
        assert!(common::settings_enums::RendererBackend::from_str("invalid").is_err());
    }
}
