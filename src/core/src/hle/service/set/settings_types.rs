// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/core/hle/service/set/settings_types.h
//!
//! All enums, structs, and constants used by the settings service.

/// Setting item name buffer type. Upstream: `SettingItemName`.
pub type SettingItemName = [u8; 0x48];

/// nn::settings::system::AudioOutputMode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum AudioOutputMode {
    Ch1 = 0,
    Ch2 = 1,
    Ch5_1 = 2,
    Ch7_1 = 3,
}

/// nn::settings::system::AudioOutputModeTarget
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum AudioOutputModeTarget {
    None = 0,
    Hdmi = 1,
    Speaker = 2,
    Headphone = 3,
    Type3 = 4,
    Type4 = 5,
}

/// nn::settings::system::AudioVolumeTarget
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum AudioVolumeTarget {
    Speaker = 0,
    Headphone = 1,
}

/// nn::settings::system::ClockSourceId
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ClockSourceId {
    NetworkSystemClock = 0,
    SteadyClock = 1,
}

/// nn::settings::system::CmuMode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum CmuMode {
    None = 0,
    ColorInvert = 1,
    HighContrast = 2,
    GrayScale = 3,
}

/// nn::settings::system::ChineseTraditionalInputMethod
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ChineseTraditionalInputMethod {
    Unknown0 = 0,
    Unknown1 = 1,
    Unknown2 = 2,
}

/// Indicates the current theme set by the system settings
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ColorSet {
    BasicWhite = 0,
    BasicBlack = 1,
}

impl Default for ColorSet {
    fn default() -> Self {
        Self::BasicWhite
    }
}

/// nn::settings::system::ConsoleSleepPlan
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ConsoleSleepPlan {
    Sleep1Hour = 0,
    Sleep2Hour = 1,
    Sleep3Hour = 2,
    Sleep6Hour = 3,
    Sleep12Hour = 4,
    Never = 5,
}

/// nn::settings::system::ErrorReportSharePermission
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ErrorReportSharePermission {
    NotConfirmed = 0,
    Granted = 1,
    Denied = 2,
}

impl Default for ErrorReportSharePermission {
    fn default() -> Self {
        Self::NotConfirmed
    }
}

/// nn::settings::system::EulaVersionClockType
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum EulaVersionClockType {
    NetworkSystemClock = 0,
    SteadyClock = 1,
}

/// nn::settings::factory::RegionCode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum FactoryRegionCode {
    Japan = 0,
    Usa = 1,
    Europe = 2,
    Australia = 3,
    China = 4,
    Korea = 5,
    Taiwan = 6,
}

/// nn::settings::system::FriendPresenceOverlayPermission
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FriendPresenceOverlayPermission {
    NotConfirmed = 0,
    NoDisplay = 1,
    FavoriteFriends = 2,
    Friends = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GetFirmwareVersionType {
    Version1,
    Version2,
}

/// nn::settings::system::HandheldSleepPlan
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum HandheldSleepPlan {
    Sleep1Min = 0,
    Sleep3Min = 1,
    Sleep5Min = 2,
    Sleep10Min = 3,
    Sleep30Min = 4,
    Never = 5,
}

/// nn::settings::system::HdmiContentType
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum HdmiContentType {
    None = 0,
    Graphics = 1,
    Cinema = 2,
    Photo = 3,
    Game = 4,
}

/// nn::settings::system::KeyboardLayout
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum KeyboardLayout {
    Japanese = 0,
    EnglishUs = 1,
    EnglishUsInternational = 2,
    EnglishUk = 3,
    French = 4,
    FrenchCa = 5,
    Spanish = 6,
    SpanishLatin = 7,
    German = 8,
    Italian = 9,
    Portuguese = 10,
    Russian = 11,
    Korean = 12,
    ChineseSimplified = 13,
    ChineseTraditional = 14,
}

impl Default for KeyboardLayout {
    fn default() -> Self {
        Self::EnglishUsInternational
    }
}

/// nn::settings::Language
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Language {
    Japanese = 0,
    AmericanEnglish = 1,
    French = 2,
    German = 3,
    Italian = 4,
    Spanish = 5,
    Chinese = 6,
    Korean = 7,
    Dutch = 8,
    Portiguesue = 9,
    Russian = 10,
    Taiwanese = 11,
    BritishEnglish = 12,
    CanadianFrench = 13,
    LatinAmericanSpanish = 14,
    SimplifiedChinese = 15,
    TraditionalChinese = 16,
    BrazilianPortuguese = 17,
    Polish = 18,
    Thai = 19,
}

/// nn::settings::LanguageCode - NUL-terminated string stored in a u64.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum LanguageCode {
    Ja = 0x000000000000616A,
    EnUs = 0x00000053552D6E65,
    Fr = 0x0000000000007266,
    De = 0x0000000000006564,
    It = 0x0000000000007469,
    Es = 0x0000000000007365,
    ZhCn = 0x0000004E432D687A,
    Ko = 0x0000000000006F6B,
    Nl = 0x0000000000006C6E,
    Pt = 0x0000000000007470,
    Ru = 0x0000000000007572,
    ZhTw = 0x00000057542D687A,
    EnGb = 0x00000042472D6E65,
    FrCa = 0x00000041432D7266,
    Es419 = 0x00003931342D7365,
    ZhHans = 0x00736E61482D687A,
    ZhHant = 0x00746E61482D687A,
    PtBr = 0x00000052422D7470,
    Pl = 0x000000000000706C,
    Th = 0x0000000000006874,
}

