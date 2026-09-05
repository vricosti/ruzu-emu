// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later
//
//! GTK counterpart of Eden `src/qt_common/util/game.{h,cpp}`'s shortcut slice.

use std::path::{Path, PathBuf};
use std::rc::Rc;
#[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gtk::prelude::*;
use ruzu_core::file_sys::patch_manager::PatchManager;
use ruzu_core::hle::service::filesystem::filesystem::FileSystemController;
use ruzu_core::loader::loader::{get_loader, ResultStatus, System as LoaderSystem};

/// Eden `QtCommon::Game::OpenRootDataFolder`.
pub fn open_root_data_folder() {
    let path = common::fs::path_util::get_ruzu_path(common::fs::path_util::RuzuPath::RuzuDir);
    if let Err(error) = open_folder(&path) {
        log::error!("Failed to open ruzu folder {}: {error}", path.display());
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn open_folder(path: &Path) -> std::io::Result<()> {
    // GIO's Windows build does not necessarily ship a default `file://` URI
    // handler. `QDesktopServices::openUrl(QUrl::fromLocalFile(...))` reaches
    // Explorer through the native shell in Eden; invoke Explorer directly here.
    windows_open_folder_command(path).spawn().map(drop)
}

#[cfg(target_os = "windows")]
fn windows_open_folder_command(path: &Path) -> std::process::Command {
    let mut command = std::process::Command::new("explorer.exe");
    command.arg(path);
    command
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn open_folder(path: &Path) -> std::io::Result<()> {
    let directory = gtk::gio::File::for_path(path);
    gtk::gio::AppInfo::launch_default_for_uri(&directory.uri(), gtk::gio::AppLaunchContext::NONE)
        .map_err(|error| std::io::Error::other(error.to_string()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub enum ShortcutTarget {
    Desktop,
    Applications,
}

struct ShortcutData {
    parent: gtk::Window,
    shortcut_path: PathBuf,
    command: PathBuf,
    icon_path: PathBuf,
    arguments: String,
    game_title: String,
}

/// Eden `QtCommon::Game::ResetMetadata`.
///
/// The game-list cache includes the `pv.txt` files used by the Add-ons column
/// and ruzu's `arch.txt` files, so a manual refresh must remove the complete
/// directory before rebuilding the frontend content provider.
pub fn reset_metadata(parent: Option<&gtk::Window>, show_message: bool) {
    let cache_dir = common::fs::path_util::get_ruzu_path(common::fs::path_util::RuzuPath::CacheDir)
        .join("game_list");
    match remove_metadata_cache(&cache_dir) {
        Ok(false) => {
            if show_message {
                crate::gtk_compat::show_warning(
                    parent,
                    "Reset Metadata Cache",
                    "The metadata cache is already empty.",
                );
            }
        }
        Ok(true) => {
            crate::uisettings::request_game_list_reload();
            if show_message {
                crate::gtk_compat::show_message(
                    parent,
                    "Reset Metadata Cache",
                    "The operation completed successfully.",
                );
            }
        }
        Err(error) => {
            log::error!(
                "Failed to remove metadata cache {}: {error}",
                cache_dir.display()
            );
            if show_message {
                crate::gtk_compat::show_warning(
                    parent,
                    "Reset Metadata Cache",
                    "The metadata cache couldn't be deleted. It might be in use or non-existent.",
                );
            }
        }
    }
}

fn remove_metadata_cache(cache_dir: &Path) -> std::io::Result<bool> {
    if !cache_dir.try_exists()? {
        return Ok(false);
    }
    std::fs::remove_dir_all(cache_dir)?;
    Ok(true)
}

/// Eden `QtCommon::Game::CreateShortcut`.
pub fn create_shortcut(
    parent: &gtk::ApplicationWindow,
    game_path: &str,
    program_id: u64,
    game_title: &str,
    target: ShortcutTarget,
    arguments: String,
    needs_title: bool,
) {
    let command = get_ruzu_command();
    let Some(shortcut_path) = get_shortcut_path(target) else {
        show_failed(parent, game_title);
        return;
    };
    if !shortcut_path.exists() {
        log::error!("Invalid shortcut target {}", shortcut_path.display());
        show_failed(parent, &shortcut_path.to_string_lossy());
        return;
    }

    let (loader_title, icon) = read_title_and_icon(game_path, program_id);
    let game_title = if needs_title {
        loader_title.unwrap_or_else(|| format!("{program_id:016X}"))
    } else {
        game_title.to_owned()
    };
    let game_title = sanitize_shortcut_name(&game_title);

    let icon_path = match make_shortcut_icon_path(program_id, &game_title) {
        Ok(path) => path,
        Err(error) => {
            log::error!("Cannot create shortcut icon path: {error}");
            crate::gtk_compat::show_error(
                Some(parent),
                "Create Icon",
                &crate::i18n::tr_args(
                    "Cannot create icon file. Path \"%1\" does not exist and cannot be created.",
                    &[error.to_string()],
                ),
            );
            PathBuf::new()
        }
    };
    if !icon_path.as_os_str().is_empty() && !icon.is_empty() {
        if let Err(error) = save_icon_to_file(&icon_path, &icon) {
            log::error!("Could not write icon to file: {error}");
        }
    }

    let data = Rc::new(ShortcutData {
        parent: parent.clone().upcast(),
        shortcut_path,
        command,
        icon_path,
        arguments,
        game_title,
    });

    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
    if data.command.to_string_lossy().ends_with(".AppImage")
        && !APPIMAGE_SHORTCUT_ALREADY_WARNED.load(Ordering::Relaxed)
    {
        let data_for_answer = Rc::clone(&data);
        crate::gtk_compat::ask_question(
            Some(&data.parent),
            "Shortcut may be Volatile!",
            "This will create a shortcut to the current AppImage. This may not work well if you update. Continue?",
            "Cancel",
            "OK",
            move |accepted| {
                if accepted {
                    APPIMAGE_SHORTCUT_ALREADY_WARNED.store(true, Ordering::Relaxed);
                    ask_fullscreen(data_for_answer);
                }
            },
        );
        return;
    }

    ask_fullscreen(data);
}

fn ask_fullscreen(data: Rc<ShortcutData>) {
    let data_for_answer = Rc::clone(&data);
    crate::gtk_compat::ask_question(
        Some(&data.parent),
        "Create Shortcut",
        "Do you want to launch the game in fullscreen?",
        "No",
        "Yes",
        move |fullscreen| finish_create_shortcut(&data_for_answer, fullscreen),
    );
}

fn finish_create_shortcut(data: &ShortcutData, fullscreen: bool) {
    let arguments = if fullscreen {
        format!("-f {}", data.arguments)
    } else {
        data.arguments.clone()
    };
    let comment = format!("Start {} with the Ruzu Emulator", data.game_title);
    let created = create_shortcut_link(
        &data.shortcut_path,
        &comment,
        &data.icon_path,
        &data.command,
        &arguments,
        "Game;Emulator;Qt;",
        "Switch;Nintendo;",
        &data.game_title,
    );

    if created {
        crate::gtk_compat::show_message(
            Some(&data.parent),
            "Shortcut Created",
            &crate::i18n::tr_args(
                "Successfully created a shortcut to %1",
                std::slice::from_ref(&data.game_title),
            ),
        );
    } else {
        show_failed(&data.parent, &data.game_title);
    }
}

fn show_failed(parent: &impl IsA<gtk::Window>, game_title: &str) {
    crate::gtk_compat::show_error(
        Some(parent),
        "Failed to Create Shortcut",
        &crate::i18n::tr_args(
            "Failed to create a shortcut to %1",
            &[game_title.to_owned()],
        ),
    );
}

fn get_ruzu_command() -> PathBuf {
    std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from("ruzu"))
}

#[cfg_attr(
    any(target_os = "macos", target_os = "android"),
    allow(unused_variables)
)]
pub fn get_shortcut_path(target: ShortcutTarget) -> Option<PathBuf> {
    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
    {
        return match target {
            ShortcutTarget::Desktop => {
                gtk::glib::user_special_dir(gtk::glib::UserDirectory::Desktop)
            }
            ShortcutTarget::Applications => Some(gtk::glib::user_data_dir().join("applications")),
        };
    }

    #[cfg(target_os = "windows")]
    {
        return match target {
            ShortcutTarget::Desktop => std::env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .map(|path| path.join("Desktop")),
            ShortcutTarget::Applications => std::env::var_os("APPDATA")
                .map(PathBuf::from)
                .map(|path| path.join("Microsoft/Windows/Start Menu/Programs")),
        };
    }

    #[allow(unreachable_code)]
    None
}

fn make_shortcut_icon_path(program_id: u64, game_title: &str) -> std::io::Result<PathBuf> {
    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
    let directory = gtk::glib::user_data_dir().join("icons/hicolor/256x256");
    #[cfg(not(all(unix, not(target_os = "macos"), not(target_os = "android"))))]
    let directory = common::fs::path_util::get_ruzu_path(common::fs::path_util::RuzuPath::IconsDir);

    std::fs::create_dir_all(&directory)?;
    let name = if program_id == 0 {
        format!("ruzu-{game_title}.png")
    } else {
        format!("ruzu-{program_id:016X}.png")
    };
    Ok(directory.join(name))
}

fn save_icon_to_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let texture = gtk::gdk::Texture::from_bytes(&gtk::glib::Bytes::from(bytes))
        .map_err(|error| error.to_string())?;
    texture.save_to_png(path).map_err(|error| error.to_string())
}

fn read_title_and_icon(game_path: &str, program_id: u64) -> (Option<String>, Vec<u8>) {
    let vfs = crate::game_list::frontend_vfs();
    let content_provider = crate::game_list::frontend_content_provider_union();
    let mut controller = FileSystemController::new();
    controller.set_content_provider(Arc::clone(&content_provider));
    controller.create_factories(Arc::clone(&vfs), false);
    let controller = Arc::new(Mutex::new(controller));
    let mut loader_system = LoaderSystem::new(
        Some(Arc::clone(&content_provider)),
        Some(Arc::clone(&controller)),
    );

    let Some(file) = vfs.arc_open_file(
        game_path,
        ruzu_core::file_sys::fs_filesystem::OpenMode::READ,
    ) else {
        return (None, Vec::new());
    };
    let Some(loader) = get_loader(&mut loader_system, file, 0, 0) else {
        return (None, Vec::new());
    };

    let (control, control_icon) = {
        let controller = controller.lock().unwrap_or_else(|error| error.into_inner());
        let content_provider = content_provider
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        PatchManager::new(program_id, &controller, &*content_provider).get_control_metadata()
    };

    let title = control
        .map(|control| control.get_application_name())
        .or_else(|| {
            let mut title = String::new();
            (loader.read_title(&mut title) == ResultStatus::Success).then_some(title)
        });
    let icon = control_icon
        .map(|icon| icon.read_all_bytes())
        .unwrap_or_else(|| {
            let mut icon = Vec::new();
            if loader.read_icon(&mut icon) == ResultStatus::Success {
                icon
            } else {
                Vec::new()
            }
        });
    (title, icon)
}

fn sanitize_shortcut_name(title: &str) -> String {
    title
        .chars()
        .filter(|character| !"<>:\"/\\|?*.".contains(*character))
        .collect()
}

#[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
fn create_shortcut_link(
    shortcut_path: &Path,
    comment: &str,
    icon_path: &Path,
    command: &Path,
    arguments: &str,
    categories: &str,
    keywords: &str,
    name: &str,
) -> bool {
    let contents = desktop_entry_contents(
        comment, icon_path, command, arguments, categories, keywords, name,
    );
    let path = shortcut_path.join(format!("{name}.desktop"));
    if let Err(error) = std::fs::write(&path, contents) {
        log::error!("Failed to create shortcut {}: {error}", path.display());
        return false;
    }
    true
}

#[cfg(target_os = "windows")]
fn create_shortcut_link(
    shortcut_path: &Path,
    comment: &str,
    icon_path: &Path,
    command: &Path,
    arguments: &str,
    _categories: &str,
    _keywords: &str,
    name: &str,
) -> bool {
    let shortcut = shortcut_path.join(format!("{name}.lnk"));
    let script = "$s=(New-Object -ComObject WScript.Shell).CreateShortcut($args[0]);$s.TargetPath=$args[1];$s.Arguments=$args[2];$s.Description=$args[3];if(Test-Path $args[4]){$s.IconLocation=$args[4]};$s.Save()";
    std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .arg(shortcut)
        .arg(command)
        .arg(arguments)
        .arg(comment)
        .arg(icon_path)
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(not(any(
    target_os = "windows",
    all(unix, not(target_os = "macos"), not(target_os = "android"))
)))]
fn create_shortcut_link(
    _shortcut_path: &Path,
    _comment: &str,
    _icon_path: &Path,
    _command: &Path,
    _arguments: &str,
    _categories: &str,
    _keywords: &str,
    _name: &str,
) -> bool {
    false
}

#[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
fn desktop_entry_contents(
    comment: &str,
    icon_path: &Path,
    command: &Path,
    arguments: &str,
    categories: &str,
    keywords: &str,
    name: &str,
) -> String {
    let mut contents = format!("[Desktop Entry]\nType=Application\nVersion=1.0\nName={name}\n");
    if !comment.is_empty() {
        contents.push_str(&format!("Comment={comment}\n"));
    }
    if icon_path.is_file() {
        contents.push_str(&format!("Icon={}\n", icon_path.display()));
    }
    contents.push_str(&format!("TryExec={}\n", command.display()));
    contents.push_str(&format!("Exec={} {arguments}\n", command.display()));
    if !categories.is_empty() {
        contents.push_str(&format!("Categories={categories}\n"));
    }
    if !keywords.is_empty() {
        contents.push_str(&format!("Keywords={keywords}\n"));
    }
    contents
}

#[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
static APPIMAGE_SHORTCUT_ALREADY_WARNED: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_metadata_removes_the_complete_game_list_cache() {
        let directory = tempfile::tempdir().unwrap();
        let cache = directory.path().join("game_list");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("0100F2C0115B6000.pv.txt"), b"Update (1.0.0)").unwrap();

        assert_eq!(remove_metadata_cache(&cache).unwrap(), true);
        assert!(!cache.exists());
        assert_eq!(remove_metadata_cache(&cache).unwrap(), false);
    }

    #[test]
    fn shortcut_title_removes_edens_illegal_characters() {
        assert_eq!(sanitize_shortcut_name("A<B>:C\"/D\\E|F?G*H.I"), "ABCDEFGHI");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_folder_launcher_uses_explorer_with_a_single_native_path_argument() {
        use std::ffi::OsStr;

        let path = Path::new(r"C:\Users\Ruzu User\AppData\Roaming\ruzu");
        let command = windows_open_folder_command(path);

        assert_eq!(command.get_program(), OsStr::new("explorer.exe"));
        assert_eq!(command.get_args().collect::<Vec<_>>(), [path.as_os_str()]);
    }

    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
    #[test]
    fn desktop_entry_matches_edens_field_order_and_optional_fields() {
        let directory = tempfile::tempdir().unwrap();
        let icon = directory.path().join("icon.png");
        std::fs::write(&icon, b"icon").unwrap();
        let entry = desktop_entry_contents(
            "Start Game with the Ruzu Emulator",
            &icon,
            Path::new("/opt/ruzu"),
            "-f -g \"/games/Game.nsp\"",
            "Game;Emulator;Qt;",
            "Switch;Nintendo;",
            "Game",
        );
        assert_eq!(
            entry,
            format!(
                "[Desktop Entry]\nType=Application\nVersion=1.0\nName=Game\nComment=Start Game with the Ruzu Emulator\nIcon={}\nTryExec=/opt/ruzu\nExec=/opt/ruzu -f -g \"/games/Game.nsp\"\nCategories=Game;Emulator;Qt;\nKeywords=Switch;Nintendo;\n",
                icon.display()
            )
        );
    }
}
