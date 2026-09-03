// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust counterpart of the upstream frontend-only settings container in
// `/home/vricosti/Dev/emulators/zuyu/src/yuzu/uisettings.h` +
// `uisettings.cpp` (`UISettings::Values` / `UISettings::values`).
//
// These settings belong to the *frontend*, not to `Common::Settings`: upstream
// keeps them in the `yuzu` GUI target because they describe window chrome, the
// game list, screenshots, and multiplayer lobby state. The ruzu port therefore
// owns them here rather than in the `common` crate, matching upstream's file
// ownership.
//
// Upstream stores each field in a `Setting<T>` registered against a `linkage`
// object so the config serializer can walk them. The ruzu port reuses
// `common::settings_common::Setting<T>`, which provides the same
// `get_value` / `set_value` / `get_default` contract.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use common::settings_common::{Setting, SwitchableSetting};
use common::settings_enums::{Category, ConfirmStop};

/// Upstream `UISettings::values.is_game_list_reload_pending`.
static GAME_LIST_RELOAD_PENDING: AtomicBool = AtomicBool::new(false);

pub fn request_game_list_reload() {
    GAME_LIST_RELOAD_PENDING.store(true, Ordering::Release);
}

pub fn take_game_list_reload_pending() -> bool {
    GAME_LIST_RELOAD_PENDING.swap(false, Ordering::AcqRel)
}

/// One configured game directory — upstream `UISettings::GameDir`.
///
/// `path` may be one of the special provider tokens (`SDMC`, `UserNAND`,
/// `SysNAND`) rather than a filesystem path; upstream stores them in the same
/// array and distinguishes them when scanning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameDir {
    pub path: String,
    pub deep_scan: bool,
    pub expanded: bool,
}

/// One configured frontend shortcut — upstream `UISettings::Shortcut` and
/// `UISettings::ContextualShortcut`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shortcut {
    pub name: String,
    pub group: String,
    pub keyseq: String,
    pub controller_keyseq: String,
    pub context: i32,
    pub repeat: bool,
}

/// Static counterpart of upstream `UISettings::default_hotkeys`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DefaultHotkey {
    pub name: &'static str,
    pub group: &'static str,
    pub keyseq: &'static str,
    pub controller_keyseq: &'static str,
    pub context: i32,
    pub repeat: bool,
}

const WINDOW_SHORTCUT: i32 = 1;
const APPLICATION_SHORTCUT: i32 = 2;
const WIDGET_WITH_CHILDREN_SHORTCUT: i32 = 3;

