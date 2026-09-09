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
mod gamemode;
mod gtk_compat;
#[cfg(target_os = "linux")]
mod gui_settings;
mod homebrew_vfs;
mod hotkeys;
mod i18n;
#[cfg(unix)]
mod input_session;
mod install_dialog;
mod loading_screen;
mod main_window;
mod migration_worker;
mod multiplayer;
mod overlay_dialog;
#[cfg(target_os = "macos")]
mod render_window;
mod render;
#[cfg(target_os = "windows")]
mod render_window_windows;
#[cfg(target_os = "linux")]
mod render_window_x11;
mod startup_checks;
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

/// Prefer GTK's Cairo renderer on Windows unless the caller selected another
/// GSK renderer explicitly.
///
/// The Win32 GL renderer creates a `GdkWin32GL` child for each popup surface.
/// Its transparent CSS shadow margins are presented as opaque black pixels,
/// which puts a rectangular black frame around menus, dropdowns, and popovers.
/// Cairo composites those margins correctly. The emulated game keeps using its
/// separate native Vulkan child window, so this only changes GTK's UI renderer.
#[cfg(target_os = "windows")]
fn windows_gsk_renderer_default(current: Option<&std::ffi::OsStr>) -> Option<&'static str> {
    current.is_none().then_some("cairo")
}

#[cfg(target_os = "windows")]
fn configure_windows_gsk_renderer() -> bool {
    let current = std::env::var_os("GSK_RENDERER");
    let Some(value) = windows_gsk_renderer_default(current.as_deref()) else {
        return false;
    };

    std::env::set_var("GSK_RENDERER", value);
    true
}

