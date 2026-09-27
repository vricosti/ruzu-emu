// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/am/am_types.h

/// nn::am::AppletType
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AppletType {
    #[default]
    Application,
    LibraryApplet,
    OverlayApplet,
    SystemApplet,
}

/// nn::am::GamePlayRecordingState
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GamePlayRecordingState {
    #[default]
    Disabled = 0,
    Enabled = 1,
}

/// nn::oe::FocusState
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FocusState {
    // Upstream value-initializes LifecycleManager's focus-state fields with
    // `{}`, producing the unnamed underlying value 0 before the first update.
    #[default]
    Unknown = 0,
    InFocus = 1,
    NotInFocus = 2,
    Background = 3,
}

/// nn::oe::OperationMode
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OperationMode {
    #[default]
    Handheld = 0,
    Docked = 1,
}

/// nn::am::service::SystemButtonType
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SystemButtonType {
    #[default]
    None,
    HomeButtonShortPressing,
    HomeButtonLongPressing,
    PowerButtonShortPressing,
    PowerButtonLongPressing,
    ShutdownSystem,
    CaptureButtonShortPressing,
    CaptureButtonLongPressing,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct AppletProcessLaunchReason {
    pub flag: u8,
    pub _padding: [u8; 3],
}
const _: () = assert!(std::mem::size_of::<AppletProcessLaunchReason>() == 0x4);

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScreenshotPermission {
    #[default]
    Inherit = 0,
    Enable = 1,
    Disable = 2,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IdleTimeDetectionExtension {
    #[default]
    Disabled = 0,
    Extended = 1,
    ExtendedUnsafe = 2,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AppletId {
    #[default]
    None = 0x00,
    Application = 0x01,
    OverlayDisplay = 0x02,
    QLaunch = 0x03,
    Starter = 0x04,
    Auth = 0x0A,
    Cabinet = 0x0B,
    Controller = 0x0C,
    DataErase = 0x0D,
    Error = 0x0E,
    NetConnect = 0x0F,
    ProfileSelect = 0x10,
    SoftwareKeyboard = 0x11,
    MiiEdit = 0x12,
    Web = 0x13,
    Shop = 0x14,
    PhotoViewer = 0x15,
    Settings = 0x16,
    OfflineWeb = 0x17,
    LoginShare = 0x18,
    WebAuth = 0x19,
    MyPage = 0x1A,
    Lhub = 0x35,
}

#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppletProgramId {
    QLaunch = 0x0100000000001000,
    Auth = 0x0100000000001001,
    Cabinet = 0x0100000000001002,
    Controller = 0x0100000000001003,
    DataErase = 0x0100000000001004,
    Error = 0x0100000000001005,
    NetConnect = 0x0100000000001006,
    ProfileSelect = 0x0100000000001007,
    SoftwareKeyboard = 0x0100000000001008,
    MiiEdit = 0x0100000000001009,
    Web = 0x010000000000100A,
    Shop = 0x010000000000100B,
    OverlayDisplay = 0x010000000000100C,
    PhotoViewer = 0x010000000000100D,
    Settings = 0x010000000000100E,
    OfflineWeb = 0x010000000000100F,
    LoginShare = 0x0100000000001010,
    WebAuth = 0x0100000000001011,
    Starter = 0x0100000000001012,
    MyPage = 0x0100000000001013,
    MaxProgramId = 0x0100000000001FFF,
}

/// nn::am::AppletMessage
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AppletMessage {
    #[default]
    None = 0,
    ChangeIntoForeground = 1,
    ChangeIntoBackground = 2,
    Exit = 4,
    ApplicationExited = 6,
    FocusStateChanged = 15,
    Resume = 16,
    DetectShortPressingHomeButton = 20,
    DetectLongPressingHomeButton = 21,
    DetectShortPressingPowerButton = 22,
    DetectMiddlePressingPowerButton = 23,
    DetectLongPressingPowerButton = 24,
    RequestToPrepareSleep = 25,
    FinishedSleepSequence = 26,
    SleepRequiredByHighTemperature = 27,
    SleepRequiredByLowBattery = 28,
    AutoPowerDown = 29,
    OperationModeChanged = 30,
    PerformanceModeChanged = 31,
    DetectReceivingCecSystemStandby = 32,
    SdCardRemoved = 33,
    LaunchApplicationRequested = 50,
    RequestToDisplay = 51,
    ShowApplicationLogo = 55,
    HideApplicationLogo = 56,
    ForceHideApplicationLogo = 57,
    FloatingApplicationDetected = 60,
    DetectShortPressingCaptureButton = 90,
    DetectLongPressingCaptureButton = 91,
    AlbumScreenShotTaken = 92,
    AlbumRecordingSaved = 93,
    StartupLogoDisappeared = 95,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LibraryAppletMode {
    #[default]
    AllForeground = 0,
    PartialForeground = 1,
    NoUi = 2,
    PartialForegroundIndirectDisplay = 3,
    AllForegroundInitiallyHidden = 4,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchParameterKind {
    UserChannel = 1,
    AccountPreselectedUser = 2,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CommonArgumentVersion {
    #[default]
    Version0 = 0,
    Version1 = 1,
    Version2 = 2,
    Version3 = 3,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommonArgumentSize {
    Version3 = 0x20,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeColor {
    #[default]
    BasicWhite = 0,
    BasicBlack = 3,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputDetectionPolicy {
    #[default]
    Unknown0 = 0,
    Unknown1 = 1,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowOriginMode {
    #[default]
    LowerLeft = 0,
    UpperLeft = 1,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramSpecifyKind {
    ExecuteProgram = 0,
    JumpToSubApplicationProgramForDevelopment = 1,
    RestartProgram = 2,
}

/// Upstream AppletZIndex: composition sorts back-to-front.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppletZIndex {
    Background = 0,
    Foreground = 1,
    ForegroundVisible = 2,
    Overlay = 3,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CommonArguments {
    pub arguments_version: CommonArgumentVersion,
    pub size: u32,
    pub library_version: u32,
    pub theme_color: ThemeColor,
    pub play_startup_sound: bool,
    pub _pad: [u8; 3],
    pub system_tick: u64,
}
const _: () = assert!(std::mem::size_of::<CommonArguments>() == 0x20);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct AppletIdentityInfo {
    pub applet_id: u32,
    pub _padding: [u8; 0x4],
    pub application_id: u64,
}
const _: () = assert!(std::mem::size_of::<AppletIdentityInfo>() == 0x10);

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct AppletAttribute {
    pub flag: u8,
    pub _padding: [u8; 0x7F],
}
const _: () = assert!(std::mem::size_of::<AppletAttribute>() == 0x80);

impl Default for AppletAttribute {
    fn default() -> Self {
        Self {
            flag: 0,
            _padding: [0; 0x7F],
        }
    }
}

/// nn::oe::DisplayVersion
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct DisplayVersion {
    pub string: [u8; 0x10],
}
const _: () = assert!(std::mem::size_of::<DisplayVersion>() == 0x10);

/// nn::pdm::ApplicationPlayStatistics
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ApplicationPlayStatistics {
    pub application_id: u64,
    pub play_time_ns: u64,
    pub launch_count: u64,
}
const _: () = assert!(std::mem::size_of::<ApplicationPlayStatistics>() == 0x18);

/// AppletResourceUserId
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct AppletResourceUserId {
    pub pid: u64,
}

pub type ProgramId = u64;
