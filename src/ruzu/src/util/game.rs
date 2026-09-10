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
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    // SaveDataFactory produces slash-separated VFS paths. Rust accepts those
    // on Windows, but Explorer needs native separators (Qt's file-URL adapter
    // performs this conversion upstream). Preserve the original UTF-16 path.
    let native_path: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .map(|unit| {
            if unit == b'/' as u16 {
                b'\\' as u16
            } else {
                unit
            }
        })
        .collect();
    let mut command = std::process::Command::new("explorer.exe");
    command.arg(std::ffi::OsString::from_wide(&native_path));
    command
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn open_folder(path: &Path) -> std::io::Result<()> {
    let directory = gtk::gio::File::for_path(path);
    gtk::gio::AppInfo::launch_default_for_uri(&directory.uri(), gtk::gio::AppLaunchContext::NONE)
        .map_err(|error| std::io::Error::other(error.to_string()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    target_os = "android",
    allow(unused_variables)
)]
pub fn get_shortcut_path(target: ShortcutTarget) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        return match target {
            ShortcutTarget::Desktop => gtk::glib::user_special_dir(gtk::glib::UserDirectory::Desktop),
            ShortcutTarget::Applications => {
                let path = gtk::glib::home_dir().join("Applications");
                if let Err(error) = std::fs::create_dir_all(&path) {
                    log::error!("Cannot create user Applications folder: {error}");
                    return None;
                }
                Some(path)
            }
        };
    }
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
        use std::os::windows::ffi::OsStringExt;
        use windows_sys::Win32::System::Com::CoTaskMemFree;
        use windows_sys::Win32::UI::Shell::{
            FOLDERID_Desktop, FOLDERID_Programs, SHGetKnownFolderPath,
        };

        // QStandardPaths queries the Windows shell in Eden. Do not reconstruct
        // these paths: OneDrive and administrators can redirect either folder.
        let folder = match target {
            ShortcutTarget::Desktop => &FOLDERID_Desktop,
            ShortcutTarget::Applications => &FOLDERID_Programs,
        };
        let mut path = std::ptr::null_mut();
        let result = unsafe { SHGetKnownFolderPath(folder, 0, std::ptr::null_mut(), &mut path) };
        let resolved = if result >= 0 && !path.is_null() {
            // The successful shell result is a NUL-terminated, allocated UTF-16 string.
            let mut length = 0;
            unsafe {
                while *path.add(length) != 0 {
                    length += 1;
                }
                Some(PathBuf::from(std::ffi::OsString::from_wide(
                    std::slice::from_raw_parts(path, length),
                )))
            }
        } else {
            log::error!("Cannot resolve shortcut folder: HRESULT {result:#010x}");
            None
        };
        unsafe { CoTaskMemFree(path.cast()) };
        return resolved;
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
    let extension = if cfg!(target_os = "windows") {
        "ico"
    } else {
        "png"
    };
    let name = if program_id == 0 {
        format!("ruzu-{game_title}.{extension}")
    } else {
        format!("ruzu-{program_id:016X}.{extension}")
    };
    Ok(directory.join(name))
}

#[cfg(not(target_os = "windows"))]
fn save_icon_to_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let texture = gtk::gdk::Texture::from_bytes(&gtk::glib::Bytes::from(bytes))
        .map_err(|error| error.to_string())?;
    texture.save_to_png(path).map_err(|error| error.to_string())
}

