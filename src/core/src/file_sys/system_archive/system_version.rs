// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Ported from: core/file_sys/system_archive/system_version.h / .cpp
// Status: COMPLETE
//
// Synthesizes the SystemVersion system archive containing firmware
// version information matching upstream HLE::ApiVersion constants.

use std::sync::Arc;

use crate::file_sys::vfs::vfs_types::{VirtualDir, VirtualFile};
use crate::file_sys::vfs::vfs_vector::{VectorVfsDirectory, VectorVfsFile};
use crate::hle::api_version;

/// Get the long display version string.
///
/// Corresponds to upstream `GetLongDisplayVersion`.
pub fn get_long_display_version() -> String {
    api_version::DISPLAY_TITLE.to_string()
}

/// Synthesize the SystemVersion archive.
///
/// Creates a 0x100-byte file with firmware version fields at fixed offsets,
/// matching the upstream layout exactly:
/// - offset 0x00: HOS_VERSION_MAJOR (u8)
/// - offset 0x01: HOS_VERSION_MINOR (u8)
/// - offset 0x02: HOS_VERSION_MICRO (u8)
/// - offset 0x04: SDK_REVISION_MAJOR (u8)
/// - offset 0x05: SDK_REVISION_MINOR (u8)
/// - offset 0x08..0x28: PLATFORM_STRING (max 0x20 bytes)
/// - offset 0x28..0x68: VERSION_HASH (max 0x40 bytes)
/// - offset 0x68..0x80: DISPLAY_VERSION (max 0x18 bytes)
/// - offset 0x80..0x100: DISPLAY_TITLE (max 0x80 bytes)
pub fn system_version() -> Option<VirtualDir> {
    log::warn!(
        "SystemVersion: Using hardcoded firmware version '{}'",
        get_long_display_version()
    );

    let mut data = vec![0u8; 0x100];

    // Write version fields.
    data[0] = api_version::HOS_VERSION_MAJOR;
    data[1] = api_version::HOS_VERSION_MINOR;
    data[2] = api_version::HOS_VERSION_MICRO;
    // offset 3 is padding
    data[4] = api_version::SDK_REVISION_MAJOR;
    data[5] = api_version::SDK_REVISION_MINOR;
    // offsets 6-7 are padding

    // Write PLATFORM_STRING at offset 0x08 (max 0x20 bytes).
    let platform_bytes = api_version::PLATFORM_STRING.as_bytes();
    let platform_len = platform_bytes.len().min(0x20);
    data[0x08..0x08 + platform_len].copy_from_slice(&platform_bytes[..platform_len]);

    // Write VERSION_HASH at offset 0x28 (max 0x40 bytes).
    let hash_bytes = api_version::VERSION_HASH.as_bytes();
    let hash_len = hash_bytes.len().min(0x40);
    data[0x28..0x28 + hash_len].copy_from_slice(&hash_bytes[..hash_len]);

    // Write DISPLAY_VERSION at offset 0x68 (max 0x18 bytes).
    let display_ver_bytes = api_version::DISPLAY_VERSION.as_bytes();
    let display_ver_len = display_ver_bytes.len().min(0x18);
    data[0x68..0x68 + display_ver_len].copy_from_slice(&display_ver_bytes[..display_ver_len]);

    // Write DISPLAY_TITLE at offset 0x80 (max 0x80 bytes).
    let display_title_bytes = api_version::DISPLAY_TITLE.as_bytes();
    let display_title_len = display_title_bytes.len().min(0x80);
    data[0x80..0x80 + display_title_len].copy_from_slice(&display_title_bytes[..display_title_len]);

    let file: VirtualFile = Arc::new(VectorVfsFile::new(data, "file".to_string(), None));

    Some(Arc::new(VectorVfsDirectory::new(
        vec![file],
        vec![],
        "data".to_string(),
        None,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_long_display_version() {
        let ver = get_long_display_version();
        assert!(ver.contains("NintendoSDK"));
        assert!(ver.contains("12.1.0"));
    }

    #[test]
    fn test_system_version_archive_structure() {
        let dir = system_version().expect("system_version should return Some");
        assert_eq!(dir.get_name(), "data");
        assert_eq!(dir.get_files().len(), 1);
    }

    #[test]
    fn test_system_version_file_contents() {
        let dir = system_version().unwrap();
        let files = dir.get_files();
        let file = &files[0];
        assert_eq!(file.get_name(), "file");
        assert_eq!(file.get_size(), 0x100);

        let data = file.read_all_bytes();
        // Check version fields.
        assert_eq!(data[0], 12); // HOS_VERSION_MAJOR
        assert_eq!(data[1], 1); // HOS_VERSION_MINOR
        assert_eq!(data[2], 0); // HOS_VERSION_MICRO
        assert_eq!(data[4], 1); // SDK_REVISION_MAJOR
        assert_eq!(data[5], 0); // SDK_REVISION_MINOR

        // Check PLATFORM_STRING starts at 0x08.
        assert_eq!(data[0x08], b'N');
        assert_eq!(data[0x09], b'X');
    }
}
