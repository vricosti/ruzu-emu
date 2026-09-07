// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Rust counterpart of Eden `frontend_common/firmware_manager.{h,cpp}`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use common::fs::path_util::{get_ruzu_path, RuzuPath};
use ruzu_core::crypto::key_manager::KeyManager;

/// Upstream `FirmwareManager::KeyInstallResult`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyInstallResult {
    Success,
    InvalidDir,
    ErrorFailedCopy,
    ErrorWrongFilename,
    ErrorFailedInit,
    ErrorInvalidArchive,
}

/// Upstream `FirmwareManager::InstallKeys`.
pub fn install_keys(location: &Path, expected_extension: &str) -> KeyInstallResult {
    log::info!("Installing key files from {}", location.display());

    let result = copy_key_files_to(
        location,
        expected_extension,
        &get_ruzu_path(RuzuPath::KeysDir),
    );
    if result != KeyInstallResult::Success {
        return result;
    }

    KeyManager::instance().lock().unwrap().reload_keys();
    if crate::content_manager::are_keys_present() {
        KeyInstallResult::Success
    } else {
        KeyInstallResult::ErrorFailedInit
    }
}

fn copy_key_files_to(
    location: &Path,
    expected_extension: &str,
    keys_dir: &Path,
) -> KeyInstallResult {
    if location
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
    {
        return copy_key_archive_to(location, keys_dir);
    }
    if !location.to_string_lossy().ends_with(expected_extension) {
        return KeyInstallResult::ErrorWrongFilename;
    }

    let Some(source_dir) = location.parent() else {
        return KeyInstallResult::InvalidDir;
    };
    if !source_dir.is_dir() {
        return KeyInstallResult::InvalidDir;
    }

    let mut source_key_files = Vec::<PathBuf>::new();
    if location.is_file() {
        source_key_files.push(location.to_path_buf());
    }
    for optional in ["title.keys", "key_retail.bin"] {
        let candidate = source_dir.join(optional);
        if candidate.is_file() {
            source_key_files.push(candidate);
        }
    }
    if source_key_files.is_empty() || !location.is_file() {
        return KeyInstallResult::ErrorWrongFilename;
    }

    if let Err(error) = ensure_keys_directory(keys_dir) {
        log::error!(
            "Could not prepare keys directory {}: {error}",
            keys_dir.display()
        );
        return KeyInstallResult::ErrorFailedCopy;
    }

    for key_file in source_key_files {
        let Some(filename) = key_file.file_name() else {
            return KeyInstallResult::ErrorFailedCopy;
        };
        let destination = keys_dir.join(filename);
        // Rust's copy operation must not receive the same source and
        // destination. Keeping this no-op guard also preserves keys selected
        // directly from the installed directory.
        if same_file(&key_file, &destination) {
            continue;
        }
        if let Err(error) = fs::copy(&key_file, &destination) {
            log::error!(
                "Failed to copy file {} to {}: {error}",
                key_file.display(),
                destination.display()
            );
            return KeyInstallResult::ErrorFailedCopy;
        }
    }

    KeyInstallResult::Success
}