#[cfg(target_os = "windows")]
fn save_icon_to_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use gtk::gdk_pixbuf::prelude::PixbufLoaderExt;

    // Eden SaveIconToFile: seven uncompressed RGB32 DIB images in an ICO.
    // Serialize fields explicitly in little endian rather than copying packed
    // C++ structs. GdkPixbuf replaces QImage's decoding and smooth scaling.
    const SCALE_SIZES: [u32; 7] = [256, 128, 64, 48, 32, 24, 16];
    const BYTES_PER_PIXEL: u32 = 4;
    let loader = gtk::gdk_pixbuf::PixbufLoader::new();
    loader.write(bytes).map_err(|error| error.to_string())?;
    loader.close().map_err(|error| error.to_string())?;
    let source = loader.pixbuf().ok_or("Cannot decode shortcut icon")?;
    // QImage::Format_RGB32 discards alpha before scaling, not afterwards.
    let source = if source.has_alpha() {
        let mut pixels = source.read_pixel_bytes().to_vec();
        for y in 0..source.height() as usize {
            for x in 0..source.width() as usize {
                pixels[y * source.rowstride() as usize + x * 4 + 3] = 255;
            }
        }
        gtk::gdk_pixbuf::Pixbuf::from_bytes(
            &gtk::glib::Bytes::from_owned(pixels),
            gtk::gdk_pixbuf::Colorspace::Rgb,
            true,
            8,
            source.width(),
            source.height(),
            source.rowstride(),
        )
    } else {
        source
    };
    let mut icon = Vec::new();
    icon.extend_from_slice(&0u16.to_le_bytes());
    icon.extend_from_slice(&1u16.to_le_bytes());
    icon.extend_from_slice(&(SCALE_SIZES.len() as u16).to_le_bytes());
    let mut image_offset = 6 + 16 * SCALE_SIZES.len() as u32;
    for size in SCALE_SIZES {
        let image_size = 40 + size * size * BYTES_PER_PIXEL;
        icon.extend_from_slice(&[size as u8, size as u8, 0, 0]);
        icon.extend_from_slice(&1u16.to_le_bytes());
        icon.extend_from_slice(&32u16.to_le_bytes());
        icon.extend_from_slice(&image_size.to_le_bytes());
        icon.extend_from_slice(&image_offset.to_le_bytes());
        image_offset += image_size;
    }
    for size in SCALE_SIZES {
        let scaled = source
            .scale_simple(
                size as i32,
                size as i32,
                gtk::gdk_pixbuf::InterpType::Bilinear,
            )
            .ok_or("Cannot scale shortcut icon")?;
        // BITMAPINFOHEADER: double height includes the implicit ICO mask;
        // like Eden, opaque RGB32 pixels are written without a separate mask.
        icon.extend_from_slice(&40u32.to_le_bytes());
        icon.extend_from_slice(&size.to_le_bytes());
        icon.extend_from_slice(&(size * 2).to_le_bytes());
        icon.extend_from_slice(&1u16.to_le_bytes());
        icon.extend_from_slice(&32u16.to_le_bytes());
        icon.extend_from_slice(&[0u8; 24]);
        let pixels = scaled.read_pixel_bytes();
        let stride = scaled.rowstride() as usize;
        let channels = scaled.n_channels() as usize;
        for y in (0..size as usize).rev() {
            for x in 0..size as usize {
                let offset = y * stride + x * channels;
                icon.extend_from_slice(&[
                    pixels[offset + 2],
                    pixels[offset + 1],
                    pixels[offset],
                    255,
                ]);
            }
        }
    }
    std::fs::write(path, icon).map_err(|error| error.to_string())
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
    // Unlike upstream's write-only path, desktop launchers require execution
    // permission. Add only owner execution, preserving the user's other modes.
    use std::os::unix::fs::PermissionsExt;
    let executable = std::fs::metadata(&path).and_then(|metadata| {
        let mut permissions = metadata.permissions();
        permissions.set_mode(permissions.mode() | 0o100);
        std::fs::set_permissions(&path, permissions)
    });
    if let Err(error) = executable {
        log::error!("Failed to make shortcut executable {}: {error}", path.display());
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
    use std::os::windows::process::CommandExt;
    // Windows PowerShell -Command parses trailing arguments as script text,
    // not as $args. Keep paths and game arguments out of the script entirely.
    // WScript.Shell supplies the same ShellLink COM object Eden uses directly.
    let script = "$ErrorActionPreference='Stop';$s=(New-Object -ComObject WScript.Shell).CreateShortcut($env:RUZU_SHORTCUT_PATH);$s.TargetPath=$env:RUZU_SHORTCUT_COMMAND;$s.Arguments=$env:RUZU_SHORTCUT_ARGUMENTS;$s.Description=$env:RUZU_SHORTCUT_COMMENT;if($env:RUZU_SHORTCUT_ICON -and (Test-Path -LiteralPath $env:RUZU_SHORTCUT_ICON -PathType Leaf)){$s.IconLocation=$env:RUZU_SHORTCUT_ICON};$s.Save()";
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("RUZU_SHORTCUT_PATH", &shortcut)
        .env("RUZU_SHORTCUT_COMMAND", command)
        .env("RUZU_SHORTCUT_ARGUMENTS", arguments)
        .env("RUZU_SHORTCUT_COMMENT", comment)
        .env("RUZU_SHORTCUT_ICON", icon_path)
        .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
        .output();
    match output {
        Ok(output) if output.status.success() && shortcut.is_file() => true,
        Ok(output) => {
            log::error!(
                "Failed to create shortcut {} ({}): {}",
                shortcut.display(),
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
            false
        }
        Err(error) => {
            log::error!("Failed to create shortcut {}: {error}", shortcut.display());
            false
        }
    }
}

// macOS extension: Eden returns false here. A Finder alias cannot retain argv,
// so keep a small application bundle with a quoted launcher and embedded icon.
#[cfg(target_os = "macos")]
fn create_shortcut_link(
    shortcut_path: &Path,
    _comment: &str,
    icon_path: &Path,
    command: &Path,
    arguments: &str,
    _categories: &str,
    _keywords: &str,
    name: &str,
) -> bool {
    match create_macos_shortcut(shortcut_path, icon_path, command, arguments, name) {
        Ok(()) => true,
        Err(error) => {
            log::error!("Cannot create macOS shortcut {name}: {error}");
            false
        }
    }
}

#[cfg(target_os = "macos")]
fn create_macos_shortcut(
    directory: &Path,
    icon: &Path,
    command: &Path,
    arguments: &str,
    name: &str,
) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    if name.is_empty() || name == "." || name == ".." || name.contains(['/', ':']) {
        return Err(std::io::Error::other("Invalid shortcut name"));
    }
    let destination = directory.join(format!("{name}.app"));
    if destination.symlink_metadata().is_ok() {
        return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "Shortcut already exists"));
    }
    // Resolve before writing: a missing target must not leave a broken shortcut.
    let command = command.canonicalize()?;
    let argv = gtk::glib::shell_parse_argv(arguments)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let staging = tempfile::Builder::new().prefix(".ruzu-shortcut-").tempdir_in(directory)?;
    let bundle = staging.path().join("Launcher.app");
    let contents = bundle.join("Contents");
    std::fs::create_dir_all(contents.join("MacOS"))?;
    std::fs::create_dir_all(contents.join("Resources"))?;

    // Never evaluate the original argument string. Parse it and quote each argv
    // element separately so $, backticks, quotes and newlines stay literal.
    let mut script = format!("#!/bin/sh\nexec {}", gtk::glib::shell_quote(&command).to_string_lossy());
    for argument in argv {
        script.push(' ');
        script.push_str(&gtk::glib::shell_quote(argument).to_string_lossy());
    }
    script.push('\n');
    let executable = contents.join("MacOS/launch");
    std::fs::write(&executable, script)?;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))?;

    let icon_entry = if icon.is_file() {
        let iconset = staging.path().join("game.iconset");
        std::fs::create_dir(&iconset)?;
        for (size, filename) in [(256, "icon_256x256.png"), (512, "icon_256x256@2x.png")] {
            let output = Command::new("/usr/bin/sips").arg("-z")
                .args([size.to_string(), size.to_string()]).arg(icon)
                .arg("--out").arg(iconset.join(filename)).output()?;
            if !output.status.success() {
                return Err(std::io::Error::other(String::from_utf8_lossy(&output.stderr).into_owned()));
            }
        }
        let output = Command::new("/usr/bin/iconutil").args(["-c", "icns"])
            .arg(&iconset).arg("-o").arg(contents.join("Resources/game.icns")).output()?;
        if !output.status.success() {
            return Err(std::io::Error::other(String::from_utf8_lossy(&output.stderr).into_owned()));
        }
        "<key>CFBundleIconFile</key><string>game.icns</string>"
    } else {
        ""
    };
    let escaped_name = gtk::glib::markup_escape_text(name);
    std::fs::write(contents.join("Info.plist"), format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\"><dict>\
         <key>CFBundleName</key><string>{escaped_name}</string>\
         <key>CFBundleDisplayName</key><string>{escaped_name}</string>\
         <key>CFBundleExecutable</key><string>launch</string>\
         <key>CFBundlePackageType</key><string>APPL</string>\
         <key>CFBundleVersion</key><string>1</string>\
         {icon_entry}</dict></plist>\n"
    ))?;
    // Publish only after the complete bundle exists. Never delete an existing app.
    std::fs::rename(bundle, destination)
}

