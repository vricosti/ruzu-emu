// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

// SPDX-FileCopyrightText: Copyright 2021 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of eden/src/core/hle/api_version.h
//!
//! Pure constants for HOS version, SDK revision, and Atmosphere version.

// Horizon OS version constants.

pub const HOS_VERSION_MAJOR: u8 = 23;
pub const HOS_VERSION_MINOR: u8 = 0;
pub const HOS_VERSION_MICRO: u8 = 0;

// NintendoSDK version constants.

pub const SDK_REVISION_MAJOR: u8 = 1;
pub const SDK_REVISION_MINOR: u8 = 0;

pub const PLATFORM_STRING: &str = "NX";
pub const VERSION_HASH: &str = "c2663accb7490a6ccfe9bc4dfd120ca215490525";
pub const DISPLAY_VERSION: &str = "23.0.0";
pub const DISPLAY_TITLE: &str = "NintendoSDK Firmware for NX 23.0.0-4.0";
/// Leave empty when there's no digest (N/A). Eden stores this as `char[0x40]`.
pub const VERSION_DIGEST: &str = "";

// Atmosphere version constants.

pub const ATMOSPHERE_RELEASE_VERSION_MAJOR: u8 = 1;
pub const ATMOSPHERE_RELEASE_VERSION_MINOR: u8 = 12;
pub const ATMOSPHERE_RELEASE_VERSION_MICRO: u8 = 0;

/// Compute the atmosphere target firmware value with revision.
#[inline]
pub const fn atmosphere_target_firmware_with_revision(
    major: u8,
    minor: u8,
    micro: u8,
    rev: u8,
) -> u32 {
    (major as u32) << 24 | (minor as u32) << 16 | (micro as u32) << 8 | (rev as u32)
}

/// Compute the atmosphere target firmware value (revision defaults to 0).
#[inline]
pub const fn atmosphere_target_firmware(major: u8, minor: u8, micro: u8) -> u32 {
    atmosphere_target_firmware_with_revision(major, minor, micro, 0)
}

/// Get the target firmware for the current HOS version.
#[inline]
pub const fn get_target_firmware() -> u32 {
    atmosphere_target_firmware(HOS_VERSION_MAJOR, HOS_VERSION_MINOR, HOS_VERSION_MICRO)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_target_firmware() {
        let fw = get_target_firmware();
        // 23 << 24 | 0 << 16 | 0 << 8 | 0
        assert_eq!(fw, 0x17_00_00_00);
    }

    #[test]
    fn test_atmosphere_target_firmware_with_revision() {
        let fw = atmosphere_target_firmware_with_revision(1, 2, 3, 4);
        assert_eq!(fw, 0x01_02_03_04);
    }

    #[test]
    fn firmware_23_reports_empty_digest() {
        assert!(VERSION_DIGEST.is_empty());
        assert_eq!(DISPLAY_VERSION, "23.0.0");
        assert_eq!(ATMOSPHERE_RELEASE_VERSION_MINOR, 12);
    }
}