/// ZIP input is a requested frontend extension to Eden's InstallKeys. Read only
/// recognized files into bounded memory; never extract archive paths to disk.
fn copy_key_archive_to(location: &Path, keys_dir: &Path) -> KeyInstallResult {
    use std::io::Read;

    const MAX_KEY_FILE_SIZE: u64 = 16 * 1024 * 1024;
    let read_keys = || -> Result<Vec<(&'static str, Vec<u8>)>, Box<dyn std::error::Error>> {
        let mut archive = zip::ZipArchive::new(fs::File::open(location)?)?;
        let mut entries = Vec::new();
        for index in 0..archive.len() {
            let entry = archive.by_index(index)?;
            if entry.is_dir() {
                continue;
            }
            let name = entry.name().replace('\\', "/");
            // Validate using ZIP separators, independently of the host OS.
            if name.starts_with('/')
                || name.contains(':')
                || name.split('/').any(|part| part == "..")
            {
                return Err("Unsafe archive path".into());
            }
            if entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
            {
                return Err("Archive contains a symbolic link".into());
            }
            entries.push((index, name));
        }
        let candidates: Vec<_> = entries
            .iter()
            .filter(|(_, name)| name.rsplit('/').next() == Some("prod.keys"))
            .collect();
        if candidates.len() != 1 {
            return Err("Archive must contain exactly one prod.keys file".into());
        }
        let parent = candidates[0]
            .1
            .rsplit_once('/')
            .map_or("", |(parent, _)| parent);
        let mut files = Vec::new();
        for filename in ["prod.keys", "title.keys", "key_retail.bin"] {
            let expected = if parent.is_empty() {
                filename.to_owned()
            } else {
                format!("{parent}/{filename}")
            };
            let matches: Vec<_> = entries
                .iter()
                .filter(|(_, name)| *name == expected)
                .collect();
            if matches.len() > 1 {
                return Err("Duplicate key file".into());
            }
            if let Some((index, _)) = matches.first() {
                let entry = archive.by_index(*index)?;
                if entry.size() > MAX_KEY_FILE_SIZE {
                    return Err("Key file too large".into());
                }
                let mut bytes = Vec::new();
                entry.take(MAX_KEY_FILE_SIZE + 1).read_to_end(&mut bytes)?;
                if bytes.is_empty() || bytes.len() as u64 > MAX_KEY_FILE_SIZE {
                    return Err("Invalid key file size".into());
                }
                files.push((filename, bytes));
            }
        }
        Ok(files)
    };
    let files = match read_keys() {
        Ok(files) => files,
        Err(error) => {
            log::error!("Could not read keys archive: {error}");
            return KeyInstallResult::ErrorInvalidArchive;
        }
    };
    if ensure_keys_directory(keys_dir).is_err() {
        return KeyInstallResult::ErrorFailedCopy;
    }
    for (filename, bytes) in files {
        if fs::write(keys_dir.join(filename), bytes).is_err() {
            return KeyInstallResult::ErrorFailedCopy;
        }
    }
    KeyInstallResult::Success
}

/// Ruzu's Share migration can leave a dangling directory link when its source
/// emulator is moved. Preserve valid links; replace only a link whose target no
/// longer exists. Ordinary files and non-directory targets remain errors.
fn ensure_keys_directory(keys_dir: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(keys_dir) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };

    let Some(metadata) = metadata else {
        return fs::create_dir_all(keys_dir);
    };

    if is_directory_link(&metadata) {
        return match fs::metadata(keys_dir) {
            Ok(target) if target.is_dir() => Ok(()),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!(
                    "keys link target is not a directory: {}",
                    keys_dir.display()
                ),
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                remove_directory_link(keys_dir, &metadata)?;
                fs::create_dir_all(keys_dir)
            }
            Err(error) => Err(error),
        };
    }

    if metadata.is_dir() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("keys path is not a directory: {}", keys_dir.display()),
        ))
    }
}