/// Kept in the exact upstream positional order required by its shortcut
/// serializer (including recently appended entries at the end).
#[rustfmt::skip]
pub const DEFAULT_HOTKEYS: &[DefaultHotkey] = &[
    DefaultHotkey { name: "Audio Mute/Unmute", group: "Main Window", keyseq: "Ctrl+M", controller_keyseq: "Home+Dpad_Right", context: WINDOW_SHORTCUT, repeat: false },
    DefaultHotkey { name: "Audio Volume Down", group: "Main Window", keyseq: "-", controller_keyseq: "Home+Dpad_Down", context: APPLICATION_SHORTCUT, repeat: true },
    DefaultHotkey { name: "Audio Volume Up", group: "Main Window", keyseq: "=", controller_keyseq: "Home+Dpad_Up", context: APPLICATION_SHORTCUT, repeat: true },
    DefaultHotkey { name: "Capture Screenshot", group: "Main Window", keyseq: "Ctrl+P", controller_keyseq: "Screenshot", context: WIDGET_WITH_CHILDREN_SHORTCUT, repeat: false },
    DefaultHotkey { name: "Change Adapting Filter", group: "Main Window", keyseq: "F8", controller_keyseq: "Home+L", context: APPLICATION_SHORTCUT, repeat: false },
    DefaultHotkey { name: "Change Docked Mode", group: "Main Window", keyseq: "F10", controller_keyseq: "Home+X", context: APPLICATION_SHORTCUT, repeat: false },
    DefaultHotkey { name: "Change GPU Mode", group: "Main Window", keyseq: "F9", controller_keyseq: "Home+R", context: APPLICATION_SHORTCUT, repeat: false },
    DefaultHotkey { name: "Configure", group: "Main Window", keyseq: "Ctrl+,", controller_keyseq: "", context: WIDGET_WITH_CHILDREN_SHORTCUT, repeat: false },
    DefaultHotkey { name: "Configure Current Game", group: "Main Window", keyseq: "Ctrl+.", controller_keyseq: "", context: WIDGET_WITH_CHILDREN_SHORTCUT, repeat: false },
    DefaultHotkey { name: "Continue/Pause Emulation", group: "Main Window", keyseq: "F4", controller_keyseq: "Home+Plus", context: WINDOW_SHORTCUT, repeat: false },
    DefaultHotkey { name: "Exit Fullscreen", group: "Main Window", keyseq: "Esc", controller_keyseq: "", context: WINDOW_SHORTCUT, repeat: false },
    DefaultHotkey { name: "Exit ruzu", group: "Main Window", keyseq: "Ctrl+Q", controller_keyseq: "Home+Minus", context: WINDOW_SHORTCUT, repeat: false },
    DefaultHotkey { name: "Fullscreen", group: "Main Window", keyseq: "F11", controller_keyseq: "Home+B", context: WINDOW_SHORTCUT, repeat: false },
    DefaultHotkey { name: "Load File", group: "Main Window", keyseq: "Ctrl+O", controller_keyseq: "", context: WIDGET_WITH_CHILDREN_SHORTCUT, repeat: false },
    DefaultHotkey { name: "Load/Remove Amiibo", group: "Main Window", keyseq: "F2", controller_keyseq: "Home+A", context: WIDGET_WITH_CHILDREN_SHORTCUT, repeat: false },
    DefaultHotkey { name: "Browse Public Game Lobby", group: "Main Window", keyseq: "Ctrl+B", controller_keyseq: "", context: APPLICATION_SHORTCUT, repeat: false },
    DefaultHotkey { name: "Create Room", group: "Main Window", keyseq: "Ctrl+N", controller_keyseq: "", context: APPLICATION_SHORTCUT, repeat: false },
    DefaultHotkey { name: "Direct Connect to Room", group: "Main Window", keyseq: "Ctrl+C", controller_keyseq: "", context: APPLICATION_SHORTCUT, repeat: false },
    DefaultHotkey { name: "Leave Room", group: "Main Window", keyseq: "Ctrl+L", controller_keyseq: "", context: APPLICATION_SHORTCUT, repeat: false },
    DefaultHotkey { name: "Show Current Room", group: "Main Window", keyseq: "Ctrl+R", controller_keyseq: "", context: APPLICATION_SHORTCUT, repeat: false },
    DefaultHotkey { name: "Restart Emulation", group: "Main Window", keyseq: "F6", controller_keyseq: "R+Plus+Minus", context: WINDOW_SHORTCUT, repeat: false },
    DefaultHotkey { name: "Stop Emulation", group: "Main Window", keyseq: "F5", controller_keyseq: "L+Plus+Minus", context: WINDOW_SHORTCUT, repeat: false },
    DefaultHotkey { name: "TAS Record", group: "Main Window", keyseq: "Ctrl+F7", controller_keyseq: "", context: APPLICATION_SHORTCUT, repeat: false },
    DefaultHotkey { name: "TAS Reset", group: "Main Window", keyseq: "Ctrl+F6", controller_keyseq: "", context: APPLICATION_SHORTCUT, repeat: false },
    DefaultHotkey { name: "TAS Start/Stop", group: "Main Window", keyseq: "Ctrl+F5", controller_keyseq: "", context: APPLICATION_SHORTCUT, repeat: false },
    DefaultHotkey { name: "Toggle Filter Bar", group: "Main Window", keyseq: "Ctrl+F", controller_keyseq: "", context: WINDOW_SHORTCUT, repeat: false },
    DefaultHotkey { name: "Toggle Framerate Limit", group: "Main Window", keyseq: "Ctrl+U", controller_keyseq: "Home+Y", context: APPLICATION_SHORTCUT, repeat: false },
    DefaultHotkey { name: "Toggle Turbo Speed", group: "Main Window", keyseq: "Ctrl+Z", controller_keyseq: "", context: APPLICATION_SHORTCUT, repeat: false },
    DefaultHotkey { name: "Toggle Slow Speed", group: "Main Window", keyseq: "Ctrl+X", controller_keyseq: "", context: APPLICATION_SHORTCUT, repeat: false },
    DefaultHotkey { name: "Toggle Mouse Panning", group: "Main Window", keyseq: "Ctrl+F9", controller_keyseq: "", context: APPLICATION_SHORTCUT, repeat: false },
    DefaultHotkey { name: "Toggle Renderdoc Capture", group: "Main Window", keyseq: "", controller_keyseq: "", context: APPLICATION_SHORTCUT, repeat: false },
    DefaultHotkey { name: "Toggle Status Bar", group: "Main Window", keyseq: "Ctrl+S", controller_keyseq: "", context: WINDOW_SHORTCUT, repeat: false },
    DefaultHotkey { name: "Toggle Performance Overlay", group: "Main Window", keyseq: "Ctrl+V", controller_keyseq: "", context: WINDOW_SHORTCUT, repeat: false },
];

