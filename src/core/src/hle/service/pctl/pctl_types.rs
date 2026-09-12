// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/pctl/pctl_types.h

use bitflags::bitflags;

bitflags! {
    /// Capability — parental control capability flags.
    ///
    /// Corresponds to `Capability` in upstream pctl_types.h.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Capability: u32 {
        const NONE        = 0;
        const APPLICATION  = 1 << 0;
        const SNS_POST     = 1 << 1;
        const RECOVERY     = 1 << 6;
        const STATUS       = 1 << 8;
        const STEREO_VISION = 1 << 9;
        const SYSTEM       = 1 << 15;
    }
}

/// Application info — stored per-session after Initialize.
///
/// Corresponds to `ApplicationInfo` in upstream pctl_types.h.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ApplicationInfo {
    pub application_id: u64,
    pub age_rating: [u8; 32],
    pub parental_control_flag: u32,
    pub capability: u32,
}
const _: () = assert!(core::mem::size_of::<ApplicationInfo>() == 0x30);

/// nn::pctl::RestrictionSettings.
///
/// Corresponds to `RestrictionSettings` in upstream pctl_types.h.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct RestrictionSettings {
    pub rating_age: u8,
    pub sns_post_restriction: bool,
    pub free_communication_restriction: bool,
}
const _: () = assert!(core::mem::size_of::<RestrictionSettings>() == 0x3);

/// nn::pctl::PlayTimerSettings.
///
/// Corresponds to `PlayTimerSettings` in upstream pctl_types.h.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PlayTimerSettings {
    pub settings: [u32; 13],
}
const _: () = assert!(core::mem::size_of::<PlayTimerSettings>() == 0x34);

/// nn::pctl::detail::PlayTimerDisplayState.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlayTimerDisplayState {
    TimesUp = 0,
    BedtimeAlarm = 1,
    RemainingTime = 2,
    Unknown3 = 3,
    Unknown4 = 4,
    Unknown5 = 5,
    TimerDisabled = 6,
    #[default]
    NotConfigured = 7,
}

/// nn::pctl::PlayTimerRemainingTimeDisplayInfo.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PlayTimerRemainingTimeDisplayInfo {
    pub state: PlayTimerDisplayState,
    pub _padding: [u8; 7],
    pub unknown_08: u64,
    pub remaining_time: u64,
}
const _: () = assert!(core::mem::size_of::<PlayTimerRemainingTimeDisplayInfo>() == 0x18);

impl Default for PlayTimerRemainingTimeDisplayInfo {
    fn default() -> Self {
        Self {
            state: PlayTimerDisplayState::NotConfigured,
            _padding: [0; 7],
            unknown_08: 0,
            remaining_time: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_timer_remaining_time_display_info_is_0x18() {
        assert_eq!(
            core::mem::size_of::<PlayTimerRemainingTimeDisplayInfo>(),
            0x18
        );
        assert_eq!(
            PlayTimerRemainingTimeDisplayInfo::default().state,
            PlayTimerDisplayState::NotConfigured
        );
    }
}