#[cfg(unix)]
fn is_directory_link(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn is_directory_link(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(any(unix, windows)))]
fn is_directory_link(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn remove_directory_link(path: &Path, _metadata: &fs::Metadata) -> io::Result<()> {
    fs::remove_file(path)
}

#[cfg(windows)]
fn remove_directory_link(path: &Path, metadata: &fs::Metadata) -> io::Result<()> {
    if metadata.is_dir() {
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    }
}

#[cfg(not(any(unix, windows)))]
fn remove_directory_link(path: &Path, _metadata: &fs::Metadata) -> io::Result<()> {
    fs::remove_file(path)
}

fn same_file(first: &Path, second: &Path) -> bool {
    match (fs::canonicalize(first), fs::canonicalize(second)) {
        (Ok(first), Ok(second)) => first == second,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive(path: &Path, entries: &[(&str, &[u8])]) {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(fs::File::create(path).unwrap());
        for (name, bytes) in entries {
            writer
                .start_file(
                    *name,
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Stored),
                )
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap();
    }

    #[test]
    fn installs_zip_keys_at_any_depth_and_only_adjacent_optional_files() {
        for prefix in ["", "backup/keys/"] {
            let root = tempfile::tempdir().unwrap();
            let input = root.path().join("keys.ZIP");
            let dest = root.path().join("installed");
            fs::create_dir(&dest).unwrap();
            fs::write(dest.join("prod.keys"), b"old").unwrap();
            archive(
                &input,
                &[
                    (&format!("{prefix}prod.keys"), b"synthetic prod"),
                    (&format!("{prefix}title.keys"), b"synthetic title"),
                    (&format!("{prefix}key_retail.bin"), b"synthetic retail"),
                    ("other/title.keys", b"unrelated"),
                    ("readme.txt", b"ignored"),
                ],
            );
            assert_eq!(
                copy_key_files_to(&input, "keys", &dest),
                KeyInstallResult::Success
            );
            assert_eq!(fs::read(dest.join("prod.keys")).unwrap(), b"synthetic prod");
            assert_eq!(
                fs::read(dest.join("title.keys")).unwrap(),
                b"synthetic title"
            );
            assert_eq!(
                fs::read(dest.join("key_retail.bin")).unwrap(),
                b"synthetic retail"
            );
            assert_eq!(fs::read_dir(&dest).unwrap().count(), 3);
        }
    }

    #[test]
    fn rejects_crc_failure_before_replacing_existing_keys() {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("keys.zip");
        let dest = root.path().join("installed");
        fs::create_dir(&dest).unwrap();
        fs::write(dest.join("prod.keys"), b"original").unwrap();
        archive(&input, &[("prod.keys", b"synthetic")]);
        let offset = {
            let mut zip = zip::ZipArchive::new(fs::File::open(&input).unwrap()).unwrap();
            let offset = zip.by_index(0).unwrap().data_start() as usize;
            offset
        };
        let mut bytes = fs::read(&input).unwrap();
        bytes[offset] ^= 1;
        fs::write(&input, bytes).unwrap();
        assert_eq!(
            copy_key_files_to(&input, "keys", &dest),
            KeyInstallResult::ErrorInvalidArchive
        );
        assert_eq!(fs::read(dest.join("prod.keys")).unwrap(), b"original");
    }

    #[test]
    fn rejects_bad_archives_before_touching_installed_keys() {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("keys.zip");
        let dest = root.path().join("installed");
        fs::create_dir(&dest).unwrap();
        fs::write(dest.join("prod.keys"), b"original").unwrap();
        let cases: Vec<Vec<(&str, &[u8])>> = vec![
            vec![("title.keys", b"missing prod")],
            vec![("a/prod.keys", b"one"), ("b/prod.keys", b"two")],
            vec![("../prod.keys", b"unsafe")],
            vec![("..\\prod.keys", b"unsafe")],
            vec![("/prod.keys", b"unsafe")],
            vec![("prod.keys", b"")],
        ];
        for entries in cases {
            archive(&input, &entries);
            assert_eq!(
                copy_key_files_to(&input, "keys", &dest),
                KeyInstallResult::ErrorInvalidArchive
            );
            assert_eq!(fs::read(dest.join("prod.keys")).unwrap(), b"original");
        }
        fs::write(&input, b"not a ZIP").unwrap();
        assert_eq!(
            copy_key_files_to(&input, "keys", &dest),
            KeyInstallResult::ErrorInvalidArchive
        );
        assert_eq!(fs::read(dest.join("prod.keys")).unwrap(), b"original");
    }

    #[test]
    fn copies_the_selected_and_adjacent_key_files_with_overwrite() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::write(source.join("prod.keys"), b"new prod").unwrap();
        fs::write(source.join("title.keys"), b"new title").unwrap();
        fs::write(source.join("key_retail.bin"), b"new retail").unwrap();
        fs::write(destination.join("prod.keys"), b"old prod").unwrap();

        assert_eq!(
            copy_key_files_to(&source.join("prod.keys"), "keys", &destination),
            KeyInstallResult::Success
        );
        assert_eq!(
            fs::read(destination.join("prod.keys")).unwrap(),
            b"new prod"
        );
        assert_eq!(
            fs::read(destination.join("title.keys")).unwrap(),
            b"new title"
        );
        assert_eq!(
            fs::read(destination.join("key_retail.bin")).unwrap(),
            b"new retail"
        );
    }

    #[cfg(unix)]
    #[test]
    fn replaces_a_broken_share_link_with_a_real_keys_directory() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("keys");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("prod.keys"), b"prod").unwrap();
        std::os::unix::fs::symlink(root.path().join("missing-source"), &destination).unwrap();

        assert_eq!(
            copy_key_files_to(&source.join("prod.keys"), "keys", &destination),
            KeyInstallResult::Success
        );
        assert!(fs::symlink_metadata(&destination)
            .unwrap()
            .file_type()
            .is_dir());
        assert_eq!(fs::read(destination.join("prod.keys")).unwrap(), b"prod");
    }

    #[cfg(unix)]
    #[test]
    fn preserves_a_valid_share_link_and_writes_through_it() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let shared = root.path().join("shared");
        let destination = root.path().join("keys");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&shared).unwrap();
        fs::write(source.join("prod.keys"), b"prod").unwrap();
        std::os::unix::fs::symlink(&shared, &destination).unwrap();

        assert_eq!(
            copy_key_files_to(&source.join("prod.keys"), "keys", &destination),
            KeyInstallResult::Success
        );
        assert!(fs::symlink_metadata(&destination)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read(shared.join("prod.keys")).unwrap(), b"prod");
    }
}