pub fn default_shortcuts() -> Vec<Shortcut> {
    DEFAULT_HOTKEYS
        .iter()
        .map(|hotkey| Shortcut {
            name: hotkey.name.to_owned(),
            group: hotkey.group.to_owned(),
            keyseq: hotkey.keyseq.to_owned(),
            controller_keyseq: hotkey.controller_keyseq.to_owned(),
            context: hotkey.context,
            repeat: hotkey.repeat,
        })
        .collect()
}

impl GameDir {
    /// Whether this entry is a real filesystem directory rather than one of the
    /// `SDMC` / `UserNAND` / `SysNAND` provider tokens.
    pub fn is_filesystem_path(&self) -> bool {
        !matches!(self.path.as_str(), "SDMC" | "UserNAND" | "SysNAND")
    }
}

/// Frontend settings container — upstream `UISettings::Values`.
#[derive(Clone)]
pub struct Values {
    /// Configured game directories — upstream `UISettings::values.game_dirs`.
    /// Not a `Setting<T>`: upstream stores it as a plain `QVector<GameDir>`
    /// serialized through `QSettings::beginWriteArray`, not through the
    /// settings registry.
    pub game_dirs: Vec<GameDir>,

    /// Program IDs the user marked as favorites — upstream
    /// `UISettings::values.favorited_ids`. Like `game_dirs`, upstream keeps it as a
    /// plain `QVector<u64>` written through `BeginArray`/`EndArray` rather than
    /// through the settings registry, so it is not a `Setting<T>` here either.
    pub favorited_ids: Vec<u64>,

    /// Frontend shortcuts — upstream `UISettings::values.shortcuts`.
    pub shortcuts: Vec<Shortcut>,

    // ── Ui ──────────────────────────────────────────────────────────────
    pub single_window_mode: Setting<bool>,
    pub fullscreen: Setting<bool>,
    pub display_titlebar: Setting<bool>,
    pub show_filter_bar: Setting<bool>,
    pub show_status_bar: Setting<bool>,

    pub confirm_before_stopping: Setting<ConfirmStop>,
    pub pause_when_in_background: Setting<bool>,
    pub mute_when_in_background: Setting<bool>,
    pub hide_mouse: Setting<bool>,
    pub controller_applet_disabled: Setting<bool>,
    pub select_user_on_boot: Setting<bool>,
    pub enable_gamemode: SwitchableSetting<bool>,

    /// Interface language, as a locale string ("" = use the system language).
    pub language: Setting<String>,
    /// Widget theme name, matching one of [`THEMES`].
    pub theme: Setting<String>,
    /// Mirror the log to a console window — upstream `show_console`.
    pub show_console: Setting<bool>,
    #[cfg(unix)]
    pub gui_force_x11: Setting<bool>,
    #[cfg(unix)]
    pub gui_hide_backend_warning: Setting<bool>,

    // ── Screenshots ─────────────────────────────────────────────────────
    pub enable_screenshot_save_as: Setting<bool>,
    pub screenshot_path: Setting<String>,

    // ── Multiplayer ─────────────────────────────────────────────────────
    // Upstream `UISettings::values.multiplayer_{nickname,ip,port}`, remembered
    // between runs so the Direct Connect dialog reopens on the last room used.
    pub multiplayer_nickname: Setting<String>,
    pub multiplayer_filter_text: Setting<String>,
    pub multiplayer_filter_games_owned: Setting<bool>,
    pub multiplayer_filter_hide_empty: Setting<bool>,
    pub multiplayer_filter_hide_full: Setting<bool>,
    pub multiplayer_ip: Setting<String>,
    pub multiplayer_port: Setting<u32>,
    pub screenshot_height: Setting<u32>,

