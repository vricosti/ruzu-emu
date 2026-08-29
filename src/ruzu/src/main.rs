// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of the upstream Qt frontend entry point in
// `/Users/vricosti/Dev/emulators/zuyu/src/yuzu/main.cpp` (`int main(...)`).
//
// Upstream `main()` constructs a `QApplication`, instantiates the
// `GMainWindow`, shows it, and enters the Qt event loop. Here we construct a
// `gtk::Application`, install the menu bar into the native macOS menu bar on
// `startup`, build the main window on `activate`, and enter the GTK event loop.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};

mod about_dialog;
mod applets;
mod boot;
mod configuration;
mod emu_window;
mod file_menu;
mod game_list;
mod gtk_compat;
#[cfg(target_os = "linux")]
mod gui_settings;
mod homebrew_vfs;
mod i18n;
mod loading_screen;
mod main_window;
mod migration_worker;
mod multiplayer;
mod overlay_dialog;
#[cfg(target_os = "macos")]
mod render_window;
#[cfg(target_os = "windows")]
mod render_window_windows;
#[cfg(target_os = "linux")]
mod render_window_x11;
mod status_bar;
mod uisettings;
mod user_data_migration;
mod util;
mod vk_device_info;

use main_window::GMainWindow;

/// Application identifier — mirrors upstream's reverse-DNS app id conventions
/// (`org.yuzu_emu.yuzu`), adapted for ruzu.
const APPLICATION_ID: &str = "org.ruzu_emu.ruzu";

thread_local! {
    /// Keeps the current main window alive for the process lifetime.
    static MAIN_WINDOW: RefCell<Option<Rc<GMainWindow>>> = const { RefCell::new(None) };
}

/// Store the main window, dropping any previous one.
fn set_main_window(window: Rc<GMainWindow>) {
    MAIN_WINDOW.with(|slot| *slot.borrow_mut() = Some(window));
}

/// Return the process-wide main window, if it has already been created.
///
/// `gio::Application` forwards later launches to the existing process and
/// emits `activate` or `open` again. Reusing the window preserves upstream's
/// single `GMainWindow` lifetime and, critically, its single input subsystem
/// and emulation `System`.
fn main_window() -> Option<Rc<GMainWindow>> {
    MAIN_WINDOW.with(|slot| slot.borrow().as_ref().cloned())
}

/// Let Win32 own the non-client frame unless the caller explicitly asks GTK
/// to draw client-side decorations.
///
/// GTK4 forces CSD on its Windows backend by default. Its transparent resize
/// and shadow margins can be presented as opaque black pixels by the Windows
/// rendering path, producing a rectangular black frame around every toplevel
/// and dialog. Eden uses ordinary native Windows frames through Qt's
/// `windowsvista` style, so disabling GTK's default CSD is the matching
/// platform adaptation.
#[cfg(target_os = "windows")]
fn windows_gtk_csd_default(current: Option<&std::ffi::OsStr>) -> Option<&'static str> {
    current.is_none().then_some("0")
}

#[cfg(target_os = "windows")]
fn configure_windows_native_decorations() -> bool {
    let current = std::env::var_os("GTK_CSD");
    let Some(value) = windows_gtk_csd_default(current.as_deref()) else {
        return false;
    };

    std::env::set_var("GTK_CSD", value);
    true
}

/// Apply the selected interface locale to the live launcher, matching
/// upstream's `GMainWindow::OnLanguageChanged` retranslation step.
pub(crate) fn retranslate_application() {
    if let Some(window) = main_window() {
        window.retranslate();
    }
}

#[cfg(target_os = "linux")]
fn linux_gdk_backend_override(
    current_backend: Option<&str>,
    force_x11: bool,
) -> Option<&'static str> {
    // Eden respects an explicit backend environment override. The persisted
    // preference is applied only when the caller did not already choose one.
    if force_x11 && current_backend.is_none() {
        Some("x11")
    } else {
        None
    }
}

/// Apply the early backend preference before GTK initializes, mirroring
/// Eden's `GraphicsBackend::GetForceX11()` startup path.
#[cfg(target_os = "linux")]
fn configure_linux_gdk_backend() -> bool {
    let current_backend = std::env::var("GDK_BACKEND").ok();
    let force_x11 = crate::gui_settings::get_force_x11();
    let Some(backend) = linux_gdk_backend_override(current_backend.as_deref(), force_x11) else {
        return false;
    };
    std::env::set_var("GDK_BACKEND", backend);
    true
}

