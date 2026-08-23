// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/vi/vi_types.h

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum DisplayResolution {
    DockedWidth = 1920,
    DockedHeight = 1080,
    UndockedWidth = 1280,
    UndockedHeight = 720,
}

/// Permission level for a particular VI service instance
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    User,
    System,
    Manager,
}

/// A policy type that may be requested via GetDisplayService and
/// GetDisplayServiceWithProxyNameExchange
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Policy {
    User = 0,
    Compositor = 1,
}

impl Policy {
    pub fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            0 => Some(Self::User),
            1 => Some(Self::Compositor),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum ConvertedScaleMode {
    Freeze = 0,
    ScaleToWindow = 1,
    ScaleAndCrop = 2,
    None = 3,
    PreserveAspectRatio = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum NintendoScaleMode {
    None = 0,
    Freeze = 1,
    ScaleToWindow = 2,
    ScaleAndCrop = 3,
    PreserveAspectRatio = 4,
}

impl NintendoScaleMode {
    pub fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            0 => Some(Self::None),
            1 => Some(Self::Freeze),
            2 => Some(Self::ScaleToWindow),
            3 => Some(Self::ScaleAndCrop),
            4 => Some(Self::PreserveAspectRatio),
            _ => None,
        }
    }
}

pub type DisplayName = [u8; 0x40];

#[derive(Clone, Copy)]
#[repr(C)]
pub struct DisplayInfo {
    /// The name of this particular display.
    pub display_name: DisplayName,
    /// Whether or not the display has a limited number of layers.
    pub has_limited_layers: u8,
    pub _padding: [u8; 7],
    /// Indicates the total amount of layers supported by the display.
    pub max_layers: u64,
    /// Maximum width in pixels.
    pub width: u64,
    /// Maximum height in pixels.
    pub height: u64,
}
const _: () = assert!(core::mem::size_of::<DisplayInfo>() == 0x60);

impl Default for DisplayInfo {
    fn default() -> Self {
        let mut display_name = [0u8; 0x40];
        let name = b"Default";
        display_name[..name.len()].copy_from_slice(name);
        Self {
            display_name,
            has_limited_layers: 1,
            _padding: [0; 7],
            max_layers: 1,
            width: 1920,
            height: 1080,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct DisplayMode {
    pub width: u32,
    pub height: u32,
    pub refresh_rate: f32,
    pub unknown: u32,
}
const _: () = assert!(core::mem::size_of::<DisplayMode>() == 0x10);

#[derive(Clone, Copy)]
#[repr(C)]
pub struct NativeWindow {
    magic: u32,
    process_id: u32,
    id: u64,
    _padding0: [u32; 2],
    dispdrv: [u8; 8],
    _padding1: [u32; 2],
}
const _: () = assert!(core::mem::size_of::<NativeWindow>() == 0x28);

impl NativeWindow {
    pub fn new(id: i32) -> Self {
        Self {
            magic: 2,
            process_id: 1,
            id: id as u64,
            _padding0: [0; 2],
            dispdrv: *b"dispdrv\0",
            _padding1: [0; 2],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{NintendoScaleMode, Policy};

    #[test]
    fn policy_from_raw_matches_upstream_values() {
        assert_eq!(Policy::from_raw(0), Some(Policy::User));
        assert_eq!(Policy::from_raw(1), Some(Policy::Compositor));
        assert_eq!(Policy::from_raw(2), None);
        assert_eq!(Policy::from_raw(u32::MAX), None);
    }

    #[test]
    fn nintendo_scale_mode_from_raw_rejects_unknown_values() {
        assert_eq!(
            NintendoScaleMode::from_raw(0),
            Some(NintendoScaleMode::None)
        );
        assert_eq!(
            NintendoScaleMode::from_raw(4),
            Some(NintendoScaleMode::PreserveAspectRatio)
        );
        assert_eq!(NintendoScaleMode::from_raw(5), None);
        assert_eq!(NintendoScaleMode::from_raw(u32::MAX), None);
    }
}