    // ── UiGameList ──────────────────────────────────────────────────────
    pub show_add_ons: Setting<bool>,
    pub show_compat: Setting<bool>,
    pub show_size: Setting<bool>,
    pub show_types: Setting<bool>,
    pub show_play_time: Setting<bool>,
    pub game_icon_size: Setting<u32>,
    pub folder_icon_size: Setting<u32>,
    pub row_1_text_id: Setting<u8>,
    pub row_2_text_id: Setting<u8>,
    pub cache_game_list: Setting<bool>,
    pub favorites_expanded: Setting<bool>,
}

impl Default for Values {
    fn default() -> Self {
        use Category::*;

        Self {
            game_dirs: Vec::new(),
            favorited_ids: Vec::new(),
            shortcuts: default_shortcuts(),

            single_window_mode: Setting::new(true, "singleWindowMode", Ui),
            fullscreen: Setting::new(false, "fullscreen", Ui),
            display_titlebar: Setting::new(true, "displayTitleBars", Ui),
            show_filter_bar: Setting::new(true, "showFilterBar", Ui),
            show_status_bar: Setting::new(true, "showStatusBar", Ui),

            confirm_before_stopping: Setting::new(ConfirmStop::AskAlways, "confirmStop", Ui),
            pause_when_in_background: Setting::new(false, "pauseWhenInBackground", Ui),
            mute_when_in_background: Setting::new(false, "muteWhenInBackground", Ui),
            hide_mouse: Setting::new(true, "hideInactiveMouse", Ui),
            controller_applet_disabled: Setting::new(false, "disableControllerApplet", Ui),
            select_user_on_boot: Setting::new(false, "select_user_on_boot", Ui),
            enable_gamemode: SwitchableSetting::new(
                !cfg!(target_env = "msvc"),
                "enable_gamemode",
                UiGeneral,
            ),

            language: Setting::new(String::new(), "language", Paths),
            theme: Setting::new(String::from("Default Colorful"), "theme", Ui),
            show_console: Setting::new(false, "showConsole", Ui),
            #[cfg(unix)]
            gui_force_x11: Setting::new(false, "gui_force_x11", UiGeneral),
            #[cfg(unix)]
            gui_hide_backend_warning: Setting::new(false, "gui_hide_backend_warning", UiGeneral),

            enable_screenshot_save_as: Setting::new(true, "enable_screenshot_save_as", Screenshots),
            screenshot_path: Setting::new(String::new(), "screenshot_path", Screenshots),
            multiplayer_nickname: Setting::new(String::new(), "nickname", Multiplayer),
            multiplayer_filter_text: Setting::new(String::new(), "filter_text", Multiplayer),
            multiplayer_filter_games_owned: Setting::new(false, "filter_games_owned", Multiplayer),
            multiplayer_filter_hide_empty: Setting::new(
                false,
                "filter_games_hide_empty",
                Multiplayer,
            ),
            multiplayer_filter_hide_full: Setting::new(
                false,
                "filter_games_hide_full",
                Multiplayer,
            ),
            multiplayer_ip: Setting::new(String::new(), "ip", Multiplayer),
            multiplayer_port: Setting::new(24872, "port", Multiplayer),
            screenshot_height: Setting::new(0, "screenshot_height", Screenshots),

            show_add_ons: Setting::new(true, "show_add_ons", UiGameList),
            show_compat: Setting::new(false, "show_compat", UiGameList),
            show_size: Setting::new(true, "show_size", UiGameList),
            show_types: Setting::new(true, "show_types", UiGameList),
            show_play_time: Setting::new(true, "show_play_time", UiGameList),
            game_icon_size: Setting::new(64, "game_icon_size", UiGameList),
            folder_icon_size: Setting::new(48, "folder_icon_size", UiGameList),
            row_1_text_id: Setting::new(3, "row_1_text_id", UiGameList),
            row_2_text_id: Setting::new(2, "row_2_text_id", UiGameList),
            cache_game_list: Setting::new(true, "cache_game_list", UiGameList),
            favorites_expanded: Setting::new(true, "favorites_expanded", UiGameList),
        }
    }
}