fn main() -> glib::ExitCode {
    // This must happen before constructing any GTK object: GtkWindow reads
    // GTK_CSD while deciding how each native toplevel is decorated.
    #[cfg(target_os = "windows")]
    let enabled_native_windows_decorations = configure_windows_native_decorations();

    #[cfg(target_os = "linux")]
    let _xlib_threading = crate::render_window_x11::initialize_xlib_threads();

    #[cfg(target_os = "linux")]
    let forced_x11 = configure_linux_gdk_backend();

    env_logger::init();

    #[cfg(target_os = "windows")]
    if enabled_native_windows_decorations {
        log::info!("Using native Win32 window decorations (GTK_CSD=0)");
    }

    #[cfg(target_os = "linux")]
    if forced_x11 {
        log::info!("Using the X11 GDK backend for the embedded Linux render surface");
    }

    // Legacy user data is offered for verified, non-destructive migration once
    // the main window is mapped. The explicit `migration_prompt_seen` marker,
    // rather than the eagerly-created config directory, owns first-run state.

    // Load the configured game directories out of ruzu's own config, the way
    // upstream's `Config::ReadUIValues` fills `UISettings::values.game_dirs`
    // before the game list is built.
    let game_dirs = configuration::qt_config::load_game_dirs();
    log::info!("Loaded {} configured game directory(ies)", game_dirs.len());
    uisettings::with_mut(|v| v.game_dirs = game_dirs);
    configuration::qt_config::load_external_content_dirs();

    // Upstream `Config::ReadUIGamelistValues` reads the favorites array in the same
    // pass that fills `game_dirs`.
    let favorited_ids = configuration::qt_config::load_favorited_ids();
    uisettings::with_mut(|v| v.favorited_ids = favorited_ids);
    configuration::qt_config::load_favorites_expanded();

    configuration::qt_config::load_ui_language();
    configuration::qt_config::load_view_values();
    configuration::qt_config::load_multiplayer_values();
    let interface_language = uisettings::with(|v| v.language.get_value().clone());
    i18n::set_language(&interface_language);
    i18n::configure_toolkit_language(&interface_language);

    // Upstream's `Config` constructor reads every category, controls included,
    // before the window is built. Without this the Controls page would open on
    // an empty mapping even though one was saved last session.
    configuration::qt_config::load_global_values();
    configuration::qt_config::load_control_values();

    // Upstream constructs `QApplication app(argc, argv)`. We register handling
    // of file arguments ourselves later (open a game passed on the command
    // line), so declare HANDLES_OPEN even though the handler is not wired yet.
    let app = gtk::Application::builder()
        .application_id(APPLICATION_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    // `startup` fires exactly once, before the first `activate`/`open`. This is
    // where the application-scoped menu bar and actions are installed. On the
    // macOS (quartz) GDK backend, the menu model set here is bridged into the
    // native global menu bar at the top of the screen.
    app.connect_startup(|app| {
        // Upstream calls `UpdateUITheme()` early in the `GMainWindow`
        // constructor. It follows the desktop's dark-mode preference for the
        // system themes rather than forcing one, which is why yuzu renders
        // light on a light Linux desktop and dark on a dark macOS one.
        main_window::update_ui_theme();
        main_window::init_app_menu(app);
    });

    // Upstream: `GMainWindow main_window{...}; main_window.show();`
    // The `Rc<GMainWindow>` must outlive the closure — GTK keeps the widget
    // tree, but our wrapper owns the session, loading screen, and the `Weak`
    // captured by the menu actions. Keep it in a thread-local.
    app.connect_activate(|app| {
        if let Some(window) = main_window() {
            window.present();
            return;
        }

        let window = GMainWindow::new(app);
        window.present();
        set_main_window(window);
    });

    // With HANDLES_OPEN set, GTK routes file arguments to `open` instead of
    // `activate`. Boot the first file directly (like `yuzu <game>`); the window
    // defers the boot until its render surface is realized.
    app.connect_open(|app, files, _hint| {
        let existing_window = main_window();
        let window = existing_window
            .clone()
            .unwrap_or_else(|| GMainWindow::new_for_direct_game(app));
        window.present();
        if let Some(path) = files.first().and_then(|f| f.path()) {
            window.boot_game(path.to_string_lossy().into_owned());
        }
        if existing_window.is_none() {
            set_main_window(window);
        }
    });

    app.run()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn linux_launcher_uses_x11_when_the_persisted_preference_is_enabled() {
        assert_eq!(linux_gdk_backend_override(None, true), Some("x11"));
        assert_eq!(linux_gdk_backend_override(Some("x11"), true), None);
        assert_eq!(linux_gdk_backend_override(Some("wayland"), true), None);
    }

    #[test]
    fn linux_launcher_keeps_the_default_backend_without_the_preference() {
        assert_eq!(linux_gdk_backend_override(None, false), None);
        assert_eq!(linux_gdk_backend_override(Some("wayland"), false), None);
    }
}

#[cfg(all(test, target_os = "windows"))]
mod windows_tests {
    use super::*;

    #[test]
    fn native_decorations_are_the_windows_default() {
        assert_eq!(windows_gtk_csd_default(None), Some("0"));
        assert_eq!(
            windows_gtk_csd_default(Some(std::ffi::OsStr::new("1"))),
            None
        );
        assert_eq!(
            windows_gtk_csd_default(Some(std::ffi::OsStr::new("0"))),
            None
        );
    }
}