#[cfg(not(any(
    target_os = "windows",
    target_os = "macos",
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

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_shortcut_preserves_argv_and_launches_through_finder() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;
        let directory = tempfile::tempdir().unwrap();
        let command = directory.path().join("fake ruzu '$`.sh");
        let log = directory.path().join("argv");
        std::fs::write(&command, format!(
            "#!/bin/sh\nprintf '%s\\0' \"$@\" > {}\n",
            gtk::glib::shell_quote(&log).to_string_lossy()
        )).unwrap();
        std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o755)).unwrap();
        let game = "/games/L'été & $(touch injected) `test` \"quoted\"\nnew line.nsp";
        let arguments = format!("-f -g {}", gtk::glib::shell_quote(game).to_string_lossy());
        create_macos_shortcut(directory.path(), Path::new(""), &command, &arguments, "Game & Friends").unwrap();
        let app = directory.path().join("Game & Friends.app");
        let expected = format!("-f\0-g\0{game}\0").into_bytes();
        assert!(Command::new(app.join("Contents/MacOS/launch")).status().unwrap().success());
        assert_eq!(std::fs::read(&log).unwrap(), expected);
        std::fs::remove_file(&log).unwrap();
        // Exercise LaunchServices too, without starting Ruzu or opening a game.
        let mut child = Command::new("/usr/bin/open").args(["-n", "-W"]).arg(&app).spawn().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("LaunchServices did not finish the test launcher");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(std::fs::read(&log).unwrap(), expected);
        assert!(Command::new("/usr/bin/plutil").arg("-lint").arg(app.join("Contents/Info.plist"))
            .status().unwrap().success());
        assert_eq!(create_macos_shortcut(directory.path(), Path::new(""), &command, "-qlaunch", "Game & Friends")
            .unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
        assert!(app.join("Contents/MacOS/launch").is_file());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_shortcut_embeds_icon_and_cleans_up_failures() {
        let directory = tempfile::tempdir().unwrap();
        let command = Path::new("/usr/bin/true");
        let icon = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ruzu-rusty-lemon.png");
        create_macos_shortcut(directory.path(), &icon, command, "-qlaunch", "Icon test").unwrap();
        let bytes = std::fs::read(directory.path().join("Icon test.app/Contents/Resources/game.icns")).unwrap();
        assert_eq!(&bytes[..4], b"icns");
        assert!(create_macos_shortcut(directory.path(), &icon, command, "'unterminated", "Broken").is_err());
        assert!(create_macos_shortcut(directory.path(), &icon, command, "-qlaunch", "../escape").is_err());
        assert!(create_macos_shortcut(directory.path(), &icon, Path::new("/nonexistent/ruzu"), "-qlaunch", "Missing").is_err());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

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
    fn windows_shortcut_icon_matches_edens_ico_layout_and_loads_in_windows() {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            DestroyIcon, LoadImageW, IMAGE_ICON, LR_LOADFROMFILE,
        };
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("game.ico");
        // Red top row, blue bottom row: tests DIB row inversion and BGR order.
        let pixels = gtk::glib::Bytes::from_static(&[
            255, 0, 0, 0, 255, 0, 0, 0, 0, 0, 255, 255, 0, 0, 255, 255,
        ]);
        let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_bytes(
            &pixels,
            gtk::gdk_pixbuf::Colorspace::Rgb,
            true,
            8,
            2,
            2,
            8,
        );
        save_icon_to_file(&path, &pixbuf.save_to_bufferv("png", &[]).unwrap()).unwrap();
        let ico = std::fs::read(&path).unwrap();
        assert_eq!(&ico[..6], &[0, 0, 1, 0, 7, 0]);
        let read_u32 =
            |offset| u32::from_le_bytes(ico[offset..offset + 4].try_into().unwrap()) as usize;
        let mut expected_offset = 6 + 7 * 16;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        for (index, size) in [256usize, 128, 64, 48, 32, 24, 16].into_iter().enumerate() {
            let entry = 6 + index * 16;
            assert_eq!(
                &ico[entry..entry + 8],
                &[size as u8, size as u8, 0, 0, 1, 0, 32, 0]
            );
            assert_eq!(read_u32(entry + 8), 40 + size * size * 4);
            assert_eq!(read_u32(entry + 12), expected_offset);
            assert_eq!(read_u32(expected_offset), 40);
            assert_eq!(read_u32(expected_offset + 4), size);
            assert_eq!(read_u32(expected_offset + 8), size * 2);
            assert_eq!(
                &ico[expected_offset + 12..expected_offset + 16],
                &[1, 0, 32, 0]
            );
            assert_eq!(&ico[expected_offset + 16..expected_offset + 40], &[0; 24]);
            assert_eq!(
                &ico[expected_offset + 40..expected_offset + 44],
                &[255, 0, 0, 255]
            );
            let top = expected_offset + 40 + (size - 1) * size * 4;
            assert_eq!(&ico[top..top + 4], &[0, 0, 255, 255]);
            let handle = unsafe {
                LoadImageW(
                    std::ptr::null_mut(),
                    wide.as_ptr(),
                    IMAGE_ICON,
                    size as i32,
                    size as i32,
                    LR_LOADFROMFILE,
                )
            };
            assert!(
                !handle.is_null(),
                "Windows failed to load {size}px icon: {}",
                std::io::Error::last_os_error()
            );
            unsafe {
                DestroyIcon(handle);
            }
            expected_offset += 40 + size * size * 4;
        }
        assert_eq!(ico.len(), expected_offset);
        assert!(save_icon_to_file(&directory.path().join("invalid.ico"), b"not an image").is_err());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_shortcut_folders_match_shell_locations() {
        use std::os::windows::process::CommandExt;
        for (target, special_folder) in [
            (ShortcutTarget::Desktop, "DesktopDirectory"),
            (ShortcutTarget::Applications, "Programs"),
        ] {
            let output = std::process::Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command",
                    "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8;[Environment]::GetFolderPath($env:RUZU_TEST_FOLDER)"])
                .env("RUZU_TEST_FOLDER", special_folder)
                .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
                .output().unwrap();
            assert!(output.status.success());
            let expected = String::from_utf8(output.stdout).unwrap();
            assert_eq!(
                get_shortcut_path(target).unwrap(),
                PathBuf::from(expected.trim_start_matches('\u{feff}').trim())
            );
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_shortcut_roundtrips_paths_and_arguments_without_script_interpretation() {
        use std::os::windows::process::CommandExt;
        let directory = tempfile::tempdir().unwrap();
        let folder = directory.path().join("Jeux été & amis [1] $test");
        std::fs::create_dir(&folder).unwrap();
        let command = folder.join("ruzu test.exe");
        std::fs::write(&command, b"test target; never executed").unwrap();
        let arguments = "-f -g \"C:\\Jeux\\L'été & $test [1].nsp\"";
        let comment = "Démarrer l'été & $test";
        assert!(create_shortcut_link(
            &folder,
            comment,
            Path::new(""),
            &command,
            arguments,
            "",
            "",
            "Jeu été"
        ));
        let shortcut = folder.join("Jeu été.lnk");
        let output = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command",
                "$ErrorActionPreference='Stop';[Console]::OutputEncoding=[System.Text.Encoding]::UTF8;$s=(New-Object -ComObject WScript.Shell).CreateShortcut($env:RUZU_TEST_LINK);@{target=$s.TargetPath;arguments=$s.Arguments;description=$s.Description}|ConvertTo-Json -Compress"])
            .env("RUZU_TEST_LINK", &shortcut)
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json = String::from_utf8(output.stdout).unwrap();
        let properties: serde_json::Value =
            serde_json::from_str(json.trim_start_matches('\u{feff}')).unwrap();
        assert_eq!(
            properties["target"].as_str().unwrap(),
            command.to_str().unwrap()
        );
        assert_eq!(properties["arguments"], arguments);
        assert_eq!(properties["description"], comment);
        assert!(!create_shortcut_link(
            &folder.join("missing"),
            comment,
            Path::new(""),
            &command,
            arguments,
            "",
            "",
            "Jeu"
        ));
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

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_folder_launcher_normalizes_vfs_separators() {
        use std::ffi::OsStr;

        for (input, expected) in [
            (
                r"C:\Users\René User\AppData\Roaming\ruzu\nand/user/save/account/0123/0100000000000001/0",
                r"C:\Users\René User\AppData\Roaming\ruzu\nand\user\save\account\0123\0100000000000001\0",
            ),
            (
                r"//server/share/Game Saves/user/save",
                r"\\server\share\Game Saves\user\save",
            ),
        ] {
            let command = windows_open_folder_command(Path::new(input));
            assert_eq!(
                command.get_args().collect::<Vec<_>>(),
                [OsStr::new(expected)]
            );
        }
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

    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
    #[test]
    fn desktop_shortcut_is_executable_on_creation_and_replacement() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Homebrew.desktop");
        let create = || {
            create_shortcut_link(
                directory.path(),
                "",
                Path::new(""),
                Path::new("/opt/ruzu"),
                "",
                "",
                "",
                "Homebrew",
            )
        };
        assert!(create());
        assert_ne!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o100, 0);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(create());
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(std::fs::read_to_string(&path).unwrap().contains("Name=Homebrew\n"));
    }
}