/// nn::settings::system::NotificationVolume
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum NotificationVolume {
    Mute = 0,
    Low = 1,
    High = 2,
}

/// nn::settings::system::PrimaryAlbumStorage
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum PrimaryAlbumStorage {
    Nand = 0,
    SdCard = 1,
}

impl Default for PrimaryAlbumStorage {
    fn default() -> Self {
        Self::Nand
    }
}

/// Indicates the current console is a retail or kiosk unit
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum QuestFlag {
    Retail = 0,
    Kiosk = 1,
}

impl Default for QuestFlag {
    fn default() -> Self {
        Self::Retail
    }
}

/// nn::settings::system::RgbRange
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum RgbRange {
    Auto = 0,
    Full = 1,
    Limited = 2,
}

/// nn::settings::system::RegionCode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum SystemRegionCode {
    Japan = 0,
    Usa = 1,
    Europe = 2,
    Australia = 3,
    HongKongTaiwanKorea = 4,
    China = 5,
}

/// nn::settings::system::TouchScreenMode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum TouchScreenMode {
    Stylus = 0,
    Standard = 1,
}

impl Default for TouchScreenMode {
    fn default() -> Self {
        Self::Standard
    }
}

/// nn::settings::system::TvResolution
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum TvResolution {
    Auto = 0,
    Resolution1080p = 1,
    Resolution720p = 2,
    Resolution480p = 3,
}

/// nn::settings::system::PlatformRegion
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum PlatformRegion {
    Global = 1,
    Terra = 2,
}

impl Default for PlatformRegion {
    fn default() -> Self {
        Self::Global
    }
}

// --- Language code and layout tables ---

pub const AVAILABLE_LANGUAGE_CODES: [LanguageCode; 20] = [
    LanguageCode::Ja,
    LanguageCode::EnUs,
    LanguageCode::Fr,
    LanguageCode::De,
    LanguageCode::It,
    LanguageCode::Es,
    LanguageCode::ZhCn,
    LanguageCode::Ko,
    LanguageCode::Nl,
    LanguageCode::Pt,
    LanguageCode::Ru,
    LanguageCode::ZhTw,
    LanguageCode::EnGb,
    LanguageCode::FrCa,
    LanguageCode::Es419,
    LanguageCode::ZhHans,
    LanguageCode::ZhHant,
    LanguageCode::PtBr,
    LanguageCode::Pl,
    LanguageCode::Th,
];

pub const LANGUAGE_TO_LAYOUT: [(LanguageCode, KeyboardLayout); 20] = [
    (LanguageCode::Ja, KeyboardLayout::Japanese),
    (LanguageCode::EnUs, KeyboardLayout::EnglishUs),
    (LanguageCode::Fr, KeyboardLayout::French),
    (LanguageCode::De, KeyboardLayout::German),
    (LanguageCode::It, KeyboardLayout::Italian),
    (LanguageCode::Es, KeyboardLayout::Spanish),
    (LanguageCode::ZhCn, KeyboardLayout::ChineseSimplified),
    (LanguageCode::Ko, KeyboardLayout::Korean),
    (LanguageCode::Nl, KeyboardLayout::EnglishUsInternational),
    (LanguageCode::Pt, KeyboardLayout::Portuguese),
    (LanguageCode::Ru, KeyboardLayout::Russian),
    (LanguageCode::ZhTw, KeyboardLayout::ChineseTraditional),
    (LanguageCode::EnGb, KeyboardLayout::EnglishUk),
    (LanguageCode::FrCa, KeyboardLayout::FrenchCa),
    (LanguageCode::Es419, KeyboardLayout::SpanishLatin),
    (LanguageCode::ZhHans, KeyboardLayout::ChineseSimplified),
    (LanguageCode::ZhHant, KeyboardLayout::ChineseTraditional),
    (LanguageCode::PtBr, KeyboardLayout::Portuguese),
    (LanguageCode::Pl, KeyboardLayout::EnglishUsInternational),
    (LanguageCode::Th, KeyboardLayout::EnglishUsInternational),
];

