// SPDX-License-Identifier: GPL-3.0-or-later
//
// Ruzu distribution support for freely redistributable homebrew applications.
// This has no Eden counterpart: Eden does not ship games with the emulator.
// Keep path discovery here so the game list only owns presentation and scans.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy)]
#[allow(dead_code)] // All layouts are exercised together by the cross-platform path-contract tests.
enum PackageLayout {
    Windows,
    MacOs,
    Unix,
}

/// Return the read-only free-game directory installed with the executable.
///
/// The directory is deliberately not stored in `qt-config.ini`: an installed
/// package may be moved, upgraded, or mounted at a different prefix between
/// runs, so its location must always be resolved from the running executable.
pub fn packaged_directory() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let directory = directory_for_executable(&executable, current_layout())?;
    directory.is_dir().then_some(directory)
}

/// Whether `game` belongs to the immutable payload installed with Ruzu.
/// Bundled games must use the normal user SDMC for saves instead of treating
/// their package directory as the writable homebrew layer.
pub fn contains(game: &Path) -> bool {
    let Some(directory) = packaged_directory() else {
        return false;
    };
    canonical_path_is_within(game, &directory)
}

fn canonical_path_is_within(path: &Path, directory: &Path) -> bool {
    let (Ok(path), Ok(directory)) = (path.canonicalize(), directory.canonicalize()) else {
        return false;
    };
    path.starts_with(directory)
}

#[cfg(target_os = "windows")]
fn current_layout() -> PackageLayout {
    PackageLayout::Windows
}

#[cfg(target_os = "macos")]
fn current_layout() -> PackageLayout {
    PackageLayout::MacOs
}

#[cfg(all(unix, not(target_os = "macos")))]
fn current_layout() -> PackageLayout {
    PackageLayout::Unix
}

#[cfg(not(any(target_os = "windows", unix)))]
fn current_layout() -> PackageLayout {
    PackageLayout::Unix
}

fn directory_for_executable(executable: &Path, layout: PackageLayout) -> Option<PathBuf> {
    let executable_directory = executable.parent()?;
    match layout {
        PackageLayout::Windows => Some(executable_directory.join("share/ruzu/freegames")),
        PackageLayout::MacOs => Some(executable_directory.parent()?.join("Resources/freegames")),
        PackageLayout::Unix => Some(executable_directory.parent()?.join("share/ruzu/freegames")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_package_path_is_relative_to_the_executable() {
        let executable = Path::new(r"C:\Users\test\AppData\Local\Programs\Ruzu\ruzu.exe");
        assert_eq!(
            directory_for_executable(executable, PackageLayout::Windows).unwrap(),
            PathBuf::from(r"C:\Users\test\AppData\Local\Programs\Ruzu\share\ruzu\freegames")
        );
    }

    #[test]
    fn macos_package_path_is_inside_the_app_resources() {
        let executable = Path::new("/Applications/Ruzu.app/Contents/MacOS/ruzu");
        assert_eq!(
            directory_for_executable(executable, PackageLayout::MacOs).unwrap(),
            PathBuf::from("/Applications/Ruzu.app/Contents/Resources/freegames")
        );
    }

    #[test]
    fn unix_package_path_uses_the_install_prefix_share_directory() {
        let executable = Path::new("/opt/ruzu/bin/ruzu");
        assert_eq!(
            directory_for_executable(executable, PackageLayout::Unix).unwrap(),
            PathBuf::from("/opt/ruzu/share/ruzu/freegames")
        );
    }

    #[test]
    fn containment_requires_existing_canonical_paths() {
        let temporary = tempfile::tempdir().unwrap();
        let free_games = temporary.path().join("freegames");
        let freebrick = free_games.join("freebrick");
        std::fs::create_dir_all(&freebrick).unwrap();
        let nro = freebrick.join("freebrick.nro");
        let outside = temporary.path().join("outside.nro");
        std::fs::write(&nro, []).unwrap();
        std::fs::write(&outside, []).unwrap();

        assert!(canonical_path_is_within(&nro, &free_games));
        assert!(!canonical_path_is_within(&outside, &free_games));
        assert!(!canonical_path_is_within(
            &freebrick.join("missing.nro"),
            &free_games
        ));
    }
}