#[cfg(target_os = "macos")]
fn configure_macos_bundle_runtime() {
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    let Some(contents) = executable.parent().and_then(std::path::Path::parent) else {
        return;
    };
    if contents.file_name().and_then(|name| name.to_str()) != Some("Contents") {
        return;
    }

    let resources = contents.join("Resources");
    let schema_dir = resources.join("share/glib-2.0/schemas");
    if schema_dir.join("gschemas.compiled").is_file()
        && std::env::var_os("GSETTINGS_SCHEMA_DIR").is_none()
    {
        std::env::set_var("GSETTINGS_SCHEMA_DIR", schema_dir);
    }

    let loader_cache = resources.join("gdk-pixbuf-2.0/loaders.cache");
    if loader_cache.is_file() && std::env::var_os("GDK_PIXBUF_MODULE_FILE").is_none() {
        std::env::set_var("GDK_PIXBUF_MODULE_FILE", loader_cache);
    }

    let gio_modules = contents.join("PlugIns/gio");
    if gio_modules.is_dir() && std::env::var_os("GIO_EXTRA_MODULES").is_none() {
        std::env::set_var("GIO_EXTRA_MODULES", gio_modules);
    }
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
    if startup_checks::check_env_vars() {
        return glib::ExitCode::SUCCESS;
    }
    // Homebrew's GTK runtime uses installation-prefix paths. A distributable
    // app supplies equivalent resources inside Contents/Resources instead.
    #[cfg(target_os = "macos")]
    configure_macos_bundle_runtime();

    // This must happen before constructing any GTK object: GtkWindow reads
    // GTK_CSD while deciding how each native toplevel is decorated.
    #[cfg(target_os = "windows")]
    let enabled_native_windows_decorations = configure_windows_native_decorations();

    #[cfg(target_os = "windows")]
    let enabled_windows_cairo_renderer = configure_windows_gsk_renderer();

    #[cfg(target_os = "linux")]
    let _xlib_threading = crate::render_window_x11::initialize_xlib_threads();

    #[cfg(target_os = "linux")]
    let forced_x11 = configure_linux_gdk_backend();

    configuration::qt_config::reload_all_values();
    // Read the persisted setting before probing. The child exits above without
    // reading configuration, starting logging, or initializing GTK.
    let perform_vulkan_check = *common::settings::values().perform_vulkan_check.get_value();
    match startup_checks::startup_checks(perform_vulkan_check) {
        Ok(broken) => uisettings::with_mut(|values| values.has_broken_vulkan = broken),
        Err(error) => eprintln!("Could not run Vulkan startup check: {error}"),
    }
    let log_filter = common::settings::values().log_filter.get_value().clone();
    common::logging::backend::initialize_with_config(
        Some(common::fs::path_util::get_ruzu_path(
            common::fs::path_util::RuzuPath::LogDir,
        )),
        &log_filter,
        uisettings::with(|values| *values.show_console.get_value()),
    );

    #[cfg(target_os = "windows")]
    if enabled_native_windows_decorations {
        log::info!("Using native Win32 window decorations (GTK_CSD=0)");
    }

    #[cfg(target_os = "windows")]
    if enabled_windows_cairo_renderer {
        log::info!("Using the GTK Cairo renderer for correctly composited Win32 popups");
    }

    #[cfg(target_os = "linux")]
    if forced_x11 {
        log::info!("Using the X11 GDK backend for the embedded Linux render surface");
    }

    // Legacy user data is offered for verified, non-destructive migration once
    // the main window is mapped. The explicit `migration_prompt_seen` marker,
    // rather than the eagerly-created config directory, owns first-run state.

    // QtConfig owns the complete reload, including controls and frontend state.
    let interface_language = uisettings::with(|v| v.language.get_value().clone());
    i18n::set_language(&interface_language);
    i18n::configure_toolkit_language(&interface_language);

    // GTK parses/forwards command lines; GMainWindow owns their launch behavior
    // just as Eden's MainWindow constructor does after QApplication startup.
    let app = gtk::Application::builder()
        .application_id(APPLICATION_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN | gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    register_launch_options(&app);
    app.connect_command_line(|app, command_line| {
        let (game, fullscreen) = launch_options(command_line);
        let existing = main_window();
        let window = existing.clone().unwrap_or_else(|| {
            if game.is_some() {
                GMainWindow::new_for_direct_game(app)
            } else {
                GMainWindow::new(app)
            }
        });
        window.present();
        if existing.is_none() {
            set_main_window(Rc::clone(&window));
        }
        window.apply_launch_options(game, fullscreen);
        0
    });

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
        #[cfg(target_os = "windows")]
        main_window::watch_system_theme();
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

    let result = app.run();
    common::logging::backend::stop();
    result
}

fn register_launch_options(app: &impl IsA<gio::Application>) {
    app.add_main_option(
        "fullscreen",
        b'f'.into(),
        glib::OptionFlags::NONE,
        glib::OptionArg::None,
        "Launch the game in fullscreen",
        None,
    );
    app.add_main_option(
        "game",
        b'g'.into(),
        glib::OptionFlags::NONE,
        glib::OptionArg::Filename,
        "Launch a game",
        Some("PATH"),
    );
}

fn launch_options(command_line: &gio::ApplicationCommandLine) -> (Option<String>, bool) {
    let options = command_line.options_dict();
    let (game, fullscreen) = decoded_launch_options(&options, command_line.arguments());
    // Use the invoking process's working directory, including when GApplication
    // forwards this launch to an already running Ruzu instance.
    let game = game
        .and_then(|arg| command_line.create_file_for_arg(arg).path())
        .map(|path| path.to_string_lossy().into_owned());
    (game, fullscreen)
}

fn decoded_launch_options(
    options: &glib::VariantDict,
    arguments: Vec<std::ffi::OsString>,
) -> (Option<std::ffi::OsString>, bool) {
    let fullscreen = options
        .lookup::<bool>("fullscreen")
        .ok()
        .flatten()
        .unwrap_or(false);
    let game = options
        .lookup::<std::ffi::OsString>("game")
        .ok()
        .flatten()
        .or_else(|| arguments.into_iter().skip(1).last());
    (game, fullscreen)
}

#[cfg(test)]
mod launch_tests {
    use super::*;

    #[test]
    fn decoded_shortcut_options_preserve_paths_and_fullscreen() {
        let path = std::ffi::OsString::from("Jeux/Mario Kart é [1].nsp");
        for fullscreen in [false, true] {
            let options = glib::VariantDict::new(None);
            options.insert("game", &path);
            options.insert("fullscreen", fullscreen);
            assert_eq!(
                decoded_launch_options(&options, vec!["ruzu".into()]),
                (Some(path.clone()), fullscreen)
            );
        }
        let options = glib::VariantDict::new(None);
        assert_eq!(
            decoded_launch_options(&options, vec!["ruzu".into(), path.clone()]),
            (Some(path), false)
        );
        assert_eq!(
            decoded_launch_options(&options, vec!["ruzu".into()]),
            (None, false)
        );
        options.insert("fullscreen", true);
        assert_eq!(
            decoded_launch_options(&options, vec!["ruzu".into()]),
            (None, true)
        );
    }

    // GLib on Windows reads the real UTF-16 process command line, ignoring
    // run_with_args' synthetic argv. Validate that platform with the binary.
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn gtk_parses_shortcut_options_and_positional_files() {
        for (arguments, game, fullscreen) in [
            (
                vec!["ruzu", "-f", "-g", "Jeux/Mario Kart é.nsp"],
                Some("Mario Kart é.nsp"),
                true,
            ),
            (
                vec!["ruzu", "--game", "Jeux/Mario Kart é.nsp", "--fullscreen"],
                Some("Mario Kart é.nsp"),
                true,
            ),
            (
                vec!["ruzu", "-g", "Jeux/Mario Kart é.nsp"],
                Some("Mario Kart é.nsp"),
                false,
            ),
            (
                vec!["ruzu", "Jeux/Mario Kart é.nsp"],
                Some("Mario Kart é.nsp"),
                false,
            ),
            (vec!["ruzu", "-f"], None, true),
            (vec!["ruzu"], None, false),
        ] {
            let app = gio::Application::new(
                None,
                gio::ApplicationFlags::NON_UNIQUE | gio::ApplicationFlags::HANDLES_COMMAND_LINE,
            );
            register_launch_options(&app);
            let observed = Rc::new(RefCell::new(None));
            let result = Rc::clone(&observed);
            app.connect_command_line(move |_, command_line| {
                *result.borrow_mut() = Some(launch_options(command_line));
                0
            });
            assert_eq!(app.run_with_args(&arguments), glib::ExitCode::SUCCESS);
            let (actual_game, actual_fullscreen) = observed.borrow_mut().take().unwrap();
            assert_eq!(actual_fullscreen, fullscreen);
            assert_eq!(
                actual_game.as_deref().map(|path| std::path::Path::new(path)
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()),
                game
            );
        }
    }
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

    #[test]
    fn cairo_is_the_windows_gsk_renderer_default() {
        assert_eq!(windows_gsk_renderer_default(None), Some("cairo"));
        assert_eq!(
            windows_gsk_renderer_default(Some(std::ffi::OsStr::new("gl"))),
            None
        );
        assert_eq!(
            windows_gsk_renderer_default(Some(std::ffi::OsStr::new("cairo"))),
            None
        );
    }
}