/// Selectable widget themes — upstream `UISettings::themes`.
///
/// Each entry is `(display name, internal name)`. GTK has no direct equivalent
/// of Qt's `.qss` stylesheet themes, so only the two GTK provides natively
/// (light / dark, via `gtk-application-prefer-dark-theme`) actually change the
/// appearance; the rest are kept so the combo box matches upstream's contents.
pub const THEMES: &[(&str, &str)] = &[
    ("Default", "default"),
    ("Default Colorful", "colorful"),
    ("Dark", "qdarkstyle"),
    ("Dark Colorful", "colorful_dark"),
    ("Midnight Blue", "qdarkstyle_midnight_blue"),
    ("Midnight Blue Colorful", "colorful_midnight_blue"),
];

/// Game-list row text sources — upstream `UISettings::game_list_row_text`,
/// indexed by `row_1_text_id` / `row_2_text_id`.
pub const GAME_LIST_ROW_TEXT: &[&str] = &["Filename", "Filetype", "Title ID", "Title Name", "None"];

static VALUES: RwLock<Option<Values>> = RwLock::new(None);

/// Shared read access to the frontend settings — upstream `UISettings::values`.
pub fn values() -> RwLockReadGuard<'static, Option<Values>> {
    ensure_initialized();
    VALUES.read().unwrap()
}

/// Shared write access to the frontend settings.
pub fn values_mut() -> RwLockWriteGuard<'static, Option<Values>> {
    ensure_initialized();
    VALUES.write().unwrap()
}

fn ensure_initialized() {
    if VALUES.read().unwrap().is_some() {
        return;
    }
    let mut guard = VALUES.write().unwrap();
    if guard.is_none() {
        *guard = Some(Values::default());
    }
}

/// Convenience: run `f` with a shared reference to the values.
pub fn with<R>(f: impl FnOnce(&Values) -> R) -> R {
    let guard = values();
    f(guard.as_ref().expect("UI settings initialized"))
}

/// Convenience: run `f` with a mutable reference to the values.
pub fn with_mut<R>(f: impl FnOnce(&mut Values) -> R) -> R {
    let mut guard = values_mut();
    f(guard.as_mut().expect("UI settings initialized"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_upstream_uisettings() {
        // Spot-check the defaults declared in upstream `uisettings.h`.
        let v = Values::default();
        assert!(*v.single_window_mode.get_value());
        assert!(*v.show_status_bar.get_value());
        assert!(*v.hide_mouse.get_value());
        assert!(!*v.pause_when_in_background.get_value());
        assert_eq!(*v.game_icon_size.get_value(), 64);
        assert_eq!(*v.folder_icon_size.get_value(), 48);
        assert_eq!(*v.row_1_text_id.get_value(), 3);
        assert_eq!(*v.row_2_text_id.get_value(), 2);
        assert!(*v.favorites_expanded.get_value());
    }

    #[test]
    fn row_text_ids_index_game_list_row_text() {
        let v = Values::default();
        // Upstream's defaults render "Title Name" over "Title ID".
        assert_eq!(
            GAME_LIST_ROW_TEXT[*v.row_1_text_id.get_value() as usize],
            "Title Name"
        );
        assert_eq!(
            GAME_LIST_ROW_TEXT[*v.row_2_text_id.get_value() as usize],
            "Title ID"
        );
    }

    #[test]
    fn game_dir_distinguishes_only_upstream_provider_tokens() {
        let game_dir = |path: &str| GameDir {
            path: path.to_owned(),
            deep_scan: false,
            expanded: true,
        };

        assert!(!game_dir("SDMC").is_filesystem_path());
        assert!(!game_dir("UserNAND").is_filesystem_path());
        assert!(!game_dir("SysNAND").is_filesystem_path());
        assert!(game_dir("/games/switch").is_filesystem_path());
        assert!(game_dir(r"D:\Games\Switch").is_filesystem_path());
        assert!(game_dir(r"\\server\share\Switch").is_filesystem_path());
    }

    #[test]
    fn global_values_are_lazily_initialized() {
        let icon = with(|v| *v.game_icon_size.get_value());
        assert_eq!(icon, 64);
        with_mut(|v| v.game_icon_size.set_value(128));
        assert_eq!(with(|v| *v.game_icon_size.get_value()), 128);
        with_mut(|v| v.game_icon_size.set_value(64));
    }
}