// --- Bitfield flag structs ---

/// nn::settings::system::AccountNotificationFlag
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct AccountNotificationFlag {
    pub raw: u32,
}
const _: () = assert!(std::mem::size_of::<AccountNotificationFlag>() == 4);

impl AccountNotificationFlag {
    pub fn friend_online_flag(&self) -> bool {
        (self.raw & (1 << 0)) != 0
    }
    pub fn friend_request_flag(&self) -> bool {
        (self.raw & (1 << 1)) != 0
    }
    pub fn coral_invitation_flag(&self) -> bool {
        (self.raw & (1 << 8)) != 0
    }
}

/// nn::settings::system::AccountSettings
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct AccountSettings {
    pub flags: u32,
}
const _: () = assert!(std::mem::size_of::<AccountSettings>() == 4);

/// nn::settings::system::DataDeletionFlag
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct DataDeletionFlag {
    pub raw: u32,
}
const _: () = assert!(std::mem::size_of::<DataDeletionFlag>() == 4);

/// nn::settings::system::InitialLaunchFlag
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct InitialLaunchFlag {
    pub raw: u32,
}
const _: () = assert!(std::mem::size_of::<InitialLaunchFlag>() == 4);

impl InitialLaunchFlag {
    pub fn initial_launch_completion_flag(&self) -> bool {
        (self.raw & (1 << 0)) != 0
    }
    pub fn initial_launch_user_addition_flag(&self) -> bool {
        (self.raw & (1 << 8)) != 0
    }
    pub fn initial_launch_timestamp_flag(&self) -> bool {
        (self.raw & (1 << 16)) != 0
    }
}

/// nn::settings::system::SleepFlag
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct SleepFlag {
    pub raw: u32,
}
const _: () = assert!(std::mem::size_of::<SleepFlag>() == 4);

/// nn::settings::system::NotificationFlag
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct NotificationFlag {
    pub raw: u32,
}
const _: () = assert!(std::mem::size_of::<NotificationFlag>() == 4);

/// nn::settings::system::PlatformConfig
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct PlatformConfig {
    pub raw: u32,
}
const _: () = assert!(std::mem::size_of::<PlatformConfig>() == 4);

/// nn::settings::system::TvFlag
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct TvFlag {
    pub raw: u32,
}
const _: () = assert!(std::mem::size_of::<TvFlag>() == 4);

/// nn::settings::system::UserSelectorFlag
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct UserSelectorFlag {
    pub raw: u32,
}
const _: () = assert!(std::mem::size_of::<UserSelectorFlag>() == 4);

/// nn::settings::system::AccountNotificationSettings
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct AccountNotificationSettings {
    pub uid: [u8; 16], // Common::UUID
    pub flags: AccountNotificationFlag,
    pub friend_presence_permission: u8, // FriendPresenceOverlayPermission
    pub friend_invitation_permission: u8, // FriendPresenceOverlayPermission
    pub _padding: [u8; 2],
}
const _: () = assert!(std::mem::size_of::<AccountNotificationSettings>() == 0x18);

impl Default for AccountNotificationSettings {
    fn default() -> Self {
        Self {
            uid: [0u8; 16],
            flags: AccountNotificationFlag::default(),
            friend_presence_permission: FriendPresenceOverlayPermission::NotConfirmed as u8,
            friend_invitation_permission: FriendPresenceOverlayPermission::NotConfirmed as u8,
            _padding: [0u8; 2],
        }
    }
}

/// nn::settings::factory::BatteryLot
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct BatteryLot {
    pub lot_number: [u8; 0x18],
}
const _: () = assert!(std::mem::size_of::<BatteryLot>() == 0x18);

impl Default for BatteryLot {
    fn default() -> Self {
        Self {
            lot_number: [0u8; 0x18],
        }
    }
}

/// nn::settings::system::EulaVersion
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct EulaVersion {
    pub version: u32,
    pub region_code: u32, // SystemRegionCode
    pub clock_type: u32,  // EulaVersionClockType
    pub _padding: [u8; 4],
    pub system_clock_context: [u8; 0x20], // PSC::Time::SystemClockContext
}
const _: () = assert!(std::mem::size_of::<EulaVersion>() == 0x30);

impl Default for EulaVersion {
    fn default() -> Self {
        Self {
            version: 0,
            region_code: 0,
            clock_type: 0,
            _padding: [0u8; 4],
            system_clock_context: [0u8; 0x20],
        }
    }
}

/// nn::settings::system::FirmwareVersionFormat
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct FirmwareVersionFormat {
    pub major: u8,
    pub minor: u8,
    pub micro: u8,
    pub _padding0: u8,
    pub revision_major: u8,
    pub revision_minor: u8,
    pub _padding1: [u8; 2],
    pub platform: [u8; 0x20],
    pub version_hash: [u8; 0x40],
    pub display_version: [u8; 0x18],
    pub display_title: [u8; 0x80],
}
const _: () = assert!(std::mem::size_of::<FirmwareVersionFormat>() == 0x100);

impl Default for FirmwareVersionFormat {
    fn default() -> Self {
        Self {
            major: 0,
            minor: 0,
            micro: 0,
            _padding0: 0,
            revision_major: 0,
            revision_minor: 0,
            _padding1: [0u8; 2],
            platform: [0u8; 0x20],
            version_hash: [0u8; 0x40],
            display_version: [0u8; 0x18],
            display_title: [0u8; 0x80],
        }
    }
}

/// nn::settings::system::HomeMenuScheme
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct HomeMenuScheme {
    pub main: u32,
    pub back: u32,
    pub sub: u32,
    pub bezel: u32,
    pub extra: u32,
}
const _: () = assert!(std::mem::size_of::<HomeMenuScheme>() == 0x14);

/// nn::settings::system::InitialLaunchSettings
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct InitialLaunchSettings {
    pub flags: InitialLaunchFlag,
    pub _padding: [u8; 4],
    pub timestamp: [u8; 0x18], // PSC::Time::SteadyClockTimePoint
}
const _: () = assert!(std::mem::size_of::<InitialLaunchSettings>() == 0x20);

impl Default for InitialLaunchSettings {
    fn default() -> Self {
        Self {
            flags: InitialLaunchFlag::default(),
            _padding: [0u8; 4],
            timestamp: [0u8; 0x18],
        }
    }
}

/// Packed variant for serialization.
#[derive(Debug, Clone, Copy)]
#[repr(C, packed(4))]
pub struct InitialLaunchSettingsPacked {
    pub flags: InitialLaunchFlag,
    pub timestamp: [u8; 0x18],
}
const _: () = assert!(std::mem::size_of::<InitialLaunchSettingsPacked>() == 0x1C);

/// nn::settings::system::NotificationTime
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct NotificationTime {
    pub hour: u32,
    pub minute: u32,
}
const _: () = assert!(std::mem::size_of::<NotificationTime>() == 0x8);

/// nn::settings::system::NotificationSettings
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct NotificationSettings {
    pub flags: NotificationFlag,
    pub volume: u32, // NotificationVolume
    pub start_time: NotificationTime,
    pub stop_time: NotificationTime,
}
const _: () = assert!(std::mem::size_of::<NotificationSettings>() == 0x18);

/// nn::settings::factory::SerialNumber
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct SerialNumber {
    pub serial_number: [u8; 0x18],
}
const _: () = assert!(std::mem::size_of::<SerialNumber>() == 0x18);

impl Default for SerialNumber {
    fn default() -> Self {
        Self {
            serial_number: [0u8; 0x18],
        }
    }
}

/// nn::settings::system::SleepSettings
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct SleepSettings {
    pub flags: SleepFlag,
    pub handheld_sleep_plan: u32, // HandheldSleepPlan
    pub console_sleep_plan: u32,  // ConsoleSleepPlan
}
const _: () = assert!(std::mem::size_of::<SleepSettings>() == 0xC);

/// nn::settings::system::TvSettings
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct TvSettings {
    pub flags: TvFlag,
    pub tv_resolution: u32,     // TvResolution
    pub hdmi_content_type: u32, // HdmiContentType
    pub rgb_range: u32,         // RgbRange
    pub cmu_mode: u32,          // CmuMode
    pub tv_underscan: u32,
    pub tv_gama: f32,
    pub contrast_ratio: f32,
}
const _: () = assert!(std::mem::size_of::<TvSettings>() == 0x20);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_notification_permissions_preserve_raw_wire_values() {
        let mut settings = AccountNotificationSettings::default();
        settings.friend_presence_permission = 0xFE;
        settings.friend_invitation_permission = 0xFF;

        assert_eq!(core::mem::size_of_val(&settings), 0x18);
        assert_eq!(settings.friend_presence_permission, 0xFE);
        assert_eq!(settings.friend_invitation_permission, 0xFF);
    }
}
